//! Agents ask the user for a secret (a token, a password, a pairing code) with
//! a card in the chat instead of in the chat itself, where the value would sit
//! in the transcript, the prompt history, the title and on paired devices.
//!
//! The agent calls `request_secret` on Arakun's MCP server (`arakun mcp`, see
//! [`mcp_reply`]). That process forwards the call over the control socket with
//! the token of the chat it was started for; [`ask`] hands it to that chat's
//! [`Route`], and the chat shows the card. The agent gets back only the outcome
//! and, for a saved secret, a `secret-ref:` handle per field, never the value.
//! Only Arakun's own tools redeem a handle: `run_with_secrets` sets it as an
//! environment variable for one command. A handle works once, in the project
//! it was made for, for 24 hours; the value waits in the keyring
//! (`crate::secret_store`) until then.
//!
//! A command that must read the value itself (`sudo -S`, a pairing CLI) asks
//! for a file instead (`deliver: "file"`): the card tells the user the agent's
//! commands can read it, and the agent gets the path of a file only the user
//! can read. `secret_to_file` moves a secret the user pasted in the chat to
//! such a file, so the agent stops repeating it. Files go after 24 hours.
//!
//! Under `arakun server` the call goes to the server's local socket instead
//! ([`serve_from`]), and the card goes to the devices that watch the session
//! (`phone::wire::secret`). They send what was typed back only over a private
//! path ([`answer`]): the relay, Tailscale or https, never plain http.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use futures::channel::{mpsc, oneshot};
use serde_json::{Value, json};

use super::AgentKind;
use crate::secret::Secret;

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
pub const HANDLE_PREFIX: &str = "secret-ref:";
/// Runs one command with handles redeemed into its environment.
pub const RUN_TOOL: &str = "run_with_secrets";
/// Puts a secret the user pasted in the chat into a private file.
pub const FILE_TOOL: &str = "secret_to_file";
/// How long `run_with_secrets` lets a command run when the agent does not say.
const RUN_TIMEOUT: Duration = Duration::from_secs(120);
const MAX_RUN_TIMEOUT: Duration = Duration::from_secs(30 * 60);
/// How much of a command's output goes back to the agent, from its end.
const MAX_OUTPUT: usize = 20_000;
/// What the card says before Arakun may write a secret to a file.
pub const FILE_NOTICE: &str = "The agent's commands can read this. Arakun puts it in a file only you can read, and deletes the file after 24 hours.";

/// How the agent gets a saved secret.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub enum Deliver {
    /// A `secret-ref:` handle that only Arakun's tools redeem.
    #[default]
    Handle,
    /// The path of a private file, for a command that must read the value.
    File,
}

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
    pub deliver: Deliver,
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
        let deliver = match &args["deliver"] {
            Value::Null => Deliver::Handle,
            v if v == "handle" => Deliver::Handle,
            v if v == "file" => Deliver::File,
            _ => return Err("`deliver` is \"handle\" or \"file\"".into()),
        };
        Ok(Self {
            id: 0,
            label,
            reason,
            url,
            fields,
            timeout,
            deliver,
        })
    }
}

