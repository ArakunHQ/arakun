//! The app's connection to one server, kept up from a thread of its own: it
//! pairs when it starts from a link, dials again when the connection drops,
//! waiting longer each time, and gives up only when the server will not have
//! this app any more. The shell hears how it goes as [`Event`]s.
//!
//! The app is the phone here. To a server with a socket of its own it pairs
//! over HTTP and then talks JSON on a WebSocket, as the page does. Through the
//! relay it does what `phone/relay.js` does: one WebSocket to the relay, a
//! handshake that proves the other end holds the key the address names
//! (`phone::e2e`), and every message sealed from then on.

use std::net::{TcpStream, ToSocketAddrs as _};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, mpsc as std_mpsc};
use std::time::{Duration, Instant};

use futures::channel::mpsc;
use serde_json::{Value, json};
use tungstenite::{Message, WebSocket, stream::MaybeTlsStream};

use super::{Invite, Saved, Way, socket_origin};
use crate::phone::e2e::{Handshake, Session};
use crate::phone::identity::is_label;
use crate::phone::relay::{CONTROL, DATA, HELLO};
use crate::phone::wire;

/// The first wait after a drop; each failure doubles it up to `RETRY_MAX`.
const RETRY_MIN: Duration = Duration::from_secs(1);
const RETRY_MAX: Duration = Duration::from_secs(60);
/// How long dialing and the handshakes may take together.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
/// How long a read waits before the loop looks at what there is to send.
const READ_TICK: Duration = Duration::from_millis(20);
/// How often the stop flag is looked at between dials.
const WAIT_TICK: Duration = Duration::from_millis(100);

/// How often the other end hears from us while nothing else is said, and how
/// long it may say nothing back before the connection counts as lost.
#[derive(Clone, Copy)]
struct Timing {
    ping: Duration,
    silence: Duration,
}

const TIMING: Timing = Timing {
    ping: Duration::from_secs(30),
    silence: Duration::from_secs(90),
};

/// How the connection to a server is doing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Status {
    Connecting,
    Connected,
    /// At the relay, where the server is not right now: it may be off, or its
    /// network down. It is picked up when it comes back.
    ServerAway,
    /// Dropped for now; dialing again after the wait.
    Waiting {
        why: String,
        retry_in: Duration,
    },
    /// The server will not have this app, and asking again would not change
    /// that: it was unpaired, the link ran out, or another machine answers there.
    Refused(String),
}

pub enum Event {
    Status(Status),
    /// Pairing worked: what to keep to come back with.
    Paired(Saved),
    /// A message from the server, whole (`wire`'s `part`s put back together).
    Message(Value),
    /// A message [`Client::send`] was given that never went out: the
    /// connection dropped first. What the user typed in it can go back.
    Undelivered(Value),
}

/// A server's events come with the id the app knows it by.
pub type Events = mpsc::UnboundedSender<(String, Event)>;

/// What the shell asks of the connection's thread.
enum Out {
    /// A message to the server.
    Say(String),
    /// Tell the server to forget this app, then hang up for good.
    Unpair,
}

/// The connection. Dropping it hangs up.
pub struct Client {
    id: String,
    out: std_mpsc::Sender<Out>,
    stop: Arc<AtomicBool>,
    /// Set by the connection's thread when it gives up for good. Held while
    /// a message is queued, so none slips in after its last look at the queue.
    closed: Arc<Mutex<bool>>,
    events: Events,
}

impl Client {
    /// Pairs with the server `invite` names and stays connected. `id` is the
    /// one its [`Saved`] will carry.
    pub fn pair(id: String, invite: Invite, name: String, events: Events) -> Client {
        Self::start(id, Who::Invite(invite), name, events, TIMING)
    }

    /// Connects to a server paired earlier.
    pub fn connect(saved: Saved, events: Events) -> Client {
        Self::start(
            saved.id.clone(),
            Who::Saved(saved),
            String::new(),
            events,
            TIMING,
        )
    }

    fn start(id: String, who: Who, name: String, events: Events, timing: Timing) -> Client {
        let (out, queued) = std_mpsc::channel();
        let stop = Arc::new(AtomicBool::new(false));
        let closed = Arc::new(Mutex::new(false));
        let link = Link {
            id: id.clone(),
            who,
            name,
            queued,
            events: events.clone(),
            stop: stop.clone(),
            closed: closed.clone(),
            status: None,
            parts: wire::Parts::default(),
            timing,
            torn: false,
        };
        let _ = std::thread::Builder::new()
            .name("arakun-server-link".into())
            .spawn(move || link.run());
        Client {
            id,
            out,
            stop,
            closed,
            events,
        }
    }

    /// A message to the server (the phone's `ClientMsg`, as JSON). Dropped
    /// while there is no connection, and told back as [`Event::Undelivered`]:
    /// the server says where things stand again when there is one. After the
    /// connection gave up for good, it is told back at once.
    pub fn send(&self, msg: &Value) {
        let closed = self.closed.lock().unwrap_or_else(|e| e.into_inner());
        if *closed || self.out.send(Out::Say(msg.to_string())).is_err() {
            let _ = self
                .events
                .unbounded_send((self.id.clone(), Event::Undelivered(msg.clone())));
        }
    }

    /// Has the server forget this app, when it can be told right now, and
    /// hangs up either way. A server that was away keeps the device in its
    /// list; the token it gave is gone from here all the same.
    pub fn unpair(&self) {
        let _ = self.out.send(Out::Unpair);
    }
}

