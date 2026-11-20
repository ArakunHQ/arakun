//! Doop for the Doop tab: the instances the user signed in to, the canvases
//! each one holds, and the frames of a canvas with their renders.
//!
//! An instance is an origin (`https://doop.arakun.dev`, or a self-hosted one).
//! Everything is read through its MCP endpoint, `<instance>/mcp`, the same one
//! the agents use: JSON-RPC over POST with a bearer token. The token is an
//! OAuth one from a sign-in in the browser (`oauth`), or an agent key the user
//! pasted. Sign-ins (`Session`) are kept in `app_dir/doop/sessions.json`
//! (0600) and never printed.
//!
//! Instances differ in version, so nothing here assumes a fixed tool list. The
//! tab asks for the tools first (`tools/list`), picks the ones it knows by
//! name, and names their arguments from their schemas. An instance without a
//! tool that lists canvases still works: the user adds a canvas by its link,
//! and it is kept in `app_dir/doop/pins.json`.

mod fixture;
pub mod oauth;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use base64::Engine as _;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::Digest as _;

use crate::agent::AgentKind;

/// Doop as Arakun hosts it. Any other origin is a self-hosted instance.
pub const DEFAULT_HOST: &str = "https://doop.arakun.dev";

/// A render is drawn at most this wide: the grid's cards are smaller.
const THUMB_W: u32 = 720;
/// A render on disk is used as it is for this long.
const THUMB_FRESH: Duration = Duration::from_secs(24 * 3600);

#[derive(Clone, Debug, PartialEq)]
pub enum Error {
    /// The instance refused the token: sign in again.
    SignedOut(String),
    Failed(String),
}

impl From<crate::guard::Panicked> for Error {
    fn from(panicked: crate::guard::Panicked) -> Self {
        Error::Failed(panicked.to_string())
    }
}

impl Error {
    pub fn message(&self) -> String {
        match self {
            Error::SignedOut(m) | Error::Failed(m) => m.clone(),
        }
    }
}

// ---------------------------------------------------------------------------
// Instances.

/// The origin of what the user typed: `doop.acme.dev`, a canvas link or the
/// MCP address all name the same instance. Empty is Doop itself. Plain http
/// is only taken for a host on this machine or network, since the token
/// travels with every call. The answer holds only characters that are safe
/// in a shell command (`connect_command`).
pub fn normalize_host(input: &str) -> Result<String, String> {
    let s = input.trim();
    if s.is_empty() {
        return Ok(DEFAULT_HOST.into());
    }
    let (scheme, rest) = match s.split_once("://") {
        Some((scheme, rest)) => (scheme.to_ascii_lowercase(), rest),
        None => ("https".to_string(), s),
    };
    if scheme != "https" && scheme != "http" {
        return Err("An instance address starts with https://.".into());
    }
    let authority = rest
        .split(['/', '?', '#'])
        .next()
        .unwrap_or_default()
        .to_ascii_lowercase();
    let safe = |c: char| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | ':' | '[' | ']');
    if authority.is_empty() || !authority.chars().all(safe) {
        return Err("That doesn't look like an instance address (doop.example.com).".into());
    }
    if scheme == "http" && !is_local(&authority) {
        return Err(
            "Use https:// for this instance. Plain http only works on this machine or network."
                .into(),
        );
    }
    Ok(format!("{scheme}://{authority}"))
}

/// Whether `authority` (host and maybe a port) is this machine or a private network.
fn is_local(authority: &str) -> bool {
    if authority.starts_with('[') {
        return authority.starts_with("[::1]");
    }
    let host = authority.split(':').next().unwrap_or_default();
    if let Ok(ip) = host.parse::<std::net::Ipv4Addr>() {
        return ip.is_loopback() || ip.is_private() || ip.is_link_local();
    }
    host == "localhost"
        || !host.contains('.')
        || [".local", ".lan", ".internal", ".home.arpa", ".localhost"]
            .iter()
            .any(|tld| host.ends_with(tld))
}

/// `doop.arakun.dev`: an instance as the tab names it.
pub fn host_label(host: &str) -> &str {
    host.trim_start_matches("https://")
        .trim_start_matches("http://")
}

pub fn mcp_url(host: &str) -> String {
    format!("{host}/mcp")
}

/// The public render of a frame.
pub fn render_url(host: &str, frame: &str) -> String {
    format!("{host}/i/{}.png", encode(frame))
}

/// A canvas on the web, at one of its frames when given.
pub fn canvas_url(host: &str, canvas: &str, frame: Option<&str>) -> String {
    match frame {
        Some(f) => format!("{host}/c/{}?frame={}", encode(canvas), encode(f)),
        None => format!("{host}/c/{}", encode(canvas)),
    }
}

/// A canvas the user named: its id, and its instance when a link said it
/// (`https://doop.acme.dev/c/bnj8ZjYM5d?frame=…`, or just `bnj8ZjYM5d`).
pub fn canvas_ref(input: &str) -> Option<(Option<String>, String)> {
    let s = input.trim();
    let is_id = |id: &str| {
        (4..=64).contains(&id.len())
            && id
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_'))
    };
    if let Some((_, after)) = s.split_once("/c/") {
        let id = after.split(['/', '?', '#']).next().unwrap_or_default();
        let host = normalize_host(s).ok()?;
        return is_id(id).then(|| (Some(host), id.to_string()));
    }
    is_id(s).then(|| (None, s.to_string()))
}

/// `s` for a URL path or query value.
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

/// A host or an id as a file name: only characters every file system takes.
fn file_safe(s: &str) -> String {
    s.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.') {
                c
            } else {
                '_'
            }
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Sign-ins.

/// What a session signs its calls with. Debug never shows the secrets.
#[derive(Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Credential {
    /// An agent key pasted in the tab.
    AgentKey { key: String },
    /// Tokens from a sign-in in the browser.
    #[serde(rename = "oauth")]
    OAuth { tokens: oauth::Tokens },
}

impl std::fmt::Debug for Credential {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Credential::AgentKey { .. } => f.write_str("AgentKey(…)"),
            Credential::OAuth { .. } => f.write_str("OAuth(…)"),
        }
    }
}

impl Credential {
    fn bearer(&self) -> &str {
        match self {
            Credential::AgentKey { key } => key,
            Credential::OAuth { tokens } => &tokens.access,
        }
    }

