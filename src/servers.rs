//! The servers this app reaches: `arakun server` running on other machines
//! (`crate::server`). To a server the app is one more device, like a phone,
//! and speaks the phone's messages (`crate::phone`) over the phone's ways in.
//! A server reached directly (its network, or Tailscale) is paired with the
//! link `arakun server pair` prints. A server signed in to the same account
//! is reached through the relay with the app's own sign-in, and needs no
//! link. [`client`] is that connection; the Computers tab (`servers_view`,
//! `shell::servers`) is what it is for.
//!
//! What pairing gave is kept in `servers.json` next to the state, readable by
//! this user only. The token pairing gave goes to the keyring: with it,
//! anyone could start agents on the server. A file from before still holds
//! it, until the server is next dialed. A server reached through the account
//! has no token at all: the sign-in is the keyring's.
//! What each server last listed is kept in `servers-inbox.json`, so its
//! environments are back at start before the server answers, or when it can't.

pub mod balance;
pub mod client;
pub mod git;

use std::collections::HashMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::secret::Secret;

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
    /// This app as the server knows it. Empty through the account.
    #[serde(default)]
    pub device: String,
    /// What the server lets this app in with. Empty through the account.
    /// It is kept in the keyring (`servers::token`): here it is only while a file
    /// from before holds it, until it is moved, or when the keyring refused
    /// it.
    #[serde(default, skip_serializing_if = "Secret::is_empty")]
    pub token: Secret,
    /// The label of the key a server reached directly signs its handshake
    /// with, pinned at pairing: no other key is trusted at its address.
    /// Empty for one paired before keys were pinned, until it next connects.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub key: String,
    /// The server's user removed this app from its devices: it is not dialed
    /// again until it is forgotten here and connected anew, or the server
    /// would take the app back.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub removed: bool,
}

impl Saved {
    /// The server's label on the relay, when it is reached through it.
    pub fn label(&self) -> Option<&str> {
        match &self.way {
            Way::Direct => None,
            Way::Relay { label, .. } | Way::Account { label } => Some(label),
        }
    }
}

/// How the app gets to a server.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(tag = "via", rename_all = "snake_case")]
pub enum Way {
    /// A socket of the server's own: its network, or Tailscale.
    Direct,
    /// Through the relay with a pairing ticket, from before accounts. Kept in
    /// the file as it is until the account lists the label; the ticket is
    /// never sent again.
    Relay { label: String, ticket: String },
    /// Through the relay with this app's sign-in, end to end encrypted.
    /// `label` names the server's key, which every connection is checked
    /// against.
    Account { label: String },
}

/// Rewrites the servers paired through the old relay whose label the
/// account lists (`labels`) as reached through the account at `base`. The
/// id stays, so their environments stay. Whether any was.
pub fn adopt(saved: &mut [Saved], labels: &[String], base: &str) -> bool {
    let mut changed = false;
    for s in saved.iter_mut() {
        let Way::Relay { label, .. } = &s.way else {
            continue;
        };
        if !labels.contains(label) {
            continue;
        }
        s.way = Way::Account {
            label: label.clone(),
        };
        s.origin = base.to_string();
        s.device.clear();
        s.token = Secret::default();
        changed = true;
    }
    changed
}

/// Notes that the server `id` removed this app from its devices. Whether
/// that is new, so the file is to be written.
pub fn mark_removed(saved: &mut [Saved], id: &str) -> bool {
    match saved.iter_mut().find(|s| s.id == id && !s.removed) {
        Some(s) => {
            s.removed = true;
            true
        }
        None => false,
    }
}

/// What Remove does beyond forgetting the pairing: the label to take off
/// the account, for a server reached through it. One paired by link has
/// nothing on the account; one paired through the old relay may not be
/// listed there, and is reached again only once it is, so only its pairing goes.
pub fn account_to_leave(saved: &Saved) -> Option<&str> {
    match &saved.way {
        Way::Account { label } => Some(label),
        Way::Direct | Way::Relay { .. } => None,
    }
}

/// Whether a server reached through the account can be dialed with the sign-in
/// `base` issued: its label, or why not. One added under another service (its
/// saved origin is another one) stays in the list, but is not dialed: the
/// sign-in only ever goes to the service that issued it.
pub fn account_label<'a>(saved: &'a Saved, base: &str) -> Result<&'a str, String> {
    let Way::Account { label } = &saved.way else {
        return Err("This server is not reached through your account.".into());
    };
    if !crate::cloud::same_origin(&saved.origin, base) {
        return Err(format!(
            "This server belongs to another account server ({}), not {}.",
            saved.origin.trim_end_matches('/'),
            base.trim_end_matches('/')
        ));
    }
    Ok(label)
}

