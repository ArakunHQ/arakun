//! Finding (and, for Claude and Codex, installing) the entry point of each agent:
//! ACP adapters for Claude and Codex, Factory's native `stream-jsonrpc` for Droid
//! unless `ARAKUN_DROID_ACP=1`, and the ACP servers OpenCode and Devin have
//! themselves (`opencode acp`, `devin acp`).

use std::ffi::OsString;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

use anyhow::{Context as _, Result, anyhow, bail};

use super::AgentKind;

/// Pinned so an adapter release can't change the protocol under us; bump deliberately.
pub const CLAUDE_ACP_PACKAGE: &str = "@agentclientprotocol/claude-agent-acp@0.102.0";
/// The Codex adapter brings a matching `@openai/codex`, so the user's own `codex`
/// can be another version without breaking the protocol between them.
pub const CODEX_ACP_PACKAGE: &str = "@agentclientprotocol/codex-acp@2.6.0";

pub struct LaunchSpec {
    pub program: PathBuf,
    pub args: Vec<String>,
    pub env: Vec<(String, OsString)>,
}

impl LaunchSpec {
    pub fn display(&self) -> String {
        let mut s = self.program.display().to_string();
        for a in &self.args {
            s.push(' ');
            s.push_str(a);
        }
        s
    }
}

pub(crate) fn home() -> PathBuf {
    crate::platform::home()
}

/// `~/.local/share/arakun/agents` (`%LOCALAPPDATA%\arakun\agents` on Windows), where adapters
/// are installed and agent stderr is logged.
pub fn agents_dir() -> PathBuf {
    crate::platform::app_dir().join("agents")
}

pub fn log_path(kind: AgentKind) -> PathBuf {
    agents_dir().join(format!("{}.log", kind.id()))
}

/// Opens the agent's log for appending, starting over once it passes 8 MB.
pub fn open_log(kind: AgentKind) -> Option<std::fs::File> {
    let path = log_path(kind);
    std::fs::create_dir_all(path.parent()?).ok()?;
    let too_big = std::fs::metadata(&path).is_ok_and(|m| m.len() > 8 << 20);
    std::fs::OpenOptions::new()
        .create(true)
        .append(!too_big)
        .write(true)
        .truncate(too_big)
        .open(&path)
        .ok()
}

/// Apps started from a compositor keybinding often get a bare PATH without
/// `~/.local/bin`, which is where both CLIs install themselves.
fn search_path() -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = std::env::var_os("PATH")
        .map(|p| std::env::split_paths(&p).collect())
        .unwrap_or_default();
    for extra in extra_dirs() {
        if !dirs.contains(&extra) {
            dirs.push(extra);
        }
    }
    dirs
}

#[cfg(not(windows))]
fn extra_dirs() -> Vec<PathBuf> {
    vec![
        home().join(".local/bin"),
        PathBuf::from("/usr/local/bin"),
        PathBuf::from("/usr/bin"),
    ]
}

/// Where the CLIs land on Windows when the installer's PATH change has not reached arakun
/// yet: Factory's installer, `npm install -g`, and Claude Code's native installer.
#[cfg(windows)]
fn extra_dirs() -> Vec<PathBuf> {
    let mut dirs = vec![home().join(".local").join("bin"), home().join("bin")];
    if let Some(appdata) = std::env::var_os("APPDATA") {
        dirs.push(PathBuf::from(appdata).join("npm"));
    }
    dirs
}

pub fn child_path_env() -> OsString {
    std::env::join_paths(search_path()).unwrap_or_default()
}

fn is_executable(path: &Path) -> bool {
    crate::platform::is_executable(path)
}

/// The interactive sign-in for `kind`, to run in a terminal: Claude Code's
/// `/login` (the user's `claude`, else the binary bundled with the adapter's
/// SDK; both share `~/.claude`), plain `droid`, which asks to sign in,
/// `opencode auth login`, which asks for a provider and its key,
/// `codex login`, which signs in with ChatGPT in the browser, or
/// `devin auth login`, which signs in to Devin in the browser.
pub fn login_command(kind: AgentKind) -> Option<Vec<String>> {
    match kind {
        AgentKind::Claude => {
            let bin = claude_cli()?;
            Some(vec![bin.display().to_string(), "/login".into()])
        }
        AgentKind::Droid => {
            let bin = droid_bin().ok()?;
            Some(vec![bin.display().to_string()])
        }
        AgentKind::OpenCode | AgentKind::WorkersAi => {
            let bin = opencode_bin().ok()?;
            Some(vec![
                bin.display().to_string(),
                "auth".into(),
                "login".into(),
            ])
        }
        AgentKind::Codex => {
            let bin = codex_cli()?;
            Some(vec![bin.display().to_string(), "login".into()])
        }
        AgentKind::Devin => Some(devin_auth_command(&devin_bin().ok()?, "login")),
    }
}

/// The interactive sign-out for `kind`, to run in a terminal: Claude's `/logout`,
/// `opencode auth logout`, which asks which provider, `codex logout` or
/// `devin auth logout`. Droid's
/// CLI is signed out from Factory's side.
pub fn logout_command(kind: AgentKind) -> Option<Vec<String>> {
    match kind {
        AgentKind::Claude => {
            let bin = claude_cli()?;
            Some(vec![bin.display().to_string(), "/logout".into()])
        }
        AgentKind::Droid | AgentKind::WorkersAi => None,
        AgentKind::OpenCode => {
            let bin = opencode_bin().ok()?;
            Some(vec![
                bin.display().to_string(),
                "auth".into(),
                "logout".into(),
            ])
        }
        AgentKind::Codex => {
            let bin = codex_cli()?;
            Some(vec![bin.display().to_string(), "logout".into()])
        }
        AgentKind::Devin => Some(devin_auth_command(&devin_bin().ok()?, "logout")),
    }
}

fn devin_auth_command(bin: &Path, action: &str) -> Vec<String> {
    vec![bin.display().to_string(), "auth".into(), action.into()]
}