impl SecretRequest {
    /// As the devices that watch a server's session get it. `private` says
    /// whether every connection from them to the agent is encrypted, so what
    /// they type may be sent; the card offers only Decline otherwise.
    /// A file request leads its reason with [`FILE_NOTICE`], so a device that
    /// does not know `deliver` still tells the user.
    pub fn to_wire(&self, private: bool) -> Value {
        let reason = match self.deliver {
            Deliver::Handle => self.reason.clone(),
            Deliver::File => format!("{FILE_NOTICE} {}", self.reason),
        };
        json!({
            "id": self.id,
            "label": self.label,
            "reason": reason,
            "deliver": if self.deliver == Deliver::File { "file" } else { "handle" },
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
        let mut v = v.clone();
        if v["deliver"] == "file"
            && let Some(reason) = v["reason"].as_str()
            && let Some(rest) = reason.strip_prefix(FILE_NOTICE)
        {
            v["reason"] = rest.trim_start().into();
        }
        let mut request = Self::parse(&v).ok()?;
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
    /// The path of a private file per field, in the order the agent asked.
    Filed(Vec<(String, String)>),
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
            Outcome::Filed(files) => {
                let files: serde_json::Map<String, Value> = files
                    .iter()
                    .map(|(name, p)| (name.clone(), Value::from(p.as_str())))
                    .collect();
                json!({"outcome": "saved", "files": files})
            }
            Outcome::Declined => json!({"outcome": "declined"}),
            Outcome::Cancelled(why) => json!({"outcome": "cancelled", "reason": why.as_str()}),
        }
    }

    /// [`Outcome::to_json`] read back, as the MCP server gets it over the socket.
    pub fn from_json(v: &Value) -> Option<Self> {
        let pairs = |m: &Value| -> Option<Vec<(String, String)>> {
            m.as_object()?
                .iter()
                .map(|(k, h)| Some((k.clone(), h.as_str()?.to_string())))
                .collect()
        };
        Some(match v["outcome"].as_str()? {
            "saved" if v.get("files").is_some() => Outcome::Filed(pairs(&v["files"])?),
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
                    "The user saved it. These are handles, not the values. Redeem them with the `{RUN_TOOL}` tool, which sets each as an environment variable for one command; each works once, in this project, within 24 hours. Never type a handle into a page or a terminal:\n{list}"
                )
            }
            Outcome::Filed(files) => {
                let list = files
                    .iter()
                    .map(|(name, p)| format!("{name}: {p}"))
                    .collect::<Vec<_>>()
                    .join("\n");
                format!(
                    "The user saved it to files only they can read. Use a file in a command as \"$(cat <path>)\" or `< <path>`, and never print it. Arakun deletes the files after 24 hours:\n{list}"
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

/// Where a chat's requests go, and the project its handles belong to.
struct Address {
    tx: mpsc::UnboundedSender<Asked>,
    cwd: PathBuf,
}

static ROUTES: Mutex<Option<HashMap<String, Address>>> = Mutex::new(None);
static NEXT_ID: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

/// A chat's address for requests. Dropping it closes the address, so a call
/// that comes later fails instead of waiting for nobody.
pub struct Route {
    token: String,
}

impl Route {
    /// A new address for a chat in the project at `cwd`, and where its
    /// requests arrive. `None` when the OS has no random bytes for the token.
    pub fn open(cwd: &Path) -> Option<(Self, mpsc::UnboundedReceiver<Asked>)> {
        start_sweeper();
        let token = random_hex(16)?;
        let (tx, rx) = mpsc::unbounded();
        ROUTES
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get_or_insert_with(HashMap::new)
            .insert(
                token.clone(),
                Address {
                    tx,
                    cwd: cwd.to_path_buf(),
                },
            );
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

const NOT_OPEN: &str = "this session is not open in Arakun";

/// The project of the chat `token` names.
fn project_of(token: &str) -> Result<PathBuf, String> {
    let routes = ROUTES.lock().unwrap_or_else(|e| e.into_inner());
    routes
        .as_ref()
        .and_then(|r| r.get(token))
        .map(|a| a.cwd.clone())
        .ok_or_else(|| NOT_OPEN.to_string())
}

/// Hands a `request_secret` call to the chat `token` names. The receiver gets
/// the outcome; the error is for the agent.
pub fn ask(token: &str, args: &Value) -> Result<oneshot::Receiver<Outcome>, String> {
    let mut request = SecretRequest::parse(args)?;
    request.id = NEXT_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let routes = ROUTES.lock().unwrap_or_else(|e| e.into_inner());
    let tx = &routes
        .as_ref()
        .and_then(|r| r.get(token))
        .ok_or(NOT_OPEN)?
        .tx;
    let (reply, rx) = oneshot::channel();
    tx.unbounded_send(Asked { request, reply })
        .map_err(|_| NOT_OPEN.to_string())?;
    Ok(rx)
}

/// The answer to one call from Arakun's MCP server.
pub enum Answer {
    /// Known at once.
    Now(Value),
    /// A card's outcome, once the user answered it.
    Later(oneshot::Receiver<Outcome>),
}

/// One call from Arakun's MCP server for the chat `token` names, as the
/// control socket carries it: `request_secret`'s arguments as they are, or
/// `{"tool": "redeem" | "file", "arguments": …}`. The error is for the agent.
pub fn call(token: &str, args: &Value) -> Result<Answer, String> {
    match args.get("tool").and_then(Value::as_str) {
        None => ask(token, args).map(Answer::Later),
        Some("redeem") => redeem(token, &args["arguments"]).map(Answer::Now),
        Some("file") => pasted_to_file(token, &args["arguments"]).map(Answer::Now),
        Some(other) => Err(format!("Arakun does not know the call `{other}`")),
    }
}

/// The values behind `handles`, each once, for `run_with_secrets`, with the
/// project they belong to: `{"cwd": …, "values": […]}`. None is taken unless
/// all of them can be.
fn redeem(token: &str, args: &Value) -> Result<Value, String> {
    let cwd = project_of(token)?;
    let handles: Vec<&str> = args["handles"]
        .as_array()
        .ok_or("`handles` must be a list")?
        .iter()
        .map(|h| h.as_str().ok_or("each handle must be a string"))
        .collect::<Result<_, _>>()?;
    for h in &handles {
        check(h, &cwd)?;
    }
    let mut values = Vec::with_capacity(handles.len());
    for h in handles {
        values.push(Value::from(take(h, &cwd)?.expose()));
    }
    Ok(json!({"cwd": cwd, "values": values}))
}

/// `secret_to_file`: a secret the agent already has goes to a private file.
fn pasted_to_file(token: &str, args: &Value) -> Result<Value, String> {
    project_of(token)?;
    let value = args["value"]
        .as_str()
        .filter(|v| !v.is_empty())
        .ok_or("`value` is required: the secret to put in the file")?;
    if value.starts_with(HANDLE_PREFIX) {
        return Err(format!(
            "that is a handle, not a secret; redeem it with `{RUN_TOOL}`"
        ));
    }
    let name = args["name"].as_str().unwrap_or("secret");
    let path = write_file(name, value)?;
    Ok(json!({"path": path}))
}

/// Why Arakun does not type `text` into a page or a terminal: it holds a
/// handle, which the agent may have taken for the value.
pub fn refuse_handle(text: &str) -> Result<(), String> {
    if text.contains(HANDLE_PREFIX) {
        return Err(format!(
            "That is a `{HANDLE_PREFIX}` handle, not the value, so Arakun does not type it. Redeem it with the `{RUN_TOOL}` tool, or ask for the secret with `deliver: \"file\"`."
        ));
    }
    Ok(())
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
pub const HINT: &str = "- Secrets: when only the user can give a login, token, PIN or password, call the `request_secret` tool (Arakun's MCP server); it shows the user a private card and gives you a handle, never the value. Use a handle only through `run_with_secrets`, never in a page or a terminal. When a command must read the value itself, ask with `deliver: \"file\"` for a private file. When the user pasted a secret in a message, move it to a file with `secret_to_file` and stop repeating it. Never ask for a secret in a message, and never put one in a task or prompt for another agent.\n";

// ---- the values ------------------------------------------------------------

/// What waits behind a handle, as the keyring holds it.
#[derive(serde::Serialize, serde::Deserialize)]
struct Kept {
    value: Secret,
    cwd: PathBuf,
    /// Unix seconds after which it is gone.
    until: u64,
}

fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// Where values wait: the keyring, or memory in the tests.
trait Vault: Send + Sync {
    fn put(&self, key: &str, kept: &str, until: u64) -> Result<(), String>;
    fn get(&self, key: &str) -> Result<Option<String>, String>;
    fn forget(&self, key: &str);
    /// Forgets what expired by `now`.
    fn sweep(&self, now: u64);
}

/// The keyring through `secret_store`, with a list of what it holds and
/// until when, so a sweep finds what nobody redeemed. The list names keys
/// only, which are hashes of handles, so it gives nothing away.
#[cfg_attr(test, allow(dead_code))]
struct Keyring;

#[cfg_attr(test, allow(dead_code))]
impl Keyring {
    fn index_path() -> PathBuf {
        crate::paths::shared(crate::paths::SECRET_REFS)
    }

    fn edit(f: impl FnOnce(&mut std::collections::BTreeMap<String, u64>)) {
        static INDEX: Mutex<()> = Mutex::new(());
        let _held = INDEX.lock().unwrap_or_else(|e| e.into_inner());
        let path = Self::index_path();
        let mut index = crate::durable::load_json(&path).unwrap_or_default();
        f(&mut index);
        let bytes = serde_json::to_vec(&index).unwrap_or_default();
        if let Err(e) = crate::durable::write_private(&path, &bytes) {
            eprintln!("arakun: could not save {}: {e}", path.display());
        }
    }
}

impl Vault for Keyring {
    fn put(&self, key: &str, kept: &str, until: u64) -> Result<(), String> {
        // Listed first: a crash between the two leaves a key to sweep, never
        // a value nobody knows about.
        Self::edit(|i| {
            i.insert(key.to_string(), until);
        });
        crate::secret_store::set(key, "arakun: a secret for an agent", kept)
    }

    fn get(&self, key: &str) -> Result<Option<String>, String> {
        crate::secret_store::get(key)
    }

    fn forget(&self, key: &str) {
        crate::secret_store::forget(key);
        Self::edit(|i| {
            i.remove(key);
        });
    }

    fn sweep(&self, now: u64) {
        Self::edit(|i| {
            i.retain(|key, until| {
                let keep = *until > now;
                if !keep {
                    crate::secret_store::forget(key);
                }
                keep
            });
        });
    }
}

#[cfg(test)]
#[derive(Default)]
struct Memory(Mutex<HashMap<String, (String, u64)>>);

#[cfg(test)]
impl Vault for Memory {
    fn put(&self, key: &str, kept: &str, until: u64) -> Result<(), String> {
        let mut m = self.0.lock().unwrap_or_else(|e| e.into_inner());
        m.insert(key.into(), (kept.into(), until));
        Ok(())
    }

    fn get(&self, key: &str) -> Result<Option<String>, String> {
        let m = self.0.lock().unwrap_or_else(|e| e.into_inner());
        Ok(m.get(key).map(|(k, _)| k.clone()))
    }

    fn forget(&self, key: &str) {
        self.0.lock().unwrap_or_else(|e| e.into_inner()).remove(key);
    }

    fn sweep(&self, now: u64) {
        let mut m = self.0.lock().unwrap_or_else(|e| e.into_inner());
        m.retain(|_, (_, until)| *until > now);
    }
}

fn vault() -> &'static dyn Vault {
    #[cfg(test)]
    {
        static MEMORY: OnceLock<Memory> = OnceLock::new();
        MEMORY.get_or_init(Memory::default)
    }
    #[cfg(not(test))]
    {
        &Keyring
    }
}

/// The key `handle`'s value waits under. A hash, so the list of keys does
/// not hold handles.
fn key_of(handle: &str) -> Result<String, String> {
    use sha2::{Digest as _, Sha256};
    let rest = handle
        .strip_prefix(HANDLE_PREFIX)
        .filter(|r| !r.is_empty())
        .ok_or_else(|| format!("`{handle}` is not a `{HANDLE_PREFIX}` handle"))?;
    let digest = Sha256::digest(rest.as_bytes());
    Ok(format!("agent-secret:{}", hex(&digest[..16])))
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// HMAC-SHA-256 (RFC 2104).
fn hmac(key: &[u8], msg: &[u8]) -> [u8; 32] {
    use sha2::{Digest as _, Sha256};
    let mut block = [0u8; 64];
    if key.len() > block.len() {
        block[..32].copy_from_slice(&Sha256::digest(key));
    } else {
        block[..key.len()].copy_from_slice(key);
    }
    let mut inner = Sha256::new();
    inner.update(block.map(|b| b ^ 0x36));
    inner.update(msg);
    let mut outer = Sha256::new();
    outer.update(block.map(|b| b ^ 0x5c));
    outer.update(inner.finalize());
    let mut out = [0u8; 32];
    out.copy_from_slice(&outer.finalize());
    out
}

/// The key handles are made with: random, kept in the keyring so it lasts
/// across restarts. Where the keyring fails, one for this run.
fn salt() -> &'static [u8] {
    static SALT: OnceLock<Vec<u8>> = OnceLock::new();
    SALT.get_or_init(|| {
        const ACCOUNT: &str = "agent-secret-salt";
        #[cfg(not(test))]
        if let Ok(Some(s)) = crate::secret_store::get(ACCOUNT)
            && s.len() >= 32
        {
            return s.into_bytes();
        }
        let fresh = random_hex(32).unwrap_or_default();
        #[cfg(not(test))]
        if let Err(e) = crate::secret_store::set(ACCOUNT, "arakun: agent secret salt", &fresh) {
            eprintln!("arakun: no lasting key for secret handles: {e}");
        }
        #[cfg(test)]
        let _ = ACCOUNT;
        fresh.into_bytes()
    })
}

/// A handle for one field of one card in one session: an HMAC of them, keyed
/// by [`salt`], with random bytes so no two are alike.
fn make_handle(session: &str, card: u64, field: &str) -> Result<String, String> {
    let nonce = random_hex(16).ok_or("the system gave no random bytes")?;
    let msg = [session, &card.to_string(), field, &nonce].join("\0");
    Ok(format!(
        "{HANDLE_PREFIX}{}",
        hex(&hmac(salt(), msg.as_bytes()))
    ))
}

/// Keeps what the user saved on card `card` of `session`, for the project in
/// `cwd`, and returns a handle per field. On an error nothing is kept, so the
/// card can try again.
pub fn keep(
    cwd: &Path,
    session: &str,
    card: u64,
    values: Vec<(String, String)>,
) -> Result<Vec<(String, String)>, String> {
    let until = unix_now() + KEPT_FOR.as_secs();
    let mut handles: Vec<(String, String)> = Vec::with_capacity(values.len());
    for (name, value) in values {
        let handle = make_handle(session, card, &name)?;
        let kept = serde_json::to_string(&Kept {
            value: Secret::new(value),
            cwd: cwd.to_path_buf(),
            until,
        })
        .map_err(|e| e.to_string());
        let put = kept.and_then(|k| vault().put(&key_of(&handle)?, &k, until));
        if let Err(e) = put {
            for (_, h) in &handles {
                if let Ok(key) = key_of(h) {
                    vault().forget(&key);
                }
            }
            return Err(format!("Could not keep it: {e}"));
        }
        handles.push((name, handle));
    }
    Ok(handles)
}

const GONE: &str = "that handle was used, has expired, or never was";

/// The value behind `handle`, checked for the project in `cwd`, without
/// taking it.
fn check(handle: &str, cwd: &Path) -> Result<Kept, String> {
    check_in(vault(), handle, cwd)
}

fn check_in(vault: &dyn Vault, handle: &str, cwd: &Path) -> Result<Kept, String> {
    let raw = vault.get(&key_of(handle)?)?.ok_or(GONE)?;
    let kept: Kept = serde_json::from_str(&raw).map_err(|_| GONE)?;
    if kept.cwd != cwd {
        return Err("that handle was made for another project".into());
    }
    Ok(kept)
}

/// The value behind `handle`, once, for the project it was saved for. It is
/// deleted from the keyring before it is handed out; when that fails, it is
/// not handed out.
pub fn take(handle: &str, cwd: &Path) -> Result<Secret, String> {
    take_from(vault(), handle, cwd)
}

fn take_from(vault: &dyn Vault, handle: &str, cwd: &Path) -> Result<Secret, String> {
    static TAKING: Mutex<()> = Mutex::new(());
    let _one = TAKING.lock().unwrap_or_else(|e| e.into_inner());
    let kept = check_in(vault, handle, cwd)?;
    let key = key_of(handle)?;
    vault.forget(&key);
    if !matches!(vault.get(&key), Ok(None)) {
        return Err("Arakun could not delete the stored secret, so it does not hand it out".into());
    }
    if kept.until <= unix_now() {
        return Err(GONE.into());
    }
    Ok(kept.value)
}

/// The private folder secret files go to.
fn files_dir() -> PathBuf {
    #[cfg(test)]
    {
        std::env::temp_dir().join(format!("arakun-secret-files-{}", std::process::id()))
    }
    #[cfg(not(test))]
    {
        crate::paths::shared(crate::paths::SECRET_FILES)
    }
}

/// Writes `value` to a new file only the user can read, and returns its
/// path. The folder is 0700 and the file 0600 from the start: it is written
/// to a fresh temp file (`create_new`) and renamed into place.
pub fn write_file(name: &str, value: &str) -> Result<PathBuf, String> {
    let name: String = name
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '_' || *c == '-')
        .take(40)
        .collect();
    let name = if name.is_empty() {
        "secret".into()
    } else {
        name
    };
    let id = random_hex(12).ok_or("the system gave no random bytes")?;
    let dir = files_dir();
    let path = dir.join(format!("{name}-{id}"));
    crate::durable::write_private(&path, value.as_bytes())
        .map_err(|e| format!("Could not write the file: {e}"))?;
    // An older folder made by hand may be open to others.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let _ = std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700));
    }
    Ok(path)
}

/// Writes one file per field, and returns their paths. On an error the
/// files already written are removed, so the card can try again.
pub fn file(values: Vec<(String, String)>) -> Result<Vec<(String, String)>, String> {
    let mut files: Vec<(String, String)> = Vec::with_capacity(values.len());
    for (name, value) in values {
        match write_file(&name, &value) {
            Ok(path) => files.push((name, path.display().to_string())),
            Err(e) => {
                for (_, p) in &files {
                    let _ = std::fs::remove_file(p);
                }
                return Err(e);
            }
        }
    }
    Ok(files)
}

/// Deletes the files and the handles' values older than 24 hours.
pub fn sweep() {
    let files = crate::housekeep::remove_old(&files_dir(), KEPT_FOR, |_| true);
    if files > 0 {
        eprintln!("arakun: deleted {files} secret files older than a day");
    }
    vault().sweep(unix_now());
}

/// Sweeps now and every hour after, on a thread of its own. Only the first
/// call starts it.
pub fn start_sweeper() {
    #[cfg(not(test))]
    {
        static STARTED: OnceLock<()> = OnceLock::new();
        STARTED.get_or_init(|| {
            let _ = std::thread::Builder::new()
                .name("secret-sweep".into())
                .spawn(|| {
                    loop {
                        sweep();
                        std::thread::sleep(Duration::from_secs(60 * 60));
                    }
                });
        });
    }
}

fn random_hex(bytes: usize) -> Option<String> {
    let mut buf = vec![0u8; bytes];
    getrandom::fill(&mut buf).ok()?;
    Some(hex(&buf))
}

// ---- the MCP server ----------------------------------------------------------

fn tools() -> Value {
    json!([
        {
            "name": TOOL,
            "title": "Ask the user for a secret",
            "description": format!("Ask the user for a secret only they can give (a login, API token, PIN, password or pairing code) with a private card in Arakun. Never ask for a secret in a message. The call waits until the user saves or declines, the run stops, or the timeout passes. It returns the outcome and, when saved, a `secret-ref:` handle per field, never the value: redeem a handle with `{RUN_TOOL}`. For a command that must read the value itself (such as `sudo -S` or a pairing CLI), set `deliver` to \"file\" to get the path of a private file instead. Never put a secret or a handle in a task for another agent."),
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
                    "deliver": {"type": "string", "enum": ["handle", "file"], "description": "\"handle\" (the default) for a one-use handle, or \"file\" for a private file your commands read with \"$(cat <path>)\" or `< <path>`. The card tells the user which."},
                    "timeout_secs": {"type": "integer", "description": "How long the card stays open, in seconds. 600 when left out."}
                },
                "required": ["label", "reason"]
            }
        },
        {
            "name": RUN_TOOL,
            "title": "Run a command with secrets",
            "description": "Run one shell command in the project with `secret-ref:` handles redeemed into environment variables, such as {\"GITHUB_TOKEN\": \"secret-ref:…\"}. Each handle works once. The values are masked in the output that comes back.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "command": {"type": "string", "description": "The command, run by `sh -c` (`cmd /C` on Windows) in the project folder. Refer to the secrets as environment variables, such as \"$GITHUB_TOKEN\"."},
                    "env": {"type": "object", "additionalProperties": {"type": "string"}, "description": "Variable name to `secret-ref:` handle."},
                    "timeout_secs": {"type": "integer", "description": "How long the command may run, in seconds. 120 when left out."}
                },
                "required": ["command", "env"]
            }
        },
        {
            "name": FILE_TOOL,
            "title": "Move a pasted secret to a file",
            "description": "When the user pasted a secret in a message, put it in a private file and use the path from then on, so you stop repeating the value. Read it in commands with \"$(cat <path>)\" or `< <path>`. Arakun deletes the file after 24 hours.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "value": {"type": "string", "description": "The secret, as the user pasted it."},
                    "name": {"type": "string", "description": "A short name for the file, such as \"github-token\"."}
                },
                "required": ["value"]
            }
        }
    ])
}