impl Drop for Client {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

/// Who the link is, to the server: a stranger with a link, or a paired device.
enum Who {
    Invite(Invite),
    Saved(Saved),
}

/// Why one connection ended, and whether another should follow.
#[derive(Debug)]
enum Ended {
    /// Dial again after a wait.
    Retry(String),
    /// Do not come back until the user does something.
    Refused(String),
    /// The client was dropped.
    Stopped,
}

type Socket = WebSocket<MaybeTlsStream<TcpStream>>;

struct Link {
    id: String,
    who: Who,
    /// What this app asks to be listed as when it pairs.
    name: String,
    queued: std_mpsc::Receiver<Out>,
    events: Events,
    stop: Arc<AtomicBool>,
    closed: Arc<Mutex<bool>>,
    /// The last status told, so the same one is not told twice.
    status: Option<Status>,
    parts: wire::Parts,
    timing: Timing,
    /// A write failed and the socket may still hold that frame: it is
    /// dropped without a close or a flush, so what was told back as
    /// undelivered does not go out after all.
    torn: bool,
}

impl Link {
    fn run(mut self) {
        let mut wait = RETRY_MIN;
        loop {
            if self.stop.load(Ordering::Relaxed) {
                return;
            }
            self.set_status(Status::Connecting);
            let through_relay = match &self.who {
                Who::Invite(invite) => invite.secret.is_some(),
                Who::Saved(saved) => matches!(saved.way, Way::Relay { .. }),
            };
            let ended = if through_relay {
                self.relay(&mut wait)
            } else {
                self.direct(&mut wait)
            };
            match ended {
                Ended::Stopped => return,
                Ended::Refused(why) => {
                    self.set_status(Status::Refused(why));
                    self.close();
                    return;
                }
                // A link works once and for two minutes: a pairing that did
                // not get through is not tried again behind the user's back.
                Ended::Retry(why) if matches!(self.who, Who::Invite(_)) => {
                    self.set_status(Status::Refused(why));
                    self.close();
                    return;
                }
                Ended::Retry(why) => {
                    self.set_status(Status::Waiting {
                        why,
                        retry_in: wait,
                    });
                    let until = Instant::now() + wait;
                    while Instant::now() < until {
                        if self.stop.load(Ordering::Relaxed) {
                            return;
                        }
                        self.forget_queued();
                        std::thread::sleep(WAIT_TICK);
                    }
                    wait = (wait * 2).min(RETRY_MAX);
                }
            }
        }
    }

    fn tell(&self, event: Event) {
        let _ = self.events.unbounded_send((self.id.clone(), event));
    }

    fn set_status(&mut self, status: Status) {
        if self.status.as_ref() != Some(&status) {
            self.status = Some(status.clone());
            self.tell(Event::Status(status));
        }
    }

    /// What was asked while there was no connection has nowhere to go.
    fn forget_queued(&self) {
        while let Ok(out) = self.queued.try_recv() {
            if let Out::Say(text) = out {
                self.undelivered(&text);
            }
        }
    }

    /// No more connections: what is queued, and what is sent from now on, is told back.
    fn close(&self) {
        let mut closed = self.closed.lock().unwrap_or_else(|e| e.into_inner());
        *closed = true;
        self.forget_queued();
    }

    /// Ends a connection. After a failed write the frame may still be in
    /// the socket's buffer, and a close or flush would send it.
    fn hang_up(&mut self, ws: &mut Socket) {
        if !std::mem::take(&mut self.torn) {
            let _ = ws.close(None);
            let _ = ws.flush();
        }
    }

    /// `text` did not go out: the shell hears what it was.
    fn undelivered(&self, text: &str) {
        if let Ok(msg) = serde_json::from_str(text) {
            self.tell(Event::Undelivered(msg));
        }
    }

    /// Pairing gave the app its place on the server: from now on it comes as itself.
    fn paired(&mut self, saved: Saved) {
        self.who = Who::Saved(saved.clone());
        self.tell(Event::Paired(saved));
    }

    /// A message from the server, once all of it is here.
    fn heard(&mut self, msg: Value) {
        if let Some(whole) = self.parts.take(msg) {
            self.tell(Event::Message(whole));
        }
    }

    /// In, by whichever way: the server will now say where things stand.
    fn connected(&mut self, wait: &mut Duration) {
        *wait = RETRY_MIN;
        self.forget_queued();
        self.set_status(Status::Connected);
    }

    // ---- a socket of the server's own -------------------------------------

    fn direct(&mut self, wait: &mut Duration) -> Ended {
        if let Who::Invite(invite) = &self.who {
            match pair_direct(invite, &self.name, &self.id) {
                Ok(saved) => self.paired(saved),
                Err(ended) => return ended,
            }
        }
        let Who::Saved(saved) = &self.who else {
            return Ended::Refused("Pairing did not finish.".into());
        };
        let url = format!("{}/ws?t={}", socket_origin(&saved.origin), saved.token);
        let unpair = (format!("{}/unpair", saved.origin), saved.token.clone());
        let mut ws = match connect(&url) {
            Ok(ws) => ws,
            Err(ended) => return ended,
        };
        self.connected(wait);
        let ended = self.talk_direct(&mut ws, &unpair);
        self.hang_up(&mut ws);
        ended
    }

    /// `unpair` is where the server is told to forget this app, and the token to say it with.
    fn talk_direct(&mut self, ws: &mut Socket, unpair: &(String, String)) -> Ended {
        let mut pulse = Pulse::new(self.timing);
        loop {
            // Read before the queue is: `unpair` is queued before the client
            // is dropped, so a stop seen here has its unpair in the queue below.
            let stopping = self.stop.load(Ordering::Relaxed);
            match ws.read() {
                Ok(Message::Text(text)) => {
                    pulse.heard();
                    if let Ok(msg) = serde_json::from_str::<Value>(text.as_str()) {
                        self.heard(msg);
                    }
                }
                Ok(Message::Close(_)) => {
                    let _ = ws.flush();
                    return Ended::Retry("The server closed the connection.".into());
                }
                Ok(_) => pulse.heard(),
                Err(e) if would_block(&e) => {}
                Err(e) => return Ended::Retry(format!("The connection dropped: {e}")),
            }
            while let Ok(out) = self.queued.try_recv() {
                match out {
                    Out::Say(text) => {
                        if let Err(e) = ws.send(Message::text(text.as_str())) {
                            self.torn = true;
                            self.undelivered(&text);
                            return Ended::Retry(format!("Could not write to the server: {e}"));
                        }
                    }
                    Out::Unpair => {
                        let _ = fetch("POST", &unpair.0, None, Some(&unpair.1));
                        return Ended::Stopped;
                    }
                }
            }
            if stopping {
                return Ended::Stopped;
            }
            if let Err(ended) = pulse.beat(ws) {
                return ended;
            }
        }
    }

    // ---- through the relay ------------------------------------------------

