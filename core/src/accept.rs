//! The accept loop the local servers share: one thread per connection, with a
//! cap on how many are open at once, and a pause when `accept` keeps failing.

use std::collections::HashMap;
use std::io;
use std::net::{IpAddr, TcpStream};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// The pause after `accept` or a thread spawn fails for a reason that does not
/// pass at once, such as running out of file descriptors. Retrying at once
/// would keep a core busy until it does.
const BACKOFF: Duration = Duration::from_millis(100);

/// How many connections may be open at once.
#[derive(Clone, Copy)]
pub struct Limits {
    pub total: usize,
    /// From one address. Loopback and local sockets count only toward `total`:
    /// `tailscale serve` brings every tailnet phone in from the loopback.
    pub per_peer: usize,
}

/// Where a connection comes from, when that is worth a limit of its own.
pub trait Peer {
    fn peer(&self) -> Option<IpAddr>;
}

impl Peer for TcpStream {
    fn peer(&self) -> Option<IpAddr> {
        self.peer_addr()
            .ok()
            .map(|a| a.ip())
            .filter(|ip| !ip.is_loopback())
    }
}

#[cfg(unix)]
impl Peer for std::os::unix::net::UnixStream {
    fn peer(&self) -> Option<IpAddr> {
        None
    }
}

#[cfg(windows)]
impl Peer for uds_windows::UnixStream {
    fn peer(&self) -> Option<IpAddr> {
        None
    }
}

/// Accepts until `incoming` ends or `stop` says so. `job` makes the work for
/// each connection on the accept thread; the work runs on a thread of its own
/// named `name`. A connection over the limits is closed at once.
pub fn serve<S, F>(
    incoming: impl IntoIterator<Item = io::Result<S>>,
    limits: Limits,
    name: &str,
    mut stop: impl FnMut() -> bool,
    mut job: impl FnMut(S) -> F,
) where
    S: Peer,
    F: FnOnce() + Send + 'static,
{
    let open = Arc::new(Mutex::new(Open::default()));
    // Said once per run of failures, not once per retry.
    let mut failing = false;
    for conn in incoming {
        if stop() {
            break;
        }
        let stream = match conn {
            Ok(stream) => stream,
            Err(e) if passes(&e) => continue,
            Err(e) => {
                if !std::mem::replace(&mut failing, true) {
                    eprintln!("arakun: {name}: accept: {e}");
                }
                std::thread::sleep(BACKOFF);
                continue;
            }
        };
        let Some(slot) = Slot::take(&open, stream.peer(), limits) else {
            continue;
        };
        let work = job(stream);
        let spawned = std::thread::Builder::new()
            .name(name.into())
            .spawn(move || {
                let _slot = slot;
                work();
            });
        match spawned {
            Ok(_) => failing = false,
            // The work and its stream were dropped with the closure, which closes it.
            Err(e) => {
                if !std::mem::replace(&mut failing, true) {
                    eprintln!("arakun: {name}: no thread for a connection: {e}");
                }
                std::thread::sleep(BACKOFF);
            }
        }
    }
}

/// Errors about one connection only: the next accept is fine.
fn passes(e: &io::Error) -> bool {
    matches!(
        e.kind(),
        io::ErrorKind::Interrupted
            | io::ErrorKind::ConnectionAborted
            | io::ErrorKind::ConnectionReset
    )
}

#[derive(Default)]
struct Open {
    total: usize,
    by_peer: HashMap<IpAddr, usize>,
}

/// One open connection's place under the limits, given back on drop.
struct Slot {
    open: Arc<Mutex<Open>>,
    peer: Option<IpAddr>,
}

impl Slot {
    fn take(open: &Arc<Mutex<Open>>, peer: Option<IpAddr>, limits: Limits) -> Option<Slot> {
        let mut o = open.lock().unwrap();
        if o.total >= limits.total {
            return None;
        }
        if let Some(ip) = peer {
            let n = o.by_peer.entry(ip).or_default();
            if *n >= limits.per_peer {
                return None;
            }
            *n += 1;
        }
        o.total += 1;
        Some(Slot {
            open: open.clone(),
            peer,
        })
    }
}

impl Drop for Slot {
    fn drop(&mut self) {
        let mut o = self.open.lock().unwrap();
        o.total -= 1;
        if let Some(ip) = self.peer
            && let Some(n) = o.by_peer.get_mut(&ip)
        {
            *n -= 1;
            if *n == 0 {
                o.by_peer.remove(&ip);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;
    use std::time::Instant;

    struct Fake(Option<IpAddr>, u32);

    impl Peer for Fake {
        fn peer(&self) -> Option<IpAddr> {
            self.0
        }
    }

    /// Runs `serve` over `conns` with work that holds its slot until the end,
    /// and gives back which connections got to run.
    fn served(conns: Vec<io::Result<Fake>>, limits: Limits) -> Vec<u32> {
        let (started, ran) = mpsc::channel();
        let mut release = Vec::new();
        serve(
            conns,
            limits,
            "test",
            || false,
            |Fake(_, id)| {
                let (go, wait) = mpsc::channel::<()>();
                release.push(go);
                let started = started.clone();
                move || {
                    started.send(id).unwrap();
                    let _ = wait.recv();
                }
            },
        );
        drop(started);
        drop(release);
        let mut ids: Vec<u32> = ran.iter().collect();
        ids.sort();
        ids
    }

    #[test]
    fn closes_connections_past_the_total() {
        let conns = (1..=4).map(|id| Ok(Fake(None, id))).collect();
        let limits = Limits {
            total: 2,
            per_peer: 2,
        };
        assert_eq!(served(conns, limits), [1, 2]);
    }

    #[test]
    fn one_address_cannot_take_every_place() {
        let a = Some(IpAddr::from([192, 168, 1, 9]));
        let b = Some(IpAddr::from([192, 168, 1, 10]));
        let conns = vec![
            Ok(Fake(a, 1)),
            Ok(Fake(a, 2)),
            Ok(Fake(a, 3)),
            Ok(Fake(None, 4)),
            Ok(Fake(b, 5)),
        ];
        let limits = Limits {
            total: 10,
            per_peer: 2,
        };
        assert_eq!(served(conns, limits), [1, 2, 4, 5]);
    }

    #[test]
    fn a_place_comes_back_when_its_connection_ends() {
        let open = Arc::new(Mutex::new(Open::default()));
        let ip = Some(IpAddr::from([10, 0, 0, 1]));
        let limits = Limits {
            total: 1,
            per_peer: 1,
        };
        let slot = Slot::take(&open, ip, limits).unwrap();
        assert!(Slot::take(&open, None, limits).is_none());
        drop(slot);
        assert!(Slot::take(&open, ip, limits).is_some());
        assert!(open.lock().unwrap().by_peer.is_empty());
    }

    #[test]
    fn waits_after_a_failed_accept_and_goes_on() {
        let conns = vec![
            Err(io::Error::other("too many open files")),
            Err(io::ErrorKind::ConnectionAborted.into()),
            Ok(Fake(None, 1)),
        ];
        let limits = Limits {
            total: 1,
            per_peer: 1,
        };
        let start = Instant::now();
        assert_eq!(served(conns, limits), [1]);
        // One pause, for the failure that does not pass on its own.
        let took = start.elapsed();
        assert!(took >= BACKOFF, "{took:?}");
    }
}