/// The user's `claude`, else the binary bundled with the adapter's SDK; both share `~/.claude`.
pub fn claude_cli() -> Option<PathBuf> {
    which("claude").or_else(|| {
        bundled_claude_candidates()?
            .into_iter()
            .find(|p| is_executable(p))
    })
}

/// The user's `codex`, else the one installed with the adapter. Both keep the
/// sign-in in `~/.codex` (or `CODEX_HOME`), so either one signs the adapter in.
pub fn codex_cli() -> Option<PathBuf> {
    which("codex").or_else(|| Some(codex_bundled_cli()).filter(|p| is_executable(p)))
}

fn codex_dir() -> PathBuf {
    agents_dir().join("codex")
}

/// npm writes a `.cmd` launcher for every bin on Windows; the bare name there is a sh script.
fn npm_bin(dir: &Path, name: &str) -> PathBuf {
    let name = if cfg!(windows) {
        format!("{name}.cmd")
    } else {
        name.to_string()
    };
    dir.join("node_modules").join(".bin").join(name)
}

fn codex_adapter_bin() -> PathBuf {
    npm_bin(&codex_dir(), "codex-acp")
}

fn codex_bundled_cli() -> PathBuf {
    npm_bin(&codex_dir(), "codex")
}

/// How Codex is signed in, from the `auth_mode` of its `auth.json`: "ChatGPT"
/// or "API key". Reads that one key, never the tokens.
pub fn codex_sign_in() -> Option<&'static str> {
    let dir = std::env::var_os("CODEX_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .unwrap_or_else(|| home().join(".codex"));
    let v: serde_json::Value =
        serde_json::from_slice(&std::fs::read(dir.join("auth.json")).ok()?).ok()?;
    codex_auth_label(&v)
}

/// The variable that holds Codex's API key when only the environment has one:
/// `CODEX_API_KEY` or `OPENAI_API_KEY`. Names it, never reads the key out.
pub fn codex_env_key() -> Option<&'static str> {
    ["CODEX_API_KEY", "OPENAI_API_KEY"]
        .into_iter()
        .find(|k| std::env::var_os(k).is_some_and(|v| !v.is_empty()))
}

fn codex_auth_label(auth: &serde_json::Value) -> Option<&'static str> {
    match auth["auth_mode"].as_str() {
        Some(m) if m.eq_ignore_ascii_case("chatgpt") => Some("ChatGPT"),
        Some(m) if m.eq_ignore_ascii_case("apikey") => Some("API key"),
        Some(_) => Some("Signed in"),
        // Older files have no mode: tokens mean ChatGPT, a key means a key.
        None if auth["tokens"].is_object() => Some("ChatGPT"),
        None if auth["OPENAI_API_KEY"].is_string() => Some("API key"),
        None => None,
    }
}

/// Codex takes the arakun hint as `developer_instructions`, which the adapter
/// reads from the JSON in `CODEX_CONFIG`. A value the user set keeps its keys;
/// instructions of their own come first.
fn codex_hint_env(cwd: Option<&Path>) -> Option<(String, OsString)> {
    const VAR: &str = "CODEX_CONFIG";
    let hint = agent_hint(cwd)?;
    let config = codex_config(std::env::var(VAR).ok().as_deref(), &hint)?;
    Some((VAR.to_string(), config.to_string().into()))
}

/// `None` when the user's `CODEX_CONFIG` is not a JSON object: theirs wins over the hint.
fn codex_config(theirs: Option<&str>, hint: &str) -> Option<serde_json::Value> {
    let mut config = match theirs {
        Some(raw) => serde_json::from_str::<serde_json::Value>(raw)
            .ok()
            .filter(serde_json::Value::is_object)?,
        None => serde_json::json!({}),
    };
    let text = match config["developer_instructions"].as_str() {
        Some(own) if !own.trim().is_empty() => format!("{own}\n\n{hint}"),
        _ => hint.to_string(),
    };
    config["developer_instructions"] = text.into();
    Some(config)
}

/// Where the adapter's SDK keeps the Claude Code binary for this platform: one
/// npm optional dependency per platform, with a musl twin on Linux. `None` when
/// arakun does not know the package name for this OS or CPU.
fn bundled_claude_candidates() -> Option<Vec<PathBuf>> {
    let os = match std::env::consts::OS {
        "windows" => "win32",
        "macos" => "darwin",
        "linux" => "linux",
        _ => return None,
    };
    let arch = match std::env::consts::ARCH {
        "x86_64" => "x64",
        "aarch64" => "arm64",
        _ => return None,
    };
    let exe = if cfg!(windows) {
        "claude.exe"
    } else {
        "claude"
    };
    let base = agents_dir().join("node_modules").join("@anthropic-ai");
    let mut pkgs = vec![format!("claude-agent-sdk-{os}-{arch}")];
    if os == "linux" {
        pkgs.push(format!("claude-agent-sdk-{os}-{arch}-musl"));
    }
    Some(pkgs.into_iter().map(|p| base.join(p).join(exe)).collect())
}

/// The Claude Code binary to run directly: `CLAUDE_CODE_EXECUTABLE`, the one
/// bundled with the adapter, or one on the PATH. It signs in with the user's
/// Claude account (OAuth), so calls through it count against their plan.
pub fn claude_code_binary() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("CLAUDE_CODE_EXECUTABLE") {
        return Some(PathBuf::from(p));
    }
    bundled_claude_candidates()
        .and_then(|c| c.into_iter().find(|p| is_executable(p)))
        .or_else(|| which("claude"))
}

/// Whether the adapter will find a Claude Code binary to run. npm skips optional
/// dependencies under `omit=optional`, and antivirus software sometimes removes
/// the `.exe`; the adapter then fails every session with "native binary not found".
fn claude_binary_ok() -> bool {
    std::env::var_os("CLAUDE_CODE_EXECUTABLE").is_some()
        || bundled_claude_candidates().is_none_or(|c| c.iter().any(|p| is_executable(p)))
}

pub fn which(name: &str) -> Option<PathBuf> {
    if crate::platform::is_path(name) {
        let p = PathBuf::from(name);
        let dir = p.parent().map(Path::to_path_buf).unwrap_or_default();
        let file = p.file_name()?.to_string_lossy().into_owned();
        return crate::platform::find_in([dir], &file);
    }
    crate::platform::find_in(search_path(), name)
}

