//! What arakun knows about Droid without a session: the defaults in
//! `~/.factory/settings.json`, the model catalog (one `droid.list_models` call,
//! cached in arakun's data dir), and the sessions Droid keeps under
//! `~/.factory/sessions/<cwd with / as ->/<id>.jsonl`. Nothing here writes to
//! `~/.factory`.

use std::io::{BufRead as _, BufReader, Write as _};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, Mutex, mpsc};
use std::time::Duration;

use anyhow::{Context as _, Result, anyhow};
use serde_json::{Value, json};

use super::launch;
use super::rpc::{FACTORY_API_VERSION, FACTORY_PROTOCOL_VERSION, unix_now};
use super::thread::{ConfigOption, ModelInfo, NativeSettings, parse_config};
use super::{AgentKind, Setting};

/// Settings a new native Droid session starts with, and the models it can pick.
#[derive(Clone, Default)]
pub struct Catalog {
    pub defaults: NativeSettings,
    pub models: Vec<ModelInfo>,
}

static CATALOG: Mutex<Option<Arc<Catalog>>> = Mutex::new(None);

/// How long a cached model list is trusted before `refresh` asks Droid again.
const MODELS_MAX_AGE: u64 = 12 * 3600;

/// Whether Droid runs on its native protocol (see `launch::droid_native`).
pub fn droid_native() -> bool {
    launch::droid_native()
}

fn home() -> PathBuf {
    crate::platform::home()
}

fn factory_dir() -> PathBuf {
    home().join(".factory")
}

fn cache_path() -> PathBuf {
    let agents = launch::agents_dir();
    agents
        .parent()
        .map_or(agents.clone(), Path::to_path_buf)
        .join("droid-models.json")
}

/// The catalog as last loaded; reads the local files the first time.
pub fn current() -> Arc<Catalog> {
    let mut slot = CATALOG.lock().unwrap();
    slot.get_or_insert_with(|| Arc::new(load_local())).clone()
}

fn load_local() -> Catalog {
    let (models, _) = read_cache().unwrap_or_default();
    build(models)
}

fn build(models: Vec<ModelInfo>) -> Catalog {
    let mut defaults = read_defaults();
    if defaults.reasoning_effort.is_empty()
        && let Some(m) = models.iter().find(|m| m.id == defaults.model_id)
    {
        defaults.reasoning_effort = m.default_effort.clone().unwrap_or_default();
    }
    Catalog { defaults, models }
}

/// Re-reads the defaults and, when the cached model list is missing or old,
/// asks Droid for a fresh one. Blocking; run it off the main thread.
pub fn refresh() -> Arc<Catalog> {
    let cached = read_cache();
    let fresh = cached
        .as_ref()
        .is_some_and(|(m, at)| !m.is_empty() && unix_now().saturating_sub(*at) < MODELS_MAX_AGE);
    let models = if fresh {
        cached.map(|(m, _)| m).unwrap_or_default()
    } else {
        match fetch_models() {
            Ok(raw) => {
                let _ = write_cache(&raw);
                parse_models(&raw)
            }
            Err(e) => {
                eprintln!("arakun: droid.list_models: {e:#}");
                cached.map(|(m, _)| m).unwrap_or_default()
            }
        }
    };
    let catalog = Arc::new(build(models));
    *CATALOG.lock().unwrap() = Some(catalog.clone());
    catalog
}

fn parse_models(raw: &Value) -> Vec<ModelInfo> {
    raw.as_array()
        .into_iter()
        .flatten()
        .filter_map(ModelInfo::from_json)
        .collect()
}

fn read_cache() -> Option<(Vec<ModelInfo>, u64)> {
    let v: Value = serde_json::from_slice(&std::fs::read(cache_path()).ok()?).ok()?;
    Some((
        parse_models(&v["models"]),
        v["fetched"].as_u64().unwrap_or(0),
    ))
}

fn write_cache(models: &Value) -> std::io::Result<()> {
    crate::durable::write(
        &cache_path(),
        json!({"fetched": unix_now(), "models": models})
            .to_string()
            .as_bytes(),
    )
}

