//! Agents ask the user for a secret (a token, a password, a pairing code) with
//! a card in the chat instead of in the chat itself, where the value would sit
//! in the transcript, the prompt history, the title and on paired devices.
//!
//! The agent calls `request_secret` on Arakun's MCP server (`arakun mcp`, see
//! [`mcp_reply`]). That process forwards the call over the control socket with
//! the token of the chat it was started for; [`ask`] hands it to that chat's
//! [`Route`], and the chat shows the card. The agent gets back only the outcome
//! and, for a saved secret, a `secret-ref:` handle per field, never the value.
//!
//! Under `arakun server` the call goes to the server's local socket instead
//! ([`serve_from`]), and the card goes to the devices that watch the session
//! (`phone::wire::secret`). They send what was typed back only over a private
//! path ([`answer`]): the relay, Tailscale or https, never plain http.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use futures::channel::{mpsc, oneshot};
use serde_json::{Value, json};

use super::AgentKind;

/// The MCP server's name, as agents show its tools (`mcp__arakun__…`).
pub const SERVER: &str = "arakun";
pub const TOOL: &str = "request_secret";
/// Names the chat a call came from, in the MCP server's environment.
pub const TOKEN_VAR: &str = "ARAKUN_SECRET_TOKEN";
/// The local socket of the `arakun server` that started the MCP server, in
/// its environment. Without it, calls go to the app's control socket.
pub const SERVER_SOCKET_VAR: &str = "ARAKUN_SERVER_SOCKET";
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(10 * 60);
const MIN_TIMEOUT: Duration = Duration::from_secs(30);
const MAX_TIMEOUT: Duration = Duration::from_secs(60 * 60);
pub const MAX_FIELDS: usize = 4;
/// How long a saved value waits to be redeemed.
const KEPT_FOR: Duration = Duration::from_secs(24 * 60 * 60);
const HANDLE_PREFIX: &str = "secret-ref:";

/// One masked input on the card.
#[derive(Clone, Debug, PartialEq)]
pub struct Field {
    /// The key the agent gets the handle under.
    pub name: String,
    pub label: String,
}

/// What the agent asked for.
#[derive(Clone, Debug, PartialEq)]
pub struct SecretRequest {
    pub id: u64,
    /// What the secret is ("GitHub token").
    pub label: String,
    /// Why the agent needs it.
    pub reason: String,
    /// Where the user signs in or makes one.
    pub url: Option<String>,
    pub fields: Vec<Field>,
    pub timeout: Duration,
}

impl SecretRequest {
    /// The tool's arguments, checked. The error goes back to the agent.
    pub fn parse(args: &Value) -> Result<Self, String> {
        let text = |k: &str, max: usize| -> Result<Option<String>, String> {
            match &args[k] {
                Value::Null => Ok(None),
                Value::String(s) if s.chars().count() > max => {
                    Err(format!("`{k}` is longer than {max} characters"))
                }
                Value::String(s) => Ok(Some(s.trim().to_string()).filter(|s| !s.is_empty())),
                _ => Err(format!("`{k}` must be a string")),
            }
        };
        let label = text("label", 80)?.ok_or("`label` is required: what the secret is")?;
        let reason = text("reason", 400)?.ok_or("`reason` is required: why you need it")?;
        let url = text("url", 2000)?;
        if let Some(u) = &url
            && !(u.starts_with("https://") || u.starts_with("http://"))
        {
            return Err("`url` must start with https:// or http://".into());
        }
        let fields = match &args["fields"] {
            Value::Null => vec![Field {
                name: "value".into(),
                label: label.clone(),
            }],
            Value::Array(list) if list.is_empty() || list.len() > MAX_FIELDS => {
                return Err(format!("`fields` takes 1 to {MAX_FIELDS} fields"));
            }
            Value::Array(list) => {
                let mut fields: Vec<Field> = Vec::new();
                for f in list {
                    let name = f["name"].as_str().map(str::trim).unwrap_or_default();
                    let ok = !name.is_empty()
                        && name.len() <= 40
                        && name
                            .chars()
                            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-');
                    if !ok {
                        return Err(
                            "each field needs a `name` of letters, digits, `_` or `-`".into()
                        );
                    }
                    if fields.iter().any(|g| g.name == name) {
                        return Err(format!("the field `{name}` is there twice"));
                    }
                    let label = f["label"]
                        .as_str()
                        .map(str::trim)
                        .filter(|l| !l.is_empty())
                        .unwrap_or(name);
                    fields.push(Field {
                        name: name.to_string(),
                        label: label.chars().take(60).collect(),
                    });
                }
                fields
            }
            _ => return Err("`fields` must be a list of {name, label}".into()),
        };
        let timeout = match &args["timeout_secs"] {
            Value::Null => DEFAULT_TIMEOUT,
            v => Duration::from_secs(v.as_u64().ok_or("`timeout_secs` must be a number")?)
                .clamp(MIN_TIMEOUT, MAX_TIMEOUT),
        };
        Ok(Self {
            id: 0,
            label,
            reason,
            url,
            fields,
            timeout,
        })
    }
}

