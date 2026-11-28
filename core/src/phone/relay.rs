//! The computer's side of the relay. One WebSocket to
//! `wss://<relay>/machines/<label>/pc` carries every device: the computer
//! brings its account's session, the relay challenges it to sign a nonce with
//! its identity, then opens a numbered channel for each device signed in to
//! the same account, and passes their frames along unread. The connection is
//! kept up from a thread of its own, which dials again when it drops, waiting
//! longer each time; the shell hears how it goes as `Incoming`.
//!
//! Frames to and from the relay are binary: a type byte, the channel as four
//! big-endian bytes, then the payload. Channel 0 is the relay itself, speaking
//! JSON control messages; the other channels carry a device's `hello` and
//! `data` frames, which only the device and this computer can read (`e2e`).
//!
//! A channel counts as a client to the shell, numbered as the relay numbered
//! it, once two things hold: the relay's `open` named this computer's own
//! account, and the keys are agreed. Then the device speaks JSON: the same
//! messages it would send on a socket of its own (`ClientMsg`), plus
//! `{req: {id, method, path, body}}` for what it would otherwise ask over HTTP
//! (`route`), answered with `{res: {id, status, type, body, more}}` in pieces
//! small enough for the relay. Any other message too big for one frame goes
//! as `{part: {body, more}}` pieces, which the device joins back (`say`).

use std::collections::HashMap;
use std::net::{TcpStream, ToSocketAddrs as _};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, mpsc as std_mpsc};
use std::time::{Duration, Instant};

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD as B64;
use futures::channel::mpsc;
use polling::{Event as PollEvent, Events, Poller};
use serde_json::{Value, json};
use tungstenite::client::IntoClientRequest as _;
use tungstenite::http::{HeaderValue, header::AUTHORIZATION};
use tungstenite::{Message, WebSocket, stream::MaybeTlsStream};

use super::e2e::{Handshake, PhoneHello, Session};
use super::identity::Identity;
use super::quiet::Quiet;
use super::{ClientId, ClientMsg, Effect, Incoming, Seen, Shared, linear, wire};

pub const HELLO: u8 = 0x01;
pub const DATA: u8 = 0x02;
pub const CONTROL: u8 = 0x03;
/// Starts every claim, so the identity key never signs a claim that could
/// also be read as another message it signs, such as the handshake
/// transcript (`e2e::transcript`).
const CLAIM_TAG: &[u8] = b"arakun-relay-claim-v1\0";
/// The relay's challenge nonce is always this long. Any other length is
/// refused before signing: a relay must not choose how much we sign.
const CLAIM_NONCE_LEN: usize = 16;

/// How much of a message or a response body goes in one piece: with base64
/// and the envelope a piece stays under `WHOLE`.
const CHUNK: usize = 160 * 1024;
/// The longest message that goes out whole: sealed, it stays under the
/// relay's frame limit (256 KiB). A `res` piece fits, so it is not split again.
const WHOLE: usize = 250 * 1024;
/// The most of the operator's text a status carries.
const MAX_MESSAGE: usize = 300;

/// The first wait after a drop; each failure doubles it up to `RETRY_MAX`.
const RETRY_MIN: Duration = Duration::from_secs(1);
const RETRY_MAX: Duration = Duration::from_secs(60);
/// How long to wait after the relay turned us away for a reason that may pass,
/// such as a block the operator lifts again. Long, so a blocked computer does
/// not knock all day; short in tests, which wait for the next dial.
const REFUSED_WAIT: Duration = if cfg!(test) {
    Duration::from_millis(300)
} else {
    Duration::from_secs(10 * 60)
};
/// How long dialing and the handshakes may take together.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
/// The relay hears from us this often while nothing else is said, and is
/// given up on after this long without a word back.
const PING_EVERY: Duration = Duration::from_secs(30);
const SILENCE_MAX: Duration = Duration::from_secs(90);
/// How long a read waits while claiming, before the deadline is looked at.
/// Once claimed, the loop waits on the socket and on the shell instead.
const READ_TICK: Duration = Duration::from_millis(20);
/// The poller's key for the relay socket.
const SOCKET: usize = 0;
/// How often the stop flag and the commands are looked at between dials.
const WAIT_TICK: Duration = Duration::from_millis(100);

/// How the connection to the relay is doing, as the Phone tab shows it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Status {
    Connecting,
    Connected,
    /// Dropped or refused for now; dialing again after the wait.
    Reconnecting {
        why: String,
        retry_in: Duration,
    },
    /// The relay turned this computer away: the operator blocked the account,
    /// or another computer with this very key took the place. That can
    /// change, so it asks again, but only after a long wait.
    Refused {
        why: String,
        retry_in: Duration,
    },
    /// The relay will not have this computer, and asking again would not
    /// change that: the label belongs to another key or account, the relay
    /// does not accept its signature, or the computer was removed from the
    /// account. It asks again only when given a session (`set_session`).
    Rejected(String),
    /// There is no session, or the relay says it is no longer good. Nothing
    /// is dialed until `set_session` gives one.
    SignedOut,
}

/// What a computer is in its account's list.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MachineKind {
    App,
    Server,
}

impl MachineKind {
    /// As the relay's door takes it.
    pub fn as_str(self) -> &'static str {
        match self {
            MachineKind::App => "app",
            MachineKind::Server => "server",
        }
    }
}

/// How this computer is named in its account's list of machines.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Machine {
    pub name: String,
    pub kind: MachineKind,
}

/// The account this computer reaches the relay as. No `Debug`: the session
/// opens the account, so it must never end up in a log.
#[derive(Clone)]
pub struct Account {
    /// The account's GitHub ID. A channel the relay opens for any other
    /// account is closed. A different account means a new client.
    pub github_id: u64,
    /// The session token, sent as the bearer. `None` when signed out.
    pub session: Option<String>,
}

/// What the shell asks the connection thread to do.
enum Command {
    /// A new session, or none: signed in again, or signed out.
    Session(Option<String>),
    /// A message to the device on a channel, sealed there.
    Send { channel: u32, text: String },
    /// A message to every client.
    Broadcast(String),
    /// Close a device's channel; the relay tells the device why.
    CloseChannel { channel: u32, reason: String },
}

/// A channel the relay opened, as the shell sees it: the device the relay
/// named, and whether it is a client yet (`ready`, once the keys are agreed).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Channel {
    pub device: Option<String>,
    pub ready: bool,
}

type Channels = Arc<Mutex<HashMap<u32, Channel>>>;

/// The connection to the relay. Dropping it hangs up and ends the thread.
/// Devices look like the server's clients, numbered by channel.
pub struct RelayClient {
    page: String,
    channels: Channels,
    cmd: std_mpsc::Sender<Command>,
    /// Wakes the connection thread when there is something to send.
    poller: Arc<Poller>,
    stop: Arc<AtomicBool>,
}

impl RelayClient {
    /// Dials `base` as this identity, named `machine`, with the account's
    /// session, and keeps the connection up. Status changes (`Incoming::Relay`,
    /// starting from `Connecting`, or `SignedOut` without a session) and what
    /// devices say come out of the receiver; the devices are kept in `access`.
    pub fn start(
        base: &str,
        identity: Identity,
        access: Shared,
        machine: Machine,
        account: Account,
    ) -> (RelayClient, mpsc::UnboundedReceiver<Incoming>) {
        let label = identity.label();
        let (tx, rx) = mpsc::unbounded();
        let (cmd, commands) = std_mpsc::channel();
        let channels: Channels = Arc::default();
        let stop = Arc::new(AtomicBool::new(false));
        let poller = Arc::new(Poller::new().expect("a poller can be made"));
        let url = format!(
            "{}?name={}&kind={}",
            endpoint(base, &label),
            super::percent_encode(&machine.name),
            machine.kind.as_str()
        );
        let link = Link {
            // The session is a bearer: in the clear only to this computer.
            plain_ok: crate::cloud::local_http(base.trim()),
            url,
            identity,
            access,
            github_id: account.github_id,
            // Without an account no channel could ever be ours: dialing
            // would only hold a place on the relay for nothing.
            session: account.session.filter(|_| account.github_id != 0),
            commands,
            tx,
            status: None,
            channels: channels.clone(),
            peers: HashMap::new(),
            poller: poller.clone(),
            stop: stop.clone(),
        };
        std::thread::Builder::new()
            .name("arakun-relay".into())
            .spawn(move || link.run())
            .expect("a thread can start");
        (
            RelayClient {
                page: page_url(base),
                channels,
                cmd,
                poller,
                stop,
            },
            rx,
        )
    }

    /// Hands the connection thread a command and wakes it, so it goes out now.
    fn tell(&self, cmd: Command) {
        let _ = self.cmd.send(cmd);
        let _ = self.poller.notify();
    }

    /// Where devices find this computer through the relay.
    pub fn page_url(&self) -> &str {
        &self.page
    }

    /// The channels open right now, by number.
    pub fn channels(&self) -> HashMap<u32, Channel> {
        self.channels.lock().unwrap().clone()
    }

    /// A new session for the same account, or `None` once signed out. A
    /// different one dials again at once, even after a `Rejected`; the same
    /// one again changes nothing; none hangs up. A client started without an
    /// account (`github_id` 0) takes none.
    pub fn set_session(&self, session: Option<String>) {
        self.tell(Command::Session(session));
    }

    pub fn close_channel(&self, channel: u32, reason: &str) {
        self.tell(Command::CloseChannel {
            channel,
            reason: reason.to_string(),
        });
    }

    /// The device behind a client, once it is one.
    pub fn device_of(&self, id: ClientId) -> Option<String> {
        let channels = self.channels.lock().unwrap();
        let c = channels.get(&u32::try_from(id).ok()?)?;
        c.ready.then(|| c.device.clone()).flatten()
    }

    pub fn send(&self, id: ClientId, text: String) {
        if let Ok(channel) = u32::try_from(id) {
            self.tell(Command::Send { channel, text });
        }
    }

    pub fn broadcast(&self, text: &str) {
        self.tell(Command::Broadcast(text.to_string()));
    }

    pub fn client_count(&self) -> usize {
        self.channels
            .lock()
            .unwrap()
            .values()
            .filter(|c| c.ready)
            .count()
    }

    /// The devices with a channel that is a client right now.
    pub fn connected_devices(&self) -> Vec<String> {
        let mut ids: Vec<String> = self
            .channels
            .lock()
            .unwrap()
            .values()
            .filter(|c| c.ready)
            .filter_map(|c| c.device.clone())
            .collect();
        ids.sort();
        ids.dedup();
        ids
    }

    /// Closes every channel of a device, as forgetting it must. It is still
    /// signed in to the account and may come back; signing it out is the
    /// account's job.
    pub fn disconnect_device(&self, device: &str) {
        for (channel, c) in self.channels() {
            if c.device.as_deref() == Some(device) {
                self.close_channel(channel, "unpaired");
            }
        }
    }
}

impl Drop for RelayClient {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        let _ = self.poller.notify();
    }
}

/// How long until the next dial, as people say it: seconds, or minutes once
/// it is that long.
pub fn wait_text(wait: Duration) -> String {
    let secs = wait.as_secs().max(1);
    if secs < 120 {
        format!("{secs} s")
    } else {
        format!("{} min", secs.div_ceil(60))
    }
}

/// The WebSocket address of this computer's door on the relay. A base at a
/// bare address or `localhost` is a relay run by hand (`wrangler dev`), which
/// may go without TLS.
pub fn endpoint(base: &str, label: &str) -> String {
    let (secure, host) = origin(base);
    format!(
        "{}://{host}/machines/{label}/pc",
        if secure { "wss" } else { "ws" }
    )
}

/// The page devices open to reach this computer through the relay: the
/// relay's own, which lists the account's computers.
pub fn page_url(base: &str) -> String {
    let (secure, host) = origin(base);
    format!("{}://{host}/", if secure { "https" } else { "http" })
}

/// Whether a session from the service at `account_base` may go to the relay
/// at `relay_base`. The relay's address is read as the client dials it, so
/// `app.arakun.dev` with no scheme is the same server as
/// `https://app.arakun.dev`. The app and `arakun-server` both ask this.
pub fn takes_session_from(relay_base: &str, account_base: &str) -> bool {
    crate::cloud::same_origin(&page_url(relay_base), account_base)
}

/// Whether the relay is reached over TLS, and its host (with port).
fn origin(base: &str) -> (bool, String) {
    let rest = base.trim().trim_end_matches('/');
    let (scheme, host) = rest.split_once("://").unwrap_or(("https", rest));
    let name = host.rsplit_once(':').map_or(host, |(name, port)| {
        if port.bytes().all(|b| b.is_ascii_digit()) {
            name
        } else {
            host
        }
    });
    let local = name == "localhost" || name.parse::<std::net::IpAddr>().is_ok();
    (!local || scheme == "https", host.to_string())
}