/// Serves MCP on stdin and stdout until stdin closes. `forward` takes a call
/// (see [`call`]) to whoever holds the chat, with the chat's token from
/// [`TOKEN_VAR`], and waits for its answer.
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
                let call = |tool: &str, args: &Value| {
                    if token.is_empty() {
                        return Err(
                            "Arakun did not start this server for a session, so it has no card to show".into(),
                        );
                    }
                    run_tool(|a: &Value| forward(&token, a), tool, args)
                };
                if let Some(reply) = mcp_reply(&line, call) {
                    write(&out, reply);
                }
            });
        } else if let Some(reply) = mcp_reply(&line, |_, _| Err(String::new())) {
            write(&out, reply);
        }
    }
    0
}

/// How long the MCP server waits for the answer to a forwarded call: a little
/// longer than a card stays open, or a moment for the rest.
pub fn call_wait(args: &Value) -> Result<Duration, String> {
    if args.get("tool").is_some() {
        return Ok(Duration::from_secs(60));
    }
    Ok(SecretRequest::parse(args)?.timeout + Duration::from_secs(60))
}

/// Runs one of the tools: `forward` takes a call to the app or the server,
/// and gives its answer. Ok is the text the agent reads and the structured
/// result.
pub fn run_tool(
    forward: impl Fn(&Value) -> Result<Value, String>,
    tool: &str,
    args: &Value,
) -> Result<(String, Value), String> {
    match tool {
        TOOL => {
            let outcome = forward(args)?;
            let text =
                Outcome::from_json(&outcome).map_or_else(|| outcome.to_string(), |o| o.text());
            Ok((text, outcome))
        }
        FILE_TOOL => {
            let got = forward(&json!({"tool": "file", "arguments": args}))?;
            let path = got["path"].as_str().ok_or("Arakun wrote no file")?;
            Ok((
                format!(
                    "Saved to {path}, a file only the user can read. From now on use \"$(cat {path})\" or `< {path}` and never repeat the value. Arakun deletes the file after 24 hours."
                ),
                got,
            ))
        }
        RUN_TOOL => {
            let run = Run::parse(args)?;
            let handles: Vec<&str> = run.env.iter().map(|(_, h)| h.as_str()).collect();
            let got = forward(&json!({"tool": "redeem", "arguments": {"handles": handles}}))?;
            let cwd = got["cwd"].as_str().map(PathBuf::from);
            let values: Vec<Secret> = got["values"]
                .as_array()
                .ok_or("Arakun gave no values")?
                .iter()
                .map(|v| v.as_str().map(Secret::new))
                .collect::<Option<_>>()
                .filter(|v: &Vec<Secret>| v.len() == run.env.len())
                .ok_or("Arakun gave the wrong values")?;
            let (code, output) = run.run(cwd.as_deref(), &values)?;
            let text = format!("exit {code}\n{output}");
            Ok((text, json!({"exit_code": code, "output": output})))
        }
        other => Err(format!("unknown tool {other}")),
    }
}

