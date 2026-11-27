//! PostHog for the PostHog tab: every saved sign-in side by side, the
//! projects they can see, which one belongs to the open Arakun project, and
//! what that project's overview shows.
//!
//! Sign-ins (`Session`) are personal API keys. Who signed in is kept in
//! `app_dir/posthog/sessions.json`; the key is kept in the keyring
//! (`secret_store`) and never printed. One session covers every organization
//! its user belongs to. The projects they see are the `Directory`, cached
//! in `app_dir/cache/posthog-projects.json`. An Arakun project is matched
//! to a PostHog project by the `phc_` keys in its files (`scan`), or by the
//! user's pick, kept in `app_dir/posthog/links.json`.
//!
//! PostHog counts query API calls per hour (since 2026-11-02): the overview
//! reads three small HogQL queries with time ranges, keeps the answer on disk
//! (`cached`), and after a 429 sends no query to that account until its
//! Retry-After has passed (`blocked_until`).
//!
//! The Events and People tabs read through `query`: paged HogQL with bound
//! values, one hourly budget per account, and answers cached on disk. A 403
//! names the missing scope (`scope`) so only the tab that needs it stops.

pub mod ask;
pub mod defs;
pub mod events;
mod fixture;
pub mod flag_edit;
pub mod inspection;
pub mod lifecycle;
pub mod people;
pub mod query;
pub mod scan;
pub mod scope;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::secret::Secret;

/// Where an account lives: PostHog Cloud US or EU.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Region {
    Us,
    Eu,
}

impl Region {
    pub const ALL: [Region; 2] = [Region::Us, Region::Eu];

    pub fn host(self) -> &'static str {
        match self {
            Region::Us => "https://us.posthog.com",
            Region::Eu => "https://eu.posthog.com",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Region::Us => "US",
            Region::Eu => "EU",
        }
    }

    pub fn of_host(host: &str) -> Option<Region> {
        Region::ALL
            .into_iter()
            .find(|r| r.host() == host.trim_end_matches('/'))
    }
}

/// What a session signs its calls with.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Credential {
    /// A personal API key (`phx_…`) pasted in the tab. The key is in the
    /// keyring; `key` holds it only as a file from before had it, until it
    /// is moved there, and between checking a new key and saving it.
    ApiKey {
        #[serde(default, skip_serializing_if = "Secret::is_empty")]
        key: Secret,
    },
}

impl Credential {
    fn bearer(&self) -> &str {
        match self {
            Credential::ApiKey { key } => key.expose(),
        }
    }

    /// A key that is not in the keyring yet.
    fn in_file(&self) -> Option<&str> {
        Some(self.bearer()).filter(|k| !k.is_empty())
    }
}

/// The keyring entry of the session `id`. Not per test Arakun: the sessions
/// file it goes with is shared too.
fn secret_account(id: &str) -> String {
    format!("posthog:{id}")
}

const SECRET_LABEL: &str = "arakun: PostHog personal API key";

/// The keys read from the keyring this run, by session. The tab makes many
/// calls at once, and each would ask the keyring again otherwise.
fn keys() -> std::sync::MutexGuard<'static, HashMap<String, String>> {
    static KEYS: OnceLock<Mutex<HashMap<String, String>>> = OnceLock::new();
    KEYS.get_or_init(Default::default)
        .lock()
        .unwrap_or_else(|e| e.into_inner())
}

/// The key `s` signs its calls with, from the keyring. A key still in the
/// file is moved to the keyring first. Not on the UI thread: the keyring may
/// wait for the person to unlock it.
fn key_for(s: &Session) -> Result<String, ApiError> {
    // A fixture answers every call, and its sessions have no key to look for.
    if fixture::active() {
        return Ok(String::new());
    }
    if let Some(key) = s.credential.in_file() {
        move_to_keyring(&s.id, key);
        return Ok(key.to_string());
    }
    // Held while the keyring is asked, so calls that start together wait for
    // one answer instead of each asking.
    let mut keys = keys();
    if let Some(key) = keys.get(&s.id) {
        return Ok(key.clone());
    }
    match crate::secret_store::get(&secret_account(&s.id)) {
        Ok(Some(key)) => {
            keys.insert(s.id.clone(), key.clone());
            Ok(key)
        }
        Ok(None) => Err(ApiError::SignedOut(
            "The keyring has no key for this sign-in. Sign in again.".into(),
        )),
        Err(e) => Err(ApiError::Failed(format!(
            "The keyring could not give the PostHog key: {e}"
        ))),
    }
}

/// Moves the key of session `id` from the file to the keyring. When the
/// keyring can't take it, it stays in the file and is still used.
fn move_to_keyring(id: &str, key: &str) {
    let _g = store_lock();
    let file = store_file();
    let mut store = Store::read_from(&file);
    // Removed or signed in again since `key` was read: nothing to move.
    let Some(s) = store
        .sessions
        .iter_mut()
        .find(|s| s.id == id && s.credential.in_file() == Some(key))
    else {
        return;
    };
    if let Err(e) = crate::secret_store::set(&secret_account(id), SECRET_LABEL, key) {
        eprintln!("arakun: could not move the PostHog key to the keyring: {e}");
        return;
    }
    s.credential = Credential::ApiKey {
        key: Secret::default(),
    };
    if let Err(e) = store.write_to(&file) {
        eprintln!("arakun: {e}");
    }
    keys().insert(id.to_string(), key.to_string());
}

/// One signed-in PostHog user on one region.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Session {
    /// `<host>#<user uuid>`: the same user signing in again replaces only this one.
    pub id: String,
    pub host: String,
    pub email: String,
    pub name: String,
    pub credential: Credential,
    pub added_at: u64,
}