/// What pasting a link of the old relay gets.
pub const RELAY_LINK: &str =
    "Relay links are no longer used. Sign in on the server with `arakun-server login`.";

/// What a pairing link says.
#[derive(Clone, Debug, PartialEq)]
pub struct Invite {
    pub origin: String,
    /// What the server's door takes.
    pub code: String,
    /// The label of the server's key, to pin; a link from before keys were
    /// pinned has none, and the first handshake's key is taken.
    pub key: Option<String>,
}

/// Reads the link `arakun server pair` prints for a server reached directly:
/// `https://host/?p=<code>&k=<label>`. One of the old relay,
/// `https://<label>.<relay>/#p=<code>&s=<secret>`, says what to do instead.
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
    if value(fragment, "p").is_some() && value(fragment, "s").is_some() {
        return Err(RELAY_LINK.into());
    }
    let key = value(query, "k");
    if key
        .as_deref()
        .is_some_and(|k| !crate::phone::identity::is_label(k))
    {
        return Err(HELP.into());
    }
    match value(query, "p") {
        Some(code) => Ok(Invite { origin, code, key }),
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
    crate::durable::load_json(file()).unwrap_or_default()
}

/// Written on the file writer's thread, after every save before it: the one
/// a Remove makes lands last, so an earlier one can't bring the server back.
pub fn save(servers: &[Saved]) {
    match serde_json::to_vec_pretty(servers) {
        Ok(bytes) => {
            // Whether it went is logged there: nothing here waits on it.
            drop(crate::durable::write_private_soon(
                file(),
                bytes,
                "could not save servers.json",
            ));
        }
        Err(e) => eprintln!("arakun: could not save {}: {e}", file().display()),
    }
}

/// The keyring entry of server `id`'s token. Per test Arakun, as the file
/// is: a test Arakun's servers are its own.
fn token_account(id: &str) -> String {
    crate::secret_store::account(&format!("server-token:{id}"))
}

/// Where the servers' tokens are kept: the keyring, or a fake one in the
/// tests. Its calls may wait for the person to unlock the keyring, so they
/// run on a server's connection thread, never on the UI thread.
pub trait Vault: Send + Sync {
    fn get(&self, account: &str) -> Result<Option<String>, String>;
    fn set(&self, account: &str, label: &str, secret: &str) -> Result<(), String>;
    fn forget(&self, account: &str);
}

pub struct Keyring;

impl Vault for Keyring {
    fn get(&self, account: &str) -> Result<Option<String>, String> {
        crate::secret_store::get(account)
    }

    fn set(&self, account: &str, label: &str, secret: &str) -> Result<(), String> {
        crate::secret_store::set(account, label, secret)
    }

    fn forget(&self, account: &str) {
        crate::secret_store::forget(account)
    }
}

/// Puts `saved`'s token in the keyring. Once it is there, the file can go
/// without it.
pub fn keep_token(vault: &dyn Vault, saved: &Saved) -> Result<(), String> {
    let label = format!("arakun: token for the server {}", saved.name);
    vault.set(&token_account(&saved.id), &label, saved.token.expose())
}

/// The token `saved` comes in with: the one it holds (a file from before,
/// or one just paired), else the keyring's. None when there is neither.
pub fn token(vault: &dyn Vault, saved: &Saved) -> Result<Option<String>, String> {
    if !saved.token.is_empty() {
        return Ok(Some(saved.token.expose().to_string()));
    }
    vault
        .get(&token_account(&saved.id))
        .map(|t| t.filter(|t| !t.is_empty()))
}

/// Forgets server `id`'s token, on a thread of its own: this is called on
/// the UI thread, and the keyring may wait for an unlock.
pub fn forget_token(id: &str) {
    let account = token_account(id);
    let _ = std::thread::Builder::new()
        .name("arakun-forget-token".into())
        .spawn(move || Keyring.forget(&account));
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
    crate::durable::load_json(inbox_file()).unwrap_or_default()
}