/// Whether an override variable names a program. A blank one is ignored, so it
/// must not skip the install prompt either.
fn names_program(raw: Option<&str>) -> bool {
    raw.is_some_and(|r| r.split_whitespace().next().is_some())
}

fn from_override(var: &str) -> Option<Result<LaunchSpec>> {
    let raw = std::env::var(var).ok()?;
    let mut parts = raw.split_whitespace();
    let program = parts.next()?;
    let args = parts.map(str::to_string).collect();
    Some(
        which(program)
            .ok_or_else(|| anyhow!("{var}={raw}: `{program}` is not an executable"))
            .map(|program| LaunchSpec {
                program,
                args,
                env: Vec::new(),
            }),
    )
}

/// Droid speaks its own `stream-jsonrpc` protocol unless `ARAKUN_DROID_ACP` asks
/// for the old ACP bridge (`droid exec --output-format acp`).
pub fn droid_native() -> bool {
    !std::env::var_os("ARAKUN_DROID_ACP").is_some_and(|v| !v.is_empty() && v != "0")
}

pub fn droid_bin() -> Result<PathBuf> {
    let name = std::env::var("ARAKUN_DROID_BIN").unwrap_or_else(|_| "droid".into());
    which(&name).ok_or_else(|| {
        anyhow!("`{name}` was not found on PATH or in ~/.local/bin. Install Factory's CLI first.")
    })
}

/// `droid exec` in stream-jsonrpc mode. `ARAKUN_DROID_BIN` picks another binary.
/// `cwd` is where the session works: its project's memory goes in the hint.
pub fn resolve_droid_native(cwd: Option<&Path>) -> Result<LaunchSpec> {
    Ok(LaunchSpec {
        program: droid_bin()?,
        args: [
            "exec",
            "--input-format",
            "stream-jsonrpc",
            "--output-format",
            "stream-jsonrpc",
        ]
        .map(String::from)
        .into_iter()
        .chain(droid_hint_args(cwd))
        .collect(),
        env: vec![("PATH".into(), child_path_env())],
    })
}

/// Told to the agents arakun starts, so they can show their work in the window,
/// and, for a session working in `cwd`, what its project's memory holds.
/// `ARAKUN_AGENT_HINT=off` leaves their prompts alone; `ARAKUN_MEMORY=off` keeps
/// memory out of them.
pub fn agent_hint(cwd: Option<&Path>) -> Option<String> {
    if std::env::var_os("ARAKUN_AGENT_HINT").is_some_and(|v| v == "off") {
        if std::env::var_os("ARAKUN_MEMORY").is_some_and(|v| v == "on") {
            return cwd.map(crate::memory::prompt_for);
        }
        return None;
    }
    let memory = cwd
        .filter(|_| !std::env::var_os("ARAKUN_MEMORY").is_some_and(|v| v == "off"))
        .map(|dir| format!("\n\n{}", crate::memory::prompt_for(dir)))
        .unwrap_or_default();
    Some(format!("{}{memory}", base_hint()))
}

fn base_hint() -> String {
    format!("{HINT}{LAB_HINT}{HINT_END}")
}

// `arakun lab` exists only in the coderlabs build. Told to an agent in a public
// build, it would run an unknown command, which opens a second window.
#[cfg(feature = "coderlabs")]
const LAB_HINT: &str =
    "- `arakun lab` shows the harness lab dashboard (Arakun Lab evals and experiments).\n";
#[cfg(not(feature = "coderlabs"))]
const LAB_HINT: &str = "";

const HINT: &str = "You are running inside arakun, a desktop app where the user reads this conversation. \
         The `arakun` command on your PATH controls its window, acting on this project:\n\
         - `arakun browse -s <url>` shows a web page (for example a dev server you started) beside the chat.\n\
         - `arakun open -s <file[:line]>` shows a file beside the chat, at that line.\n\
         - `arakun run <command>` starts a long-running command (dev server, watcher) in a terminal \
         the user can see, instead of blocking your own shell. To wait until it is up, put \
         `--port <n>` and/or `--ready <text>` before the command (`arakun run --port 5173 npm run dev`): \
         it returns once the port answers or the text shows in its output, and fails with the output \
         if the command exits first or is not up within `--timeout <secs>` (default 60).\n\
         - `arakun logs [terminal]` prints the recent output of a terminal in that dock (one you started \
         or the user's dev server), to read errors without asking; alone it lists the terminals. \
         `arakun logs <terminal> --until <text> [--timeout <secs>]` waits until the terminal prints the text \
         (a watcher's `compiled` line after an edit) and prints what it printed meanwhile.\n\
         - `arakun page <command>` works with this project's browser tabs the way the user would: \
         `screenshot` (prints a PNG path to look at), `console` (errors and logs since the page loaded), \
         `text`, `open <url>`, `reload`, and on local pages `click <selector|text>`, \
         `type <field> <text>`, `press <key>`, `eval <js>`. Use it to check UI changes you made; \
         `arakun page` alone lists the tabs.\n\
         - `arakun check` runs the project's check command, if the user set one. arakun also runs it after \
         each of your turns that edits files and sends you the output when it fails.\n\
         - `arakun fetch <url>` prints a web page as Markdown (`--html` for HTML, `--links` for its links), \
         loaded in a real browser on Cloudflare (Browser Run), so sites that block curl still answer; \
         `arakun crawl [--limit N] [--depth N] <url>` reads a whole site the same way. Prefer them to curl \
         for reading the web; at most one fetch every 10 s.\n\
         - `arakun rename <branch>` renames this environment's branch, before it is pushed, when the \
         work turns out to be something its name doesn't say.\n\
         - `arakun notify <message>` sends the user a desktop notification.\n\
         - `arakun agent new [--model <provider/model>] [--effort <level>] <prompt>` starts a background agent in this \
         project that the user can watch in the sidebar: OpenCode, or arakun's own Workers AI agent with \
         `--model workers-ai/@cf/deepseek-ai/deepseek-v4.1-flash-1017` or `--model workers-ai/@cf/zai-org/glm-5.3` \
         (an unknown `--model` or `--effort` prints the ones on offer); \
         `arakun agent wait <id>`, `arakun agent log <id>`, `arakun agent send <id> <text>`, \
         `arakun agent stop <id>` and `arakun agent ls` follow it up.\n";