    pub fn label(&self) -> &'static str {
        match self {
            Credential::AgentKey { .. } => "agent key",
            Credential::OAuth { .. } => "signed in",
        }
    }
}

/// One sign-in on one instance.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Session {
    /// `<host>#<who>`: the same user signing in again replaces only this one.
    pub id: String,
    pub host: String,
    /// Empty when the instance does not say (an agent key has only a name).
    #[serde(default)]
    pub email: String,
    #[serde(default)]
    pub name: String,
    pub credential: Credential,
    pub added_at: u64,
}

impl Session {
    /// The email, else the name, else how the sign-in was made.
    pub fn who(&self) -> String {
        if !self.email.is_empty() {
            self.email.clone()
        } else if !self.name.is_empty() {
            self.name.clone()
        } else {
            match self.credential {
                Credential::AgentKey { .. } => "Agent key".into(),
                Credential::OAuth { .. } => "Signed in".into(),
            }
        }
    }

    pub fn self_hosted(&self) -> bool {
        self.host != DEFAULT_HOST
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
        crate::durable::write_private(file, &json)
            .map_err(|e| format!("Couldn't save the Doop sign-in: {e}"))
    }

    /// Adds `s`, or replaces the session of the same user and instance.
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
    crate::platform::app_dir().join("doop")
}

fn store_file() -> PathBuf {
    dir().join("sessions.json")
}

/// One writer at a time: sign-ins and token refreshes save from background threads.
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

/// The agent key the environment or `~/.config/arakun/doop_key` holds, to
/// offer in the form. The Canvas command reads the same two places.
pub fn key_at_hand() -> Option<String> {
    let from_env = std::env::var("DOOP_KEY").ok();
    let from_file = || {
        std::fs::read_to_string(
            crate::platform::home()
                .join(".config")
                .join("arakun")
                .join("doop_key"),
        )
        .ok()
    };
    from_env
        .or_else(from_file)
        .map(|k| k.trim().to_string())
        .filter(|k| !k.is_empty())
}

/// Checks a pasted agent key against `host`: it must be able to list the tools.
pub fn sign_in_with_key(host: &str, key: &str) -> Result<Session, Error> {
    let key = key.trim();
    let digest = sha2::Sha256::digest(key.as_bytes());
    let mark: String = digest.iter().take(4).map(|b| format!("{b:02x}")).collect();
    let s = Session {
        id: format!("{host}#key:{mark}"),
        host: host.to_string(),
        email: String::new(),
        name: String::new(),
        credential: Credential::AgentKey {
            key: key.to_string(),
        },
        added_at: crate::model::unix_now(),
    };
    match post(host, key, &request("tools/list", json!({})))? {
        (200..=299, _) => Ok(named(s)),
        (401 | 403, _) => Err(Error::SignedOut(format!(
            "{} refused this key.",
            host_label(host)
        ))),
        (status, _) => Err(Error::Failed(format!(
            "{} answered {status}. Is this a Doop instance?",
            host_label(host)
        ))),
    }
}

/// `s` with the name of the key's owner, when the instance has a tool that
/// says it. The same owner's key on the same instance is then one sign-in.
fn named(mut s: Session) -> Session {
    if fixture::on() {
        return s;
    }
    let call = request(
        "tools/call",
        json!({"name": "get_profile", "arguments": {}}),
    );
    let profile = post(&s.host, s.credential.bearer(), &call)
        .ok()
        .and_then(|(status, text)| answer(&s.host, status, &text).ok())
        .and_then(|r| tool_answer(&r).ok())
        .map(|a| a.value)
        .unwrap_or(Value::Null);
    s.name = first_str(&profile, &["name"]).unwrap_or_default();
    s.email = first_str(&profile, &["email"]).unwrap_or_default();
    if let Some(id) = first_str(&profile, &["id"]) {
        s.id = format!("{}#{id}", s.host);
    }
    s
}

// ---------------------------------------------------------------------------
// Calls.

/// One client for every call, so connections (and their TLS) are reused.
pub(crate) fn agent() -> &'static ureq::Agent {
    static AGENT: OnceLock<ureq::Agent> = OnceLock::new();
    AGENT.get_or_init(|| {
        ureq::Agent::config_builder()
            .timeout_global(Some(Duration::from_secs(30)))
            .http_status_as_error(false)
            .user_agent(concat!("arakun/", env!("ARAKUN_VERSION")))
            .build()
            .into()
    })
}

/// Sends a request and reads up to `limit` bytes of the answer.
pub(crate) fn run(
    request: ureq::http::request::Builder,
    body: Option<String>,
    limit: u64,
) -> Result<(u16, Vec<u8>), Error> {
    let sent = match body {
        Some(b) => request
            .body(b)
            .map_err(|e| e.to_string())
            .and_then(|r| agent().run(r).map_err(|e| e.to_string())),
        None => request
            .body(())
            .map_err(|e| e.to_string())
            .and_then(|r| agent().run(r).map_err(|e| e.to_string())),
    };
    let mut response = sent.map_err(|e| Error::Failed(format!("Couldn't reach Doop: {e}")))?;
    let status = response.status().as_u16();
    let bytes = response
        .body_mut()
        .with_config()
        .limit(limit)
        .read_to_vec()
        .unwrap_or_default();
    Ok((status, bytes))
}

fn request(method: &str, params: Value) -> Value {
    json!({"jsonrpc": "2.0", "id": 1, "method": method, "params": params})
}

/// One JSON-RPC message to `<host>/mcp`. The token goes in the header only.
fn post(host: &str, token: &str, message: &Value) -> Result<(u16, String), Error> {
    let builder = ureq::http::Request::builder()
        .method("POST")
        .uri(mcp_url(host))
        .header("Authorization", format!("Bearer {token}"))
        .header("Content-Type", "application/json")
        .header("Accept", "application/json, text/event-stream");
    let (status, bytes) = run(builder, Some(message.to_string()), 32 * 1024 * 1024)?;
    Ok((status, String::from_utf8_lossy(&bytes).into_owned()))
}