/// `sessionDefaultSettings` from `~/.factory/settings.json`: what Droid's own
/// CLI starts a session with.
fn read_defaults() -> NativeSettings {
    let mut s = NativeSettings {
        interaction_mode: "auto".into(),
        ..Default::default()
    };
    let Some(v) = std::fs::read(factory_dir().join("settings.json"))
        .ok()
        .and_then(|b| serde_json::from_slice::<Value>(&b).ok())
    else {
        return s;
    };
    let d = &v["sessionDefaultSettings"];
    let text = |k: &str| d.get(k).and_then(Value::as_str).map(str::to_string);
    if let Some(m) = text("model") {
        s.model_id = m;
    }
    if let Some(e) = text("reasoningEffort") {
        s.reasoning_effort = e;
    }
    s.autonomy_level = text("autonomyLevel")
        .or_else(|| {
            text("autonomyMode").and_then(|m| match m.as_str() {
                "normal" => Some("off".to_string()),
                m => m.strip_prefix("auto-").map(str::to_string),
            })
        })
        .unwrap_or_default();
    if let Some(m) = text("interactionMode").filter(|m| m == "auto" || m == "spec") {
        s.interaction_mode = m;
    }
    s
}

/// One `droid.list_models` against a short-lived `droid exec`; it needs no
/// session, so nothing is written to Droid's session store.
fn fetch_models() -> Result<Value> {
    let spec = launch::resolve_droid_native(None)?;
    let mut child = crate::platform::command(&spec.program)
        .args(&spec.args)
        .envs(spec.env.iter().map(|(k, v)| (k, v)))
        .current_dir(std::env::temp_dir())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .with_context(|| format!("starting {}", spec.display()))?;
    let mut stdin = child.stdin.take().context("droid stdin")?;
    let stdout = child.stdout.take().context("droid stdout")?;
    let id = "arakun-list-models";
    let request = json!({
        "jsonrpc": "2.0",
        "type": "request",
        "factoryApiVersion": FACTORY_API_VERSION,
        "factoryProtocolVersion": FACTORY_PROTOCOL_VERSION,
        "id": id,
        "method": "droid.list_models",
        "params": {"includeDisabled": true},
    });
    let wrote = writeln!(stdin, "{request}").and_then(|_| stdin.flush());
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines() {
            let Ok(line) = line else { break };
            let Ok(v) = serde_json::from_str::<Value>(&line) else {
                continue;
            };
            if v["id"].as_str() == Some(id) {
                let _ = tx.send(v);
                break;
            }
        }
    });
    let reply = wrote.map_err(anyhow::Error::from).and_then(|_| {
        rx.recv_timeout(Duration::from_secs(20))
            .map_err(|_| anyhow!("no answer"))
    });
    let _ = child.kill();
    let _ = child.wait();
    let reply = reply?;
    if let Some(e) = reply.get("error") {
        return Err(anyhow!("{}", e["message"].as_str().unwrap_or("error")));
    }
    let models = reply["result"]["models"].clone();
    if !models.is_array() {
        return Err(anyhow!("no model list in the reply"));
    }
    Ok(models)
}

// ---- Claude Code's options --------------------------------------------------

/// Claude's config options (mode, model, effort, fast mode) as a fresh session
/// starts with them, so the New session tab can offer them before a session exists.
static CLAUDE: Mutex<Option<Arc<Vec<ConfigOption>>>> = Mutex::new(None);

fn claude_cache_path() -> PathBuf {
    cache_path().with_file_name("claude-options.json")
}

/// The options as last seen, from the cache; empty until a Claude session or
/// `refresh_claude` has run once.
pub fn claude_options() -> Arc<Vec<ConfigOption>> {
    let mut slot = CLAUDE.lock().unwrap();
    slot.get_or_insert_with(|| {
        let raw = std::fs::read(claude_cache_path())
            .ok()
            .and_then(|b| serde_json::from_slice::<Value>(&b).ok());
        Arc::new(
            raw.map(|v| claude_defaults(parse_config(&v["options"])))
                .unwrap_or_default(),
        )
    })
    .clone()
}