impl Session {
    pub fn region_label(&self) -> &'static str {
        Region::of_host(&self.host).map_or("self-hosted", Region::label)
    }

    /// Whether `other` is this sign-in, with no key change since. Signing in
    /// again sets a new `added_at`. The key is left out: it moves from the
    /// file to the keyring without the sign-in changing.
    fn same_sign_in(&self, other: &Session) -> bool {
        self.id == other.id
            && self.host == other.host
            && self.email == other.email
            && self.name == other.name
            && self.added_at == other.added_at
    }
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
            .map_err(|e| format!("Couldn't save the PostHog sign-in: {e}"))
    }

    /// Adds `s`, or replaces the session of the same user and region.
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

fn dir() -> PathBuf {
    crate::paths::shared(crate::paths::POSTHOG)
}

pub(crate) fn cache_dir() -> PathBuf {
    crate::paths::shared(crate::paths::CACHE)
}

fn store_file() -> PathBuf {
    dir().join("sessions.json")
}

/// One writer at a time: sign-ins save from background threads.
fn store_lock() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: Mutex<()> = Mutex::new(());
    LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

pub fn sessions() -> Vec<Session> {
    forget_shared_cache();
    if let Some(s) = fixture::sessions() {
        return s;
    }
    Store::read_from(&store_file()).sessions
}

/// Saves `s`, its key in the keyring and the rest in the file. Not on the UI
/// thread: it asks the keyring.
pub fn save_session(mut s: Session) -> Result<(), String> {
    let _g = store_lock();
    let mut store = Store::read_from(&store_file());
    let key = s.credential.bearer().to_string();
    // The key saved before, to tell whether this is the same one again.
    let before =
        store
            .sessions
            .iter()
            .find(|o| o.id == s.id)
            .map(|o| match o.credential.in_file() {
                Some(k) => Some(k.to_string()),
                None => crate::secret_store::get(&secret_account(&o.id))
                    .ok()
                    .flatten(),
            });
    if !key.is_empty() {
        crate::secret_store::set(&secret_account(&s.id), SECRET_LABEL, &key)
            .map_err(|e| format!("The keyring could not keep the PostHog key: {e}"))?;
        s.credential = Credential::ApiKey {
            key: Secret::default(),
        };
        keys().insert(s.id.clone(), key.clone());
    }
    invalidate_directory()?;
    // The same key again keeps its answers and pins; a new one may see less.
    if before.flatten().is_none_or(|k| k != key) {
        clear_answers(&s.id)?;
    }
    store.upsert(s);
    store.write_to(&store_file())
}

/// Removes the sign-in `id`. Its key leaves the keyring on a thread of its
/// own, as the keyring may wait to be unlocked.
pub fn remove_session(id: &str) -> Result<(), String> {
    let _g = store_lock();
    let mut store = Store::read_from(&store_file());
    invalidate_directory()?;
    clear_answers(id)?;
    store.remove(id);
    store.write_to(&store_file())?;
    keys().remove(id);
    let id = id.to_string();
    std::thread::spawn(move || {
        let _g = store_lock();
        // Signed in again meanwhile: that key is the new one.
        if !Store::read_from(&store_file())
            .sessions
            .iter()
            .any(|s| s.id == id)
        {
            crate::secret_store::forget(&secret_account(&id));
        }
    });
    Ok(())
}

/// Where `id`'s answers are kept. Each account has its own directory, so two
/// accounts that see the same project never read each other's answers, and
/// removing one clears only its own.
fn answers_dir(id: &str) -> PathBuf {
    cache_dir().join("posthog-answers").join(
        Sha256::digest(id.as_bytes())
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>(),
    )
}

fn clear_answers(id: &str) -> Result<(), String> {
    match std::fs::remove_dir_all(answers_dir(id)) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(format!(
            "Could not clear this account's cached answers: {e}"
        )),
    }
}

/// Saves an answer under `s`'s cache, readable only by this user. Skipped
/// when the sign-in was removed or its key replaced while the request ran, so
/// a late answer can't come back after its cache was cleared.
fn cache_answer(s: &Session, file: &Path, value: &impl Serialize) -> std::io::Result<()> {
    let _guard = store_lock();
    if !sessions().iter().any(|o| o.same_sign_in(s)) {
        return Err(std::io::Error::other("The sign-in changed."));
    }
    crate::durable::write_private(file, &serde_json::to_vec(value)?)
}

/// Earlier versions kept every account's answers together in `cache/posthog`,
/// readable by other users. Nothing reads them now, so they go once per run,
/// the first time the tab looks for its accounts.
fn forget_shared_cache() {
    static DONE: OnceLock<()> = OnceLock::new();
    if DONE.set(()).is_ok() {
        let _ = std::fs::remove_dir_all(cache_dir().join("posthog"));
    }
}

// ---------------------------------------------------------------------------
// Calls.

#[derive(Clone, Debug, PartialEq)]
pub enum ApiError {
    /// 401/403 on the sign-in itself: the key was revoked, or lacks a scope.
    SignedOut(String),
    /// 429: nothing more until this unix second.
    RateLimited(u64),
    /// 403 on one endpoint: the key lacks this scope (`person:read`).
    MissingScope(String),
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
            ApiError::SignedOut(m) | ApiError::Failed(m) => m.clone(),
            ApiError::RateLimited(until) => {
                format!("PostHog asked to wait until {}.", clock(*until))
            }
            ApiError::MissingScope(scope) => format!("The key lacks the {scope} scope."),
        }
    }
}

/// Which budget a call spends: queries are counted per hour, the rest is plain REST.
fn bucket(path: &str) -> &'static str {
    if path.contains("/query") || path.ends_with(ask::PATH) {
        "query"
    } else {
        "api"
    }
}

fn blocks() -> std::sync::MutexGuard<'static, HashMap<String, u64>> {
    static BLOCKS: OnceLock<Mutex<HashMap<String, u64>>> = OnceLock::new();
    BLOCKS
        .get_or_init(Default::default)
        .lock()
        .unwrap_or_else(|e| e.into_inner())
}