impl SecretRequest {
    /// As the devices that watch a server's session get it. `private` says
    /// whether every connection from them to the agent is encrypted, so what
    /// they type may be sent; the card offers only Decline otherwise.
    pub fn to_wire(&self, private: bool) -> Value {
        json!({
            "id": self.id,
            "label": self.label,
            "reason": self.reason,
            "url": self.url,
            "fields": self.fields.iter().map(|f| json!({"name": f.name, "label": f.label})).collect::<Vec<_>>(),
            "timeout_secs": self.timeout.as_secs(),
            "private": private,
        })
    }

    /// [`SecretRequest::to_wire`] read back by the app, and whether it is private.
    /// A server words it, so it is checked as an agent's request is.
    pub fn from_wire(v: &Value) -> Option<(Self, bool)> {
        let id = v["id"].as_u64()?;
        let mut request = Self::parse(v).ok()?;
        request.id = id;
        Some((request, v["private"] == true))
    }
}

/// Why a request ended without an answer.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Why {
    Stopped,
    NewMessage,
    TimedOut,
    TurnEnded,
    Closed,
}

impl Why {
    const ALL: [Why; 5] = [
        Why::Stopped,
        Why::NewMessage,
        Why::TimedOut,
        Why::TurnEnded,
        Why::Closed,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Why::Stopped => "the user stopped the run",
            Why::NewMessage => "the user sent a new message",
            Why::TimedOut => "it timed out",
            Why::TurnEnded => "the turn ended",
            Why::Closed => "the session closed",
        }
    }

    fn parse(s: &str) -> Self {
        Self::ALL
            .into_iter()
            .find(|w| w.as_str() == s)
            .unwrap_or(Why::Closed)
    }
}

/// How a request ended. Only this goes back to the agent and into the transcript.
#[derive(Clone, Debug, PartialEq)]
pub enum Outcome {
    /// A handle per field, in the order the agent asked.
    Saved(Vec<(String, String)>),
    Declined,
    Cancelled(Why),
}

impl Outcome {
    pub fn to_json(&self) -> Value {
        match self {
            Outcome::Saved(handles) => {
                let handles: serde_json::Map<String, Value> = handles
                    .iter()
                    .map(|(name, h)| (name.clone(), Value::from(h.as_str())))
                    .collect();
                json!({"outcome": "saved", "handles": handles})
            }
            Outcome::Declined => json!({"outcome": "declined"}),
            Outcome::Cancelled(why) => json!({"outcome": "cancelled", "reason": why.as_str()}),
        }
    }

    /// [`Outcome::to_json`] read back, as the MCP server gets it over the socket.
    pub fn from_json(v: &Value) -> Option<Self> {
        Some(match v["outcome"].as_str()? {
            "saved" => Outcome::Saved(
                v["handles"]
                    .as_object()?
                    .iter()
                    .map(|(k, h)| Some((k.clone(), h.as_str()?.to_string())))
                    .collect::<Option<_>>()?,
            ),
            "declined" => Outcome::Declined,
            "cancelled" => Outcome::Cancelled(Why::parse(v["reason"].as_str().unwrap_or(""))),
            _ => return None,
        })
    }

