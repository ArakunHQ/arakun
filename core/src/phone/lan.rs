//! The sealed WebSocket a phone (or the app dialing an `arakun server`) uses
//! on this computer's own server, over the network or Tailscale. It speaks the
//! relay's end-to-end protocol (`e2e`), only without the relay: frames are a
//! type byte and the payload, and the handshake is signed for channel
//! [`CHANNEL`]. The phone checks the hello is signed by the key whose label the
//! pairing link carried (`?k=`), or, paired by a typed code, by the key it met
//! then. So someone on the same Wi-Fi reads nothing, and the token never
//! crosses the network in the clear.
//!
//! The first sealed message says who the phone is: `{pair: {code, name}}`
//! trades a pairing code for a token, `{auth: {token}}` brings the token it
//! has. The computer answers `{type: "paired", token, id, name, host}`,
//! `{type: "welcome", id, host}`, or `{type: "refused", reason}` and closes.
//! From then on it is as through the relay: the messages of a socket of its
//! own (`ClientMsg`), and `{req: {id, method, path, body}}` for what would be
//! HTTP (`route`), answered with `{res: {id, status, type, body, more}}`.

use std::io::{Read, Write};
use std::net::TcpStream;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, mpsc as std_mpsc};
use std::time::{Duration, Instant};

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD as B64;
use futures::channel::mpsc;
use serde_json::{Value, json};
use tungstenite::{Message, WebSocket};

use super::e2e::{Handshake, PhoneHello, Session};
use super::identity::{Identity, label_of};
use super::quiet::Quiet;
use super::relay::{DATA, HELLO};
use super::{
    Client, ClientId, ClientMsg, Clients, Door, Effect, Incoming, PHONE_PING_EVERY, PHONE_QUEUE,
    PHONE_SEND_PASS, PHONE_SILENCE_MAX, PHONE_WRITE_TIMEOUT, Reply, Shared, host_name, wire,
};

/// Where the sealed socket is.
pub const PATH: &str = "/e2e";
/// The channel the handshake signs here. The relay numbers its channels from
/// 1, so a hello signed on this server never passes for one of the relay's.
pub const CHANNEL: u32 = 0;
/// How long the hello and the first sealed message may take, together.
const GREET_WAIT: Duration = Duration::from_secs(15);
/// How long one read waits, so what the shell queued goes out in between.
const READ_TICK: Duration = Duration::from_millis(20);

/// Serves one phone on the sealed socket until it leaves or `ended` says so.
pub(super) fn serve(
    stream: TcpStream,
    id: ClientId,
    identity: &Identity,
    access: &Shared,
    clients: &Clients,
    tx: &mpsc::UnboundedSender<Incoming>,
    ended: impl Fn() -> bool,
) {
    let _ = stream.set_read_timeout(Some(READ_TICK));
    let _ = stream.set_write_timeout(Some(PHONE_WRITE_TIMEOUT));
    let mut ws = match tungstenite::accept(stream) {
        Ok(ws) => ws,
        Err(e) => {
            eprintln!("arakun: phone handshake: {e}");
            return;
        }
    };
    let Some((mut session, device)) = admit(&mut ws, identity, access, tx, &ended) else {
        let _ = ws.close(None);
        let _ = ws.flush();
        return;
    };
    let (out_tx, out_rx) = std_mpsc::sync_channel(PHONE_QUEUE);
    let gone = Arc::new(AtomicBool::new(false));
    clients.lock().unwrap().insert(
        id,
        Client {
            device: device.clone(),
            out: out_tx,
            gone: gone.clone(),
        },
    );
    // Unpairing forgets the device, then drops its clients. Checked after
    // this one is listed, a device unpaired while it shook hands is not let in.
    let paired = access
        .lock()
        .unwrap()
        .devices
        .iter()
        .any(|d| d.id == device && Door::Local.admits(d));
    if !paired
        || tx
            .unbounded_send(Incoming::Connected(id, device.clone()))
            .is_err()
    {
        clients.lock().unwrap().remove(&id);
        let _ = ws.close(None);
        let _ = ws.flush();
        return;
    }
    Phone {
        ws: &mut ws,
        session: &mut session,
        id,
        device: &device,
        access,
        clients,
        tx,
    }
    .talk(out_rx, || ended() || gone.load(Ordering::Relaxed));
    clients.lock().unwrap().remove(&id);
    let _ = tx.unbounded_send(Incoming::Disconnected(id));
}