const HINT_END: &str = "Use them when showing something helps the user; they are optional.";

fn droid_hint_args(cwd: Option<&Path>) -> Vec<String> {
    agent_hint(cwd)
        .map(|hint| vec!["--append-system-prompt".to_string(), hint.to_string()])
        .unwrap_or_default()
}

/// `opencode auth login -p opencode-go`: skips the provider list and asks only for the
/// OpenCode Go API key.
pub fn opencode_go_login_command() -> Option<Vec<String>> {
    let bin = opencode_bin().ok()?;
    Some(
        [
            bin.display().to_string(),
            "auth".into(),
            "login".into(),
            "-p".into(),
            "opencode-go".into(),
        ]
        .into(),
    )
}

pub fn opencode_bin() -> Result<PathBuf> {
    let name = std::env::var("ARAKUN_OPENCODE_BIN").unwrap_or_else(|_| "opencode".into());
    which(&name).ok_or_else(|| {
        anyhow!("`{name}` was not found on PATH or in ~/.local/bin. Install OpenCode first (https://opencode.ai).")
    })
}

/// The user's `devin`. Arakun does not install it: Cognition's installer also
/// sets up its updates. `ARAKUN_DEVIN_BIN` picks another binary.
pub fn devin_bin() -> Result<PathBuf> {
    let name = std::env::var("ARAKUN_DEVIN_BIN").unwrap_or_else(|_| "devin".into());
    which(&name).ok_or_else(|| {
        anyhow!(
            "`{name}` was not found on PATH or in ~/.local/bin. Install the Devin CLI first (https://docs.devin.ai/work-with-devin/devin-cli)."
        )
    })
}

/// Whether `devin auth login` has signed in: its credentials file exists.
/// Only checks that it is there, never reads it.
pub fn devin_signed_in() -> bool {
    devin_credentials(std::env::var_os("XDG_DATA_HOME"), &home()).is_file()
}

/// Where Devin keeps its sign-in: `$XDG_DATA_HOME/devin/credentials.toml`,
/// else under `~/.local/share`. A relative `XDG_DATA_HOME` is ignored, as the
/// XDG spec says.
fn devin_credentials(xdg_data: Option<OsString>, home: &Path) -> PathBuf {
    xdg_data
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .unwrap_or_else(|| home.join(".local").join("share"))
        .join("devin")
        .join("credentials.toml")
}

/// The variable with Devin's API key when the environment has one. Names it,
/// never reads the key out.
pub fn devin_env_key() -> Option<&'static str> {
    const VAR: &str = "WINDSURF_API_KEY";
    std::env::var_os(VAR)
        .is_some_and(|v| !v.is_empty())
        .then_some(VAR)
}

/// The providers OpenCode keeps credentials for (`opencode auth login`), by
/// name only: the keys of its `auth.json`, never the secrets.
pub fn opencode_providers() -> Vec<String> {
    let data = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .unwrap_or_else(|| home().join(".local").join("share"));
    let Some(v) = std::fs::read(data.join("opencode").join("auth.json"))
        .ok()
        .and_then(|b| serde_json::from_slice::<serde_json::Value>(&b).ok())
    else {
        return Vec::new();
    };
    v.as_object()
        .map(|o| o.keys().cloned().collect())
        .unwrap_or_default()
}

/// OpenCode takes no system prompt over ACP or its command line, but its config
/// takes `instructions` (files added to the system prompt), and it merges
/// `OPENCODE_CONFIG_CONTENT` over the user's own config, concatenating arrays.
/// So the hint goes to a file per working directory, named in that variable;
/// a value the user set themselves keeps its keys and gets the file appended.
/// The same variable pins the model list to [`OPENCODE_MODELS`].
fn opencode_hint_env(cwd: Option<&Path>) -> Option<(String, OsString)> {
    use std::hash::{Hash as _, Hasher as _};
    const VAR: &str = "OPENCODE_CONFIG_CONTENT";
    let mut config = match std::env::var(VAR) {
        Ok(raw) => match serde_json::from_str::<serde_json::Value>(&raw) {
            Ok(v) if v.is_object() => v,
            // Not plain JSON (JSONC, say): theirs wins over the hint.
            _ => return None,
        },
        Err(_) => serde_json::json!({}),
    };
    pin_opencode_models(&mut config);
    let Some(hint) = agent_hint(cwd) else {
        return Some((VAR.to_string(), config.to_string().into()));
    };
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    cwd.hash(&mut hasher);
    let path = agents_dir()
        .join("opencode")
        .join(format!("hint-{:016x}.md", hasher.finish()));
    let wrote = path
        .parent()
        .map_or(Ok(()), std::fs::create_dir_all)
        .and_then(|_| std::fs::write(&path, hint));
    if let Err(e) = wrote {
        if let Some(mut log) = open_log(AgentKind::OpenCode) {
            let _ = writeln!(
                log,
                "=== could not write the arakun hint to {}: {e}",
                path.display()
            );
        }
        return None;
    }
    let path = serde_json::Value::String(path.display().to_string());
    match config["instructions"].as_array_mut() {
        Some(list) => list.push(path),
        None => config["instructions"] = serde_json::json!([path]),
    }
    Some((VAR.to_string(), config.to_string().into()))
}

/// The only models arakun's OpenCode sessions offer, for now, by provider:
/// OpenCode Go's Gemini Argon (the default) and Kimi K3.1 Pro, and two
/// Cloudflare Workers AI models. `ARAKUN_OPENCODE_ALL_MODELS=1` brings back every provider.
const OPENCODE_MODELS: &[(&str, &[&str])] = &[
    ("opencode-go", &["gemini-4-argon", "kimi-k3.1-pro"]),
    (
        "cloudflare-workers-ai",
        &[
            "@cf/deepseek-ai/deepseek-v4.1-flash-1017",
            "@cf/zai-org/glm-5.3",
        ],
    ),
];

