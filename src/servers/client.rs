//! The app's connection to one server, kept up from a thread of its own: it
//! pairs when it starts from a link, dials again when the connection drops,
//! waiting longer each time, and gives up only when the server will not have
//! this app any more. The shell hears how it goes as [`Event`]s.
//!
//! The app is the phone here. To a server with a socket of its own it pairs
//! over HTTP and then talks JSON on a WebSocket, as the page does. To a server
//! of the same account it goes through the relay as `phone/relay.js` does:
//! one WebSocket to the relay with the app's sign-in, a handshake that proves
//! the other end holds the key the account lists for it (`phone::e2e`), and
//! every message sealed from then on.

use std::net::{TcpStream, ToSocketAddrs as _};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, mpsc as std_mpsc};
use std::time::{Duration, Instant};

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD as B64;
use futures::channel::mpsc;
use serde_json::{Value, json};
use tungstenite::client::IntoClientRequest as _;
use tungstenite::handshake::client::Request;
use tungstenite::http::HeaderValue;
use tungstenite::{Message, WebSocket, stream::MaybeTlsStream};

use super::{Invite, Saved, Way, socket_origin};
use crate::phone::e2e::{Handshake, Session};
use crate::phone::identity::label_of;
use crate::phone::relay::{CONTROL, DATA, HELLO, Joiner};
use crate::phone::wire;

/// The first wait after a drop; each failure doubles it up to `RETRY_MAX`.
const RETRY_MIN: Duration = Duration::from_secs(1);
const RETRY_MAX: Duration = Duration::from_secs(60);
/// How long dialing and the handshakes may take together.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
/// A server's status when its sign-in through the relay ended.
pub const SIGNED_OUT: &str = "Your sign-in is no longer valid. Sign in again to reach this server.";
/// A server's status while it waits for the account to list it again.
pub const SIGN_IN_TO_RECONNECT: &str = "Sign in to reconnect.";
/// The longest operator's text a refusal passes on.
const MAX_MESSAGE: usize = 300;

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
    /// A message [`Client::send`] was given that the server did not get:
    /// the connection dropped first, before it went out or, with a server
    /// that acknowledges what it reads, before the server said it read it.
    /// What the user typed in it can go back.
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

    /// Connects to a server of the account through the relay, with the
    /// app's sign-in, checking it holds the key the account lists.
    pub fn connect_account(saved: Saved, account: Account, events: Events) -> Client {
        Self::start(
            saved.id.clone(),
            Who::Account(saved, account),
            String::new(),
            events,
            TIMING,
        )
    }

    /// A server that is not to be dialed now, and why: it says so as refused.
    pub fn held(saved: &Saved, why: String, events: Events) -> Client {
        Self::start(
            saved.id.clone(),
            Who::Held(why),
            String::new(),
            events,
            TIMING,
        )
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
            page: crate::servers::new_id(),
            torn: false,
            acks: false,
            seq: 0,
            unacked: Vec::new(),
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
    /// the server says where things stand again when there is one. So is one
    /// that went out but was not acknowledged before the connection dropped,
    /// when the server acknowledges. After the connection gave up for good,
    /// it is told back at once.
    pub fn send(&self, msg: &Value) {
        send(&self.id, &self.out, &self.closed, &self.events, msg);
    }

    /// What sends to the server from any thread, as [`Client::send`] does.
    /// It stops working once the connection is dropped or gives up.
    pub fn sender(&self) -> Sender {
        Sender {
            id: self.id.clone(),
            out: self.out.clone(),
            closed: self.closed.clone(),
            events: self.events.clone(),
        }
    }

    /// Has the server forget this app, when it can be told right now, and
    /// hangs up either way. A server that was away keeps the device in its
    /// list; the token it gave is gone from here all the same.
    pub fn unpair(&self) {
        let _ = self.out.send(Out::Unpair);
    }
}

/// [`Client::sender`]: sends to a server from another thread.
#[derive(Clone)]
pub struct Sender {
    id: String,
    out: std_mpsc::Sender<Out>,
    closed: Arc<Mutex<bool>>,
    events: Events,
}

impl Sender {
    pub fn send(&self, msg: &Value) {
        send(&self.id, &self.out, &self.closed, &self.events, msg);
    }
}

fn send(id: &str, out: &std_mpsc::Sender<Out>, closed: &Mutex<bool>, events: &Events, msg: &Value) {
    let closed = closed.lock().unwrap_or_else(|e| e.into_inner());
    if *closed || out.send(Out::Say(msg.to_string())).is_err() {
        let _ = events.unbounded_send((id.to_string(), Event::Undelivered(msg.clone())));
    }
}