    /// What the agent reads.
    pub fn text(&self) -> String {
        match self {
            Outcome::Saved(handles) => {
                let list = handles
                    .iter()
                    .map(|(name, h)| format!("{name}: {h}"))
                    .collect::<Vec<_>>()
                    .join("\n");
                format!(
                    "The user saved it. These are handles, not the values; only Arakun's own tools redeem them, once each, within 24 hours:\n{list}"
                )
            }
            Outcome::Declined => "The user declined. Do not ask for this secret in a message; go on without it, or stop and say what you need.".into(),
            Outcome::Cancelled(why) => format!(
                "The request was cancelled: {}. Do not ask for this secret in a message.",
                why.as_str()
            ),
        }
    }
}

/// What the user typed into a card, one value per field, on its way from a
/// device or the card to [`keep`]. Its `Debug` hides the values, so a logged
/// message does not show them.
#[derive(Clone, PartialEq, serde::Deserialize, serde::Serialize)]
#[serde(transparent)]
pub struct Typed(pub Vec<String>);

impl std::fmt::Debug for Typed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Typed({} hidden)", self.0.len())
    }
}

/// Why typed values are refused: the path they would take, or took, is not
/// encrypted. Until the LAN path is (ARAKUN-150), only the relay, Tailscale
/// and https carry them.
pub const NOT_PRIVATE: &str = "This connection is not encrypted, so the secret can't be sent over it. Decline, or answer from a device that reaches the computer through your account or Tailscale.";

/// What a device's answer comes to: `None` declines. Values that came, or
/// would go, over a path that is not `private` are refused.
pub fn answer(values: Option<Typed>, private: bool) -> Result<Option<Typed>, &'static str> {
    match values {
        Some(_) if !private => Err(NOT_PRIVATE),
        v => Ok(v),
    }
}

/// A request on its way to a chat, with the way back to the agent.
pub struct Asked {
    pub request: SecretRequest,
    pub reply: oneshot::Sender<Outcome>,
}

static ROUTES: Mutex<Option<HashMap<String, mpsc::UnboundedSender<Asked>>>> = Mutex::new(None);
static NEXT_ID: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

/// A chat's address for requests. Dropping it closes the address, so a call
/// that comes later fails instead of waiting for nobody.
pub struct Route {
    token: String,
}

impl Route {
    /// A new address and where its requests arrive. `None` when the OS has no
    /// random bytes for the token.
    pub fn open() -> Option<(Self, mpsc::UnboundedReceiver<Asked>)> {
        let token = random_hex(16)?;
        let (tx, rx) = mpsc::unbounded();
        ROUTES
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get_or_insert_with(HashMap::new)
            .insert(token.clone(), tx);
        Some((Self { token }, rx))
    }

    pub fn token(&self) -> &str {
        &self.token
    }
}

impl Drop for Route {
    fn drop(&mut self) {
        if let Some(routes) = ROUTES.lock().unwrap_or_else(|e| e.into_inner()).as_mut() {
            routes.remove(&self.token);
        }
    }
}

/// Hands a `request_secret` call to the chat `token` names. The receiver gets
/// the outcome; the error is for the agent.
pub fn ask(token: &str, args: &Value) -> Result<oneshot::Receiver<Outcome>, String> {
    let mut request = SecretRequest::parse(args)?;
    request.id = NEXT_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let routes = ROUTES.lock().unwrap_or_else(|e| e.into_inner());
    let tx = routes
        .as_ref()
        .and_then(|r| r.get(token))
        .ok_or("this session is not open in Arakun")?;
    let (reply, rx) = oneshot::channel();
    tx.unbounded_send(Asked { request, reply })
        .map_err(|_| "this session is not open in Arakun".to_string())?;
    Ok(rx)
}

/// How Arakun's MCP server is started: the command, its arguments and its
/// environment. The app gives it through [`crate::host::Host::secret_mcp`].
#[derive(Clone, Debug, PartialEq)]
pub struct McpCommand {
    pub program: PathBuf,
    pub args: Vec<String>,
    pub env: Vec<(String, String)>,
}

static SERVER_SOCKET: OnceLock<PathBuf> = OnceLock::new();

/// Sends the calls of this process's sessions to `arakun server`'s local
/// socket at `socket`, which answers `secret` ([`ask`]), instead of to the app.
/// Set once, when the server starts.
pub fn serve_from(socket: &Path) {
    let _ = SERVER_SOCKET.set(socket.to_path_buf());
}

fn server_socket() -> Option<&'static Path> {
    SERVER_SOCKET.get().map(PathBuf::as_path)
}