/// When PostHog said this session may query again, if that is still ahead.
pub fn blocked_until(session: &str, path: &str, now: u64) -> Option<u64> {
    blocks()
        .get(&format!("{session} {}", bucket(path)))
        .copied()
        .filter(|t| *t > now)
}

fn block(session: &str, path: &str, until: u64) {
    blocks().insert(format!("{session} {}", bucket(path)), until);
}

pub use crate::when::{clock, parse_iso, retry_after};

/// What came back from one call, before it is judged.
struct Reply {
    status: u16,
    retry_after: Option<String>,
    /// PostHog's rate-limit headers, when it sends them: (remaining, limit).
    remaining: Option<u64>,
    limit: Option<u64>,
    text: String,
}

/// A call with a bearer token against `host`. The token goes in the header only.
fn fetch(
    host: &str,
    token: &str,
    method: &str,
    path: &str,
    body: Option<&Value>,
) -> Result<Reply, ApiError> {
    if let Some(r) = fixture::reply(method, path, body) {
        return Ok(r);
    }
    let url = format!("{}{path}", host.trim_end_matches('/'));
    // PostHog AI writes and runs a query before it answers, which often
    // takes longer than a plain read.
    let timeout = if path.ends_with(ask::PATH) { 120 } else { 30 };
    let reply = crate::http::json_call(
        crate::http::agent(Duration::from_secs(timeout)),
        method,
        &url,
        Some(token),
        body.map(Value::to_string),
        16 * 1024 * 1024,
    )
    .map_err(|e| ApiError::Failed(format!("Couldn't reach PostHog: {e}")))?;
    let number = |names: &[&str]| {
        names
            .iter()
            .find_map(|n| reply.header(n))
            .and_then(|v| v.trim().parse::<u64>().ok())
    };
    let status = reply.status;
    let retry_after = reply.header("retry-after").map(str::to_string);
    let remaining = number(&["x-ratelimit-remaining", "ratelimit-remaining"]);
    let limit = number(&["x-ratelimit-limit", "ratelimit-limit"]);
    let text = reply.text();
    Ok(Reply {
        status,
        retry_after,
        remaining,
        limit,
        text,
    })
}

fn send(
    host: &str,
    token: &str,
    method: &str,
    path: &str,
    body: Option<&Value>,
) -> Result<Value, ApiError> {
    let r = fetch(host, token, method, path, body)?;
    answer(
        r.status,
        r.retry_after.as_deref(),
        &r.text,
        crate::model::unix_now(),
    )
}

/// Turns a status and body into the value or the error the tab shows.
fn answer(status: u16, retry: Option<&str>, text: &str, now: u64) -> Result<Value, ApiError> {
    let v: Value = serde_json::from_str(text).unwrap_or(Value::Null);
    let detail = v["detail"]
        .as_str()
        .or(v["error"].as_str())
        .map(str::to_string);
    match status {
        200..=299 => Ok(v),
        429 => Err(ApiError::RateLimited(retry_after(retry, now))),
        401 => Err(ApiError::SignedOut(
            detail.unwrap_or_else(|| "PostHog refused the sign-in.".into()),
        )),
        403 => Err(ApiError::SignedOut(detail.unwrap_or_else(|| {
            "This sign-in can't read that (a missing scope?).".into()
        }))),
        _ => Err(ApiError::Failed(
            detail.unwrap_or_else(|| format!("PostHog answered {status}.")),
        )),
    }
}

/// A call as `s`, skipped while PostHog has asked this session to wait.
/// A 403 that names a scope (or hits an endpoint with a known one) comes back
/// as `MissingScope`, so only what needs that scope stops.
pub(crate) fn call(
    s: &Session,
    method: &str,
    path: &str,
    body: Option<&Value>,
) -> Result<Value, ApiError> {
    let now = crate::model::unix_now();
    if let Some(until) = blocked_until(&s.id, path, now) {
        return Err(ApiError::RateLimited(until));
    }
    let key = key_for(s)?;
    if bucket(path) == "query" {
        query::reserve(&s.id, now)?;
    }
    let reply = match fetch(&s.host, &key, method, path, body) {
        Err(ApiError::Failed(e)) if writes(method, path) => Err(ApiError::Failed(format!(
            "{e} The change may still have been saved. Reload before trying again."
        ))),
        other => other,
    };
    if bucket(path) == "query" {
        let (remaining, limit) = reply
            .as_ref()
            .map_or((None, None), |r| (r.remaining, r.limit));
        query::note_headers(&s.id, remaining, limit, now);
    }
    let reply = reply?;
    let got = answer(reply.status, reply.retry_after.as_deref(), &reply.text, now);
    match got {
        Err(ApiError::RateLimited(until)) => {
            block(&s.id, path, until);
            Err(ApiError::RateLimited(until))
        }
        // A refused change is not a missing read scope: the tab keeps
        // working, and the person needs PostHog's reason.
        Err(ApiError::SignedOut(detail)) if reply.status == 403 && writes(method, path) => {
            Err(ApiError::Failed(format!(
                "PostHog refused this change. The key needs {}, and the project may \
                 require approval for it. PostHog said: {detail}",
                write_scope(path)
            )))
        }
        Err(ApiError::SignedOut(detail)) if reply.status == 403 => {
            Err(scope::forbidden(path, &detail)
                .map_or(ApiError::SignedOut(detail), ApiError::MissingScope))
        }
        other => other,
    }
}

/// Whether a call changes something in PostHog, rather than reading or
/// running a query.
fn writes(method: &str, path: &str) -> bool {
    method != "GET" && bucket(path) != "query"
}

/// The scope a change on `path` needs.
fn write_scope(path: &str) -> &'static str {
    if path.contains("/experiments/") {
        "experiment:write"
    } else {
        "feature_flag:write"
    }
}