/// Written as `save` is. What comes back says whether they were.
pub fn save_inboxes(inboxes: &HashMap<String, Value>) -> futures::channel::oneshot::Receiver<bool> {
    // Folder paths and session titles: this user's only, as the servers are.
    match serde_json::to_vec(inboxes) {
        Ok(bytes) => crate::durable::write_private_soon(
            inbox_file(),
            bytes,
            "could not save servers-inbox.json",
        ),
        Err(e) => {
            eprintln!("arakun: could not save {}: {e}", inbox_file().display());
            let (done, saved) = futures::channel::oneshot::channel();
            let _ = done.send(false);
            saved
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// A keyring in memory. `locked` refuses everything, as one that stayed
    /// locked does.
    #[derive(Default)]
    pub(super) struct Fake {
        pub entries: Mutex<HashMap<String, String>>,
        pub locked: bool,
    }

    impl Vault for Fake {
        fn get(&self, account: &str) -> Result<Option<String>, String> {
            if self.locked {
                return Err("locked".into());
            }
            Ok(self.entries.lock().unwrap().get(account).cloned())
        }

        fn set(&self, account: &str, _label: &str, secret: &str) -> Result<(), String> {
            if self.locked {
                return Err("locked".into());
            }
            self.entries
                .lock()
                .unwrap()
                .insert(account.to_string(), secret.to_string());
            Ok(())
        }

        fn forget(&self, account: &str) {
            self.entries.lock().unwrap().remove(account);
        }
    }

    #[test]
    fn the_file_keeps_no_token_once_it_is_in_the_keyring() {
        let vault = Fake::default();
        let mut s = direct();
        keep_token(&vault, &s).unwrap();
        s.token = Secret::default();
        let json = serde_json::to_string(&s).unwrap();
        assert!(!json.contains("token"), "{json}");
        assert_eq!(token(&vault, &s).unwrap().as_deref(), Some("k2"));
        // One from before still holds it, and that one is used.
        let old: Saved = serde_json::from_str(
            &json.replace(r#""device":"d2""#, r#""device":"d2","token":"k3""#),
        )
        .unwrap();
        assert_eq!(token(&vault, &old).unwrap().as_deref(), Some("k3"));
        // Neither: nothing lets the app in.
        let other = Saved {
            id: "c3".into(),
            ..s.clone()
        };
        assert_eq!(token(&vault, &other).unwrap(), None);
        let locked = Fake {
            locked: true,
            ..Fake::default()
        };
        assert!(token(&locked, &s).is_err());
        assert!(keep_token(&locked, &direct()).is_err());
    }

    #[test]
    fn a_link_says_where_and_with_what() {
        assert_eq!(
            parse_link("  http://10.0.0.2:7474/?p=123456\n"),
            Ok(Invite {
                origin: "http://10.0.0.2:7474".into(),
                code: "123456".into(),
                key: None,
            })
        );
        assert_eq!(
            parse_link("http://10.0.0.2:7474/?p=123456&k=5pmbdp76s6bol6ax"),
            Ok(Invite {
                origin: "http://10.0.0.2:7474".into(),
                code: "123456".into(),
                key: Some("5pmbdp76s6bol6ax".into()),
            })
        );
        // A key that is no label is no link of ours.
        assert!(
            parse_link("http://10.0.0.2:7474/?p=123456&k=nope")
                .unwrap_err()
                .contains("not a pairing link")
        );
        // The page's address alone pairs nothing.
        assert!(
            parse_link("https://box.example/")
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
    fn a_relay_link_says_to_sign_in_on_the_server() {
        assert_eq!(
            parse_link("https://abcdefghij234567.arakun.dev/#p=c0de-_A&s=s3cr3t"),
            Err(RELAY_LINK.into())
        );
        assert_eq!(
            RELAY_LINK,
            "Relay links are no longer used. Sign in on the server with `arakun-server login`."
        );
    }

    #[test]
    fn the_socket_is_at_the_same_host() {
        assert_eq!(socket_origin("https://a.arakun.dev"), "wss://a.arakun.dev");
        assert_eq!(socket_origin("http://10.0.0.2:7474"), "ws://10.0.0.2:7474");
    }

    #[test]
    fn a_saved_server_reads_back_every_way() {
        let servers = vec![
            Saved {
                id: "a1".into(),
                name: "build-box".into(),
                origin: "https://app.arakun.dev".into(),
                way: Way::Account {
                    label: "abcdefghij234567".into(),
                },
                device: String::new(),
                token: Secret::default(),
                key: String::new(),
                removed: false,
            },
            Saved {
                id: "b2".into(),
                name: "desk".into(),
                origin: "http://10.0.0.2:7474".into(),
                way: Way::Direct,
                device: "d2".into(),
                token: "k2".into(),
                key: String::new(),
                removed: false,
            },
        ];
        let json = serde_json::to_string(&servers).unwrap();
        assert!(json.contains(r#""via":"account","label":"abcdefghij234567""#));
        assert!(json.contains(r#""via":"direct""#));
        assert_eq!(serde_json::from_str::<Vec<Saved>>(&json).unwrap(), servers);
        // One through the account was written with no device or token.
        let bare = r#"[{"id":"a1","name":"build-box","origin":"https://app.arakun.dev","via":"account","label":"abcdefghij234567"}]"#;
        assert_eq!(
            serde_json::from_str::<Vec<Saved>>(bare).unwrap(),
            servers[..1]
        );
        assert_ne!(new_id(), new_id());
    }

    /// Paired through the relay before accounts: it is reached again once
    /// the account lists it, which takes a sign-in here and on the server.
    fn needs_sign_in(s: &Saved) -> bool {
        matches!(s.way, Way::Relay { .. })
    }

    /// A server paired through the relay before accounts, as `servers.json` kept it.
    fn old_relay_entry() -> Saved {
        let json = r#"{"id":"r1","name":"build-box","origin":"https://abcdefghij234567.arakun.dev","via":"relay","label":"abcdefghij234567","ticket":"t1","device":"d1","token":"k1"}"#;
        serde_json::from_str(json).unwrap()
    }

    #[test]
    fn a_server_paired_through_the_old_relay_needs_a_sign_in() {
        let old = old_relay_entry();
        assert_eq!(
            old.way,
            Way::Relay {
                label: "abcdefghij234567".into(),
                ticket: "t1".into(),
            }
        );
        assert!(needs_sign_in(&old));
        assert_eq!(old.label(), Some("abcdefghij234567"));
        // Kept as it is, ticket and all, until the account lists it.
        let json = serde_json::to_string(&old).unwrap();
        assert!(json.contains(r#""via":"relay""#) && json.contains(r#""ticket":"t1""#));
        let account = Saved {
            way: Way::Account {
                label: "abcdefghij234567".into(),
            },
            ..old.clone()
        };
        assert!(!needs_sign_in(&account));
        assert!(!needs_sign_in(&direct()));
    }

    #[test]
    fn a_server_that_removed_this_app_is_marked_in_the_file() {
        let mut saved = vec![direct(), old_relay_entry()];
        assert!(!saved[1].removed);
        // Not written while it is not so: a file from before reads the same.
        assert!(!serde_json::to_string(&saved).unwrap().contains("removed"));
        assert!(mark_removed(&mut saved, "r1"));
        assert!(saved[1].removed && !saved[0].removed);
        // Told again, or of a server not kept: nothing to write.
        assert!(!mark_removed(&mut saved, "r1"));
        assert!(!mark_removed(&mut saved, "gone"));
        let back: Vec<Saved> =
            serde_json::from_str(&serde_json::to_string(&saved).unwrap()).unwrap();
        assert_eq!(back, saved);
    }

    fn direct() -> Saved {
        Saved {
            id: "b2".into(),
            name: "desk".into(),
            origin: "http://10.0.0.2:7474".into(),
            way: Way::Direct,
            device: "d2".into(),
            token: "k2".into(),
            key: String::new(),
            removed: false,
        }
    }

    #[test]
    fn remove_takes_only_a_server_of_the_account_off_the_account() {
        let mut account = old_relay_entry();
        account.way = Way::Account {
            label: "abcdefghij234567".into(),
        };
        assert_eq!(account_to_leave(&account), Some("abcdefghij234567"));
        assert_eq!(account_to_leave(&direct()), None);
        assert_eq!(account_to_leave(&old_relay_entry()), None);
    }

    #[test]
    fn a_server_of_another_account_server_is_not_dialed() {
        let mut s = old_relay_entry();
        s.way = Way::Account {
            label: "abcdefghij234567".into(),
        };
        s.origin = "https://App.Arakun.dev/".into();
        assert_eq!(
            account_label(&s, "https://app.arakun.dev"),
            Ok("abcdefghij234567")
        );
        s.origin = "https://relay.example".into();
        let why = account_label(&s, "https://app.arakun.dev").unwrap_err();
        assert!(why.contains("another account server"), "{why}");
        assert!(account_label(&direct(), "https://app.arakun.dev").is_err());
    }

    #[test]
    fn an_old_relay_server_comes_through_the_account_once_listed() {
        let mut saved = vec![old_relay_entry(), direct()];
        assert!(!adopt(
            &mut saved,
            &["zzzzzzzzzzzzzzzz".into()],
            "https://app.arakun.dev"
        ));
        assert!(needs_sign_in(&saved[0]));

        assert!(adopt(
            &mut saved,
            &["abcdefghij234567".into()],
            "https://app.arakun.dev"
        ));
        let now = &saved[0];
        assert_eq!(now.id, "r1", "its environments stay where they are");
        assert_eq!(now.name, "build-box");
        assert_eq!(now.origin, "https://app.arakun.dev");
        assert_eq!(
            now.way,
            Way::Account {
                label: "abcdefghij234567".into()
            }
        );
        // The ticket and the old token are of no use any more: gone.
        assert!(now.token.is_empty() && now.device.is_empty());
        assert_eq!(saved[1], direct());
        assert!(!adopt(
            &mut saved,
            &["abcdefghij234567".into()],
            "https://app.arakun.dev"
        ));
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