/// A `run_with_secrets` call, checked.
#[derive(Debug, PartialEq)]
struct Run {
    command: String,
    /// Variable names and their handles.
    env: Vec<(String, String)>,
    timeout: Duration,
}

impl Run {
    fn parse(args: &Value) -> Result<Self, String> {
        let command = args["command"]
            .as_str()
            .map(str::trim)
            .filter(|c| !c.is_empty())
            .ok_or("`command` is required")?
            .to_string();
        let map = args["env"]
            .as_object()
            .filter(|m| !m.is_empty() && m.len() <= 8)
            .ok_or("`env` takes 1 to 8 variables, each a name and a handle")?;
        let mut env = Vec::with_capacity(map.len());
        for (name, handle) in map {
            let ok = name
                .chars()
                .next()
                .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
                && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
            if !ok {
                return Err(format!("`{name}` is not a variable name"));
            }
            let handle = handle
                .as_str()
                .filter(|h| h.starts_with(HANDLE_PREFIX))
                .ok_or_else(|| format!("`{name}` must be a `{HANDLE_PREFIX}` handle"))?;
            env.push((name.clone(), handle.to_string()));
        }
        let timeout = match &args["timeout_secs"] {
            Value::Null => RUN_TIMEOUT,
            v => Duration::from_secs(v.as_u64().ok_or("`timeout_secs` must be a number")?)
                .clamp(Duration::from_secs(1), MAX_RUN_TIMEOUT),
        };
        Ok(Self {
            command,
            env,
            timeout,
        })
    }