/// `s` for a URL query value.
pub(crate) fn encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Who a credential belongs to, and what it can see.

/// The user behind a credential on `host`, as a session ready to save.
pub fn identify(host: &str, credential: Credential) -> Result<Session, ApiError> {
    let me = send(host, credential.bearer(), "GET", "/api/users/@me/", None)?;
    let uuid = me["uuid"]
        .as_str()
        .or(me["distinct_id"].as_str())
        .unwrap_or_default();
    let email = me["email"].as_str().unwrap_or_default().to_string();
    if uuid.is_empty() && email.is_empty() {
        return Err(ApiError::Failed(
            "PostHog didn't say who this sign-in belongs to.".into(),
        ));
    }
    let name = [me["first_name"].as_str(), me["last_name"].as_str()]
        .into_iter()
        .flatten()
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join(" ");
    Ok(Session {
        id: format!(
            "{}#{}",
            host.trim_end_matches('/'),
            if uuid.is_empty() {
                email.as_str()
            } else {
                uuid
            }
        ),
        host: host.trim_end_matches('/').to_string(),
        email,
        name,
        credential,
        added_at: crate::model::unix_now(),
    })
}

/// Checks a pasted personal API key on `region`, or on both when None.
pub fn sign_in_with_key(key: &str, region: Option<Region>) -> Result<Session, ApiError> {
    let key = key.trim();
    if key.starts_with("phc_") {
        return Err(ApiError::Failed("That's a project key (phc_), which only sends events. Paste a personal API key (phx_).".into()));
    }
    let credential = Credential::ApiKey { key: key.into() };
    let tries: Vec<Region> = match region {
        Some(r) => vec![r],
        None => Region::ALL.to_vec(),
    };
    let mut last = ApiError::Failed("No region to try.".into());
    for r in tries {
        match identify(r.host(), credential.clone()) {
            Ok(s) => return Ok(s),
            Err(e @ ApiError::SignedOut(_)) => last = e,
            Err(e) => return Err(e),
        }
    }
    Err(last)
}

/// A PostHog project one of the sessions can see.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Project {
    pub session: String,
    pub host: String,
    pub email: String,
    pub org_id: String,
    pub org: String,
    pub id: u64,
    pub name: String,
    /// The project's public key (`phc_…`), as found in its apps' code.
    #[serde(default)]
    // Not a secret: the project's public key, in its apps' code.
    pub api_token: String,
}

impl Project {
    pub fn url(&self, path: &str) -> String {
        format!("{}/project/{}{path}", self.host, self.id)
    }
}

/// Every project the sessions see, with what couldn't be read.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Directory {
    pub projects: Vec<Project>,
    pub problems: Vec<String>,
    pub fetched_at: u64,
}

/// A read that panicked found no projects.
impl crate::guard::Fallback for Directory {
    fn fallback(_: crate::guard::Panicked) -> Self {
        Self::default()
    }
}

fn directory_file() -> PathBuf {
    cache_dir().join("posthog-projects.json")
}

fn invalidate_directory() -> Result<(), String> {
    match std::fs::remove_file(directory_file()) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(format!("Could not clear the saved project directory: {e}")),
    }
}

pub fn cached_directory() -> Option<Directory> {
    crate::durable::load_json(directory_file())
}

/// One session's projects, and what it couldn't read.
type SessionProjects = (Vec<Project>, Vec<String>);

/// The organizations and projects of one session.
fn projects_of(s: &Session) -> Result<SessionProjects, ApiError> {
    let me = call(s, "GET", "/api/users/@me/", None)?;
    let mut orgs: Vec<(String, String)> = me["organizations"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|o| {
            Some((
                o["id"].as_str()?.to_string(),
                o["name"].as_str().unwrap_or_default().to_string(),
            ))
        })
        .collect();
    if orgs.is_empty()
        && let Some(id) = me["organization"]["id"].as_str()
    {
        orgs.push((
            id.to_string(),
            me["organization"]["name"]
                .as_str()
                .unwrap_or_default()
                .to_string(),
        ));
    }
    let mut out = Vec::new();
    let mut problems = Vec::new();
    for (org_id, org) in orgs {
        match call(
            s,
            "GET",
            &format!("/api/organizations/{org_id}/projects/?limit=200"),
            None,
        ) {
            Ok(v) => {
                for p in parse_projects(&v) {
                    let mut p = Project {
                        session: s.id.clone(),
                        host: s.host.clone(),
                        email: s.email.clone(),
                        org_id: org_id.clone(),
                        org: org.clone(),
                        ..p
                    };
                    if p.api_token.is_empty()
                        && let Ok(d) = call(s, "GET", &format!("/api/projects/{}/", p.id), None)
                    {
                        p.api_token = d["api_token"].as_str().unwrap_or_default().to_string();
                    }
                    out.push(p);
                }
            }
            Err(ApiError::RateLimited(until)) => return Err(ApiError::RateLimited(until)),
            Err(e) => problems.push(format!("{} · {org}: {}", s.email, e.message())),
        }
    }
    Ok((out, problems))
}

/// `results` (or a bare list) of a projects listing.
fn parse_projects(v: &Value) -> Vec<Project> {
    let list = v["results"].as_array().or(v.as_array());
    list.into_iter()
        .flatten()
        .filter_map(|p| {
            Some(Project {
                session: String::new(),
                host: String::new(),
                email: String::new(),
                org_id: String::new(),
                org: String::new(),
                id: p["id"].as_u64()?,
                name: p["name"].as_str().unwrap_or("Untitled").to_string(),
                api_token: p["api_token"].as_str().unwrap_or_default().to_string(),
            })
        })
        .collect()
}

