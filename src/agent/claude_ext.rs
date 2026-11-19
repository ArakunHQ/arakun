//! What Claude Code loads in a folder: its MCP servers (with the health
//! `claude mcp list` reports), plugins, skills and permission rules. Reads
//! Claude's own files (`~/.claude.json`, settings, `.mcp.json`, skill folders)
//! and asks its CLI only for what the files don't say. Secrets in commands,
//! URLs and rules are masked before they leave this module.

use std::collections::{HashMap, HashSet};
use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use anyhow::{Result, anyhow};
use serde_json::Value;

use super::launch;

/// `claude mcp list` health-checks every server (stdio ones are started).
const LIST_TIMEOUT: Duration = Duration::from_secs(45);
const CLI_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Clone, Debug, PartialEq)]
pub enum Health {
    Connected,
    NeedsAuth,
    /// A `.mcp.json` server nobody approved yet.
    Pending,
    Failed(String),
    /// Turned off for this folder.
    Off,
    /// Not checked (no `claude` on PATH, or the check timed out).
    Unknown,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Scope {
    /// `~/.claude.json`, every project.
    User,
    /// `~/.claude.json` under this folder, just for you.
    Local,
    /// The folder's `.mcp.json`, shared with the repo.
    Project,
    /// Brought by an installed plugin.
    Plugin(String),
    /// A connector of the claude.ai account.
    ClaudeAi,
}

impl Scope {
    pub fn label(&self) -> String {
        match self {
            Scope::User => "user".into(),
            Scope::Local => "local".into(),
            Scope::Project => "project".into(),
            Scope::Plugin(p) => format!("plugin {p}"),
            Scope::ClaudeAi => "claude.ai".into(),
        }
    }

    pub fn about(&self) -> &'static str {
        match self {
            Scope::User => "Your user config (~/.claude.json): every project",
            Scope::Local => "~/.claude.json, only in this folder and only for you",
            Scope::Project => "The folder's .mcp.json, shared through the repo",
            Scope::Plugin(_) => "Comes with an installed plugin",
            Scope::ClaudeAi => "A connector of your claude.ai account",
        }
    }
}

#[derive(Clone, Debug)]
pub struct Server {
    /// As Claude names it (`plugin:posthog:posthog`, `claude.ai Gmail`).
    pub name: String,
    pub scope: Scope,
    /// stdio, http or sse.
    pub transport: String,
    /// The command line or URL, secrets masked.
    pub target: String,
    /// Header and env var names; their values are never read out.
    pub headers: Vec<String>,
    pub env: Vec<String>,
    pub health: Health,
    /// Permission rules naming the server's tools.
    pub rules: Vec<Rule>,
    /// What the server answered when asked directly, for failing HTTP ones.
    pub diagnosis: Option<Diagnosis>,
}

/// A failing HTTP server's own answer to an `initialize` request.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Diagnosis {
    /// 0 when it could not be reached at all.
    pub status: u16,
    /// Its error message, secrets masked.
    pub message: String,
    /// It advertises OAuth (`WWW-Authenticate: Bearer resource_metadata=…`).
    pub oauth: bool,
}

impl Diagnosis {
    /// One sentence on what went wrong.
    pub fn explain(&self, token: bool) -> String {
        let said = if self.message.is_empty() {
            String::new()
        } else {
            format!(": “{}”", self.message)
        };
        match self.status {
            0 => {
                format!("The server could not be reached{said}. Check the URL and your connection.")
            }
            401 | 403 if token => format!(
                "The server rejected the token Claude sends ({}{said}). It expired, was revoked or belongs to another account.",
                self.status
            ),
            401 | 403 => format!("The server asks for a sign-in ({}{said}).", self.status),
            404 => {
                format!("Nothing answers at this URL (404{said}). The address may have changed.")
            }
            s if s >= 500 => format!("The server is having trouble ({s}{said}). Try again later."),
            s => format!("The server answered {s}{said}."),
        }
    }
}

impl Server {
    /// A claude.ai connector you never signed in to: offered, not set up.
    pub fn unused_connector(&self) -> bool {
        self.scope == Scope::ClaudeAi && self.health == Health::NeedsAuth
    }

    /// Its token rides in a header Claude sends as is.
    pub fn token_header(&self) -> Option<&str> {
        self.headers
            .iter()
            .find(|h| h.eq_ignore_ascii_case("authorization"))
            .or_else(|| {
                self.headers.iter().find(|h| {
                    let h = h.to_lowercase();
                    h.contains("key") || h.contains("token")
                })
            })
            .map(String::as_str)
    }

    /// Scopes `claude mcp remove` and `add-json` work on.
    pub fn editable(&self) -> bool {
        matches!(self.scope, Scope::User | Scope::Local | Scope::Project)
    }

    /// A stdio command that can run in a terminal as shown (no masked secrets, no env).
    pub fn runnable(&self) -> bool {
        self.transport == "stdio"
            && self.env.is_empty()
            && !self.target.contains('•')
            && !self.target.is_empty()
    }

    pub fn tool_prefix(&self) -> String {
        format!("mcp__{}", tool_name(&self.name))
    }
}

#[derive(Clone, Debug)]
pub struct Plugin {
    pub id: String,
    pub version: String,
    pub enabled: bool,
    pub path: PathBuf,
    pub skills: usize,
    pub agents: usize,
    pub commands: usize,
    pub hooks: bool,
    pub servers: Vec<String>,
}

impl Plugin {
    /// `posthog@claude-plugins-official` → `posthog`.
    pub fn short(&self) -> &str {
        self.id.split('@').next().unwrap_or(&self.id)
    }

    pub fn marketplace(&self) -> &str {
        self.id.split_once('@').map_or("", |(_, m)| m)
    }
}

