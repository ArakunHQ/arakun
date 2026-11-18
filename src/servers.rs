//! The servers this app is paired with: `arakun server` running on other
//! machines (`crate::server`). To a server the app is one more paired device,
//! like a phone: it pairs with the link `arakun server pair` prints, and then
//! speaks the phone's messages (`crate::phone`) over the phone's ways in, a
//! socket of the server's own or the relay. [`client`] is that connection;
//! the Servers tab (`servers_view`, `shell::servers`) is what it is for.
//!
//! What pairing gave is kept in `servers.json` next to the state, readable by
//! this user only: with the token in it, anyone could start agents on the server.

pub mod client;
pub mod tailnet;

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// A server this app is paired with.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Saved {
    /// Ours for it: the server has no name that could not change.
    pub id: String,
    /// What the machine calls itself.
    pub name: String,
    /// Where it is reached: scheme and host, no path.
    pub origin: String,
    #[serde(flatten)]
    pub way: Way,
    /// This app as the server knows it.
    pub device: String,
    /// What the server lets this app in with.
    pub token: String,
}

/// How the app gets to a server.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(tag = "via", rename_all = "snake_case")]
pub enum Way {
    /// A socket of the server's own: its network, or Tailscale.
    Direct,
    /// Through the relay, end to end encrypted. `label` names the server's
    /// key, which every connection is checked against; `ticket` is what the
    /// relay lets this app through with.
    Relay { label: String, ticket: String },
}

/// What a pairing link says.
#[derive(Clone, Debug, PartialEq)]
pub struct Invite {
    pub origin: String,
    /// What the door takes: the server itself, or the relay in front of it.
    pub code: String,
    /// Through the relay, what the server is then shown inside the encrypted
    /// channel, so the relay cannot pair in the app's place.
    pub secret: Option<String>,
}

impl Invite {
    /// No link at all: the server at `origin`, an address in the tailnet,
    /// lets this machine in because Tailscale says it is the same user's.
    pub fn tailnet(origin: String) -> Invite {
        Invite {
            origin,
            code: String::new(),
            secret: None,
        }
    }

    /// A link always carries a code; only [`Invite::tailnet`] has none.
    pub fn is_tailnet(&self) -> bool {
        self.code.is_empty() && self.secret.is_none()
    }
}

/// Reads the link `arakun server pair` prints: `https://host/?p=<code>` for a
/// server reached directly, `https://<label>.<relay>/#p=<code>&s=<secret>`
/// through the relay.
pub fn parse_link(link: &str) -> Result<Invite, String> {
    const HELP: &str = "That is not a pairing link. `arakun-server pair` prints one.";
    let link = link.trim();
    let (scheme, rest) = link.split_once("://").ok_or(HELP)?;
    if !matches!(scheme, "http" | "https") {
        return Err(HELP.into());
    }
    let (rest, fragment) = rest.split_once('#').unwrap_or((rest, ""));
    let (rest, query) = rest.split_once('?').unwrap_or((rest, ""));
    let host = rest.split('/').next().unwrap_or("");
    if host.is_empty() {
        return Err(HELP.into());
    }
    let value = |params: &str, name: &str| {
        params
            .split('&')
            .filter_map(|pair| pair.split_once('='))
            .find(|(k, _)| *k == name)
            .map(|(_, v)| v.to_string())
            .filter(|v| !v.is_empty())
    };
    let origin = format!("{scheme}://{host}");
    // Through the relay both travel in the fragment, which no request carries.
    if let (Some(code), Some(secret)) = (value(fragment, "p"), value(fragment, "s")) {
        return Ok(Invite {
            origin,
            code,
            secret: Some(secret),
        });
    }
    match value(query, "p") {
        Some(code) => Ok(Invite {
            origin,
            code,
            secret: None,
        }),
        None => Err("That link carries no pairing code. Ask the server for a new one.".into()),
    }
}