/// `ARAKUN_OPENCODE_ALL_MODELS` lifts the pin.
pub fn opencode_all_models() -> bool {
    std::env::var_os("ARAKUN_OPENCODE_ALL_MODELS").is_some_and(|v| !v.is_empty() && v != "0")
}

/// The pinned models as OpenCode names them, `provider/model`; the provider
/// ends at the first `/`. The first one is the default.
pub fn opencode_models() -> Vec<String> {
    OPENCODE_MODELS
        .iter()
        .flat_map(|(provider, models)| models.iter().map(move |m| format!("{provider}/{m}")))
        .collect()
}

fn pin_opencode_models(config: &mut serde_json::Value) {
    if !opencode_all_models() {
        pin_models(config, OPENCODE_MODELS);
    }
}

/// `enabled_providers` drops the other providers and each `whitelist` the
/// provider's other models, so the ACP model menu lists only these. Keys the
/// user already set win.
fn pin_models(config: &mut serde_json::Value, pins: &[(&str, &[&str])]) {
    let Some(obj) = config.as_object_mut() else {
        return;
    };
    let providers: Vec<&str> = pins.iter().map(|(p, _)| *p).collect();
    obj.entry("enabled_providers")
        .or_insert_with(|| serde_json::json!(providers));
    if let Some((provider, models)) = pins.first() {
        obj.entry("model")
            .or_insert_with(|| format!("{provider}/{}", models[0]).into());
    }
    let table = obj
        .entry("provider")
        .or_insert_with(|| serde_json::json!({}));
    if !table.is_object() {
        return;
    }
    for (provider, models) in pins {
        let entry = &mut table[*provider];
        if entry.is_null() || (entry.is_object() && entry.get("whitelist").is_none()) {
            entry["whitelist"] = serde_json::json!(models);
        }
    }
}

fn claude_adapter_bin() -> PathBuf {
    npm_bin(&agents_dir(), "claude-agent-acp")
}

/// An ACP adapter that has to be installed with npm before `kind` can start.
/// [`resolve`] returns it as its error until the user agrees, so they see what
/// goes where first. The adapter being there is the remembered answer: updates
/// and repairs of an installed one don't ask again.
#[derive(Clone, Debug, PartialEq)]
pub struct AdapterInstall {
    pub kind: AgentKind,
    pub package: &'static str,
    pub dir: PathBuf,
}

/// The flags [`npm_install`] adds. `--include=optional` beats an `omit=optional`
/// in the user's npm config: the Claude Code and Codex binaries are optional
/// dependencies.
const NPM_FLAGS: [&str; 4] = [
    "--include=optional",
    "--no-audit",
    "--no-fund",
    "--loglevel=error",
];

impl AdapterInstall {
    /// The command arakun runs, for the user to read or run by hand.
    pub fn command(&self) -> String {
        let dir = shell_quote(&self.dir.display().to_string());
        format!(
            "npm install --prefix {dir} {} {}",
            NPM_FLAGS[..3].join(" "),
            self.package
        )
    }

    /// The variable that starts another adapter instead.
    pub fn override_var(&self) -> &'static str {
        match self.kind {
            AgentKind::Codex => "ARAKUN_CODEX_ACP",
            _ => "ARAKUN_CLAUDE_ACP",
        }
    }
}

/// Every line stands alone: hosts without the install prompt (the server, the
/// island) show this text as it is.
impl std::fmt::Display for AdapterInstall {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        writeln!(
            f,
            "{} needs its ACP adapter, which is not installed yet.",
            self.kind.label()
        )?;
        writeln!(f, "Package: {}", self.package)?;
        writeln!(f, "Folder: {}", self.dir.display())?;
        writeln!(f, "Command: {}", self.command())?;
        write!(
            f,
            "Install it from the session in Arakun, or run the command yourself and retry. \
             {} starts another adapter instead.",
            self.override_var()
        )
    }
}

impl std::error::Error for AdapterInstall {}

/// `arg` as one word for the user's shell, so a folder with spaces, `$` or quotes
/// in it stays that folder when they paste the command.
fn shell_quote(arg: &str) -> String {
    // A backslash separates folders on Windows but escapes in sh.
    let safe = if cfg!(windows) {
        "\\/._-+:,@"
    } else {
        "/._-+:,@"
    };
    let plain = !arg.is_empty()
        && arg
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || safe.contains(c));
    if plain {
        arg.to_string()
    } else if cfg!(windows) {
        // cmd and PowerShell both take double quotes, and a Windows path can't hold one.
        format!("\"{arg}\"")
    } else {
        // Nothing expands inside single quotes; a quote itself ends, is escaped
        // and starts again.
        format!("'{}'", arg.replace('\'', r"'\''"))
    }
}

/// What has to be installed before `kind` can start, if anything: only the
/// Claude and Codex adapters, and only when no override replaces them.
pub fn adapter_install(kind: AgentKind) -> Option<AdapterInstall> {
    let (var, bin, package, dir) = match kind {
        AgentKind::Claude => (
            "ARAKUN_CLAUDE_ACP",
            claude_adapter_bin(),
            CLAUDE_ACP_PACKAGE,
            agents_dir(),
        ),
        AgentKind::Codex => (
            "ARAKUN_CODEX_ACP",
            codex_adapter_bin(),
            CODEX_ACP_PACKAGE,
            codex_dir(),
        ),
        AgentKind::Droid | AgentKind::OpenCode | AgentKind::Devin | AgentKind::WorkersAi => {
            return None;
        }
    };
    if names_program(std::env::var(var).ok().as_deref()) || is_executable(&bin) {
        return None;
    }
    Some(AdapterInstall { kind, package, dir })
}