impl McpCommand {
    /// The server for the chat whose [`Route`] has `token`. `None` when this
    /// program has no MCP server (the examples), or when a sandboxed agent
    /// could not reach `arakun server`'s socket.
    pub fn for_chat(token: &str, sandboxed: bool) -> Option<Self> {
        // Codex starts MCP servers with only a few of its own variables.
        let mut env = vec![(TOKEN_VAR.to_string(), token.to_string())];
        if let Some(socket) = server_socket() {
            // The sandbox shows the app's control socket only.
            if sandboxed {
                return None;
            }
            env.push((SERVER_SOCKET_VAR.into(), socket.display().to_string()));
            // `arakun mcp` and `arakun-server mcp` both serve it.
            let program = std::env::current_exe().ok()?;
            return Some(Self {
                program,
                args: vec!["mcp".into()],
                env,
            });
        }
        let (program, args) = (crate::host::get().secret_mcp)()?;
        // A sandboxed agent reaches arakun through the sandbox's socket.
        let socket = if sandboxed {
            Some((crate::host::get().sandbox_socket)())
        } else {
            std::env::var_os("ARAKUN_SOCKET").map(PathBuf::from)
        };
        if let Some(socket) = socket {
            env.push(("ARAKUN_SOCKET".into(), socket.display().to_string()));
        }
        Some(Self { program, args, env })
    }

    /// As ACP's `mcpServers` lists it.
    pub fn acp(&self) -> Value {
        json!({
            "name": SERVER,
            "command": self.program,
            "args": self.args,
            "env": self.env.iter().map(|(k, v)| json!({"name": k, "value": v})).collect::<Vec<_>>(),
        })
    }

    fn env_object(&self) -> serde_json::Map<String, Value> {
        self.env
            .iter()
            .map(|(k, v)| (k.clone(), Value::from(v.as_str())))
            .collect()
    }

    /// As Droid's `initialize_session` and `load_session` list it.
    pub fn droid(&self) -> Value {
        json!({
            "name": SERVER,
            "command": self.program,
            "args": self.args,
            "env": self.env_object(),
        })
    }

    /// As Codex's `mcp_servers.<name>` table. Codex ends a tool call after 60
    /// seconds unless told otherwise, and the card waits longer.
    pub fn codex(&self) -> Value {
        json!({
            "command": self.program,
            "args": self.args,
            "env": self.env_object(),
            "tool_timeout_sec": MAX_TIMEOUT.as_secs() + 60,
        })
    }
}

/// Whether agents of `kind` get the tool here, so their instructions name it.
/// The others take no MCP servers from arakun, or have no program to run it.
pub fn offered_to(kind: AgentKind) -> bool {
    matches!(
        kind,
        AgentKind::Claude | AgentKind::Codex | AgentKind::Droid
    ) && (server_socket().is_some() || (crate::host::get().secret_mcp)().is_some())
}

/// Told to the agents that have the tool.
pub const HINT: &str = "- Secrets: when only the user can give a login, token, PIN or password, call the `request_secret` tool (Arakun's MCP server); it shows the user a private card and gives you a handle, never the value. Never ask for a secret in a message, and never put one in a task or prompt for another agent.\n";

// ---- the values ------------------------------------------------------------

struct Kept {
    value: String,
    cwd: PathBuf,
    until: Instant,
}

static KEPT: Mutex<Option<HashMap<String, Kept>>> = Mutex::new(None);

/// Keeps what the user saved, for the project in `cwd`, and returns a handle
/// per field. On an error nothing is kept, so the card can try again.
pub fn keep(cwd: &Path, values: Vec<(String, String)>) -> Result<Vec<(String, String)>, String> {
    let now = Instant::now();
    let mut handles = Vec::with_capacity(values.len());
    let mut fresh = Vec::with_capacity(values.len());
    for (name, value) in values {
        let handle = random_hex(16)
            .map(|h| format!("{HANDLE_PREFIX}{h}"))
            .ok_or("the system gave no random bytes")?;
        handles.push((name, handle.clone()));
        fresh.push((
            handle,
            Kept {
                value,
                cwd: cwd.to_path_buf(),
                until: now + KEPT_FOR,
            },
        ));
    }
    let mut kept = KEPT.lock().unwrap_or_else(|e| e.into_inner());
    let kept = kept.get_or_insert_with(HashMap::new);
    kept.retain(|_, k| k.until > now);
    kept.extend(fresh);
    Ok(handles)
}

