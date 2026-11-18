//! The computer's side of the relay. One WebSocket to `wss://<label>.<relay>/pc`
//! carries every phone: the relay challenges the computer to sign a nonce with
//! its identity, then opens a numbered channel for each phone that brings a
//! ticket or the pairing code, and passes their frames along unread. The
//! connection is kept up from a thread of its own, which dials again when it
//! drops, waiting longer each time; the shell hears how it goes as `Incoming`.
//!
//! Frames to and from the relay are binary: a type byte, the channel as four
//! big-endian bytes, then the payload. Channel 0 is the relay itself, speaking
//! JSON control messages; the other channels carry a phone's `hello` and `data`
//! frames, which only the phone and this computer can read (`e2e`).
//!
//! Inside a channel, once the keys are agreed, the phone speaks JSON: first
//! `{pair: {secret, name}}` or `{auth: {token}}` to say who it is, then the
//! same messages it would send on a socket of its own (`ClientMsg`), plus
//! `{req: {id, method, path, body}}` for what it would otherwise ask over HTTP
//! (`route`), answered with `{res: {id, status, type, body, more}}` in pieces
//! small enough for the relay. A channel that said who it is counts as a
//! client to the shell, numbered as the relay numbered the channel.

use std::collections::HashMap;
use std::net::{TcpStream, ToSocketAddrs as _};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, mpsc as std_mpsc};
use std::time::{Duration, Instant};

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD as B64;
use futures::channel::mpsc;
use serde_json::{Value, json};
use tungstenite::{Message, WebSocket, stream::MaybeTlsStream};

use super::e2e::{Handshake, PhoneHello, Session};
use super::identity::Identity;
use super::{ClientId, ClientMsg, Effect, Head, Incoming, Shared, wire};

pub const HELLO: u8 = 0x01;
pub const DATA: u8 = 0x02;
pub const CONTROL: u8 = 0x03;

/// How much of a response body goes in one `res` piece: with base64 and the
/// envelope it stays under the relay's frame limit (256 KiB).
const RES_CHUNK: usize = 160 * 1024;
/// The most a `req` body may carry, like `MAX_BODY` over TCP.
const MAX_REQ_BODY: usize = 4096;

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
/// How long a read waits before the loop looks at what there is to send.
const READ_TICK: Duration = Duration::from_millis(20);
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
    /// The relay turned this computer away: the operator blocked it, or
    /// another computer with this very key took the place. That can change,
    /// so it asks again, but only after a long wait.
    Refused {
        why: String,
        retry_in: Duration,
    },
    /// The relay will not have this computer, and asking again would not
    /// change that: the label belongs to another key, or the relay does not
    /// accept its signature.
    Rejected(String),
}

/// What the shell asks the connection thread to do.
enum Command {
    /// The whole list of ticket hashes the relay should admit.
    Tickets(Vec<String>),
    /// Let one phone in with the code whose hash this is, for so long.
    PairOpen {
        hash: String,
        ttl: Duration,
    },
    PairClose,
    /// A message to the phone on a channel, sealed there.
    Send {
        channel: u32,
        text: String,
    },
    /// A message to every phone that said who it is.
    Broadcast(String),
    /// Close a phone's channel; the relay tells the phone why.
    CloseChannel {
        channel: u32,
        reason: String,
    },
}

/// A channel the relay opened, as the shell sees it: the phone's device once
/// it said who it is (`ready`), or what the relay knew of it before.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Channel {
    pub device: Option<String>,
    pub ready: bool,
}

type Channels = Arc<Mutex<HashMap<u32, Channel>>>;

/// The connection to the relay. Dropping it hangs up and ends the thread.
/// Phones that said who they are look like the server's clients, numbered by
/// channel.
pub struct RelayClient {
    page: String,
    channels: Channels,
    cmd: std_mpsc::Sender<Command>,
    stop: Arc<AtomicBool>,
}

impl RelayClient {
    /// Dials `base` as this identity and keeps the connection up. Status changes
    /// (`Incoming::Relay`, starting from `Connecting`) and what phones say come
    /// out of the receiver; who may come in is read from `access`.
    pub fn start(
        base: &str,
        identity: Identity,
        access: Shared,
    ) -> (RelayClient, mpsc::UnboundedReceiver<Incoming>) {
        let label = identity.label();
        let (tx, rx) = mpsc::unbounded();
        let (cmd, commands) = std_mpsc::channel();
        let channels: Channels = Arc::default();
        let stop = Arc::new(AtomicBool::new(false));
        let tickets = access.lock().unwrap().ticket_hashes();
        let link = Link {
            url: endpoint(base, &label),
            page: page_url(base, &label),
            identity,
            access,
            tickets,
            pairing: None,
            commands,
            tx,
            status: None,
            channels: channels.clone(),
            peers: HashMap::new(),
            stop: stop.clone(),
        };
        std::thread::Builder::new()
            .name("arakun-relay".into())
            .spawn(move || link.run())
            .expect("a thread can start");
        (
            RelayClient {
                page: page_url(base, &label),
                channels,
                cmd,
                stop,
            },
            rx,
        )
    }