/// Resolves how to start `kind` in ACP mode. Blocking: with `install`, a missing
/// Claude or Codex adapter is installed with npm into [`agents_dir`]; without it,
/// the error is an [`AdapterInstall`] that says what would be. `progress` gets a
/// short status line. `ARAKUN_DROID`, `ARAKUN_CLAUDE_ACP`, `ARAKUN_OPENCODE_ACP`,
/// `ARAKUN_CODEX_ACP` and `ARAKUN_DEVIN_ACP` replace the command.
pub fn resolve(
    kind: AgentKind,
    cwd: Option<&Path>,
    install: bool,
    progress: &dyn Fn(String),
) -> Result<LaunchSpec> {
    if let Some(needed) = adapter_install(kind) {
        if !install {
            return Err(needed.into());
        }
        progress(format!(
            "Installing {} into {}…",
            needed.package,
            needed.dir.display()
        ));
    }
    let mut spec = match kind {
        AgentKind::Droid => match from_override("ARAKUN_DROID") {
            Some(spec) => spec?,
            None => LaunchSpec {
                program: droid_bin()?,
                args: ["exec", "--output-format", "acp"]
                    .map(String::from)
                    .into_iter()
                    .chain(droid_hint_args(cwd))
                    .collect(),
                env: Vec::new(),
            },
        },
        AgentKind::Claude => match from_override("ARAKUN_CLAUDE_ACP") {
            Some(spec) => spec?,
            None => {
                let bin = claude_adapter_bin();
                // One repair per run: when an antivirus keeps deleting the binary,
                // reinstalling on every session start would only make each one slow.
                if !is_executable(&bin) {
                    install_claude_adapter()?;
                } else if !claude_binary_ok() && !REPAIR_TRIED.swap(true, Ordering::Relaxed) {
                    progress("Repairing the Claude Code ACP adapter…".into());
                    install_claude_adapter()?;
                } else if !claude_adapter_current() && !UPGRADE_TRIED.swap(true, Ordering::Relaxed)
                {
                    // A bumped pin: without this the first install would stay forever.
                    // If npm fails, the old adapter still works, so keep it.
                    progress("Updating the Claude Code ACP adapter…".into());
                    if let Err(e) = install_claude_adapter()
                        && let Some(mut log) = open_log(AgentKind::Claude)
                    {
                        let _ = writeln!(
                            log,
                            "=== updating to {CLAUDE_ACP_PACKAGE} failed, keeping the old one: {e:#}"
                        );
                    }
                }
                let mut env = Vec::new();
                if !claude_binary_ok() {
                    let name = if cfg!(windows) {
                        "claude.exe"
                    } else {
                        "claude"
                    };
                    let cli = which(name).ok_or_else(|| anyhow!(missing_claude_binary()))?;
                    env.push(("CLAUDE_CODE_EXECUTABLE".to_string(), cli.into_os_string()));
                }
                env.extend(claude_extra_model_env());
                LaunchSpec {
                    program: bin,
                    args: Vec::new(),
                    env,
                }
            }
        },
        AgentKind::OpenCode => {
            let mut spec = match from_override("ARAKUN_OPENCODE_ACP") {
                Some(spec) => spec?,
                None => LaunchSpec {
                    program: opencode_bin()?,
                    args: vec!["acp".into()],
                    env: Vec::new(),
                },
            };
            spec.env.extend(opencode_hint_env(cwd));
            spec
        }
        AgentKind::Codex => {
            let mut spec = match from_override("ARAKUN_CODEX_ACP") {
                Some(spec) => spec?,
                None => {
                    let bin = codex_adapter_bin();
                    if !is_executable(&bin) {
                        install_codex_adapter()?;
                    } else if !adapter_current(&codex_dir(), CODEX_ACP_PACKAGE)
                        && !CODEX_UPGRADE_TRIED.swap(true, Ordering::Relaxed)
                    {
                        progress("Updating the Codex ACP adapter…".into());
                        if let Err(e) = install_codex_adapter()
                            && let Some(mut log) = open_log(AgentKind::Codex)
                        {
                            let _ = writeln!(
                                log,
                                "=== updating to {CODEX_ACP_PACKAGE} failed, keeping the old one: {e:#}"
                            );
                        }
                    }
                    LaunchSpec {
                        program: bin,
                        args: Vec::new(),
                        env: Vec::new(),
                    }
                }
            };
            spec.env.extend(codex_hint_env(cwd));
            // arakun does not call `authenticate`, so a key that is only in the
            // environment needs this. The adapter uses it only when Codex has no
            // sign-in, so a ChatGPT login still comes first.
            if codex_env_key().is_some() && std::env::var_os("DEFAULT_AUTH_REQUEST").is_none() {
                spec.env.push((
                    "DEFAULT_AUTH_REQUEST".into(),
                    r#"{"methodId":"api-key"}"#.into(),
                ));
            }
            spec
        }
        // Devin takes no system prompt over ACP or its command line, so it gets
        // no arakun hint; it reads the project's AGENTS.md itself.
        AgentKind::Devin => match from_override("ARAKUN_DEVIN_ACP") {
            Some(spec) => spec?,
            None => LaunchSpec {
                program: devin_bin()?,
                args: vec!["acp".into()],
                env: Vec::new(),
            },
        },
        // Nothing to start: it runs in arakun (`Connection::in_process`).
        AgentKind::WorkersAi => LaunchSpec {
            program: PathBuf::from("arakun"),
            args: Vec::new(),
            env: Vec::new(),
        },
    };
    spec.env.push(("PATH".into(), child_path_env()));
    Ok(spec)
}

static INSTALL: Mutex<()> = Mutex::new(());
static REPAIR_TRIED: AtomicBool = AtomicBool::new(false);
static UPGRADE_TRIED: AtomicBool = AtomicBool::new(false);
static CODEX_UPGRADE_TRIED: AtomicBool = AtomicBool::new(false);

/// A model Claude Code can run but doesn't list yet, offered as its one custom
/// option (the adapter refuses any value outside the list). Drop it once the
/// list has it; a custom option of the user's own wins.
const CLAUDE_EXTRA_MODEL: (&str, &str) = ("claude-fable-5-2[1m]", "Fable 5.2");