/// The handshake, then the first sealed message: a pairing or a token. The
/// session and the device when the phone may stay; it was told why not otherwise.
fn admit<S: Read + Write>(
    ws: &mut WebSocket<S>,
    identity: &Identity,
    access: &Shared,
    tx: &mpsc::UnboundedSender<Incoming>,
    ended: &impl Fn() -> bool,
) -> Option<(Session, String)> {
    let deadline = Instant::now() + GREET_WAIT;
    let (kind, hello) = next_frame(ws, deadline, ended).ok()?;
    if kind != HELLO {
        return None;
    }
    let hello = PhoneHello::parse(&hello).ok()?;
    let (reply, mut session) = Handshake::begin()
        .answer(&hello, identity, CHANNEL, &host_name())
        .ok()?;
    send(ws, HELLO, &reply).ok()?;
    let (kind, first) = next_frame(ws, deadline, ended).ok()?;
    if kind != DATA {
        return None;
    }
    let msg: Value = serde_json::from_slice(&session.open(&first).ok()?).ok()?;
    let now = crate::model::unix_now();
    let refused = |reason: &str| (json!({ "type": "refused", "reason": reason }), None);
    let (answer, device) = if let Some(pair) = msg.get("pair") {
        let code = pair["code"].as_str().unwrap_or_default();
        let name = pair["name"].as_str().unwrap_or_default();
        let paired = access.lock().unwrap().pair(code, name, now);
        match paired {
            Some((d, token)) => {
                let _ = tx.unbounded_send(Incoming::Paired);
                (
                    json!({ "type": "paired", "token": token, "id": d.id, "name": d.name, "host": host_name() }),
                    Some(d.id),
                )
            }
            None => refused("code"),
        }
    } else if let Some(token) = msg.get("auth").and_then(|a| a["token"].as_str()) {
        let known = access.lock().unwrap().device_for(token, now);
        match known {
            Some(d) => (
                json!({ "type": "welcome", "id": d.id, "host": host_name() }),
                Some(d.id),
            ),
            None => refused("unpaired"),
        }
    } else {
        refused("bad")
    };
    let sealed = session.seal(answer.to_string().as_bytes());
    send(ws, DATA, &sealed).ok()?;
    let _ = ws.flush();
    Some((session, device?))
}

/// A phone the computer let in.
struct Phone<'a, S: Read + Write> {
    ws: &'a mut WebSocket<S>,
    session: &'a mut Session,
    id: ClientId,
    device: &'a str,
    access: &'a Shared,
    clients: &'a Clients,
    tx: &'a mpsc::UnboundedSender<Incoming>,
}