struct Link {
    /// The door, with the machine's name and kind in the query.
    url: String,
    /// Whether the door may be plain `ws://`: only on this computer.
    plain_ok: bool,
    identity: Identity,
    access: Shared,
    /// The account whose devices may become clients.
    github_id: u64,
    session: Option<String>,
    commands: std_mpsc::Receiver<Command>,
    tx: mpsc::UnboundedSender<Incoming>,
    /// The last status told, so the same one is not told twice.
    status: Option<Status>,
    channels: Channels,
    /// The devices on the open channels, as far as each has got.
    peers: HashMap<u32, Peer>,
    poller: Arc<Poller>,
    stop: Arc<AtomicBool>,
}

/// A device on a channel, step by step.
enum Peer {
    /// The relay opened the channel for a device of this account; no hello yet.
    Opened {
        device: String,
        name: String,
        kind: String,
    },
    /// Keys agreed: the shell knows it as a client. Boxed, as the session
    /// is far bigger than the rest.
    Ready {
        session: Box<Session>,
        device: String,
    },
}

/// Why one connection ended, and whether another should follow.
enum Ended {
    /// Dial again after a wait.
    Retry(String),
    /// Turned away; dial again after `REFUSED_WAIT`.
    Refused(String),
    /// Do not come back until the user changes something.
    Rejected(String),
    /// The session is no good: forget it and wait for another.
    SignedOut,
    /// The shell gave a new session: dial again with it now.
    Resume,
    /// The client was dropped.
    Stopped,
}

type Socket = WebSocket<MaybeTlsStream<TcpStream>>;

impl Link {
    fn run(mut self) {
        let mut wait = RETRY_MIN;
        loop {
            if self.stop.load(Ordering::Relaxed) {
                return;
            }
            if self.session.is_none() {
                self.set_status(Status::SignedOut);
                if !self.pause(None) {
                    return;
                }
                wait = RETRY_MIN;
                continue;
            }
            self.set_status(Status::Connecting);
            let ended = match self.connect() {
                // Dropped while it dialed: the client that replaced this one
                // may hold the place by now, and a claim would take it away.
                Ok(_) if self.stop.load(Ordering::Relaxed) => return,
                Ok(mut ws) => match self.claim(&mut ws) {
                    Ok(()) => {
                        wait = RETRY_MIN;
                        self.set_status(Status::Connected);
                        let ended = self.talk(&mut ws);
                        let _ = ws.close(None);
                        let _ = ws.flush();
                        ended
                    }
                    Err(ended) => ended,
                },
                Err(ended) => ended,
            };
            self.forget_channels();
            match ended {
                Ended::Stopped => return,
                Ended::Resume => wait = RETRY_MIN,
                Ended::SignedOut => self.session = None,
                Ended::Rejected(why) => {
                    self.set_status(Status::Rejected(why));
                    // Only a person can change this: by signing in again,
                    // which gives a session, or by starting a new client.
                    if !self.pause(None) {
                        return;
                    }
                    wait = RETRY_MIN;
                }
                Ended::Refused(why) => {
                    self.set_status(Status::Refused {
                        why,
                        retry_in: REFUSED_WAIT,
                    });
                    if !self.pause(Some(REFUSED_WAIT)) {
                        return;
                    }
                }
                Ended::Retry(why) => {
                    self.set_status(Status::Reconnecting {
                        why,
                        retry_in: wait,
                    });
                    if !self.pause(Some(wait)) {
                        return;
                    }
                    wait = (wait * 2).min(RETRY_MAX);
                }
            }
        }
    }

    /// Waits between dials for `wait`, or with none until the shell gives a
    /// session; a session given during a wait ends it early too. False when
    /// the client was dropped during the wait.
    fn pause(&mut self, wait: Option<Duration>) -> bool {
        let until = wait.map(|w| Instant::now() + w);
        loop {
            if self.stop.load(Ordering::Relaxed) {
                return false;
            }
            if self.drain_commands_offline() {
                return true;
            }
            if until.is_some_and(|u| Instant::now() >= u) {
                return true;
            }
            std::thread::sleep(WAIT_TICK);
        }
    }

    fn set_status(&mut self, status: Status) {
        if self.status.as_ref() != Some(&status) {
            self.status = Some(status.clone());
            let _ = self.tx.unbounded_send(Incoming::Relay(status));
        }
    }

    /// Commands that arrive while there is no connection: a session is kept;
    /// frames have nowhere to go. Whether the session changed.
    fn drain_commands_offline(&mut self) -> bool {
        let mut changed = false;
        while let Ok(cmd) = self.commands.try_recv() {
            if let Command::Session(s) = cmd {
                let s = self.usable(s);
                if s != self.session {
                    self.session = s;
                    changed = true;
                }
            }
        }
        changed
    }

    /// A session the shell gave, as this client may use it: none without an
    /// account.
    fn usable(&self, session: Option<String>) -> Option<String> {
        session.filter(|_| self.github_id != 0)
    }

    /// The connection is gone, and every channel with it: the shell hears each
    /// client leave.
    fn forget_channels(&mut self) {
        self.channels.lock().unwrap().clear();
        for (channel, peer) in std::mem::take(&mut self.peers) {
            if matches!(peer, Peer::Ready { .. }) {
                let _ = self
                    .tx
                    .unbounded_send(Incoming::Disconnected(ClientId::from(channel)));
            }
        }
    }

    /// TCP, TLS when the address asks for it, and the WebSocket handshake
    /// with the session as the bearer.
    fn connect(&self) -> Result<Socket, Ended> {
        if self.url.starts_with("ws://") && !self.plain_ok {
            return Err(Ended::Rejected(
                "The relay address must start with https:// (http:// only for this computer)."
                    .into(),
            ));
        }
        let session = self.session.as_deref().ok_or(Ended::SignedOut)?;
        let bad_address = || Ended::Rejected(format!("Bad relay address: {}", self.url));
        let (host, port, _) = parse_url(&self.url).ok_or_else(bad_address)?;
        let mut request = self
            .url
            .as_str()
            .into_client_request()
            .map_err(|_| bad_address())?;
        // A session that is not a header's worth of text is not one at all.
        let mut bearer =
            HeaderValue::from_str(&format!("Bearer {session}")).map_err(|_| Ended::SignedOut)?;
        bearer.set_sensitive(true);
        request.headers_mut().insert(AUTHORIZATION, bearer);
        let addrs = (host.as_str(), port)
            .to_socket_addrs()
            .map_err(|e| Ended::Retry(format!("Could not resolve {host}: {e}")))?;
        let mut stream = None;
        for addr in addrs {
            if let Ok(s) = TcpStream::connect_timeout(&addr, CONNECT_TIMEOUT) {
                stream = Some(s);
                break;
            }
        }
        let stream =
            stream.ok_or_else(|| Ended::Retry(format!("Could not reach {host}:{port}")))?;
        // Devices wait on small frames: send each at once, not with the next.
        let _ = stream.set_nodelay(true);
        let _ = stream.set_read_timeout(Some(CONNECT_TIMEOUT));
        let _ = stream.set_write_timeout(Some(CONNECT_TIMEOUT));
        // A `ws://` address comes back as a plain stream in the same wrapper.
        let (ws, _) = tungstenite::client_tls(request, stream).map_err(|e| match e {
            tungstenite::HandshakeError::Failure(tungstenite::Error::Http(res)) => {
                let body = res
                    .body()
                    .as_deref()
                    .map(|b| String::from_utf8_lossy(b).trim().to_string())
                    .unwrap_or_default();
                let text = if body.is_empty() {
                    format!("The relay answered {}", res.status())
                } else {
                    body
                };
                match res.status().as_u16() {
                    // A relay from before accounts, or one of one's own
                    // without them: no such door, or its page where the door
                    // would be. Dialing again would not change that.
                    200..=399 | 404 => {
                        Ended::Rejected("This relay does not support account sign-in.".into())
                    }
                    // The door did not take this name or kind, and would
                    // not take them next time either.
                    400 => Ended::Rejected(text),
                    // Refusals come as a `bye` on an accepted socket, so
                    // anything else, a 403 included, is from something in
                    // front of the relay that may let the next attempt through.
                    _ => Ended::Retry(text),
                }
            }
            tungstenite::HandshakeError::Failure(e) => {
                Ended::Retry(format!("Could not connect: {e}"))
            }
            // The stream blocks, so the handshake runs to its end.
            tungstenite::HandshakeError::Interrupted(_) => {
                Ended::Retry("The handshake was cut short".into())
            }
        })?;
        if let Some(s) = tcp(&ws) {
            let _ = s.set_read_timeout(Some(READ_TICK));
        }
        Ok(ws)
    }

    /// Answers the challenge with the signature of `CLAIM_TAG ‖ nonce ‖ label`
    /// (`claim_message`). A refusal at the door comes instead of the
    /// challenge, as a `bye`.
    fn claim(&mut self, ws: &mut Socket) -> Result<(), Ended> {
        let deadline = Instant::now() + CONNECT_TIMEOUT;
        let nonce = loop {
            if Instant::now() > deadline {
                return Err(Ended::Retry(
                    "The relay did not challenge us in time".into(),
                ));
            }
            match self.next_control(ws)? {
                Some(msg) if msg["type"] == "challenge" => {
                    let nonce = msg["nonce"].as_str().and_then(|n| B64.decode(n).ok());
                    break nonce.ok_or_else(|| {
                        Ended::Retry("The relay sent a challenge without a nonce".into())
                    })?;
                }
                Some(msg) if msg["type"] == "bye" => return Err(bye(&msg)),
                // Sent again after `claimed`, so an early one can go.
                Some(msg) if msg["type"] == "linear" => {}
                Some(msg) => {
                    return Err(Ended::Retry(format!(
                        "The relay said {} before challenging us",
                        msg["type"]
                    )));
                }
                None => {}
            }
        };
        let signed = claim_message(&nonce, &self.identity.label()).ok_or_else(|| {
            Ended::Retry("The relay sent a challenge with a nonce of the wrong length".into())
        })?;
        let claim = json!({
            "type": "claim",
            "pub": B64.encode(self.identity.public_raw()),
            "sig": B64.encode(self.identity.sign(&signed)),
        });
        send_control(ws, 0, &claim)?;
        loop {
            if Instant::now() > deadline {
                return Err(Ended::Retry(
                    "The relay did not answer the claim in time".into(),
                ));
            }
            match self.next_control(ws)? {
                Some(msg) if msg["type"] == "claimed" => return Ok(()),
                Some(msg) if msg["type"] == "bye" => return Err(bye(&msg)),
                // A delivery can race `claimed`. The relay sends it again
                // after, so the early one is dropped.
                Some(msg) if msg["type"] == "linear" => {}
                Some(msg) => {
                    return Err(Ended::Retry(format!(
                        "The relay said {} to our claim",
                        msg["type"]
                    )));
                }
                None => {}
            }
        }
    }

    /// The next control message on channel 0, if one arrives within a read tick.
    /// Frames on other channels before the claim are not expected and dropped.
    fn next_control(&self, ws: &mut Socket) -> Result<Option<Value>, Ended> {
        match read(ws)? {
            Read::Frame(CONTROL, 0, payload) => Ok(serde_json::from_slice::<Value>(&payload).ok()),
            _ => Ok(None),
        }
    }

    /// The claimed connection: frames in, commands out, pings in the silences.
    /// Between them the thread sleeps until the socket has something to read
    /// or the shell something to send, so neither waits on the other.
    fn talk(&mut self, ws: &mut Socket) -> Ended {
        // A second handle on the same socket, to watch it and switch it to
        // non-blocking reads while `ws` is borrowed.
        let Some(stream) = tcp(ws).and_then(|s| s.try_clone().ok()) else {
            return Ended::Retry("Could not watch the relay connection".into());
        };
        // SAFETY: the stream is registered with this poller only, and deleted
        // before it is dropped at the end of this function.
        if unsafe { self.poller.add(&stream, PollEvent::readable(SOCKET)) }.is_err() {
            return Ended::Retry("Could not watch the relay connection".into());
        }
        let ended = self.talk_on(ws, &stream);
        let _ = self.poller.delete(&stream);
        ended
    }