/// The JSON-RPC answer in a body: plain JSON, or the one event of a stream
/// that carries a result or an error.
fn payload(text: &str) -> Option<Value> {
    if let Ok(v) = serde_json::from_str::<Value>(text) {
        return Some(v);
    }
    let mut data = String::new();
    let mut found = None;
    for line in text.lines().chain([""]) {
        if let Some(rest) = line.strip_prefix("data:") {
            if !data.is_empty() {
                data.push('\n');
            }
            data.push_str(rest.strip_prefix(' ').unwrap_or(rest));
        } else if line.is_empty() && !data.is_empty() {
            if let Ok(v) = serde_json::from_str::<Value>(&data)
                && (v.get("result").is_some() || v.get("error").is_some())
            {
                found = Some(v);
            }
            data.clear();
        }
    }
    found
}

/// Turns a status and body into the result, or the error the tab shows.
fn answer(host: &str, status: u16, text: &str) -> Result<Value, Error> {
    let v = payload(text).unwrap_or(Value::Null);
    let said = v["error"]["message"]
        .as_str()
        .or(v["error"].as_str())
        .map(str::to_string);
    match status {
        200..=299 => match said {
            Some(e) => Err(Error::Failed(e)),
            None if v.get("result").is_some() => Ok(v["result"].clone()),
            None => Err(Error::Failed(format!(
                "{} didn't answer as an MCP server.",
                host_label(host)
            ))),
        },
        401 | 403 => Err(Error::SignedOut(format!(
            "{} no longer takes this sign-in.",
            host_label(host)
        ))),
        404 | 405 => Err(Error::Failed(format!(
            "{} has no MCP endpoint. Is this a Doop instance?",
            host_label(host)
        ))),
        _ => Err(Error::Failed(said.unwrap_or_else(|| {
            format!("{} answered {status}.", host_label(host))
        }))),
    }
}

/// The token to send as `s`: the saved one, traded for a new one first when
/// it ran out or when the instance just refused `refused`.
fn token(s: &Session, refused: Option<&str>) -> Result<String, Error> {
    let _g = store_lock();
    let mut store = Store::read_from(&store_file());
    // Another call may have traded the token since `s` was read.
    let saved = store
        .sessions
        .iter()
        .find(|o| o.id == s.id)
        .cloned()
        .unwrap_or_else(|| s.clone());
    let Credential::OAuth { tokens } = &saved.credential else {
        return Ok(saved.credential.bearer().to_string());
    };
    let spent = tokens.expires_at <= crate::model::unix_now() + 60;
    if !spent && refused != Some(tokens.access.as_str()) {
        return Ok(tokens.access.clone());
    }
    let fresh = oauth::refresh(tokens)?;
    let access = fresh.access.clone();
    store.upsert(Session {
        credential: Credential::OAuth { tokens: fresh },
        ..saved
    });
    store.write_to(&store_file()).map_err(Error::Failed)?;
    Ok(access)
}

/// A JSON-RPC call as `s`. A refused OAuth token is traded once for a new one.
fn rpc(s: &Session, method: &str, params: Value) -> Result<Value, Error> {
    if let Some(reply) = fixture::reply(method, &params) {
        return Ok(reply);
    }
    let message = request(method, params);
    let first = token(s, None)?;
    let (status, text) = post(&s.host, &first, &message)?;
    if status == 401 && matches!(s.credential, Credential::OAuth { .. }) {
        let second = token(s, Some(&first))?;
        let (status, text) = post(&s.host, &second, &message)?;
        return answer(&s.host, status, &text);
    }
    answer(&s.host, status, &text)
}

// ---------------------------------------------------------------------------
// Tools.

/// A tool the instance's MCP server offers.
#[derive(Clone, Debug, PartialEq)]
pub struct Tool {
    pub name: String,
    pub description: String,
    /// Its `inputSchema`.
    pub schema: Value,
}

impl Tool {
    /// The name this tool gives an argument: the first of `wanted` its schema
    /// has (`canvas_id` also matches `canvasId`), else its only required one.
    fn arg(&self, wanted: &[&str]) -> String {
        let plain = |s: &str| s.replace(['_', '-'], "").to_ascii_lowercase();
        let props: Vec<&String> = self.schema["properties"]
            .as_object()
            .map(|o| o.keys().collect())
            .unwrap_or_default();
        for w in wanted {
            if let Some(p) = props.iter().find(|p| plain(p) == plain(w)) {
                return (*p).clone();
            }
        }
        match self.schema["required"].as_array().map(Vec::as_slice) {
            Some([only]) => only.as_str().unwrap_or(wanted[0]).to_string(),
            _ => wanted[0].to_string(),
        }
    }
}

/// Every tool of the instance.
pub fn tools(s: &Session) -> Result<Vec<Tool>, Error> {
    let mut out = Vec::new();
    let mut cursor: Option<String> = None;
    // A server pages its list; ten pages is far more than Doop has.
    for _ in 0..10 {
        let params = match &cursor {
            Some(c) => json!({"cursor": c}),
            None => json!({}),
        };
        let r = rpc(s, "tools/list", params)?;
        for t in r["tools"].as_array().into_iter().flatten() {
            if let Some(name) = t["name"].as_str() {
                out.push(Tool {
                    name: name.to_string(),
                    description: t["description"].as_str().unwrap_or_default().to_string(),
                    schema: t["inputSchema"].clone(),
                });
            }
        }
        cursor = r["nextCursor"].as_str().map(str::to_string);
        if cursor.is_none() {
            break;
        }
    }
    Ok(out)
}

/// What a tool answered: its JSON (or its text), and a picture if it sent one.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Answer {
    /// The structured answer when there is one, else the text's.
    pub value: Value,
    /// The JSON in the text, when it is not what `value` holds: Doop says
    /// more there (pages) than in the structured answer.
    pub text: Option<Value>,
    pub image: Option<Vec<u8>>,
}

fn tool_answer(r: &Value) -> Result<Answer, Error> {
    let mut texts = Vec::new();
    let mut image = None;
    for c in r["content"].as_array().into_iter().flatten() {
        match c["type"].as_str() {
            Some("text") => texts.extend(c["text"].as_str()),
            Some("image") if image.is_none() => {
                image = c["data"].as_str().and_then(|d| {
                    base64::engine::general_purpose::STANDARD
                        .decode(d.trim())
                        .ok()
                });
            }
            _ => {}
        }
    }
    let text = texts.join("\n");
    if r["isError"].as_bool() == Some(true) {
        return Err(Error::Failed(if text.is_empty() {
            "Doop couldn't do that.".into()
        } else {
            text
        }));
    }
    let in_text = serde_json::from_str::<Value>(&text).ok();
    let (value, text) = match r.get("structuredContent") {
        Some(v) if !v.is_null() => (v.clone(), in_text),
        _ => (in_text.unwrap_or(Value::String(text)), None),
    };
    Ok(Answer { value, text, image })
}

