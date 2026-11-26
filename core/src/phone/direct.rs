//! A phone's socket on this computer's own server, `/e2e`, on the network or
//! through Tailscale. It speaks the relay's end-to-end protocol (`e2e`) with
//! nobody in between: the phone sends its hello, the computer answers signed
//! with its identity, and every frame after that is sealed. The phone pins the
//! label of that key when it pairs (the QR link carries it, `super::link`),
//! or takes it from the first handshake when the code was typed. So someone
//! who listens on the network reads neither the token nor the transcripts.
//!
//! Frames are binary WebSocket messages: a type byte (`relay::HELLO` or
//! `relay::DATA`), then the payload, as a phone sends them to the relay. The
//! signed channel is `e2e::DIRECT`. The first sealed message says who the
//! phone is: `{auth: {token}}` for a paired phone, `{pair: {code, name}}` for
//! a new one. The computer answers `{authed: {id}}` or
//! `{paired: {id, name, token, host}}`, or `{refused: {reason}}` (`unpaired`,
//! `bad_code`, `bad_request`) and hangs up. From then on the phone speaks
//! `ClientMsg` JSON, plus `{req}` for what would be an HTTP request
//! (`super::asked`), answered with one `{res}`, as through the relay.

use std::net::TcpStream;
use std::sync::mpsc as std_mpsc;
use std::time::{Duration, Instant};

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD as B64;
use futures::channel::mpsc;
use serde_json::{Value, json};
use tungstenite::{Message, WebSocket};

use super::e2e::{DIRECT, Handshake, PhoneHello, Session};
use super::identity::Identity;
use super::quiet::Quiet;
use super::relay::{DATA, HELLO};
use super::{
    ClientId, ClientMsg, Clients, Device, Door, Effect, Incoming, PHONE_PING_EVERY,
    PHONE_SEND_PASS, PHONE_SILENCE_MAX, Shared, asked, host_name, wire,
};

/// How long the hello and the message that says who the phone is may take,
/// together.
const GREET_FOR: Duration = Duration::from_secs(15);
/// How long a read waits once the phone is in, before the queue is looked at.
const READ_TICK: Duration = Duration::from_millis(20);

/// A phone that got through the handshake and said who it is.
pub(super) struct Greeted {
    pub session: Session,
    pub device: Device,
    /// It brought a pairing code and is new.
    pub paired: bool,
}

/// The handshake, then who the phone is. `None` when it does not keep to the
/// protocol, or is not let in: then it was told why, if the keys were agreed.
pub(super) fn greet(
    ws: &mut WebSocket<TcpStream>,
    identity: &Identity,
    access: &Shared,
    now: u64,
) -> Option<Greeted> {
    let deadline = Instant::now() + GREET_FOR;
    let hello = PhoneHello::parse(&read_frame(ws, HELLO, deadline)?).ok()?;
    let (reply, mut session) = Handshake::begin()
        .answer(&hello, identity, DIRECT, &host_name())
        .ok()?;
    send_frame(ws, HELLO, &reply).ok()?;
    let first = read_frame(ws, DATA, deadline)?;
    let msg: Value = session
        .open(&first)
        .ok()
        .and_then(|plain| serde_json::from_slice(&plain).ok())?;
    match who(&msg, access, now) {
        Ok((device, answer, paired)) => {
            say(ws, &mut session, &answer.to_string()).ok()?;
            Some(Greeted {
                session,
                device,
                paired,
            })
        }
        Err(reason) => {
            let refused = json!({ "refused": { "reason": reason } });
            let _ = say(ws, &mut session, &refused.to_string());
            None
        }
    }
}

/// Who the first message says the phone is: the device, the answer, and
/// whether it was paired just now. Or why it is not let in.
fn who(msg: &Value, access: &Shared, now: u64) -> Result<(Device, Value, bool), &'static str> {
    if let Some(token) = msg["auth"]["token"].as_str() {
        let device = access
            .lock()
            .unwrap()
            .device_for(token, now)
            .ok_or("unpaired")?;
        let answer = json!({ "authed": { "id": device.id } });
        return Ok((device, answer, false));
    }
    if let Some(code) = msg["pair"]["code"].as_str() {
        let name = msg["pair"]["name"].as_str().unwrap_or_default();
        let (device, token) = access
            .lock()
            .unwrap()
            .pair(code, name, now)
            .ok_or("bad_code")?;
        let answer = json!({ "paired": {
            "id": device.id, "name": device.name, "token": token, "host": host_name(),
        } });
        return Ok((device, answer, true));
    }
    Err("bad_request")
}

/// The next frame, which must be of `kind`; `None` when another comes, or
/// none before the deadline. The deadline is for all of it: a timeout per
/// read would let a peer hold the thread with a byte now and then.
fn read_frame(ws: &mut WebSocket<TcpStream>, kind: u8, deadline: Instant) -> Option<Vec<u8>> {
    loop {
        let left = deadline.checked_duration_since(Instant::now())?;
        // A zero timeout means none to the socket.
        ws.get_ref()
            .set_read_timeout(Some(left.max(Duration::from_millis(1))))
            .ok()?;
        match ws.read() {
            Ok(Message::Binary(bytes)) => {
                let (&k, payload) = bytes.split_first()?;
                return (k == kind).then(|| payload.to_vec());
            }
            Ok(Message::Ping(_) | Message::Pong(_)) => {}
            Ok(_) => return None,
            Err(e) if would_block(&e) => {}
            Err(_) => return None,
        }
    }
}

