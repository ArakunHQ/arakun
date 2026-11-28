//! Linear for the Linear tab: saved sign-ins and the GraphQL client.
//!
//! Sign-ins (`Session`) are personal API keys. Who signed in is kept in
//! `app_dir/linear/sessions.json`; the key is kept in the keyring
//! (`secret_store`) and never printed. The key goes to `api.linear.app`, and
//! to Arakun's server only to link the account once the person lets Linear
//! start sessions here (`shell::linear_sync`).

pub(crate) mod fixture;
pub mod issues;
pub mod places;

// The rules a delegated issue goes by live in core, so `arakun server` keeps
// them too.
pub use arakun_core::linear::{
    ApiError, answer, delegation, env, events, follow, outbox, pr, prompt, remote, runs, start,
    uploads,
};

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::secret::Secret;

/// What a session signs its calls with.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Credential {
    /// A personal API key (`lin_api_…`) pasted in the tab. The key is in the
    /// keyring; `key` holds it only as a file from before had it, until it
    /// is moved there.
    ApiKey {
        #[serde(default, skip_serializing_if = "Secret::is_empty")]
        key: Secret,
    },
}

impl Credential {
    /// A key that is not in the keyring yet.
    fn in_file(&self) -> Option<&str> {
        match self {
            Credential::ApiKey { key } => Some(key.expose()).filter(|k| !k.is_empty()),
        }
    }
}

/// The keyring entry of the session `id`. Not per test Arakun: the sessions
/// file it goes with is shared too.
fn secret_account(id: &str) -> String {
    format!("linear:{id}")
}

const SECRET_LABEL: &str = "arakun: Linear API key";

/// The key `s` signs its calls with, from the keyring. A key still in the
/// file is moved to the keyring first. Not on the UI thread: the keyring may
/// wait for the person to unlock it. The sync reads it too, to link a
/// workspace (`shell::linear_sync`).
pub(crate) fn key_for(s: &Session) -> Result<String, ApiError> {
    if let Some(key) = s.credential.in_file() {
        move_to_keyring(&s.id, key);
        return Ok(key.to_string());
    }
    match crate::secret_store::get(&secret_account(&s.id)) {
        Ok(Some(key)) => Ok(key),
        Ok(None) => Err(ApiError::SignedOut),
        Err(e) => Err(ApiError::Failed(format!(
            "The keyring could not give the Linear key: {e}"
        ))),
    }
}

/// Moves the key of session `id` from the file to the keyring. When the
/// keyring can't take it, it stays in the file and is still used.
fn move_to_keyring(id: &str, key: &str) {
    let _k = keyring_lock();
    // Signed out or in again since this session was read: its key is not
    // this one any more.
    if !holds_key(id, key) {
        return;
    }
    if let Err(e) = crate::secret_store::set(&secret_account(id), SECRET_LABEL, key) {
        eprintln!("arakun: could not move the Linear key to the keyring: {e}");
        return;
    }
    let _g = store_lock();
    let file = store_file();
    let mut store = Store::read_from(&file);
    let mut moved = false;
    for s in store.sessions.iter_mut() {
        if s.id == id && s.credential.in_file() == Some(key) {
            s.credential = Credential::ApiKey {
                key: Secret::default(),
            };
            moved = true;
        }
    }
    if moved && let Err(e) = store.write_to(&file) {
        eprintln!("arakun: {e}");
    }
}

/// Whether the file still keeps `key` for session `id`.
fn holds_key(id: &str, key: &str) -> bool {
    let _g = store_lock();
    Store::read_from(&store_file())
        .sessions
        .iter()
        .any(|s| s.id == id && s.credential.in_file() == Some(key))
}

/// One keyring write at a time. Apart from `store_lock`, which the UI thread
/// takes to sign out: a keyring can wait minutes for the person to unlock it,
/// and the UI must not wait with it.
fn keyring_lock() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: Mutex<()> = Mutex::new(());
    LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

/// One signed-in Linear user in one workspace.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Session {
    /// `<workspace id>#<user id>`: the same user signing in again replaces only this one.
    pub id: String,
    pub user_id: String,
    pub name: String,
    pub email: String,
    pub workspace_id: String,
    pub workspace: String,
    // Not a secret: the workspace's name in its URL.
    pub url_key: String,
    pub credential: Credential,
    pub added_at: u64,
}

/// The saved sessions, as on disk.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Store {
    pub sessions: Vec<Session>,
}