fn claude_extra_model_env() -> Vec<(String, OsString)> {
    if std::env::var_os("ANTHROPIC_CUSTOM_MODEL_OPTION").is_some() {
        return Vec::new();
    }
    let (id, name) = CLAUDE_EXTRA_MODEL;
    vec![
        ("ANTHROPIC_CUSTOM_MODEL_OPTION".into(), id.into()),
        ("ANTHROPIC_CUSTOM_MODEL_OPTION_NAME".into(), name.into()),
        (
            "ANTHROPIC_CUSTOM_MODEL_OPTION_DESCRIPTION".into(),
            format!("{name} · Most capable for your hardest and longest-running tasks").into(),
        ),
    ]
}

/// Whether the installed adapter is the pinned [`CLAUDE_ACP_PACKAGE`] version.
fn claude_adapter_current() -> bool {
    adapter_current(&agents_dir(), CLAUDE_ACP_PACKAGE)
}

/// Whether `package` (`name@version`) is installed under `dir` at that version.
fn adapter_current(dir: &Path, package: &str) -> bool {
    let (name, want) = package.rsplit_once('@').unwrap_or((package, ""));
    std::fs::read_to_string(dir.join("node_modules").join(name).join("package.json"))
        .ok()
        .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
        .is_some_and(|j| j["version"] == want)
}

fn missing_claude_binary() -> String {
    let path = bundled_claude_candidates()
        .and_then(|c| c.into_iter().next())
        .unwrap_or_else(agents_dir);
    format!(
        "Claude Code's own binary is missing: {} is not there, even after reinstalling the adapter. \
         An antivirus may have removed it (allow the folder {} and restart Arakun), \
         or set CLAUDE_CODE_EXECUTABLE to a claude binary. Details are in {}.",
        path.display(),
        agents_dir().display(),
        log_path(AgentKind::Claude).display()
    )
}

fn install_claude_adapter() -> Result<()> {
    let _guard = INSTALL.lock().unwrap_or_else(|e| e.into_inner());
    if is_executable(&claude_adapter_bin()) && claude_binary_ok() && claude_adapter_current() {
        return Ok(());
    }
    let npm = which("npm").ok_or_else(|| {
        anyhow!("npm was not found; it is needed once to install {CLAUDE_ACP_PACKAGE}. Install nodejs and npm, or set ARAKUN_CLAUDE_ACP.")
    })?;
    let dir = agents_dir();
    std::fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
    npm_install(&npm, &dir, CLAUDE_ACP_PACKAGE, AgentKind::Claude)?;
    if !claude_binary_ok() {
        // A lockfile written without this platform's package keeps npm from ever
        // adding it (npm/cli#4828), so start over from nothing once.
        if let Some(mut log) = open_log(AgentKind::Claude) {
            let _ = writeln!(
                log,
                "=== the Claude Code binary is still missing; reinstalling from scratch"
            );
        }
        let _ = std::fs::remove_file(dir.join("package-lock.json"));
        let _ = std::fs::remove_dir_all(dir.join("node_modules"));
        npm_install(&npm, &dir, CLAUDE_ACP_PACKAGE, AgentKind::Claude)?;
    }
    if !is_executable(&claude_adapter_bin()) {
        bail!(
            "npm finished but {} is missing",
            claude_adapter_bin().display()
        );
    }
    Ok(())
}

/// Installs the Codex adapter, and with it the `codex` CLI, when it is missing
/// or old, so a first sign-in works before any session ran. Blocking.
pub fn install_codex() -> Result<()> {
    install_codex_adapter()
}

/// The Codex adapter goes in a folder of its own, so starting the Claude adapter
/// over from nothing leaves it alone.
fn install_codex_adapter() -> Result<()> {
    let _guard = INSTALL.lock().unwrap_or_else(|e| e.into_inner());
    let dir = codex_dir();
    if is_executable(&codex_adapter_bin()) && adapter_current(&dir, CODEX_ACP_PACKAGE) {
        return Ok(());
    }
    let npm = which("npm").ok_or_else(|| {
        anyhow!("npm was not found; it is needed once to install {CODEX_ACP_PACKAGE}. Install nodejs and npm, or set ARAKUN_CODEX_ACP.")
    })?;
    std::fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
    npm_install(&npm, &dir, CODEX_ACP_PACKAGE, AgentKind::Codex)?;
    if !is_executable(&codex_adapter_bin()) {
        bail!(
            "npm finished but {} is missing",
            codex_adapter_bin().display()
        );
    }
    Ok(())
}