    /// Where phones find this computer through the relay.
    pub fn page_url(&self) -> &str {
        &self.page
    }

    /// The channels open right now, by number.
    pub fn channels(&self) -> HashMap<u32, Channel> {
        self.channels.lock().unwrap().clone()
    }

    /// The relay admits phones with these ticket hashes from now on, and only these.
    pub fn set_tickets(&self, hashes: Vec<String>) {
        let _ = self.cmd.send(Command::Tickets(hashes));
    }

    /// One phone may come in with the code whose SHA-256 is `hash` (hex), for `ttl`.
    pub fn open_pairing(&self, hash: String, ttl: Duration) {
        let _ = self.cmd.send(Command::PairOpen { hash, ttl });
    }

    pub fn close_pairing(&self) {
        let _ = self.cmd.send(Command::PairClose);
    }

    pub fn close_channel(&self, channel: u32, reason: &str) {
        let _ = self.cmd.send(Command::CloseChannel {
            channel,
            reason: reason.to_string(),
        });
    }

    /// The device behind a client, once it said who it is.
    pub fn device_of(&self, id: ClientId) -> Option<String> {
        let channels = self.channels.lock().unwrap();
        let c = channels.get(&u32::try_from(id).ok()?)?;
        c.ready.then(|| c.device.clone()).flatten()
    }

    pub fn send(&self, id: ClientId, text: String) {
        if let Ok(channel) = u32::try_from(id) {
            let _ = self.cmd.send(Command::Send { channel, text });
        }
    }

    pub fn broadcast(&self, text: &str) {
        let _ = self.cmd.send(Command::Broadcast(text.to_string()));
    }

    pub fn client_count(&self) -> usize {
        self.channels
            .lock()
            .unwrap()
            .values()
            .filter(|c| c.ready)
            .count()
    }

    /// The devices with a channel that said who it is right now.
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

    /// Closes every channel of a device, as unpairing it must.
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

/// The WebSocket address of this computer's place on the relay. A base at a
/// bare address or `localhost` is a relay run by hand (`wrangler dev`), which
/// takes the label from its configuration: no label in front, no TLS.
pub fn endpoint(base: &str, label: &str) -> String {
    let (secure, host) = origin(base, label);
    format!("{}://{host}/pc", if secure { "wss" } else { "ws" })
}

/// The page phones open to reach this computer through the relay.
pub fn page_url(base: &str, label: &str) -> String {
    let (secure, host) = origin(base, label);
    format!("{}://{host}/", if secure { "https" } else { "http" })
}

/// Whether the relay is reached over TLS, and the host (with port) phones and
/// this computer use for it.
fn origin(base: &str, label: &str) -> (bool, String) {
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
    if local {
        (scheme == "https", host.to_string())
    } else {
        (true, format!("{label}.{host}"))
    }
}

struct Link {
    url: String,
    /// The page phones open, whose `_relay` says whether the label is blocked.
    page: String,
    identity: Identity,
    access: Shared,
    tickets: Vec<String>,
    /// The pairing the relay should know about, until when.
    pairing: Option<(String, Instant)>,
    commands: std_mpsc::Receiver<Command>,
    tx: mpsc::UnboundedSender<Incoming>,
    /// The last status told, so the same one is not told twice.
    status: Option<Status>,
    channels: Channels,
    /// The phones on the open channels, as far as each has got.
    peers: HashMap<u32, Peer>,
    stop: Arc<AtomicBool>,
}

/// A phone on a channel, step by step.
enum Peer {
    /// The relay opened the channel; no hello yet.
    Opened,
    /// Keys agreed; the phone has yet to say who it is.
    Greeted(Session),
    /// A paired device: the shell knows it as a client.
    Ready {
        session: Session,
        token: String,
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
            self.set_status(Status::Connecting);
            let ended = match self.connect() {
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
                Ended::Rejected(why) => {
                    self.set_status(Status::Rejected(why));
                    return;
                }
                Ended::Refused(why) => {
                    self.set_status(Status::Refused {
                        why,
                        retry_in: REFUSED_WAIT,
                    });
                    if !self.pause(REFUSED_WAIT) {
                        return;
                    }
                }
                Ended::Retry(why) => {
                    self.set_status(Status::Reconnecting {
                        why,
                        retry_in: wait,
                    });
                    if !self.pause(wait) {
                        return;
                    }
                    wait = (wait * 2).min(RETRY_MAX);
                }
            }
        }
    }

    /// Waits between dials, keeping what the shell asks for meanwhile. False
    /// when the client was dropped during the wait.
    fn pause(&mut self, wait: Duration) -> bool {
        let until = Instant::now() + wait;
        while Instant::now() < until {
            if self.stop.load(Ordering::Relaxed) {
                return false;
            }
            self.drain_commands_offline();
            std::thread::sleep(WAIT_TICK);
        }
        true
    }

    fn set_status(&mut self, status: Status) {
        if self.status.as_ref() != Some(&status) {
            self.status = Some(status.clone());
            let _ = self.tx.unbounded_send(Incoming::Relay(status));
        }
    }