    fn talk_on(&mut self, ws: &mut Socket, stream: &TcpStream) -> Ended {
        let mut events = Events::new();
        // Counts sleep: a laptop that wakes gives up on the old socket at once.
        let mut last_heard = Quiet::now();
        let mut last_ping = Quiet::now();
        // The claim may have left frames read but not taken: look first.
        let mut readable = true;
        loop {
            if self.stop.load(Ordering::Relaxed) {
                return Ended::Stopped;
            }
            // Judged before reading: a pong buffered before the computer
            // slept would otherwise pass for a relay that is still there.
            if last_heard.elapsed() > SILENCE_MAX {
                return Ended::Retry("The relay went quiet".into());
            }
            // After a frame, more may sit in the TLS and WebSocket buffers
            // where the poller cannot see them, so read until nothing is left.
            let mut more = false;
            if readable {
                match read_now(ws, stream) {
                    Ok(Read::Frame(kind, channel, payload)) => {
                        last_heard = Quiet::now();
                        more = true;
                        if let Err(ended) = self.heard(ws, kind, channel, payload) {
                            return ended;
                        }
                    }
                    Ok(Read::Alive) => {
                        last_heard = Quiet::now();
                        more = true;
                    }
                    Ok(Read::Nothing) => {}
                    Err(ended) => return ended,
                }
            }
            if last_ping.elapsed() > PING_EVERY {
                last_ping = Quiet::now();
                if ws.send(Message::Ping(Vec::new().into())).is_err() {
                    return Ended::Retry("Could not write to the relay".into());
                }
            }
            loop {
                match self.commands.try_recv() {
                    Ok(cmd) => {
                        if let Err(ended) = self.obey(ws, cmd) {
                            return ended;
                        }
                    }
                    Err(std_mpsc::TryRecvError::Empty) => break,
                    Err(std_mpsc::TryRecvError::Disconnected) => return Ended::Stopped,
                }
            }
            if ws.flush().is_err() {
                return Ended::Retry("Could not write to the relay".into());
            }
            if more {
                readable = true;
                continue;
            }
            // Wake for the socket, the shell, or the next ping or silence check.
            let due = PING_EVERY
                .saturating_sub(last_ping.elapsed())
                .min(SILENCE_MAX.saturating_sub(last_heard.elapsed()))
                + Duration::from_millis(1);
            events.clear();
            if self
                .poller
                .modify(stream, PollEvent::readable(SOCKET))
                .is_err()
            {
                return Ended::Retry("Could not watch the relay connection".into());
            }
            match self.poller.wait(&mut events, Some(due)) {
                Ok(_) => {}
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
                Err(_) => return Ended::Retry("Could not watch the relay connection".into()),
            }
            readable = events.iter().any(|e| e.key == SOCKET && e.readable);
        }
    }

    fn obey(&mut self, ws: &mut Socket, cmd: Command) -> Result<(), Ended> {
        match cmd {
            // The relay checked the old session at the door: a new one needs
            // a new socket, and none means hanging up.
            Command::Session(session) => {
                let session = self.usable(session);
                if session == self.session {
                    return Ok(());
                }
                let signed_out = session.is_none();
                self.session = session;
                Err(if signed_out {
                    Ended::SignedOut
                } else {
                    Ended::Resume
                })
            }
            Command::Send { channel, text } => match self.peers.get_mut(&channel) {
                Some(Peer::Ready { session, .. }) => say(ws, channel, session, &text),
                _ => Ok(()),
            },
            Command::Broadcast(text) => {
                for (channel, peer) in &mut self.peers {
                    if let Peer::Ready { session, .. } = peer {
                        say(ws, *channel, session, &text)?;
                    }
                }
                Ok(())
            }
            Command::CloseChannel { channel, reason } => self.drop_channel(ws, channel, &reason),
        }
    }

    /// A frame from the relay. Control on channel 0 is about channels and about
    /// us; anything on another channel is a device's.
    fn heard(
        &mut self,
        ws: &mut Socket,
        kind: u8,
        channel: u32,
        payload: Vec<u8>,
    ) -> Result<(), Ended> {
        if channel != 0 {
            return self.phone_said(ws, kind, channel, payload);
        }
        if kind != CONTROL {
            return Ok(());
        }
        let Ok(msg) = serde_json::from_slice::<Value>(&payload) else {
            return Ok(());
        };
        let channel = || msg["channel"].as_u64().and_then(|c| u32::try_from(c).ok());
        match msg["type"].as_str() {
            Some("open") => {
                if let Some(channel) = channel().filter(|c| *c != 0) {
                    self.opened(ws, channel, &msg)?;
                }
            }
            // It may be one we dropped already: nothing to do then.
            Some("close") => {
                if let Some(channel) = channel() {
                    let device = self
                        .channels
                        .lock()
                        .unwrap()
                        .get(&channel)
                        .and_then(|c| c.device.clone());
                    self.forget_channel(channel);
                    // Its session ended: that id never comes back, so its
                    // record goes now, and its push subscription with it.
                    if msg["reason"] == "signed_out"
                        && let Some(device) = device
                        && self
                            .access
                            .lock()
                            .unwrap()
                            .unpair_through(&device, super::Door::Relay)
                    {
                        let _ = self.tx.unbounded_send(Incoming::Unpaired);
                    }
                }
            }
            Some("bye") => return Err(bye(&msg)),
            // A broken one is dropped: the relay sends the delivery again.
            Some("linear") => {
                if let Some(delivery) = linear::parse(&msg) {
                    let _ = self.tx.unbounded_send(Incoming::Linear(delivery));
                }
            }
            _ => {}
        }
        Ok(())
    }

    /// The relay opened a channel for a device. Only the relay's word that
    /// the device is signed in to this computer's own account lets it go on
    /// to the handshake; any other channel is closed at once.
    fn opened(&mut self, ws: &mut Socket, channel: u32, msg: &Value) -> Result<(), Ended> {
        // A relay that names no account for a device has no accounts: no
        // device would ever get in, so staying connected only misleads.
        if msg.get("account").is_none() {
            return Err(Ended::Rejected(
                "This relay does not support account sign-in.".into(),
            ));
        }
        // A number used again: the channel before it is over.
        self.forget_channel(channel);
        let ours = self.github_id != 0 && msg["account"].as_u64() == Some(self.github_id);
        let device = msg["device"].as_str().filter(|d| !d.is_empty());
        let (true, Some(device)) = (ours, device) else {
            let reason = if ours { "bad_open" } else { "not_yours" };
            return send_control(ws, channel, &json!({ "type": "close", "reason": reason }));
        };
        self.channels.lock().unwrap().insert(
            channel,
            Channel {
                device: Some(device.to_string()),
                ready: false,
            },
        );
        self.peers.insert(
            channel,
            Peer::Opened {
                device: device.to_string(),
                name: msg["name"].as_str().unwrap_or_default().to_string(),
                kind: msg["kind"].as_str().unwrap_or_default().to_string(),
            },
        );
        Ok(())
    }

    /// A device's frame on its channel: the hello first, then sealed messages.
    /// A device that does not keep to this is cut off; a bad frame on an
    /// agreed channel means a replay or tampering and ends it too. Frames on
    /// a channel the relay did not open for this account go nowhere.
    fn phone_said(
        &mut self,
        ws: &mut Socket,
        kind: u8,
        channel: u32,
        payload: Vec<u8>,
    ) -> Result<(), Ended> {
        let was_ready = matches!(self.peers.get(&channel), Some(Peer::Ready { .. }));
        let said = self.peer_said(ws, kind, channel, payload);
        // A failed write leaves the peer out of `peers`, so `forget_channels`
        // would not tell the shell this client left. A channel still listed was
        // not closed (and told) by `peer_said` itself.
        if said.is_err()
            && was_ready
            && !self.peers.contains_key(&channel)
            && self.channels.lock().unwrap().remove(&channel).is_some()
        {
            let _ = self
                .tx
                .unbounded_send(Incoming::Disconnected(ClientId::from(channel)));
        }
        said
    }

    fn peer_said(
        &mut self,
        ws: &mut Socket,
        kind: u8,
        channel: u32,
        payload: Vec<u8>,
    ) -> Result<(), Ended> {
        let Some(peer) = self.peers.remove(&channel) else {
            return Ok(());
        };
        let was_ready = matches!(peer, Peer::Ready { .. });
        let now = crate::model::unix_now();
        // The peer as it stands after the frame, or why the channel ends.
        let outcome: Result<Peer, &str> = match (kind, peer) {
            (HELLO, Peer::Opened { device, name, kind }) => {
                let host = super::host_name();
                let answered = PhoneHello::parse(&payload).and_then(|hello| {
                    Handshake::begin().answer(&hello, &self.identity, channel, &host)
                });
                match answered {
                    Ok((reply, session)) => {
                        send_frame(ws, HELLO, channel, &reply)?;
                        self.ready(channel, &device, &name, &kind, now);
                        Ok(Peer::Ready {
                            session: Box::new(session),
                            device,
                        })
                    }
                    Err(_) => Err("bad_hello"),
                }
            }
            (
                DATA,
                Peer::Ready {
                    mut session,
                    device,
                },
            ) => match session.open(&payload) {
                Err(_) => Err("bad_frame"),
                Ok(plain) => {
                    let msg = serde_json::from_slice::<Value>(&plain);
                    let mut outcome = Ok(());
                    match msg {
                        Err(_) => say(
                            ws,
                            channel,
                            &mut session,
                            &wire::error("bad message: not json"),
                        )?,
                        Ok(msg) if msg.get("req").is_some() => {
                            let req = &msg["req"];
                            let (reply, effect) = self.route(req, &device, now);
                            respond(ws, channel, &mut session, &req["id"], reply)?;
                            outcome = self.apply(ws, channel, effect)?;
                        }
                        // An old page saying who it is the old way. The relay
                        // already said, and only that counts: no answer, and
                        // no error that would end the page's channel.
                        Ok(msg) if msg.get("pair").is_some() || msg.get("auth").is_some() => {}
                        Ok(msg) => {
                            let seq = wire::seq(&msg);
                            match serde_json::from_value::<ClientMsg>(msg) {
                                Ok(ClientMsg::Ping) => {}
                                Ok(msg) => {
                                    let _ = self.tx.unbounded_send(Incoming::Message(
                                        ClientId::from(channel),
                                        msg,
                                    ));
                                }
                                Err(e) => say(
                                    ws,
                                    channel,
                                    &mut session,
                                    &wire::error(&format!("bad message: {e}")),
                                )?,
                            }
                            if let Some(seq) = seq {
                                say(ws, channel, &mut session, &wire::ack(seq))?;
                            }
                        }
                    }
                    outcome.map(|()| Peer::Ready { session, device })
                }
            },
            _ => Err("bad_frame"),
        };
        match outcome {
            Ok(next) => {
                self.peers.insert(channel, next);
            }
            Err(reason) => {
                self.channels.lock().unwrap().remove(&channel);
                if was_ready {
                    let _ = self
                        .tx
                        .unbounded_send(Incoming::Disconnected(ClientId::from(channel)));
                }
                send_control(ws, channel, &json!({ "type": "close", "reason": reason }))?;
            }
        }
        Ok(())
    }

    /// What a `req` on `asking` changed, acted on. Forgetting the device
    /// closes its other channels here and ends the asking one with the reason
    /// given back.
    fn apply(
        &mut self,
        ws: &mut Socket,
        asking: u32,
        effect: Effect,
    ) -> Result<Result<(), &'static str>, Ended> {
        match effect {
            Effect::None => {}
            Effect::Paired => {
                let _ = self.tx.unbounded_send(Incoming::Paired);
            }
            Effect::Subscribed => {
                let _ = self.tx.unbounded_send(Incoming::Subscribed);
            }
            Effect::Unpaired(device) => {
                let theirs: Vec<u32> = self
                    .channels
                    .lock()
                    .unwrap()
                    .iter()
                    .filter(|(ch, c)| **ch != asking && c.device.as_deref() == Some(&device))
                    .map(|(ch, _)| *ch)
                    .collect();
                for ch in theirs {
                    self.drop_channel(ws, ch, "unpaired")?;
                }
                let _ = self.tx.unbounded_send(Incoming::Unpaired);
                return Ok(Err("unpaired"));
            }
        }
        Ok(Ok(()))
    }

    /// A `req` from a client, answered as the HTTP it stands for, as `device`.
    fn route(&self, req: &Value, device: &str, now: u64) -> (super::Reply, Effect) {
        super::sealed_request(req, device, super::Door::Relay, &self.access, now)
    }

    /// The keys are agreed on a channel the relay opened for this account:
    /// the shell gets a client, and hears of the device first if it is new.
    fn ready(&mut self, channel: u32, device: &str, name: &str, kind: &str, now: u64) {
        let (known, seen) = self.access.lock().unwrap().relay_device(device, name, now);
        let news = match seen {
            Seen::New => Some(Incoming::NewDevice {
                name: known.name,
                kind: kind.to_string(),
            }),
            Seen::Renamed => Some(Incoming::Renamed),
            Seen::Known => None,
        };
        if let Some(news) = news {
            let _ = self.tx.unbounded_send(news);
        }
        self.channels.lock().unwrap().insert(
            channel,
            Channel {
                device: Some(device.to_string()),
                ready: true,
            },
        );
        let _ = self.tx.unbounded_send(Incoming::Connected(
            ClientId::from(channel),
            device.to_string(),
        ));
    }

    /// Ends a channel from this side: the relay passes the reason to the device.
    fn drop_channel(&mut self, ws: &mut Socket, channel: u32, reason: &str) -> Result<(), Ended> {
        if self.forget_channel(channel) {
            send_control(ws, channel, &json!({ "type": "close", "reason": reason }))?;
        }
        Ok(())
    }

    /// A channel is gone; whether it was one. The shell hears a client leave
    /// when it was one.
    fn forget_channel(&mut self, channel: u32) -> bool {
        let known = self.channels.lock().unwrap().remove(&channel).is_some();
        if let Some(Peer::Ready { .. }) = self.peers.remove(&channel) {
            let _ = self
                .tx
                .unbounded_send(Incoming::Disconnected(ClientId::from(channel)));
        }
        known
    }
}