impl<S: Read + Write> Phone<'_, S> {
    /// Reads and writes until the phone leaves or `ended` says so. Reads time
    /// out often so writes queued by the shell go out in between.
    fn talk(&mut self, out: std_mpsc::Receiver<String>, ended: impl Fn() -> bool) {
        let mut last_heard = Quiet::now();
        let mut last_ping = Quiet::now();
        loop {
            if ended() {
                let _ = self.ws.close(None);
                let _ = self.ws.flush();
                return;
            }
            // Judged before reading: input buffered while the computer slept
            // would otherwise pass for a phone that is still there.
            // Browsers answer pings themselves, even for a page in the background.
            if last_heard.elapsed() > PHONE_SILENCE_MAX {
                return;
            }
            let read = self.ws.read();
            if read.is_ok() {
                // Judged again: the computer can sleep while `read` waits, and
                // what it returns on waking may have sat in a buffer all along.
                if last_heard.elapsed() > PHONE_SILENCE_MAX {
                    return;
                }
                last_heard = Quiet::now();
            }
            match read {
                Ok(Message::Binary(bytes)) => {
                    // Only sealed messages from here on; anything else, or one
                    // that does not open as the next, is a trick or a fault.
                    let plain = match bytes.split_first() {
                        Some((&kind, payload)) if kind == DATA => self.session.open(payload),
                        _ => return,
                    };
                    let Ok(plain) = plain else {
                        return;
                    };
                    if self.heard(&plain).is_err() {
                        return;
                    }
                }
                Ok(Message::Close(_)) => {
                    // tungstenite queued the answering close frame.
                    let _ = self.ws.flush();
                    return;
                }
                Ok(_) => {}
                Err(e) if would_block(&e) => {}
                Err(_) => return,
            }
            if last_ping.elapsed() > PHONE_PING_EVERY {
                last_ping = Quiet::now();
                if self.ws.send(Message::Ping(Vec::new().into())).is_err() {
                    return;
                }
            }
            let pass_ends = Instant::now() + PHONE_SEND_PASS;
            loop {
                if ended() || Instant::now() > pass_ends {
                    break;
                }
                match out.try_recv() {
                    Ok(text) => {
                        if self.say(&text).is_err() {
                            return;
                        }
                    }
                    Err(std_mpsc::TryRecvError::Empty) => break,
                    Err(std_mpsc::TryRecvError::Disconnected) => {
                        let _ = self.ws.close(None);
                        let _ = self.ws.flush();
                        return;
                    }
                }
            }
            // Pongs tungstenite queued while reading.
            let _ = self.ws.flush();
        }
    }

    /// One message from the phone. An error ends the connection: a write
    /// failed, or the phone let go of this computer.
    fn heard(&mut self, plain: &[u8]) -> Result<(), ()> {
        let Ok(msg) = serde_json::from_slice::<Value>(plain) else {
            return self.say(&wire::error("bad message: not json"));
        };
        if let Some(req) = msg.get("req") {
            let now = crate::model::unix_now();
            let (reply, effect) =
                super::sealed_request(req, self.device, Door::Local, self.access, now);
            self.respond(&req["id"], reply)?;
            return self.apply(effect);
        }
        let seq = wire::seq(&msg);
        match serde_json::from_value::<ClientMsg>(msg) {
            // The page's own ping: it cannot see ours, so it hears it is still here.
            Ok(ClientMsg::Ping) => self.say(&wire::pong())?,
            Ok(msg) => {
                self.tx
                    .unbounded_send(Incoming::Message(self.id, msg))
                    .map_err(drop)?;
            }
            Err(e) => self.say(&wire::error(&format!("bad message: {e}")))?,
        }
        // A bad message was read all the same: sending it again would not help.
        if let Some(seq) = seq {
            self.say(&wire::ack(seq))?;
        }
        Ok(())
    }

    /// What a `req` changed, acted on.
    fn apply(&mut self, effect: Effect) -> Result<(), ()> {
        match effect {
            Effect::None => {}
            Effect::Paired => {
                let _ = self.tx.unbounded_send(Incoming::Paired);
            }
            Effect::Subscribed => {
                let _ = self.tx.unbounded_send(Incoming::Subscribed);
            }
            // Its token died: this socket and every other of the device close.
            Effect::Unpaired(device) => {
                self.clients
                    .lock()
                    .unwrap()
                    .retain(|_, c| c.device != device);
                let _ = self.tx.unbounded_send(Incoming::Unpaired);
                let _ = self.ws.close(None);
                let _ = self.ws.flush();
                return Err(());
            }
        }
        Ok(())
    }

    /// A `req`'s answer, in one piece: nothing between here and the phone
    /// limits a frame as the relay does.
    fn respond(&mut self, id: &Value, reply: Reply) -> Result<(), ()> {
        let res = json!({
            "id": id,
            "status": reply.status,
            "type": reply.content_type,
            "body": B64.encode(&reply.body),
            "more": false,
        });
        self.say(&json!({ "res": res }).to_string())
    }

    fn say(&mut self, text: &str) -> Result<(), ()> {
        let sealed = self.session.seal(text.as_bytes());
        send(self.ws, DATA, &sealed).map_err(drop)
    }
}