    fn relay(&mut self, wait: &mut Duration) -> Ended {
        let (origin, label, query) = match &self.who {
            Who::Invite(invite) => match relay_label(&invite.origin) {
                Ok(label) => (
                    invite.origin.clone(),
                    label,
                    format!("p={}", percent(&invite.code)),
                ),
                Err(ended) => return ended,
            },
            Who::Saved(saved) => {
                let Way::Relay { label, ticket } = &saved.way else {
                    return Ended::Refused("This server is not reached through a relay.".into());
                };
                (
                    saved.origin.clone(),
                    label.clone(),
                    format!("d={}&t={}", percent(&saved.device), percent(ticket)),
                )
            }
        };
        let url = format!("{}/ws?{query}", socket_origin(&origin));
        let mut ws = match connect(&url) {
            Ok(ws) => ws,
            Err(ended) => return ended,
        };
        let ended = self.talk_relay(&mut ws, &origin, &label, wait);
        self.hang_up(&mut ws);
        ended
    }

    fn talk_relay(
        &mut self,
        ws: &mut Socket,
        origin: &str,
        label: &str,
        wait: &mut Duration,
    ) -> Ended {
        let mut pulse = Pulse::new(self.timing);
        let mut channel = 0u32;
        // A hello sent and not yet answered; then the keys it agreed on.
        let mut handshake: Option<Handshake> = None;
        let mut session: Option<Session> = None;
        // The server knows who this is: messages may go.
        let mut ready = false;
        loop {
            // As in `talk_direct`: a stop is acted on after the queue is read.
            let stopping = self.stop.load(Ordering::Relaxed);
            match ws.read() {
                Ok(Message::Binary(bytes)) => {
                    pulse.heard();
                    let Some((&kind, payload)) = bytes.split_first() else {
                        continue;
                    };
                    match kind {
                        CONTROL => {
                            let msg: Value = serde_json::from_slice(payload).unwrap_or_default();
                            match msg["type"].as_str().unwrap_or("") {
                                // The relay let us in, or the server came back
                                // and is new to this channel: it waits for a hello.
                                kind @ ("open" | "pc_online") => {
                                    if kind == "open" {
                                        channel = msg["channel"].as_u64().unwrap_or(0) as u32;
                                    }
                                    (session, ready) = (None, false);
                                    let hs = Handshake::begin();
                                    if let Err(ended) = send_frame(ws, HELLO, &hs.hello()) {
                                        return ended;
                                    }
                                    handshake = Some(hs);
                                }
                                "pc_offline" => {
                                    (handshake, session, ready) = (None, None, false);
                                    self.set_status(Status::ServerAway);
                                }
                                "bye" => {
                                    return hung_up(msg["reason"].as_str().unwrap_or(""), None);
                                }
                                "limit" => {
                                    return Ended::Retry(
                                        "The relay is holding this connection back for now.".into(),
                                    );
                                }
                                _ => {}
                            }
                        }
                        HELLO => {
                            let Some(hs) = handshake.take() else { continue };
                            match hs.finish(payload, label, channel) {
                                Ok(agreed) => session = Some(agreed),
                                // Whoever answered does not hold the key the address names.
                                Err(e) => {
                                    return Ended::Refused(format!(
                                        "The machine at this address is not the server that was paired ({e})."
                                    ));
                                }
                            }
                            let who = match &self.who {
                                Who::Saved(saved) => json!({ "auth": { "token": saved.token } }),
                                Who::Invite(invite) => json!({ "pair": {
                                    "secret": invite.secret.clone().unwrap_or_default(),
                                    "name": self.name,
                                } }),
                            };
                            let sealed = session
                                .as_mut()
                                .map(|s| s.seal(who.to_string().as_bytes()))
                                .unwrap_or_default();
                            if let Err(ended) = send_frame(ws, DATA, &sealed) {
                                return ended;
                            }
                        }
                        DATA => {
                            let Some(open) = session.as_mut() else {
                                continue;
                            };
                            let msg: Value = match open
                                .open(payload)
                                .ok()
                                .and_then(|plain| serde_json::from_slice(&plain).ok())
                            {
                                Some(msg) => msg,
                                None => {
                                    return Ended::Retry(
                                        "A message from the server did not open.".into(),
                                    );
                                }
                            };
                            if let Some(p) = msg.get("paired") {
                                let text = |key: &str| p[key].as_str().unwrap_or("").to_string();
                                self.paired(Saved {
                                    id: self.id.clone(),
                                    name: text("host"),
                                    origin: origin.to_string(),
                                    way: Way::Relay {
                                        label: label.to_string(),
                                        ticket: text("ticket"),
                                    },
                                    device: text("id"),
                                    token: text("token"),
                                });
                                ready = true;
                                self.connected(wait);
                            } else if msg.get("ok").is_some() {
                                ready = true;
                                self.connected(wait);
                            } else if msg.get("res").is_none() {
                                // `res` answers a `req`, and the app asks none.
                                self.heard(msg);
                            }
                        }
                        _ => {}
                    }
                }
                Ok(Message::Close(frame)) => {
                    let _ = ws.flush();
                    let reason = frame
                        .as_ref()
                        .map(|f| f.reason.to_string())
                        .unwrap_or_default();
                    return hung_up(&reason, frame.map(|f| u16::from(f.code)));
                }
                Ok(_) => pulse.heard(),
                Err(e) if would_block(&e) => {}
                Err(e) => return Ended::Retry(format!("The connection dropped: {e}")),
            }
            while let Ok(out) = self.queued.try_recv() {
                // Inside the channel, what would be an HTTP request is a `req`.
                let (text, last) = match out {
                    Out::Say(text) => (text, false),
                    Out::Unpair => (
                        json!({ "req": { "id": 1, "method": "POST", "path": "/unpair", "body": null } })
                            .to_string(),
                        true,
                    ),
                };
                // Before the server knows who this is, it would not listen.
                if let (true, Some(seal)) = (ready, session.as_mut()) {
                    let sealed = seal.seal(text.as_bytes());
                    if let Err(ended) = send_frame(ws, DATA, &sealed) {
                        self.torn = true;
                        if !last {
                            self.undelivered(&text);
                        }
                        return ended;
                    }
                } else if !last {
                    self.undelivered(&text);
                }
                if last {
                    return Ended::Stopped;
                }
            }
            if stopping {
                return Ended::Stopped;
            }
            if let Err(ended) = pulse.beat(ws) {
                return ended;
            }
        }
    }
}

/// Pings in the silences, and notices when nothing comes back.
struct Pulse {
    timing: Timing,
    last_heard: Instant,
    last_ping: Instant,
}

impl Pulse {
    fn new(timing: Timing) -> Pulse {
        Pulse {
            timing,
            last_heard: Instant::now(),
            last_ping: Instant::now(),
        }
    }

