//! The servers this app is paired with: `arakun server` running on other
//! machines (`crate::server`). To a server the app is one more paired device,
//! like a phone: it pairs with the link `arakun server pair` prints, and then
//! speaks the phone's messages (`crate::phone`) over the phone's ways in, a
//! socket of the server's own or the relay. [`client`] is that connection;
//! the Servers tab (`servers_view`, `shell::servers`) is what it is for.
//!
//! What pairing gave is kept in `servers.json` next to the state, readable by
//! this user only: with the token in it, anyone could start agents on the server.
//! What each server last listed is kept in `servers-inbox.json`, so its
//! environments are back at start before the server answers, or when it can't.

pub mod client;

use std::collections::HashMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

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

fn inbox_file() -> PathBuf {
    file().with_file_name("servers-inbox.json")
}

/// What is kept of a server's inbox: its folders and sessions. No session is
/// shown at work or waiting from it: that was true when it was heard, and the
/// server says what is true now once it answers. An error stays: it is where
/// the session ended. The lines a session changed move during each turn and
/// are not shown from here, so they are not kept.
pub fn kept_inbox(inbox: &Value) -> Value {
    let mut sessions = inbox["sessions"].as_array().cloned().unwrap_or_default();
    for s in &mut sessions {
        if let Some(s) = s.as_object_mut() {
            if s.get("status")
                .is_some_and(|st| st == "working" || st == "needs_input")
            {
                s.insert("status".into(), json!("idle"));
            }
            s.remove("asks");
            s.remove("added");
            s.remove("removed");
        }
    }
    let mut kept = json!({
        "places": inbox["places"].as_array().cloned().unwrap_or_default(),
        "sessions": sessions,
    });
    // Its sessions can be followed before it answers, several at once if it
    // said it can. A list kept without it reads as a server from before.
    if inbox["watch_several"] == true {
        kept["watch_several"] = json!(true);
    }
    kept
}

/// Whether two kept inboxes hold the same folders and sessions. A session's
/// `updated` moves with each change of its status, and the server lists the
/// sessions by it. Both are left out, or every status change would write the
/// file again.
pub fn same_kept(a: &Value, b: &Value) -> bool {
    let without_times = |v: &Value| {
        let mut v = v.clone();
        if let Some(sessions) = v["sessions"].as_array_mut() {
            for s in sessions.iter_mut().filter_map(Value::as_object_mut) {
                s.remove("updated");
            }
            sessions.sort_by(|x, y| x["id"].as_str().cmp(&y["id"].as_str()));
        }
        v
    };
    without_times(a) == without_times(b)
}

/// The inbox each server sent last, by its id.
pub fn load_inboxes() -> HashMap<String, Value> {
    std::fs::read(inbox_file())
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default()
}

/// Whether they were saved.
pub fn save_inboxes(inboxes: &HashMap<String, Value>) -> bool {
    // Folder paths and session titles: this user's only, as the servers are.
    let saved = serde_json::to_vec(inboxes)
        .map_err(std::io::Error::other)
        .and_then(|bytes| crate::durable::write_private(&inbox_file(), &bytes));
    if let Err(e) = &saved {
        eprintln!("arakun: could not save {}: {e}", inbox_file().display());
    }
    saved.is_ok()
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

    #[test]
    fn a_kept_inbox_holds_the_folders_and_sessions_but_not_their_state() {
        let inbox = json!({
            "type": "inbox",
            "host": "box",
            "places": [{ "path": "/srv/app", "branch": "main" }],
            "sessions": [
                { "id": "a", "title": "Fix it", "status": "working", "updated": 2, "added": 3, "removed": 1 },
                { "id": "b", "title": "Ask", "status": "needs_input", "updated": 1, "asks": { "kind": "permission" } },
                { "id": "c", "title": "Broke", "status": "error", "updated": 0 },
            ],
        });
        let kept = kept_inbox(&inbox);
        assert_eq!(kept["places"], inbox["places"]);
        assert_eq!(kept["sessions"][0]["title"], "Fix it");
        // What it was doing then is not what it is doing now.
        assert_eq!(kept["sessions"][0]["status"], "idle");
        assert_eq!(kept["sessions"][1]["status"], "idle");
        assert!(kept["sessions"][1].get("asks").is_none());
        assert!(kept["sessions"][0].get("added").is_none());
        // How it ended does not go stale.
        assert_eq!(kept["sessions"][2]["status"], "error");
        assert!(kept.get("host").is_none());
        assert!(kept.get("watch_several").is_none());
        let mut several = inbox.clone();
        several["watch_several"] = json!(true);
        assert_eq!(kept_inbox(&several)["watch_several"], true);
        // Only a change in what it holds is written again. A status change
        // moves the session's time too.
        let mut later = inbox.clone();
        later["sessions"][0]["status"] = json!("idle");
        later["sessions"][0]["updated"] = json!(1_800_000_000);
        assert!(same_kept(&kept_inbox(&later), &kept));
        later["sessions"][0]["added"] = json!(9);
        assert!(same_kept(&kept_inbox(&later), &kept));
        later["sessions"][0]["title"] = json!("Fix it now");
        assert!(!same_kept(&kept_inbox(&later), &kept));
    }

    #[test]
    fn a_status_change_that_reorders_the_sessions_is_not_a_change() {
        let inbox = json!({
            "places": [],
            "sessions": [
                { "id": "a", "title": "One", "status": "idle", "updated": 2 },
                { "id": "b", "title": "Two", "status": "idle", "updated": 1 },
            ],
        });
        // The second one starts work: the server lists it first now.
        let later = json!({
            "places": [],
            "sessions": [
                { "id": "b", "title": "Two", "status": "working", "updated": 3 },
                { "id": "a", "title": "One", "status": "idle", "updated": 2 },
            ],
        });
        assert!(same_kept(&kept_inbox(&later), &kept_inbox(&inbox)));
        let mut renamed = later.clone();
        renamed["sessions"][0]["title"] = json!("Two, renamed");
        assert!(!same_kept(&kept_inbox(&renamed), &kept_inbox(&inbox)));
    }
}