impl Drop for Client {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

/// Who the link is, to the server: a stranger with a link, a paired device,
/// or a device of the server's account.
enum Who {
    Invite(Invite),
    Saved(Saved),
    Account(Saved, Account),
    /// Not dialed, for the reason given.
    Held(String),
}

/// What reaching a server through the account takes. No `Debug`: the
/// session opens the account, so it must never end up in a log.
#[derive(Clone)]
pub struct Account {
    /// The service that issued the sign-in, the only place it goes. Not the
    /// server's saved origin: `servers.json` must not decide where it goes.
    pub base: String,
    /// The app's sign-in, the bearer at the relay's door.
    pub session: String,
    /// The server's public key as the account lists it (`pub`).
    pub public: String,
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
    /// This client's name to the relay, as `phone/relay.js` has one per page.
    /// It stays the same across reconnects, so the relay replaces the socket
    /// a sleep or a network switch left behind instead of counting it as one
    /// more phone until it is full.
    page: String,
    /// A write failed and the socket may still hold that frame: it is
    /// dropped without a close or a flush, so what was told back as
    /// undelivered does not go out after all.
    torn: bool,
    /// This connection's server acknowledges each message by its `seq`
    /// (`wire::ack`). An older one does not; what went out counts as got.
    acks: bool,
    /// The last `seq` given.
    seq: u64,
    /// What went out with a `seq` and was not acknowledged yet, oldest
    /// first: the number and the message as the shell gave it.
    unacked: Vec<(u64, String)>,
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
                Who::Invite(_) => false,
                Who::Saved(saved) => saved.way != Way::Direct,
                Who::Account(..) | Who::Held(_) => true,
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

    /// `text` as it goes to the server: numbered when the server acknowledges,
    /// and then kept until it does.
    fn numbered(&mut self, text: &str) -> String {
        if !self.acks {
            return text.to_string();
        }
        let Ok(Value::Object(mut msg)) = serde_json::from_str::<Value>(text) else {
            return text.to_string();
        };
        self.seq += 1;
        msg.insert("seq".into(), self.seq.into());
        self.unacked.push((self.seq, text.to_string()));
        Value::Object(msg).to_string()
    }

    /// The server read message `seq`.
    fn acked(&mut self, seq: u64) {
        self.unacked.retain(|(s, _)| *s != seq);
    }

