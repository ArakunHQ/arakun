//! A channel's live socket, kept up from a thread of its own: it dials the
//! channel's WebSocket with the session and the device, hands every frame to
//! the app, and dials again when the connection drops, waiting longer each
//! time. It gives up only when the server says the user is not a member.
//!
//! Frames are what PROTOCOL.md says: `send` out, `message` and `error` in.
//! Nothing is replayed after a drop; the app reads the history it missed
//! when the socket is back (`Event::Status(Status::Connected)`).

use std::net::{TcpStream, ToSocketAddrs as _};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, mpsc as std_mpsc};
use std::time::{Duration, Instant};

use futures::channel::mpsc;
use tungstenite::client::IntoClientRequest as _;
use tungstenite::{Message, WebSocket, stream::MaybeTlsStream};

use super::api::DEVICE_HEADER;

const RETRY_MIN: Duration = Duration::from_secs(1);
const RETRY_MAX: Duration = Duration::from_secs(60);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
/// How long a read waits before the loop looks at what there is to send.
const READ_TICK: Duration = Duration::from_millis(20);
const WAIT_TICK: Duration = Duration::from_millis(100);
const PING: Duration = Duration::from_secs(30);
const SILENCE: Duration = Duration::from_secs(90);
/// The server closes with this when the user is not (any more) a member.
const CLOSE_FORBIDDEN: u16 = 4403;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Status {
    Connecting,
    Connected,
    /// Dropped for now; dialing again after the wait.
    Waiting(String),
    /// The server will not have this user on the channel.
    Refused(String),
}

pub enum Event {
    Status(Status),
    /// A frame from the server, as JSON.
    Frame(serde_json::Value),
}

/// Events come with the key the app knows the channel by.
pub type Events = mpsc::UnboundedSender<(String, Event)>;

/// The connection. Dropping it hangs up.
pub struct Link {
    out: std_mpsc::Sender<String>,
    stop: Arc<AtomicBool>,
}

pub struct Dial {
    pub url: String,
    pub session: String,
    pub device: String,
}

impl Link {
    pub fn open(key: String, dial: Dial, events: Events) -> Link {
        let (out, queued) = std_mpsc::channel();
        let stop = Arc::new(AtomicBool::new(false));
        let runner = Runner {
            key,
            dial,
            queued,
            events,
            stop: stop.clone(),
            status: None,
        };
        let _ = std::thread::Builder::new()
            .name("arakun-teams-channel".into())
            .spawn(move || runner.run());
        Link { out, stop }
    }

    /// A frame to the server. Dropped while there is no connection: the app
    /// sends again what the server did not acknowledge once it is back.
    pub fn send(&self, frame: String) {
        let _ = self.out.send(frame);
    }
}

impl Drop for Link {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

enum Ended {
    Retry(String),
    Refused(String),
    Stopped,
}

type Socket = WebSocket<MaybeTlsStream<TcpStream>>;

struct Runner {
    key: String,
    dial: Dial,
    queued: std_mpsc::Receiver<String>,
    events: Events,
    stop: Arc<AtomicBool>,
    status: Option<Status>,
}

impl Runner {
    fn run(mut self) {
        let mut wait = RETRY_MIN;
        loop {
            if self.stop.load(Ordering::Relaxed) {
                return;
            }
            self.set_status(Status::Connecting);
            let ended = match connect(&self.dial) {
                Ok(mut ws) => {
                    wait = RETRY_MIN;
                    while self.queued.try_recv().is_ok() {}
                    self.set_status(Status::Connected);
                    let ended = self.talk(&mut ws);
                    let _ = ws.close(None);
                    let _ = ws.flush();
                    ended
                }
                Err(ended) => ended,
            };
            match ended {
                Ended::Stopped => return,
                Ended::Refused(why) => {
                    self.set_status(Status::Refused(why));
                    return;
                }
                Ended::Retry(why) => {
                    self.set_status(Status::Waiting(why));
                    let until = Instant::now() + wait;
                    while Instant::now() < until {
                        if self.stop.load(Ordering::Relaxed) {
                            return;
                        }
                        while self.queued.try_recv().is_ok() {}
                        std::thread::sleep(WAIT_TICK);
                    }
                    wait = (wait * 2).min(RETRY_MAX);
                }
            }
        }
    }

    fn tell(&self, event: Event) {
        let _ = self.events.unbounded_send((self.key.clone(), event));
    }

    fn set_status(&mut self, status: Status) {
        if self.status.as_ref() != Some(&status) {
            self.status = Some(status.clone());
            self.tell(Event::Status(status));
        }
    }