fn call_tool(s: &Session, tool: &str, args: Value) -> Result<Answer, Error> {
    tool_answer(&rpc(
        s,
        "tools/call",
        json!({"name": tool, "arguments": args}),
    )?)
}

fn find<'a>(tools: &'a [Tool], names: &[&str]) -> Option<&'a Tool> {
    names
        .iter()
        .find_map(|n| tools.iter().find(|t| t.name == *n))
}

/// The tool that lists the canvases, if this instance has one.
fn list_tool(tools: &[Tool]) -> Option<&Tool> {
    find(
        tools,
        &["list_canvases", "get_canvases", "list_canvas", "canvases"],
    )
    .or_else(|| {
        tools.iter().find(|t| {
            t.name.contains("canvases")
                && ["list", "get", "search"]
                    .iter()
                    .any(|p| t.name.starts_with(p))
        })
    })
}

// ---------------------------------------------------------------------------
// Canvases and frames.

/// A canvas by name, before its frames are read.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CanvasRef {
    pub id: String,
    #[serde(default)]
    pub name: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Frame {
    pub id: String,
    pub name: String,
    /// In canvas pixels, when the instance says.
    pub size: Option<(u32, u32)>,
    /// The id of the page it is on.
    pub page: Option<String>,
    /// Changes when the frame does: a render of another version is old.
    pub version: Option<String>,
}

/// A page of a canvas: a group of its frames.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Page {
    pub id: String,
    pub name: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Canvas {
    pub id: String,
    pub name: String,
    /// Empty when the instance has no pages, or does not say.
    pub pages: Vec<Page>,
    pub frames: Vec<Frame>,
    pub fetched_at: u64,
}

impl Canvas {
    /// The frames on `page`, or all of them without one.
    pub fn frames_on(&self, page: Option<&str>) -> Vec<&Frame> {
        self.frames
            .iter()
            .filter(|f| page.is_none() || f.page.as_deref() == page)
            .collect()
    }

    /// The pages that hold a frame, in their order.
    pub fn pages_in_use(&self) -> Vec<&Page> {
        self.pages
            .iter()
            .filter(|p| self.frames.iter().any(|f| f.page.as_ref() == Some(&p.id)))
            .collect()
    }
}

/// What tells one version of a frame from the next: the `v` of its picture's
/// address, else when it was last changed. Only characters a file name takes.
fn frame_version(f: &Value) -> Option<String> {
    let from_url = f["image_url"].as_str().and_then(|u| {
        u.split_once('?')?
            .1
            .split('&')
            .find_map(|p| p.strip_prefix("v="))
    });
    let raw = match from_url {
        Some(v) => v.to_string(),
        None => first_str(f, &["updatedAt", "updated_at", "version"])?,
    };
    let v: String = raw
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .take(40)
        .collect();
    (!v.is_empty()).then_some(v)
}

fn first_str(v: &Value, keys: &[&str]) -> Option<String> {
    keys.iter().find_map(|k| match &v[*k] {
        Value::String(s) if !s.trim().is_empty() => Some(s.trim().to_string()),
        Value::Number(n) => Some(n.to_string()),
        _ => None,
    })
}

fn first_list<'a>(v: &'a Value, keys: &[&str]) -> Option<&'a Vec<Value>> {
    v.as_array()
        .or_else(|| keys.iter().find_map(|k| v[*k].as_array()))
}

/// The canvases in a list tool's answer: a list, or one under a known key.
fn parse_canvas_list(v: &Value) -> Vec<CanvasRef> {
    let mut out: Vec<CanvasRef> = Vec::new();
    for c in first_list(v, &["canvases", "items", "data", "results"])
        .into_iter()
        .flatten()
    {
        let (id, name) = match c {
            Value::String(id) => (Some(id.clone()), None),
            _ => (
                first_str(c, &["id", "canvas_id", "canvasId", "slug"]),
                first_str(c, &["name", "title"]),
            ),
        };
        if let Some(id) = id
            && !out.iter().any(|o| o.id == id)
        {
            out.push(CanvasRef {
                id,
                name: name.unwrap_or_default(),
            });
        }
    }
    out
}

/// A canvas from `get_canvas`'s answer: an object with its frames, or just
/// the list of frames. `more` is the same canvas as the answer's text gave
/// it: what a frame lacks in `v` (its page) is taken from there.
fn parse_canvas(v: &Value, more: Option<&Value>, id: &str, now: u64) -> Canvas {
    let root = if v["canvas"].is_object() {
        &v["canvas"]
    } else {
        v
    };
    let keys = ["frames", "artboards"];
    let frames_of =
        |v: &'_ Value| -> Vec<Value> { first_list(v, &keys).cloned().unwrap_or_default() };
    let mut list = frames_of(root);
    if list.is_empty() {
        list = frames_of(v);
    }
    let extra = more.map(frames_of).unwrap_or_default();
    if list.is_empty() {
        list = extra.clone();
    }
    let id_of = |f: &Value| first_str(f, &["id", "frame_id", "frameId"]);
    let mut frames: Vec<Frame> = Vec::new();
    for f in &list {
        let Some(fid) = id_of(f) else {
            continue;
        };
        if frames.iter().any(|o| o.id == fid) {
            continue;
        }
        let twin = extra.iter().find(|e| id_of(e).as_ref() == Some(&fid));
        let side = |keys: &[&str]| {
            keys.iter()
                .find_map(|k| f[*k].as_f64().or(f["size"][*k].as_f64()))
                .filter(|n| *n >= 1. && *n <= 100_000.)
                .map(|n| n.round() as u32)
        };
        let page_keys = ["pageId", "page_id", "page"];
        frames.push(Frame {
            name: first_str(f, &["name", "title", "label"]).unwrap_or_else(|| fid.clone()),
            size: side(&["width", "w"]).zip(side(&["height", "h"])),
            page: first_str(f, &page_keys).or_else(|| first_str(twin?, &page_keys)),
            version: frame_version(f).or_else(|| frame_version(twin?)),
            id: fid,
        });
    }
    let mut pages: Vec<Page> = Vec::new();
    for source in [Some(root), Some(v), more].into_iter().flatten() {
        if !pages.is_empty() {
            break;
        }
        for p in source["pages"].as_array().into_iter().flatten() {
            if let Some(pid) = first_str(p, &["id"])
                && !pages.iter().any(|o| o.id == pid)
            {
                pages.push(Page {
                    name: first_str(p, &["name", "title"]).unwrap_or_else(|| pid.clone()),
                    id: pid,
                });
            }
        }
    }
    let name = [Some(root), more]
        .into_iter()
        .flatten()
        .find_map(|v| first_str(v, &["name", "title"]))
        .unwrap_or_default();
    Canvas {
        id: id.to_string(),
        name,
        pages,
        frames,
        fetched_at: now,
    }
}