impl Store {
    pub fn read_from(file: &Path) -> Store {
        crate::durable::load_json(file).unwrap_or_default()
    }

    pub fn write_to(&self, file: &Path) -> Result<(), String> {
        let json = serde_json::to_vec_pretty(self).map_err(|e| e.to_string())?;
        crate::durable::write_private(file, &json)
            .map_err(|e| format!("Couldn't save the Linear sign-in: {e}"))
    }

    /// Adds `s`, or replaces the session of the same user and workspace.
    pub fn upsert(&mut self, s: Session) {
        match self.sessions.iter_mut().find(|o| o.id == s.id) {
            Some(o) => *o = s,
            None => self.sessions.push(s),
        }
    }

    pub fn remove(&mut self, id: &str) -> bool {
        let before = self.sessions.len();
        self.sessions.retain(|s| s.id != id);
        self.sessions.len() != before
    }
}

fn store_file() -> PathBuf {
    crate::paths::shared(crate::paths::LINEAR_SESSIONS)
}

/// One writer at a time: sign-ins save from background threads.
fn store_lock() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: Mutex<()> = Mutex::new(());
    LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

pub fn sessions() -> Vec<Session> {
    if let Some(s) = fixture::sessions() {
        return s;
    }
    Store::read_from(&store_file()).sessions
}

/// Saves `s`, its key in the keyring and the rest in the file.
pub fn save_session(mut s: Session) -> Result<(), String> {
    let _k = keyring_lock();
    if let Some(key) = s.credential.in_file() {
        crate::secret_store::set(&secret_account(&s.id), SECRET_LABEL, key)
            .map_err(|e| format!("The keyring could not keep the Linear key: {e}"))?;
        s.credential = Credential::ApiKey {
            key: Secret::default(),
        };
    }
    let _g = store_lock();
    let mut store = Store::read_from(&store_file());
    store.upsert(s);
    store.write_to(&store_file())
}

/// Removes session `id` from the file now, and its key from the keyring on
/// a thread of its own: the tab signs out on the UI thread.
pub fn remove_session(id: &str) -> Result<(), String> {
    {
        let _g = store_lock();
        let mut store = Store::read_from(&store_file());
        store.remove(id);
        store.write_to(&store_file())?;
    }
    let id = id.to_string();
    std::thread::spawn(move || {
        let _k = keyring_lock();
        // Signed in again meanwhile: that key is the new one.
        let back = sessions_in_file().iter().any(|s| s.id == id);
        if !back {
            crate::secret_store::forget(&secret_account(&id));
        }
    });
    Ok(())
}

/// The sign-in that speaks for a Linear organization: the first one saved.
/// The server link and the reads for a delegated issue both use it, so they
/// act as the same person.
pub fn session_for<'a>(sessions: &'a [Session], organization: &str) -> Option<&'a Session> {
    sessions.iter().find(|s| s.workspace_id == organization)
}

fn sessions_in_file() -> Vec<Session> {
    let _g = store_lock();
    Store::read_from(&store_file()).sessions
}

// ---------------------------------------------------------------------------
// Calls.

/// Runs `query` as `s` and returns its `data` object. The key goes in the
/// header only.
pub(crate) fn graphql(s: &Session, query: &str, variables: Value) -> Result<Value, ApiError> {
    // A fixture's sessions have no key.
    if let Some((status, text)) = fixture::reply(query) {
        return answer(status, None, &text, crate::model::unix_now());
    }
    call(&key_for(s)?, query, variables)
}

/// Runs `query` with `key`, or answers from the fixture in a test Arakun.
fn call(key: &str, query: &str, variables: Value) -> Result<Value, ApiError> {
    if let Some((status, text)) = fixture::reply(query) {
        return answer(status, None, &text, crate::model::unix_now());
    }
    arakun_core::linear::call(key, query, variables)
}

/// The session for the user behind `data`, a `viewer` answer.
fn session_from(data: &Value, credential: Credential, now: u64) -> Option<Session> {
    let viewer = &data["viewer"];
    let user_id = viewer["id"].as_str()?;
    let org = &viewer["organization"];
    let workspace_id = org["id"].as_str()?;
    Some(Session {
        id: format!("{workspace_id}#{user_id}"),
        user_id: user_id.to_string(),
        name: viewer["name"].as_str().unwrap_or_default().to_string(),
        email: viewer["email"].as_str().unwrap_or_default().to_string(),
        workspace_id: workspace_id.to_string(),
        workspace: org["name"].as_str().unwrap_or_default().to_string(),
        url_key: org["urlKey"].as_str().unwrap_or_default().to_string(),
        credential,
        added_at: now,
    })
}