/// A `req`'s answer, sealed and sent in pieces the relay lets through: the
/// first carries the status and type, each says whether more follow.
fn respond(
    ws: &mut Socket,
    channel: u32,
    session: &mut Session,
    id: &Value,
    reply: super::Reply,
) -> Result<(), Ended> {
    let pieces: Vec<&[u8]> = if reply.body.is_empty() {
        vec![&[]]
    } else {
        reply.body.chunks(CHUNK).collect()
    };
    let last = pieces.len() - 1;
    for (i, piece) in pieces.into_iter().enumerate() {
        let mut res = json!({ "id": id, "body": B64.encode(piece), "more": i < last });
        if i == 0 {
            res["status"] = json!(reply.status);
            res["type"] = json!(reply.content_type);
        }
        say(ws, channel, session, &json!({ "res": res }).to_string())?;
    }
    Ok(())
}

/// A message to a phone, sealed. One the relay would not let through in a
/// single frame goes as `{part}` pieces of its bytes, each saying whether more
/// follow; the phone joins them and reads the message as if it came whole.
/// The pieces of one message go out together, so they need no name.
///
/// This is the relay's own split, under any message. `wire::parts`
/// (`type: "part"`) is another one, a layer up: a server cuts a transcript
/// there so each piece is a message the shell can use by itself.
fn say(ws: &mut Socket, channel: u32, session: &mut Session, text: &str) -> Result<(), Ended> {
    if text.len() <= WHOLE {
        return send_frame(ws, DATA, channel, &session.seal(text.as_bytes()));
    }
    for part in parts(text, CHUNK) {
        send_frame(ws, DATA, channel, &session.seal(part.as_bytes()))?;
    }
    Ok(())
}

/// Joins the `{part}` pieces of [`parts`] back into the message they carry,
/// for the desktop app when it is the one a server talks to: arakun-server
/// sends through this same client, so its big messages come in pieces too.
/// The phone's page does the same in `phone/public/relay.js` (`joiner`).
#[derive(Default)]
pub struct Joiner {
    bytes: Vec<u8>,
}

impl Joiner {
    /// The most one joined message may be. Far over anything sent, and it
    /// keeps a peer from growing the buffer without end.
    const MOST: usize = 64 * 1024 * 1024;

    /// A message as it came: itself when it is whole, nothing while pieces
    /// are still due, and the joined message with the last piece.
    pub fn take(&mut self, msg: Value) -> Result<Option<Value>, &'static str> {
        let Some(part) = msg.get("part") else {
            // The pieces of one message come together: a whole one between
            // them means the rest never will.
            self.bytes.clear();
            return Ok(Some(msg));
        };
        let piece = part
            .get("body")
            .and_then(Value::as_str)
            .and_then(|body| B64.decode(body).ok())
            .ok_or("A piece of a message did not read.")?;
        if self.bytes.len() + piece.len() > Self::MOST {
            self.bytes = Vec::new();
            return Err("A message in pieces was too big.");
        }
        self.bytes.extend(piece);
        if part.get("more").and_then(Value::as_bool) == Some(true) {
            return Ok(None);
        }
        let whole = std::mem::take(&mut self.bytes);
        serde_json::from_slice(&whole)
            .map(Some)
            .map_err(|_| "A message in pieces did not read.")
    }
}

/// `text` as `{part: {body, more}}` messages that each carry `chunk` of its
/// bytes; `phone/public/relay.js` joins them (`joiner`), and [`Joiner`] does here.
pub fn parts(text: &str, chunk: usize) -> Vec<String> {
    let last = text.len().div_ceil(chunk).saturating_sub(1);
    text.as_bytes()
        .chunks(chunk)
        .enumerate()
        .map(|(i, piece)| {
            json!({ "part": { "body": B64.encode(piece), "more": i < last } }).to_string()
        })
        .collect()
}

/// What a `bye` from the relay means for the next attempt.
fn bye(msg: &Value) -> Ended {
    refused(
        msg["reason"].as_str().unwrap_or("refused"),
        msg["message"].as_str(),
    )
}

/// What `Status::Rejected` says when the account's plan has no room for this
/// computer. The words are the app's: a server, which has no relay switch,
/// knows the case by this text and says its own.
pub const MACHINES_FULL: &str = "Your account has as many computers on the relay as its plan allows. Remove one you no longer use from your account, then turn the relay off and on again.";

/// What `Status::Rejected` says when the computer was removed from its
/// account while connected. Known by its text, as `MACHINES_FULL` is.
pub const REMOVED: &str = "This computer was removed from your account. Turn the relay off and on again, or sign in again, to add it back.";

/// What the relay's reason for sending this computer away means for the next
/// attempt, with the operator's text when it came with one.
fn refused(reason: &str, message: Option<&str>) -> Ended {
    let message = message
        .map(|m| m.trim().chars().take(MAX_MESSAGE).collect::<String>())
        .filter(|m| !m.is_empty());
    let with = |text: &str| match &message {
        Some(m) => format!("{text}: {m}"),
        None => format!("{text}."),
    };
    match reason {
        "signed_out" => Ended::SignedOut,
        "wrong_label" | "key_mismatch" => {
            Ended::Rejected("This address belongs to another key.".into())
        }
        "bad_signature" => {
            Ended::Rejected("The relay did not accept this computer's signature.".into())
        }
        "label_taken" => Ended::Rejected(
            "Another account has this computer's address on the relay. Replace this computer's key to get a new address.".into(),
        ),
        "machines_full" => Ended::Rejected(MACHINES_FULL.into()),
        "no_machine" => Ended::Rejected(REMOVED.into()),
        "update" => Ended::Rejected("This relay needs a newer Arakun. Update Arakun.".into()),
        // The operator may lift a block, and the other computer may go away:
        // keep asking, slowly, so a server comes back without a restart.
        "blocked" => Ended::Refused(with("The relay operator blocked this account")),
        "replaced" => {
            Ended::Refused("Another computer with this key connected to the relay.".into())
        }
        // Closed by the operator for now: as for a drop, with their word.
        "disabled" => Ended::Retry(with("The relay is closed for now")),
        other => Ended::Retry(format!("The relay hung up: {other}")),
    }
}

/// One frame from the relay, or none within a read tick. A close is an end.
/// What one read tick brought: a frame, a sign the relay is there, or nothing yet.
enum Read {
    Frame(u8, u32, Vec<u8>),
    Alive,
    Nothing,
}

fn read(ws: &mut Socket) -> Result<Read, Ended> {
    match ws.read() {
        Ok(Message::Binary(bytes)) => {
            Ok(parse_frame(&bytes).map_or(Read::Nothing, |(k, c, p)| Read::Frame(k, c, p)))
        }
        // The pong to our ping is all the relay says while no device is around:
        // it must count as hearing from it, or every silence looks like a loss.
        Ok(Message::Pong(_) | Message::Ping(_)) => Ok(Read::Alive),
        Ok(Message::Close(frame)) => {
            let _ = ws.flush();
            let reason = frame
                .as_ref()
                .map(|f| f.reason.to_string())
                .unwrap_or_default();
            let code = frame.map(|f| u16::from(f.code));
            Err(match code {
                Some(4401 | 4403 | 4404 | 4409 | 4410 | 4429 | 4503) if !reason.is_empty() => {
                    refused(&reason, None)
                }
                _ => Ended::Retry(if reason.is_empty() {
                    "The relay closed the connection".into()
                } else {
                    reason
                }),
            })
        }
        Ok(_) => Ok(Read::Alive),
        Err(tungstenite::Error::Io(e))
            if matches!(
                e.kind(),
                std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
            ) =>
        {
            Ok(Read::Nothing)
        }
        Err(tungstenite::Error::ConnectionClosed | tungstenite::Error::AlreadyClosed) => {
            Err(Ended::Retry("The relay closed the connection".into()))
        }
        Err(e) => Err(Ended::Retry(format!("Lost the relay: {e}"))),
    }
}

/// The TCP stream under the WebSocket, TLS or not.
fn tcp(ws: &Socket) -> Option<&TcpStream> {
    match ws.get_ref() {
        MaybeTlsStream::Plain(s) => Some(s),
        MaybeTlsStream::Rustls(s) => Some(s.get_ref()),
        _ => None,
    }
}

/// A read that takes what has arrived and does not wait for more. Writes stay
/// blocking, so a frame is never half sent.
fn read_now(ws: &mut Socket, stream: &TcpStream) -> Result<Read, Ended> {
    if stream.set_nonblocking(true).is_err() {
        return Err(Ended::Retry("Could not read from the relay".into()));
    }
    let got = read(ws);
    let _ = stream.set_nonblocking(false);
    got
}

fn send_control(ws: &mut Socket, channel: u32, msg: &Value) -> Result<(), Ended> {
    send_frame(ws, CONTROL, channel, msg.to_string().as_bytes())
}

fn send_frame(ws: &mut Socket, kind: u8, channel: u32, payload: &[u8]) -> Result<(), Ended> {
    ws.send(Message::Binary(frame(kind, channel, payload).into()))
        .map_err(|e| Ended::Retry(format!("Could not write to the relay: {e}")))
}

/// A computer frame: type, channel (big-endian), payload.
pub fn frame(kind: u8, channel: u32, payload: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(5 + payload.len());
    out.push(kind);
    out.extend_from_slice(&channel.to_be_bytes());
    out.extend_from_slice(payload);
    out
}

pub fn parse_frame(bytes: &[u8]) -> Option<(u8, u32, Vec<u8>)> {
    if bytes.len() < 5 || !matches!(bytes[0], HELLO | DATA | CONTROL) {
        return None;
    }
    let channel = u32::from_be_bytes([bytes[1], bytes[2], bytes[3], bytes[4]]);
    Some((bytes[0], channel, bytes[5..].to_vec()))
}

/// Host, port and whether TLS is on, out of a `ws://` or `wss://` address.
fn parse_url(url: &str) -> Option<(String, u16, bool)> {
    let (scheme, rest) = url.split_once("://")?;
    let secure = match scheme {
        "wss" => true,
        "ws" => false,
        _ => return None,
    };
    let host_port = rest.split(['/', '?', '#']).next()?;
    let (host, port) = match host_port.rsplit_once(':') {
        Some((h, p)) if p.bytes().all(|b| b.is_ascii_digit()) && !p.is_empty() => {
            (h, p.parse().ok()?)
        }
        _ => (host_port, if secure { 443 } else { 80 }),
    };
    Some((host.trim_matches(['[', ']']).to_string(), port, secure))
}