#[derive(Clone, Debug)]
pub struct Skill {
    /// `seo-audit`, or `claude-seo:seo-audit` for a plugin's.
    pub name: String,
    pub description: String,
    /// personal, project or the plugin's name.
    pub source: String,
    pub path: PathBuf,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum RuleKind {
    Allow,
    Ask,
    Deny,
}

impl RuleKind {
    pub fn label(self) -> &'static str {
        match self {
            RuleKind::Allow => "allow",
            RuleKind::Ask => "ask",
            RuleKind::Deny => "deny",
        }
    }
}

#[derive(Clone, Debug)]
pub struct Rule {
    pub kind: RuleKind,
    /// The rule, secrets masked (`Bash(git push:*)`, `mcp__posthog__*`).
    pub rule: String,
    /// managed, user, project or local.
    pub source: &'static str,
}

#[derive(Clone, Debug, Default)]
pub struct Permissions {
    /// default, acceptEdits, plan, bypassPermissions... and the file that set it.
    pub mode: Option<(String, &'static str)>,
    pub rules: Vec<Rule>,
    pub dirs: Vec<String>,
    /// The settings files that exist, in the order Claude weighs them.
    pub files: Vec<(&'static str, PathBuf)>,
}

#[derive(Clone, Debug, Default)]
pub struct ClaudeExtensions {
    pub cli: bool,
    /// Health is what the last check found; a fresh one is running.
    pub checking: bool,
    pub servers: Vec<Server>,
    pub plugins: Vec<Plugin>,
    pub skills: Vec<Skill>,
    pub permissions: Permissions,
    /// What `claude mcp list` warned about (skipped configs...), masked.
    pub warnings: Vec<String>,
}

impl ClaudeExtensions {
    /// Servers that need you: failing, or yours and waiting for a sign-in.
    pub fn problems(&self) -> usize {
        self.servers
            .iter()
            .filter(|s| {
                matches!(s.health, Health::Failed(_))
                    || (s.health == Health::NeedsAuth && !s.unused_connector())
            })
            .count()
    }
}

fn claude_bin() -> Option<PathBuf> {
    launch::which("claude")
}

/// What the slow check found, kept on disk so the next open shows it at once.
#[derive(Default, serde::Serialize, serde::Deserialize)]
struct Checked {
    /// `claude mcp list`'s output.
    mcp_list: Option<String>,
    probes: HashMap<String, Diagnosis>,
}

fn cache_file(cwd: &Path) -> PathBuf {
    use std::hash::{Hash as _, Hasher as _};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    cwd.hash(&mut h);
    crate::platform::app_dir()
        .join("cache")
        .join("extensions")
        .join(format!("claude-{:016x}.json", h.finish()))
}

fn read_checked(cwd: &Path) -> Checked {
    std::fs::read(cache_file(cwd))
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
}

fn save_checked(cwd: &Path, checked: &Checked) {
    let file = cache_file(cwd);
    if let Ok(json) = serde_json::to_vec(checked) {
        let _ = crate::durable::write(&file, &json);
    }
}

/// Instant: Claude's files, with the health the last check found (if any).
/// `checking` is set: `load` tells what is true now.
pub fn load_quick(cwd: &Path) -> ClaudeExtensions {
    let home = launch::home();
    let checked = read_checked(cwd);
    let plugins = read_plugins(&installed_plugins(&home, cwd));
    let listed = checked
        .mcp_list
        .as_deref()
        .map(parse_list)
        .unwrap_or_default();
    let mut x = assemble(&home, cwd, plugins, listed, |_, s| {
        checked.probes.get(&s.name).cloned()
    });
    x.cli = claude_bin().is_some();
    x.checking = true;
    x
}

/// Blocking, seconds: runs `claude plugin list` and `claude mcp list` in `cwd`
/// (which health-checks every server) and asks failing HTTP ones directly.
pub fn load(cwd: &Path) -> ClaudeExtensions {
    let home = launch::home();
    let bin = claude_bin();
    let plugin_list = bin
        .as_ref()
        .and_then(|b| {
            run(b, cwd, &["plugin", "list", "--json"], CLI_TIMEOUT)
                .ok()
                .and_then(|o| serde_json::from_str(&o).ok())
        })
        .unwrap_or_else(|| installed_plugins(&home, cwd));
    let plugins = read_plugins(&plugin_list);
    let mut checked = Checked::default();
    let listed = match &bin {
        Some(b) => match run(b, cwd, &["mcp", "list"], LIST_TIMEOUT) {
            Ok(out) => {
                let l = parse_list(&out);
                checked.mcp_list = Some(out);
                l
            }
            Err(e) => (Vec::new(), vec![format!("claude mcp list: {e}")]),
        },
        None => (Vec::new(), Vec::new()),
    };
    let mut x = assemble(&home, cwd, plugins, listed, |raw, s| {
        (matches!(s.health, Health::Failed(_)) && s.transport != "stdio")
            .then(|| raw.and_then(probe))
            .flatten()
    });
    for s in &x.servers {
        if let Some(d) = &s.diagnosis {
            checked.probes.insert(s.name.clone(), d.clone());
        }
    }
    if bin.is_some() {
        save_checked(cwd, &checked);
    }
    x.cli = bin.is_some();
    x
}

/// Puts the files and what was listed together; `diagnose` gets each server's raw config.
fn assemble(
    home: &Path,
    cwd: &Path,
    plugins: Vec<Plugin>,
    (listed, warnings): (Vec<Listed>, Vec<String>),
    diagnose: impl Fn(Option<&Value>, &Server) -> Option<Diagnosis>,
) -> ClaudeExtensions {
    let state = read_json(&home.join(".claude.json"));
    let project = project_entry(&state, cwd);
    let settings = settings_files(home, cwd);
    let permissions = read_permissions(&settings, project);
    let mut servers = configured_servers(&state, project, cwd);
    merge_health(&mut servers, listed);
    turn_off(&mut servers, project, &settings);
    for s in &mut servers {
        let prefix = s.tool_prefix();
        s.rules = permissions
            .rules
            .iter()
            .filter(|r| {
                r.rule == prefix
                    || r.rule.starts_with(&format!("{prefix}__"))
                    || r.rule.starts_with(&format!("{prefix}("))
            })
            .cloned()
            .collect();
        let raw = raw_config(&state, project, cwd, &s.name, &s.scope);
        s.diagnosis = diagnose(raw.as_ref(), s);
    }
    servers.sort_by(|a, b| {
        scope_rank(&a.scope)
            .cmp(&scope_rank(&b.scope))
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });
    let skills = read_skills(home, cwd, &plugins);
    ClaudeExtensions {
        cli: false,
        checking: false,
        servers,
        plugins,
        skills,
        permissions,
        warnings,
    }
}

/// What `claude plugin list --json` says, from the files: installed plugins,
/// on where a settings file turns them on, and the servers they bring.
fn installed_plugins(home: &Path, cwd: &Path) -> Value {
    let installed = read_json(&home.join(".claude/plugins/installed_plugins.json"));
    let settings = settings_files(home, cwd);
    let mut out = Vec::new();
    for (id, entries) in installed["plugins"].as_object().into_iter().flatten() {
        let Some(e) = entries.as_array().and_then(|a| {
            a.iter()
                .find(|e| e["scope"] == "user")
                .or_else(|| a.first())
        }) else {
            continue;
        };
        let path = PathBuf::from(e["installPath"].as_str().unwrap_or_default());
        let enabled = settings
            .iter()
            .find_map(|(_, _, v)| v["enabledPlugins"][id].as_bool())
            .unwrap_or(false);
        let servers = [".mcp.json", "mcp.json"]
            .iter()
            .map(|f| read_json(&path.join(f)))
            .find(|v| !v.is_null())
            .map(|v| {
                if v["mcpServers"].is_object() {
                    v["mcpServers"].clone()
                } else {
                    v
                }
            })
            .unwrap_or(Value::Null);
        out.push(serde_json::json!({
            "id": id,
            "version": e["version"],
            "scope": e["scope"],
            "enabled": enabled,
            "installPath": e["installPath"],
            "mcpServers": servers,
        }));
    }
    Value::Array(out)
}

/// The config of a server as its file has it, secrets and all: never shown.
fn raw_config(
    state: &Value,
    project: &Value,
    cwd: &Path,
    name: &str,
    scope: &Scope,
) -> Option<Value> {
    let v = match scope {
        Scope::User => state["mcpServers"][name].clone(),
        Scope::Local => project["mcpServers"][name].clone(),
        Scope::Project => read_json(&cwd.join(".mcp.json"))["mcpServers"][name].clone(),
        _ => Value::Null,
    };
    (!v.is_null()).then_some(v)
}

fn scope_arg(scope: &Scope) -> Result<&'static str> {
    match scope {
        Scope::User => Ok("user"),
        Scope::Local => Ok("local"),
        Scope::Project => Ok("project"),
        _ => Err(anyhow!(
            "Claude Code can't edit this server: it comes from a plugin or claude.ai"
        )),
    }
}

/// Asks an HTTP server to `initialize` with its own headers, through curl with
/// the request on stdin so no secret lands in a process list.
fn probe(raw: &Value) -> Option<Diagnosis> {
    let url = raw["url"].as_str()?;
    let quote = |s: &str| s.replace('\\', "\\\\").replace('"', "\\\"");
    let mut config = format!(
        "url = \"{}\"\nrequest = \"POST\"\nsilent\nmax-time = 10\nwrite-out = \"\\n%{{http_code}}\\n%header{{www-authenticate}}\"\n",
        quote(url)
    );
    for (k, v) in raw["headers"].as_object().into_iter().flatten() {
        config.push_str(&format!(
            "header = \"{}: {}\"\n",
            quote(k),
            quote(v.as_str().unwrap_or_default())
        ));
    }
    config.push_str("header = \"Content-Type: application/json\"\nheader = \"Accept: application/json, text/event-stream\"\n");
    let body = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"arakun","version":"1"}}}"#;
    config.push_str(&format!("data = \"{}\"\n", quote(body)));
    let mut child = crate::platform::command("curl")
        .args(["--config", "-"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    {
        use std::io::Write as _;
        child.stdin.take()?.write_all(config.as_bytes()).ok()?;
    }
    let out = child.wait_with_output().ok()?;
    Some(parse_probe(&String::from_utf8_lossy(&out.stdout)))
}

/// curl's output: the body, then the status line and WWW-Authenticate from `write-out`.
fn parse_probe(out: &str) -> Diagnosis {
    // split, not lines: an empty last field (no WWW-Authenticate) still counts.
    let mut lines: Vec<&str> = out.split('\n').collect();
    let auth = lines.pop().unwrap_or_default();
    let status: u16 = lines.pop().and_then(|l| l.trim().parse().ok()).unwrap_or(0);
    let body = lines.join("\n");
    let body = body.trim();
    let message = serde_json::from_str::<Value>(body)
        .ok()
        .and_then(|v| {
            let m = v["error"]["message"]
                .as_str()
                .or(v["error_description"].as_str())
                .or(v["error"].as_str())
                .or(v["message"].as_str())?;
            Some(m.to_string())
        })
        .unwrap_or_else(|| {
            if body.starts_with('<') {
                String::new()
            } else {
                body.chars().take(160).collect()
            }
        });
    Diagnosis {
        status,
        message: redact(message.trim()),
        oauth: auth.contains("resource_metadata") || auth.to_lowercase().starts_with("bearer"),
    }
}

/// Removes a server from the scope it lives in.
pub fn remove(cwd: &Path, name: &str, scope: &Scope) -> Result<()> {
    let bin = claude_bin().ok_or_else(|| anyhow!("claude is not on PATH"))?;
    run(
        &bin,
        cwd,
        &["mcp", "remove", name, "-s", scope_arg(scope)?],
        CLI_TIMEOUT,
    )
    .map(|_| ())
}

/// Puts `config` in place of the server's, putting the old one back if that fails.
fn replace(cwd: &Path, name: &str, scope: &Scope, old: &Value, config: &Value) -> Result<()> {
    let bin = claude_bin().ok_or_else(|| anyhow!("claude is not on PATH"))?;
    let scope = scope_arg(scope)?;
    run(
        &bin,
        cwd,
        &["mcp", "remove", name, "-s", scope],
        CLI_TIMEOUT,
    )?;
    if let Err(e) = run(
        &bin,
        cwd,
        &["mcp", "add-json", name, &config.to_string(), "-s", scope],
        CLI_TIMEOUT,
    ) {
        let _ = run(
            &bin,
            cwd,
            &["mcp", "add-json", name, &old.to_string(), "-s", scope],
            CLI_TIMEOUT,
        );
        return Err(e);
    }
    Ok(())
}

fn current(cwd: &Path, name: &str, scope: &Scope) -> Result<Value> {
    let state = read_json(&launch::home().join(".claude.json"));
    raw_config(&state, project_entry(&state, cwd), cwd, name, scope)
        .ok_or_else(|| anyhow!("{name} is not in its config file anymore"))
}

/// Swaps the token in `header`, keeping a `Bearer ` the old value had.
pub fn replace_token(
    cwd: &Path,
    name: &str,
    scope: &Scope,
    header: &str,
    token: &str,
) -> Result<()> {
    let token = token.trim();
    if token.is_empty() {
        return Err(anyhow!("The token is empty"));
    }
    let old = current(cwd, name, scope)?;
    let mut config = old.clone();
    let before = config["headers"][header]
        .as_str()
        .unwrap_or_default()
        .to_string();
    let bearer = before.starts_with("Bearer ") && !token.starts_with("Bearer ");
    config["headers"][header] = Value::String(if bearer {
        format!("Bearer {token}")
    } else {
        token.to_string()
    });
    replace(cwd, name, scope, &old, &config)
}

/// Drops the token header so Claude signs in with OAuth instead; sign in next.
pub fn use_sign_in(cwd: &Path, name: &str, scope: &Scope, header: &str) -> Result<()> {
    let old = current(cwd, name, scope)?;
    let mut config = old.clone();
    if let Some(h) = config["headers"].as_object_mut() {
        h.remove(header);
        if h.is_empty() {
            config.as_object_mut().map(|o| o.remove("headers"));
        }
    }
    replace(cwd, name, scope, &old, &config)
}

/// Adds a remote server for your user (every project).
pub fn add_http(cwd: &Path, name: &str, url: &str) -> Result<()> {
    let bin = claude_bin().ok_or_else(|| anyhow!("claude is not on PATH"))?;
    run(
        &bin,
        cwd,
        &["mcp", "add", "--transport", "http", "-s", "user", name, url],
        CLI_TIMEOUT,
    )
    .map(|_| ())
}

/// Popular remote servers to add in one click: (name, URL, what it gives, needs sign-in).
pub const CATALOG: &[(&str, &str, &str, bool)] = &[
    (
        "linear",
        "https://mcp.linear.app/mcp",
        "Issues, projects and cycles",
        true,
    ),
    (
        "notion",
        "https://mcp.notion.com/mcp",
        "Pages, databases and search",
        true,
    ),
    (
        "sentry",
        "https://mcp.sentry.dev/mcp",
        "Errors, traces and releases",
        true,
    ),
    (
        "supabase",
        "https://mcp.supabase.com/mcp",
        "Database, auth and edge functions",
        true,
    ),
    (
        "stripe",
        "https://mcp.stripe.com",
        "Payments, customers and billing",
        true,
    ),
    (
        "figma",
        "https://mcp.figma.com/mcp",
        "Designs, components and variables",
        true,
    ),
    (
        "vercel",
        "https://mcp.vercel.com",
        "Deployments, logs and projects",
        true,
    ),
    (
        "cloudflare",
        "https://mcp.cloudflare.com/mcp",
        "Workers, DNS, storage and analytics",
        true,
    ),
    (
        "posthog",
        "https://mcp.posthog.com/mcp",
        "Product analytics, flags and errors",
        true,
    ),
    (
        "neon",
        "https://mcp.neon.tech/mcp",
        "Serverless Postgres and branches",
        true,
    ),
    (
        "context7",
        "https://mcp.context7.com/mcp",
        "Up-to-date docs for any library",
        false,
    ),
];

/// The host of a URL (`https://mcp.linear.app/mcp` → `mcp.linear.app`).
pub fn host(url: &str) -> &str {
    let rest = url.split_once("://").map_or(url, |(_, r)| r);
    rest.split(['/', '?', ':']).next().unwrap_or(rest)
}

/// Turns a plugin on or off for your user, as `/plugin` does.
pub fn set_plugin(cwd: &Path, id: &str, enabled: bool) -> Result<()> {
    let bin = claude_bin().ok_or_else(|| anyhow!("claude is not on PATH"))?;
    run(
        &bin,
        cwd,
        &["plugin", if enabled { "enable" } else { "disable" }, id],
        CLI_TIMEOUT,
    )
    .map(|_| ())
}

/// The terminal command that signs in to a server (opens the browser).
pub fn login_command(name: &str) -> String {
    format!("claude mcp login {}", shell_quote(name))
}

fn shell_quote(s: &str) -> String {
    if s.chars()
        .all(|c| c.is_ascii_alphanumeric() || "-_.:@/".contains(c))
    {
        s.to_string()
    } else {
        format!("'{}'", s.replace('\'', r"'\''"))
    }
}

fn scope_rank(s: &Scope) -> u8 {
    match s {
        Scope::Local => 0,
        Scope::Project => 1,
        Scope::User => 2,
        Scope::Plugin(_) => 3,
        Scope::ClaudeAi => 4,
    }
}

// ---------------------------------------------------------------------------
// Files.

fn read_json(path: &Path) -> Value {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or(Value::Null)
}

/// This folder's entry in `~/.claude.json` (keyed by absolute path).
fn project_entry<'a>(state: &'a Value, cwd: &Path) -> &'a Value {
    let key = cwd.to_string_lossy();
    let entry = &state["projects"][key.trim_end_matches('/')];
    if entry.is_null() {
        let canon = std::fs::canonicalize(cwd).unwrap_or_default();
        let canon = canon.to_string_lossy();
        if let Some(v) = state["projects"].get(canon.as_ref()) {
            return v;
        }
    }
    entry
}