/// Reads every session's projects in parallel and keeps them for `cached_directory`.
/// A session that fails keeps the projects the last read found for it.
pub fn load_directory(sessions: &[Session]) -> Directory {
    let previous = cached_directory().unwrap_or_default();
    let mut dir = Directory {
        fetched_at: crate::model::unix_now(),
        ..Default::default()
    };
    let results: Vec<(Session, Result<SessionProjects, ApiError>)> = std::thread::scope(|scope| {
        let handles: Vec<_> = sessions
            .iter()
            .cloned()
            .map(|s| {
                scope.spawn(move || {
                    let got = projects_of(&s);
                    (s, got)
                })
            })
            .collect();
        handles.into_iter().filter_map(|h| h.join().ok()).collect()
    });
    for (s, got) in results {
        match got {
            Ok((projects, problems)) => {
                dir.projects.extend(projects);
                dir.problems.extend(problems);
            }
            Err(e) => {
                dir.problems.push(format!("{}: {}", s.email, e.message()));
                dir.projects.extend(
                    previous
                        .projects
                        .iter()
                        .filter(|p| p.session == s.id)
                        .cloned(),
                );
            }
        }
    }
    // Same as `cache_answer`: a directory read with a sign-in that changed
    // since is not saved.
    let _guard = store_lock();
    let now = self::sessions();
    if sessions.len() == now.len()
        && sessions.iter().zip(&now).all(|(a, b)| a.same_sign_in(b))
        && let Ok(bytes) = serde_json::to_vec(&dir)
        && let Err(e) = crate::durable::write_private(&directory_file(), &bytes)
    {
        eprintln!("arakun: could not save the PostHog project directory: {e}");
    }
    dir
}

// ---------------------------------------------------------------------------
// Which PostHog project is the open Arakun project.

/// A remembered answer for one Arakun project.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Link {
    pub session: String,
    pub project: u64,
    /// Found by its `phc_` key rather than picked.
    #[serde(default)]
    pub auto: bool,
}

fn links_file() -> PathBuf {
    dir().join("links.json")
}

fn root_key(root: &Path) -> String {
    root.to_string_lossy().into_owned()
}

pub fn links_from(file: &Path) -> HashMap<String, Link> {
    crate::durable::load_json(file).unwrap_or_default()
}

pub fn link_of(root: &Path) -> Option<Link> {
    links_from(&links_file()).remove(&root_key(root))
}

/// Remembers (or with None forgets) which project `root` shows.
pub fn set_link(root: &Path, link: Option<Link>) {
    let file = links_file();
    let mut links = links_from(&file);
    match link {
        Some(l) => links.insert(root_key(root), l),
        None => links.remove(&root_key(root)),
    };
    if let Err(e) = crate::durable::save_json(&file, &links) {
        eprintln!("arakun: could not save the PostHog project links: {e}");
    }
}

#[derive(Debug, PartialEq)]
pub enum Resolved<'a> {
    /// The remembered pick, or the one project whose key is in the files.
    One(&'a Project, bool),
    /// Several projects' keys are in the files.
    Several(Vec<&'a Project>),
    /// Nothing matched: the user picks from everything.
    Unknown,
}

/// Which project `root` shows: its remembered link if the project is still
/// visible, else the projects whose `api_token` is among the `keys` found in
/// its files. The same project seen by two sessions counts once (the first).
pub fn resolve<'a>(link: Option<&Link>, keys: &[String], projects: &'a [Project]) -> Resolved<'a> {
    if let Some(l) = link
        && let Some(p) = projects
            .iter()
            .find(|p| p.session == l.session && p.id == l.project)
    {
        return Resolved::One(p, l.auto);
    }
    let mut hits: Vec<&Project> = Vec::new();
    for k in keys {
        for p in projects
            .iter()
            .filter(|p| !p.api_token.is_empty() && &p.api_token == k)
        {
            if !hits.iter().any(|h| h.host == p.host && h.id == p.id) {
                hits.push(p);
            }
        }
    }
    match hits.len() {
        0 => Resolved::Unknown,
        1 => Resolved::One(hits[0], true),
        _ => Resolved::Several(hits),
    }
}

// ---------------------------------------------------------------------------
// The overview of one project.

/// An error tracking issue with exceptions in the last week.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Issue {
    pub id: String,
    pub kind: String,
    pub message: String,
    pub count: u64,
    pub users: u64,
    pub last_seen: u64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Flag {
    pub id: u64,
    // Not a secret: the flag's name.
    pub key: String,
    pub name: String,
    pub active: bool,
    /// "100%", "25%", "3 conditions", "multivariate".
    pub rollout: String,
    /// PostHog's count of changes to the flag, sent back with a change so
    /// PostHog can refuse it when the flag changed since it was read.
    #[serde(default)]
    pub version: Option<u64>,
    /// The release conditions as PostHog stores them, for changing a rollout.
    #[serde(default)]
    pub filters: Value,
}

/// An experiment as the project's list gives it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Experiment {
    pub id: u64,
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub start_date: Option<String>,
    #[serde(default)]
    pub end_date: Option<String>,
    #[serde(default)]
    pub archived: bool,
    #[serde(default)]
    pub conclusion: Option<String>,
}

impl Experiment {
    /// Where it is in its life, from its dates, as PostHog names it.
    pub fn status(&self) -> &'static str {
        if self.archived {
            "Archived"
        } else if self.end_date.is_some() {
            "Complete"
        } else if self.start_date.is_some() {
            "Running"
        } else {
            "Draft"
        }
    }
}

/// What the tab shows for a project. Parts that failed keep the last answer.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Overview {
    pub project: u64,
    pub events: u64,
    pub pageviews: u64,
    pub visitors: u64,
    /// Events and unique persons per hour, oldest first (24).
    pub hourly: Vec<(u64, u64)>,
    pub issues: Vec<Issue>,
    pub flags: Vec<Flag>,
    /// The first 100 experiments. A failed read leaves them empty with
    /// `experiment_error`, so a missing scope doesn't stop the rest.
    #[serde(default)]
    pub experiments: Vec<Experiment>,
    #[serde(default)]
    pub experiment_error: Option<String>,
    /// PostHog has more than `experiments`.
    #[serde(default)]
    pub experiments_more: bool,
    pub fetched_at: u64,
    /// When PostHog's query budget lets the next read happen.
    pub limited_until: Option<u64>,
    pub problems: Vec<String>,
}

