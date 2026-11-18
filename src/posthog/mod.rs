//! PostHog for the PostHog tab: every saved sign-in side by side, the
//! projects they can see, which one belongs to the open Arakun project, and
//! what that project's overview shows.
//!
//! Sign-ins (`Session`) are personal API keys, kept in `app_dir/posthog/sessions.json` (0600) and never printed. One
//! session covers every organization its user belongs to. The projects they
//! see are the `Directory`, cached in `app_dir/cache/posthog-projects.json`.
//! An Arakun project is matched to a PostHog project by the `phc_` keys in its
//! files (`scan`), or by the user's pick, kept in `app_dir/posthog/links.json`.
//!
//! PostHog counts query API calls per hour (since 2026-11-02): the overview
//! reads three small HogQL queries with time ranges, keeps the answer on disk
//! (`cached`), and after a 429 sends no query to that account until its
//! Retry-After has passed (`blocked_until`).

pub mod scan;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

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

/// What a session signs its calls with. Debug never shows the secret.
#[derive(Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Credential {
    /// A personal API key (`phx_…`) pasted in the tab.
    ApiKey { key: String },
}

impl std::fmt::Debug for Credential {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Credential::ApiKey { .. } => f.write_str("ApiKey(…)"),
        }
    }
}

impl Credential {
    fn bearer(&self) -> &str {
        match self {
            Credential::ApiKey { key } => key,
        }
    }
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
}

/// The saved sessions, as on disk.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Store {
    pub sessions: Vec<Session>,
}

impl Store {
    pub fn read_from(file: &Path) -> Store {
        std::fs::read(file)
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default()
    }

    pub fn write_to(&self, file: &Path) -> Result<(), String> {
        let json = serde_json::to_vec_pretty(self).map_err(|e| e.to_string())?;
        write_private(file, &json).map_err(|e| format!("Couldn't save the PostHog sign-in: {e}"))
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
    crate::platform::app_dir().join("posthog")
}

fn cache_dir() -> PathBuf {
    crate::platform::app_dir().join("cache")
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
    Store::read_from(&store_file()).sessions
}

pub fn save_session(s: Session) -> Result<(), String> {
    let _g = store_lock();
    let mut store = Store::read_from(&store_file());
    store.upsert(s);
    store.write_to(&store_file())
}

pub fn remove_session(id: &str) -> Result<(), String> {
    let _g = store_lock();
    let mut store = Store::read_from(&store_file());
    store.remove(id);
    store.write_to(&store_file())
}

/// Writes `bytes` so only this user can read them: 0600 in a 0700 directory
/// on Unix (Windows keeps app data under the user's profile already).
fn write_private(file: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write as _;
    if let Some(d) = file.parent() {
        std::fs::create_dir_all(d)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let _ = std::fs::set_permissions(d, std::fs::Permissions::from_mode(0o700));
        }
    }
    let tmp = file.with_extension("tmp");
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        opts.mode(0o600);
    }
    let mut f = opts.open(&tmp)?;
    f.write_all(bytes)?;
    f.sync_all()?;
    drop(f);
    std::fs::rename(&tmp, file)
}

fn write_json(file: &Path, v: &impl Serialize) {
    if let Some(d) = file.parent() {
        let _ = std::fs::create_dir_all(d);
    }
    if let Ok(json) = serde_json::to_vec(v) {
        let tmp = file.with_extension("tmp");
        if std::fs::write(&tmp, json).is_ok() {
            let _ = std::fs::rename(tmp, file);
        }
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
        }
    }
}

/// One client for every call, so connections (and their TLS) are reused.
fn agent() -> &'static ureq::Agent {
    static AGENT: OnceLock<ureq::Agent> = OnceLock::new();
    AGENT.get_or_init(|| {
        ureq::Agent::config_builder()
            .timeout_global(Some(Duration::from_secs(30)))
            .http_status_as_error(false)
            .user_agent("arakun")
            .build()
            .into()
    })
}