fn npm_install(npm: &Path, dir: &Path, package: &str, kind: AgentKind) -> Result<()> {
    let out = crate::platform::command(npm)
        .args(["install", "--prefix"])
        .arg(dir)
        .args(NPM_FLAGS)
        .arg(package)
        .env("PATH", child_path_env())
        .stdin(Stdio::null())
        .output()
        .context("running npm install")?;
    if let Some(mut log) = open_log(kind) {
        let _ = writeln!(log, "=== npm install {package} ({})", out.status);
        let _ = log.write_all(&out.stdout);
        let _ = log.write_all(&out.stderr);
    }
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        let tail: Vec<&str> = err.lines().rev().take(6).collect();
        let tail: Vec<&str> = tail.into_iter().rev().collect();
        bail!(
            "Installing {package} failed ({}):\n{}",
            out.status,
            tail.join("\n")
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn adapter_install_says_what_where_and_how() {
        let plan = AdapterInstall {
            kind: AgentKind::Codex,
            package: CODEX_ACP_PACKAGE,
            dir: PathBuf::from("/data/arakun/agents/codex"),
        };
        assert_eq!(
            plan.command(),
            format!(
                "npm install --prefix /data/arakun/agents/codex --include=optional --no-audit --no-fund {CODEX_ACP_PACKAGE}"
            )
        );
        let text = plan.to_string();
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(
            lines[0],
            "Codex needs its ACP adapter, which is not installed yet."
        );
        assert_eq!(lines[1], format!("Package: {CODEX_ACP_PACKAGE}"));
        assert_eq!(lines[2], "Folder: /data/arakun/agents/codex");
        assert!(lines[4].contains("ARAKUN_CODEX_ACP"));
        // The thread finds the plan in the error `resolve` returns.
        let err: anyhow::Error = plan.clone().into();
        assert_eq!(err.downcast_ref::<AdapterInstall>(), Some(&plan));
    }

    #[cfg(not(windows))]
    #[test]
    fn the_install_folder_stays_one_word_in_the_shell() {
        assert_eq!(shell_quote("/home/a/.local/share"), "/home/a/.local/share");
        assert_eq!(shell_quote("/Users/a b/x"), "'/Users/a b/x'");
        assert_eq!(shell_quote("/home/a$b/x"), "'/home/a$b/x'");
        assert_eq!(shell_quote("/home/a\\b/x"), "'/home/a\\b/x'");
        assert_eq!(shell_quote("/home/o'neil/x"), r"'/home/o'\''neil/x'");
        let plan = AdapterInstall {
            kind: AgentKind::Claude,
            package: CLAUDE_ACP_PACKAGE,
            dir: PathBuf::from("/Users/a b/Library/Application Support/arakun/agents"),
        };
        assert!(plan.command().starts_with(
            "npm install --prefix '/Users/a b/Library/Application Support/arakun/agents' "
        ));
        assert_eq!(plan.override_var(), "ARAKUN_CLAUDE_ACP");
    }

    #[test]
    fn a_blank_override_still_asks_before_installing() {
        assert!(!names_program(None));
        assert!(!names_program(Some("")));
        assert!(!names_program(Some("  \t")));
        assert!(names_program(Some("my-adapter --flag")));
    }

    #[test]
    fn only_the_npm_adapters_are_installed() {
        for kind in [
            AgentKind::Droid,
            AgentKind::OpenCode,
            AgentKind::Devin,
            AgentKind::WorkersAi,
        ] {
            assert_eq!(adapter_install(kind), None);
        }
    }

    #[test]
    fn codex_auth_reads_only_the_mode() {
        let label = |v| codex_auth_label(&v);
        assert_eq!(
            label(serde_json::json!({"auth_mode": "chatgpt"})),
            Some("ChatGPT")
        );
        assert_eq!(
            label(serde_json::json!({"auth_mode": "apikey"})),
            Some("API key")
        );
        assert_eq!(label(serde_json::json!({"tokens": {}})), Some("ChatGPT"));
        assert_eq!(
            label(serde_json::json!({"OPENAI_API_KEY": "k"})),
            Some("API key")
        );
        assert_eq!(label(serde_json::json!({"OPENAI_API_KEY": null})), None);
    }

    #[test]
    fn the_hint_names_lab_only_in_a_build_that_has_it() {
        let hint = base_hint();
        assert_eq!(hint.contains("`arakun lab`"), cfg!(feature = "coderlabs"));
        assert!(hint.ends_with("they are optional."));
    }

    #[test]
    fn codex_config_adds_the_hint_after_the_users_own() {
        assert_eq!(
            codex_config(None, "hint"),
            Some(serde_json::json!({"developer_instructions": "hint"}))
        );
        let theirs = r#"{"model": "gpt-x", "developer_instructions": "be brief"}"#;
        assert_eq!(
            codex_config(Some(theirs), "hint"),
            Some(
                serde_json::json!({"model": "gpt-x", "developer_instructions": "be brief\n\nhint"})
            )
        );
        assert_eq!(codex_config(Some("not json"), "hint"), None);
        assert_eq!(codex_config(Some("[1]"), "hint"), None);
    }

    #[test]
    fn pinned_models_fill_only_what_the_user_left_out() {
        let mut config = serde_json::json!({});
        pin_models(&mut config, OPENCODE_MODELS);
        assert_eq!(
            config,
            serde_json::json!({
                "enabled_providers": ["opencode-go", "cloudflare-workers-ai"],
                "model": "opencode-go/gemini-4-argon",
                "provider": {
                    "opencode-go": { "whitelist": ["gemini-4-argon", "kimi-k3.1-pro"] },
                    "cloudflare-workers-ai": {
                        "whitelist": ["@cf/deepseek-ai/deepseek-v4.1-flash-1017", "@cf/zai-org/glm-5.3"]
                    },
                },
            })
        );
        let mut theirs = serde_json::json!({
            "model": "anthropic/x",
            "provider": { "opencode-go": { "whitelist": ["y"] } },
        });
        pin_models(&mut theirs, OPENCODE_MODELS);
        assert_eq!(theirs["model"], "anthropic/x");
        assert_eq!(
            theirs["provider"]["opencode-go"]["whitelist"],
            serde_json::json!(["y"])
        );
        assert_eq!(
            theirs["provider"]["cloudflare-workers-ai"]["whitelist"][1],
            "@cf/zai-org/glm-5.3"
        );
        assert_eq!(
            opencode_models(),
            [
                "opencode-go/gemini-4-argon",
                "opencode-go/kimi-k3.1-pro",
                "cloudflare-workers-ai/@cf/deepseek-ai/deepseek-v4.1-flash-1017",
                "cloudflare-workers-ai/@cf/zai-org/glm-5.3",
            ]
        );
    }

    // Windows has no absolute path without a drive letter.
    #[cfg(not(windows))]
    #[test]
    fn devin_credentials_follow_xdg_data_home() {
        let home = Path::new("/home/u");
        assert_eq!(
            devin_credentials(None, home),
            Path::new("/home/u/.local/share/devin/credentials.toml")
        );
        assert_eq!(
            devin_credentials(Some("/data".into()), home),
            Path::new("/data/devin/credentials.toml")
        );
        assert_eq!(
            devin_credentials(Some("relative".into()), home),
            Path::new("/home/u/.local/share/devin/credentials.toml")
        );
    }

    #[test]
    fn devin_signs_in_and_out_with_its_auth_command() {
        let bin = Path::new("/home/u/.local/bin/devin");
        assert_eq!(
            devin_auth_command(bin, "login"),
            ["/home/u/.local/bin/devin", "auth", "login"]
        );
        assert_eq!(
            devin_auth_command(bin, "logout"),
            ["/home/u/.local/bin/devin", "auth", "logout"]
        );
    }
}