    fn heard(&mut self) {
        self.last_heard = Instant::now();
    }

    fn beat(&mut self, ws: &mut Socket) -> Result<(), Ended> {
        if self.last_heard.elapsed() >= self.timing.silence {
            return Err(Ended::Retry("The server went quiet.".into()));
        }
        if self.last_ping.elapsed() >= self.timing.ping {
            self.last_ping = Instant::now();
            ws.send(Message::Ping(Vec::new().into()))
                .map_err(|e| Ended::Retry(format!("Could not write to the server: {e}")))?;
        }
        Ok(())
    }
}

fn would_block(e: &tungstenite::Error) -> bool {
    matches!(e, tungstenite::Error::Io(e) if matches!(
        e.kind(),
        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
    ))
}

/// A frame as a phone sends it: the type, then the payload. The relay adds
/// the channel on the server's side.
fn send_frame(ws: &mut Socket, kind: u8, payload: &[u8]) -> Result<(), Ended> {
    let mut frame = Vec::with_capacity(1 + payload.len());
    frame.push(kind);
    frame.extend_from_slice(payload);
    ws.send(Message::Binary(frame.into()))
        .map_err(|e| Ended::Retry(format!("Could not write to the relay: {e}")))
}

/// What the relay or the server said as it hung up, in the app's terms.
fn hung_up(reason: &str, code: Option<u16>) -> Ended {
    match reason {
        "unauthorized" | "revoked" | "bad_ticket" | "unpaired" | "wrong_secret" => {
            Ended::Refused("This app is no longer paired with the server.".into())
        }
        "no_pairing" | "bad_code" | "no_credentials" => Ended::Refused(
            "The link has run out or was used already; ask the server for a new one.".into(),
        ),
        "untrusted" | "bad_hello" => {
            Ended::Refused("The machine at this address is not the server that was paired.".into())
        }
        _ if code == Some(4401) => {
            Ended::Refused("This app is no longer paired with the server.".into())
        }
        _ if code == Some(4429) || reason.starts_with("limit:") => {
            Ended::Retry("The relay is holding this connection back for now.".into())
        }
        "" => Ended::Retry("The relay closed the connection.".into()),
        other => Ended::Retry(format!("The relay hung up: {other}")),
    }
}

/// `value` as it goes in a query: what is not plainly safe is escaped.
fn percent(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for b in value.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

fn agent() -> &'static ureq::Agent {
    static AGENT: std::sync::OnceLock<ureq::Agent> = std::sync::OnceLock::new();
    AGENT.get_or_init(|| {
        ureq::Agent::config_builder()
            .timeout_global(Some(CONNECT_TIMEOUT))
            .http_status_as_error(false)
            .user_agent("arakun")
            .build()
            .into()
    })
}

/// A small JSON answer to a request, with its status. `bearer` is the token
/// the server knows this app by, where the request needs one.
fn fetch(
    method: &str,
    url: &str,
    body: Option<String>,
    bearer: Option<&str>,
) -> Result<(u16, Value), String> {
    let mut request = ureq::http::Request::builder()
        .method(method)
        .uri(url)
        .header("Content-Type", "application/json");
    if let Some(token) = bearer {
        request = request.header("Authorization", format!("Bearer {token}"));
    }
    let sent = match body {
        Some(body) => request
            .body(body)
            .map_err(|e| e.to_string())
            .and_then(|r| agent().run(r).map_err(|e| e.to_string())),
        None => request
            .body(())
            .map_err(|e| e.to_string())
            .and_then(|r| agent().run(r).map_err(|e| e.to_string())),
    };
    let mut response = sent?;
    let text = response
        .body_mut()
        .with_config()
        .limit(64 * 1024)
        .read_to_string()
        .unwrap_or_default();
    Ok((
        response.status().as_u16(),
        serde_json::from_str(&text).unwrap_or_default(),
    ))
}

/// Pairs with a server on a socket of its own: the code for a token.
fn pair_direct(invite: &Invite, name: &str, id: &str) -> Result<Saved, Ended> {
    let body = json!({ "code": invite.code, "name": name }).to_string();
    let (status, answer) = fetch("POST", &format!("{}/pair", invite.origin), Some(body), None)
        .map_err(|e| Ended::Refused(format!("Could not reach {}: {e}", invite.origin)))?;
    let text = |key: &str| answer[key].as_str().unwrap_or("").to_string();
    match status {
        200 if !text("token").is_empty() => Ok(Saved {
            id: id.to_string(),
            name: text("host"),
            origin: invite.origin.clone(),
            way: Way::Direct,
            device: text("id"),
            token: text("token"),
        }),
        403 => Err(Ended::Refused(
            "The link has run out or was used already; ask the server for a new one.".into(),
        )),
        other => Err(Ended::Refused(format!(
            "{} answered {other} to the pairing.",
            invite.origin
        ))),
    }
}

/// What a relay address says about whose it is.
#[derive(Debug, PartialEq)]
enum Named {
    /// `<label>.<relay>`: the address itself names the server's key.
    Label(String),
    /// Localhost or an address on a private network: a relay run by hand,
    /// which has no names to give.
    Local,
    /// A name that starts with no label.
    Nothing,
}

fn named(origin: &str) -> Named {
    let host = origin.split_once("://").map_or(origin, |(_, rest)| rest);
    let host = host.split(['/', '?', '#']).next().unwrap_or("");
    let name = match host.rsplit_once(':') {
        Some((name, port)) if !port.is_empty() && port.bytes().all(|b| b.is_ascii_digit()) => name,
        _ => host,
    };
    let name = name.trim_matches(['[', ']']);
    if name == "localhost" {
        return Named::Local;
    }
    if let Ok(ip) = name.parse::<std::net::IpAddr>() {
        // Across the internet whoever sits on the way could answer for the
        // relay and name a key of their own. A public address needs a name.
        return if near(ip) {
            Named::Local
        } else {
            Named::Nothing
        };
    }
    match name.split('.').next() {
        Some(first) if is_label(first) => Named::Label(first.to_string()),
        _ => Named::Nothing,
    }
}

/// An address on this machine or a network of one's own: loopback, the
/// private ranges, link-local, and the range Tailscale gives out.
fn near(ip: std::net::IpAddr) -> bool {
    match ip {
        std::net::IpAddr::V4(ip) => {
            let [a, b, ..] = ip.octets();
            ip.is_loopback()
                || ip.is_private()
                || ip.is_link_local()
                || (a == 100 && (64..128).contains(&b))
        }
        std::net::IpAddr::V6(ip) => {
            let first = ip.segments()[0];
            ip.is_loopback() || first & 0xfe00 == 0xfc00 || first & 0xffc0 == 0xfe80
        }
    }
}

/// The label of the server a relay address stands for: the key every hello
/// from that address is checked against.
///
/// It is read off the address, which came in the link from the server's own
/// terminal. The relay is not asked. A relay that could say which key to
/// trust could name one of its own, answer the pairing in the server's place,
/// and read the secret that is there to keep it out. Only a relay run by hand
/// on localhost or at an address of a private network says which label it
/// serves: it has no names, and whoever runs it is the one pairing.
fn relay_label(origin: &str) -> Result<String, Ended> {
    match named(origin) {
        Named::Label(label) => return Ok(label),
        Named::Nothing => {
            return Err(Ended::Refused(
                "That address names no server: a relay link starts with the server's label. A relay at a public address needs a name.".into(),
            ));
        }
        Named::Local => {}
    }
    let (status, answer) = fetch("GET", &format!("{origin}/_relay"), None, None)
        .map_err(|e| Ended::Refused(format!("Could not reach {origin}: {e}")))?;
    let label = answer["label"].as_str().unwrap_or("");
    match status {
        200 if is_label(label) => Ok(label.to_string()),
        403 => Err(Ended::Refused("The relay has blocked this address.".into())),
        503 => Err(Ended::Refused(
            answer["disabled"]
                .as_str()
                .unwrap_or("The relay is closed right now.")
                .to_string(),
        )),
        _ => Err(Ended::Refused(format!("{origin} is not a relay."))),
    }
}

/// TCP, TLS when the address asks for it, and the WebSocket handshake.
fn connect(url: &str) -> Result<Socket, Ended> {
    let (host, port) =
        host_port(url).ok_or_else(|| Ended::Refused(format!("Bad address: {url}")))?;
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
    let stream = stream.ok_or_else(|| Ended::Retry(format!("Could not reach {host}:{port}.")))?;
    let _ = stream.set_nodelay(true);
    let _ = stream.set_read_timeout(Some(CONNECT_TIMEOUT));
    let _ = stream.set_write_timeout(Some(CONNECT_TIMEOUT));
    // A `ws://` address comes back as a plain stream in the same wrapper.
    let (ws, _) = tungstenite::client_tls(url, stream).map_err(|e| match e {
        tungstenite::HandshakeError::Failure(tungstenite::Error::Http(res)) => {
            match res.status().as_u16() {
                // The door itself said no: the token or the ticket is no longer good.
                401 | 403 => Ended::Refused("This app is no longer paired with the server.".into()),
                status => Ended::Retry(format!("{host} answered {status}.")),
            }
        }
        tungstenite::HandshakeError::Failure(e) => Ended::Retry(format!("Could not connect: {e}")),
        // The stream blocks, so the handshake runs to its end.
        tungstenite::HandshakeError::Interrupted(_) => {
            Ended::Retry("The handshake was cut short.".into())
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

/// Host and port out of a `ws://` or `wss://` address.
fn host_port(url: &str) -> Option<(String, u16)> {
    let (scheme, rest) = url.split_once("://")?;
    let default = match scheme {
        "wss" => 443,
        "ws" => 80,
        _ => return None,
    };
    let host_port = rest.split(['/', '?', '#']).next()?;
    let (host, port) = match host_port.rsplit_once(':') {
        Some((h, p)) if !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()) => {
            (h, p.parse().ok()?)
        }
        _ => (host_port, default),
    };
    (!host.is_empty()).then(|| (host.trim_matches(['[', ']']).to_string(), port))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::phone::e2e::PhoneHello;
    use crate::phone::identity::Identity;
    use crate::phone::{Access, ClientMsg, Incoming, Server};
    use futures::StreamExt as _;
    use std::io::{Read as _, Write as _};
    use std::net::{Ipv4Addr, TcpListener};

    const QUICK: Timing = Timing {
        ping: Duration::from_millis(40),
        silence: Duration::from_millis(400),
    };

    type Heard = mpsc::UnboundedReceiver<(String, Event)>;

    /// The next event, or a panic when none comes in time.
    fn next(rx: &mut Heard) -> Event {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            match rx.try_recv() {
                Ok((id, event)) => {
                    assert_eq!(id, "srv");
                    return event;
                }
                Err(_) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(5)),
                Err(_) => panic!("no event from the client"),
            }
        }
    }

    fn status(rx: &mut Heard) -> Status {
        match next(rx) {
            Event::Status(s) => s,
            Event::Paired(_) => panic!("paired, where a status was expected"),
            Event::Message(m) => panic!("{m}, where a status was expected"),
            Event::Undelivered(m) => panic!("{m} undelivered, where a status was expected"),
        }
    }

    fn from_server(rx: &mut mpsc::UnboundedReceiver<Incoming>) -> Incoming {
        futures::executor::block_on(rx.next()).expect("the server is listening")
    }

    /// Whether the server heard the unpair in time. The socket closing as it
    /// does races the unpair to the server's channel, so its `Disconnected`
    /// may come first. A lost unpair leaves the channel quiet, hence the deadline.
    fn unpaired(rx: &mut mpsc::UnboundedReceiver<Incoming>) -> bool {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            match rx.try_recv() {
                Ok(Incoming::Unpaired) => return true,
                Ok(Incoming::Disconnected(_)) => {}
                Ok(_) => return false,
                Err(_) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(5)),
                Err(_) => return false,
            }
        }
    }

