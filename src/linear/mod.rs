//! Linear for the Linear tab: saved sign-ins and the GraphQL client.
//!
//! Sign-ins (`Session`) are personal API keys. Who signed in is kept in
//! `app_dir/linear/sessions.json`; the key is kept in the keyring
//! (`secret_store`) and never printed. The key goes to `api.linear.app`, and
//! to Arakun's server only to link the account once the person lets Linear
//! start sessions here (`shell::linear_sync`).

pub mod delegation;
pub(crate) mod fixture;
pub mod issues;
pub mod places;
pub mod prompt;

use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

/// What a session signs its calls with. Debug never shows the secret.
#[derive(Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Credential {
    /// A personal API key (`lin_api_…`) pasted in the tab. The key is in the
    /// keyring; `key` holds it only as a file from before had it, until it
    /// is moved there.
    ApiKey {
        #[serde(default, skip_serializing_if = "String::is_empty")]
        key: String,
    },
}

impl std::fmt::Debug for Credential {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Credential::ApiKey { .. } => f.write_str("ApiKey(…)"),
        }
    }
}

impl Credential {
    /// A key that is not in the keyring yet.
    fn in_file(&self) -> Option<&str> {
        match self {
            Credential::ApiKey { key } => Some(key.as_str()).filter(|k| !k.is_empty()),
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
/// wait for the person to unlock it.
fn key_for(s: &Session) -> Result<String, ApiError> {
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
    let _g = store_lock();
    if let Err(e) = crate::secret_store::set(&secret_account(id), SECRET_LABEL, key) {
        eprintln!("arakun: could not move the Linear key to the keyring: {e}");
        return;
    }
    let file = store_file();
    let mut store = Store::read_from(&file);
    for s in store.sessions.iter_mut().filter(|s| s.id == id) {
        s.credential = Credential::ApiKey { key: String::new() };
    }
    if let Err(e) = store.write_to(&file) {
        eprintln!("arakun: {e}");
    }
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
    crate::platform::app_dir()
        .join("linear")
        .join("sessions.json")
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
    let _g = store_lock();
    if let Some(key) = s.credential.in_file() {
        crate::secret_store::set(&secret_account(&s.id), SECRET_LABEL, key)
            .map_err(|e| format!("The keyring could not keep the Linear key: {e}"))?;
        s.credential = Credential::ApiKey { key: String::new() };
    }
    let mut store = Store::read_from(&store_file());
    store.upsert(s);
    store.write_to(&store_file())
}

pub fn remove_session(id: &str) -> Result<(), String> {
    let _g = store_lock();
    let mut store = Store::read_from(&store_file());
    store.remove(id);
    store.write_to(&store_file())?;
    crate::secret_store::forget(&secret_account(id));
    Ok(())
}

// ---------------------------------------------------------------------------
// Calls.

#[derive(Clone, Debug, PartialEq)]
pub enum ApiError {
    /// The key was revoked or is not valid.
    SignedOut,
    /// Nothing more until this unix second.
    Limited {
        until: u64,
    },
    Failed(String),
}

impl From<crate::guard::Panicked> for ApiError {
    fn from(panicked: crate::guard::Panicked) -> Self {
        ApiError::Failed(panicked.to_string())
    }
}

impl ApiError {
    pub fn message(&self) -> String {
        match self {
            ApiError::SignedOut => "Linear refused the sign-in. Sign in again.".into(),
            ApiError::Failed(m) => m.clone(),
            ApiError::Limited { until } => {
                format!(
                    "Linear asked to wait until {}.",
                    crate::posthog::clock(*until)
                )
            }
        }
    }
}

/// Turns a status and body into the `data` object or the error the tab shows.
/// Linear reports most failures as a 200 (or 400) with an `errors` list.
fn answer(status: u16, retry: Option<&str>, text: &str, now: u64) -> Result<Value, ApiError> {
    let v: Value = serde_json::from_str(text).unwrap_or(Value::Null);
    let first = &v["errors"][0];
    let code = first["extensions"]["code"].as_str();
    if status == 401 || code == Some("AUTHENTICATION_ERROR") {
        return Err(ApiError::SignedOut);
    }
    if status == 429 || code == Some("RATELIMITED") {
        return Err(ApiError::Limited {
            until: crate::posthog::retry_after(retry, now),
        });
    }
    if let Some(m) = first["message"].as_str() {
        return Err(ApiError::Failed(m.to_string()));
    }
    if !(200..300).contains(&status) {
        return Err(ApiError::Failed(format!("Linear answered {status}.")));
    }
    match v.get("data") {
        Some(d) if d.is_object() => Ok(d.clone()),
        _ => Err(ApiError::Failed("Linear sent no data.".into())),
    }
}

const ENDPOINT: &str = "https://api.linear.app/graphql";

/// Runs `query` as `s` and returns its `data` object. The key goes in the
/// header only.
pub(crate) fn graphql(s: &Session, query: &str, variables: Value) -> Result<Value, ApiError> {
    // A fixture's sessions have no key.
    if let Some((status, text)) = fixture::reply(query) {
        return answer(status, None, &text, crate::model::unix_now());
    }
    call(&key_for(s)?, query, variables)
}

/// Linear takes a personal API key as it is, without `Bearer`.
fn call(key: &str, query: &str, variables: Value) -> Result<Value, ApiError> {
    let now = crate::model::unix_now();
    if let Some((status, text)) = fixture::reply(query) {
        return answer(status, None, &text, now);
    }
    let request = ureq::http::Request::builder()
        .method("POST")
        .uri(ENDPOINT)
        .header("Content-Type", "application/json")
        .header("Authorization", key);
    let body = json!({ "query": query, "variables": variables }).to_string();
    let reply = crate::http::run(
        crate::http::agent(Duration::from_secs(30)),
        request,
        Some(body),
        16 * 1024 * 1024,
    )
    .map_err(|e| ApiError::Failed(format!("Couldn't reach Linear: {e}")))?;
    answer(
        reply.status,
        reply.header("retry-after"),
        &reply.text(),
        now,
    )
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
    let credential = Credential::ApiKey {
        key: key.to_string(),
    };
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
    fn a_good_reply_gives_its_data() {
        let v = answer(200, None, r#"{"data":{"viewer":{"id":"u1"}}}"#, 100).unwrap();
        assert_eq!(v["viewer"]["id"], "u1");
    }

    #[test]
    fn a_401_is_signed_out() {
        assert_eq!(answer(401, None, "{}", 100), Err(ApiError::SignedOut));
    }

    #[test]
    fn an_authentication_error_in_a_200_is_signed_out() {
        let body = r#"{"errors":[{"message":"x","extensions":{"code":"AUTHENTICATION_ERROR"}}]}"#;
        assert_eq!(answer(200, None, body, 100), Err(ApiError::SignedOut));
    }

    #[test]
    fn a_rate_limit_says_when() {
        assert_eq!(
            answer(429, Some("30"), "{}", 100),
            Err(ApiError::Limited { until: 130 })
        );
        let body = r#"{"errors":[{"message":"x","extensions":{"code":"RATELIMITED"}}]}"#;
        // No Retry-After: wait a minute.
        assert_eq!(
            answer(400, None, body, 100),
            Err(ApiError::Limited { until: 160 })
        );
    }

    #[test]
    fn another_error_carries_linears_message() {
        let body = r#"{"errors":[{"message":"Entity not found"}]}"#;
        assert_eq!(
            answer(200, None, body, 100),
            Err(ApiError::Failed("Entity not found".into()))
        );
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
        s.credential = Credential::ApiKey { key: String::new() };
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