    /// Commands that arrive while there is no connection: what the relay must
    /// be told on the next claim is kept; frames have nowhere to go.
    fn drain_commands_offline(&mut self) {
        while let Ok(cmd) = self.commands.try_recv() {
            self.remember(&cmd);
        }
    }

    fn remember(&mut self, cmd: &Command) {
        match cmd {
            Command::Tickets(hashes) => self.tickets = hashes.clone(),
            Command::PairOpen { hash, ttl } => {
                self.pairing = Some((hash.clone(), Instant::now() + *ttl))
            }
            Command::PairClose => self.pairing = None,
            Command::Send { .. } | Command::Broadcast(_) | Command::CloseChannel { .. } => {}
        }
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

    /// TCP, TLS when the address asks for it, and the WebSocket handshake.
    fn connect(&self) -> Result<Socket, Ended> {
        let (host, port, _) = parse_url(&self.url)
            .ok_or_else(|| Ended::Rejected(format!("Bad relay address: {}", self.url)))?;
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
        let _ = stream.set_read_timeout(Some(CONNECT_TIMEOUT));
        let _ = stream.set_write_timeout(Some(CONNECT_TIMEOUT));
        // A `ws://` address comes back as a plain stream in the same wrapper.
        let (ws, _) = tungstenite::client_tls(self.url.as_str(), stream).map_err(|e| match e {
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
                // A 403 may come from a firewall in front of the relay, which
                // can let the next attempt through. Only the relay's own word
                // makes it a block: the body here is whatever arrived with the
                // head, raw, so `_relay` is asked instead.
                if res.status().as_u16() == 403 && self.blocked_at_door() {
                    refused("blocked")
                } else {
                    Ended::Retry(text)
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
        if let MaybeTlsStream::Plain(s) = ws.get_ref() {
            let _ = s.set_read_timeout(Some(READ_TICK));
        }
        if let MaybeTlsStream::Rustls(s) = ws.get_ref() {
            let _ = s.get_ref().set_read_timeout(Some(READ_TICK));
        }
        Ok(ws)
    }

    /// Whether the relay itself says this label is blocked. Anything else,
    /// including no answer, is not a block.
    fn blocked_at_door(&self) -> bool {
        let agent: ureq::Agent = ureq::Agent::config_builder()
            .timeout_global(Some(CONNECT_TIMEOUT))
            .http_status_as_error(false)
            .build()
            .into();
        let Ok(mut res) = agent.get(format!("{}_relay", self.page)).call() else {
            return false;
        };
        let Ok(body) = res.body_mut().with_config().limit(4096).read_to_string() else {
            return false;
        };
        res.status().as_u16() == 403
            && serde_json::from_str::<Value>(&body).is_ok_and(|v| v["blocked"] == true)
    }

    /// Answers the challenge with the signature of `nonce ‖ label`, then tells
    /// the relay which tickets to admit and whether a pairing is open.
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
                Some(msg) => {
                    return Err(Ended::Retry(format!(
                        "The relay said {} before challenging us",
                        msg["type"]
                    )));
                }
                None => {}
            }
        };
        let label = self.identity.label();
        let mut signed = nonce;
        signed.extend_from_slice(label.as_bytes());
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
                Some(msg) if msg["type"] == "claimed" => break,
                Some(msg) if msg["type"] == "bye" => {
                    return Err(refused(msg["reason"].as_str().unwrap_or("refused")));
                }
                Some(msg) => {
                    return Err(Ended::Retry(format!(
                        "The relay said {} to our claim",
                        msg["type"]
                    )));
                }
                None => {}
            }
        }
        self.drain_commands_offline();
        send_control(ws, 0, &json!({ "type": "tickets", "hashes": self.tickets }))?;
        if let Some((hash, until)) = &self.pairing {
            let left = until.saturating_duration_since(Instant::now());
            if !left.is_zero() {
                send_control(
                    ws,
                    0,
                    &json!({ "type": "pair_open", "hash": hash, "ttl": left.as_secs().max(1) }),
                )?;
            }
        }
        Ok(())
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
    fn talk(&mut self, ws: &mut Socket) -> Ended {
        let mut last_heard = Instant::now();
        let mut last_ping = Instant::now();
        loop {
            if self.stop.load(Ordering::Relaxed) {
                return Ended::Stopped;
            }
            match read(ws) {
                Ok(Read::Frame(kind, channel, payload)) => {
                    last_heard = Instant::now();
                    if let Err(ended) = self.heard(ws, kind, channel, payload) {
                        return ended;
                    }
                }
                Ok(Read::Alive) => last_heard = Instant::now(),
                Ok(Read::Nothing) => {}
                Err(ended) => return ended,
            }
            if last_heard.elapsed() > SILENCE_MAX {
                return Ended::Retry("The relay went quiet".into());
            }
            if last_ping.elapsed() > PING_EVERY {
                last_ping = Instant::now();
                if ws.send(Message::Ping(Vec::new().into())).is_err() {
                    return Ended::Retry("Could not write to the relay".into());
                }
            }
            loop {
                match self.commands.try_recv() {
                    Ok(cmd) => {
                        self.remember(&cmd);
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
        }
    }

    fn obey(&mut self, ws: &mut Socket, cmd: Command) -> Result<(), Ended> {
        match cmd {
            Command::Tickets(hashes) => {
                send_control(ws, 0, &json!({ "type": "tickets", "hashes": hashes }))
            }
            Command::PairOpen { hash, ttl } => send_control(
                ws,
                0,
                &json!({ "type": "pair_open", "hash": hash, "ttl": ttl.as_secs().max(1) }),
            ),
            Command::PairClose => send_control(ws, 0, &json!({ "type": "pair_close" })),
            Command::Send { channel, text } => match self.peers.get_mut(&channel) {
                Some(Peer::Ready { session, .. }) => {
                    send_frame(ws, DATA, channel, &session.seal(text.as_bytes()))
                }
                _ => Ok(()),
            },
            Command::Broadcast(text) => {
                for (channel, peer) in &mut self.peers {
                    if let Peer::Ready { session, .. } = peer {
                        send_frame(ws, DATA, *channel, &session.seal(text.as_bytes()))?;
                    }
                }
                Ok(())
            }
            Command::CloseChannel { channel, reason } => self.drop_channel(ws, channel, &reason),
        }
    }

    /// A frame from the relay. Control on channel 0 is about channels and about
    /// us; anything on another channel is a phone's.
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
                if let Some(channel) = channel() {
                    let device = msg["device"].as_str().map(str::to_string);
                    self.channels.lock().unwrap().insert(
                        channel,
                        Channel {
                            device,
                            ready: false,
                        },
                    );
                    self.peers.insert(channel, Peer::Opened);
                }
            }
            Some("close") => {
                if let Some(channel) = channel() {
                    self.forget_channel(channel);
                }
            }
            Some("bye") => return Err(refused(msg["reason"].as_str().unwrap_or("refused"))),
            _ => {}
        }
        Ok(())
    }