/// What the computer signs to claim its label: `CLAIM_TAG ‖ nonce ‖ label`,
/// or nothing when the nonce is not the length the relay issues.
pub(super) fn claim_message(nonce: &[u8], label: &str) -> Option<Vec<u8>> {
    if nonce.len() != CLAIM_NONCE_LEN {
        return None;
    }
    let mut out = Vec::with_capacity(CLAIM_TAG.len() + CLAIM_NONCE_LEN + label.len());
    out.extend_from_slice(CLAIM_TAG);
    out.extend_from_slice(nonce);
    out.extend_from_slice(label.as_bytes());
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ring::signature;
    use std::net::TcpListener;
    use tungstenite::protocol::frame::coding::CloseCode;

    #[test]
    fn the_endpoint_is_the_machine_door_on_the_relay_host() {
        assert_eq!(
            endpoint("https://app.arakun.dev", "abc"),
            "wss://app.arakun.dev/machines/abc/pc"
        );
        assert_eq!(
            endpoint("https://example.workers.dev/", "abc"),
            "wss://example.workers.dev/machines/abc/pc"
        );
        assert_eq!(
            endpoint("relay.example", "abc"),
            "wss://relay.example/machines/abc/pc"
        );
        assert_eq!(
            endpoint("http://127.0.0.1:8787", "abc"),
            "ws://127.0.0.1:8787/machines/abc/pc"
        );
        assert_eq!(
            endpoint("http://localhost:8787/", "abc"),
            "ws://localhost:8787/machines/abc/pc"
        );
        assert_eq!(
            endpoint("https://127.0.0.1:8443", "abc"),
            "wss://127.0.0.1:8443/machines/abc/pc"
        );
        // A host by name always goes over TLS, whatever the base says.
        assert_eq!(
            endpoint("http://relay.example", "abc"),
            "wss://relay.example/machines/abc/pc"
        );
        assert_eq!(
            page_url("https://app.arakun.dev/"),
            "https://app.arakun.dev/"
        );
        assert_eq!(page_url("http://127.0.0.1:8787"), "http://127.0.0.1:8787/");
    }

    #[test]
    fn a_relay_address_without_a_scheme_takes_the_session_of_its_https_server() {
        let account = "https://app.arakun.dev";
        for relay in [
            "app.arakun.dev",
            "app.arakun.dev/",
            "https://APP.arakun.dev/",
            " https://app.arakun.dev ",
        ] {
            assert!(takes_session_from(relay, account), "{relay}");
        }
        assert!(!takes_session_from("relay.example", account));
        assert!(!takes_session_from("app.arakun.dev:8443", account));
        assert!(takes_session_from(
            "http://localhost:8787",
            "http://localhost:8787"
        ));
    }

    #[test]
    fn frames_round_trip() {
        let f = frame(DATA, 7, b"hi");
        assert_eq!(f, [0x02, 0, 0, 0, 7, b'h', b'i']);
        assert_eq!(parse_frame(&f), Some((DATA, 7, b"hi".to_vec())));
        assert_eq!(parse_frame(&[0x02, 0, 0]), None);
        assert_eq!(parse_frame(&[0x09, 0, 0, 0, 1]), None);
        assert_eq!(
            parse_url("wss://abc.arakun.dev/pc"),
            Some(("abc.arakun.dev".into(), 443, true))
        );
        assert_eq!(
            parse_url("ws://127.0.0.1:8787/machines/abc/pc?name=x&kind=app"),
            Some(("127.0.0.1".into(), 8787, false))
        );
        assert_eq!(parse_url("https://x"), None);
    }

    /// The most the relay takes in one frame's payload (`cloud/src/relay/limits.ts`).
    const RELAY_FRAME: usize = 256 * 1024;
    // Sealing adds a counter and a tag; the relay counts those too.
    const _: () = assert!(WHOLE + 64 <= RELAY_FRAME);

    /// The account this computer is signed in to in the tests.
    const ACCOUNT: u64 = 42;

    fn machine() -> Machine {
        Machine {
            name: "Rik's laptop".into(),
            kind: MachineKind::App,
        }
    }

    fn account(session: Option<&str>) -> Account {
        Account {
            github_id: ACCOUNT,
            session: session.map(str::to_string),
        }
    }

    fn empty_access() -> Shared {
        super::super::Access::new(Vec::new(), "pk".into())
    }

    /// A client of `relay`, signed in with the session `tok`.
    fn start(
        relay: &FakeRelay,
        me: Identity,
        access: Shared,
    ) -> (RelayClient, mpsc::UnboundedReceiver<Incoming>) {
        RelayClient::start(&relay.base(), me, access, machine(), account(Some("tok")))
    }

    /// What the computer asked for when it opened its socket.
    struct Door {
        /// The path with its query.
        target: String,
        authorization: Option<String>,
    }

    /// A relay of our own on the loopback: accepts one computer at a time.
    struct FakeRelay {
        listener: TcpListener,
        label: String,
    }

    impl FakeRelay {
        fn start(label: &str) -> FakeRelay {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            FakeRelay {
                listener,
                label: label.into(),
            }
        }

        fn base(&self) -> String {
            format!(
                "http://127.0.0.1:{}",
                self.listener.local_addr().unwrap().port()
            )
        }

        /// Takes the next socket, noting what it asked for. The callback's
        /// error type is tungstenite's.
        #[allow(clippy::result_large_err)]
        fn accept(&self) -> (WebSocket<TcpStream>, Door) {
            self.listener.set_nonblocking(false).unwrap();
            let (stream, _) = self.listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let mut door = Door {
                target: String::new(),
                authorization: None,
            };
            let ws = tungstenite::accept_hdr(
                stream,
                |req: &tungstenite::handshake::server::Request,
                 res: tungstenite::handshake::server::Response| {
                    door.target = req.uri().to_string();
                    door.authorization = req
                        .headers()
                        .get("authorization")
                        .and_then(|v| v.to_str().ok())
                        .map(str::to_string);
                    Ok(res)
                },
            )
            .unwrap();
            (ws, door)
        }

        /// Takes the next computer, challenges it and checks the claim; gives
        /// the socket back claimed.
        fn accept_and_claim(&self, public: &[u8]) -> (WebSocket<TcpStream>, Door) {
            let (mut ws, door) = self.accept();
            let nonce = [7u8; 16];
            tell(
                &mut ws,
                json!({"type": "challenge", "nonce": B64.encode(nonce)}),
            );
            let claim = control(&mut ws);
            assert_eq!(claim["type"], "claim");
            assert_eq!(B64.decode(claim["pub"].as_str().unwrap()).unwrap(), public);
            let sig = B64.decode(claim["sig"].as_str().unwrap()).unwrap();
            let mut signed = b"arakun-relay-claim-v1\0".to_vec();
            signed.extend_from_slice(&nonce);
            signed.extend_from_slice(self.label.as_bytes());
            signature::UnparsedPublicKey::new(&signature::ECDSA_P256_SHA256_FIXED, public)
                .verify(&signed, &sig)
                .unwrap();
            tell(&mut ws, json!({"type": "claimed"}));
            (ws, door)
        }

        /// Turns the next socket away as the Worker does at its door: accepted,
        /// then a `bye` with the reason, then closed.
        fn refuse(&self, bye: Value, code: u16) -> Door {
            let (mut ws, door) = self.accept();
            let reason = bye["reason"].as_str().unwrap().to_string();
            tell(&mut ws, bye);
            hang_up(&mut ws, code, &reason);
            door
        }

        /// Nobody dials within `wait`.
        fn no_dial_within(&self, wait: Duration) {
            self.listener.set_nonblocking(true).unwrap();
            std::thread::sleep(wait);
            assert!(self.listener.accept().is_err(), "the computer dialed again");
            self.listener.set_nonblocking(false).unwrap();
        }
    }

    fn hang_up(ws: &mut WebSocket<TcpStream>, code: u16, reason: &str) {
        ws.close(Some(tungstenite::protocol::CloseFrame {
            code: CloseCode::from(code),
            reason: reason.to_string().into(),
        }))
        .unwrap();
        let _ = ws.flush();
    }

    /// The next control message on channel 0; pings are answered on the way.
    fn control(ws: &mut WebSocket<TcpStream>) -> Value {
        loop {
            match ws.read().unwrap() {
                Message::Binary(b) => {
                    let (kind, channel, payload) = parse_frame(&b).unwrap();
                    assert_eq!((kind, channel), (CONTROL, 0));
                    return serde_json::from_slice(&payload).unwrap();
                }
                Message::Ping(_) | Message::Pong(_) => continue,
                other => panic!("unexpected {other:?}"),
            }
        }
    }

    fn tell(ws: &mut WebSocket<TcpStream>, msg: Value) {
        ws.send(Message::Binary(
            frame(CONTROL, 0, msg.to_string().as_bytes()).into(),
        ))
        .unwrap();
    }

    /// The relay opens `channel` for a device of `account`, as the Worker does.
    fn open(ws: &mut WebSocket<TcpStream>, channel: u32, device: &str, account: Value) {
        tell(
            ws,
            json!({"type": "open", "channel": channel, "device": device, "name": "Rik's iPhone", "kind": "ios", "account": account}),
        );
    }

    fn next_status(rx: &mut mpsc::UnboundedReceiver<Incoming>) -> Status {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            match rx.try_recv() {
                Ok(Incoming::Relay(s)) => return s,
                Ok(_) => {}
                Err(mpsc::TryRecvError::Closed) => panic!("the client hung up"),
                Err(mpsc::TryRecvError::Empty) => {
                    assert!(Instant::now() < deadline, "no status in time");
                    std::thread::sleep(Duration::from_millis(10));
                }
            }
        }
    }

    fn until<T>(mut f: impl FnMut() -> Option<T>) -> T {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if let Some(v) = f() {
                return v;
            }
            assert!(Instant::now() < deadline, "did not happen in time");
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    /// The next frame from the computer on any channel; pings are answered on the way.
    fn next_frame(ws: &mut WebSocket<TcpStream>) -> (u8, u32, Vec<u8>) {
        loop {
            match ws.read().unwrap() {
                Message::Binary(b) => return parse_frame(&b).unwrap(),
                Message::Ping(_) | Message::Pong(_) => continue,
                other => panic!("unexpected {other:?}"),
            }
        }
    }

    /// A phone in Rust, on one channel of the fake relay: it speaks `e2e` like
    /// the page does.
    struct FakePhone {
        channel: u32,
        session: Session,
    }

    impl FakePhone {
        /// Has the relay open `channel` for `device` of this account, shakes
        /// hands and checks the computer's hello is from `label`.
        fn greet(
            ws: &mut WebSocket<TcpStream>,
            label: &str,
            channel: u32,
            device: &str,
        ) -> FakePhone {
            open(ws, channel, device, json!(ACCOUNT));
            let hs = Handshake::begin();
            ws.send(Message::Binary(frame(HELLO, channel, &hs.hello()).into()))
                .unwrap();
            let (kind, ch, reply) = next_frame(ws);
            assert_eq!((kind, ch), (HELLO, channel));
            let session = hs.finish(&reply, label, channel).unwrap();
            FakePhone { channel, session }
        }

        fn say(&mut self, ws: &mut WebSocket<TcpStream>, msg: Value) {
            let sealed = self.session.seal(msg.to_string().as_bytes());
            ws.send(Message::Binary(frame(DATA, self.channel, &sealed).into()))
                .unwrap();
        }

        /// The next sealed message from the computer on this channel, as it
        /// came: one frame, which must be one the relay lets through.
        fn hear_frame(&mut self, ws: &mut WebSocket<TcpStream>) -> Value {
            let (kind, ch, payload) = next_frame(ws);
            assert_eq!(
                (kind, ch),
                (DATA, self.channel),
                "{}",
                String::from_utf8_lossy(&payload)
            );
            assert!(payload.len() <= RELAY_FRAME, "{} bytes", payload.len());
            serde_json::from_slice(&self.session.open(&payload).unwrap()).unwrap()
        }

        /// The next message from the computer on this channel, its `part`
        /// pieces joined as the page joins them.
        fn hear(&mut self, ws: &mut WebSocket<TcpStream>) -> Value {
            let first = self.hear_frame(ws);
            if first.get("part").is_none() {
                return first;
            }
            let mut whole = Vec::new();
            let mut msg = first;
            loop {
                let part = &msg["part"];
                whole.extend(B64.decode(part["body"].as_str().unwrap()).unwrap());
                if part["more"] != true {
                    return serde_json::from_slice(&whole).unwrap();
                }
                msg = self.hear_frame(ws);
                assert!(
                    msg.get("part").is_some(),
                    "pieces of one message stay together"
                );
            }
        }

        /// A `req` and its whole answer: status and body, pieces joined.
        fn req(
            &mut self,
            ws: &mut WebSocket<TcpStream>,
            method: &str,
            path: &str,
            body: Option<&str>,
        ) -> (u64, Vec<u8>) {
            self.say(
                ws,
                json!({"req": {"id": 9, "method": method, "path": path, "body": body}}),
            );
            let mut status = 0;
            let mut out = Vec::new();
            loop {
                // As it came: a `res` piece is small enough to go whole.
                let msg = self.hear_frame(ws);
                let res = &msg["res"];
                assert_eq!(res["id"], 9);
                if let Some(s) = res["status"].as_u64() {
                    status = s;
                }
                out.extend(B64.decode(res["body"].as_str().unwrap()).unwrap());
                if res["more"] != true {
                    return (status, out);
                }
            }
        }
    }

    /// The next control message the computer sent to the relay about `channel`.
    fn closed(ws: &mut WebSocket<TcpStream>, channel: u32) -> String {
        let (kind, ch, payload) = next_frame(ws);
        assert_eq!(
            (kind, ch),
            (CONTROL, channel),
            "{}",
            String::from_utf8_lossy(&payload)
        );
        let msg: Value = serde_json::from_slice(&payload).unwrap();
        assert_eq!(msg["type"], "close");
        msg["reason"].as_str().unwrap().to_string()
    }

    fn next_incoming(rx: &mut mpsc::UnboundedReceiver<Incoming>) -> Incoming {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            match rx.try_recv() {
                Ok(Incoming::Relay(_)) => {}
                Ok(msg) => return msg,
                Err(mpsc::TryRecvError::Closed) => panic!("the client hung up"),
                Err(mpsc::TryRecvError::Empty) => {
                    assert!(Instant::now() < deadline, "nothing came in time");
                    std::thread::sleep(Duration::from_millis(10));
                }
            }
        }
    }

    /// The shell hears a device it has not seen before, then a client.
    fn new_device_connects(rx: &mut mpsc::UnboundedReceiver<Incoming>, channel: u32) {
        match next_incoming(rx) {
            Incoming::NewDevice { name, kind } => {
                assert_eq!((name.as_str(), kind.as_str()), ("Rik's iPhone", "ios"))
            }
            _ => panic!("expected a new device"),
        }
        assert!(matches!(next_incoming(rx), Incoming::Connected(c, _) if c == u64::from(channel)));
    }

    #[test]
    fn the_computer_brings_its_session_and_claims_its_label() {
        let me = Identity::generate();
        let label = me.label();
        let public = me.public_raw().to_vec();
        let relay = FakeRelay::start(&label);
        let (client, mut rx) = start(&relay, me, empty_access());
        assert_eq!(next_status(&mut rx), Status::Connecting);
        // Devices find it on the relay's own page now, not under a host of its own.
        assert_eq!(client.page_url(), format!("{}/", relay.base()));

        let (mut ws, door) = relay.accept_and_claim(&public);
        assert_eq!(
            door.target,
            format!("/machines/{label}/pc?name=Rik%27s%20laptop&kind=app")
        );
        assert_eq!(door.authorization.as_deref(), Some("Bearer tok"));
        assert_eq!(next_status(&mut rx), Status::Connected);

        // No tickets and no pairing follow the claim: the computer is quiet
        // until a device comes. A close for a channel it never had is no harm.
        tell(
            &mut ws,
            json!({"type": "close", "channel": 9, "reason": "gone"}),
        );
        open(&mut ws, 4, "sess1", json!(ACCOUNT));
        until(|| (client.channels().len() == 1).then_some(()));
        client.close_channel(4, "done");
        let (kind, channel, payload) = next_frame(&mut ws);
        assert_eq!((kind, channel), (CONTROL, 4));
        assert_eq!(
            serde_json::from_slice::<Value>(&payload).unwrap()["reason"],
            "done"
        );

        drop(client);
        assert!(
            matches!(ws.read(), Ok(Message::Close(_)) | Err(_)),
            "the relay hears the computer hang up"
        );
    }

    #[test]
    fn a_device_of_the_account_is_a_client_once_the_keys_are_agreed() {
        let me = Identity::generate();
        let label = me.label();
        let public = me.public_raw().to_vec();
        let relay = FakeRelay::start(&label);
        let access = empty_access();
        let (client, mut rx) = start(&relay, me, access.clone());
        let (mut ws, _) = relay.accept_and_claim(&public);

        // Opened by the relay, but no keys yet: not a client.
        open(&mut ws, 5, "sess1", json!(ACCOUNT));
        until(|| (client.channels().len() == 1).then_some(()));
        assert_eq!(
            client.channels()[&5],
            Channel {
                device: Some("sess1".into()),
                ready: false
            }
        );
        assert_eq!(client.client_count(), 0);
        assert_eq!(client.device_of(5), None);
        assert!(access.lock().unwrap().devices.is_empty());

        // The handshake is all it takes: no pairing, no token.
        let hs = Handshake::begin();
        ws.send(Message::Binary(frame(HELLO, 5, &hs.hello()).into()))
            .unwrap();
        let (kind, ch, reply) = next_frame(&mut ws);
        assert_eq!((kind, ch), (HELLO, 5));
        let mut phone = FakePhone {
            channel: 5,
            session: hs.finish(&reply, &label, 5).unwrap(),
        };
        new_device_connects(&mut rx, 5);
        assert_eq!(client.device_of(5), Some("sess1".into()));
        assert_eq!(client.connected_devices(), vec!["sess1".to_string()]);
        assert_eq!(client.client_count(), 1);
        let device = access.lock().unwrap().devices[0].clone();
        assert_eq!(device.id, "sess1");
        assert_eq!(device.name, "Rik's iPhone");
        assert_eq!(
            (device.token_hash.as_str(), device.ticket_hash.as_str()),
            ("", "")
        );

        // What it would ask over HTTP goes inside, as this device.
        let (status, body) = phone.req(&mut ws, "GET", "/me", None);
        assert_eq!(status, 200);
        let me: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(me["id"], "sess1");
        assert_eq!(me["push_key"], "pk");

        // What it would say on its socket goes inside too, both ways.
        phone.say(&mut ws, json!({"type": "ping"}));
        phone.say(&mut ws, json!({"type": "usage"}));
        assert!(matches!(
            next_incoming(&mut rx),
            Incoming::Message(5, ClientMsg::Usage)
        ));
        client.send(5, r#"{"type":"inbox","sessions":[]}"#.into());
        assert_eq!(phone.hear(&mut ws)["type"], "inbox");
        client.broadcast(r#"{"type":"usage"}"#);
        assert_eq!(phone.hear(&mut ws)["type"], "usage");
        phone.say(&mut ws, json!({"type": "nonsense"}));
        assert_eq!(phone.hear(&mut ws)["type"], "error");

        // The same device on a second channel: a client again, not a new device.
        let _again = FakePhone::greet(&mut ws, &label, 6, "sess1");
        assert!(matches!(next_incoming(&mut rx), Incoming::Connected(6, d) if d == "sess1"));
        assert_eq!(client.client_count(), 2);
        assert_eq!(access.lock().unwrap().devices.len(), 1);

        // The relay closes one; a second close for it is no harm.
        tell(
            &mut ws,
            json!({"type": "close", "channel": 5, "reason": "closed"}),
        );
        assert!(matches!(next_incoming(&mut rx), Incoming::Disconnected(5)));
        tell(
            &mut ws,
            json!({"type": "close", "channel": 5, "reason": "closed"}),
        );

        // Forgetting the device closes its channel and drops its record.
        access.lock().unwrap().unpair("sess1");
        client.disconnect_device("sess1");
        assert_eq!(closed(&mut ws, 6), "unpaired");
        assert!(matches!(next_incoming(&mut rx), Incoming::Disconnected(6)));
        assert_eq!(client.client_count(), 0);

        // It comes back when it connects again, and is new again.
        let _back = FakePhone::greet(&mut ws, &label, 7, "sess1");
        new_device_connects(&mut rx, 7);
        assert_eq!(access.lock().unwrap().devices.len(), 1);
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn a_device_signed_out_elsewhere_is_forgotten_when_the_relay_says_so() {
        let me = Identity::generate();
        let label = me.label();
        let public = me.public_raw().to_vec();
        let relay = FakeRelay::start(&label);
        let access = empty_access();
        let (_client, mut rx) = start(&relay, me, access.clone());
        let (mut ws, _) = relay.accept_and_claim(&public);
        let _phone = FakePhone::greet(&mut ws, &label, 5, "sess1");
        new_device_connects(&mut rx, 5);
        let _other = FakePhone::greet(&mut ws, &label, 6, "sess2");
        assert!(matches!(next_incoming(&mut rx), Incoming::NewDevice { .. }));
        assert!(matches!(next_incoming(&mut rx), Incoming::Connected(6, _)));

        // A plain close keeps the record.
        tell(
            &mut ws,
            json!({"type": "close", "channel": 6, "reason": "closed"}),
        );
        assert!(matches!(next_incoming(&mut rx), Incoming::Disconnected(6)));
        // A sign-out drops it: the device goes, the other stays.
        tell(
            &mut ws,
            json!({"type": "close", "channel": 5, "reason": "signed_out"}),
        );
        assert!(matches!(next_incoming(&mut rx), Incoming::Disconnected(5)));
        assert!(matches!(next_incoming(&mut rx), Incoming::Unpaired));
        let left: Vec<String> = access
            .lock()
            .unwrap()
            .devices
            .iter()
            .map(|d| d.id.clone())
            .collect();
        assert_eq!(left, ["sess2"]);
    }

    /// A delivery as the relay sends it, with its own `id`.
    fn delivery(id: &str) -> Value {
        json!({
            "type": "linear",
            "id": id,
            "kind": "created",
            "agent_session": "5b0c1f9e-0000-4000-8000-000000000001",
            "issue": "5b0c1f9e-0000-4000-8000-000000000002",
            "organization": "5b0c1f9e-0000-4000-8000-000000000003",
            "activity": null,
            "body": null,
        })
    }

    const FIRST: &str = "0123456789abcdef0123456789abcdef";
    const SECOND: &str = "fedcba9876543210fedcba9876543210";

    fn next_delivery(rx: &mut mpsc::UnboundedReceiver<Incoming>) -> linear::Delivery {
        match next_incoming(rx) {
            Incoming::Linear(d) => d,
            _ => panic!("expected a Linear delivery"),
        }
    }

    #[test]
    fn a_linear_delivery_reaches_the_app() {
        let me = Identity::generate();
        let label = me.label();
        let public = me.public_raw().to_vec();
        let relay = FakeRelay::start(&label);
        let (_client, mut rx) = start(&relay, me, empty_access());
        let (mut ws, _) = relay.accept_and_claim(&public);

        tell(&mut ws, delivery(FIRST));
        assert_eq!(
            next_delivery(&mut rx),
            linear::Delivery {
                id: FIRST.into(),
                kind: linear::DeliveryKind::Created,
                agent_session: "5b0c1f9e-0000-4000-8000-000000000001".into(),
                issue: "5b0c1f9e-0000-4000-8000-000000000002".into(),
                organization: "5b0c1f9e-0000-4000-8000-000000000003".into(),
                activity: None,
                reason: None,
                comment: None,
                repository: None,
                body: None,
                guidance: Vec::new(),
            }
        );
    }

    #[test]
    fn a_malformed_linear_delivery_is_dropped_and_the_connection_stays() {
        let me = Identity::generate();
        let label = me.label();
        let public = me.public_raw().to_vec();
        let relay = FakeRelay::start(&label);
        let (_client, mut rx) = start(&relay, me, empty_access());
        let (mut ws, _) = relay.accept_and_claim(&public);
        assert_eq!(next_status(&mut rx), Status::Connecting);
        assert_eq!(next_status(&mut rx), Status::Connected);

        let mut bad = delivery(FIRST);
        bad["id"] = json!("not hex");
        tell(&mut ws, bad);
        let mut bad = delivery(FIRST);
        bad["kind"] = json!("deleted");
        tell(&mut ws, bad);
        // The next good one comes first, down the same socket.
        tell(&mut ws, delivery(SECOND));
        assert_eq!(next_delivery(&mut rx).id, SECOND);
        assert!(rx.try_recv().is_err(), "nothing else, and no status change");
    }

    #[test]
    fn a_linear_delivery_before_claimed_does_not_break_the_claim() {
        let me = Identity::generate();
        let label = me.label();
        let relay = FakeRelay::start(&label);
        let (_client, mut rx) = start(&relay, me, empty_access());
        assert_eq!(next_status(&mut rx), Status::Connecting);

        let (mut ws, _) = relay.accept();
        tell(
            &mut ws,
            json!({"type": "challenge", "nonce": B64.encode([7u8; 16])}),
        );
        assert_eq!(control(&mut ws)["type"], "claim");
        // In a narrow race the relay sends one before `claimed`. It sends it
        // again after, so the early one is dropped.
        tell(&mut ws, delivery(FIRST));
        tell(&mut ws, json!({"type": "claimed"}));
        assert_eq!(next_status(&mut rx), Status::Connected);
        tell(&mut ws, delivery(SECOND));
        assert_eq!(next_delivery(&mut rx).id, SECOND);
        assert!(rx.try_recv().is_err(), "the early one never comes");
    }

    #[test]
    fn a_device_of_another_account_is_turned_away() {
        let me = Identity::generate();
        let label = me.label();
        let public = me.public_raw().to_vec();
        let relay = FakeRelay::start(&label);
        let access = empty_access();
        let (client, mut rx) = start(&relay, me, access.clone());
        let (mut ws, _) = relay.accept_and_claim(&public);

        open(&mut ws, 3, "theirs", json!(43));
        assert_eq!(closed(&mut ws, 3), "not_yours");
        // An account not as a number, or none at all: not ours either.
        open(&mut ws, 4, "x", json!(null));
        assert_eq!(closed(&mut ws, 4), "not_yours");
        open(&mut ws, 8, "y", json!("42"));
        assert_eq!(closed(&mut ws, 8), "not_yours");
        assert!(client.channels().is_empty());

        // Its frames go nowhere: no hello comes back on that channel, and
        // data on a channel the relay never opened is dropped too.
        let hs = Handshake::begin();
        ws.send(Message::Binary(frame(HELLO, 3, &hs.hello()).into()))
            .unwrap();
        ws.send(Message::Binary(frame(DATA, 3, b"{\"pair\":{}}").into()))
            .unwrap();
        ws.send(Message::Binary(frame(DATA, 11, b"{\"auth\":{}}").into()))
            .unwrap();

        // The next thing the computer says is to a device of its own account.
        let _ours = FakePhone::greet(&mut ws, &label, 5, "mine");
        new_device_connects(&mut rx, 5);
        assert_eq!(client.client_count(), 1);
        let ids: Vec<String> = access
            .lock()
            .unwrap()
            .devices
            .iter()
            .map(|d| d.id.clone())
            .collect();
        assert_eq!(ids, ["mine"]);
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn pair_and_auth_on_a_relay_channel_say_nothing_about_who_it_is() {
        let me = Identity::generate();
        let label = me.label();
        let public = me.public_raw().to_vec();
        let relay = FakeRelay::start(&label);
        let access = empty_access();
        let (client, mut rx) = start(&relay, me, access.clone());
        let (mut ws, _) = relay.accept_and_claim(&public);
        // A pairing open on the network meanwhile.
        let code = access.lock().unwrap().begin_pairing();

        let mut phone = FakePhone::greet(&mut ws, &label, 5, "sess1");
        new_device_connects(&mut rx, 5);
        // An old page says who it is the old way: ignored, no answer, no error.
        phone.say(
            &mut ws,
            json!({"pair": {"secret": code, "code": code, "name": "Mallory"}}),
        );
        phone.say(&mut ws, json!({"auth": {"token": "stolen"}}));
        phone.say(&mut ws, json!({"type": "usage"}));
        assert!(matches!(
            next_incoming(&mut rx),
            Incoming::Message(5, ClientMsg::Usage)
        ));
        // Nothing was sent on the channel before this.
        client.send(5, r#"{"type":"inbox","sessions":[]}"#.into());
        assert_eq!(phone.hear(&mut ws)["type"], "inbox");

        // Pairing as HTTP is the network's, not the relay's.
        let (status, _) = phone.req(
            &mut ws,
            "POST",
            "/pair",
            Some(&format!(r#"{{"code":"{code}","name":"Mallory"}}"#)),
        );
        assert_eq!(status, 404);
        assert_eq!(access.lock().unwrap().devices.len(), 1);
        assert!(
            access.lock().unwrap().pairing().is_some(),
            "the network's code was not spent"
        );
        assert_eq!(client.client_count(), 1);
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn a_dropped_connection_is_dialed_again() {
        let me = Identity::generate();
        let public = me.public_raw().to_vec();
        let relay = FakeRelay::start(&me.label());
        let (client, mut rx) = start(&relay, me, empty_access());
        let (mut ws, _) = relay.accept_and_claim(&public);
        assert_eq!(next_status(&mut rx), Status::Connecting);
        assert_eq!(next_status(&mut rx), Status::Connected);
        open(&mut ws, 1, "d", json!(ACCOUNT));
        until(|| (client.channels().len() == 1).then_some(()));

        ws.close(None).unwrap();
        let _ = ws.flush();
        drop(ws);
        let status = next_status(&mut rx);
        assert!(matches!(status, Status::Reconnecting { .. }), "{status:?}");
        assert!(client.channels().is_empty());

        let (_ws, door) = relay.accept_and_claim(&public);
        assert_eq!(door.authorization.as_deref(), Some("Bearer tok"));
        assert_eq!(next_status(&mut rx), Status::Connecting);
        assert_eq!(next_status(&mut rx), Status::Connected);
    }

    #[test]
    fn a_signed_out_computer_dials_only_once_it_has_a_session() {
        let me = Identity::generate();
        let public = me.public_raw().to_vec();
        let relay = FakeRelay::start(&me.label());
        let (client, mut rx) =
            RelayClient::start(&relay.base(), me, empty_access(), machine(), account(None));
        assert_eq!(next_status(&mut rx), Status::SignedOut);
        relay.no_dial_within(Duration::from_millis(400));
        client.set_session(None);
        relay.no_dial_within(Duration::from_millis(200));

        client.set_session(Some("fresh".into()));
        let (mut ws, door) = relay.accept_and_claim(&public);
        assert_eq!(door.authorization.as_deref(), Some("Bearer fresh"));
        assert_eq!(next_status(&mut rx), Status::Connecting);
        assert_eq!(next_status(&mut rx), Status::Connected);

        // The relay's recheck finds the session signed out.
        tell(&mut ws, json!({"type": "bye", "reason": "signed_out"}));
        hang_up(&mut ws, 4401, "signed_out");
        assert_eq!(next_status(&mut rx), Status::SignedOut);
        relay.no_dial_within(Duration::from_millis(400));

        client.set_session(Some("again".into()));
        let (mut ws, door) = relay.accept_and_claim(&public);
        assert_eq!(door.authorization.as_deref(), Some("Bearer again"));
        assert_eq!(next_status(&mut rx), Status::Connecting);
        assert_eq!(next_status(&mut rx), Status::Connected);

        // Signing out here hangs up.
        client.set_session(None);
        assert_eq!(next_status(&mut rx), Status::SignedOut);
        assert!(matches!(ws.read(), Ok(Message::Close(_)) | Err(_)));
        relay.no_dial_within(Duration::from_millis(200));
    }

    /// What the computer makes of a refusal at the relay's door.
    fn refused_at_the_door(bye: Value, code: u16) -> (Status, FakeRelay, RelayClient) {
        let me = Identity::generate();
        let relay = FakeRelay::start(&me.label());
        let (client, mut rx) = start(&relay, me, empty_access());
        relay.refuse(bye, code);
        assert_eq!(next_status(&mut rx), Status::Connecting);
        (next_status(&mut rx), relay, client)
    }

    #[test]
    fn each_refusal_at_the_door_says_what_to_do() {
        let (status, relay, _client) =
            refused_at_the_door(json!({"type": "bye", "reason": "signed_out"}), 4401);
        assert_eq!(status, Status::SignedOut);
        relay.no_dial_within(Duration::from_millis(300));

        let (status, _, _client) = refused_at_the_door(
            json!({"type": "bye", "reason": "blocked", "message": "Too many frames."}),
            4403,
        );
        assert_eq!(
            status,
            Status::Refused {
                why: "The relay operator blocked this account: Too many frames.".into(),
                retry_in: REFUSED_WAIT,
            }
        );
        let (status, _, _client) =
            refused_at_the_door(json!({"type": "bye", "reason": "blocked"}), 4403);
        assert_eq!(
            status,
            Status::Refused {
                why: "The relay operator blocked this account.".into(),
                retry_in: REFUSED_WAIT,
            }
        );

        let (status, relay, _client) =
            refused_at_the_door(json!({"type": "bye", "reason": "label_taken"}), 4403);
        assert_eq!(
            status,
            Status::Rejected(
                "Another account has this computer's address on the relay. Replace this computer's key to get a new address.".into()
            )
        );
        relay.no_dial_within(Duration::from_millis(300));

        let (status, _, _client) =
            refused_at_the_door(json!({"type": "bye", "reason": "machines_full"}), 4429);
        assert_eq!(
            status,
            Status::Rejected(
                "Your account has as many computers on the relay as its plan allows. Remove one you no longer use from your account, then turn the relay off and on again.".into()
            )
        );

        let (status, _, _client) =
            refused_at_the_door(json!({"type": "bye", "reason": "update"}), 4410);
        assert_eq!(
            status,
            Status::Rejected("This relay needs a newer Arakun. Update Arakun.".into())
        );

        // The operator closed the relay for now: dial again as after a drop.
        let (status, relay, _client) = refused_at_the_door(
            json!({"type": "bye", "reason": "disabled", "message": "Back at noon."}),
            4503,
        );
        assert_eq!(
            status,
            Status::Reconnecting {
                why: "The relay is closed for now: Back at noon.".into(),
                retry_in: RETRY_MIN,
            }
        );
        relay.accept();
    }

    #[test]
    fn a_computer_removed_from_the_account_stops_until_signed_in_again() {
        let me = Identity::generate();
        let public = me.public_raw().to_vec();
        let relay = FakeRelay::start(&me.label());
        let (client, mut rx) = start(&relay, me, empty_access());
        let (mut ws, _) = relay.accept_and_claim(&public);
        assert_eq!(next_status(&mut rx), Status::Connecting);
        assert_eq!(next_status(&mut rx), Status::Connected);
        tell(&mut ws, json!({"type": "bye", "reason": "no_machine"}));
        hang_up(&mut ws, 4404, "no_machine");
        assert_eq!(
            next_status(&mut rx),
            Status::Rejected(
                "This computer was removed from your account. Turn the relay off and on again, or sign in again, to add it back.".into()
            )
        );
        relay.no_dial_within(Duration::from_millis(400));

        // Signing in again brings it back.
        client.set_session(Some("new".into()));
        let (_ws, door) = relay.accept_and_claim(&public);
        assert_eq!(door.authorization.as_deref(), Some("Bearer new"));
        assert_eq!(next_status(&mut rx), Status::Connecting);
        assert_eq!(next_status(&mut rx), Status::Connected);
    }

    #[test]
    fn a_session_never_goes_out_in_the_clear_over_a_network() {
        let me = Identity::generate();
        let (_client, mut rx) = RelayClient::start(
            "http://10.255.255.1:8787",
            me,
            empty_access(),
            machine(),
            account(Some("tok")),
        );
        assert_eq!(next_status(&mut rx), Status::Connecting);
        assert_eq!(
            next_status(&mut rx),
            Status::Rejected(
                "The relay address must start with https:// (http:// only for this computer)."
                    .into()
            )
        );
    }

    #[test]
    fn a_message_too_big_for_a_frame_goes_in_pieces() {
        let me = Identity::generate();
        let label = me.label();
        let public = me.public_raw().to_vec();
        let relay = FakeRelay::start(&label);
        let (client, mut rx) = start(&relay, me, empty_access());
        let (mut ws, _) = relay.accept_and_claim(&public);
        let mut phone = FakePhone::greet(&mut ws, &label, 5, "sess1");
        new_device_connects(&mut rx, 5);

        // 1 MB, with letters of more than one byte so a piece may end inside one.
        let text = "é".repeat(512 * 1024);
        let big = json!({"type": "entries", "text": text}).to_string();
        client.send(5, big.clone());
        let first = phone.hear_frame(&mut ws);
        assert_eq!(first["part"]["more"], true, "sent in pieces");
        let mut whole = B64.decode(first["part"]["body"].as_str().unwrap()).unwrap();
        loop {
            let msg = phone.hear_frame(&mut ws);
            whole.extend(B64.decode(msg["part"]["body"].as_str().unwrap()).unwrap());
            if msg["part"]["more"] != true {
                break;
            }
        }
        assert_eq!(String::from_utf8(whole).unwrap(), big);

        // To everyone too, and a small message after it still comes whole.
        client.broadcast(&big);
        assert_eq!(phone.hear(&mut ws)["text"], text);
        client.send(5, r#"{"type":"usage"}"#.into());
        assert_eq!(phone.hear_frame(&mut ws)["type"], "usage");
    }

    #[test]
    fn the_pieces_join_back_into_the_message() {
        let message = json!({ "type": "entries", "text": "aé€😀 ".repeat(8) });
        let mut joiner = Joiner::default();
        // Cut inside letters of more than one byte.
        let pieces = parts(&message.to_string(), 7);
        let (last, first) = pieces.split_last().unwrap();
        for piece in first {
            let piece: Value = serde_json::from_str(piece).unwrap();
            assert_eq!(joiner.take(piece), Ok(None));
        }
        let last: Value = serde_json::from_str(last).unwrap();
        assert_eq!(joiner.take(last), Ok(Some(message.clone())));
        // A whole message passes as it is, and the next pieces start clean.
        assert_eq!(
            joiner.take(json!({"type": "usage"})),
            Ok(Some(json!({"type": "usage"})))
        );
        for piece in parts(&message.to_string(), 4096) {
            let got = joiner.take(serde_json::from_str(&piece).unwrap());
            assert_eq!(got, Ok(Some(message.clone())));
        }
        assert!(
            joiner
                .take(json!({"part": {"body": "!", "more": false}}))
                .is_err()
        );
    }

    #[test]
    fn a_piece_of_either_kind_goes_out_whole() {
        // The largest `res` piece and the largest `part`: neither is split again.
        let body = B64.encode(vec![0xff; CHUNK]);
        let res = json!({ "res": { "id": u64::MAX, "body": body, "more": true, "status": 200, "type": "application/octet-stream; charset=utf-8" } });
        assert!(res.to_string().len() <= WHOLE);
        let text = "x".repeat(CHUNK * 2);
        assert!(parts(&text, CHUNK).iter().all(|p| p.len() <= WHOLE));
    }

    /// What `phone/public/relay.js` is checked against: `WRITE_VECTORS=1 cargo test
    /// phone::relay::tests::part_vectors` writes it; the plain run checks Rust
    /// still splits the same way, so a change on either side shows up.
    const PART_VECTORS: &str = "../phone/test/vectors/part.json";

    #[test]
    fn part_vectors() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(PART_VECTORS);
        // Letters of two, three and four bytes, cut where a piece ends inside one.
        let message = json!({ "type": "entries", "text": "aé€😀 ".repeat(8) }).to_string();
        let cases: Vec<Value> = [7, 64, 4096]
            .into_iter()
            .map(|chunk| json!({ "chunk": chunk, "parts": parts(&message, chunk) }))
            .collect();
        if std::env::var_os("WRITE_VECTORS").is_some() {
            let v = json!({
                "note": "Written by `WRITE_VECTORS=1 cargo test phone::relay::tests::part_vectors`; checked by phone/test/relay.test.mjs.",
                "message": message,
                "cases": cases,
            });
            std::fs::write(&path, serde_json::to_string_pretty(&v).unwrap() + "\n").unwrap();
            return;
        }
        let v: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(v["message"], message);
        assert_eq!(v["cases"], json!(cases));
    }

    #[test]
    fn a_device_that_breaks_the_handshake_is_cut_off() {
        let me = Identity::generate();
        let label = me.label();
        let public = me.public_raw().to_vec();
        let relay = FakeRelay::start(&label);
        let access = empty_access();
        let (client, mut rx) = start(&relay, me, access.clone());
        let (mut ws, _) = relay.accept_and_claim(&public);

        // Data before the hello.
        open(&mut ws, 3, "a", json!(ACCOUNT));
        ws.send(Message::Binary(frame(DATA, 3, b"early").into()))
            .unwrap();
        assert_eq!(closed(&mut ws, 3), "bad_frame");

        // A hello that is not one.
        open(&mut ws, 4, "b", json!(ACCOUNT));
        ws.send(Message::Binary(
            frame(HELLO, 4, br#"{"v":1,"e":"short"}"#).into(),
        ))
        .unwrap();
        assert_eq!(closed(&mut ws, 4), "bad_hello");
        assert!(access.lock().unwrap().devices.is_empty());

        // A client whose frame does not open (replayed): out, and the shell hears it leave.
        let phone = FakePhone::greet(&mut ws, &label, 5, "c");
        new_device_connects(&mut rx, 5);
        let mut session = phone.session;
        let replay = session.seal(br#"{"type":"ping"}"#);
        ws.send(Message::Binary(frame(DATA, 5, &replay).into()))
            .unwrap();
        ws.send(Message::Binary(frame(DATA, 5, &replay).into()))
            .unwrap();
        assert_eq!(closed(&mut ws, 5), "bad_frame");
        assert!(matches!(next_incoming(&mut rx), Incoming::Disconnected(5)));
        assert!(client.channels().is_empty());

        // Nothing of this reached the shell as a message.
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn a_label_that_is_not_ours_is_final() {
        let me = Identity::generate();
        let relay = FakeRelay::start("other");
        let (_client, mut rx) = start(&relay, me, empty_access());
        let (mut ws, _) = relay.accept();
        tell(
            &mut ws,
            json!({"type": "challenge", "nonce": B64.encode([1u8; 16])}),
        );
        let _claim = control(&mut ws);
        tell(&mut ws, json!({"type": "bye", "reason": "wrong_label"}));
        hang_up(&mut ws, 4403, "wrong_label");
        assert_eq!(next_status(&mut rx), Status::Connecting);
        assert_eq!(
            next_status(&mut rx),
            Status::Rejected("This address belongs to another key.".into())
        );
        relay.no_dial_within(Duration::from_millis(300));
    }

    #[test]
    fn a_block_is_asked_about_again_after_a_long_wait() {
        let me = Identity::generate();
        let public = me.public_raw().to_vec();
        let relay = FakeRelay::start(&me.label());
        let (_client, mut rx) = start(&relay, me, empty_access());
        let (mut ws, _) = relay.accept_and_claim(&public);
        assert_eq!(next_status(&mut rx), Status::Connecting);
        assert_eq!(next_status(&mut rx), Status::Connected);
        tell(&mut ws, json!({"type": "bye", "reason": "blocked"}));
        hang_up(&mut ws, 4403, "blocked");
        assert_eq!(
            next_status(&mut rx),
            Status::Refused {
                why: "The relay operator blocked this account.".into(),
                retry_in: REFUSED_WAIT,
            }
        );
        // The operator lifted the block: the next dial gets back in.
        let (_ws, _) = relay.accept_and_claim(&public);
        assert_eq!(next_status(&mut rx), Status::Connecting);
        assert_eq!(next_status(&mut rx), Status::Connected);
    }

    /// Answers the next request with a plain HTTP response, the head and the
    /// body apart, as a slow network may deliver them. Gives back the path asked.
    fn answer_http(relay: &FakeRelay, status: &str, body: &str) -> String {
        use std::io::{Read as _, Write as _};
        let (mut stream, _) = relay.listener.accept().unwrap();
        let mut request = Vec::new();
        let mut buf = [0u8; 4096];
        while !request.windows(4).any(|w| w == b"\r\n\r\n") {
            let n = stream.read(&mut buf).unwrap();
            assert!(n > 0, "the request ended early");
            request.extend_from_slice(&buf[..n]);
        }
        let head = format!(
            "HTTP/1.1 {status}\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
            body.len()
        );
        stream.write_all(head.as_bytes()).unwrap();
        stream.flush().unwrap();
        std::thread::sleep(Duration::from_millis(50));
        stream.write_all(body.as_bytes()).unwrap();
        let request = String::from_utf8_lossy(&request).to_string();
        request.split(' ').nth(1).unwrap_or_default().to_string()
    }

    #[test]
    fn a_403_from_a_firewall_is_dialed_again_soon() {
        let me = Identity::generate();
        let label = me.label();
        let public = me.public_raw().to_vec();
        let relay = FakeRelay::start(&label);
        let (_client, mut rx) = start(&relay, me, empty_access());
        assert!(
            answer_http(&relay, "403 Forbidden", "").starts_with(&format!("/machines/{label}/pc?"))
        );
        assert_eq!(next_status(&mut rx), Status::Connecting);
        assert_eq!(
            next_status(&mut rx),
            Status::Reconnecting {
                why: "The relay answered 403 Forbidden".into(),
                retry_in: RETRY_MIN,
            }
        );
        let (_ws, _) = relay.accept_and_claim(&public);
        assert_eq!(next_status(&mut rx), Status::Connecting);
        assert_eq!(next_status(&mut rx), Status::Connected);
    }

    #[test]
    fn a_relay_without_accounts_is_final() {
        // An older relay, or one of one's own without accounts: no such door,
        // or its page where the door would be.
        for (status, body) in [
            ("404 Not Found", "not found"),
            ("200 OK", "<!doctype html>"),
        ] {
            let me = Identity::generate();
            let relay = FakeRelay::start(&me.label());
            let (_client, mut rx) = start(&relay, me, empty_access());
            answer_http(&relay, status, body);
            assert_eq!(next_status(&mut rx), Status::Connecting);
            assert_eq!(
                next_status(&mut rx),
                Status::Rejected("This relay does not support account sign-in.".into())
            );
            relay.no_dial_within(Duration::from_millis(300));
        }
    }

    #[test]
    fn an_open_without_any_account_means_a_relay_without_accounts() {
        let me = Identity::generate();
        let public = me.public_raw().to_vec();
        let relay = FakeRelay::start(&me.label());
        let (_client, mut rx) = start(&relay, me, empty_access());
        let (mut ws, _) = relay.accept_and_claim(&public);
        assert_eq!(next_status(&mut rx), Status::Connecting);
        assert_eq!(next_status(&mut rx), Status::Connected);
        // An old relay names no account at all: every device would be turned
        // away, so staying connected helps nobody.
        tell(
            &mut ws,
            json!({"type": "open", "channel": 4, "device": "x", "name": "n", "kind": "ios"}),
        );
        assert_eq!(
            next_status(&mut rx),
            Status::Rejected("This relay does not support account sign-in.".into())
        );
        relay.no_dial_within(Duration::from_millis(300));
    }

    #[test]
    fn a_client_without_an_account_never_dials() {
        let me = Identity::generate();
        let relay = FakeRelay::start(&me.label());
        let nobody = Account {
            github_id: 0,
            session: Some("tok".into()),
        };
        let (client, mut rx) =
            RelayClient::start(&relay.base(), me, empty_access(), machine(), nobody);
        assert_eq!(next_status(&mut rx), Status::SignedOut);
        relay.no_dial_within(Duration::from_millis(300));
        client.set_session(Some("fresh".into()));
        relay.no_dial_within(Duration::from_millis(300));
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn only_a_new_session_cuts_a_wait_short() {
        let me = Identity::generate();
        let public = me.public_raw().to_vec();
        let relay = FakeRelay::start(&me.label());
        let (client, mut rx) = start(&relay, me, empty_access());
        relay.refuse(json!({"type": "bye", "reason": "label_taken"}), 4409);
        assert_eq!(next_status(&mut rx), Status::Connecting);
        assert!(matches!(next_status(&mut rx), Status::Rejected(_)));

        // The shell telling the same session again changes nothing.
        client.set_session(Some("tok".into()));
        relay.no_dial_within(Duration::from_millis(400));

        client.set_session(Some("other".into()));
        let (_ws, door) = relay.accept_and_claim(&public);
        assert_eq!(door.authorization.as_deref(), Some("Bearer other"));
    }

    #[test]
    fn a_relay_code_without_a_reason_reads_as_a_plain_close() {
        let me = Identity::generate();
        let public = me.public_raw().to_vec();
        let relay = FakeRelay::start(&me.label());
        let (_client, mut rx) = start(&relay, me, empty_access());
        let (mut ws, _) = relay.accept_and_claim(&public);
        assert_eq!(next_status(&mut rx), Status::Connecting);
        assert_eq!(next_status(&mut rx), Status::Connected);
        hang_up(&mut ws, 4403, "");
        assert_eq!(
            next_status(&mut rx),
            Status::Reconnecting {
                why: "The relay closed the connection".into(),
                retry_in: RETRY_MIN,
            }
        );
    }

    #[test]
    fn a_device_that_changed_its_name_is_told_to_the_shell() {
        let me = Identity::generate();
        let label = me.label();
        let public = me.public_raw().to_vec();
        let relay = FakeRelay::start(&label);
        let access = empty_access();
        access.lock().unwrap().relay_device("sess1", "Old name", 1);
        let (_client, mut rx) = start(&relay, me, access.clone());
        let (mut ws, _) = relay.accept_and_claim(&public);
        let _phone = FakePhone::greet(&mut ws, &label, 5, "sess1");
        assert!(matches!(next_incoming(&mut rx), Incoming::Renamed));
        assert!(matches!(next_incoming(&mut rx), Incoming::Connected(5, _)));
        assert_eq!(access.lock().unwrap().devices[0].name, "Rik's iPhone");
    }

    #[test]
    fn waits_read_in_seconds_then_minutes() {
        assert_eq!(wait_text(Duration::from_millis(200)), "1 s");
        assert_eq!(wait_text(Duration::from_secs(60)), "60 s");
        assert_eq!(wait_text(Duration::from_secs(600)), "10 min");
    }

    #[test]
    fn a_claim_signs_only_a_nonce_of_the_relays_length() {
        let label = "abcdefghijklmnop";
        for len in [0, 15, 17, 32, 181] {
            assert_eq!(claim_message(&vec![1u8; len], label), None, "{len} bytes");
        }
        let signed = claim_message(&[1u8; CLAIM_NONCE_LEN], label).unwrap();
        assert!(signed.starts_with(b"arakun-relay-claim-v1\0"));
        assert_eq!(
            signed.len(),
            CLAIM_TAG.len() + CLAIM_NONCE_LEN + label.len()
        );
        assert!(signed.ends_with(label.as_bytes()));
    }

    #[test]
    fn a_challenge_with_a_long_nonce_is_not_signed() {
        let me = Identity::generate();
        let relay = FakeRelay::start(&me.label());
        let (_client, mut rx) = start(&relay, me, empty_access());
        let (mut ws, _) = relay.accept();
        assert_eq!(next_status(&mut rx), Status::Connecting);
        tell(
            &mut ws,
            json!({"type": "challenge", "nonce": B64.encode([1u8; 181])}),
        );
        // The computer hangs up instead of answering with a claim.
        loop {
            match ws.read() {
                Ok(Message::Binary(b)) => panic!("the computer answered: {b:?}"),
                Ok(Message::Close(_)) | Err(_) => break,
                Ok(_) => {}
            }
        }
    }
}