pub const HOURS: usize = 24;

const TOTALS: &str = "SELECT count(), countIf(event = '$pageview'), uniq(person_id) FROM events \
    WHERE timestamp > now() - INTERVAL 24 HOUR";
const HOURLY: &str = "SELECT intDiv(dateDiff('second', timestamp, now()), 3600) AS ago, count(), uniq(person_id) FROM events \
    WHERE timestamp > now() - INTERVAL 24 HOUR GROUP BY ago ORDER BY ago";
const ISSUES: &str = "SELECT properties.$exception_issue_id AS issue, any(properties.$exception_types), \
    any(properties.$exception_values), count(), uniq(person_id), max(timestamp) AS last FROM events \
    WHERE event = '$exception' AND timestamp > now() - INTERVAL 7 DAY AND issue IS NOT NULL \
    GROUP BY issue ORDER BY uniq(person_id) DESC, last DESC LIMIT 100";

fn overview_file(p: &Project) -> PathBuf {
    answers_dir(&p.session).join(format!("{}-overview.json", p.id))
}

/// Drops what the last read of `p` found, so the next look reads it again.
pub fn forget_cached(p: &Project) {
    let _ = std::fs::remove_file(overview_file(p));
}

/// What the last read of `p` found, to show at once.
pub fn cached(p: &Project) -> Option<Overview> {
    crate::durable::load_json(overview_file(p))
}

fn hogql(s: &Session, project: u64, q: &str) -> Result<Vec<Value>, ApiError> {
    let body = json!({ "query": { "kind": "HogQLQuery", "query": q } });
    let v = call(
        s,
        "POST",
        &format!("/api/projects/{project}/query/"),
        Some(&body),
    )?;
    Ok(v["results"].as_array().cloned().unwrap_or_default())
}

pub(crate) fn num(v: &Value) -> u64 {
    v.as_u64()
        .or_else(|| v.as_f64().map(|f| f.max(0.) as u64))
        .or_else(|| v.as_str().and_then(|s| s.parse().ok()))
        .unwrap_or(0)
}

/// Rows of HOURLY → 24 buckets, oldest first.
fn parse_hourly(rows: &[Value]) -> Vec<(u64, u64)> {
    let mut out = vec![(0, 0); HOURS];
    for r in rows {
        let ago = (num(&r[0]) as usize).min(HOURS - 1);
        out[HOURS - 1 - ago] = (num(&r[1]), num(&r[2]));
    }
    out
}

/// `["TypeError"]` (an array, or that array as JSON text) → "TypeError".
fn first_of(v: &Value) -> String {
    let arr = match v {
        Value::String(s) => {
            serde_json::from_str::<Value>(s).unwrap_or_else(|_| Value::String(s.clone()))
        }
        other => other.clone(),
    };
    match arr {
        Value::Array(a) => a
            .first()
            .and_then(|x| x.as_str())
            .unwrap_or_default()
            .to_string(),
        Value::String(s) => s,
        _ => String::new(),
    }
}

fn parse_issues(rows: &[Value]) -> Vec<Issue> {
    rows.iter()
        .filter_map(|r| {
            Some(Issue {
                id: r[0].as_str()?.to_string(),
                kind: first_of(&r[1]),
                message: first_of(&r[2]),
                count: num(&r[3]),
                users: num(&r[4]),
                last_seen: r[5].as_str().and_then(parse_iso).unwrap_or(0),
            })
        })
        .collect()
}

fn parse_flags(v: &Value) -> Vec<Flag> {
    let mut flags: Vec<Flag> = v["results"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|f| f["deleted"].as_bool() != Some(true))
        .filter_map(|f| {
            let groups = f["filters"]["groups"]
                .as_array()
                .cloned()
                .unwrap_or_default();
            let rollout = if f["filters"]["multivariate"].is_object() {
                "multivariate".to_string()
            } else if groups.len() == 1 {
                let g = &groups[0];
                let pct = g["rollout_percentage"].as_f64().unwrap_or(100.);
                let props = g["properties"].as_array().is_some_and(|p| !p.is_empty());
                if props {
                    format!("{pct:.0}% of a condition")
                } else {
                    format!("{pct:.0}%")
                }
            } else if groups.is_empty() {
                "0%".to_string()
            } else {
                format!("{} conditions", groups.len())
            };
            Some(Flag {
                id: f["id"].as_u64()?,
                key: f["key"].as_str()?.to_string(),
                name: f["name"].as_str().unwrap_or_default().to_string(),
                active: f["active"].as_bool().unwrap_or(false),
                rollout,
                version: f["version"].as_u64(),
                filters: f["filters"].clone(),
            })
        })
        .collect();
    // Live flags first, then by key.
    flags.sort_by(|a, b| b.active.cmp(&a.active).then_with(|| a.key.cmp(&b.key)));
    flags
}

/// A finished call, or a crash of its thread as an error.
pub(crate) fn joined<T>(
    h: std::thread::ScopedJoinHandle<'_, Result<T, ApiError>>,
) -> Result<T, ApiError> {
    h.join()
        .unwrap_or_else(|_| Err(ApiError::Failed("The read crashed.".into())))
}