/// Keeps the options a live Claude session reported, when they changed.
pub fn remember_claude_options(raw: &Value) {
    let fresh = claude_defaults(parse_config(raw));
    if fresh.is_empty() {
        return;
    }
    let same = |a: &[ConfigOption], b: &[ConfigOption]| {
        a.len() == b.len()
            && a.iter().zip(b).all(|(x, y)| {
                x.id == y.id
                    && x.options
                        .iter()
                        .map(|c| &c.id)
                        .eq(y.options.iter().map(|c| &c.id))
            })
    };
    if same(&claude_options(), &fresh) {
        return;
    }
    let _ = crate::durable::write(
        &claude_cache_path(),
        json!({"fetched": unix_now(), "options": raw})
            .to_string()
            .as_bytes(),
    );
    *CLAUDE.lock().unwrap() = Some(Arc::new(fresh));
}

/// A session's values are its own; a new one starts on the defaults: the
/// `default` choice where there is one, fast mode off, and the permission mode
/// from `~/.claude/settings.json`.
fn claude_defaults(mut options: Vec<ConfigOption>) -> Vec<ConfigOption> {
    let mode = std::fs::read(home().join(".claude/settings.json"))
        .ok()
        .and_then(|b| serde_json::from_slice::<Value>(&b).ok())
        .and_then(|v| v["permissions"]["defaultMode"].as_str().map(str::to_string));
    for o in &mut options {
        let has = |id: &str| o.options.iter().any(|c| c.id == id);
        let pick = if o.category.as_deref() == Some("mode") && mode.as_deref().is_some_and(has) {
            mode.clone()
        } else if has("default") {
            Some("default".to_string())
        } else if has("off") {
            Some("off".to_string())
        } else {
            o.options.first().map(|c| c.id.clone())
        };
        if let Some(pick) = pick {
            o.current = pick;
        }
    }
    options
}

/// Asks the Claude Code adapter for its options when none are cached: a session
/// that never gets a prompt, so it costs no tokens. Blocking; run it off the main thread.
pub fn refresh_claude() {
    if !claude_options().is_empty() {
        return;
    }
    match fetch_options(AgentKind::Claude) {
        Ok((_, raw)) => remember_claude_options(&raw),
        Err(e) => eprintln!("arakun: claude options: {e:#}"),
    }
}

/// The id and config options of a session `kind` starts in a scratch folder
/// and never prompts.
fn fetch_options(kind: AgentKind) -> Result<(String, Value)> {
    let cwd = std::env::temp_dir().join(format!("arakun-{}-options", kind.id()));
    std::fs::create_dir_all(&cwd)?;
    let spec = launch::resolve(kind, Some(&cwd), &|_| {})?;
    let mut child = crate::platform::command(&spec.program)
        .args(&spec.args)
        .envs(spec.env.iter().map(|(k, v)| (k, v)))
        .current_dir(&cwd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .with_context(|| format!("starting {}", spec.display()))?;
    let mut stdin = child.stdin.take().context("adapter stdin")?;
    let stdout = child.stdout.take().context("adapter stdout")?;
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines() {
            let Ok(line) = line else { break };
            let Ok(v) = serde_json::from_str::<Value>(&line) else {
                continue;
            };
            let id = v["id"].as_u64();
            if matches!(id, Some(1 | 2)) {
                let last = id == Some(2);
                if tx.send(v).is_err() || last {
                    break;
                }
            }
        }
    });
    let mut send = |r: Value| -> Result<Value> {
        writeln!(stdin, "{r}").and_then(|_| stdin.flush())?;
        rx.recv_timeout(Duration::from_secs(60))
            .map_err(|_| anyhow!("no answer"))
    };
    // The Codex adapter refuses `session/new` before `initialize` has answered.
    let reply = send(json!({"jsonrpc": "2.0", "id": 1, "method": "initialize",
                            "params": {"protocolVersion": 1, "clientCapabilities": {}}}))
    .and_then(|_| {
        send(json!({"jsonrpc": "2.0", "id": 2, "method": "session/new",
                    "params": {"cwd": cwd, "mcpServers": []}}))
    });
    let _ = child.kill();
    let _ = child.wait();
    let reply = reply?;
    if let Some(e) = reply.get("error") {
        return Err(anyhow!("{}", e["message"].as_str().unwrap_or("error")));
    }
    let options = reply["result"]["configOptions"].clone();
    if !options.is_array() {
        return Err(anyhow!("no config options in the reply"));
    }
    Ok((
        reply["result"]["sessionId"]
            .as_str()
            .unwrap_or_default()
            .to_string(),
        options,
    ))
}

