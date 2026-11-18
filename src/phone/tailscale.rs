//! Tailscale as the way to the phone: when the computer and the phone are in
//! the same tailnet, `tailscale serve` takes HTTPS at the machine's name with
//! a certificate browsers already trust, and hands the bytes to the phone
//! server on the loopback. The phone then reaches arakun from anywhere, over
//! an encrypted link, and gets a secure origin (what a service worker and
//! push notifications need) without a certificate authority of ours on it.
//!
//! Everything here goes through the `tailscale` command line, which talks to
//! the daemon the user already runs; nothing is linked in.

use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};

use serde::Deserialize;

/// The port `serve` takes HTTPS on: the default, so the address carries none.
const HTTPS_PORT: u16 = 443;
/// Where to turn HTTPS certificates on for the tailnet.
pub const ADMIN_DNS_URL: &str = "https://login.tailscale.com/admin/dns";

/// What the daemon says about this machine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Status {
    /// The command line is not here (or not where arakun looks).
    Missing,
    /// Installed but not connected to the tailnet: stopped, logged out, starting.
    Down,
    /// Connected, but the tailnet does not issue HTTPS certificates.
    NoHttps,
    /// Connected and able to serve `https://{name}/`.
    Ready { name: String },
}

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct StatusJson {
    backend_state: String,
    #[serde(rename = "Self")]
    this: Option<Node>,
    #[serde(default)]
    cert_domains: Option<Vec<String>>,
    #[serde(default)]
    peer: Option<HashMap<String, Node>>,
}

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct Node {
    #[serde(rename = "DNSName", default)]
    dns_name: String,
    #[serde(default)]
    host_name: String,
    #[serde(rename = "UserID", default)]
    user_id: u64,
    #[serde(rename = "TailscaleIPs", default)]
    ips: Option<Vec<IpAddr>>,
    #[serde(default)]
    tags: Option<Vec<String>>,
    #[serde(default)]
    online: bool,
}

impl Node {
    fn machine(&self) -> Option<Machine> {
        let ip = self.ips.iter().flatten().find_map(|ip| match ip {
            IpAddr::V4(v4) => Some(*v4),
            IpAddr::V6(_) => None,
        })?;
        // The first label of the tailnet name is what the admin console shows.
        let name = match self.dns_name.split('.').next() {
            Some(label) if !label.is_empty() => label.to_string(),
            _ => self.host_name.clone(),
        };
        Some(Machine {
            name,
            ip,
            user: self.user_id,
            tagged: self.tags.as_ref().is_some_and(|t| !t.is_empty()),
        })
    }
}

/// A machine in the tailnet: this one, or a peer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Machine {
    pub name: String,
    /// Its address in the tailnet, which stays the same for as long as it is
    /// in it.
    pub ip: Ipv4Addr,
    /// The tailnet user it belongs to, by Tailscale's number for them.
    pub user: u64,
    /// A tagged machine belongs to the tailnet, not to a user.
    pub tagged: bool,
}

/// This machine and the peers that are online, when Tailscale is connected.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tailnet {
    pub this: Machine,
    pub peers: Vec<Machine>,
}

/// Asks the daemon who is in the tailnet. Runs a process: call it off the
/// main thread.
pub fn tailnet() -> Result<Tailnet, String> {
    let out = run(&["status", "--json"]).ok_or("Tailscale is not installed on this machine.")?;
    parse_tailnet(&String::from_utf8_lossy(&out.stdout))
        .ok_or_else(|| "Tailscale is not connected.".to_string())
}

fn parse_tailnet(json: &str) -> Option<Tailnet> {
    let s = serde_json::from_str::<StatusJson>(json).ok()?;
    if s.backend_state != "Running" {
        return None;
    }
    let this = s.this?.machine()?;
    let mut peers: Vec<Machine> = s
        .peer
        .unwrap_or_default()
        .values()
        .filter(|n| n.online)
        .filter_map(Node::machine)
        .collect();
    peers.sort_by(|a, b| a.name.cmp(&b.name).then(a.ip.cmp(&b.ip)));
    Some(Tailnet { this, peers })
}

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct WhoisJson {
    node: WhoisNode,
    #[serde(default)]
    user_profile: Option<WhoisUser>,
}

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct WhoisNode {
    #[serde(default)]
    user: u64,
    #[serde(default)]
    tags: Option<Vec<String>>,
}

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct WhoisUser {
    #[serde(default)]
    login_name: String,
}

/// Who a connection from the tailnet came from, as the daemon knows them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Caller {
    pub user: u64,
    pub login: String,
    pub tagged: bool,
}