/// The canvases this sign-in can see. None when the instance has no tool
/// that lists them: the tab then shows only the ones added by link.
pub fn canvases(s: &Session, tools: &[Tool]) -> Result<Option<Vec<CanvasRef>>, Error> {
    let Some(tool) = list_tool(tools) else {
        return Ok(None);
    };
    let got = call_tool(s, &tool.name, json!({}))?;
    Ok(Some(parse_canvas_list(&got.value)))
}

pub fn canvas(s: &Session, tools: &[Tool], id: &str) -> Result<Canvas, Error> {
    let tool = find(tools, &["get_canvas", "read_canvas"]).ok_or_else(|| {
        Error::Failed(format!(
            "The MCP server of {} has no get_canvas tool.",
            host_label(&s.host)
        ))
    })?;
    let arg = tool.arg(&["canvas_id", "canvas", "id"]);
    let got = call_tool(s, &tool.name, json!({ arg: id }))?;
    Ok(parse_canvas(
        &got.value,
        got.text.as_ref(),
        id,
        crate::model::unix_now(),
    ))
}

// ---------------------------------------------------------------------------
// Canvases added by link.

fn pins_file() -> PathBuf {
    dir().join("pins.json")
}

fn read_pins() -> BTreeMap<String, Vec<CanvasRef>> {
    std::fs::read(pins_file())
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
}

/// The canvases the user added to `host` by link.
pub fn pins(host: &str) -> Vec<CanvasRef> {
    read_pins().remove(host).unwrap_or_default()
}

/// Adds, renames (`Some`) or removes (`None`) a canvas added by link.
pub fn set_pin(host: &str, id: &str, pin: Option<CanvasRef>) {
    let _g = store_lock();
    let mut all = read_pins();
    let list = all.entry(host.to_string()).or_default();
    list.retain(|c| c.id != id);
    list.extend(pin);
    if list.is_empty() {
        all.remove(host);
    }
    if let Ok(json) = serde_json::to_vec_pretty(&all) {
        let _ = crate::durable::write(&pins_file(), &json);
    }
}

// ---------------------------------------------------------------------------
// Renders.

fn thumb_dir(host: &str) -> PathBuf {
    crate::platform::app_dir()
        .join("cache")
        .join("doop")
        .join(file_safe(host_label(host)))
}

/// Where a frame's render is kept. Its version is in the name, so a changed
/// frame is a new file and the old picture is never shown for it.
pub fn thumb_path(host: &str, frame: &Frame) -> PathBuf {
    let name = match &frame.version {
        Some(v) => format!("{}@{}.png", file_safe(&frame.id), file_safe(v)),
        None => format!("{}.png", file_safe(&frame.id)),
    };
    thumb_dir(host).join(name)
}

/// A frame's render already on disk, and whether it is due for a new
/// download: never when the frame says its version, after a day otherwise.
pub fn cached_thumb(host: &str, frame: &Frame) -> Option<(PathBuf, bool)> {
    let path = thumb_path(host, frame);
    let modified = std::fs::metadata(&path).ok()?.modified().ok()?;
    let due = frame.version.is_none()
        && std::time::SystemTime::now()
            .duration_since(modified)
            .is_ok_and(|age| age > THUMB_FRESH);
    Some((path, due))
}

/// Removes the renders of the frame's other versions.
fn drop_old_thumbs(kept: &Path, frame: &Frame) {
    let Some(dir) = kept.parent() else { return };
    let prefix = format!("{}@", file_safe(&frame.id));
    for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        let path = entry.path();
        let name = entry.file_name();
        if path != kept && name.to_string_lossy().starts_with(&prefix) {
            let _ = std::fs::remove_file(path);
        }
    }
}

fn is_image(bytes: &[u8]) -> bool {
    image::guess_format(bytes).is_ok() || bytes.starts_with(&[0xff, 0xd8])
}

fn get_image(url: &str, token: Option<&str>) -> Option<Vec<u8>> {
    let mut builder = ureq::http::Request::builder().method("GET").uri(url);
    if let Some(t) = token {
        builder = builder.header("Authorization", format!("Bearer {t}"));
    }
    let (status, bytes) = run(builder, None, 32 * 1024 * 1024).ok()?;
    ((200..=299).contains(&status) && is_image(&bytes)).then_some(bytes)
}

/// The picture as a PNG at most `THUMB_W` wide. One the decoder can't read
/// (a JPEG) is kept as it came: the app draws it from the file all the same.
fn shrink(bytes: Vec<u8>) -> Vec<u8> {
    let mut reader = image::ImageReader::new(std::io::Cursor::new(&bytes));
    reader.set_format(image::ImageFormat::Png);
    reader.limits(image::Limits::default());
    let Ok(picture) = reader.decode() else {
        return bytes;
    };
    if picture.width() <= THUMB_W {
        return bytes;
    }
    let h = (picture.height() as u64 * THUMB_W as u64 / picture.width() as u64).max(1) as u32;
    let small = picture.resize_exact(THUMB_W, h, image::imageops::FilterType::Triangle);
    let mut out = Vec::new();
    match small.write_to(&mut std::io::Cursor::new(&mut out), image::ImageFormat::Png) {
        Ok(()) => out,
        Err(_) => bytes,
    }
}