    /// The server that was asked is gone: what it did not acknowledge is told
    /// back, oldest first, and the next server is asked whether it acknowledges.
    fn lost_unacked(&mut self) {
        self.acks = false;
        for (_, text) in std::mem::take(&mut self.unacked) {
            self.undelivered(&text);
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
        if msg["type"] == "ack" {
            if let Some(seq) = wire::seq(&msg) {
                self.acked(seq);
            }
            return;
        }
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
        let (mut ws, acks) = match connect(&url, None) {
            Ok(ws) => ws,
            Err(ended) => return ended,
        };
        self.acks = acks;
        self.connected(wait);
        let ended = self.talk_direct(&mut ws, &unpair);
        self.hang_up(&mut ws);
        self.lost_unacked();
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
                        let said = self.numbered(&text);
                        if let Err(e) = write_alone(ws, Message::text(said)) {
                            self.torn = true;
                            // Told back with the rest, in the order they went.
                            if !self.acks {
                                self.undelivered(&text);
                            }
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
        if let Who::Held(why) = &self.who {
            return Ended::Refused(why.clone());
        }
        let Who::Account(saved, account) = &self.who else {
            // Paired through the old relay: its ticket is never sent again.
            return Ended::Refused(SIGN_IN_TO_RECONNECT.into());
        };
        let Way::Account { label } = &saved.way else {
            return Ended::Refused(SIGN_IN_TO_RECONNECT.into());
        };
        // The label is the key's own name: a key that does not give it is not
        // this server's, whoever lists it.
        let Some(public) = B64
            .decode(account.public.trim())
            .ok()
            .filter(|p| label_of(p) == *label)
        else {
            return Ended::Refused(
                "The key your account lists for this server does not match its address.".into(),
            );
        };
        let label = label.clone();
        let url = account_url(&account.base, &label, &self.page);
        let (mut ws, _) = match connect(&url, Some(&account.session)) {
            Ok(ws) => ws,
            Err(ended) => return ended,
        };
        let ended = self.talk_relay(&mut ws, &label, &public, wait);
        self.hang_up(&mut ws);
        self.lost_unacked();
        ended
    }

    /// `public` is the key the account lists for the server, which `label`
    /// is the name of.
    fn talk_relay(
        &mut self,
        ws: &mut Socket,
        label: &str,
        public: &[u8],
        wait: &mut Duration,
    ) -> Ended {
        // The relay reaps a phone that stops pinging, but it counts only its
        // own text ping; a protocol ping would leave a dead socket holding a
        // place for good.
        let mut pulse = Pulse::relay(self.timing);
        let mut channel = 0u32;
        // A hello sent and not yet answered; then the keys it agreed on.
        let mut handshake: Option<Handshake> = None;
        let mut session: Option<Session> = None;
        // The keys are agreed: messages may go.
        let mut ready = false;
        // A message too big for one relay frame comes as `{part}` pieces.
        let mut pieces = Joiner::default();
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
                                    // A server that replaced the one this was
                                    // talking to: the relay says no `pc_offline`
                                    // first, but what the old one was asked
                                    // gets no answer, so it went away all the same.
                                    if ready {
                                        self.lost_unacked();
                                        self.set_status(Status::ServerAway);
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
                                    self.lost_unacked();
                                    self.set_status(Status::ServerAway);
                                }
                                "bye" => {
                                    return relay_hung_up(
                                        msg["reason"].as_str().unwrap_or(""),
                                        msg["message"].as_str(),
                                        None,
                                    );
                                }
                                "limit" => {
                                    return held_back(msg["which"].as_str().unwrap_or(""));
                                }
                                _ => {}
                            }
                        }
                        HELLO => {
                            let Some(hs) = handshake.take() else { continue };
                            match hs.finish(payload, label, channel) {
                                // The relay let in only a device of the
                                // server's account: no more to say who this
                                // is. The computer acknowledges what it reads.
                                Ok(agreed) if hello_key(payload).as_deref() == Some(public) => {
                                    session = Some(agreed);
                                    // Pieces from before belong to no message now.
                                    pieces = Joiner::default();
                                    self.acks = true;
                                    ready = true;
                                    self.connected(wait);
                                }
                                // Whoever answered does not hold the key the account lists.
                                Ok(_) => {
                                    return Ended::Refused(
                                        "The machine at this address is not the server your account lists.".into(),
                                    );
                                }
                                Err(e) => {
                                    return Ended::Refused(format!(
                                        "The machine at this address is not the server your account lists ({e})."
                                    ));
                                }
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
                            let msg = match pieces.take(msg) {
                                Ok(Some(msg)) => msg,
                                // More pieces of it are on their way.
                                Ok(None) => continue,
                                Err(why) => return Ended::Retry(why.into()),
                            };
                            if msg.get("res").is_none() {
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
                    return relay_hung_up(&reason, None, frame.map(|f| u16::from(f.code)));
                }
                Ok(_) => pulse.heard(),
                Err(e) if would_block(&e) => {}
                Err(e) => return Ended::Retry(format!("The connection dropped: {e}")),
            }
            while let Ok(out) = self.queued.try_recv() {
                // Inside the channel, what would be an HTTP request is a `req`.
                let (text, said, last) = match out {
                    Out::Say(text) => {
                        let said = if ready { self.numbered(&text) } else { text.clone() };
                        (text, said, false)
                    }
                    Out::Unpair => (
                        String::new(),
                        json!({ "req": { "id": 1, "method": "POST", "path": "/unpair", "body": null } })
                            .to_string(),
                        true,
                    ),
                };
                // Before the server knows who this is, it would not listen.
                if let (true, Some(seal)) = (ready, session.as_mut()) {
                    let sealed = seal.seal(said.as_bytes());
                    if let Err(ended) = send_frame(ws, DATA, &sealed) {
                        self.torn = true;
                        // Numbered, it is told back with the rest.
                        if !last && !self.acks {
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
    /// Pings as the text `ping`, which the relay answers `pong` itself.
    text: bool,
    last_heard: Instant,
    last_ping: Instant,
}

impl Pulse {
    fn new(timing: Timing) -> Pulse {
        Pulse {
            timing,
            text: false,
            last_heard: Instant::now(),
            last_ping: Instant::now(),
        }
    }

    fn relay(timing: Timing) -> Pulse {
        Pulse {
            text: true,
            ..Pulse::new(timing)
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
            let ping = if self.text {
                Message::text("ping")
            } else {
                Message::Ping(Vec::new().into())
            };
            ws.send(ping)
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
    write_alone(ws, Message::Binary(frame.into()))
        .map_err(|e| Ended::Retry(format!("Could not write to the relay: {e}")))
}

/// Writes `msg` with nothing else behind it. A read can queue a pong; it
/// goes out first, on its own. Otherwise `msg` could go out whole and the
/// pong after it fail, and a message the server got would be told back
/// as undelivered. So an error here means `msg` did not fully go out.
fn write_alone<S: std::io::Read + std::io::Write>(
    ws: &mut WebSocket<S>,
    msg: Message,
) -> tungstenite::Result<()> {
    ws.flush()?;
    ws.send(msg)
}

/// What a server reached through the account says when its user removed
/// this app there.
pub const UNPAIRED_BY_SERVER: &str = "The server removed this app from its devices. It does not connect again by itself: forget it here, then connect it again from Your servers.";

/// What the relay or the server said as it hung up, in the app's terms.
fn hung_up(reason: &str, code: Option<u16>) -> Ended {
    match reason {
        "unauthorized" | "revoked" | "unpaired" => {
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
            held_back(reason.strip_prefix("limit:").unwrap_or(""))
        }
        "" => Ended::Retry("The relay closed the connection.".into()),
        other => Ended::Retry(format!("The relay hung up: {other}")),
    }
}

/// What the relay said as it turned this app away from a server of the
/// account, or let it go, with the operator's text when it came with one.
/// The rest is as from any server.
fn relay_hung_up(reason: &str, message: Option<&str>, code: Option<u16>) -> Ended {
    let message = message
        .map(|m| m.trim().chars().take(MAX_MESSAGE).collect::<String>())
        .filter(|m| !m.is_empty());
    let with = |text: &str| match &message {
        Some(m) => format!("{text}: {m}"),
        None => format!("{text}."),
    };
    match reason {
        "signed_out" => Ended::Refused(SIGNED_OUT.into()),
        "blocked" => Ended::Refused(with("The relay operator blocked this account")),
        "not_yours" => Ended::Refused("This server belongs to another account.".into()),
        "no_machine" => Ended::Refused(
            "This server was removed from your account. Sign in on it again with `arakun-server login`.".into(),
        ),
        "update" => Ended::Refused("This relay needs a newer Arakun. Update Arakun.".into()),
        // Through the account, the server's own user removed this app there:
        // dialing again would undo that.
        "unpaired" => Ended::Refused(UNPAIRED_BY_SERVER.into()),
        "disabled" => Ended::Retry(with("The relay is closed for now")),
        _ => hung_up(reason, code),
    }
}

/// The key a server's hello names, as raw bytes.
fn hello_key(reply: &[u8]) -> Option<Vec<u8>> {
    let v: Value = serde_json::from_slice(reply).ok()?;
    B64.decode(v["pub"].as_str()?).ok()
}

/// The door of the server `label` on the relay at `origin`, for a device of
/// its account. `page` stays the same across reconnects, so the relay lets a
/// new socket take the place of one a sleep left behind.
fn account_url(origin: &str, label: &str, page: &str) -> String {
    format!(
        "{}/machines/{label}/ws?page={page}",
        socket_origin(origin.trim().trim_end_matches('/'))
    )
}

/// The request that opens `url`, with the sign-in as the bearer when there
/// is one. A bearer goes in the clear only to this computer.
fn request(url: &str, bearer: Option<&str>) -> Result<Request, Ended> {
    let bad = || Ended::Refused(format!("Bad address: {url}"));
    let mut req = url.into_client_request().map_err(|_| bad())?;
    if let Some(bearer) = bearer {
        let http = format!("http{}", url.strip_prefix("ws").unwrap_or(url));
        if url.starts_with("ws://") && !crate::cloud::local_http(&http) {
            return Err(Ended::Refused(
                "The relay address must start with https:// (http:// only for this computer)."
                    .into(),
            ));
        }
        let mut value = HeaderValue::from_str(&format!("Bearer {bearer}"))
            .map_err(|_| Ended::Refused(SIGNED_OUT.into()))?;
        value.set_sensitive(true);
        req.headers_mut().insert("authorization", value);
    }
    Ok(req)
}

/// The relay closed the connection at one of its limits, named by `which`.
fn held_back(which: &str) -> Ended {
    Ended::Retry(
        match which {
            "phones" => "The relay has as many devices connected to this server as it allows.",
            "burst" => "The relay slowed this connection down: too many messages at once.",
            "daily" => "The relay has passed this server's messages for today.",
            "frame" => "The relay turned down a message that was too big.",
            _ => "The relay is holding this connection back for now.",
        }
        .into(),
    )
}

/// A small JSON answer to a request, with its status. `bearer` is the token
/// the server knows this app by, where the request needs one.
fn fetch(
    method: &str,
    url: &str,
    body: Option<String>,
    bearer: Option<&str>,
) -> Result<(u16, Value), String> {
    let agent = crate::http::agent(CONNECT_TIMEOUT);
    let reply = crate::http::json_call(agent, method, url, bearer, body, 64 * 1024)?;
    Ok((
        reply.status,
        serde_json::from_slice(&reply.bytes).unwrap_or_default(),
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

/// TCP, TLS when the address asks for it, and the WebSocket handshake, with
/// the sign-in as the bearer when there is one. The socket, and whether the
/// server acknowledges what it reads.
fn connect(url: &str, bearer: Option<&str>) -> Result<(Socket, bool), Ended> {
    let req = request(url, bearer)?;
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
    let (ws, res) = tungstenite::client_tls(req, stream).map_err(|e| match e {
        tungstenite::HandshakeError::Failure(tungstenite::Error::Http(res)) => {
            match (res.status().as_u16(), bearer.is_some()) {
                // No such door, or a page where it would be: a relay from
                // before accounts.
                (200..=399 | 404, true) => {
                    Ended::Refused("This relay does not support account sign-in.".into())
                }
                (401, true) => Ended::Refused(SIGNED_OUT.into()),
                // The door itself said no: the token is no longer good.
                (401 | 403, false) => {
                    Ended::Refused("This app is no longer paired with the server.".into())
                }
                (status, _) => Ended::Retry(format!("{host} answered {status}.")),
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
    Ok((ws, res.headers().contains_key(wire::ACKS_HEADER)))
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
            Incoming::Message(_, ClientMsg::Open { session, .. }) if session == "s1"
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

    /// Takes the first `room` bytes written to it, then fails. Reads give
    /// `incoming`, then would block.
    struct Cramped {
        incoming: std::io::Cursor<Vec<u8>>,
        written: Vec<u8>,
        room: usize,
    }

    impl std::io::Read for Cramped {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            match self.incoming.read(buf)? {
                0 => Err(std::io::ErrorKind::WouldBlock.into()),
                n => Ok(n),
            }
        }
    }

    impl std::io::Write for Cramped {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            let n = buf.len().min(self.room - self.written.len());
            if n == 0 {
                return Err(std::io::ErrorKind::BrokenPipe.into());
            }
            self.written.extend_from_slice(&buf[..n]);
            Ok(n)
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn a_message_told_back_as_undelivered_did_not_go_out() {
        const PONG: usize = 6; // Header and mask, no payload.
        let text = r#"{"type":"answer","session":"s1"}"#;
        let frame = 6 + text.len();
        // A failed write may stop anywhere, also after the message and
        // before the pong that a ping queued.
        for room in 0..=PONG + frame {
            let stream = Cramped {
                incoming: std::io::Cursor::new(vec![0x89, 0x00]),
                written: Vec::new(),
                room,
            };
            let mut ws =
                WebSocket::from_raw_socket(stream, tungstenite::protocol::Role::Client, None);
            assert!(matches!(ws.read(), Ok(Message::Ping(_))));
            let sent = write_alone(&mut ws, Message::text(text));
            let out = &ws.get_ref().written;
            let start = if out.first() == Some(&0x8a) { PONG } else { 0 };
            let whole = out.len() >= start + frame;
            assert_eq!(sent.is_ok(), whole, "room {room}");
        }
    }

    /// A server's own socket, answered by hand: says it acknowledges when
    /// `acks`, and returns what the app sent first.
    // The callback's error type is tungstenite's to choose.
    #[allow(clippy::result_large_err)]
    fn accept_direct(listener: &TcpListener, acks: bool) -> WebSocket<TcpStream> {
        let (stream, _) = listener.accept().unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(10)))
            .unwrap();
        tungstenite::accept_hdr(
            stream,
            |_: &tungstenite::handshake::server::Request,
             mut res: tungstenite::handshake::server::Response| {
                if acks {
                    res.headers_mut().insert(
                        wire::ACKS_HEADER,
                        tungstenite::http::HeaderValue::from_static("1"),
                    );
                }
                Ok(res)
            },
        )
        .unwrap()
    }

    fn next_text(ws: &mut WebSocket<TcpStream>) -> Value {
        loop {
            match ws.read().unwrap() {
                Message::Text(t) => return serde_json::from_str(t.as_str()).unwrap(),
                Message::Ping(_) | Message::Pong(_) => continue,
                other => panic!("unexpected {other:?}"),
            }
        }
    }

    fn direct_saved(listener: &TcpListener) -> Saved {
        Saved {
            id: "srv".into(),
            name: "box".into(),
            origin: format!("http://127.0.0.1:{}", listener.local_addr().unwrap().port()),
            way: Way::Direct,
            device: "d".into(),
            token: "t".into(),
        }
    }

    #[test]
    fn what_went_out_and_was_not_acknowledged_is_told_back() {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let (tx, mut rx) = mpsc::unbounded();
        let saved = direct_saved(&listener);
        let client = Client::start("srv".into(), Who::Saved(saved), String::new(), tx, QUICK);
        assert_eq!(status(&mut rx), Status::Connecting);
        let mut ws = accept_direct(&listener, true);
        assert_eq!(status(&mut rx), Status::Connected);

        let got = json!({ "type": "answer", "session": "s1", "answers": [] });
        let lost = json!({ "type": "send", "session": "s1", "text": "hi" });
        client.send(&got);
        let first = next_text(&mut ws);
        assert_eq!(first["type"], "answer");
        ws.send(Message::text(wire::ack(wire::seq(&first).unwrap())))
            .unwrap();
        client.send(&lost);
        let second = next_text(&mut ws);
        assert_eq!(second["type"], "send");
        assert!(wire::seq(&second) > wire::seq(&first));

        // The server read the second, but hangs up before it says so.
        drop(ws);
        match next(&mut rx) {
            Event::Undelivered(msg) => assert_eq!(msg, lost),
            _ => panic!("the unacknowledged message comes back before the drop"),
        }
        assert!(matches!(status(&mut rx), Status::Waiting { .. }));
    }

    #[test]
    fn a_server_that_does_not_acknowledge_gets_plain_messages() {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let (tx, mut rx) = mpsc::unbounded();
        let saved = direct_saved(&listener);
        let client = Client::start("srv".into(), Who::Saved(saved), String::new(), tx, QUICK);
        assert_eq!(status(&mut rx), Status::Connecting);
        let mut ws = accept_direct(&listener, false);
        assert_eq!(status(&mut rx), Status::Connected);
        let msg = json!({ "type": "cancel", "session": "s1" });
        client.send(&msg);
        assert_eq!(next_text(&mut ws), msg);
        // Written whole, it counts as got, as before servers acknowledged.
        drop(ws);
        assert!(matches!(status(&mut rx), Status::Waiting { .. }));
    }

    #[test]
    fn the_server_acknowledges_what_it_reads() {
        let access = Access::new(Vec::new(), String::new());
        let (server, mut incoming) = Server::start(Ipv4Addr::LOCALHOST, 0, access.clone()).unwrap();
        let code = access.lock().unwrap().begin_pairing();
        let (_, token) = access.lock().unwrap().pair(&code, "pc", 0).unwrap();
        let url = format!("ws://127.0.0.1:{}/ws?t={token}", server.port());
        let (mut ws, res) = tungstenite::connect(url).unwrap();
        assert!(res.headers().contains_key(wire::ACKS_HEADER));
        assert!(matches!(
            from_server(&mut incoming),
            Incoming::Connected(..)
        ));
        ws.send(Message::text(r#"{"type":"usage","seq":4}"#))
            .unwrap();
        ws.send(Message::text(r#"{"type":"nonsense","seq":5}"#))
            .unwrap();
        ws.send(Message::text(r#"{"type":"usage"}"#)).unwrap();
        let mut acks = Vec::new();
        let deadline = Instant::now() + Duration::from_secs(10);
        while acks.len() < 2 && Instant::now() < deadline {
            if let Ok(Message::Text(t)) = ws.read() {
                let msg: Value = serde_json::from_str(t.as_str()).unwrap();
                if msg["type"] == "ack" {
                    acks.push(msg["seq"].as_u64().unwrap());
                }
            }
        }
        // A message it could not make sense of was read all the same.
        assert_eq!(acks, [4, 5]);
    }

    #[test]
    fn forgetting_a_server_has_it_forget_this_app() {
        let access = Access::new(Vec::new(), String::new());
        let (server, mut incoming) = Server::start(Ipv4Addr::LOCALHOST, 0, access.clone()).unwrap();
        let invite = Invite {
            origin: format!("http://127.0.0.1:{}", server.port()),
            code: access.lock().unwrap().begin_pairing(),
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
                Message::Text(t) if t.as_str() == "ping" => continue,
                other => panic!("unexpected {other:?}"),
            }
        }
    }

    /// What the app asked the relay's door for.
    struct Door {
        /// The path with its query.
        target: String,
        authorization: Option<String>,
    }

    /// The relay and the server behind it, for one connection: lets the app
    /// in on `channel`, shakes hands as the server and returns the sealed
    /// session along with what the app asked the door for.
    // The callback's error type is tungstenite's to choose.
    #[allow(clippy::result_large_err)]
    fn accept_through_relay(
        listener: &TcpListener,
        identity: &Identity,
        channel: u32,
    ) -> (WebSocket<TcpStream>, Session, Door) {
        let (stream, _) = listener.accept().unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(10)))
            .unwrap();
        let mut door = Door {
            target: String::new(),
            authorization: None,
        };
        let mut ws = tungstenite::accept_hdr(
            stream,
            |req: &tungstenite::handshake::server::Request, res| {
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
        let open = json!({ "type": "open", "channel": channel }).to_string();
        ws.send(phone_frame(CONTROL, open.as_bytes())).unwrap();
        let session = answer_hello(&mut ws, identity, channel);
        (ws, session, door)
    }

    /// The server's side of the handshake, for the hello the app sends next.
    fn answer_hello(ws: &mut WebSocket<TcpStream>, identity: &Identity, channel: u32) -> Session {
        let (kind, hello) = next_frame(ws);
        assert_eq!(kind, HELLO);
        let hello = PhoneHello::parse(&hello).unwrap();
        let (reply, session) = Handshake::begin()
            .answer(&hello, identity, channel, "test")
            .unwrap();
        ws.send(phone_frame(HELLO, &reply)).unwrap();
        session
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

    fn control(ws: &mut WebSocket<TcpStream>, msg: Value) {
        ws.send(phone_frame(CONTROL, msg.to_string().as_bytes()))
            .unwrap();
    }

    /// A server of the account, as `servers.json` keeps it, and what the
    /// account says of it: its key, and the app's sign-in.
    fn through_account(listener: &TcpListener, identity: &Identity) -> (Saved, Account) {
        let origin = format!("http://127.0.0.1:{}", listener.local_addr().unwrap().port());
        let saved = Saved {
            id: "srv".into(),
            name: "build-box".into(),
            origin,
            way: Way::Account {
                label: identity.label(),
            },
            device: String::new(),
            token: String::new(),
        };
        let account = Account {
            base: saved.origin.clone(),
            session: "tok".into(),
            public: B64.encode(identity.public_raw()),
        };
        (saved, account)
    }

    fn start_account(saved: Saved, account: Account, tx: Events) -> Client {
        Client::start(
            "srv".into(),
            Who::Account(saved, account),
            String::new(),
            tx,
            QUICK,
        )
    }

    #[test]
    fn the_door_of_a_server_of_the_account_is_on_the_relay_host() {
        assert_eq!(
            account_url("https://app.arakun.dev", "abcdefghij234567", "0a1b2c3d4e5f"),
            "wss://app.arakun.dev/machines/abcdefghij234567/ws?page=0a1b2c3d4e5f"
        );
        assert_eq!(
            account_url("http://localhost:8787/", "abcdefghij234567", "0a1b2c3d"),
            "ws://localhost:8787/machines/abcdefghij234567/ws?page=0a1b2c3d"
        );
        let req = request(
            "wss://app.arakun.dev/machines/abcdefghij234567/ws?page=0a1b2c3d",
            Some("tok"),
        )
        .unwrap();
        let bearer = &req.headers()["authorization"];
        assert_eq!(bearer.to_str().unwrap(), "Bearer tok");
        assert!(bearer.is_sensitive());
        // The sign-in never crosses a network in the clear.
        assert!(matches!(
            request(
                "ws://10.0.0.2:8787/machines/x/ws?page=0a1b2c3d",
                Some("tok")
            ),
            Err(Ended::Refused(_))
        ));
        assert!(
            request(
                "ws://127.0.0.1:8787/machines/x/ws?page=0a1b2c3d",
                Some("tok")
            )
            .is_ok()
        );
        assert!(
            request("ws://10.0.0.2:7474/ws?t=k", None)
                .unwrap()
                .headers()
                .get("authorization")
                .is_none()
        );
    }

    #[test]
    fn it_reaches_a_server_of_the_account_through_the_relay() {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let identity = Identity::generate();
        let label = identity.label();
        let (saved, account) = through_account(&listener, &identity);
        let (tx, mut rx) = mpsc::unbounded();
        let client = start_account(saved, account, tx);
        assert_eq!(status(&mut rx), Status::Connecting);
        let (mut ws, mut session, door) = accept_through_relay(&listener, &identity, 7);
        let page = door
            .target
            .strip_prefix(&format!("/machines/{label}/ws?page="))
            .unwrap()
            .to_string();
        assert!(page.len() >= 8 && page.bytes().all(|b| b.is_ascii_hexdigit()));
        assert_eq!(door.authorization.as_deref(), Some("Bearer tok"));
        // The handshake is all it takes: the relay already said who this is.
        assert_eq!(status(&mut rx), Status::Connected);

        // Sealed both ways, numbered, as the computer acknowledges each.
        seal(&mut ws, &mut session, json!({ "type": "inbox" }));
        assert!(matches!(next(&mut rx), Event::Message(m) if m["type"] == "inbox"));
        let text = "é".repeat(300 * 1024);
        let big = json!({ "type": "entries", "text": text }).to_string();
        for piece in crate::phone::relay::parts(&big, 160 * 1024) {
            seal(&mut ws, &mut session, serde_json::from_str(&piece).unwrap());
        }
        assert!(
            matches!(next(&mut rx), Event::Message(m) if m["type"] == "entries" && m["text"] == text)
        );
        let got = json!({ "type": "cancel", "session": "s1" });
        let lost = json!({ "type": "skip_question", "session": "s1" });
        client.send(&got);
        let said = sealed(&mut ws, &mut session);
        assert_eq!(said["type"], "cancel");
        assert!(said["seq"].is_u64());
        seal(
            &mut ws,
            &mut session,
            json!({ "type": "ack", "seq": said["seq"] }),
        );
        client.send(&lost);
        assert_eq!(sealed(&mut ws, &mut session)["type"], "skip_question");

        // The server goes away and comes back: a new hello, and in again.
        control(&mut ws, json!({ "type": "pc_offline" }));
        assert!(matches!(next(&mut rx), Event::Undelivered(m) if m == lost));
        assert_eq!(status(&mut rx), Status::ServerAway);
        control(&mut ws, json!({ "type": "pc_online" }));
        let mut session = answer_hello(&mut ws, &identity, 7);
        assert_eq!(status(&mut rx), Status::Connected);
        client.send(&got);
        assert_eq!(sealed(&mut ws, &mut session)["type"], "cancel");

        // The relay reaps a device by its text ping only.
        let ping = loop {
            match ws.read().unwrap() {
                Message::Text(t) => break t,
                Message::Pong(_) | Message::Ping(_) => continue,
                other => panic!("unexpected {other:?}"),
            }
        };
        assert_eq!(ping.as_str(), "ping");

        // The connection drops without a word: what was not acknowledged is
        // told back, and the next one is the same page, so the relay can let
        // it take the old one's place.
        drop(ws);
        assert!(matches!(next(&mut rx), Event::Undelivered(m) if m == got));
        let (mut ws, _session, again) = accept_through_relay(&listener, &identity, 8);
        assert_eq!(again.target, door.target);
        assert_eq!(again.authorization.as_deref(), Some("Bearer tok"));

        // The relay's recheck finds the sign-in gone: nothing to come back for.
        control(&mut ws, json!({ "type": "bye", "reason": "signed_out" }));
        loop {
            match status(&mut rx) {
                Status::Refused(why) => {
                    assert_eq!(why, SIGNED_OUT);
                    break;
                }
                Status::Connected | Status::Connecting | Status::Waiting { .. } => {}
                other => panic!("{other:?}"),
            }
        }
    }

    #[test]
    fn the_sign_in_goes_to_the_service_that_issued_it_whatever_the_file_says() {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let identity = Identity::generate();
        let (mut saved, account) = through_account(&listener, &identity);
        // Someone wrote another address in `servers.json`.
        let elsewhere = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        saved.origin = format!(
            "http://127.0.0.1:{}",
            elsewhere.local_addr().unwrap().port()
        );
        let (tx, mut rx) = mpsc::unbounded();
        let _client = start_account(saved, account, tx);
        assert_eq!(status(&mut rx), Status::Connecting);
        let (_ws, _session, door) = accept_through_relay(&listener, &identity, 3);
        assert_eq!(door.authorization.as_deref(), Some("Bearer tok"));
        elsewhere.set_nonblocking(true).unwrap();
        assert!(
            elsewhere.accept().is_err(),
            "nothing went to the file's address"
        );
    }

    #[test]
    fn a_key_the_account_names_must_be_the_one_the_label_stands_for() {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let identity = Identity::generate();
        let (saved, mut account) = through_account(&listener, &identity);
        account.public = B64.encode(Identity::generate().public_raw());
        let (tx, mut rx) = mpsc::unbounded();
        let _client = start_account(saved, account, tx);
        assert_eq!(status(&mut rx), Status::Connecting);
        assert!(matches!(status(&mut rx), Status::Refused(why) if why.contains("does not match")));
        // Nothing was dialed, so the sign-in went nowhere.
        listener.set_nonblocking(true).unwrap();
        assert!(listener.accept().is_err());
    }

    #[test]
    fn a_server_with_another_key_is_refused() {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let listed = Identity::generate();
        let impostor = Identity::generate();
        let (saved, account) = through_account(&listener, &listed);
        let (tx, mut rx) = mpsc::unbounded();
        let _client = start_account(saved, account, tx);
        assert_eq!(status(&mut rx), Status::Connecting);
        let (_ws, _session, _) = accept_through_relay(&listener, &impostor, 3);
        assert!(matches!(status(&mut rx), Status::Refused(why) if why.contains("not the server")));
    }

    #[test]
    fn a_server_paired_through_the_old_relay_waits_for_a_sign_in() {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let origin = format!("http://127.0.0.1:{}", listener.local_addr().unwrap().port());
        let saved = Saved {
            id: "srv".into(),
            name: "build-box".into(),
            origin,
            way: Way::Relay {
                label: Identity::generate().label(),
                ticket: "t".into(),
            },
            device: "dev1".into(),
            token: "tok".into(),
        };
        let (tx, mut rx) = mpsc::unbounded();
        let _client = Client::connect(saved, tx);
        assert_eq!(status(&mut rx), Status::Connecting);
        assert_eq!(
            status(&mut rx),
            Status::Refused(SIGN_IN_TO_RECONNECT.into())
        );
        // Its ticket and token go nowhere.
        listener.set_nonblocking(true).unwrap();
        assert!(listener.accept().is_err());
    }

    #[test]
    fn each_refusal_of_the_relay_says_what_to_do() {
        let refused =
            |reason: &str, message: Option<&str>| match relay_hung_up(reason, message, None) {
                Ended::Refused(why) => why,
                other => panic!("{reason}: {other:?}"),
            };
        assert_eq!(refused("signed_out", None), SIGNED_OUT);
        assert_eq!(
            refused("blocked", Some(" Too many frames. ")),
            "The relay operator blocked this account: Too many frames."
        );
        assert_eq!(
            refused("blocked", None),
            "The relay operator blocked this account."
        );
        assert_eq!(
            refused("not_yours", None),
            "This server belongs to another account."
        );
        assert_eq!(
            refused("no_machine", None),
            "This server was removed from your account. Sign in on it again with `arakun-server login`."
        );
        assert_eq!(
            refused("update", None),
            "This relay needs a newer Arakun. Update Arakun."
        );
        // The server's user removed this app: no "paired" words, no retry.
        assert_eq!(refused("unpaired", None), UNPAIRED_BY_SERVER);
        // A server reached directly still says the pairing is gone.
        assert!(matches!(
            hung_up("unpaired", None),
            Ended::Refused(why) if why.contains("no longer paired")
        ));
        // Words of the pairing through the relay that no longer exists.
        for gone in ["bad_ticket", "wrong_secret"] {
            assert!(matches!(hung_up(gone, None), Ended::Retry(_)), "{gone}");
        }
        assert!(matches!(
            relay_hung_up("disabled", Some("Back at noon."), Some(4503)),
            Ended::Retry(why) if why == "The relay is closed for now: Back at noon."
        ));
        // A close with the reason and no `bye` reads the same.
        assert!(matches!(
            relay_hung_up("not_yours", None, Some(4403)),
            Ended::Refused(why) if why.contains("another account")
        ));
        assert!(matches!(
            relay_hung_up("limit:phones", None, Some(4429)),
            Ended::Retry(why) if why.contains("devices")
        ));
        assert!(matches!(relay_hung_up("", None, None), Ended::Retry(_)));
    }

    #[test]
    fn what_ends_a_connection_decides_whether_another_follows() {
        assert!(matches!(hung_up("revoked", None), Ended::Refused(_)));
        assert!(matches!(hung_up("bad_code", None), Ended::Refused(_)));
        assert!(matches!(hung_up("", Some(4401)), Ended::Refused(_)));
        assert!(
            matches!(hung_up("limit:phones", Some(4429)), Ended::Retry(why) if why.contains("devices"))
        );
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
}
