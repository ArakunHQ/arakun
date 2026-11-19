//! How `arakun server <command>` reaches the server running on the same
//! machine: a Unix socket only its user can open, with one JSON line each way,
//! like the app's control socket. Whoever can open it is the user, so what
//! comes through it needs no pairing. Windows 10 and later have the same
//! sockets; `uds_windows` exposes them, as std only does on Unix.

use std::io::{BufRead as _, BufReader, Read as _, Write as _};
#[cfg(unix)]
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::time::Duration;

use futures::channel::{mpsc, oneshot};
use serde::{Deserialize, Serialize};
#[cfg(windows)]
use uds_windows::{UnixListener, UnixStream};

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct Request {
    pub cmd: String,
    #[serde(default)]
    pub args: Vec<String>,
}

impl Request {
    pub fn new(cmd: &str, args: &[&str]) -> Self {
        Request {
            cmd: cmd.into(),
            args: args.iter().map(|a| a.to_string()).collect(),
        }
    }
}

#[derive(Serialize, Deserialize, Debug, PartialEq)]
pub struct Reply {
    pub ok: bool,
    #[serde(default)]
    pub message: String,
}

impl Reply {
    pub fn ok(message: impl Into<String>) -> Self {
        Reply {
            ok: true,
            message: message.into(),
        }
    }

    pub fn err(message: impl Into<String>) -> Self {
        Reply {
            ok: false,
            message: message.into(),
        }
    }
}

impl From<Result<String, String>> for Reply {
    fn from(r: Result<String, String>) -> Self {
        match r {
            Ok(message) => Reply::ok(message),
            Err(message) => Reply::err(message),
        }
    }
}

pub type Incoming = (Request, oneshot::Sender<Reply>);

/// How long a request waits for the server's answer, unless told less.
pub const REPLY_WAIT: Duration = Duration::from_secs(30);

/// The socket while the server listens on it. Dropping it removes the file.
pub struct Listening {
    path: PathBuf,
}

impl Drop for Listening {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

/// Commands from the user's own processes; each is one line and its reply.
const CONNECTIONS: crate::accept::Limits = crate::accept::Limits {
    total: 64,
    per_peer: 64,
};

/// Listens on `path`; requests come out of the receiver, each with where its
/// reply goes. Fails when another server already answers there.
pub fn serve(path: &Path) -> Result<(Listening, mpsc::UnboundedReceiver<Incoming>), String> {
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let listener = match UnixListener::bind(path) {
        Ok(l) => l,
        Err(e) if e.kind() == std::io::ErrorKind::AddrInUse => {
            if UnixStream::connect(path).is_ok() {
                return Err(format!(
                    "another arakun server answers on {}",
                    path.display()
                ));
            }
            // Left behind by a crash.
            let _ = std::fs::remove_file(path);
            UnixListener::bind(path).map_err(|e| format!("{}: {e}", path.display()))?
        }
        Err(e) => return Err(format!("{}: {e}", path.display())),
    };
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
            .map_err(|e| format!("{}: {e}", path.display()))?;
    }
    let (tx, rx) = mpsc::unbounded();
    std::thread::Builder::new()
        .name("arakun-server-local".into())
        .spawn(move || {
            crate::accept::serve(
                listener.incoming(),
                CONNECTIONS,
                "arakun-server-local-conn",
                || false,
                |stream| {
                    let tx = tx.clone();
                    move || answer(stream, tx)
                },
            );
        })
        .map_err(|e| e.to_string())?;
    Ok((
        Listening {
            path: path.to_path_buf(),
        },
        rx,
    ))
}

fn answer(stream: UnixStream, tx: mpsc::UnboundedSender<Incoming>) {
    let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
    let mut line = String::new();
    let read = BufReader::new((&stream).take(1 << 20)).read_line(&mut line);
    let reply = match read.map_err(|e| e.to_string()).and_then(|_| {
        serde_json::from_str::<Request>(&line).map_err(|e| format!("bad request: {e}"))
    }) {
        Ok(req) => {
            let (done, wait) = oneshot::channel();
            if tx.unbounded_send((req, done)).is_err() {
                Reply::err("the server is shutting down")
            } else {
                futures::executor::block_on(wait)
                    .unwrap_or_else(|_| Reply::err("the server did not answer"))
            }
        }
        Err(e) => Reply::err(e),
    };
    let mut stream = stream;
    let _ = writeln!(
        stream,
        "{}",
        serde_json::to_string(&reply).unwrap_or_default()
    );
}