// ---- OpenCode's options -----------------------------------------------------

/// OpenCode's config options (model and mode, plus reasoning on models that
/// have it) as a fresh session starts with them, and the model the user last
/// picked for an OpenCode session anywhere. OpenCode itself starts every ACP
/// session on its configured default, so arakun carries the pick over.
struct OpenCodeCache {
    raw: Value,
    options: Arc<Vec<ConfigOption>>,
    picked: Vec<(Setting, String)>,
}

static OPENCODE: Mutex<Option<OpenCodeCache>> = Mutex::new(None);

fn opencode_cache_path() -> PathBuf {
    cache_path().with_file_name("opencode-options.json")
}

fn with_opencode<T>(f: impl FnOnce(&mut OpenCodeCache) -> T) -> T {
    let mut slot = OPENCODE.lock().unwrap();
    let cache = slot.get_or_insert_with(|| {
        let v = std::fs::read(opencode_cache_path())
            .ok()
            .and_then(|b| serde_json::from_slice::<Value>(&b).ok())
            .unwrap_or_default();
        let picked = v["picked"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|p| {
                Some((
                    Setting::from_key(p[0].as_str()?)?,
                    p[1].as_str()?.to_string(),
                ))
            })
            .collect();
        OpenCodeCache {
            options: Arc::new(parse_config(&v["options"])),
            raw: v["options"].clone(),
            picked,
        }
    });
    f(cache)
}

fn write_opencode(cache: &OpenCodeCache) {
    let picked: Vec<Value> = cache
        .picked
        .iter()
        .map(|(s, v)| json!([s.key(), v]))
        .collect();
    let _ = crate::durable::write(
        &opencode_cache_path(),
        json!({"fetched": unix_now(), "options": cache.raw, "picked": picked})
            .to_string()
            .as_bytes(),
    );
}

/// The options a fresh OpenCode session starts with; empty until one has run.
pub fn opencode_options() -> Arc<Vec<ConfigOption>> {
    with_opencode(|c| c.options.clone())
}

/// The model (and its reasoning) last picked for an OpenCode session.
pub fn opencode_picked() -> Vec<(Setting, String)> {
    with_opencode(|c| c.picked.clone())
}

/// Keeps what a fresh OpenCode session reported, when it changed: a new
/// provider signed in to brings its models.
pub fn remember_opencode_options(raw: &Value) {
    if parse_config(raw).is_empty() {
        return;
    }
    with_opencode(|c| {
        if c.raw == *raw {
            return;
        }
        c.raw = raw.clone();
        c.options = Arc::new(parse_config(raw));
        write_opencode(c);
    });
}

/// Remembers the model settings picked for an OpenCode session; empty forgets them.
pub fn remember_opencode_pick(picked: Vec<(Setting, String)>) {
    with_opencode(|c| {
        if c.picked != picked {
            c.picked = picked;
            write_opencode(c);
        }
    });
}

/// Asks OpenCode for its options when none are cached and it is installed: a
/// session that never gets a prompt (no tokens), deleted afterwards so it does
/// not show in OpenCode's own list. Blocking; run it off the main thread.
pub fn refresh_opencode() {
    if !opencode_options().is_empty() || launch::opencode_bin().is_err() {
        return;
    }
    match fetch_options(AgentKind::OpenCode) {
        Ok((id, raw)) => {
            remember_opencode_options(&raw);
            if let (false, Ok(bin)) = (id.is_empty(), launch::opencode_bin()) {
                let _ = crate::platform::command(&bin)
                    .args(["session", "delete", &id])
                    .current_dir(std::env::temp_dir().join("arakun-opencode-options"))
                    .env("PATH", launch::child_path_env())
                    .stdin(Stdio::null())
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .status();
            }
        }
        Err(e) => eprintln!("arakun: opencode options: {e:#}"),
    }
}