    #[test]
    fn it_pairs_with_a_server_talks_to_it_and_is_told_when_unpaired() {
        let access = Access::new(Vec::new(), String::new());
        let (server, mut incoming) = Server::start(Ipv4Addr::LOCALHOST, 0, access.clone()).unwrap();
        let origin = format!("http://127.0.0.1:{}", server.port());
        let code = access.lock().unwrap().begin_pairing();

        let (tx, mut rx) = mpsc::unbounded();
        let invite = Invite {
            origin: origin.clone(),
            code,
            secret: None,
        };
        let client = Client::start(
            "srv".into(),
            Who::Invite(invite.clone()),
            "Arakun on a test".into(),
            tx.clone(),
            QUICK,
        );
        assert_eq!(status(&mut rx), Status::Connecting);
        let Event::Paired(saved) = next(&mut rx) else {
            panic!("not paired");
        };
        assert_eq!((saved.id.as_str(), &saved.way), ("srv", &Way::Direct));
        assert_eq!(saved.origin, origin);
        assert_eq!(status(&mut rx), Status::Connected);
        assert!(matches!(from_server(&mut incoming), Incoming::Paired));
        let Incoming::Connected(peer, device) = from_server(&mut incoming) else {
            panic!("no socket on the server");
        };
        assert_eq!(device, saved.device);
        assert_eq!(access.lock().unwrap().devices[0].name, "Arakun on a test");

        // Both ways, and a message that came in pieces arrives as one.
        server.send(peer, json!({ "type": "inbox", "sessions": [] }).to_string());
        let Event::Message(msg) = next(&mut rx) else {
            panic!("no message");
        };
        assert_eq!(msg["type"], "inbox");
        let big = json!({ "type": "transcript", "text": "x".repeat(wire::FRAME_TEXT) });
        for frame in wire::parts(big.to_string()) {
            server.send(peer, frame);
        }
        let Event::Message(msg) = next(&mut rx) else {
            panic!("no message");
        };
        assert_eq!(msg, big);
        client.send(&json!({ "type": "open", "session": "s1" }));
        assert!(matches!(
            from_server(&mut incoming),
            Incoming::Message(_, ClientMsg::Open { session }) if session == "s1"
        ));

        // Several pings went by unanswered by anything but pongs: still connected.
        std::thread::sleep(QUICK.silence * 2);
        assert!(rx.try_recv().is_err());

        // The link worked once.
        let (tx2, mut rx2) = mpsc::unbounded();
        let _again = Client::start("srv".into(), Who::Invite(invite), "x".into(), tx2, QUICK);
        assert_eq!(status(&mut rx2), Status::Connecting);
        assert!(matches!(status(&mut rx2), Status::Refused(why) if why.contains("run out")));

        // Hanging up is heard; coming back as the paired device needs no link.
        drop(client);
        assert!(matches!(
            from_server(&mut incoming),
            Incoming::Disconnected(_)
        ));
        let _client = Client::start(
            "srv".into(),
            Who::Saved(saved.clone()),
            String::new(),
            tx,
            QUICK,
        );
        assert_eq!(status(&mut rx), Status::Connecting);
        assert_eq!(status(&mut rx), Status::Connected);

        // Unpaired on the server: the socket closes, and the door stays shut.
        access.lock().unwrap().unpair(&saved.device);
        server.disconnect_device(&saved.device);
        assert!(matches!(status(&mut rx), Status::Waiting { .. }));
        assert_eq!(status(&mut rx), Status::Connecting);
        assert!(
            matches!(status(&mut rx), Status::Refused(why) if why.contains("no longer paired"))
        );
    }