/// Checks a pasted personal API key and says who it belongs to.
pub fn sign_in_with_key(key: &str) -> Result<Session, ApiError> {
    let key = key.trim();
    if key.is_empty() {
        return Err(ApiError::Failed("Paste a Linear API key.".into()));
    }
    let credential = Credential::ApiKey { key: key.into() };
    let data = call(
        key,
        "query { viewer { id name email organization { id name urlKey } } }",
        json!({}),
    )?;
    session_from(&data, credential, crate::model::unix_now())
        .ok_or_else(|| ApiError::Failed("Linear didn't say who this sign-in belongs to.".into()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn session(id: &str, name: &str) -> Session {
        Session {
            id: id.into(),
            user_id: "u1".into(),
            name: name.into(),
            email: "r@x.dev".into(),
            workspace_id: "w1".into(),
            workspace: "Arakun".into(),
            url_key: "arakun".into(),
            credential: Credential::ApiKey { key: "k".into() },
            added_at: 1,
        }
    }

    #[test]
    fn the_first_sign_in_speaks_for_an_organization() {
        let mut other = session("w2#u1", "Other");
        other.workspace_id = "w2".into();
        let s = [other, session("w1#u1", "First"), session("w1#u2", "Second")];
        assert_eq!(session_for(&s, "w1").unwrap().name, "First");
        assert_eq!(session_for(&s, "w2").unwrap().name, "Other");
        assert!(session_for(&s, "w3").is_none());
    }

    #[test]
    fn debug_hides_the_key() {
        let c = Credential::ApiKey {
            key: "lin_api_secret".into(),
        };
        assert!(!format!("{c:?}").contains("secret"));
    }

    #[test]
    fn the_same_user_signing_in_again_replaces_the_session() {
        let mut store = Store::default();
        store.upsert(session("w1#u1", "old"));
        store.upsert(session("w1#u1", "new"));
        store.upsert(session("w2#u1", "other"));
        assert_eq!(store.sessions.len(), 2);
        assert_eq!(store.sessions[0].name, "new");
    }

    #[test]
    fn a_session_is_read_from_viewer() {
        let data = serde_json::json!({"viewer": {"id": "u1", "name": "Rik", "email": "r@x.dev",
            "organization": {"id": "w1", "name": "Arakun", "urlKey": "arakun"}}});
        let s = session_from(&data, Credential::ApiKey { key: "k".into() }, 7).unwrap();
        assert_eq!(
            (
                s.id.as_str(),
                s.workspace.as_str(),
                s.url_key.as_str(),
                s.added_at
            ),
            ("w1#u1", "Arakun", "arakun", 7)
        );
    }

    #[test]
    fn the_store_round_trips_and_is_private() {
        let dir = std::env::temp_dir().join(format!("arakun-lin-{}", std::process::id()));
        let file = dir.join("sessions.json");
        let mut store = Store::default();
        store.upsert(session("w1#u1", "Rik"));
        store.write_to(&file).unwrap();
        assert_eq!(Store::read_from(&file), store);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            assert_eq!(
                std::fs::metadata(&file).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn the_file_keeps_no_key_once_it_is_in_the_keyring() {
        let mut s = session("w1#u1", "Rik");
        s.credential = Credential::ApiKey {
            key: Secret::default(),
        };
        let json = serde_json::to_string(&s).unwrap();
        assert!(
            json.contains(r#""credential":{"kind":"api_key"}"#),
            "{json}"
        );
        let back: Session = serde_json::from_str(&json).unwrap();
        assert_eq!(back.credential.in_file(), None);
        // A file from before still has it, to be moved.
        let old = json.replace(
            r#"{"kind":"api_key"}"#,
            r#"{"kind":"api_key","key":"lin_api_1"}"#,
        );
        let old: Session = serde_json::from_str(&old).unwrap();
        assert_eq!(old.credential.in_file(), Some("lin_api_1"));
    }

    #[test]
    fn an_empty_key_is_not_sent() {
        assert_eq!(
            sign_in_with_key("  "),
            Err(ApiError::Failed("Paste a Linear API key.".into()))
        );
    }
}