// ---- Codex's options --------------------------------------------------------

/// Codex's config options (mode, model, reasoning effort, fast mode) as a fresh
/// session starts with them: what `~/.codex/config.toml` sets, kept as raw JSON
/// and parsed.
static CODEX: Mutex<Option<(Value, Arc<Vec<ConfigOption>>)>> = Mutex::new(None);

fn codex_cache_path() -> PathBuf {
    cache_path().with_file_name("codex-options.json")
}

fn with_codex<T>(f: impl FnOnce(&mut (Value, Arc<Vec<ConfigOption>>)) -> T) -> T {
    let mut slot = CODEX.lock().unwrap();
    let cache = slot.get_or_insert_with(|| {
        let raw = std::fs::read(codex_cache_path())
            .ok()
            .and_then(|b| serde_json::from_slice::<Value>(&b).ok())
            .map(|v| v["options"].clone())
            .unwrap_or_default();
        let options = Arc::new(parse_config(&raw));
        (raw, options)
    });
    f(cache)
}

/// The options a fresh Codex session starts with; empty until one has run.
pub fn codex_options() -> Arc<Vec<ConfigOption>> {
    with_codex(|c| c.1.clone())
}

/// Keeps what a fresh Codex session reported, when it changed.
pub fn remember_codex_options(raw: &Value) {
    let options = parse_config(raw);
    if options.is_empty() {
        return;
    }
    with_codex(|c| {
        if c.0 == *raw {
            return;
        }
        let _ = crate::durable::write(
            &codex_cache_path(),
            json!({"fetched": unix_now(), "options": raw})
                .to_string()
                .as_bytes(),
        );
        *c = (raw.clone(), Arc::new(options));
    });
}

/// Asks Codex for its options when none are cached and it is installed: a
/// session that never gets a prompt, so it costs no tokens. Blocking; run it
/// off the main thread.
pub fn refresh_codex() {
    if !codex_options().is_empty() || launch::codex_cli().is_none() {
        return;
    }
    match fetch_options(AgentKind::Codex) {
        Ok((_, raw)) => remember_codex_options(&raw),
        Err(e) => eprintln!("arakun: codex options: {e:#}"),
    }
}

// ---- The agents' own sessions ---------------------------------------------

/// A session an agent keeps on its own, that arakun can open (import).
#[derive(Clone, Debug)]
pub struct AgentSession {
    pub agent: AgentKind,
    pub id: String,
    pub title: String,
    pub cwd: PathBuf,
    /// Unix seconds of the last write to the session file.
    pub updated: u64,
}

/// Droid's directory name for a working directory: `/tmp/a.b` → `-tmp-a.b`.
pub fn session_slug(cwd: &Path) -> String {
    cwd.to_string_lossy().replace('/', "-")
}

/// The top-level sessions Droid has for `cwd`, newest first. Subagent sessions
/// (those with a `callingSessionId`) and sessions with no messages are left out.
pub fn droid_sessions(cwd: &Path) -> Vec<AgentSession> {
    let root = factory_dir().join("sessions");
    let dir = root.join(session_slug(cwd));
    let dirs: Vec<PathBuf> = if dir.is_dir() {
        vec![dir]
    } else {
        // The slug rule is inferred; if it ever differs, fall back to every folder.
        std::fs::read_dir(&root)
            .into_iter()
            .flatten()
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.is_dir())
            .collect()
    };
    let mut out: Vec<AgentSession> = dirs
        .iter()
        .flat_map(|d| std::fs::read_dir(d).into_iter().flatten().flatten())
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "jsonl"))
        .filter_map(|p| read_session(&p))
        .filter(|s| s.cwd == cwd)
        .collect();
    out.sort_by_key(|s| std::cmp::Reverse(s.updated));
    out
}

/// Every session Droid and Claude Code have for `cwd`, newest first.
pub fn agent_sessions(cwd: &Path) -> Vec<AgentSession> {
    let mut out = droid_sessions(cwd);
    out.extend(claude_sessions(cwd));
    out.sort_by_key(|s| std::cmp::Reverse(s.updated));
    out
}