    #[test]
    fn what_could_not_go_out_is_told_back() {
        // A port nobody listens on: every dial fails.
        let port = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        let saved = Saved {
            id: "srv".into(),
            name: "box".into(),
            origin: format!("http://127.0.0.1:{port}"),
            way: Way::Direct,
            device: "d".into(),
            token: "t".into(),
        };
        let (tx, mut rx) = mpsc::unbounded();
        let client = Client::start("srv".into(), Who::Saved(saved), String::new(), tx, QUICK);
        let prompt = json!({ "type": "send", "session": "s1", "text": "hi" });
        client.send(&prompt);
        loop {
            match next(&mut rx) {
                Event::Undelivered(msg) => {
                    assert_eq!(msg, prompt);
                    break;
                }
                Event::Status(_) => {}
                _ => panic!("only statuses come before it"),
            }
        }
    }

    #[test]
    fn what_a_refusing_server_never_got_is_told_back() {
        let access = Access::new(Vec::new(), String::new());
        let (server, _incoming) = Server::start(Ipv4Addr::LOCALHOST, 0, access).unwrap();
        // No pairing was begun, so the link is refused and not tried again.
        let invite = Invite {
            origin: format!("http://127.0.0.1:{}", server.port()),
            code: "not-a-code".into(),
            secret: None,
        };
        let (tx, mut rx) = mpsc::unbounded();
        let client = Client::start("srv".into(), Who::Invite(invite), "x".into(), tx, QUICK);
        // Queued while pairing: the thread gives up with it still there.
        let queued = json!({ "type": "start", "env": "/srv/app", "text": "hi" });
        client.send(&queued);
        let (mut refused, mut told) = (false, false);
        while !(refused && told) {
            match next(&mut rx) {
                Event::Status(Status::Refused(_)) => refused = true,
                Event::Status(_) => {}
                Event::Undelivered(msg) => {
                    assert_eq!(msg, queued);
                    told = true;
                }
                _ => panic!("nothing else comes"),
            }
        }
        // Sent after the thread is gone: nothing reads it, so it comes back at once.
        let late = json!({ "type": "send", "session": "s1", "text": "late" });
        client.send(&late);
        assert!(matches!(next(&mut rx), Event::Undelivered(msg) if msg == late));
    }