/// Downloads a frame's render into the cache: the public picture first, then
/// with the sign-in (an instance may keep its renders private), then through
/// the screenshot tool.
pub fn download_thumb(s: &Session, tools: &[Tool], frame: &Frame) -> Result<PathBuf, String> {
    let mut url = render_url(&s.host, &frame.id);
    if let Some(v) = &frame.version {
        // The instance's cache keeps a picture per address: name the version.
        url.push_str(&format!("?v={}", encode(v)));
    }
    // A test Arakun has no real sign-in to send: only the public picture.
    let signed = || {
        if fixture::on() {
            return None;
        }
        get_image(&url, Some(&token(s, None).ok()?)).or_else(|| {
            let tool = find(tools, &["get_frame_screenshot"])?;
            let arg = tool.arg(&["frame_id", "frame", "id"]);
            call_tool(s, &tool.name, json!({ arg: frame.id }))
                .ok()?
                .image
                .filter(|b| is_image(b))
        })
    };
    let bytes = get_image(&url, None).or_else(signed).ok_or("no render")?;
    let path = thumb_path(&s.host, frame);
    crate::durable::write(&path, &shrink(bytes)).map_err(|e| e.to_string())?;
    drop_old_thumbs(&path, frame);
    Ok(path)
}

// ---------------------------------------------------------------------------
// The agents' MCP config.

/// The agents the tab can connect an instance to.
pub const AGENTS: [AgentKind; 2] = [AgentKind::Claude, AgentKind::Droid];

/// What the agents call the server: `doop`, or `doop-design-acme-dev` for a
/// self-hosted instance, so several can sit side by side.
pub fn server_name(host: &str) -> String {
    if host == DEFAULT_HOST {
        return "doop".into();
    }
    let slug: String = host_label(host)
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    format!("doop-{}", slug.trim_matches('-'))
}

/// The server in `config` (Claude Code's `~/.claude.json`, Droid's
/// `~/.factory/mcp.json`) that points at this instance, if it is on.
fn server_in(config: &Value, host: &str) -> Option<String> {
    let url = mcp_url(host);
    config["mcpServers"]
        .as_object()?
        .iter()
        .find_map(|(name, s)| {
            (s["url"]
                .as_str()
                .is_some_and(|u| u.trim_end_matches('/') == url)
                && s["disabled"].as_bool() != Some(true))
            .then(|| name.clone())
        })
}

/// The name `agent` has this instance's MCP server under, if it has it.
pub fn agent_server(agent: AgentKind, host: &str) -> Option<String> {
    let home = crate::platform::home();
    let file = match agent {
        AgentKind::Claude => home.join(".claude.json"),
        AgentKind::Droid => home.join(".factory").join("mcp.json"),
        _ => return None,
    };
    let v: Value = serde_json::from_slice(&std::fs::read(file).ok()?).ok()?;
    server_in(&v, host)
}