fn send_frame(ws: &mut WebSocket<TcpStream>, kind: u8, payload: &[u8]) -> tungstenite::Result<()> {
    let mut frame = Vec::with_capacity(1 + payload.len());
    frame.push(kind);
    frame.extend_from_slice(payload);
    ws.send(Message::Binary(frame.into()))
}

/// A message to the phone, sealed.
fn say(
    ws: &mut WebSocket<TcpStream>,
    session: &mut Session,
    text: &str,
) -> tungstenite::Result<()> {
    send_frame(ws, DATA, &session.seal(text.as_bytes()))
}

fn would_block(e: &tungstenite::Error) -> bool {
    matches!(e, tungstenite::Error::Io(e) if matches!(
        e.kind(),
        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
    ))
}

/// What the connection's thread needs besides the socket.
pub(super) struct Phone<'a> {
    pub id: ClientId,
    pub device: String,
    pub out: std_mpsc::Receiver<String>,
    pub tx: &'a mpsc::UnboundedSender<Incoming>,
    pub access: &'a Shared,
    pub clients: &'a Clients,
}

/// Reads and writes one greeted phone until it leaves or the server stops.
/// Reads time out often so writes queued by the shell go out in between. A
/// frame that does not open ends it: a replay, or someone tampering.
pub(super) fn talk(
    mut ws: WebSocket<TcpStream>,
    mut session: Session,
    phone: Phone,
    ended: impl Fn() -> bool,
) {
    let _ = ws.get_ref().set_read_timeout(Some(READ_TICK));
    let mut last_heard = Quiet::now();
    let mut last_ping = Quiet::now();
    loop {
        if ended() {
            let _ = ws.close(None);
            let _ = ws.flush();
            return;
        }
        // Judged before reading: input buffered while the computer slept
        // would otherwise pass for a phone that is still there.
        // Browsers answer pings themselves, even for a page in the background.
        if last_heard.elapsed() > PHONE_SILENCE_MAX {
            return;
        }
        let read = ws.read();
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
                let Some((&DATA, payload)) = bytes.split_first() else {
                    return;
                };
                let Ok(plain) = session.open(payload) else {
                    return;
                };
                if heard(&mut ws, &mut session, &phone, &plain).is_err() {
                    return;
                }
            }
            Ok(Message::Close(_)) => {
                // tungstenite queued the answering close frame.
                let _ = ws.flush();
                return;
            }
            // Text is not part of this protocol.
            Ok(Message::Text(_)) => return,
            Ok(_) => {}
            Err(e) if would_block(&e) => {}
            Err(_) => return,
        }
        if last_ping.elapsed() > PHONE_PING_EVERY {
            last_ping = Quiet::now();
            if ws.send(Message::Ping(Vec::new().into())).is_err() {
                return;
            }
        }
        let pass_ends = Instant::now() + PHONE_SEND_PASS;
        loop {
            if ended() || Instant::now() > pass_ends {
                break;
            }
            match phone.out.try_recv() {
                Ok(text) => {
                    if say(&mut ws, &mut session, &text).is_err() {
                        return;
                    }
                }
                Err(std_mpsc::TryRecvError::Empty) => break,
                Err(std_mpsc::TryRecvError::Disconnected) => {
                    let _ = ws.close(None);
                    let _ = ws.flush();
                    return;
                }
            }
        }
        // Pongs tungstenite queued while reading.
        let _ = ws.flush();
    }
}

/// One message from the phone, opened.
fn heard(
    ws: &mut WebSocket<TcpStream>,
    session: &mut Session,
    phone: &Phone,
    plain: &[u8],
) -> tungstenite::Result<()> {
    let msg = match serde_json::from_slice::<Value>(plain) {
        Ok(msg) => msg,
        Err(_) => return say(ws, session, &wire::error("bad message: not json")),
    };
    if let Some(req) = msg.get("req") {
        let now = crate::model::unix_now();
        let (reply, effect) = asked(req, &phone.device, Door::Local, phone.access, now);
        let res = json!({ "res": {
            "id": req["id"], "status": reply.status, "type": reply.content_type,
            "body": B64.encode(&reply.body), "more": false,
        } });
        say(ws, session, &res.to_string())?;
        match effect {
            Effect::None => {}
            Effect::Subscribed => {
                let _ = phone.tx.unbounded_send(Incoming::Subscribed);
            }
            // Dropping its clients ends their loops, this one's too, after
            // the answer above went out.
            Effect::Unpaired(device) => {
                phone
                    .clients
                    .lock()
                    .unwrap()
                    .retain(|_, c| c.device != device);
                let _ = phone.tx.unbounded_send(Incoming::Unpaired);
            }
        }
        return Ok(());
    }
    let seq = wire::seq(&msg);
    match serde_json::from_value::<ClientMsg>(msg) {
        // The page's own ping: it cannot see ours, so it hears it is still here.
        Ok(ClientMsg::Ping) => say(ws, session, &wire::pong())?,
        Ok(msg) => {
            if phone
                .tx
                .unbounded_send(Incoming::Message(phone.id, msg))
                .is_err()
            {
                return Err(tungstenite::Error::ConnectionClosed);
            }
        }
        Err(e) => say(ws, session, &wire::error(&format!("bad message: {e}")))?,
    }
    // A bad message was read all the same: sending it again would not help.
    if let Some(seq) = seq {
        say(ws, session, &wire::ack(seq))?;
    }
    Ok(())
}