/// The value behind `handle`, once, for the project it was saved for. It is
/// gone from the store before it is handed out.
pub fn take(handle: &str, cwd: &Path) -> Option<String> {
    let mut kept = KEPT.lock().unwrap_or_else(|e| e.into_inner());
    let kept = kept.as_mut()?;
    let k = kept.get(handle)?;
    if k.cwd != cwd {
        return None;
    }
    let k = kept.remove(handle)?;
    (k.until > Instant::now()).then_some(k.value)
}

fn random_hex(bytes: usize) -> Option<String> {
    let mut buf = vec![0u8; bytes];
    getrandom::fill(&mut buf).ok()?;
    Some(buf.iter().map(|b| format!("{b:02x}")).collect())
}

// ---- the MCP server ----------------------------------------------------------

fn tool() -> Value {
    json!({
        "name": TOOL,
        "title": "Ask the user for a secret",
        "description": "Ask the user for a secret only they can give (a login, API token, PIN, password or pairing code) with a private card in Arakun. Never ask for a secret in a message. The call waits until the user saves or declines, the run stops, or the timeout passes. It returns the outcome and, when saved, a `secret-ref:` handle per field, never the value. Never put a secret or a handle in a task for another agent.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "label": {"type": "string", "description": "What the secret is, as the card's title, such as \"GitHub token\"."},
                "reason": {"type": "string", "description": "Why you need it, in one or two plain sentences."},
                "url": {"type": "string", "description": "Where the user signs in or makes the secret, if there is such a page."},
                "fields": {
                    "type": "array",
                    "maxItems": MAX_FIELDS,
                    "description": "The inputs, when there is more than one (a user name and a password). One field named `value` when left out.",
                    "items": {
                        "type": "object",
                        "properties": {
                            "name": {"type": "string", "description": "Letters, digits, `_` or `-`; the handle comes back under it."},
                            "label": {"type": "string", "description": "What the input is called on the card."}
                        },
                        "required": ["name"]
                    }
                },
                "timeout_secs": {"type": "integer", "description": "How long the card stays open, in seconds. 600 when left out."}
            },
            "required": ["label", "reason"]
        }
    })
}

/// Serves MCP on stdin and stdout until stdin closes. `forward` takes a
/// `request_secret` call to whoever shows the card, with the chat's token from
/// [`TOKEN_VAR`], and waits for its outcome.
pub fn serve_stdio<F>(forward: F) -> i32
where
    F: Fn(&str, &Value) -> Result<Value, String> + Clone + Send + 'static,
{
    use std::io::{BufRead as _, Write as _};
    use std::sync::Arc;

    let token = std::env::var(TOKEN_VAR).unwrap_or_default();
    let out = Arc::new(Mutex::new(std::io::stdout()));
    let write = |out: &Mutex<std::io::Stdout>, v: Value| {
        let mut out = out.lock().unwrap_or_else(|e| e.into_inner());
        let _ = writeln!(out, "{v}");
        let _ = out.flush();
    };
    for line in std::io::stdin().lock().lines() {
        let Ok(line) = line else { break };
        if line.trim().is_empty() {
            continue;
        }
        // A call waits for the user; pings and lists must not wait behind it.
        let is_call = serde_json::from_str::<Value>(&line)
            .is_ok_and(|v| v["method"] == "tools/call" && v.get("id").is_some());
        if is_call {
            let (out, token, forward) = (out.clone(), token.clone(), forward.clone());
            std::thread::spawn(move || {
                let call = |args: &Value| {
                    if token.is_empty() {
                        return Err(
                            "Arakun did not start this server for a session, so it has no card to show".into(),
                        );
                    }
                    forward(&token, args)
                };
                if let Some(reply) = mcp_reply(&line, call) {
                    write(&out, reply);
                }
            });
        } else if let Some(reply) = mcp_reply(&line, |_| Err(String::new())) {
            write(&out, reply);
        }
    }
    0
}

/// How long the MCP server waits for a call's outcome: a little longer than
/// the card stays open.
pub fn call_wait(args: &Value) -> Result<Duration, String> {
    Ok(SecretRequest::parse(args)?.timeout + Duration::from_secs(60))
}