    /// A phone's frame on its channel: the hello first, then sealed messages.
    /// A phone that does not keep to this is cut off; a bad frame on an
    /// agreed channel means a replay or tampering and ends it too.
    fn phone_said(
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
        let host = super::host_name();
        // The peer as it stands after the frame, or why the channel ends.
        let outcome: Result<Peer, &str> = match (kind, peer) {
            (HELLO, Peer::Opened) => {
                let answered = PhoneHello::parse(&payload).and_then(|hello| {
                    Handshake::begin().answer(&hello, &self.identity, channel, &host)
                });
                match answered {
                    Ok((reply, session)) => {
                        send_frame(ws, HELLO, channel, &reply)?;
                        Ok(Peer::Greeted(session))
                    }
                    Err(_) => Err("bad_hello"),
                }
            }
            (DATA, Peer::Greeted(mut session)) => {
                let msg = session
                    .open(&payload)
                    .ok()
                    .and_then(|p| serde_json::from_slice::<Value>(&p).ok());
                match msg {
                    None => Err("bad_frame"),
                    Some(msg) if msg.get("pair").is_some() => {
                        let ask = &msg["pair"];
                        let secret = ask["secret"].as_str().unwrap_or_default();
                        let name = ask["name"].as_str().unwrap_or_default();
                        let paired = self.access.lock().unwrap().pair_relay(secret, name, now);
                        match paired {
                            Some((device, token, ticket)) => {
                                let told = json!({ "paired": { "id": device.id, "name": device.name, "token": token, "ticket": ticket, "host": host } });
                                send_frame(
                                    ws,
                                    DATA,
                                    channel,
                                    &session.seal(told.to_string().as_bytes()),
                                )?;
                                // The relay admits the phone by ticket from its next socket on.
                                self.tickets = self.access.lock().unwrap().ticket_hashes();
                                send_control(
                                    ws,
                                    0,
                                    &json!({ "type": "tickets", "hashes": self.tickets }),
                                )?;
                                let _ = self.tx.unbounded_send(Incoming::Paired);
                                self.ready(channel, &device.id);
                                Ok(Peer::Ready {
                                    session,
                                    token,
                                    device: device.id,
                                })
                            }
                            None => Err("wrong_secret"),
                        }
                    }
                    Some(msg) if msg.get("auth").is_some() => {
                        let token = msg["auth"]["token"]
                            .as_str()
                            .unwrap_or_default()
                            .to_string();
                        let device = self.access.lock().unwrap().device_for(&token, now);
                        match device {
                            Some(device) => {
                                let told = json!({ "ok": { "id": device.id, "name": device.name, "host": host } });
                                send_frame(
                                    ws,
                                    DATA,
                                    channel,
                                    &session.seal(told.to_string().as_bytes()),
                                )?;
                                self.ready(channel, &device.id);
                                Ok(Peer::Ready {
                                    session,
                                    token,
                                    device: device.id,
                                })
                            }
                            None => Err("unauthorized"),
                        }
                    }
                    Some(_) => Err("unauthorized"),
                }
            }
            (
                DATA,
                Peer::Ready {
                    mut session,
                    token,
                    device,
                },
            ) => match session.open(&payload) {
                Err(_) => Err("bad_frame"),
                Ok(plain) => {
                    let msg = serde_json::from_slice::<Value>(&plain);
                    let mut outcome = Ok(());
                    match msg {
                        Err(_) => send_frame(
                            ws,
                            DATA,
                            channel,
                            &session.seal(wire::error("bad message: not json").as_bytes()),
                        )?,
                        Ok(msg) if msg.get("req").is_some() => {
                            let req = &msg["req"];
                            let (reply, effect) = self.route(req, &token, now);
                            respond(ws, channel, &mut session, &req["id"], reply)?;
                            outcome = self.apply(ws, channel, effect)?;
                        }
                        Ok(msg) => match serde_json::from_value::<ClientMsg>(msg) {
                            Ok(ClientMsg::Ping) => {}
                            Ok(msg) => {
                                let _ = self.tx.unbounded_send(Incoming::Message(
                                    ClientId::from(channel),
                                    msg,
                                ));
                            }
                            Err(e) => send_frame(
                                ws,
                                DATA,
                                channel,
                                &session.seal(wire::error(&format!("bad message: {e}")).as_bytes()),
                            )?,
                        },
                    }
                    outcome.map(|()| Peer::Ready {
                        session,
                        token,
                        device,
                    })
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

    /// What a `req` on `asking` changed, acted on. An unpairing closes the
    /// device's other channels here and ends the asking one with the reason
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

    /// A `req` from a phone that said who it is, answered as the HTTP it stands for.
    fn route(&self, req: &Value, token: &str, now: u64) -> (super::Reply, Effect) {
        let method = req["method"].as_str().unwrap_or("GET");
        let path = req["path"].as_str().unwrap_or("/");
        let body = req["body"]
            .as_str()
            .map(|b| b.as_bytes().to_vec())
            .filter(|b| b.len() <= MAX_REQ_BODY);
        match Head::relayed(method, path, token) {
            Some(head) => super::route(&head, body, &self.access, now),
            None => (super::Reply::error(400, "bad request"), Effect::None),
        }
    }

    /// The channel's phone said who it is: the shell gets a client.
    fn ready(&mut self, channel: u32, device: &str) {
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

    /// Ends a channel from this side: the relay passes the reason to the phone.
    fn drop_channel(&mut self, ws: &mut Socket, channel: u32, reason: &str) -> Result<(), Ended> {
        if self.forget_channel(channel) {
            send_control(ws, channel, &json!({ "type": "close", "reason": reason }))?;
        }
        Ok(())
    }

    /// A channel is gone; whether it was one. The shell hears a client leave
    /// when the phone had said who it is.
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
        reply.body.chunks(RES_CHUNK).collect()
    };
    let last = pieces.len() - 1;
    for (i, piece) in pieces.into_iter().enumerate() {
        let mut res = json!({ "id": id, "body": B64.encode(piece), "more": i < last });
        if i == 0 {
            res["status"] = json!(reply.status);
            res["type"] = json!(reply.content_type);
        }
        send_frame(
            ws,
            DATA,
            channel,
            &session.seal(json!({ "res": res }).to_string().as_bytes()),
        )?;
    }
    Ok(())
}

/// What a `bye` from the relay means for the next attempt.
fn refused(reason: &str) -> Ended {
    match reason {
        "wrong_label" | "key_mismatch" => {
            Ended::Rejected("This address belongs to another key.".into())
        }
        "bad_signature" => {
            Ended::Rejected("The relay did not accept this computer's signature.".into())
        }
        // The operator may lift a block, and the other computer may go away:
        // keep asking, slowly, so a server comes back without a restart.
        "blocked" => Ended::Refused("The relay operator blocked this address.".into()),
        "replaced" => {
            Ended::Refused("Another computer with this key connected to the relay.".into())
        }
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
        // The pong to our ping is all the relay says while no phone is around:
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
                Some(4401 | 4403 | 4409) => refused(&reason),
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

#[cfg(test)]
mod tests {
    use super::*;
    use ring::signature;
    use std::net::TcpListener;
    use tungstenite::protocol::frame::coding::CloseCode;

    #[test]
    fn the_endpoint_puts_the_label_in_front_unless_the_relay_is_local() {
        assert_eq!(
            endpoint("https://arakun.dev", "abc"),
            "wss://abc.arakun.dev/pc"
        );
        assert_eq!(
            endpoint("https://example.workers.dev/", "abc"),
            "wss://abc.example.workers.dev/pc"
        );
        assert_eq!(
            endpoint("relay.example", "abc"),
            "wss://abc.relay.example/pc"
        );
        assert_eq!(
            endpoint("http://127.0.0.1:8787", "abc"),
            "ws://127.0.0.1:8787/pc"
        );
        assert_eq!(
            endpoint("http://localhost:8787/", "abc"),
            "ws://localhost:8787/pc"
        );
        assert_eq!(
            endpoint("https://127.0.0.1:8443", "abc"),
            "wss://127.0.0.1:8443/pc"
        );
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
            parse_url("ws://127.0.0.1:8787/pc"),
            Some(("127.0.0.1".into(), 8787, false))
        );
        assert_eq!(parse_url("https://x"), None);
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

        /// Takes the next computer, challenges it and checks the claim; gives the
        /// socket back claimed, with the `tickets` message it then sent.
        fn accept_and_claim(&self, public: &[u8]) -> (WebSocket<TcpStream>, Value) {
            let (stream, _) = self.listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let mut ws = tungstenite::accept(stream).unwrap();
            let nonce = [7u8; 16];
            ws.send(Message::Binary(
                frame(
                    CONTROL,
                    0,
                    json!({"type": "challenge", "nonce": B64.encode(nonce)})
                        .to_string()
                        .as_bytes(),
                )
                .into(),
            ))
            .unwrap();
            let claim = control(&mut ws);
            assert_eq!(claim["type"], "claim");
            assert_eq!(B64.decode(claim["pub"].as_str().unwrap()).unwrap(), public);
            let sig = B64.decode(claim["sig"].as_str().unwrap()).unwrap();
            let mut signed = nonce.to_vec();
            signed.extend_from_slice(self.label.as_bytes());
            signature::UnparsedPublicKey::new(&signature::ECDSA_P256_SHA256_FIXED, public)
                .verify(&signed, &sig)
                .unwrap();
            ws.send(Message::Binary(
                frame(CONTROL, 0, br#"{"type":"claimed"}"#).into(),
            ))
            .unwrap();
            let tickets = control(&mut ws);
            assert_eq!(tickets["type"], "tickets");
            (ws, tickets)
        }
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

    /// Access with devices paired through the relay, one per ticket hash.
    fn access_with_tickets(hashes: &[&str]) -> Shared {
        let devices = hashes
            .iter()
            .enumerate()
            .map(|(i, h)| super::super::Device {
                id: format!("dev{i}"),
                name: "Phone".into(),
                token_hash: format!("tok{i}"),
                ticket_hash: h.to_string(),
                paired_at: 0,
                last_seen: 0,
                push: None,
                notices: Default::default(),
            })
            .collect();
        super::super::Access::new(devices, "pk".into())
    }

    #[test]
    fn the_computer_claims_its_label_sends_its_tickets_and_hears_channels() {
        let me = Identity::generate();
        let public = me.public_raw().to_vec();
        let relay = FakeRelay::start(&me.label());
        let (client, mut rx) =
            RelayClient::start(&relay.base(), me, access_with_tickets(&["aa", "bb"]));
        assert_eq!(next_status(&mut rx), Status::Connecting);
        assert_eq!(client.page_url(), format!("{}/", relay.base()));

        let (mut ws, tickets) = relay.accept_and_claim(&public);
        assert_eq!(tickets["hashes"], json!(["aa", "bb"]));
        assert_eq!(next_status(&mut rx), Status::Connected);

        tell(
            &mut ws,
            json!({"type": "open", "channel": 3, "device": "dev1"}),
        );
        tell(
            &mut ws,
            json!({"type": "open", "channel": 4, "device": null}),
        );
        until(|| (client.channels().len() == 2).then_some(()));
        assert_eq!(
            client.channels()[&3],
            Channel {
                device: Some("dev1".into()),
                ready: false
            }
        );
        assert_eq!(
            client.channels()[&4],
            Channel {
                device: None,
                ready: false
            }
        );
        // Not clients yet: nobody has said who they are.
        assert_eq!(client.client_count(), 0);
        assert_eq!(client.device_of(3), None);
        tell(
            &mut ws,
            json!({"type": "close", "channel": 3, "reason": "closed"}),
        );
        until(|| (client.channels().len() == 1).then_some(()));

        client.set_tickets(vec!["cc".into()]);
        assert_eq!(control(&mut ws)["hashes"], json!(["cc"]));
        client.open_pairing("ab".repeat(32), Duration::from_secs(120));
        let open = control(&mut ws);
        assert_eq!(open["type"], "pair_open");
        assert_eq!(open["ttl"], 120);
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
    fn a_dropped_connection_is_dialed_again_with_the_latest_tickets() {
        let me = Identity::generate();
        let public = me.public_raw().to_vec();
        let relay = FakeRelay::start(&me.label());
        let (client, mut rx) = RelayClient::start(&relay.base(), me, access_with_tickets(&[]));
        let (mut ws, _) = relay.accept_and_claim(&public);
        assert_eq!(next_status(&mut rx), Status::Connecting);
        assert_eq!(next_status(&mut rx), Status::Connected);
        tell(
            &mut ws,
            json!({"type": "open", "channel": 1, "device": "d"}),
        );
        until(|| (client.channels().len() == 1).then_some(()));

        ws.close(None).unwrap();
        let _ = ws.flush();
        drop(ws);
        let status = next_status(&mut rx);
        assert!(matches!(status, Status::Reconnecting { .. }), "{status:?}");
        assert!(client.channels().is_empty());
        client.set_tickets(vec!["new".into()]);

        let (_ws, tickets) = relay.accept_and_claim(&public);
        assert_eq!(tickets["hashes"], json!(["new"]));
        assert_eq!(next_status(&mut rx), Status::Connecting);
        assert_eq!(next_status(&mut rx), Status::Connected);
    }

    /// A phone in Rust, on one channel of the fake relay: it speaks `e2e` like
    /// the page does.
    struct FakePhone {
        channel: u32,
        session: Session,
    }

    impl FakePhone {
        /// Opens `channel` on the computer (as the relay would), shakes hands and
        /// checks the computer's hello is from `label`.
        fn greet(
            ws: &mut WebSocket<TcpStream>,
            label: &str,
            channel: u32,
            device: Option<&str>,
        ) -> FakePhone {
            tell(
                ws,
                json!({"type": "open", "channel": channel, "device": device}),
            );
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

        /// The next sealed message from the computer on this channel.
        fn hear(&mut self, ws: &mut WebSocket<TcpStream>) -> Value {
            let (kind, ch, payload) = next_frame(ws);
            assert_eq!(
                (kind, ch),
                (DATA, self.channel),
                "{}",
                String::from_utf8_lossy(&payload)
            );
            serde_json::from_slice(&self.session.open(&payload).unwrap()).unwrap()
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
                let msg = self.hear(ws);
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

    #[test]
    fn a_phone_pairs_through_the_relay_and_then_talks_sealed() {
        let me = Identity::generate();
        let label = me.label();
        let public = me.public_raw().to_vec();
        let relay = FakeRelay::start(&label);
        let access = access_with_tickets(&[]);
        let (client, mut rx) = RelayClient::start(&relay.base(), me, access.clone());
        let (mut ws, _) = relay.accept_and_claim(&public);
        let (code, secret) = access.lock().unwrap().begin_pairing_relay();
        assert_ne!(code, secret);

        // The phone the relay let in with the code shows the secret.
        let mut phone = FakePhone::greet(&mut ws, &label, 5, None);
        phone.say(
            &mut ws,
            json!({"pair": {"secret": secret, "name": "Alice's iPhone"}}),
        );
        let paired = phone.hear(&mut ws);
        let device = paired["paired"]["id"].as_str().unwrap().to_string();
        let token = paired["paired"]["token"].as_str().unwrap().to_string();
        let ticket = paired["paired"]["ticket"].as_str().unwrap().to_string();
        assert_eq!(paired["paired"]["name"], "Alice's iPhone");
        assert_eq!(paired["paired"]["host"], super::super::host_name());
        // The relay is told the new ticket at once, and the shell hears both things.
        let tickets = control(&mut ws);
        assert_eq!(tickets["hashes"], json!([super::super::hash(&ticket)]));
        assert!(matches!(next_incoming(&mut rx), Incoming::Paired));
        assert!(matches!(next_incoming(&mut rx), Incoming::Connected(5, d) if d == device));
        assert_eq!(client.device_of(5), Some(device.clone()));
        assert_eq!(client.connected_devices(), vec![device.clone()]);
        assert_eq!(client.client_count(), 1);
        assert!(
            access.lock().unwrap().pairing().is_none(),
            "the pairing was spent"
        );

        // What it would ask over HTTP goes inside: who am I.
        let (status, body) = phone.req(&mut ws, "GET", "/me", None);
        assert_eq!(status, 200);
        let me: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(me["id"], device);
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

        // A second channel of the same phone comes in with its token.
        let mut again = FakePhone::greet(&mut ws, &label, 6, Some(&device));
        again.say(&mut ws, json!({"auth": {"token": token}}));
        assert_eq!(again.hear(&mut ws)["ok"]["id"], device);
        assert!(matches!(next_incoming(&mut rx), Incoming::Connected(6, _)));
        assert_eq!(client.client_count(), 2);

        // Unpairing from one channel closes both, and the shell hears each leave.
        let (status, _) = again.req(&mut ws, "POST", "/unpair", None);
        assert_eq!(status, 200);
        let mut reasons = vec![closed(&mut ws, 5), closed(&mut ws, 6)];
        reasons.sort();
        assert_eq!(reasons, ["unpaired", "unpaired"]);
        let mut gone: Vec<Incoming> = (0..3).map(|_| next_incoming(&mut rx)).collect();
        gone.sort_by_key(|i| match i {
            Incoming::Disconnected(id) => *id,
            _ => 0,
        });
        assert!(matches!(gone[0], Incoming::Unpaired));
        assert!(matches!(gone[1], Incoming::Disconnected(5)));
        assert!(matches!(gone[2], Incoming::Disconnected(6)));
        assert_eq!(client.client_count(), 0);
        assert!(access.lock().unwrap().devices.is_empty());
    }

    #[test]
    fn a_phone_that_cannot_prove_itself_is_cut_off() {
        let me = Identity::generate();
        let label = me.label();
        let public = me.public_raw().to_vec();
        let relay = FakeRelay::start(&label);
        let access = access_with_tickets(&[]);
        let (client, mut rx) = RelayClient::start(&relay.base(), me, access.clone());
        let (mut ws, _) = relay.accept_and_claim(&public);

        // The wrong secret: no device, the pairing counts a failure.
        access.lock().unwrap().begin_pairing_relay();
        let mut phone = FakePhone::greet(&mut ws, &label, 1, None);
        phone.say(&mut ws, json!({"pair": {"secret": "nope", "name": "x"}}));
        assert_eq!(closed(&mut ws, 1), "wrong_secret");
        assert!(access.lock().unwrap().devices.is_empty());
        assert!(
            access.lock().unwrap().pairing().is_some(),
            "one wrong try does not spend it"
        );

        // A token nobody has.
        let mut phone = FakePhone::greet(&mut ws, &label, 2, Some("ghost"));
        phone.say(&mut ws, json!({"auth": {"token": "stolen"}}));
        assert_eq!(closed(&mut ws, 2), "unauthorized");

        // Data before the hello.
        tell(
            &mut ws,
            json!({"type": "open", "channel": 3, "device": null}),
        );
        ws.send(Message::Binary(frame(DATA, 3, b"early").into()))
            .unwrap();
        assert_eq!(closed(&mut ws, 3), "bad_frame");

        // A hello that is not one.
        tell(
            &mut ws,
            json!({"type": "open", "channel": 4, "device": null}),
        );
        ws.send(Message::Binary(
            frame(HELLO, 4, br#"{"v":1,"e":"short"}"#).into(),
        ))
        .unwrap();
        assert_eq!(closed(&mut ws, 4), "bad_hello");

        // A paired phone whose frame does not open (replayed): out, and the shell hears it leave.
        let (_, secret) = access.lock().unwrap().begin_pairing_relay();
        let mut phone = FakePhone::greet(&mut ws, &label, 5, None);
        phone.say(&mut ws, json!({"pair": {"secret": secret, "name": "x"}}));
        assert!(phone.hear(&mut ws).get("paired").is_some());
        let _tickets = control(&mut ws);
        assert!(matches!(next_incoming(&mut rx), Incoming::Paired));
        assert!(matches!(next_incoming(&mut rx), Incoming::Connected(5, _)));
        let replay = phone.session.seal(br#"{"type":"ping"}"#);
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
        let (_client, mut rx) = RelayClient::start(&relay.base(), me, access_with_tickets(&[]));
        let (stream, _) = relay.listener.accept().unwrap();
        let mut ws = tungstenite::accept(stream).unwrap();
        tell(
            &mut ws,
            json!({"type": "challenge", "nonce": B64.encode([1u8; 16])}),
        );
        let _claim = control(&mut ws);
        tell(&mut ws, json!({"type": "bye", "reason": "wrong_label"}));
        ws.close(Some(tungstenite::protocol::CloseFrame {
            code: CloseCode::from(4403),
            reason: "wrong_label".into(),
        }))
        .unwrap();
        let _ = ws.flush();
        assert_eq!(next_status(&mut rx), Status::Connecting);
        assert_eq!(
            next_status(&mut rx),
            Status::Rejected("This address belongs to another key.".into())
        );
        // No second dial.
        relay.listener.set_nonblocking(true).unwrap();
        std::thread::sleep(Duration::from_millis(300));
        assert!(relay.listener.accept().is_err());
    }

    #[test]
    fn a_block_is_asked_about_again_after_a_long_wait() {
        let me = Identity::generate();
        let public = me.public_raw().to_vec();
        let relay = FakeRelay::start(&me.label());
        let (_client, mut rx) = RelayClient::start(&relay.base(), me, access_with_tickets(&[]));
        let (mut ws, _) = relay.accept_and_claim(&public);
        assert_eq!(next_status(&mut rx), Status::Connecting);
        assert_eq!(next_status(&mut rx), Status::Connected);
        tell(&mut ws, json!({"type": "bye", "reason": "blocked"}));
        ws.close(Some(tungstenite::protocol::CloseFrame {
            code: CloseCode::from(4403),
            reason: "blocked".into(),
        }))
        .unwrap();
        let _ = ws.flush();
        assert_eq!(
            next_status(&mut rx),
            Status::Refused {
                why: "The relay operator blocked this address.".into(),
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
        let public = me.public_raw().to_vec();
        let relay = FakeRelay::start(&me.label());
        let (_client, mut rx) = RelayClient::start(&relay.base(), me, access_with_tickets(&[]));
        assert_eq!(answer_http(&relay, "403 Forbidden", ""), "/pc");
        // The relay itself does not know of a block.
        assert_eq!(answer_http(&relay, "200 OK", r#"{"label":"x"}"#), "/_relay");
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
    fn a_blocked_label_at_the_door_waits_long() {
        let me = Identity::generate();
        let relay = FakeRelay::start(&me.label());
        let (_client, mut rx) = RelayClient::start(&relay.base(), me, access_with_tickets(&[]));
        assert_eq!(answer_http(&relay, "403 Forbidden", "blocked"), "/pc");
        assert_eq!(
            answer_http(&relay, "403 Forbidden", r#"{"label":"x","blocked":true}"#),
            "/_relay"
        );
        assert_eq!(next_status(&mut rx), Status::Connecting);
        assert_eq!(
            next_status(&mut rx),
            Status::Refused {
                why: "The relay operator blocked this address.".into(),
                retry_in: REFUSED_WAIT,
            }
        );
    }

    #[test]
    fn waits_read_in_seconds_then_minutes() {
        assert_eq!(wait_text(Duration::from_millis(200)), "1 s");
        assert_eq!(wait_text(Duration::from_secs(60)), "60 s");
        assert_eq!(wait_text(Duration::from_secs(600)), "10 min");
    }
}