/// Claude Code's folder for a project: `~/.claude/projects/<cwd, every
/// non-alphanumeric as ->`, cut and hashed past 200 characters as the SDK does.
pub fn claude_project_dir(cwd: &Path) -> PathBuf {
    let config = std::env::var_os("CLAUDE_CONFIG_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| home().join(".claude"));
    let path = cwd.to_string_lossy();
    let name: String = path
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    let name = if name.chars().count() <= 200 {
        name
    } else {
        // JavaScript's 32-bit string hash over UTF-16 units, in base 36.
        let hash = path.encode_utf16().fold(0i32, |h, u| {
            h.wrapping_shl(5).wrapping_sub(h).wrapping_add(u as i32)
        });
        let mut n = hash.unsigned_abs();
        let mut digits = Vec::new();
        loop {
            digits.push(std::char::from_digit(n % 36, 36).unwrap_or('0'));
            n /= 36;
            if n == 0 {
                break;
            }
        }
        let head: String = name.chars().take(200).collect();
        format!("{head}-{}", digits.iter().rev().collect::<String>())
    };
    config.join("projects").join(name)
}

/// The sessions Claude Code has for `cwd`, newest first. Ones with no prompt
/// of the user's and archived ones (title `[archived] …`) are left out.
pub fn claude_sessions(cwd: &Path) -> Vec<AgentSession> {
    let dir = claude_project_dir(cwd);
    let mut out: Vec<AgentSession> = std::fs::read_dir(&dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "jsonl"))
        .filter_map(|p| read_claude_session(&p))
        .filter(|s| s.cwd == cwd)
        .collect();
    out.sort_by_key(|s| std::cmp::Reverse(s.updated));
    out
}

/// Reads a Claude Code transcript's start (cwd, first prompt) and end (the
/// latest title) only: they reach tens of megabytes.
fn read_claude_session(path: &Path) -> Option<AgentSession> {
    use std::io::{Read as _, Seek as _, SeekFrom};
    const HEAD: u64 = 256 * 1024;
    const TAIL: u64 = 256 * 1024;
    let id = path.file_stem()?.to_str()?.to_string();
    let mut file = std::fs::File::open(path).ok()?;
    let meta = file.metadata().ok()?;
    let mut head = Vec::new();
    (&mut file).take(HEAD).read_to_end(&mut head).ok()?;
    // The cut may split a character; only the last, partial line suffers.
    let head = String::from_utf8_lossy(&head);
    let (mut cwd, mut prompt) = (None, None);
    for line in head.lines() {
        let Ok(v) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        if cwd.is_none() {
            cwd = v["cwd"].as_str().map(PathBuf::from);
        }
        if prompt.is_none()
            && v["type"] == "user"
            && v["isSidechain"] != true
            && v["isMeta"] != true
        {
            prompt = user_text(&v["message"]["content"]);
        }
        if cwd.is_some() && prompt.is_some() {
            break;
        }
    }
    let prompt = prompt?;
    let mut tail = Vec::new();
    file.seek(SeekFrom::Start(meta.len().saturating_sub(TAIL)))
        .ok()?;
    file.read_to_end(&mut tail).ok()?;
    let tail = String::from_utf8_lossy(&tail);
    let last = |key: &str| {
        tail.lines()
            .rev()
            .filter(|l| l.contains(key))
            .find_map(|l| {
                serde_json::from_str::<Value>(l).ok()?[key]
                    .as_str()
                    .map(str::to_string)
            })
            .filter(|t| !t.trim().is_empty())
    };
    let custom = last("customTitle");
    if custom
        .as_deref()
        .is_some_and(|t| t.starts_with("[archived]"))
    {
        return None;
    }
    let title = custom.or_else(|| last("aiTitle")).unwrap_or(prompt);
    let updated = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_secs());
    Some(AgentSession {
        agent: AgentKind::Claude,
        id,
        title: title
            .lines()
            .next()
            .unwrap_or_default()
            .trim()
            .chars()
            .take(120)
            .collect(),
        cwd: cwd?,
        updated,
    })
}

