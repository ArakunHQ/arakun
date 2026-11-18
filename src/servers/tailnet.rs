//! Finding servers without a link: the machines of this tailnet that run
//! `arakun server serve --reach tailscale` answer at their tailnet address
//! (`phone::TAILNET_HELLO`), and let a machine of the same tailnet user pair
//! there with nothing to copy. Tailscale is asked who is in the tailnet
//! through its command line; nothing is sent to a machine outside it.

use std::net::{Ipv4Addr, SocketAddr, TcpStream};
use std::time::Duration;

use crate::phone::{DEFAULT_PORT, TAILNET_HELLO, tailscale};

/// How long a machine has to answer. In a tailnet a server answers well
/// within this; a machine that runs none mostly refuses at once.
const ANSWER_WITHIN: Duration = Duration::from_millis(1500);
/// The most machines asked at once: a tailnet can hold thousands.
const ASK_AT_MOST: usize = 64;

/// A server found in the tailnet.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Found {
    /// What the machine calls itself.
    pub name: String,
    /// Where the app reaches it: `http://<tailnet address>:<port>`.
    pub origin: String,
    /// Whether it is this user's machine. Another user's, or a tagged one,
    /// turns a pairing without a link away.
    pub own: bool,
}

/// The origin of the server at a tailnet address.
pub fn origin(ip: Ipv4Addr) -> String {
    format!("http://{ip}:{DEFAULT_PORT}")
}

/// Asks every machine that is online in the tailnet whether it is a server.
/// Runs a process and waits on the network: call it off the main thread.
pub fn find() -> Result<Vec<Found>, String> {
    let net = tailscale::tailnet()?;
    let this = net.this;
    let asked: Vec<_> = net
        .peers
        .into_iter()
        .take(ASK_AT_MOST)
        .map(|peer| std::thread::spawn(move || hello(peer.ip).map(|name| (peer, name))))
        .collect();
    Ok(asked
        .into_iter()
        .filter_map(|thread| thread.join().ok().flatten())
        .map(|(peer, name)| Found {
            name: if name.is_empty() { peer.name } else { name },
            origin: origin(peer.ip),
            own: !this.tagged && !peer.tagged && peer.user == this.user,
        })
        .collect())
}

/// The name of the server at `ip`, when one answers there.
fn hello(ip: Ipv4Addr) -> Option<String> {
    let addr = SocketAddr::from((ip, DEFAULT_PORT));
    // Dialed first with a short wait: most machines run no server, and the
    // request below would wait much longer on one that drops the packets.
    TcpStream::connect_timeout(&addr, ANSWER_WITHIN).ok()?;
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(ANSWER_WITHIN))
        .http_status_as_error(false)
        .user_agent("arakun")
        .build()
        .into();
    let mut response = agent
        .get(&format!("{}{TAILNET_HELLO}", origin(ip)))
        .call()
        .ok()?;
    if response.status().as_u16() != 200 {
        return None;
    }
    let text = response
        .body_mut()
        .with_config()
        .limit(4096)
        .read_to_string()
        .ok()?;
    let said: serde_json::Value = serde_json::from_str(&text).ok()?;
    (said["arakun"] == "server").then(|| said["host"].as_str().unwrap_or("").to_string())
}