/// Asks the daemon whose machine is at `peer`, the far end of a connection
/// that came in on the tailnet address. Runs a process.
pub fn whois(peer: SocketAddr) -> Result<Caller, String> {
    let out = run(&["whois", "--json", &peer.to_string()]).ok_or("Tailscale is not installed")?;
    if !out.status.success() {
        return Err(tidy(&String::from_utf8_lossy(&out.stderr)));
    }
    parse_whois(&String::from_utf8_lossy(&out.stdout))
        .ok_or_else(|| "Tailscale did not say who that is".to_string())
}

fn parse_whois(json: &str) -> Option<Caller> {
    let w = serde_json::from_str::<WhoisJson>(json).ok()?;
    Some(Caller {
        user: w.node.user,
        login: w.user_profile.map(|u| u.login_name).unwrap_or_default(),
        tagged: w.node.tags.is_some_and(|t| !t.is_empty()),
    })
}

/// Whether `caller` may pair with the server on `this` machine without a
/// link: only a machine of the same tailnet user. A tagged machine has no
/// user to be the same as, on either side, and a machine shared into the
/// tailnet belongs to someone else.
pub fn same_user(this: &Machine, caller: &Caller) -> Result<(), String> {
    if this.tagged {
        return Err("This server's machine is tagged in the tailnet, so it has no user whose devices it could let in. Pair with a link.".into());
    }
    if caller.tagged || caller.user == 0 || caller.user != this.user {
        return Err("Only devices of the tailnet user this server runs as can pair without a link. Pair with a link.".into());
    }
    Ok(())
}

/// Asks the daemon. Runs a process: call it off the main thread.
pub fn status() -> Status {
    let Some(out) = run(&["status", "--json"]) else {
        return Status::Missing;
    };
    // `status` exits 1 while stopped or logged out, with the JSON still printed.
    parse_status(&String::from_utf8_lossy(&out.stdout))
}

fn parse_status(json: &str) -> Status {
    let Ok(s) = serde_json::from_str::<StatusJson>(json) else {
        return Status::Down;
    };
    if s.backend_state != "Running" {
        return Status::Down;
    }
    let name = s
        .this
        .map(|n| n.dns_name.trim_end_matches('.').to_string())
        .unwrap_or_default();
    let certs = s.cert_domains.unwrap_or_default();
    if name.is_empty() || !certs.iter().any(|d| d.eq_ignore_ascii_case(&name)) {
        return Status::NoHttps;
    }
    Status::Ready { name }
}

/// The address phones use once `serve` is on.
pub fn url(name: &str) -> String {
    format!("https://{name}/")
}

/// Points the tailnet's HTTPS at the phone server on the loopback. Kept by the
/// daemon across runs (`--bg`), so it is also how a run that finds it already
/// there leaves it as is. The TLS-terminated TCP mode hands the connection over
/// whole, so the WebSocket and the request headers arrive as the phone sent them.
pub fn serve(port: u16) -> Result<(), String> {
    let out = run(&[
        "serve",
        "--bg",
        &format!("--tls-terminated-tcp={HTTPS_PORT}"),
        &format!("tcp://127.0.0.1:{port}"),
    ])
    .ok_or("Tailscale is not installed")?;
    if out.status.success() {
        Ok(())
    } else {
        Err(tidy(&String::from_utf8_lossy(&out.stderr)))
    }
}

/// Takes the HTTPS port back; other things the user serves stay.
pub fn stop_serving() -> Result<(), String> {
    let out = run(&[
        "serve",
        &format!("--tls-terminated-tcp={HTTPS_PORT}"),
        "off",
    ])
    .ok_or("Tailscale is not installed")?;
    if out.status.success() {
        Ok(())
    } else {
        Err(tidy(&String::from_utf8_lossy(&out.stderr)))
    }
}

/// The CLI's error, one line, without its "error: " prefix.
fn tidy(stderr: &str) -> String {
    let line = stderr
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("Tailscale refused");
    line.trim_start_matches("error: ")
        .trim_start_matches("Error: ")
        .to_string()
}

/// Runs `tailscale` with `args`; `None` when it is not here.
fn run(args: &[&str]) -> Option<std::process::Output> {
    for program in programs() {
        match crate::platform::command(&program).args(args).output() {
            Ok(out) => return Some(out),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(_) => return None,
        }
    }
    None
}

