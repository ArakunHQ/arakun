//! The app's connection to the island: a thread that keeps it up, sends what
//! the shell hands it and passes the island's presses back. It reconnects when
//! the island restarts, with the newest feed, and starts the island when none
//! answers: at most every `RESTART_EVERY`, since the island quits when its
//! monitor goes away (screen off at night) and nothing else brings it back.

use std::io::{BufRead as _, BufReader, Write as _};
use std::net::Shutdown;
#[cfg(unix)]
use std::os::unix::net::UnixStream;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::time::{Duration, Instant};
#[cfg(windows)]
use uds_windows::UnixStream;

use futures::channel::mpsc as fmpsc;

use super::proto::{self, FromIsland, ToIsland};

const FIRST_TRY: Duration = Duration::from_millis(250);
const MOST_WAIT: Duration = Duration::from_secs(5);
const RESTART_EVERY: Duration = Duration::from_secs(20);

enum Cmd {
    Send(ToIsland),
    PeerClosed,
    Disconnect,
}

#[cfg(test)]
pub(crate) struct TestReceiver(mpsc::Receiver<Cmd>);

#[cfg(test)]
impl TestReceiver {
    pub(crate) fn try_recv(&self) -> Result<ToIsland, mpsc::TryRecvError> {
        loop {
            match self.0.try_recv()? {
                Cmd::Send(m) => return Ok(m),
                Cmd::PeerClosed | Cmd::Disconnect => {}
            }
        }
    }
}

#[cfg(test)]
pub(crate) fn test_channel() -> (Sender, TestReceiver) {
    let (tx, rx) = mpsc::channel();
    (Sender::new(tx), TestReceiver(rx))
}

struct SenderInner(mpsc::Sender<Cmd>);

impl Drop for SenderInner {
    fn drop(&mut self) {
        let _ = self.0.send(Cmd::Disconnect);
    }
}

/// Sender handle to the island link thread; dropping the last clone closes the link.
#[derive(Clone)]
pub struct Sender(Arc<SenderInner>);

impl Sender {
    fn new(tx: mpsc::Sender<Cmd>) -> Self {
        Self(Arc::new(SenderInner(tx)))
    }

    pub fn send(&self, msg: ToIsland) -> bool {
        self.0.0.send(Cmd::Send(msg)).is_ok()
    }
}

/// Connects in the background; dropping the sender closes the connection. The
/// flag says whether the island is connected right now: until it is, only the
/// newest feed is kept and anything else sent is dropped.
pub fn connect(
    start: bool,
) -> (
    Sender,
    fmpsc::UnboundedReceiver<FromIsland>,
    Arc<AtomicBool>,
) {
    let (tx, rx) = mpsc::channel();
    let (events, presses) = fmpsc::unbounded();
    let up = Arc::new(AtomicBool::new(false));
    let flag = up.clone();
    let notify = tx.clone();
    let spawned = std::thread::Builder::new()
        .name("arakun-island-link".into())
        .spawn(move || run(rx, notify, events, &flag, start));
    if let Err(e) = spawned {
        eprintln!("arakun: no island link: {e}");
    }
    (Sender::new(tx), presses, up)
}

fn run(
    rx: mpsc::Receiver<Cmd>,
    notify: mpsc::Sender<Cmd>,
    events: fmpsc::UnboundedSender<FromIsland>,
    up: &Arc<AtomicBool>,
    start: bool,
) {
    let path = proto::socket_path();
    let mut feed: Option<ToIsland> = None;
    let mut wait = FIRST_TRY;
    let mut started: Option<Instant> = None;
    loop {
        match UnixStream::connect(&path) {
            Ok(stream) => {
                wait = FIRST_TRY;
                let alive = session(stream, &rx, &notify, &events, up, &mut feed);
                up.store(false, Ordering::Relaxed);
                if !alive {
                    return;
                }
            }
            Err(_) if start && started.is_none_or(|t| t.elapsed() >= RESTART_EVERY) => {
                started = Some(Instant::now());
                if let Err(e) = start_island() {
                    eprintln!("arakun: could not start the island: {e}");
                }
            }
            Err(_) => {}
        }
        // Meanwhile only the newest feed matters; alerts would be stale by then.
        let until = Instant::now() + wait;
        loop {
            match rx.recv_timeout(until.saturating_duration_since(Instant::now())) {
                Ok(Cmd::Send(msg @ ToIsland::Feed { .. })) => feed = Some(msg),
                Ok(Cmd::Send(_) | Cmd::PeerClosed) => {}
                Ok(Cmd::Disconnect) | Err(RecvTimeoutError::Disconnected) => return,
                Err(RecvTimeoutError::Timeout) => break,
            }
        }
        wait = (wait * 2).min(MOST_WAIT);
    }
}