/// Settings files Claude reads for `cwd`, most important first.
fn settings_files(home: &Path, cwd: &Path) -> Vec<(&'static str, PathBuf, Value)> {
    let managed = if cfg!(target_os = "macos") {
        PathBuf::from("/Library/Application Support/ClaudeCode/managed-settings.json")
    } else if cfg!(windows) {
        PathBuf::from(r"C:\Program Files\ClaudeCode\managed-settings.json")
    } else {
        PathBuf::from("/etc/claude-code/managed-settings.json")
    };
    let mut files = vec![
        ("managed", managed),
        ("local", cwd.join(".claude/settings.local.json")),
        ("project", cwd.join(".claude/settings.json")),
        ("user", home.join(".claude/settings.json")),
    ];
    // In the home folder the project files are the user's own.
    if cwd == home {
        files.retain(|(s, _)| *s != "project");
    }
    files
        .into_iter()
        .filter(|(_, p)| p.is_file())
        .map(|(s, p)| {
            let v = read_json(&p);
            (s, p, v)
        })
        .collect()
}

fn read_permissions(settings: &[(&'static str, PathBuf, Value)], project: &Value) -> Permissions {
    let mut p = Permissions {
        files: settings
            .iter()
            .map(|(s, path, _)| (*s, path.clone()))
            .collect(),
        ..Default::default()
    };
    let mut seen = HashSet::new();
    for (source, _, v) in settings {
        let perms = &v["permissions"];
        if p.mode.is_none()
            && let Some(m) = perms["defaultMode"].as_str()
        {
            p.mode = Some((m.to_string(), source));
        }
        for (key, kind) in [
            ("deny", RuleKind::Deny),
            ("ask", RuleKind::Ask),
            ("allow", RuleKind::Allow),
        ] {
            for r in perms[key]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
            {
                if seen.insert((kind, r.to_string())) {
                    p.rules.push(Rule {
                        kind,
                        rule: redact(r),
                        source,
                    });
                }
            }
        }
        for d in perms["additionalDirectories"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
        {
            if !p.dirs.iter().any(|x| x == d) {
                p.dirs.push(d.to_string());
            }
        }
    }
    // Older Claude Code kept "always allow" answers in ~/.claude.json.
    for r in project["allowedTools"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
    {
        if seen.insert((RuleKind::Allow, r.to_string())) {
            p.rules.push(Rule {
                kind: RuleKind::Allow,
                rule: redact(r),
                source: "local",
            });
        }
    }
    p
}

fn names(v: &Value) -> Vec<String> {
    let mut n: Vec<String> = v
        .as_object()
        .map(|o| o.keys().cloned().collect())
        .unwrap_or_default();
    n.sort();
    n
}

fn server_from(name: &str, scope: Scope, v: &Value) -> Server {
    let url = v["url"].as_str();
    let transport = v["type"].as_str().map(str::to_string).unwrap_or_else(|| {
        if url.is_some() {
            "http".into()
        } else {
            "stdio".into()
        }
    });
    let target = match url {
        Some(u) => redact(u),
        None => {
            let mut parts = vec![v["command"].as_str().unwrap_or_default().to_string()];
            parts.extend(
                v["args"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(Value::as_str)
                    .map(str::to_string),
            );
            redact(parts.join(" ").trim())
        }
    };
    Server {
        name: name.to_string(),
        scope,
        transport,
        target,
        headers: names(&v["headers"]),
        env: names(&v["env"]),
        health: Health::Unknown,
        rules: Vec::new(),
        diagnosis: None,
    }
}

/// Every server the config files name, the one Claude uses first when a name repeats.
fn configured_servers(state: &Value, project: &Value, cwd: &Path) -> Vec<Server> {
    let mcp_json = read_json(&cwd.join(".mcp.json"));
    let mut out: Vec<Server> = Vec::new();
    let mut add = |name: &str, scope: Scope, v: &Value| {
        if !out.iter().any(|s| s.name == name) {
            out.push(server_from(name, scope, v));
        }
    };
    for (scope, map) in [
        (Scope::Local, &project["mcpServers"]),
        (Scope::Project, &mcp_json["mcpServers"]),
        (Scope::User, &state["mcpServers"]),
    ] {
        for (name, v) in map.as_object().into_iter().flatten() {
            add(name, scope.clone(), v);
        }
    }
    out
}

fn read_plugins(list: &Value) -> Vec<Plugin> {
    let mut out: Vec<Plugin> = list
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|p| {
            let path = PathBuf::from(p["installPath"].as_str().unwrap_or_default());
            let count_md = |dir: &str| {
                std::fs::read_dir(path.join(dir))
                    .into_iter()
                    .flatten()
                    .flatten()
                    .filter(|e| e.path().extension().is_some_and(|x| x == "md"))
                    .count()
            };
            Some(Plugin {
                id: p["id"].as_str()?.to_string(),
                version: p["version"].as_str().unwrap_or_default().to_string(),
                enabled: p["enabled"].as_bool().unwrap_or(true),
                skills: skill_dirs(&path.join("skills")).len(),
                agents: count_md("agents"),
                commands: count_md("commands"),
                hooks: path.join("hooks/hooks.json").is_file(),
                servers: names(&p["mcpServers"]),
                path,
            })
        })
        .collect();
    out.sort_by(|a, b| b.enabled.cmp(&a.enabled).then_with(|| a.id.cmp(&b.id)));
    out
}

fn skill_dirs(dir: &Path) -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.join("SKILL.md").is_file())
        .collect();
    v.sort();
    v
}

fn read_skills(home: &Path, cwd: &Path, plugins: &[Plugin]) -> Vec<Skill> {
    let mut out = Vec::new();
    let mut roots: Vec<(String, PathBuf, Option<String>)> =
        vec![("personal".into(), home.join(".claude/skills"), None)];
    if cwd != home {
        roots.push(("project".into(), cwd.join(".claude/skills"), None));
    }
    for p in plugins.iter().filter(|p| p.enabled) {
        roots.push((
            p.short().to_string(),
            p.path.join("skills"),
            Some(p.short().to_string()),
        ));
    }
    for (source, root, prefix) in roots {
        for dir in skill_dirs(&root) {
            let (name, description) = frontmatter(&dir.join("SKILL.md"));
            let name = name.unwrap_or_else(|| {
                dir.file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .to_string()
            });
            let name = match &prefix {
                Some(p) => format!("{p}:{name}"),
                None => name,
            };
            out.push(Skill {
                name,
                description,
                source: source.clone(),
                path: dir,
            });
        }
    }
    out
}

/// `name` and `description` of a SKILL.md's YAML front matter.
fn frontmatter(path: &Path) -> (Option<String>, String) {
    let mut head = String::new();
    if let Ok(f) = std::fs::File::open(path) {
        let _ = f.take(16 * 1024).read_to_string(&mut head);
    }
    parse_frontmatter(&head)
}

fn parse_frontmatter(text: &str) -> (Option<String>, String) {
    let mut lines = text.lines();
    if lines.next().map(str::trim) != Some("---") {
        return (None, String::new());
    }
    let (mut name, mut description) = (None, String::new());
    let mut folding = false;
    for line in lines {
        if line.trim() == "---" {
            break;
        }
        let indented = line.starts_with(' ') || line.starts_with('\t');
        if folding && indented {
            if !description.is_empty() {
                description.push(' ');
            }
            description.push_str(line.trim());
            continue;
        }
        folding = false;
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        let value = value
            .trim()
            .trim_matches('"')
            .trim_matches('\'')
            .to_string();
        match key.trim() {
            "name" => name = Some(value),
            "description" => {
                if matches!(value.as_str(), "" | ">" | "|" | ">-" | "|-") {
                    folding = true;
                } else {
                    description = value;
                }
            }
            _ => {}
        }
    }
    (name, description)
}

// ---------------------------------------------------------------------------
// `claude mcp list`.

struct Listed {
    name: String,
    target: String,
    transport: Option<String>,
    health: Health,
}

/// `name: target (HTTP) - ✔ Connected` lines, and the diagnostics after them.
fn parse_list(out: &str) -> (Vec<Listed>, Vec<String>) {
    let mut listed = Vec::new();
    let mut warnings = Vec::new();
    for line in out.lines().map(str::trim) {
        if line.contains("[Warning]") || line.contains("[Error]") {
            warnings.push(redact(line.trim_start_matches(['└', ' '])));
            continue;
        }
        let Some((name, rest)) = line.split_once(": ") else {
            continue;
        };
        let Some((target, state)) = rest.rsplit_once(" - ") else {
            continue;
        };
        if name.is_empty() || name.starts_with('[') || name == "Location" {
            continue;
        }
        let mut target = target.trim().to_string();
        let mut transport = None;
        for (suffix, t) in [(" (HTTP)", "http"), (" (SSE)", "sse")] {
            if let Some(t0) = target.strip_suffix(suffix) {
                target = t0.to_string();
                transport = Some(t.to_string());
            }
        }
        let state = state
            .trim_start_matches(|c: char| !c.is_alphabetic())
            .trim();
        let health = if state.starts_with("Connected") {
            Health::Connected
        } else if state.starts_with("Needs auth") {
            Health::NeedsAuth
        } else if state.starts_with("Pending") {
            Health::Pending
        } else {
            Health::Failed(state.to_string())
        };
        listed.push(Listed {
            name: name.to_string(),
            target: redact(&target),
            transport,
            health,
        });
    }
    (listed, warnings)
}

fn merge_health(servers: &mut Vec<Server>, listed: Vec<Listed>) {
    for l in listed {
        if let Some(s) = servers.iter_mut().find(|s| s.name == l.name) {
            s.health = l.health;
            continue;
        }
        let scope = if let Some(rest) = l.name.strip_prefix("plugin:") {
            Scope::Plugin(rest.split(':').next().unwrap_or(rest).to_string())
        } else if l.name.starts_with("claude.ai ") {
            Scope::ClaudeAi
        } else {
            Scope::User
        };
        let transport = l.transport.unwrap_or_else(|| {
            if l.target.contains("://") {
                "http".into()
            } else {
                "stdio".into()
            }
        });
        servers.push(Server {
            name: l.name,
            scope,
            transport,
            target: l.target,
            headers: Vec::new(),
            env: Vec::new(),
            health: l.health,
            rules: Vec::new(),
            diagnosis: None,
        });
    }
}

/// Servers this folder turned off (`/mcp` → disable), and `.mcp.json` ones it rejected.
fn turn_off(servers: &mut [Server], project: &Value, settings: &[(&'static str, PathBuf, Value)]) {
    let list = |v: &Value| -> Vec<String> {
        v.as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .map(str::to_string)
            .collect()
    };
    let off = list(&project["disabledMcpServers"]);
    let mut rejected = list(&project["disabledMcpjsonServers"]);
    for (_, _, v) in settings {
        rejected.extend(list(&v["disabledMcpjsonServers"]));
    }
    for s in servers.iter_mut() {
        if off.contains(&s.name) || (s.scope == Scope::Project && rejected.contains(&s.name)) {
            s.health = Health::Off;
        }
    }
}

/// How Claude names a server inside tool names (`claude.ai Gmail` → `claude_ai_Gmail`).
fn tool_name(server: &str) -> String {
    server
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Running the CLI.

pub(crate) fn run_cli(bin: &Path, cwd: &Path, args: &[&str], timeout: Duration) -> Result<String> {
    run(bin, cwd, args, timeout)
}

fn run(bin: &Path, cwd: &Path, args: &[&str], timeout: Duration) -> Result<String> {
    let mut cmd: Command = crate::platform::command(bin);
    cmd.args(args)
        .current_dir(cwd)
        .env("PATH", launch::child_path_env())
        .env("NO_COLOR", "1")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = cmd.spawn()?;
    let mut stdout = child.stdout.take().ok_or_else(|| anyhow!("no stdout"))?;
    let mut stderr = child.stderr.take().ok_or_else(|| anyhow!("no stderr"))?;
    let reader = std::thread::spawn(move || {
        let mut s = String::new();
        let _ = stdout.read_to_string(&mut s);
        s
    });
    let err_reader = std::thread::spawn(move || {
        let mut s = String::new();
        let _ = stderr.read_to_string(&mut s);
        s
    });
    let start = Instant::now();
    let status = loop {
        if let Some(st) = child.try_wait()? {
            break st;
        }
        if start.elapsed() > timeout {
            let _ = child.kill();
            let _ = child.wait();
            return Err(anyhow!("timed out after {}s", timeout.as_secs()));
        }
        std::thread::sleep(Duration::from_millis(100));
    };
    let out = reader.join().unwrap_or_default();
    let err = err_reader.join().unwrap_or_default();
    if !status.success() {
        let msg = err
            .lines()
            .chain(out.lines())
            .map(str::trim)
            .find(|l| !l.is_empty())
            .unwrap_or("failed");
        return Err(anyhow!("{}", redact(msg)));
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// Secrets.

const SECRET_PREFIXES: [&str; 14] = [
    "ghp_",
    "gho_",
    "ghu_",
    "ghs_",
    "github_pat_",
    "sk-",
    "sk_",
    "phx_",
    "phc_",
    "xox",
    "AKIA",
    "eyJ",
    "glpat-",
    "sbp_",
];

/// Masks what looks like a token: known prefixes, `user:token@` in URLs and
/// long random-looking words. Keeps the first characters so it stays recognizable.
pub fn redact(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut word = String::new();
    let flush = |word: &mut String, out: &mut String| {
        if looks_secret(word) {
            out.push_str(&word.chars().take(4).collect::<String>());
            out.push_str("••••");
        } else {
            out.push_str(word);
        }
        word.clear();
    };
    for c in s.chars() {
        if c.is_ascii_alphanumeric() || "_-.+".contains(c) {
            word.push(c);
        } else {
            flush(&mut word, &mut out);
            out.push(c);
        }
    }
    flush(&mut word, &mut out);
    out
}

fn looks_secret(w: &str) -> bool {
    if w.len() >= 12 && SECRET_PREFIXES.iter().any(|p| w.starts_with(p)) {
        return true;
    }
    let letters = w.chars().filter(|c| c.is_ascii_alphabetic()).count();
    let digits = w.chars().filter(|c| c.is_ascii_digit()).count();
    let upper = w.chars().any(|c| c.is_ascii_uppercase());
    let lower = w.chars().any(|c| c.is_ascii_lowercase());
    w.len() >= 32 && !w.contains('.') && digits >= 4 && letters >= 8 && upper && lower
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn masks_tokens() {
        assert_eq!(
            redact(
                "Bash(git remote set-url origin https://me:ghp_FAKEtoken0000000000000000000000000000@github.com/a/b.git)"
            ),
            "Bash(git remote set-url origin https://me:ghp_••••@github.com/a/b.git)"
        );
        assert_eq!(
            redact("Bearer phx_FAKEkey000000000000000000000000000000000000000"),
            "Bearer phx_••••"
        );
        assert_eq!(
            redact("npx @playwright/mcp@latest --viewport-size 1280x800"),
            "npx @playwright/mcp@latest --viewport-size 1280x800"
        );
        assert_eq!(
            redact("https://mcp.supabase.com/mcp?project_ref=ahjnnuzxjuniajhxzrnw"),
            "https://mcp.supabase.com/mcp?project_ref=ahjnnuzxjuniajhxzrnw"
        );
        assert_eq!(redact("Bash(cargo build:*)"), "Bash(cargo build:*)");
    }

    #[test]
    fn reads_mcp_list() {
        let out = "Checking MCP server health…\n\n\
            claude.ai Gmail: https://gmailmcp.googleapis.com/mcp/v1 - ✔ Connected\n\
            plugin:posthog:posthog: https://mcp.posthog.com/mcp (HTTP) - ! Needs authentication\n\
            posthog: https://mcp.posthog.com/mcp (HTTP) - ✘ Failed to connect\n\
            supabase: https://mcp.supabase.com/mcp (HTTP) - ⏸ Pending approval (run `claude` to approve)\n\
            helius: npx helius-mcp@latest - ✔ Connected\n\n\
            MCP config diagnostics ⚠\n\
            Location: /x/.mcp.json\n \
            └ [Warning] [supabase] mcpServers.supabase: Skipped — invalid\n";
        let (l, w) = parse_list(out);
        let got: Vec<(&str, &Health)> = l.iter().map(|l| (l.name.as_str(), &l.health)).collect();
        assert_eq!(
            got,
            vec![
                ("claude.ai Gmail", &Health::Connected),
                ("plugin:posthog:posthog", &Health::NeedsAuth),
                ("posthog", &Health::Failed("Failed to connect".into())),
                ("supabase", &Health::Pending),
                ("helius", &Health::Connected),
            ]
        );
        assert_eq!(l[1].transport.as_deref(), Some("http"));
        assert_eq!(l[4].target, "npx helius-mcp@latest");
        assert_eq!(w.len(), 1);
    }

    #[test]
    fn merges_scopes_health_and_off() {
        let state = json!({
            "mcpServers": {"posthog": {"type": "http", "url": "https://mcp.posthog.com/mcp", "headers": {"Authorization": "Bearer x"}}},
            "projects": {"/p": {"mcpServers": {"helius": {"command": "npx", "args": ["helius-mcp@latest"], "env": {"KEY": "v"}}}, "disabledMcpServers": ["posthog"]}}
        });
        let project = project_entry(&state, Path::new("/p"));
        let mut servers = configured_servers(&state, project, Path::new("/nonexistent"));
        let (listed, _) = parse_list(
            "helius: npx helius-mcp@latest - ✔ Connected\nclaude.ai Gmail: https://g - ✔ Connected\nplugin:posthog:posthog: https://p (HTTP) - ! Needs authentication\n",
        );
        merge_health(&mut servers, listed);
        turn_off(&mut servers, project, &[]);
        let find = |n: &str| servers.iter().find(|s| s.name == n).unwrap();
        assert_eq!(find("helius").scope, Scope::Local);
        assert_eq!(find("helius").health, Health::Connected);
        assert_eq!(find("helius").env, vec!["KEY".to_string()]);
        assert_eq!(find("posthog").health, Health::Off);
        assert_eq!(find("posthog").headers, vec!["Authorization".to_string()]);
        assert_eq!(find("claude.ai Gmail").scope, Scope::ClaudeAi);
        assert_eq!(
            find("plugin:posthog:posthog").scope,
            Scope::Plugin("posthog".into())
        );
        assert_eq!(
            find("claude.ai Gmail").tool_prefix(),
            "mcp__claude_ai_Gmail"
        );
    }

    #[test]
    fn reads_probe() {
        let d =
            parse_probe("Invalid API key\n401\nBearer resource_metadata=\"https://x/.well-known\"");
        assert_eq!(
            d,
            Diagnosis {
                status: 401,
                message: "Invalid API key".into(),
                oauth: true
            }
        );
        let d = parse_probe("{\"error\":{\"message\":\"nope\"}}\n403\n");
        assert_eq!(
            (d.status, d.message.as_str(), d.oauth),
            (403, "nope", false)
        );
        assert_eq!(parse_probe("\n000\n").status, 0);
        assert!(d.explain(true).contains("rejected the token"));
        assert_eq!(host("https://mcp.linear.app/mcp"), "mcp.linear.app");
    }

    #[test]
    fn reads_frontmatter() {
        let (n, d) = parse_frontmatter(
            "---\nname: seo-audit\ndescription: >\n  Full audit\n  of a site.\n---\nbody",
        );
        assert_eq!(n.as_deref(), Some("seo-audit"));
        assert_eq!(d, "Full audit of a site.");
        let (n, d) = parse_frontmatter("---\nname: \"x\"\ndescription: 'One line'\n---\n");
        assert_eq!((n.as_deref(), d.as_str()), (Some("x"), "One line"));
    }

    #[test]
    fn rules_by_precedence() {
        let settings = vec![
            (
                "local",
                PathBuf::from("/l"),
                json!({"permissions": {"allow": ["Bash(ls:*)"], "defaultMode": "acceptEdits"}}),
            ),
            (
                "user",
                PathBuf::from("/u"),
                json!({"permissions": {"allow": ["Bash(ls:*)", "mcp__posthog__*"], "deny": ["Read(.env)"], "defaultMode": "plan"}}),
            ),
        ];
        let p = read_permissions(&settings, &Value::Null);
        assert_eq!(p.mode, Some(("acceptEdits".into(), "local")));
        let rules: Vec<(RuleKind, &str, &str)> = p
            .rules
            .iter()
            .map(|r| (r.kind, r.rule.as_str(), r.source))
            .collect();
        assert_eq!(
            rules,
            vec![
                (RuleKind::Allow, "Bash(ls:*)", "local"),
                (RuleKind::Deny, "Read(.env)", "user"),
                (RuleKind::Allow, "mcp__posthog__*", "user")
            ]
        );
    }

    #[test]
    #[ignore = "reads this machine's Claude Code"]
    fn live() {
        let dir = std::env::var("CLAUDE_EXT_DIR").unwrap_or_else(|_| ".".into());
        let x = load(&std::fs::canonicalize(dir).unwrap());
        for s in &x.servers {
            eprintln!(
                "{:<28} {:<16} {:<6} {:?} {} rules {} {:?}",
                s.name,
                s.scope.label(),
                s.transport,
                s.health,
                s.target,
                s.rules.len(),
                s.diagnosis
            );
        }
        for p in &x.plugins {
            eprintln!(
                "plugin {} {} on={} skills={} agents={} cmds={} hooks={} mcp={:?}",
                p.id, p.version, p.enabled, p.skills, p.agents, p.commands, p.hooks, p.servers
            );
        }
        eprintln!(
            "skills {} mode {:?} rules {} files {:?} warnings {:?}",
            x.skills.len(),
            x.permissions.mode,
            x.permissions.rules.len(),
            x.permissions.files,
            x.warnings
        );
    }
}