    #[test]
    fn forgetting_a_server_has_it_forget_this_app() {
        let access = Access::new(Vec::new(), String::new());
        let (server, mut incoming) = Server::start(Ipv4Addr::LOCALHOST, 0, access.clone()).unwrap();
        let invite = Invite {
            origin: format!("http://127.0.0.1:{}", server.port()),
            code: access.lock().unwrap().begin_pairing(),
            secret: None,
        };
        let (tx, mut rx) = mpsc::unbounded();
        let client = Client::start("srv".into(), Who::Invite(invite), "x".into(), tx, QUICK);
        assert_eq!(status(&mut rx), Status::Connecting);
        assert!(matches!(next(&mut rx), Event::Paired(_)));
        assert_eq!(status(&mut rx), Status::Connected);
        assert!(matches!(from_server(&mut incoming), Incoming::Paired));
        assert!(matches!(
            from_server(&mut incoming),
            Incoming::Connected(..)
        ));
        assert_eq!(access.lock().unwrap().devices.len(), 1);

        client.unpair();
        assert!(unpaired(&mut incoming));
        assert!(access.lock().unwrap().devices.is_empty());
        // For good: no status follows, and nothing dials again.
        std::thread::sleep(Duration::from_millis(300));
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn forgetting_and_letting_go_at_once_still_unpairs() {
        // "Forget" asks for the unpair and drops the client in one go. The
        // link may be anywhere in its loop then; wherever, the server is told.
        for round in 0..12u64 {
            let access = Access::new(Vec::new(), String::new());
            let (server, mut incoming) =
                Server::start(Ipv4Addr::LOCALHOST, 0, access.clone()).unwrap();
            let invite = Invite {
                origin: format!("http://127.0.0.1:{}", server.port()),
                code: access.lock().unwrap().begin_pairing(),
                secret: None,
            };
            let (tx, mut rx) = mpsc::unbounded();
            let client = Client::start("srv".into(), Who::Invite(invite), "x".into(), tx, QUICK);
            assert_eq!(status(&mut rx), Status::Connecting);
            assert!(matches!(next(&mut rx), Event::Paired(_)));
            assert_eq!(status(&mut rx), Status::Connected);
            assert!(matches!(from_server(&mut incoming), Incoming::Paired));
            assert!(matches!(
                from_server(&mut incoming),
                Incoming::Connected(..)
            ));
            // A different moment of the loop each round.
            std::thread::sleep(Duration::from_millis(round * 7));
            client.unpair();
            drop(client);
            assert!(unpaired(&mut incoming), "round {round}");
            assert!(access.lock().unwrap().devices.is_empty(), "round {round}");
        }
    }

    /// One HTTP request answered by hand: what `/_relay` says.
    fn answer_relay_info(listener: &TcpListener, label: &str) {
        let (mut stream, _) = listener.accept().unwrap();
        let mut head = [0u8; 2048];
        let n = stream.read(&mut head).unwrap();
        assert!(String::from_utf8_lossy(&head[..n]).starts_with("GET /_relay "));
        let body = json!({ "label": label }).to_string();
        write!(
            stream,
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        )
        .unwrap();
    }

    fn phone_frame(kind: u8, payload: &[u8]) -> Message {
        let mut frame = vec![kind];
        frame.extend_from_slice(payload);
        Message::Binary(frame.into())
    }

    fn next_frame(ws: &mut WebSocket<TcpStream>) -> (u8, Vec<u8>) {
        loop {
            match ws.read().unwrap() {
                Message::Binary(b) => return (b[0], b[1..].to_vec()),
                Message::Ping(_) | Message::Pong(_) => continue,
                other => panic!("unexpected {other:?}"),
            }
        }
    }

    /// The relay and the server behind it, for one connection: lets the app
    /// in on `channel`, shakes hands as the server and returns the sealed
    /// session along with the query the app came with.
    // The callback's error type is tungstenite's to choose.
    #[allow(clippy::result_large_err)]
    fn accept_through_relay(
        listener: &TcpListener,
        identity: &Identity,
        channel: u32,
    ) -> (WebSocket<TcpStream>, Session, String) {
        let (stream, _) = listener.accept().unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(10)))
            .unwrap();
        let mut query = String::new();
        let mut ws = tungstenite::accept_hdr(
            stream,
            |req: &tungstenite::handshake::server::Request, res| {
                query = req.uri().query().unwrap_or("").to_string();
                Ok(res)
            },
        )
        .unwrap();
        let open = json!({ "type": "open", "channel": channel }).to_string();
        ws.send(phone_frame(CONTROL, open.as_bytes())).unwrap();
        let (kind, hello) = next_frame(&mut ws);
        assert_eq!(kind, HELLO);
        let hello = PhoneHello::parse(&hello).unwrap();
        let (reply, session) = Handshake::begin()
            .answer(&hello, identity, channel, "test")
            .unwrap();
        ws.send(phone_frame(HELLO, &reply)).unwrap();
        (ws, session, query)
    }

    fn sealed(ws: &mut WebSocket<TcpStream>, session: &mut Session) -> Value {
        let (kind, payload) = next_frame(ws);
        assert_eq!(kind, DATA);
        serde_json::from_slice(&session.open(&payload).unwrap()).unwrap()
    }

    fn seal(ws: &mut WebSocket<TcpStream>, session: &mut Session, msg: Value) {
        ws.send(phone_frame(DATA, &session.seal(msg.to_string().as_bytes())))
            .unwrap();
    }

    #[test]
    fn it_pairs_through_the_relay_and_comes_back_with_its_ticket() {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let origin = format!("http://127.0.0.1:{}", listener.local_addr().unwrap().port());
        let identity = Identity::generate();
        let label = identity.label();

        let (tx, mut rx) = mpsc::unbounded();
        let invite = Invite {
            origin: origin.clone(),
            code: "c0de".into(),
            secret: Some("s3cr3t".into()),
        };
        let client = Client::start(
            "srv".into(),
            Who::Invite(invite),
            "Arakun".into(),
            tx.clone(),
            QUICK,
        );
        assert_eq!(status(&mut rx), Status::Connecting);
        answer_relay_info(&listener, &label);
        let (mut ws, mut session, query) = accept_through_relay(&listener, &identity, 7);
        assert_eq!(query, "p=c0de");
        // Inside the channel the app shows the secret, which the relay never saw.
        let said = sealed(&mut ws, &mut session);
        assert_eq!(
            said,
            json!({ "pair": { "secret": "s3cr3t", "name": "Arakun" } })
        );
        seal(
            &mut ws,
            &mut session,
            json!({ "paired": { "id": "dev1", "name": "Arakun", "token": "tok", "ticket": "tick et", "host": "build-box" } }),
        );
        let Event::Paired(saved) = next(&mut rx) else {
            panic!("not paired");
        };
        assert_eq!(
            saved,
            Saved {
                id: "srv".into(),
                name: "build-box".into(),
                origin: origin.clone(),
                way: Way::Relay {
                    label: label.clone(),
                    ticket: "tick et".into(),
                },
                device: "dev1".into(),
                token: "tok".into(),
            }
        );
        assert_eq!(status(&mut rx), Status::Connected);

        // Sealed both ways from here.
        seal(&mut ws, &mut session, json!({ "type": "inbox" }));
        assert!(matches!(next(&mut rx), Event::Message(m) if m["type"] == "inbox"));
        client.send(&json!({ "type": "cancel", "session": "s1" }));
        assert_eq!(
            sealed(&mut ws, &mut session),
            json!({ "type": "cancel", "session": "s1" })
        );

        // The server goes away and comes back: a new hello, and who this is again.
        let away = json!({ "type": "pc_offline" }).to_string();
        ws.send(phone_frame(CONTROL, away.as_bytes())).unwrap();
        assert_eq!(status(&mut rx), Status::ServerAway);
        let back = json!({ "type": "pc_online" }).to_string();
        ws.send(phone_frame(CONTROL, back.as_bytes())).unwrap();
        let (kind, hello) = next_frame(&mut ws);
        assert_eq!(kind, HELLO);
        let (reply, mut session) = Handshake::begin()
            .answer(&PhoneHello::parse(&hello).unwrap(), &identity, 7, "test")
            .unwrap();
        ws.send(phone_frame(HELLO, &reply)).unwrap();
        assert_eq!(
            sealed(&mut ws, &mut session),
            json!({ "auth": { "token": "tok" } })
        );
        seal(&mut ws, &mut session, json!({ "ok": { "id": "dev1" } }));
        assert_eq!(status(&mut rx), Status::Connected);

        // A new connection brings the ticket to the relay, escaped for a query.
        drop(client);
        let _client = Client::start("srv".into(), Who::Saved(saved), String::new(), tx, QUICK);
        assert_eq!(status(&mut rx), Status::Connecting);
        let (mut ws, mut session, query) = accept_through_relay(&listener, &identity, 9);
        assert_eq!(query, "d=dev1&t=tick%20et");
        assert_eq!(
            sealed(&mut ws, &mut session),
            json!({ "auth": { "token": "tok" } })
        );
        // The relay says the ticket was revoked: nothing to come back for.
        let bye = json!({ "type": "bye", "reason": "revoked" }).to_string();
        ws.send(phone_frame(CONTROL, bye.as_bytes())).unwrap();
        assert!(
            matches!(status(&mut rx), Status::Refused(why) if why.contains("no longer paired"))
        );
    }

    #[test]
    fn a_server_that_holds_another_key_is_refused() {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let origin = format!("http://127.0.0.1:{}", listener.local_addr().unwrap().port());
        let paired_with = Identity::generate();
        let impostor = Identity::generate();
        let saved = Saved {
            id: "srv".into(),
            name: "build-box".into(),
            origin,
            way: Way::Relay {
                label: paired_with.label(),
                ticket: "t".into(),
            },
            device: "dev1".into(),
            token: "tok".into(),
        };
        let (tx, mut rx) = mpsc::unbounded();
        let _client = Client::start("srv".into(), Who::Saved(saved), String::new(), tx, QUICK);
        assert_eq!(status(&mut rx), Status::Connecting);
        // The token never leaves: the hello is checked before anything is said.
        let (_ws, _session, _) = accept_through_relay(&listener, &impostor, 3);
        assert!(matches!(status(&mut rx), Status::Refused(why) if why.contains("not the server")));
    }

    #[test]
    fn what_ends_a_connection_decides_whether_another_follows() {
        assert!(matches!(hung_up("revoked", None), Ended::Refused(_)));
        assert!(matches!(hung_up("bad_code", None), Ended::Refused(_)));
        assert!(matches!(hung_up("", Some(4401)), Ended::Refused(_)));
        assert!(matches!(hung_up("limit:daily", None), Ended::Retry(_)));
        assert!(matches!(hung_up("", Some(4429)), Ended::Retry(_)));
        assert!(matches!(hung_up("", None), Ended::Retry(_)));
        assert_eq!(
            host_port("wss://a.arakun.dev/ws?d=1"),
            Some(("a.arakun.dev".into(), 443))
        );
        assert_eq!(
            host_port("ws://127.0.0.1:8787/ws"),
            Some(("127.0.0.1".into(), 8787))
        );
        assert_eq!(host_port("https://a.arakun.dev"), None);
    }

    #[test]
    fn the_key_to_trust_is_read_off_the_address_not_asked_of_the_relay() {
        assert_eq!(
            named("https://abcdefghij234567.arakun.dev"),
            Named::Label("abcdefghij234567".into())
        );
        assert_eq!(
            named("https://abcdefghij234567.me.workers.dev/"),
            Named::Label("abcdefghij234567".into())
        );
        // A name with no label in front is no server's: nothing is asked of it.
        assert_eq!(named("https://arakun.dev"), Named::Nothing);
        assert_eq!(named("https://relay.example.com"), Named::Nothing);
        assert!(matches!(
            relay_label("https://relay.example.com"),
            Err(Ended::Refused(why)) if why.contains("names no server")
        ));
        // A relay run by hand has no names; it alone says which label it serves.
        for local in [
            "http://127.0.0.1:8787",
            "http://localhost:8787",
            "http://[::1]:8787",
            "http://10.0.0.5",
            "http://192.168.1.20:8787",
            "http://100.101.102.103:8787",
            "http://[fd7a:115c:a1e0::1]:8787",
        ] {
            assert_eq!(named(local), Named::Local, "{local}");
        }
        // At a public address the answer would cross the internet in the
        // clear, and whoever is on the way could give a key of their own.
        for public in [
            "http://203.0.113.9:8787",
            "http://[2001:db8::1]:8787",
            "http://100.200.0.1",
        ] {
            assert_eq!(named(public), Named::Nothing, "{public}");
        }
        assert_eq!(percent("a-b_c.d~e f/+="), "a-b_c.d~e%20f%2F%2B%3D");
    }
}