    fn talk(&mut self, ws: &mut Socket) -> Ended {
        let mut last_heard = Instant::now();
        let mut last_ping = Instant::now();
        loop {
            if self.stop.load(Ordering::Relaxed) {
                return Ended::Stopped;
            }
            match ws.read() {
                Ok(Message::Text(text)) => {
                    last_heard = Instant::now();
                    if let Ok(v) = serde_json::from_str(text.as_str()) {
                        self.tell(Event::Frame(v));
                    }
                }
                Ok(Message::Close(frame)) => {
                    let _ = ws.flush();
                    return match frame {
                        Some(f) if u16::from(f.code) == CLOSE_FORBIDDEN => {
                            Ended::Refused("You are no longer a member of this team.".into())
                        }
                        _ => Ended::Retry("The server closed the connection.".into()),
                    };
                }
                Ok(_) => last_heard = Instant::now(),
                Err(e) if would_block(&e) => {}
                Err(e) => return Ended::Retry(format!("The connection dropped: {e}")),
            }
            while let Ok(frame) = self.queued.try_recv() {
                if let Err(e) = ws.send(Message::text(frame)) {
                    return Ended::Retry(format!("Could not write to the server: {e}"));
                }
            }
            if last_heard.elapsed() >= SILENCE {
                return Ended::Retry("The server went quiet.".into());
            }
            if last_ping.elapsed() >= PING {
                last_ping = Instant::now();
                if let Err(e) = ws.send(Message::Ping(Vec::new().into())) {
                    return Ended::Retry(format!("Could not write to the server: {e}"));
                }
            }
        }
    }
}

fn would_block(e: &tungstenite::Error) -> bool {
    matches!(e, tungstenite::Error::Io(e) if matches!(
        e.kind(),
        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
    ))
}

/// TCP, TLS for `wss://`, and the WebSocket handshake with the session and
/// the device in its headers.
fn connect(dial: &Dial) -> Result<Socket, Ended> {
    let (host, port) =
        host_port(&dial.url).ok_or_else(|| Ended::Refused(format!("Bad address: {}", dial.url)))?;
    let mut request = dial
        .url
        .as_str()
        .into_client_request()
        .map_err(|e| Ended::Refused(format!("Bad address: {e}")))?;
    let headers = request.headers_mut();
    for (name, value) in [
        ("Authorization", format!("Bearer {}", dial.session)),
        (DEVICE_HEADER, dial.device.clone()),
    ] {
        let value = value
            .parse()
            .map_err(|_| Ended::Refused("The session can't go in a header.".into()))?;
        headers.insert(name, value);
    }
    let addrs = (host.as_str(), port)
        .to_socket_addrs()
        .map_err(|e| Ended::Retry(format!("Could not resolve {host}: {e}")))?;
    let stream = addrs
        .into_iter()
        .find_map(|addr| TcpStream::connect_timeout(&addr, CONNECT_TIMEOUT).ok())
        .ok_or_else(|| Ended::Retry(format!("Could not reach {host}:{port}.")))?;
    let _ = stream.set_nodelay(true);
    let _ = stream.set_read_timeout(Some(CONNECT_TIMEOUT));
    let _ = stream.set_write_timeout(Some(CONNECT_TIMEOUT));
    let (ws, _) = tungstenite::client_tls(request, stream).map_err(|e| match e {
        tungstenite::HandshakeError::Failure(tungstenite::Error::Http(res)) => {
            match res.status().as_u16() {
                // Not a member, or the device is not the user's: asking
                // again would not change that.
                401 => Ended::Refused("The Teams session ended. Sign in again.".into()),
                403 | 404 => Ended::Refused("This channel is not open to you.".into()),
                status => Ended::Retry(format!("The server answered {status}.")),
            }
        }
        tungstenite::HandshakeError::Failure(e) => Ended::Retry(format!("Could not connect: {e}")),
        tungstenite::HandshakeError::Interrupted(_) => {
            Ended::Retry("The handshake was cut short.".into())
        }
    })?;
    match ws.get_ref() {
        MaybeTlsStream::Plain(s) => {
            let _ = s.set_read_timeout(Some(READ_TICK));
        }
        MaybeTlsStream::Rustls(s) => {
            let _ = s.get_ref().set_read_timeout(Some(READ_TICK));
        }
        _ => {}
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
    let authority = rest.split(['/', '?', '#']).next()?;
    if let Some(v6) = authority.strip_prefix('[') {
        let (host, after) = v6.split_once(']')?;
        let port = match after.strip_prefix(':') {
            Some(p) => p.parse().ok()?,
            None => default,
        };
        return Some((host.to_string(), port));
    }
    match authority.rsplit_once(':') {
        Some((host, port)) => Some((host.to_string(), port.parse().ok()?)),
        None => Some((authority.to_string(), default)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_host_and_port() {
        assert_eq!(
            host_port("wss://teams.arakun.dev/teams/A/channels/B/ws"),
            Some(("teams.arakun.dev".into(), 443))
        );
        assert_eq!(
            host_port("ws://localhost:8787/teams/A/channels/B/ws"),
            Some(("localhost".into(), 8787))
        );
        assert_eq!(host_port("ws://[::1]:8787/x"), Some(("::1".into(), 8787)));
        assert_eq!(host_port("https://teams.arakun.dev"), None);
    }
}