/// Asks the server listening on `path`, waiting at most `wait` for its answer.
pub fn send(path: &Path, req: &Request, wait: Duration) -> std::io::Result<Reply> {
    let mut stream = UnixStream::connect(path)?;
    // A zero timeout means none to the socket.
    stream.set_read_timeout(Some(wait.max(Duration::from_millis(1))))?;
    writeln!(stream, "{}", serde_json::to_string(req).unwrap_or_default())?;
    let mut line = String::new();
    BufReader::new(&stream).read_line(&mut line)?;
    serde_json::from_str(&line).map_err(std::io::Error::other)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn socket(name: &str) -> PathBuf {
        // Short: a socket path has room for about a hundred bytes.
        std::env::temp_dir().join(format!("arakun-{name}-{}.sock", std::process::id()))
    }

    #[test]
    fn a_request_gets_the_reply_made_for_it() {
        let path = socket("local");
        let _ = std::fs::remove_file(&path);
        let (listening, mut rx) = serve(&path).unwrap();
        let server = std::thread::spawn(move || {
            let (req, done) =
                futures::executor::block_on(futures::StreamExt::next(&mut rx)).unwrap();
            let _ = done.send(Reply::ok(format!("{} {}", req.cmd, req.args.join(","))));
        });
        let reply = send(&path, &Request::new("log", &["s1", "30"]), REPLY_WAIT).unwrap();
        assert_eq!(reply, Reply::ok("log s1,30"));
        server.join().unwrap();

        // A second server finds the first one answering and leaves its socket alone.
        let second = serve(&path).err().unwrap();
        assert!(second.contains("another arakun server"), "{second}");
        assert!(path.exists());

        drop(listening);
        assert!(!path.exists());
        assert!(send(&path, &Request::new("ls", &[]), REPLY_WAIT).is_err());
    }

    #[test]
    fn a_server_that_does_not_answer_is_waited_for_only_so_long() {
        let path = socket("slow");
        let _ = std::fs::remove_file(&path);
        // Requests are taken and never answered.
        let (_listening, _rx) = serve(&path).unwrap();
        let started = std::time::Instant::now();
        let reply = send(
            &path,
            &Request::new("state", &["s1"]),
            Duration::from_millis(150),
        );
        assert!(reply.is_err());
        assert!(started.elapsed() < Duration::from_secs(5));
    }

    #[test]
    fn a_socket_left_by_a_crash_is_taken_over() {
        let path = socket("stale");
        let _ = std::fs::remove_file(&path);
        // Bound and dropped without the guard: the file stays, nobody answers.
        drop(UnixListener::bind(&path).unwrap());
        assert!(path.exists());
        // On macOS a socket is made close-on-exec only after it exists, so a
        // process another test starts in between keeps it open and answering
        // until it exits. Wait for that, or `serve` sees a live server.
        let start = std::time::Instant::now();
        while UnixStream::connect(&path).is_ok() {
            assert!(
                start.elapsed() < Duration::from_secs(10),
                "something still answers on the stale socket"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
        let (listening, _rx) = serve(&path).unwrap();
        drop(listening);
    }

    #[test]
    fn a_line_that_is_no_request_is_refused() {
        let path = socket("bad");
        let _ = std::fs::remove_file(&path);
        let (_listening, _rx) = serve(&path).unwrap();
        let mut stream = UnixStream::connect(&path).unwrap();
        writeln!(stream, "not json").unwrap();
        let mut line = String::new();
        BufReader::new(&stream).read_line(&mut line).unwrap();
        let reply: Reply = serde_json::from_str(&line).unwrap();
        assert!(!reply.ok && reply.message.starts_with("bad request"));
    }
}