/// One connection, until it drops (`true`) or the shell lets go of the link (`false`).
fn session(
    stream: UnixStream,
    rx: &mpsc::Receiver<Cmd>,
    notify: &mpsc::Sender<Cmd>,
    events: &fmpsc::UnboundedSender<FromIsland>,
    up: &Arc<AtomicBool>,
    feed: &mut Option<ToIsland>,
) -> bool {
    let mut out = &stream;
    let hello = ToIsland::Hello {
        pid: std::process::id(),
    };
    if std::iter::once(&hello)
        .chain(feed.as_ref())
        .any(|m| out.write_all(proto::encode(m).as_bytes()).is_err())
    {
        return true;
    }
    let gone = Arc::new(AtomicBool::new(false));
    let reader = stream.try_clone().map(|s| {
        let (gone, up, events, notify) = (gone.clone(), up.clone(), events.clone(), notify.clone());
        std::thread::spawn(move || {
            for line in BufReader::new(s).lines() {
                let Ok(line) = line else { break };
                match proto::decode::<FromIsland>(&line) {
                    Ok(ev) => {
                        let _ = events.unbounded_send(ev);
                    }
                    Err(e) => eprintln!("arakun: island: {e}"),
                }
            }
            up.store(false, Ordering::Relaxed);
            gone.store(true, Ordering::Relaxed);
            let _ = notify.send(Cmd::PeerClosed);
        })
    });
    if reader.is_err() {
        return true;
    }
    up.store(true, Ordering::Relaxed);
    let alive = loop {
        if gone.load(Ordering::Relaxed) {
            break true;
        }
        match rx.recv() {
            Ok(Cmd::Send(msg)) => {
                let sent = out.write_all(proto::encode(&msg).as_bytes()).is_ok();
                if matches!(msg, ToIsland::Feed { .. }) {
                    *feed = Some(msg);
                }
                if !sent {
                    break true;
                }
            }
            Ok(Cmd::PeerClosed) => {
                if gone.load(Ordering::Relaxed) {
                    break true;
                }
            }
            Ok(Cmd::Disconnect) | Err(_) => break false,
        }
    };
    let _ = stream.shutdown(Shutdown::Both);
    alive
}

/// After an update is installed on Windows. The island outlives the app, so it
/// would go on with the old version, and keep its .exe in use. Asks it to quit
/// and starts the new one: Windows still names the running app by the path it
/// started from, where the new .exe now is. Does nothing when no island runs.
#[cfg(windows)]
pub fn restart_after_update() {
    if proto::request(&ToIsland::Quit).is_err() {
        return;
    }
    // A new host that finds the old one still answering would quit at once.
    let path = proto::socket_path();
    let until = Instant::now() + Duration::from_secs(2);
    while UnixStream::connect(&path).is_ok() {
        if Instant::now() > until {
            eprintln!("arakun: island: the old island did not quit; it stays until it does");
            return;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    if let Err(e) = start_island() {
        eprintln!("arakun: island: could not start the new version: {e}");
    }
}

/// `arakun island-host`, in a session of its own so it outlives the app.
fn start_island() -> std::io::Result<()> {
    let exe = std::env::current_exe()?;
    let exe = exe.to_string_lossy();
    let exe = exe.strip_suffix(" (deleted)").unwrap_or(&exe);
    let dir = crate::control::runtime_dir();
    let _ = std::fs::create_dir_all(&dir);
    let log = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.join("island.log"))?;
    let mut cmd = std::process::Command::new(exe);
    cmd.arg("island-host")
        .stdin(std::process::Stdio::null())
        .stdout(log.try_clone()?)
        .stderr(log);
    let mut child = crate::platform::detach(&mut cmd).spawn()?;
    std::thread::spawn(move || child.wait());
    Ok(())
}