/// Where the CLI may be: on the PATH, or where the installers put it.
fn programs() -> Vec<String> {
    // Only Windows and macOS add to it.
    #[allow(unused_mut)]
    let mut v = vec!["tailscale".to_string()];
    #[cfg(windows)]
    if let Ok(pf) = std::env::var("ProgramFiles") {
        v.push(format!("{pf}\\Tailscale\\tailscale.exe"));
    }
    #[cfg(target_os = "macos")]
    v.push("/Applications/Tailscale.app/Contents/MacOS/Tailscale".to_string());
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_needs_a_running_daemon_and_a_certificate_for_the_name() {
        let running = r#"{"BackendState":"Running","Self":{"DNSName":"desktop.tail1234.ts.net."},"CertDomains":["desktop.tail1234.ts.net"]}"#;
        assert_eq!(
            parse_status(running),
            Status::Ready {
                name: "desktop.tail1234.ts.net".into()
            }
        );
        assert_eq!(
            url("desktop.tail1234.ts.net"),
            "https://desktop.tail1234.ts.net/"
        );

        let stopped = r#"{"BackendState":"Stopped","Self":{"DNSName":"desktop.tail1234.ts.net."},"CertDomains":null}"#;
        assert_eq!(parse_status(stopped), Status::Down);
        let no_certs = r#"{"BackendState":"Running","Self":{"DNSName":"desktop.tail1234.ts.net."},"CertDomains":null}"#;
        assert_eq!(parse_status(no_certs), Status::NoHttps);
        let other_cert = r#"{"BackendState":"Running","Self":{"DNSName":"desktop.tail1234.ts.net."},"CertDomains":["other.ts.net"]}"#;
        assert_eq!(parse_status(other_cert), Status::NoHttps);
        assert_eq!(parse_status("not json"), Status::Down);
    }

    #[test]
    fn the_tailnet_is_this_machine_and_the_peers_that_are_online() {
        let json = r#"{"BackendState":"Running",
            "Self":{"DNSName":"desk.tail1234.ts.net.","HostName":"Desk","UserID":7,
                    "TailscaleIPs":["100.64.0.1","fd7a::1"],"Online":true},
            "Peer":{
              "a":{"DNSName":"build-box.tail1234.ts.net.","HostName":"ubuntu","UserID":7,
                   "TailscaleIPs":["fd7a::2","100.64.0.2"],"Online":true},
              "b":{"DNSName":"off.tail1234.ts.net.","UserID":7,"TailscaleIPs":["100.64.0.3"],"Online":false},
              "c":{"DNSName":"ci.tail1234.ts.net.","UserID":9,"TailscaleIPs":["100.64.0.4"],
                   "Online":true,"Tags":["tag:ci"]}}}"#;
        let net = parse_tailnet(json).unwrap();
        assert_eq!(net.this.name, "desk");
        assert_eq!(net.this.ip, Ipv4Addr::new(100, 64, 0, 1));
        assert_eq!(
            net.peers
                .iter()
                .map(|p| (p.name.as_str(), p.ip.to_string(), p.tagged))
                .collect::<Vec<_>>(),
            [
                ("build-box", "100.64.0.2".to_string(), false),
                ("ci", "100.64.0.4".to_string(), true)
            ]
        );
        assert_eq!(
            parse_tailnet(r#"{"BackendState":"Stopped","Self":{"DNSName":"d.ts.net."}}"#),
            None
        );
    }

    #[test]
    fn only_a_machine_of_the_same_user_pairs_without_a_link() {
        let whois = r#"{"Node":{"User":7},"UserProfile":{"ID":7,"LoginName":"a@example.com"},"CapMap":null}"#;
        let caller = parse_whois(whois).unwrap();
        assert_eq!(caller.login, "a@example.com");
        let mut this = Machine {
            name: "box".into(),
            ip: Ipv4Addr::new(100, 64, 0, 2),
            user: 7,
            tagged: false,
        };
        assert_eq!(same_user(&this, &caller), Ok(()));
        let other = Caller {
            user: 9,
            ..caller.clone()
        };
        assert!(same_user(&this, &other).is_err());
        // Whatever number a tagged machine carries, it is not a user's.
        let tagged = parse_whois(r#"{"Node":{"User":7,"Tags":["tag:ci"]}}"#).unwrap();
        assert!(same_user(&this, &tagged).is_err());
        this.tagged = true;
        assert!(same_user(&this, &caller).unwrap_err().contains("tagged"));
    }

    #[test]
    fn errors_come_as_one_plain_line() {
        assert_eq!(
            tidy("error: Tailscale HTTPS is not enabled\n  see https://...\n"),
            "Tailscale HTTPS is not enabled"
        );
        assert_eq!(tidy("\n"), "Tailscale refused");
    }
}