/// What the Connect button runs in a terminal: add the server if needed, then
/// sign in. The agent signs in by itself, in the browser; Arakun's own
/// sign-in is never handed over.
pub fn connect_command(agent: AgentKind, host: &str, added: bool) -> Option<String> {
    let (name, url) = (server_name(host), mcp_url(host));
    match agent {
        // Claude Code signs in to HTTP servers from its /mcp menu.
        AgentKind::Claude if added => Some("claude /mcp".into()),
        AgentKind::Claude => Some(format!(
            "claude mcp add --scope user --transport http {name} {url} && claude /mcp"
        )),
        AgentKind::Droid if added => Some("droid mcp auth".into()),
        AgentKind::Droid => Some(format!(
            "droid mcp add {name} {url} --type http; droid mcp auth"
        )),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hosts_come_out_as_origins() {
        assert_eq!(normalize_host("").unwrap(), DEFAULT_HOST);
        assert_eq!(
            normalize_host(" Doop.Acme.dev/ ").unwrap(),
            "https://doop.acme.dev"
        );
        assert_eq!(
            normalize_host("https://doop.acme.dev/c/abc123?frame=x").unwrap(),
            "https://doop.acme.dev"
        );
        assert_eq!(
            normalize_host("https://doop.acme.dev:8443/mcp").unwrap(),
            "https://doop.acme.dev:8443"
        );
        assert_eq!(
            normalize_host("http://localhost:3000").unwrap(),
            "http://localhost:3000"
        );
        assert_eq!(
            normalize_host("http://192.168.1.20:3000/").unwrap(),
            "http://192.168.1.20:3000"
        );
        assert_eq!(
            normalize_host("http://doop.lan").unwrap(),
            "http://doop.lan"
        );
        assert!(normalize_host("http://doop.acme.dev").is_err());
        assert!(normalize_host("http://8.8.8.8").is_err());
        assert!(normalize_host("ftp://doop.acme.dev").is_err());
        assert!(normalize_host("https://user@doop.acme.dev").is_err());
        assert!(normalize_host("doop.acme.dev; rm -rf ~").is_err());
        assert!(normalize_host("https://").is_err());
    }

    #[test]
    fn canvas_links_name_the_instance_and_the_id() {
        assert_eq!(
            canvas_ref("https://doop.arakun.dev/c/bnj8ZjYM5d?frame=Nxj47aWtQb"),
            Some((Some(DEFAULT_HOST.into()), "bnj8ZjYM5d".into()))
        );
        assert_eq!(
            canvas_ref("doop.acme.dev/c/sEA9hKJMVj"),
            Some((Some("https://doop.acme.dev".into()), "sEA9hKJMVj".into()))
        );
        assert_eq!(
            canvas_ref(" bnj8ZjYM5d "),
            Some((None, "bnj8ZjYM5d".into()))
        );
        assert_eq!(canvas_ref("https://doop.arakun.dev/"), None);
        assert_eq!(canvas_ref("not a canvas"), None);
        assert_eq!(canvas_ref("https://doop.arakun.dev/c/"), None);
    }

    #[test]
    fn links_escape_what_the_instance_sent() {
        assert_eq!(
            render_url(DEFAULT_HOST, "Nxj47aWtQb"),
            "https://doop.arakun.dev/i/Nxj47aWtQb.png"
        );
        assert_eq!(
            canvas_url("https://d.lan", "a b", Some("../x")),
            "https://d.lan/c/a%20b?frame=..%2Fx"
        );
        let frame = |id: &str, version: Option<&str>| Frame {
            id: id.into(),
            name: String::new(),
            size: None,
            page: None,
            version: version.map(str::to_string),
        };
        let path = thumb_path("https://d.lan:8443", &frame("../../etc/passwd", None));
        assert_eq!(path.file_name().unwrap(), ".._.._etc_passwd.png");
        assert!(path.parent().unwrap().ends_with("doop/d.lan_8443"));
        let path = thumb_path("https://d.lan", &frame("Nxj47aWtQb", Some("1794856326580")));
        assert_eq!(path.file_name().unwrap(), "Nxj47aWtQb@1794856326580.png");
    }

    #[test]
    fn answers_are_read_from_json_and_from_a_stream() {
        let plain = r#"{"jsonrpc":"2.0","id":1,"result":{"tools":[]}}"#;
        assert_eq!(answer(DEFAULT_HOST, 200, plain), Ok(json!({"tools": []})));
        let stream = "event: message\ndata: {\"jsonrpc\":\"2.0\",\"method\":\"notifications/progress\"}\n\nevent: message\ndata: {\"jsonrpc\":\"2.0\",\"id\":1,\n data: \"result\":{\"ok\":true}}\n\n";
        // A `data:` line with a leading space is not a field: only real ones join.
        assert_eq!(payload(stream), None);
        let stream = "event: message\ndata: {\"jsonrpc\":\"2.0\",\"method\":\"notifications/progress\"}\n\nevent: message\ndata: {\"jsonrpc\":\"2.0\",\"id\":1,\ndata: \"result\":{\"ok\":true}}\n";
        assert_eq!(answer(DEFAULT_HOST, 200, stream), Ok(json!({"ok": true})));
        let refused =
            r#"{"jsonrpc":"2.0","error":{"code":-32001,"message":"Unauthorized"},"id":null}"#;
        assert!(matches!(
            answer(DEFAULT_HOST, 401, refused),
            Err(Error::SignedOut(_))
        ));
        assert_eq!(
            answer(DEFAULT_HOST, 200, r#"{"error":{"message":"Unknown tool"}}"#),
            Err(Error::Failed("Unknown tool".into()))
        );
        assert!(matches!(
            answer(DEFAULT_HOST, 200, "<!doctype html>"),
            Err(Error::Failed(_))
        ));
        assert!(matches!(
            answer("https://example.com", 404, "Not found"),
            Err(Error::Failed(m)) if m.contains("example.com")
        ));
    }

    #[test]
    fn tool_answers_give_json_text_or_a_picture() {
        let text = json!({"content": [{"type": "text", "text": "{\"frames\": []}"}]});
        assert_eq!(tool_answer(&text).unwrap().value, json!({"frames": []}));
        let prose = json!({"content": [{"type": "text", "text": "Use amber once."}]});
        assert_eq!(tool_answer(&prose).unwrap().value, json!("Use amber once."));
        let structured = json!({"content": [], "structuredContent": {"a": 1}});
        assert_eq!(tool_answer(&structured).unwrap().value, json!({"a": 1}));
        let both = json!({"content": [{"type": "text", "text": "{\"b\": 2}"}], "structuredContent": {"a": 1}});
        let got = tool_answer(&both).unwrap();
        assert_eq!(
            (got.value, got.text),
            (json!({"a": 1}), Some(json!({"b": 2})))
        );
        let picture =
            json!({"content": [{"type": "image", "data": "aGk=", "mimeType": "image/png"}]});
        assert_eq!(
            tool_answer(&picture).unwrap().image.as_deref(),
            Some(b"hi".as_slice())
        );
        let failed =
            json!({"isError": true, "content": [{"type": "text", "text": "No such canvas"}]});
        assert_eq!(
            tool_answer(&failed),
            Err(Error::Failed("No such canvas".into()))
        );
    }

    fn tool(name: &str, schema: Value) -> Tool {
        Tool {
            name: name.into(),
            description: String::new(),
            schema,
        }
    }

    #[test]
    fn arguments_are_named_from_the_schema() {
        let camel = tool(
            "get_canvas",
            json!({"properties": {"canvasId": {}, "depth": {}}}),
        );
        assert_eq!(camel.arg(&["canvas_id", "canvas", "id"]), "canvasId");
        let plain = tool("get_canvas", json!({"properties": {"id": {}}}));
        assert_eq!(plain.arg(&["canvas_id", "canvas", "id"]), "id");
        let other = tool(
            "get_canvas",
            json!({"properties": {"doc": {}, "depth": {}}, "required": ["doc"]}),
        );
        assert_eq!(other.arg(&["canvas_id", "canvas", "id"]), "doc");
        let none = tool("get_canvas", Value::Null);
        assert_eq!(none.arg(&["canvas_id", "canvas", "id"]), "canvas_id");
    }

    #[test]
    fn the_list_tool_is_found_by_name() {
        let named = [
            tool("get_canvas", Value::Null),
            tool("list_canvases", Value::Null),
        ];
        assert_eq!(list_tool(&named).unwrap().name, "list_canvases");
        let other = [tool("search_canvases", Value::Null)];
        assert_eq!(list_tool(&other).unwrap().name, "search_canvases");
        let without = [
            tool("get_canvas", Value::Null),
            tool("get_tokens", Value::Null),
        ];
        assert!(list_tool(&without).is_none());
    }

    #[test]
    fn canvas_lists_are_read_in_their_shapes() {
        let wrapped = json!({"canvases": [
            {"id": "bnj8ZjYM5d", "name": "Brandbook"},
            {"canvasId": "sEA9hKJMVj", "title": "App UI"},
            {"id": "bnj8ZjYM5d", "name": "Again"},
            {"name": "No id"}
        ]});
        assert_eq!(
            parse_canvas_list(&wrapped),
            vec![
                CanvasRef {
                    id: "bnj8ZjYM5d".into(),
                    name: "Brandbook".into()
                },
                CanvasRef {
                    id: "sEA9hKJMVj".into(),
                    name: "App UI".into()
                },
            ]
        );
        assert_eq!(
            parse_canvas_list(&json!(["a1b2", {"id": 7}])),
            vec![
                CanvasRef {
                    id: "a1b2".into(),
                    name: String::new()
                },
                CanvasRef {
                    id: "7".into(),
                    name: String::new()
                },
            ]
        );
        assert!(parse_canvas_list(&json!("No canvases yet.")).is_empty());
    }

    #[test]
    fn canvases_are_read_in_their_shapes() {
        let frame = |id: &str, name: &str, size: Option<(u32, u32)>| Frame {
            id: id.into(),
            name: name.into(),
            size,
            page: None,
            version: None,
        };
        let object = json!({"name": "Brandbook", "frames": [
            {"id": "Nxj47aWtQb", "name": "01 Cover", "width": 1440, "height": 900.4},
            {"id": "RybwtRXV2z", "title": "01 Cover · light", "size": {"width": 1440, "height": 900}},
            {"name": "No id"},
            {"id": "Nxj47aWtQb"}
        ]});
        let c = parse_canvas(&object, None, "bnj8ZjYM5d", 5);
        assert_eq!(
            (c.id.as_str(), c.name.as_str()),
            ("bnj8ZjYM5d", "Brandbook")
        );
        assert_eq!(
            c.frames,
            vec![
                frame("Nxj47aWtQb", "01 Cover", Some((1440, 900))),
                frame("RybwtRXV2z", "01 Cover · light", Some((1440, 900))),
            ]
        );
        assert!(c.pages.is_empty());
        assert_eq!(c.frames_on(None).len(), 2);
        let nested = json!({"canvas": {"title": "App UI"}, "frames": [{"frameId": "f1"}]});
        let c = parse_canvas(&nested, None, "x", 0);
        assert_eq!(c.name, "App UI");
        assert_eq!(c.frames, vec![frame("f1", "f1", None)]);
        let list = json!([{"id": "f1", "name": "One"}]);
        assert_eq!(parse_canvas(&list, None, "x", 0).frames.len(), 1);
        assert!(
            parse_canvas(&json!("nothing"), None, "x", 0)
                .frames
                .is_empty()
        );
    }

    #[test]
    fn a_canvas_takes_pages_and_versions_from_both_answers() {
        // What doop.arakun.dev sends: the structured answer has the pictures'
        // versions, the text has the pages.
        let structured = json!({
            "canvas": {"id": "bnj8ZjYM5d", "name": "Brandbook"},
            "frames": [
                {"id": "BwHrX9iWmy", "name": "Button", "width": 240, "height": 80,
                 "image_url": "https://doop.arakun.dev/i/BwHrX9iWmy.png?scale=1&v=1794856326580"},
                {"id": "Nxj47aWtQb", "name": "01 Cover", "width": 1440, "height": 900,
                 "image_url": "https://doop.arakun.dev/i/Nxj47aWtQb.png?scale=1"}
            ]
        });
        let text = json!({
            "id": "bnj8ZjYM5d",
            "name": "Brandbook",
            "pages": [
                {"id": "p1", "name": "Brand book"},
                {"id": "p2", "name": "Components"},
                {"id": "p3", "name": "Empty"}
            ],
            "frames": [
                {"id": "BwHrX9iWmy", "pageId": "p2", "updatedAt": "2026-11-16T19:12:06.580Z"},
                {"id": "Nxj47aWtQb", "pageId": "p1", "updatedAt": "2026-11-16T19:12:06.581Z"}
            ]
        });
        let c = parse_canvas(&structured, Some(&text), "bnj8ZjYM5d", 0);
        assert_eq!(c.name, "Brandbook");
        assert_eq!(c.frames[0].page.as_deref(), Some("p2"));
        assert_eq!(c.frames[0].version.as_deref(), Some("1794856326580"));
        assert_eq!(c.frames[1].version.as_deref(), Some("20261116T191206581Z"));
        let used: Vec<&str> = c.pages_in_use().iter().map(|p| p.name.as_str()).collect();
        assert_eq!(used, ["Brand book", "Components"]);
        let on: Vec<&str> = c
            .frames_on(Some("p1"))
            .iter()
            .map(|f| f.name.as_str())
            .collect();
        assert_eq!(on, ["01 Cover"]);
        // The text alone is enough.
        let c = parse_canvas(&text, None, "bnj8ZjYM5d", 0);
        assert_eq!((c.pages.len(), c.frames.len()), (3, 2));
        assert_eq!(c.frames[0].page.as_deref(), Some("p2"));
    }

    #[test]
    fn sessions_are_replaced_by_id_and_hide_their_secrets() {
        let s = |id: &str, key: &str| Session {
            id: id.into(),
            host: DEFAULT_HOST.into(),
            email: String::new(),
            name: String::new(),
            credential: Credential::AgentKey { key: key.into() },
            added_at: 1,
        };
        let mut store = Store::default();
        store.upsert(s("a", "one"));
        store.upsert(s("b", "two"));
        store.upsert(s("a", "three"));
        assert_eq!(store.sessions.len(), 2);
        assert_eq!(store.sessions[0].credential.bearer(), "three");
        assert!(!format!("{:?}", store).contains("three"));
        assert!(store.remove("a"));
        assert!(!store.remove("a"));
        let back: Store = serde_json::from_str(&serde_json::to_string(&store).unwrap()).unwrap();
        assert_eq!(back, store);
    }

    #[test]
    fn agents_get_a_server_per_instance() {
        assert_eq!(server_name(DEFAULT_HOST), "doop");
        assert_eq!(
            server_name("https://design.acme.dev:8443"),
            "doop-design-acme-dev-8443"
        );
        assert_eq!(
            connect_command(AgentKind::Claude, "https://d.acme.dev", false).unwrap(),
            "claude mcp add --scope user --transport http doop-d-acme-dev https://d.acme.dev/mcp && claude /mcp"
        );
        assert_eq!(
            connect_command(AgentKind::Droid, DEFAULT_HOST, true).unwrap(),
            "droid mcp auth"
        );
        assert_eq!(connect_command(AgentKind::Codex, DEFAULT_HOST, false), None);
        let config = json!({"mcpServers": {
            "design": {"type": "http", "url": "https://d.acme.dev/mcp/"},
            "off": {"url": "https://doop.arakun.dev/mcp", "disabled": true},
            "local": {"command": "npx"}
        }});
        assert_eq!(
            server_in(&config, "https://d.acme.dev").as_deref(),
            Some("design")
        );
        assert_eq!(server_in(&config, DEFAULT_HOST), None);
    }
}