    /// Runs the command in `cwd` with `values` set, one per variable. Its
    /// output, with every value masked and only its end kept, and its code.
    fn run(&self, cwd: Option<&Path>, values: &[Secret]) -> Result<(i32, String), String> {
        let mut cmd = crate::platform::shell_command(&self.command);
        crate::platform::no_window(&mut cmd);
        if let Some(cwd) = cwd {
            cmd.current_dir(cwd);
        }
        for ((name, _), value) in self.env.iter().zip(values) {
            cmd.env(name, value.expose());
        }
        let out = crate::platform::output_within(&mut cmd, None, self.timeout)
            .map_err(|e| format!("could not run the command: {e}"))?;
        let mut text = String::from_utf8_lossy(&out.stdout).into_owned();
        text.push_str(&String::from_utf8_lossy(&out.stderr));
        Ok((out.status.code().unwrap_or(-1), mask(&text, values)))
    }
}

/// `text` with each of `values` replaced by `[redacted]`, cut to its last
/// [`MAX_OUTPUT`] bytes.
fn mask(text: &str, values: &[Secret]) -> String {
    let mut text = text.to_string();
    for v in values.iter().filter(|v| !v.is_empty()) {
        text = text.replace(v.expose(), crate::secrets::REDACTED);
    }
    if text.len() > MAX_OUTPUT {
        let mut from = text.len() - MAX_OUTPUT;
        while !text.is_char_boundary(from) {
            from += 1;
        }
        text = format!("[…]{}", &text[from..]);
    }
    text
}