/// The socket's address for an origin: the same host, `ws` for `http`.
pub fn socket_origin(origin: &str) -> String {
    match origin.split_once("://") {
        Some(("https", rest)) => format!("wss://{rest}"),
        Some((_, rest)) => format!("ws://{rest}"),
        None => format!("wss://{origin}"),
    }
}

/// What the server lists this app as among its devices.
pub fn device_name() -> String {
    match crate::phone::host_name().as_str() {
        // The machine would not say what it is called.
        "this computer" => "Arakun".into(),
        host => format!("Arakun on {host}"),
    }
}

/// An id for a server the app just paired with.
pub fn new_id() -> String {
    let mut bytes = [0u8; 6];
    getrandom::fill(&mut bytes).expect("the OS has random bytes");
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn file() -> PathBuf {
    // A test instance with its own state file keeps its own servers.
    if let Some(state) = std::env::var_os("ARAKUN_STATE") {
        return PathBuf::from(state).with_file_name("servers.json");
    }
    crate::state::data_dir().join("servers.json")
}

pub fn load() -> Vec<Saved> {
    std::fs::read(file())
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default()
}

pub fn save(servers: &[Saved]) {
    let saved = serde_json::to_vec_pretty(servers)
        .map_err(std::io::Error::other)
        .and_then(|bytes| crate::durable::write_private(&file(), &bytes));
    if let Err(e) = saved {
        eprintln!("arakun: could not save {}: {e}", file().display());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_link_says_where_and_with_what() {
        assert_eq!(
            parse_link("  http://10.0.0.2:7474/?p=123456\n"),
            Ok(Invite {
                origin: "http://10.0.0.2:7474".into(),
                code: "123456".into(),
                secret: None,
            })
        );
        assert_eq!(
            parse_link("https://abcdefghij234567.arakun.dev/#p=c0de-_A&s=s3cr3t"),
            Ok(Invite {
                origin: "https://abcdefghij234567.arakun.dev".into(),
                code: "c0de-_A".into(),
                secret: Some("s3cr3t".into()),
            })
        );
        // The page's address alone pairs nothing.
        assert!(
            parse_link("https://abc.arakun.dev/")
                .unwrap_err()
                .contains("no pairing code")
        );
        for bad in ["", "123456", "ftp://host/?p=1", "https:///?p=1"] {
            assert!(
                parse_link(bad).unwrap_err().contains("not a pairing link"),
                "{bad}"
            );
        }
    }

    #[test]
    fn only_an_invite_made_for_the_tailnet_carries_no_code() {
        let invite = Invite::tailnet("http://100.64.0.2:7474".into());
        assert!(invite.is_tailnet());
        assert!(
            !parse_link("http://100.64.0.2:7474/?p=123456")
                .unwrap()
                .is_tailnet()
        );
        // A link whose code is empty is no link, not a tailnet invite.
        assert!(parse_link("http://100.64.0.2:7474/?p=").is_err());
    }

    #[test]
    fn the_socket_is_at_the_same_host() {
        assert_eq!(socket_origin("https://a.arakun.dev"), "wss://a.arakun.dev");
        assert_eq!(socket_origin("http://10.0.0.2:7474"), "ws://10.0.0.2:7474");
    }

    #[test]
    fn a_saved_server_reads_back_either_way() {
        let servers = vec![
            Saved {
                id: "a1".into(),
                name: "build-box".into(),
                origin: "https://abc.arakun.dev".into(),
                way: Way::Relay {
                    label: "abc".into(),
                    ticket: "t".into(),
                },
                device: "d1".into(),
                token: "k1".into(),
            },
            Saved {
                id: "b2".into(),
                name: "desk".into(),
                origin: "http://10.0.0.2:7474".into(),
                way: Way::Direct,
                device: "d2".into(),
                token: "k2".into(),
            },
        ];
        let json = serde_json::to_string(&servers).unwrap();
        assert!(json.contains(r#""via":"relay""#) && json.contains(r#""via":"direct""#));
        assert_eq!(serde_json::from_str::<Vec<Saved>>(&json).unwrap(), servers);
        assert_ne!(new_id(), new_id());
    }
}