/// What the user typed in a transcript message: a string, or its text parts.
/// Tool results and commands' wrappers (`<command-name>…`) are not prompts.
fn user_text(content: &Value) -> Option<String> {
    let text = match content {
        Value::String(s) => s.clone(),
        Value::Array(parts) => parts
            .iter()
            .filter(|p| p["type"] == "text")
            .filter_map(|p| p["text"].as_str())
            .collect::<Vec<_>>()
            .join(" "),
        _ => return None,
    };
    let text = text.trim();
    (!text.is_empty() && !text.starts_with('<')).then(|| text.to_string())
}

fn read_session(path: &Path) -> Option<AgentSession> {
    let file = std::fs::File::open(path).ok()?;
    let meta = file.metadata().ok()?;
    let mut first = String::new();
    // The `session_start` line is short; the limit keeps a corrupt file from being read whole.
    BufReader::new(std::io::Read::take(file, 64 * 1024))
        .read_line(&mut first)
        .ok()?;
    if meta.len() <= first.len() as u64 + 1 {
        return None;
    }
    let v: Value = serde_json::from_str(first.trim()).ok()?;
    if v["type"].as_str() != Some("session_start") || v.get("callingSessionId").is_some() {
        return None;
    }
    let id = v["id"].as_str()?.to_string();
    let title = v["title"]
        .as_str()
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .unwrap_or("Untitled session")
        .to_string();
    let updated = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_secs());
    Some(AgentSession {
        agent: AgentKind::Droid,
        id,
        title,
        cwd: PathBuf::from(v["cwd"].as_str()?),
        updated,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn claude_project_dir_matches_the_sdk() {
        let dir = claude_project_dir(Path::new("/home/u/proj"));
        assert!(dir.ends_with("projects/-home-u-proj"));
        let long = format!("/{}", "a".repeat(250));
        let name = claude_project_dir(Path::new(&long))
            .file_name()
            .unwrap()
            .to_string_lossy()
            .into_owned();
        assert!(name.starts_with(&format!("-{}", "a".repeat(199))));
        assert_eq!(name.len(), 200 + 1 + name.rsplit('-').next().unwrap().len());
    }

    #[test]
    fn reads_a_claude_transcript_head_and_tail() {
        let dir = std::env::temp_dir().join(format!("arakun-claude-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("0f0f0f0f-1111-2222-3333-444444444444.jsonl");
        let lines = [
            json!({"type": "queue-operation", "operation": "enqueue"}),
            json!({"type": "user", "cwd": "/w", "isMeta": true, "message": {"content": "<command-name>/init</command-name>"}}),
            json!({"type": "user", "cwd": "/w", "message": {"content": [{"type": "text", "text": "Arregla el login\ncon cuidado"}]}}),
            json!({"type": "ai-title", "aiTitle": "Login roto"}),
            json!({"type": "ai-title", "aiTitle": "Arreglar el login"}),
        ];
        let text: String = lines.iter().map(|l| format!("{l}\n")).collect();
        std::fs::write(&path, &text).unwrap();
        let s = read_claude_session(&path).unwrap();
        assert_eq!(
            (s.id.as_str(), s.title.as_str()),
            ("0f0f0f0f-1111-2222-3333-444444444444", "Arreglar el login")
        );
        assert_eq!(s.cwd, PathBuf::from("/w"));
        // A name the user gave wins; an archived session is left out.
        std::fs::write(
            &path,
            format!(
                "{text}{}\n",
                json!({"type": "custom-title", "customTitle": "Mío"})
            ),
        )
        .unwrap();
        assert_eq!(read_claude_session(&path).unwrap().title, "Mío");
        std::fs::write(
            &path,
            format!(
                "{text}{}\n",
                json!({"type": "custom-title", "customTitle": "[archived] Mío"})
            ),
        )
        .unwrap();
        assert!(read_claude_session(&path).is_none());
        // No prompt of the user's: nothing to import.
        std::fs::write(&path, format!("{}\n", lines[0])).unwrap();
        assert!(read_claude_session(&path).is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