/// The MCP server's answer to one JSON-RPC line, if it needs one. `call` runs
/// a tool by name: Ok with the text and the structured result, or the error
/// for the agent.
pub fn mcp_reply(
    line: &str,
    call: impl FnOnce(&str, &Value) -> Result<(String, Value), String>,
) -> Option<Value> {
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
    let name = params["name"].as_str().unwrap_or_default();
    match msg["method"].as_str().unwrap_or_default() {
        "initialize" => ok(json!({
            "protocolVersion": params["protocolVersion"].as_str().unwrap_or("2025-06-18"),
            "capabilities": {"tools": {}},
            "serverInfo": {"name": SERVER, "title": "Arakun", "version": env!("CARGO_PKG_VERSION")},
        })),
        "ping" => ok(json!({})),
        "tools/list" => ok(json!({"tools": tools()})),
        "tools/call" if [TOOL, RUN_TOOL, FILE_TOOL].contains(&name) => {
            let args = params.get("arguments").cloned().unwrap_or(json!({}));
            match call(name, &args) {
                Ok((text, structured)) => ok(json!({
                    "content": [{"type": "text", "text": text}],
                    "structuredContent": structured,
                    "isError": false,
                })),
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
            Outcome::Filed(vec![("pw".into(), "/tmp/pw-1".into())]),
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
        // A device that does not know `deliver` still shows the notice.
        let mut f = r.clone();
        f.deliver = Deliver::File;
        let wire = f.to_wire(true);
        assert!(wire["reason"].as_str().unwrap().starts_with(FILE_NOTICE));
        assert_eq!(SecretRequest::from_wire(&wire), Some((f, true)));
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
        let (route, _rx) = Route::open(Path::new("/p")).unwrap();
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
            |_, _| unreachable!(),
        )
        .unwrap();
        assert_eq!(init["result"]["protocolVersion"], "2025-03-26");
        assert!(init["result"]["capabilities"]["tools"].is_object());
        assert!(
            mcp_reply(
                r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#,
                |_, _| unreachable!()
            )
            .is_none()
        );
        let list = mcp_reply(
            r#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#,
            |_, _| unreachable!(),
        )
        .unwrap();
        let names: Vec<_> = list["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["name"].clone())
            .collect();
        assert_eq!(names, [TOOL, RUN_TOOL, FILE_TOOL]);

        let call = r#"{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"request_secret","arguments":{"label":"T","reason":"r"}}}"#;
        let declined = mcp_reply(call, |tool, args| {
            assert_eq!(args["label"], "T");
            run_tool(|_| Ok(Outcome::Declined.to_json()), tool, args)
        })
        .unwrap();
        assert_eq!(declined["id"], 3);
        assert_eq!(
            declined["result"]["structuredContent"]["outcome"],
            "declined"
        );
        assert_eq!(declined["result"]["isError"], false);
        let failed = mcp_reply(call, |_, _| Err("not open".into())).unwrap();
        assert_eq!(failed["result"]["isError"], true);

        let unknown = mcp_reply(
            r#"{"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"x"}}"#,
            |_, _| unreachable!(),
        )
        .unwrap();
        assert!(unknown["error"].is_object());
        let bad = mcp_reply("{", |_, _| unreachable!()).unwrap();
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

    fn kept(cwd: &str, value: &str) -> String {
        let session = random_hex(8).unwrap();
        keep(
            Path::new(cwd),
            &session,
            1,
            vec![("v".into(), value.into())],
        )
        .unwrap()
        .remove(0)
        .1
    }

    #[test]
    fn a_handle_works_once_in_its_own_project() {
        let h = kept("/proj", "tok-1");
        assert!(h.starts_with(HANDLE_PREFIX));
        assert!(!h.contains("tok-1"));
        // Another project neither gets it nor uses it up.
        assert!(take(&h, Path::new("/other")).is_err());
        assert_eq!(take(&h, Path::new("/proj")).unwrap().expose(), "tok-1");
        assert!(take(&h, Path::new("/proj")).is_err(), "used once");
        assert!(take("secret-ref:made-up", Path::new("/proj")).is_err());
        assert!(take("not-a-handle", Path::new("/proj")).is_err());
    }

    #[test]
    fn a_value_that_could_not_be_deleted_is_not_handed_out() {
        struct Stuck(Memory);
        impl Vault for Stuck {
            fn put(&self, key: &str, kept: &str, until: u64) -> Result<(), String> {
                self.0.put(key, kept, until)
            }
            fn get(&self, key: &str) -> Result<Option<String>, String> {
                self.0.get(key)
            }
            fn forget(&self, _: &str) {}
            fn sweep(&self, _: u64) {}
        }
        let stuck = Stuck(Memory::default());
        let h = make_handle("s", 1, "v").unwrap();
        let k = Kept {
            value: Secret::new("v"),
            cwd: "/proj".into(),
            until: unix_now() + 60,
        };
        stuck
            .put(
                &key_of(&h).unwrap(),
                &serde_json::to_string(&k).unwrap(),
                k.until,
            )
            .unwrap();
        assert!(take_from(&stuck, &h, Path::new("/proj")).is_err());
    }

    #[test]
    fn handles_are_keyed_and_never_alike() {
        let a = make_handle("s", 1, "v").unwrap();
        let b = make_handle("s", 1, "v").unwrap();
        assert_ne!(a, b);
        assert_eq!(a.len(), HANDLE_PREFIX.len() + 64);
        // RFC 4231, test case 2.
        assert_eq!(
            hex(&hmac(b"Jefe", b"what do ya want for nothing?")),
            "5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843"
        );
    }

    #[test]
    fn an_expired_handle_is_gone() {
        let h = kept("/proj", "tok-old");
        let key = key_of(&h).unwrap();
        let mut k: Kept = serde_json::from_str(&vault().get(&key).unwrap().unwrap()).unwrap();
        k.until = unix_now() - 1;
        vault()
            .put(&key, &serde_json::to_string(&k).unwrap(), k.until)
            .unwrap();
        assert!(take(&h, Path::new("/proj")).is_err());
        assert_eq!(vault().get(&key), Ok(None), "and deleted");

        let h = kept("/proj", "tok-swept");
        let key = key_of(&h).unwrap();
        vault().sweep(unix_now() + KEPT_FOR.as_secs() + 1);
        assert_eq!(vault().get(&key), Ok(None), "the sweep forgets it");
    }

    #[test]
    fn redeeming_takes_all_the_handles_or_none() {
        let (route, _rx) = Route::open(Path::new("/proj")).unwrap();
        let token = route.token().to_string();
        let (a, b) = (kept("/proj", "va"), kept("/proj", "vb"));
        let elsewhere = kept("/other", "vc");
        let redeem_args = |hs: &[&str]| json!({"tool": "redeem", "arguments": {"handles": hs}});
        assert!(call(&token, &redeem_args(&[&a, &elsewhere])).is_err());
        let Ok(Answer::Now(got)) = call(&token, &redeem_args(&[&a, &b])) else {
            panic!("not redeemed");
        };
        assert_eq!(got["values"], json!(["va", "vb"]), "a was not used up");
        assert_eq!(got["cwd"], "/proj");
        assert!(call(&token, &redeem_args(&[&a])).is_err(), "used once");
        drop(route);
        assert!(call(&token, &redeem_args(&[&elsewhere])).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn run_with_secrets_sets_the_variable_and_masks_it() {
        let args = json!({"command": "echo \"got $TOKEN\"", "env": {"TOKEN": "secret-ref:x"}});
        let asked = Mutex::new(None);
        let (text, out) = run_tool(
            |v| {
                *asked.lock().unwrap() = Some(v.clone());
                Ok(json!({"cwd": std::env::temp_dir(), "values": ["hunter2"]}))
            },
            RUN_TOOL,
            &args,
        )
        .unwrap();
        assert_eq!(
            asked.into_inner().unwrap().unwrap(),
            json!({"tool": "redeem", "arguments": {"handles": ["secret-ref:x"]}})
        );
        assert_eq!(out["exit_code"], 0);
        assert!(!text.contains("hunter2"), "{text}");
        assert!(text.contains("got [redacted]"), "{text}");

        for bad in [
            json!({"command": "true", "env": {"TOKEN": "hunter2"}}),
            json!({"command": "true", "env": {"1X": "secret-ref:x"}}),
            json!({"command": "", "env": {"X": "secret-ref:x"}}),
            json!({"command": "true", "env": {}}),
        ] {
            assert!(Run::parse(&bad).is_err(), "{bad}");
        }
        assert_eq!(
            mask(&"a".repeat(MAX_OUTPUT + 10), &[]).len(),
            MAX_OUTPUT + "[…]".len()
        );
    }

    #[test]
    fn a_pasted_secret_moves_to_a_file_that_the_sweep_deletes() {
        let (route, _rx) = Route::open(Path::new("/proj")).unwrap();
        let file_args =
            |v: &str| json!({"tool": "file", "arguments": {"value": v, "name": "gh token"}});
        assert!(call(route.token(), &file_args("secret-ref:abc")).is_err());
        let Ok(Answer::Now(got)) = call(route.token(), &file_args("ghp_pasted")) else {
            panic!("no file");
        };
        let path = PathBuf::from(got["path"].as_str().unwrap());
        assert!(
            path.file_name()
                .unwrap()
                .to_str()
                .unwrap()
                .starts_with("ghtoken-")
        );
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "ghp_pasted");
        assert!(call("nobody", &file_args("x")).is_err());

        // A fresh file stays; one older than a day goes.
        sweep();
        assert!(path.exists());
        let old = std::time::SystemTime::now() - KEPT_FOR - Duration::from_secs(60);
        std::fs::File::options()
            .write(true)
            .open(&path)
            .unwrap()
            .set_modified(old)
            .unwrap();
        sweep();
        assert!(!path.exists());
    }

    #[test]
    fn a_handle_is_never_typed() {
        assert!(refuse_handle("hello").is_ok());
        assert!(refuse_handle("echo secret-ref:abc").is_err());
        let args = json!({"label": "T", "reason": "r", "deliver": "env"});
        assert!(SecretRequest::parse(&args).is_err());
    }
}