/// The MCP server's answer to one JSON-RPC line, if it needs one. `call` runs
/// a `request_secret` call: Ok with the outcome's JSON, or the error for the agent.
pub fn mcp_reply(line: &str, call: impl FnOnce(&Value) -> Result<Value, String>) -> Option<Value> {
    let msg: Value = match serde_json::from_str(line) {
        Ok(v) => v,
        Err(e) => {
            return Some(json!({"jsonrpc": "2.0", "id": null,
                "error": {"code": -32700, "message": format!("parse error: {e}")}}));
        }
    };
    let id = msg.get("id").cloned()?;
    let params = &msg["params"];
    let ok = |result: Value| Some(json!({"jsonrpc": "2.0", "id": id, "result": result}));
    match msg["method"].as_str().unwrap_or_default() {
        "initialize" => ok(json!({
            "protocolVersion": params["protocolVersion"].as_str().unwrap_or("2025-06-18"),
            "capabilities": {"tools": {}},
            "serverInfo": {"name": SERVER, "title": "Arakun", "version": env!("CARGO_PKG_VERSION")},
        })),
        "ping" => ok(json!({})),
        "tools/list" => ok(json!({"tools": [tool()]})),
        "tools/call" if params["name"] == TOOL => {
            let args = params.get("arguments").cloned().unwrap_or(json!({}));
            match call(&args) {
                Ok(outcome) => {
                    let text = Outcome::from_json(&outcome)
                        .map_or_else(|| outcome.to_string(), |o| o.text());
                    ok(json!({
                        "content": [{"type": "text", "text": text}],
                        "structuredContent": outcome,
                        "isError": false,
                    }))
                }
                Err(e) => ok(json!({
                    "content": [{"type": "text", "text": e}],
                    "isError": true,
                })),
            }
        }
        "tools/call" => Some(json!({"jsonrpc": "2.0", "id": id,
            "error": {"code": -32602, "message": format!("unknown tool {}", params["name"])}})),
        other => Some(json!({"jsonrpc": "2.0", "id": id,
            "error": {"code": -32601, "message": format!("method not found: {other}")}})),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_request_needs_a_label_and_a_reason() {
        assert!(SecretRequest::parse(&json!({"reason": "r"})).is_err());
        assert!(SecretRequest::parse(&json!({"label": "Token"})).is_err());
        let r = SecretRequest::parse(&json!({"label": " Token ", "reason": "To deploy."})).unwrap();
        assert_eq!(r.label, "Token");
        assert_eq!(r.timeout, DEFAULT_TIMEOUT);
        assert_eq!(
            r.fields,
            vec![Field {
                name: "value".into(),
                label: "Token".into()
            }]
        );
    }

    #[test]
    fn a_request_checks_its_fields_link_and_timeout() {
        let base = |extra: Value| {
            let mut v = json!({"label": "Login", "reason": "r"});
            v.as_object_mut()
                .unwrap()
                .extend(extra.as_object().unwrap().clone());
            SecretRequest::parse(&v)
        };
        assert!(base(json!({"fields": []})).is_err());
        let five: Vec<Value> = (0..5).map(|i| json!({"name": format!("f{i}")})).collect();
        assert!(base(json!({"fields": five})).is_err());
        assert!(base(json!({"fields": [{"name": "a b"}]})).is_err());
        assert!(base(json!({"fields": [{"name": "a"}, {"name": "a"}]})).is_err());
        assert!(base(json!({"url": "javascript:alert(1)"})).is_err());
        let r = base(json!({
            "url": "https://github.com/settings/tokens",
            "fields": [{"name": "user"}, {"name": "password", "label": "Password"}],
            "timeout_secs": 5,
        }))
        .unwrap();
        assert_eq!(r.fields[0].label, "user", "the name stands in for a label");
        assert_eq!(r.fields[1].label, "Password");
        assert_eq!(r.timeout, MIN_TIMEOUT);
        assert_eq!(
            base(json!({"timeout_secs": 999_999})).unwrap().timeout,
            MAX_TIMEOUT
        );
    }

    #[test]
    fn an_outcome_survives_the_socket() {
        for o in [
            Outcome::Saved(vec![("token".into(), "secret-ref:ab".into())]),
            Outcome::Declined,
            Outcome::Cancelled(Why::TimedOut),
            Outcome::Cancelled(Why::Stopped),
        ] {
            assert_eq!(Outcome::from_json(&o.to_json()), Some(o));
        }
        assert_eq!(Outcome::from_json(&json!({"outcome": "?"})), None);
    }

    #[test]
    fn typed_values_go_only_over_a_private_path() {
        let typed = || Some(Typed(vec!["hunter2".into()]));
        assert_eq!(answer(typed(), false), Err(NOT_PRIVATE));
        assert_eq!(answer(typed(), true), Ok(typed()));
        // Declining sends nothing, so any path may carry it.
        assert_eq!(answer(None, false), Ok(None));
        assert!(!format!("{:?}", typed()).contains("hunter2"));
    }

    #[test]
    fn a_request_survives_the_wire() {
        let mut r = SecretRequest::parse(&json!({
            "label": "Login", "reason": "To deploy.", "url": "https://example.com",
            "fields": [{"name": "user", "label": "User"}, {"name": "pw"}],
            "timeout_secs": 120,
        }))
        .unwrap();
        r.id = 7;
        assert_eq!(
            SecretRequest::from_wire(&r.to_wire(true)),
            Some((r.clone(), true))
        );
        assert_eq!(
            SecretRequest::from_wire(&r.to_wire(false)),
            Some((r, false))
        );
        assert_eq!(SecretRequest::from_wire(&Value::Null), None);
        // A server's words are checked as an agent's are.
        assert_eq!(
            SecretRequest::from_wire(
                &json!({"id": 1, "label": "T", "reason": "r", "url": "javascript:x"})
            ),
            None
        );
    }

    #[test]
    fn a_call_to_a_closed_chat_fails() {
        let (route, _rx) = Route::open().unwrap();
        let token = route.token().to_string();
        let args = json!({"label": "T", "reason": "r"});
        assert!(ask(&token, &args).is_ok());
        drop(route);
        assert!(ask(&token, &args).is_err());
        assert!(ask("nobody", &args).is_err());
    }

    #[test]
    fn the_mcp_server_lists_and_runs_its_tool() {
        let init = mcp_reply(
            r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-03-26"}}"#,
            |_| unreachable!(),
        )
        .unwrap();
        assert_eq!(init["result"]["protocolVersion"], "2025-03-26");
        assert!(init["result"]["capabilities"]["tools"].is_object());
        assert!(
            mcp_reply(
                r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#,
                |_| unreachable!()
            )
            .is_none()
        );
        let list = mcp_reply(
            r#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#,
            |_| unreachable!(),
        )
        .unwrap();
        assert_eq!(list["result"]["tools"][0]["name"], TOOL);

        let call = r#"{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"request_secret","arguments":{"label":"T","reason":"r"}}}"#;
        let declined = mcp_reply(call, |args| {
            assert_eq!(args["label"], "T");
            Ok(Outcome::Declined.to_json())
        })
        .unwrap();
        assert_eq!(declined["id"], 3);
        assert_eq!(
            declined["result"]["structuredContent"]["outcome"],
            "declined"
        );
        assert_eq!(declined["result"]["isError"], false);
        let failed = mcp_reply(call, |_| Err("not open".into())).unwrap();
        assert_eq!(failed["result"]["isError"], true);

        let unknown = mcp_reply(
            r#"{"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"x"}}"#,
            |_| unreachable!(),
        )
        .unwrap();
        assert!(unknown["error"].is_object());
        let bad = mcp_reply("{", |_| unreachable!()).unwrap();
        assert_eq!(bad["error"]["code"], -32700);
    }

    #[test]
    fn each_agent_gets_the_server_in_its_own_shape() {
        let server = McpCommand {
            program: "/bin/arakun".into(),
            args: vec!["mcp".into()],
            env: vec![(TOKEN_VAR.into(), "t".into())],
        };
        assert_eq!(
            server.acp(),
            json!({"name": "arakun", "command": "/bin/arakun", "args": ["mcp"],
                   "env": [{"name": TOKEN_VAR, "value": "t"}]})
        );
        assert_eq!(server.droid()["env"], json!({TOKEN_VAR: "t"}));
        assert!(server.codex()["tool_timeout_sec"].as_u64().unwrap() > MAX_TIMEOUT.as_secs());
    }
}