/// Which budget a call spends: queries are counted per hour, the rest is plain REST.
fn bucket(path: &str) -> &'static str {
    if path.contains("/query") {
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

/// How long a 429 asks to wait: Retry-After in seconds or as an HTTP date,
/// a minute when it says nothing usable, never more than an hour and a half.
pub fn retry_after(header: Option<&str>, now: u64) -> u64 {
    let wait = header
        .map(str::trim)
        .and_then(|h| {
            h.parse::<u64>()
                .ok()
                .or_else(|| http_date(h).map(|t| t.saturating_sub(now)))
        })
        .unwrap_or(60);
    now + wait.clamp(1, 90 * 60)
}

/// `Wed, 21 Oct 2015 07:28:00 GMT` → unix seconds.
fn http_date(s: &str) -> Option<u64> {
    let parts: Vec<&str> = s.split_whitespace().collect();
    let [_, day, month, year, time, _] = parts.as_slice() else {
        return None;
    };
    const MONTHS: [&str; 12] = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    let m = MONTHS.iter().position(|x| x.eq_ignore_ascii_case(month))? as i64 + 1;
    let (h, rest) = time.split_once(':')?;
    let (mi, sec) = rest.split_once(':')?;
    let days = days_from_civil(year.parse().ok()?, m, day.parse().ok()?);
    let t = days * 86_400
        + h.parse::<i64>().ok()? * 3600
        + mi.parse::<i64>().ok()? * 60
        + sec.parse::<i64>().ok()?;
    u64::try_from(t).ok()
}

/// `2026-11-04T14:05:09.123-06:00` (or `Z`, or with a space) → unix seconds.
pub fn parse_iso(s: &str) -> Option<u64> {
    let s = s.trim();
    let date = s.get(..10)?;
    let mut d = date.split('-');
    let (y, m, day) = (
        d.next()?.parse().ok()?,
        d.next()?.parse().ok()?,
        d.next()?.parse().ok()?,
    );
    let time = s.get(11..19)?;
    let mut t = time.split(':');
    let (h, mi, sec): (i64, i64, i64) = (
        t.next()?.parse().ok()?,
        t.next()?.parse().ok()?,
        t.next()?.parse().ok()?,
    );
    let rest = &s[19..];
    let rest = rest.trim_start_matches(|c: char| c == '.' || c.is_ascii_digit());
    let offset = match rest.as_bytes().first() {
        Some(b'+') | Some(b'-') => {
            let sign = if rest.starts_with('-') { -1 } else { 1 };
            let (oh, om) = rest[1..]
                .split_once(':')
                .unwrap_or((rest.get(1..3)?, rest.get(3..5).unwrap_or("0")));
            sign * (oh.parse::<i64>().ok()? * 3600 + om.parse::<i64>().ok()? * 60)
        }
        _ => 0,
    };
    u64::try_from(days_from_civil(y, m, day) * 86_400 + h * 3600 + mi * 60 + sec - offset).ok()
}

/// Howard Hinnant's `days_from_civil`.
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// "14:05" in local time.
pub fn clock(t: u64) -> String {
    match crate::platform::local_time(t) {
        Some(l) => format!("{:02}:{:02}", l.hour, l.minute),
        None => format!("{:02}:{:02} UTC", t % 86_400 / 3600, t % 3600 / 60),
    }
}

/// A call with a bearer token against `host`. The token goes in the header only.
fn send(
    host: &str,
    token: &str,
    method: &str,
    path: &str,
    body: Option<&Value>,
) -> Result<Value, ApiError> {
    let url = format!("{}{path}", host.trim_end_matches('/'));
    let request = ureq::http::Request::builder()
        .method(method)
        .uri(&url)
        .header("Authorization", format!("Bearer {token}"))
        .header("Content-Type", "application/json");
    let sent = match body {
        Some(b) => request
            .body(b.to_string())
            .map_err(|e| e.to_string())
            .and_then(|r| agent().run(r).map_err(|e| e.to_string())),
        None => request
            .body(())
            .map_err(|e| e.to_string())
            .and_then(|r| agent().run(r).map_err(|e| e.to_string())),
    };
    let mut response =
        sent.map_err(|e| ApiError::Failed(format!("Couldn't reach PostHog: {e}")))?;
    let status = response.status().as_u16();
    let after = response
        .headers()
        .get("retry-after")
        .and_then(|h| h.to_str().ok())
        .map(str::to_string);
    let text = response
        .body_mut()
        .with_config()
        .limit(16 * 1024 * 1024)
        .read_to_string()
        .unwrap_or_default();
    answer(status, after.as_deref(), &text, crate::model::unix_now())
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
fn call(s: &Session, method: &str, path: &str, body: Option<&Value>) -> Result<Value, ApiError> {
    let now = crate::model::unix_now();
    if let Some(until) = blocked_until(&s.id, path, now) {
        return Err(ApiError::RateLimited(until));
    }
    let got = send(&s.host, s.credential.bearer(), method, path, body);
    if let Err(ApiError::RateLimited(until)) = &got {
        block(&s.id, path, *until);
    }
    got
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
    let credential = Credential::ApiKey {
        key: key.to_string(),
    };
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

fn directory_file() -> PathBuf {
    cache_dir().join("posthog-projects.json")
}

pub fn cached_directory() -> Option<Directory> {
    serde_json::from_slice(&std::fs::read(directory_file()).ok()?).ok()
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
    write_json(&directory_file(), &dir);
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
    std::fs::read(file)
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
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
    write_json(&file, &links);
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
    pub key: String,
    pub name: String,
    pub active: bool,
    /// "100%", "25%", "3 conditions", "multivariate".
    pub rollout: String,
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
    GROUP BY issue ORDER BY last DESC LIMIT 8";

fn overview_file(p: &Project) -> PathBuf {
    let host: String = p
        .host
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect();
    cache_dir()
        .join("posthog")
        .join(format!("{host}-{}.json", p.id))
}

/// What the last read of `p` found, to show at once.
pub fn cached(p: &Project) -> Option<Overview> {
    serde_json::from_slice(&std::fs::read(overview_file(p)).ok()?).ok()
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

fn num(v: &Value) -> u64 {
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
            })
        })
        .collect();
    // Live flags first, then by key.
    flags.sort_by(|a, b| b.active.cmp(&a.active).then_with(|| a.key.cmp(&b.key)));
    flags
}

/// A finished call, or a crash of its thread as an error.
fn joined<T>(h: std::thread::ScopedJoinHandle<'_, Result<T, ApiError>>) -> Result<T, ApiError> {
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
    let (totals, hourly, issues, flags) = std::thread::scope(|scope| {
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
        (joined(t), joined(h), joined(i), joined(f))
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
        if let Some(e @ ApiError::SignedOut(_)) =
            errors.iter().find(|e| matches!(e, ApiError::SignedOut(_)))
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
    write_json(&overview_file(p), &o);
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
    fn rate_limits_honor_retry_after() {
        let now = 1_000_000;
        assert_eq!(retry_after(Some("120"), now), now + 120);
        assert_eq!(retry_after(None, now), now + 60);
        assert_eq!(retry_after(Some("soon"), now), now + 60);
        assert_eq!(retry_after(Some("999999"), now), now + 90 * 60);
        // 2015-10-21 07:28:00 UTC is 1445412480.
        assert_eq!(
            http_date("Wed, 21 Oct 2015 07:28:00 GMT"),
            Some(1_445_412_480)
        );
        assert_eq!(
            retry_after(Some("Wed, 21 Oct 2015 07:28:00 GMT"), 1_445_412_480 - 30),
            1_445_412_480
        );
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
    fn iso_times() {
        assert_eq!(parse_iso("2015-10-21T07:28:00Z"), Some(1_445_412_480));
        assert_eq!(
            parse_iso("2015-10-21T01:28:00.123-06:00"),
            Some(1_445_412_480)
        );
        assert_eq!(parse_iso("2015-10-21 07:28:00"), Some(1_445_412_480));
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