/// Why dialing a sealed socket did not get in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GreetError {
    /// The other end does not hold the key it must, or broke the protocol.
    Untrusted(String),
    /// The connection failed or went quiet; another try may do.
    Dropped(String),
}

/// What dialing a sealed socket got.
pub struct Greeted {
    pub session: Session,
    /// The label of the key that signed the handshake.
    pub label: String,
    /// The computer's answer to the greeting: `paired`, `welcome` or `refused`.
    pub answer: Value,
}

/// The dialing side, on a socket open at [`PATH`]: the handshake, checked
/// against `label`, then `greeting` (`{pair}` or `{auth}`) sealed, and the
/// computer's answer. With no label yet (pairing by a typed code), the key
/// that signs the hello is the one from then on, and its label comes back.
pub fn greet<S: Read + Write>(
    ws: &mut WebSocket<S>,
    label: Option<&str>,
    greeting: &Value,
    wait: Duration,
) -> Result<Greeted, GreetError> {
    let deadline = Instant::now() + wait;
    let never = || false;
    let dropped = |e: String| GreetError::Dropped(e);
    let handshake = Handshake::begin();
    send(ws, HELLO, &handshake.hello()).map_err(|e| dropped(e.to_string()))?;
    let (kind, reply) = next_frame(ws, deadline, &never).map_err(dropped)?;
    if kind != HELLO {
        return Err(GreetError::Untrusted("no hello".into()));
    }
    let label = match label {
        Some(label) => label.to_string(),
        None => hello_label(&reply).ok_or(GreetError::Untrusted("bad hello: pub".into()))?,
    };
    let mut session = handshake
        .finish(&reply, &label, CHANNEL)
        .map_err(|e| GreetError::Untrusted(e.to_string()))?;
    let sealed = session.seal(greeting.to_string().as_bytes());
    send(ws, DATA, &sealed).map_err(|e| dropped(e.to_string()))?;
    let (kind, answer) = next_frame(ws, deadline, &never).map_err(dropped)?;
    if kind != DATA {
        return Err(GreetError::Untrusted("no answer".into()));
    }
    let answer = session
        .open(&answer)
        .ok()
        .and_then(|plain| serde_json::from_slice(&plain).ok())
        .ok_or_else(|| GreetError::Untrusted("the answer does not open".into()))?;
    Ok(Greeted {
        session,
        label,
        answer,
    })
}

/// The label of the key a computer's hello names.
fn hello_label(reply: &[u8]) -> Option<String> {
    let v: Value = serde_json::from_slice(reply).ok()?;
    let public = B64.decode(v["pub"].as_str()?).ok()?;
    Some(label_of(&public))
}

/// A frame: the type byte, then the payload.
pub fn frame(kind: u8, payload: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(1 + payload.len());
    out.push(kind);
    out.extend_from_slice(payload);
    out
}

fn send<S: Read + Write>(
    ws: &mut WebSocket<S>,
    kind: u8,
    payload: &[u8],
) -> tungstenite::Result<()> {
    ws.send(Message::Binary(frame(kind, payload).into()))
}

/// The next frame, skipping pings and text, before `deadline`.
fn next_frame<S: Read + Write>(
    ws: &mut WebSocket<S>,
    deadline: Instant,
    ended: &impl Fn() -> bool,
) -> Result<(u8, Vec<u8>), String> {
    loop {
        if ended() {
            return Err("stopped".into());
        }
        if Instant::now() > deadline {
            return Err("no answer in time".into());
        }
        match ws.read() {
            Ok(Message::Binary(bytes)) => {
                if let Some((&kind, payload)) = bytes.split_first() {
                    return Ok((kind, payload.to_vec()));
                }
            }
            Ok(Message::Close(_)) => {
                let _ = ws.flush();
                return Err("closed".into());
            }
            Ok(_) => {}
            Err(e) if would_block(&e) => {}
            Err(e) => return Err(e.to_string()),
        }
    }
}

fn would_block(e: &tungstenite::Error) -> bool {
    matches!(e, tungstenite::Error::Io(e) if matches!(
        e.kind(),
        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
    ))
}