/// Reads the overview of `p` as `s`: three HogQL queries and the flags list,
/// in parallel. A part that fails keeps what `previous` had for it.
pub fn load_overview(
    s: Session,
    p: &Project,
    previous: Option<&Overview>,
) -> Result<Overview, ApiError> {
    let id = p.id;
    let (totals, hourly, issues, flags, experiments) = std::thread::scope(|scope| {
        let s = &s;
        let t = scope.spawn(move || hogql(s, id, TOTALS));
        let h = scope.spawn(move || hogql(s, id, HOURLY));
        let i = scope.spawn(move || hogql(s, id, ISSUES));
        let f = scope.spawn(move || {
            call(
                s,
                "GET",
                &format!("/api/projects/{id}/feature_flags/?limit=100"),
                None,
            )
        });
        let e = scope.spawn(move || {
            let value = call(
                s,
                "GET",
                &format!("/api/projects/{id}/experiments/?limit=100"),
                None,
            )?;
            let items: Vec<Experiment> = serde_json::from_value(value["results"].clone())
                .map_err(|_| ApiError::Failed("Could not read the experiments list.".into()))?;
            Ok((items, value["next"].is_string()))
        });
        (joined(t), joined(h), joined(i), joined(f), joined(e))
    });
    let errors: Vec<&ApiError> = [
        totals.as_ref().err(),
        hourly.as_ref().err(),
        issues.as_ref().err(),
        flags.as_ref().err(),
    ]
    .into_iter()
    .flatten()
    .collect();
    let previous = previous.filter(|o| o.project == id);
    if errors.len() == 4 {
        // Every part refused the sign-in: say so instead of showing stale numbers as fine.
        if let Some(e @ (ApiError::SignedOut(_) | ApiError::MissingScope(_))) = errors
            .iter()
            .find(|e| matches!(e, ApiError::SignedOut(_) | ApiError::MissingScope(_)))
        {
            return Err((*e).clone());
        }
        // Nothing read and nothing saved: the error, not a page of zeros.
        if previous.is_none() {
            return Err(errors[0].clone());
        }
    }
    let base = previous.cloned().unwrap_or_default();
    let mut o = Overview {
        project: id,
        fetched_at: crate::model::unix_now(),
        ..base
    };
    o.problems.clear();
    o.limited_until = errors
        .iter()
        .filter_map(|e| {
            if let ApiError::RateLimited(t) = e {
                Some(*t)
            } else {
                None
            }
        })
        .max();
    let mut problem = |what: &str, e: &ApiError| {
        if !matches!(e, ApiError::RateLimited(_)) {
            o.problems.push(format!("{what}: {}", e.message()));
        }
    };
    match &totals {
        Ok(rows) => {
            let r = rows.first().cloned().unwrap_or(Value::Null);
            o.events = num(&r[0]);
            o.pageviews = num(&r[1]);
            o.visitors = num(&r[2]);
        }
        Err(e) => problem("Events", e),
    }
    match &hourly {
        Ok(rows) => o.hourly = parse_hourly(rows),
        Err(e) => problem("Hourly events", e),
    }
    match &issues {
        Ok(rows) => o.issues = parse_issues(rows),
        Err(e) => problem("Error tracking", e),
    }
    match &flags {
        Ok(v) => o.flags = parse_flags(v),
        Err(e) => problem("Feature flags", e),
    }
    o.experiment_error = None;
    match experiments {
        Ok((items, more)) => {
            o.experiments = items;
            o.experiments_more = more;
        }
        Err(e) => {
            o.experiments.clear();
            o.experiments_more = false;
            o.experiment_error = Some(e.message());
        }
    }
    let _ = cache_answer(&s, &overview_file(p), &o);
    Ok(o)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn project(session: &str, id: u64, token: &str) -> Project {
        Project {
            session: session.into(),
            host: "https://us.posthog.com".into(),
            email: "me@x.dev".into(),
            org_id: "o".into(),
            org: "Org".into(),
            id,
            name: format!("p{id}"),
            api_token: token.into(),
        }
    }

    #[test]
    fn overview_paths_do_not_cross_accounts_for_the_same_project() {
        let a = project("us#a", 7, "");
        let b = project("us#b", 7, "");
        assert_ne!(overview_file(&a), overview_file(&b));
        assert!(overview_file(&a).starts_with(answers_dir(&a.session)));
    }

    #[test]
    fn only_flag_and_experiment_changes_count_as_writes() {
        assert!(writes("PATCH", "/api/projects/1/feature_flags/2/"));
        assert!(writes("POST", "/api/projects/1/experiments/3/end/"));
        assert!(!writes("GET", "/api/projects/1/experiments/3/"));
        // Queries and PostHog AI are POSTs too, but only read.
        assert!(!writes("POST", "/api/environments/1/query/"));
        assert!(!writes(
            "POST",
            "/api/environments/1/max_tools/create_and_query_insight/"
        ));
        assert_eq!(
            write_scope("/api/projects/1/experiments/3/ship_variant/"),
            "experiment:write"
        );
        assert_eq!(
            write_scope("/api/projects/1/feature_flags/2/"),
            "feature_flag:write"
        );
    }

    #[test]
    fn store_round_trips_side_by_side_and_private() {
        let dir = std::env::temp_dir().join(format!("arakun-ph-{}", std::process::id()));
        let file = dir.join("sessions.json");
        let s = |id: &str, email: &str, key: &str| Session {
            id: id.into(),
            host: "https://us.posthog.com".into(),
            email: email.into(),
            name: String::new(),
            credential: Credential::ApiKey { key: key.into() },
            added_at: 1,
        };
        let mut store = Store::default();
        store.upsert(s("us#a", "alice@example.com", "phx_1"));
        store.upsert(s("us#b", "bob@example.org", "phx_2"));
        // The same user again replaces only its own session.
        store.upsert(s("us#a", "alice@example.com", "phx_3"));
        store.write_to(&file).unwrap();
        let back = Store::read_from(&file);
        assert_eq!(back, store);
        assert_eq!(back.sessions.len(), 2);
        assert_eq!(
            back.sessions[0].credential,
            Credential::ApiKey {
                key: "phx_3".into()
            }
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            assert_eq!(
                std::fs::metadata(&file).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
        let mut store = back;
        assert!(store.remove("us#b"));
        assert!(!store.remove("us#b"));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn the_file_keeps_no_key_once_it_is_in_the_keyring() {
        let s = Session {
            id: "https://us.posthog.com#a".into(),
            host: "https://us.posthog.com".into(),
            email: "alice@example.com".into(),
            name: String::new(),
            credential: Credential::ApiKey {
                key: Secret::default(),
            },
            added_at: 1,
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
            r#"{"kind":"api_key","key":"phx_1"}"#,
        );
        let old: Session = serde_json::from_str(&old).unwrap();
        assert_eq!(old.credential.in_file(), Some("phx_1"));
        // Moving the key does not make it another sign-in.
        assert!(old.same_sign_in(&back));
        let again = Session {
            added_at: 2,
            ..back.clone()
        };
        assert!(!again.same_sign_in(&back));
    }

    #[test]
    fn debug_never_shows_secrets() {
        assert!(
            !format!(
                "{:?}",
                Credential::ApiKey {
                    key: "phx_secret".into()
                }
            )
            .contains("secret")
        );
    }

    #[test]
    fn a_429_asks_to_wait() {
        let now = 1_000_000;
        assert_eq!(
            answer(429, Some("30"), "", now),
            Err(ApiError::RateLimited(now + 30))
        );
        assert!(
            matches!(answer(401, None, r#"{"detail":"Invalid token"}"#, now), Err(ApiError::SignedOut(m)) if m == "Invalid token")
        );
        assert_eq!(answer(200, None, r#"{"ok":1}"#, now).unwrap()["ok"], 1);
    }

    #[test]
    fn a_blocked_session_skips_queries_until_the_time() {
        block("us#z", "/api/projects/1/query/", 500);
        assert_eq!(
            blocked_until("us#z", "/api/projects/2/query/", 100),
            Some(500)
        );
        assert_eq!(
            blocked_until("us#z", "/api/projects/2/feature_flags/", 100),
            None
        );
        assert_eq!(blocked_until("us#z", "/api/projects/2/query/", 600), None);
    }

    #[test]
    fn matching_projects_by_key() {
        let projects = vec![
            project("a", 1, "phc_one"),
            project("a", 2, "phc_two"),
            project("b", 1, "phc_one"),
            project("b", 3, ""),
        ];
        // One key: that project, once even though two sessions see it.
        assert_eq!(
            resolve(None, &["phc_one".into()], &projects),
            Resolved::One(&projects[0], true)
        );
        assert_eq!(
            resolve(None, &["phc_one".into(), "phc_two".into()], &projects),
            Resolved::Several(vec![&projects[0], &projects[1]])
        );
        assert_eq!(
            resolve(None, &["phc_nope".into()], &projects),
            Resolved::Unknown
        );
        assert_eq!(resolve(None, &[], &projects), Resolved::Unknown);
        // A remembered pick wins over the keys, while the project is still visible.
        let link = Link {
            session: "b".into(),
            project: 3,
            auto: false,
        };
        assert_eq!(
            resolve(Some(&link), &["phc_one".into()], &projects),
            Resolved::One(&projects[3], false)
        );
        let gone = Link {
            session: "c".into(),
            project: 9,
            auto: false,
        };
        assert_eq!(
            resolve(Some(&gone), &["phc_two".into()], &projects),
            Resolved::One(&projects[1], true)
        );
    }

    #[test]
    fn parses_answers() {
        let v: Value = serde_json::from_str(
            r#"{"results":[{"id":7,"name":"Web","api_token":"phc_x"},{"name":"no id"}]}"#,
        )
        .unwrap();
        let p = parse_projects(&v);
        assert_eq!((p.len(), p[0].id, p[0].api_token.as_str()), (1, 7, "phc_x"));

        let rows: Vec<Value> = serde_json::from_str("[[0, 10, 3], [5, 4, 2], [40, 1, 1]]").unwrap();
        let h = parse_hourly(&rows);
        assert_eq!(h.len(), HOURS);
        assert_eq!(h[HOURS - 1], (10, 3));
        assert_eq!(h[HOURS - 6], (4, 2));
        assert_eq!(h[0], (1, 1));

        let rows: Vec<Value> =
            serde_json::from_str(r#"[["iss1", "[\"TypeError\"]", ["x is undefined"], 12, 3, "2026-11-04T10:00:00Z"], [null, "", "", 1, 1, ""]]"#).unwrap();
        let i = parse_issues(&rows);
        assert_eq!(i.len(), 1);
        assert_eq!(
            (i[0].kind.as_str(), i[0].message.as_str(), i[0].count),
            ("TypeError", "x is undefined", 12)
        );

        let v: Value = serde_json::from_str(
            r#"{"results":[
                {"id":1,"key":"beta","active":false,"filters":{"groups":[{"rollout_percentage":25,"properties":[]}]}},
                {"id":2,"key":"new-nav","active":true,"filters":{"groups":[{"rollout_percentage":null,"properties":[]}]}},
                {"id":3,"key":"exp","active":true,"filters":{"groups":[{}],"multivariate":{"variants":[]}}},
                {"id":4,"key":"old","active":true,"deleted":true,"filters":{}}
            ]}"#,
        )
        .unwrap();
        let f = parse_flags(&v);
        let keys: Vec<(&str, &str)> = f
            .iter()
            .map(|f| (f.key.as_str(), f.rollout.as_str()))
            .collect();
        assert_eq!(
            keys,
            vec![
                ("exp", "multivariate"),
                ("new-nav", "100%"),
                ("beta", "25%")
            ]
        );
    }

    #[test]
    fn rejects_project_keys_as_sign_in() {
        assert!(matches!(
            sign_in_with_key("phc_abc", Some(Region::Us)),
            Err(ApiError::Failed(_))
        ));
    }
}
