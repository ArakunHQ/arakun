//! Names for new environments. The "New environment" dialog fills in a random,
//! readable branch name (`env/brave-otter`), so work can start before the user
//! knows what to call it. After the first prompt, a small model run through an
//! agent CLI turns the prompt into a name that says what the work is
//! (`fix/terminal-tab-name`), and arakun renames the branch to it.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::AtomicBool;
use std::time::Duration;

use anyhow::{Result, anyhow, bail};
use serde::{Deserialize, Serialize};

use crate::agent::AgentKind;

/// Every random name starts with this, so it is easy to spot as a placeholder.
pub const PREFIX: &str = "env/";

const ADJECTIVES: &[&str] = &[
    "amber", "bold", "brave", "bright", "calm", "clever", "cosy", "crisp", "curious", "eager",
    "fair", "fancy", "gentle", "glad", "golden", "grand", "happy", "humble", "jolly", "keen",
    "kind", "lively", "lucky", "merry", "mild", "misty", "neat", "nimble", "noble", "proud",
    "quick", "quiet", "rapid", "shiny", "silent", "silver", "sleek", "snowy", "sunny", "swift",
    "tidy", "warm", "wild", "wise", "witty", "young", "zesty", "plucky",
];

const NOUNS: &[&str] = &[
    "badger", "beaver", "birch", "brook", "canyon", "cedar", "comet", "coral", "crane", "dune",
    "falcon", "fern", "finch", "fjord", "fox", "glade", "harbor", "hare", "heron", "lark", "lynx",
    "maple", "meadow", "moose", "moss", "orca", "otter", "owl", "panda", "pebble", "pine",
    "plover", "puffin", "quail", "raven", "reef", "river", "robin", "sparrow", "spruce", "summit",
    "swan", "tiger", "trout", "tundra", "valley", "walrus", "willow",
];

/// The kinds of change a branch name may start with, as in Conventional Commits.
const TYPES: &[&str] = &[
    "feat", "fix", "docs", "refactor", "perf", "test", "build", "ci", "chore",
];

/// A random name such as `env/brave-otter`, or `env/brave-otter-42` when the short
/// ones `taken` says are in use keep coming up.
pub fn random(taken: impl Fn(&str) -> bool) -> String {
    for attempt in 0..64 {
        let [a, n, d] = random_numbers();
        let adjective = ADJECTIVES[a as usize % ADJECTIVES.len()];
        let noun = NOUNS[n as usize % NOUNS.len()];
        let name = if attempt < 8 {
            format!("{PREFIX}{adjective}-{noun}")
        } else {
            format!("{PREFIX}{adjective}-{noun}-{}", d % 90 + 10)
        };
        if !taken(&name) {
            return name;
        }
    }
    // 2304 pairs times 90 numbers all taken: only the clock is left.
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.subsec_nanos());
    format!("{PREFIX}env-{nanos:x}")
}

fn random_numbers() -> [u32; 3] {
    let mut bytes = [0u8; 12];
    if getrandom::fill(&mut bytes).is_err() {
        // No OS randomness: the clock still gives different names each time.
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos());
        bytes.copy_from_slice(&nanos.to_le_bytes()[..12]);
    }
    let word = |i: usize| u32::from_le_bytes([bytes[i], bytes[i + 1], bytes[i + 2], bytes[i + 3]]);
    [word(0), word(4), word(8)]
}

/// Whether `name` is one [`random`] could have made.
#[cfg(test)]
fn is_random(name: &str) -> bool {
    let Some(rest) = name.strip_prefix(PREFIX) else {
        return false;
    };
    let mut parts = rest.split('-');
    let (Some(adjective), Some(noun)) = (parts.next(), parts.next()) else {
        return false;
    };
    let number = parts.next();
    ADJECTIVES.contains(&adjective)
        && NOUNS.contains(&noun)
        && parts.next().is_none()
        && number.is_none_or(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
}

// ---- the namer -------------------------------------------------------------------

/// Who names a new environment's branch after its first prompt.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Choice {
    /// The agent the first prompt went to, when it is one of [`AGENTS`], so the prompt
    /// reaches no service the user didn't pick. Other agents leave the branch alone.
    #[default]
    Session,
    Off,
    Claude,
    Droid,
}

impl Choice {
    pub const ALL: [Choice; 4] = [Choice::Session, Choice::Off, Choice::Claude, Choice::Droid];

    pub fn label(self) -> &'static str {
        match self {
            Choice::Session => "Session's agent",
            Choice::Off => "Off",
            Choice::Claude => AgentKind::Claude.label(),
            Choice::Droid => AgentKind::Droid.label(),
        }
    }

    /// The agent picked for every environment, whatever its sessions use.
    pub fn agent(self) -> Option<AgentKind> {
        match self {
            Choice::Claude => Some(AgentKind::Claude),
            Choice::Droid => Some(AgentKind::Droid),
            Choice::Session | Choice::Off => None,
        }
    }
}

/// Who names branches, and with what model.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Namer {
    #[serde(default)]
    pub choice: Choice,
    /// The model of an agent picked in Settings; empty uses the CLI's own default.
    /// Following the session uses [`default_model`].
    #[serde(default)]
    pub model: String,
}

impl Namer {
    /// The agent and model that name a branch whose first prompt went to `session`;
    /// None when nobody does.
    pub fn pick(&self, session: AgentKind) -> Option<(AgentKind, String)> {
        match self.choice {
            Choice::Off => None,
            Choice::Session => AGENTS
                .contains(&session)
                .then(|| (session, default_model(session).to_string())),
            Choice::Claude | Choice::Droid => self.choice.agent().map(|a| (a, self.model.clone())),
        }
    }
}

/// The agents that can name a branch: CLIs that answer one prompt, then exit, without
/// running tools. OpenCode is not one: `opencode run` may run shell commands and edit
/// files by default, and its permission config differs between versions.
pub const AGENTS: [AgentKind; 2] = [AgentKind::Claude, AgentKind::Droid];

/// A small, fast model for each CLI. Droid's model ids depend on the user's plan, so it
/// starts on the CLI's default.
pub fn default_model(agent: AgentKind) -> &'static str {
    match agent {
        AgentKind::Claude => "haiku",
        _ => "",
    }
}

fn namer_path() -> PathBuf {
    crate::paths::store(crate::paths::BRANCH_NAMER)
}

/// The namer set in this run. `set_namer` saves in the background, so the
/// file can lag behind it, and waiting for the disk would stall the window.
static SET: std::sync::Mutex<Option<Namer>> = std::sync::Mutex::new(None);

pub fn namer() -> Namer {
    let set = SET
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if let Some(namer) = set.as_ref() {
        return namer.clone();
    }
    std::fs::read(namer_path())
        .ok()
        .and_then(|b| serde_json::from_slice::<Namer>(&b).ok())
        .unwrap_or_default()
}

/// Whether `agent` can [`ask`]: its CLI is there, or Workers AI has its key. Looks on
/// disk; call it off the UI thread.
pub fn installed(agent: AgentKind) -> bool {
    match agent {
        AgentKind::WorkersAi => crate::agent::workers_ai::endpoint().is_ok(),
        _ => program(agent).is_ok(),
    }
}

pub fn set_namer(namer: &Namer) {
    // Kept in memory too, so `namer` gives it back at once, and for this run
    // even when the file cannot be written.
    *SET.lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(namer.clone());
    // Saved on every key typed in the model field.
    crate::durable::write_soon(
        namer_path(),
        serde_json::to_vec_pretty(namer).unwrap_or_default(),
        "could not save the branch namer",
    );
}

/// The longest part of the first prompt the model reads; the start says what the task is.
const TASK_CHARS: usize = 4000;
/// A name longer than this is cut at a word.
const MAX_LEN: usize = 48;
const TIMEOUT: Duration = Duration::from_secs(90);

fn instruction(task: &str) -> String {
    let task: String = task.chars().take(TASK_CHARS).collect();
    // The model may be another provider's, and its answer becomes a branch
    // that can be pushed.
    let task = crate::secrets::scrub(&task);
    format!(
        "Name a git branch for the task below. Reply with the name only: <type>/<two to five words>, \
         lower case, words joined by hyphens, at most 40 characters. <type> is one of {}. \
         Example: fix/terminal-tab-name-on-resize\n\n<task>\n{task}\n</task>",
        TYPES.join(", "),
    )
}

/// Asks `agent` for a branch name for `task`. Blocks; run it in the background.
pub fn suggest(agent: AgentKind, model: &str, task: &str) -> Result<String> {
    let out = ask(agent, model, instruction(task), TIMEOUT)?;
    parse(&out).ok_or_else(|| anyhow!("the model answered with no usable name: {}", out.trim()))
}

/// Asks `agent` one question and returns its whole answer. A CLI runs once, in an
/// empty folder, as locked down as it lets itself be (see [`plan`]), and exits;
/// Workers AI is asked by Arakun itself, with no tools in the request. Blocks; run
/// it in the background.
pub fn ask(agent: AgentKind, model: &str, input: String, timeout: Duration) -> Result<String> {
    ask_until(agent, model, input, timeout, &AtomicBool::new(false))
}

/// [`ask`] that stops the agent once `stop` is set: its answer is no longer
/// wanted, and a CLI left running would spend tokens for nothing.
pub fn ask_until(
    agent: AgentKind,
    model: &str,
    input: String,
    timeout: Duration,
    stop: &AtomicBool,
) -> Result<String> {
    let model = model.trim();
    if agent == AgentKind::WorkersAi {
        // One blocking request that can't be called back once sent: stop before it.
        if stop.load(std::sync::atomic::Ordering::Relaxed) {
            return Err(anyhow!("the agent was stopped"));
        }
        return crate::agent::workers_ai::ask(model, &input, timeout).map_err(|e| anyhow!(e));
    }
    // Looked up first, so a missing CLI fails before a folder is made.
    let program = program(agent)?;
    // An empty folder: no project instructions, memory or files to load.
    let dir = private_dir().map_err(|e| anyhow!("could not make a folder for the agent: {e}"))?;
    let out = ask_in(agent, program, model, input, timeout, &dir, stop);
    let _ = std::fs::remove_dir_all(&dir);
    out
}

fn ask_in(
    agent: AgentKind,
    program: PathBuf,
    model: &str,
    input: String,
    timeout: Duration,
    dir: &Path,
    stop: &AtomicBool,
) -> Result<String> {
    let plan = plan(agent, model, dir)?;
    let mut cmd = crate::platform::command(program);
    if agent == AgentKind::Claude {
        crate::agent::claude_auth::one_shot(&mut cmd)
            .map_err(|e| anyhow!("could not hand Claude Code its key helper: {e}"))?;
    }
    for (key, value) in &plan.env {
        cmd.env(key, value);
    }
    cmd.args(&plan.args)
        .current_dir(dir)
        .env("PATH", crate::agent::child_path_env())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    // On stdin or in a file, not in argv: other local users can read a process's
    // arguments, and Windows refuses arguments with line breaks for a `.cmd` such as
    // npm's `claude.cmd`.
    let stdin = match &plan.prompt_file {
        Some(file) => {
            std::fs::write(file, input)
                .map_err(|e| anyhow!("could not write the prompt for the agent: {e}"))?;
            String::new()
        }
        None => input,
    };
    let out = run(cmd, stdin, timeout, stop)?;
    // A reply file holds the answer alone; stdout may carry more around it.
    Ok(plan
        .reply_file
        .and_then(|f| std::fs::read_to_string(f).ok())
        .filter(|r| !r.trim().is_empty())
        .unwrap_or(out))
}

/// A new folder only this user can open, for one run. A fixed name in a shared
/// temporary folder could be made first by another user, with a CLAUDE.md or AGENTS.md
/// in it that the agent would read; creating fails rather than reuse a folder that exists.
fn private_dir() -> std::io::Result<PathBuf> {
    let [a, b, _] = random_numbers();
    let dir = std::env::temp_dir().join(format!("arakun-namer-{a:08x}{b:08x}"));
    // On Windows the folder keeps the access rules of the temporary folder.
    #[cfg(unix)]
    std::os::unix::fs::DirBuilderExt::mode(&mut std::fs::DirBuilder::new(), 0o700).create(&dir)?;
    #[cfg(not(unix))]
    std::fs::DirBuilder::new().create(&dir)?;
    Ok(dir)
}

/// The CLI that answers for `agent`.
fn program(agent: AgentKind) -> Result<PathBuf> {
    use crate::agent;
    match agent {
        AgentKind::Claude => {
            agent::claude_cli().ok_or_else(|| anyhow!("Claude Code is not installed"))
        }
        AgentKind::Codex => agent::codex_cli().ok_or_else(|| anyhow!("Codex is not installed")),
        AgentKind::Droid => agent::droid_bin(),
        AgentKind::OpenCode => agent::opencode_bin(),
        AgentKind::Devin => agent::devin_bin(),
        AgentKind::Cursor => agent::cursor_bin(),
        // agy has no one-shot mode to answer with.
        AgentKind::Antigravity => bail!("Antigravity cannot name branches"),
        AgentKind::WorkersAi => bail!("Workers AI has no CLI to run"),
    }
}

/// How one CLI answers once: its arguments, what it adds to the environment, and
/// where the prompt and the answer go when not through stdin and stdout.
#[derive(Debug, Default)]
struct Plan {
    args: Vec<String>,
    env: Vec<(&'static str, String)>,
    prompt_file: Option<PathBuf>,
    reply_file: Option<PathBuf>,
}

/// An OpenCode agent with every tool off and every permission denied. Its own
/// agent, not settings over the default one: the user's config can allow a tool
/// for `build`, and that would win over a setting for every agent.
const OPENCODE_AGENT: &str = "arakun-answer";
const OPENCODE_CONFIG: &str = r#"{"agent":{"arakun-answer":{"mode":"primary","description":"Answers once, without tools.","tools":{"*":false},"permission":{"*":"deny"}}}}"#;

/// What Cursor reads as its prompt; the real one comes after it on stdin.
const CURSOR_PROMPT: &str = "Do what the text on standard input asks, and reply only as it says.";

/// The arguments for `agent` to answer once with as little reach as its CLI allows,
/// in `dir`. Kept apart from finding the CLI, which reads the user's home folder, so
/// tests can check them on any machine.
///
/// - Claude Code: every tool off, no MCP servers, no settings files.
/// - OpenCode: [`OPENCODE_CONFIG`], and no plugins.
/// - Codex: a read-only sandbox, no shell, no web search, and none of the user's
///   config (MCP servers, rules).
/// - Droid: `droid exec` without `--auto`, which only reads.
/// - Cursor: ask mode, which only reads, in its sandbox.
/// - Devin: the default permission mode, which lets only reads in `dir` run without
///   asking; print mode can't ask, so nothing else runs.
fn plan(agent: AgentKind, model: &str, dir: &Path) -> Result<Plan> {
    let model_flag = |flag: &str| {
        (!model.is_empty())
            .then(|| [flag.to_string(), model.to_string()])
            .into_iter()
            .flatten()
    };
    let strings = |a: &[&str]| a.iter().map(|s| s.to_string()).collect::<Vec<_>>();
    let mut plan = Plan::default();
    match agent {
        AgentKind::Claude => {
            plan.args = strings(&[
                "-p",
                "--tools",
                "",
                "--no-session-persistence",
                "--strict-mcp-config",
                "--setting-sources",
                "",
                "--output-format",
                "text",
            ]);
            plan.args.extend(model_flag("--model"));
        }
        AgentKind::Droid => {
            plan.args = strings(&["exec"]);
            plan.args.extend(model_flag("--model"));
        }
        AgentKind::OpenCode => {
            plan.args = strings(&["run", "--pure", "--agent", OPENCODE_AGENT]);
            plan.args.extend(model_flag("--model"));
            plan.env
                .push(("OPENCODE_CONFIG_CONTENT", OPENCODE_CONFIG.to_string()));
        }
        AgentKind::Codex => {
            let reply = dir.join("reply.txt");
            plan.args = strings(&[
                "exec",
                "--sandbox",
                "read-only",
                "--skip-git-repo-check",
                "--ephemeral",
                "--ignore-user-config",
                "--ignore-rules",
                "--disable",
                "shell_tool",
                "--disable",
                "unified_exec",
                "-c",
                "web_search=\"disabled\"",
                "--color",
                "never",
                "--output-last-message",
            ]);
            plan.args.push(reply.display().to_string());
            plan.args.extend(model_flag("--model"));
            plan.args.push("-".into());
            plan.reply_file = Some(reply);
        }
        AgentKind::Cursor => {
            plan.args = strings(&[
                "--print",
                "--mode",
                "ask",
                "--sandbox",
                "enabled",
                "--trust",
                "--output-format",
                "text",
            ]);
            plan.args.extend(model_flag("--model"));
            plan.args.push(CURSOR_PROMPT.into());
        }
        AgentKind::Devin => {
            let prompt = dir.join("prompt.md");
            plan.args = strings(&["--prompt-file"]);
            plan.args.push(prompt.display().to_string());
            plan.args.extend(strings(&[
                "--permission-mode",
                "normal",
                "--respect-workspace-trust",
                "false",
            ]));
            plan.args.extend(model_flag("--model"));
            // Last: it takes an optional prompt after it.
            plan.args.push("--print".into());
            plan.prompt_file = Some(prompt);
        }
        AgentKind::Antigravity => bail!("Antigravity cannot name branches"),
        AgentKind::WorkersAi => bail!("Workers AI has no CLI to run"),
    }
    Ok(plan)
}

/// Runs `cmd` with `input` on its stdin and returns its stdout; stops it, and what it
/// started, after `timeout`.
fn run(
    mut cmd: std::process::Command,
    input: String,
    timeout: Duration,
    stop: &AtomicBool,
) -> Result<String> {
    // The CLI reads its stdin to the end before answering; it is closed once written.
    let output = crate::platform::output_until(&mut cmd, Some(input.as_bytes()), timeout, stop)
        .map_err(|e| {
            if e.kind() == std::io::ErrorKind::TimedOut {
                anyhow!("the agent did not answer within {} s", timeout.as_secs())
            } else if e.kind() == std::io::ErrorKind::Interrupted {
                anyhow!("the agent was stopped")
            } else {
                anyhow!("could not start the agent: {e}")
            }
        })?;
    let status = output.status;
    let out = String::from_utf8_lossy(&output.stdout).into_owned();
    if !status.success() {
        let err = String::from_utf8_lossy(&output.stderr);
        let err = err
            .lines()
            .rev()
            .find(|l| !l.trim().is_empty())
            .unwrap_or("")
            .trim();
        bail!(
            "the agent failed ({status}){}",
            if err.is_empty() {
                String::new()
            } else {
                format!(": {err}")
            }
        );
    }
    Ok(out)
}

/// The branch name in a model's reply: its last line that reads as one, cleaned up.
/// A name without a known type gets `feat/`.
pub fn parse(reply: &str) -> Option<String> {
    reply.lines().rev().find_map(clean)
}

fn clean(line: &str) -> Option<String> {
    let line = line
        .trim()
        .trim_matches(|c: char| matches!(c, '`' | '"' | '\'' | '*' | '.' | ' '));
    if line.is_empty() || line.split_whitespace().count() > 1 {
        return None;
    }
    let line = line.to_lowercase();
    let (kind, words) = match line.split_once('/') {
        Some((kind, words)) if TYPES.contains(&kind) => (kind, words),
        Some(_) => return None,
        None => ("feat", line.as_str()),
    };
    let slug: String = words
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    let mut name = format!("{kind}/");
    for word in slug.split('-').filter(|w| !w.is_empty()) {
        let room = MAX_LEN - name.len();
        if name.ends_with('/') {
            // The slug is ASCII, so any byte is a char boundary.
            name.push_str(&word[..word.len().min(room)]);
        } else if word.len() < room {
            name.push('-');
            name.push_str(word);
        } else {
            break;
        }
    }
    (!name.ends_with('/')).then_some(name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_branch_namer_never_reads_a_secret() {
        let text = instruction("deploy with ghp_abcdefghijklmnopqrstuvwxyz0123456789 to staging");
        assert!(!text.contains("ghp_abcdefghijklmnopqrstuvwxyz0123456789"));
        assert!(text.contains("deploy with [secret] to staging"));
    }

    /// A namer that leaves a child running must not hold the call until the child ends.
    #[cfg(unix)]
    #[test]
    fn a_namer_that_leaves_a_child_running_returns_when_it_ends() {
        let mut cmd = crate::platform::command("sh");
        cmd.args(["-c", "cat; sleep 30 &"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let started = std::time::Instant::now();
        let go = AtomicBool::new(false);
        assert_eq!(run(cmd, "fix/x".into(), TIMEOUT, &go).unwrap(), "fix/x");
        assert!(started.elapsed() < Duration::from_secs(10));
    }

    /// A dialog closed while its agent writes stops the agent, not after the timeout.
    #[cfg(unix)]
    #[test]
    fn an_agent_whose_answer_nobody_wants_is_stopped() {
        let mut cmd = crate::platform::command("sh");
        cmd.args(["-c", "sleep 30"]);
        let stop = std::sync::Arc::new(AtomicBool::new(false));
        let later = stop.clone();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(200));
            later.store(true, std::sync::atomic::Ordering::Relaxed);
        });
        let started = std::time::Instant::now();
        let err = run(cmd, String::new(), TIMEOUT, &stop).unwrap_err();
        assert_eq!(err.to_string(), "the agent was stopped");
        assert!(started.elapsed() < Duration::from_secs(10));
    }

    /// A dialog closed before its agent starts: the agent never runs.
    #[cfg(unix)]
    #[test]
    fn an_agent_stopped_before_it_starts_never_runs() {
        let dir = std::env::temp_dir().join(format!("arakun-stopped-{}", std::process::id()));
        let _ = std::fs::remove_file(&dir);
        let mut cmd = crate::platform::command("sh");
        cmd.arg("-c").arg(format!("touch '{}'", dir.display()));
        let err = run(cmd, String::new(), TIMEOUT, &AtomicBool::new(true)).unwrap_err();
        assert_eq!(err.to_string(), "the agent was stopped");
        std::thread::sleep(Duration::from_millis(200));
        assert!(!dir.exists());
    }

    #[test]
    fn random_names_read_as_random() {
        for _ in 0..50 {
            let name = random(|_| false);
            assert!(is_random(&name), "{name}");
        }
        assert!(is_random("env/brave-otter-42"));
        assert!(!is_random("env/brave"));
        assert!(!is_random("env/brave-otter-x"));
        assert!(!is_random("env/brave-otter-42-1"));
        assert!(!is_random("feature/brave-otter"));
        assert!(!is_random("env/purple-otter"));
    }

    #[test]
    fn random_skips_taken_names() {
        let name = random(|n| n.matches('-').count() == 1);
        assert!(is_random(&name));
        assert_eq!(name.matches('-').count(), 2, "{name}");
    }

    #[test]
    fn parses_replies() {
        assert_eq!(
            parse("fix/terminal-tab-name-on-resize\n").as_deref(),
            Some("fix/terminal-tab-name-on-resize")
        );
        assert_eq!(parse("`Feat/Dark_Mode Toggle`").as_deref(), None);
        assert_eq!(
            parse("`Feat/Dark_Mode-toggle`").as_deref(),
            Some("feat/dark-mode-toggle")
        );
        assert_eq!(
            parse("Sure! Here it is:\n\n**docs/readme-install**").as_deref(),
            Some("docs/readme-install")
        );
        assert_eq!(
            parse("add-settings-page").as_deref(),
            Some("feat/add-settings-page")
        );
        assert_eq!(parse("bug/crash").as_deref(), None);
        assert_eq!(parse("fix/").as_deref(), None);
        assert_eq!(parse("").as_deref(), None);
        let long =
            parse("refactor/one-two-three-four-five-six-seven-eight-nine-ten-eleven").unwrap();
        assert!(
            long.len() <= MAX_LEN && long.starts_with("refactor/one-two"),
            "{long}"
        );
        assert!(!long.ends_with('-'));
        let one_word = parse(&format!("fix/{}", "a".repeat(60))).unwrap();
        assert_eq!(one_word, format!("fix/{}", "a".repeat(MAX_LEN - 4)));
    }

    #[test]
    fn namer_folders_are_new_and_private() {
        let (a, b) = (private_dir().unwrap(), private_dir().unwrap());
        assert_ne!(a, b);
        assert_eq!(std::fs::read_dir(&a).unwrap().count(), 0);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let mode = std::fs::metadata(&a).unwrap().permissions().mode();
            assert_eq!(mode & 0o077, 0, "{mode:o}");
        }
        let _ = std::fs::remove_dir(a);
        let _ = std::fs::remove_dir(b);
    }

    #[test]
    fn opencode_never_names_branches() {
        assert!(!AGENTS.contains(&AgentKind::OpenCode));
        assert_eq!(
            Namer::default().pick(AgentKind::OpenCode),
            None,
            "it only answers the commit and pull request writers"
        );
    }

    fn plan_of(agent: AgentKind, model: &str) -> Plan {
        plan(agent, model, Path::new("/x")).unwrap()
    }

    #[test]
    fn claude_command_has_no_tools() {
        let args = plan_of(AgentKind::Claude, "haiku").args;
        assert_eq!(args[0], "-p");
        assert!(args.windows(2).any(|w| w == ["--tools", ""]));
        assert!(args.ends_with(&["--model".into(), "haiku".into()]));
    }

    #[test]
    fn droid_command_reads_only() {
        let args = plan_of(AgentKind::Droid, "").args;
        assert_eq!(args, ["exec"], "no --auto: droid exec stays read-only");
    }

    #[test]
    fn opencode_answers_as_an_agent_without_tools() {
        let p = plan_of(AgentKind::OpenCode, "opencode/m");
        assert_eq!(
            p.args,
            [
                "run",
                "--pure",
                "--agent",
                OPENCODE_AGENT,
                "--model",
                "opencode/m"
            ]
        );
        let (key, config) = &p.env[0];
        assert_eq!(*key, "OPENCODE_CONFIG_CONTENT");
        let config: serde_json::Value = serde_json::from_str(config).unwrap();
        let agent = &config["agent"][OPENCODE_AGENT];
        assert_eq!(agent["tools"]["*"], false);
        assert_eq!(agent["permission"]["*"], "deny");
        assert!(p.prompt_file.is_none() && p.reply_file.is_none());
    }

    #[test]
    fn codex_reads_only_without_a_shell_or_the_users_config() {
        let p = plan_of(AgentKind::Codex, "");
        let a = &p.args;
        assert_eq!(a[0], "exec");
        assert!(a.windows(2).any(|w| w == ["--sandbox", "read-only"]));
        assert!(a.windows(2).any(|w| w == ["--disable", "shell_tool"]));
        assert!(a.windows(2).any(|w| w == ["--disable", "unified_exec"]));
        assert!(a.contains(&"--ignore-user-config".into()));
        assert!(
            !a.iter()
                .any(|x| x.contains("dangerously") || x == "--full-auto")
        );
        // The prompt on stdin, the answer in a file in the folder.
        assert_eq!(a.last().unwrap(), "-");
        let reply = Path::new("/x").join("reply.txt");
        assert_eq!(p.reply_file.as_deref(), Some(reply.as_path()));
        assert!(a.contains(&reply.display().to_string()));
    }

    #[test]
    fn cursor_asks_in_its_sandbox() {
        let a = plan_of(AgentKind::Cursor, "").args;
        assert!(a.windows(2).any(|w| w == ["--mode", "ask"]));
        assert!(a.windows(2).any(|w| w == ["--sandbox", "enabled"]));
        assert!(!a.iter().any(|x| x == "--force" || x == "--yolo"));
        // Only a pointer in argv: the prompt itself goes on stdin.
        assert_eq!(a.last().unwrap(), CURSOR_PROMPT);
    }

    #[test]
    fn devin_reads_its_prompt_from_a_file_and_asks_for_the_rest() {
        let p = plan_of(AgentKind::Devin, "opus");
        let file = Path::new("/x").join("prompt.md");
        assert_eq!(p.prompt_file.as_deref(), Some(file.as_path()));
        assert!(
            p.args
                .windows(2)
                .any(|w| w == ["--permission-mode", "normal"])
        );
        assert!(p.args.windows(2).any(|w| w == ["--model", "opus"]));
        assert_eq!(p.args.last().unwrap(), "--print");
    }

    #[test]
    fn workers_ai_runs_no_cli() {
        assert!(plan(AgentKind::WorkersAi, "", Path::new("/x")).is_err());
        assert!(program(AgentKind::WorkersAi).is_err());
    }

    #[test]
    #[ignore = "live: runs Claude Code with haiku on this machine's sign-in"]
    fn live_claude_names_a_branch_from_stdin() {
        let name = suggest(
            AgentKind::Claude,
            "haiku",
            "The terminal tab loses its name\nwhen the window is resized.",
        )
        .unwrap();
        assert!(name.starts_with("fix/"), "{name}");
    }

    #[test]
    fn by_default_the_prompt_goes_only_to_the_sessions_agent() {
        let namer = Namer::default();
        assert_eq!(namer.choice, Choice::Session);
        assert_eq!(
            namer.pick(AgentKind::Claude),
            Some((AgentKind::Claude, "haiku".into()))
        );
        assert_eq!(
            namer.pick(AgentKind::Droid),
            Some((AgentKind::Droid, String::new()))
        );
        assert_eq!(namer.pick(AgentKind::OpenCode), None);
        assert_eq!(namer.pick(AgentKind::WorkersAi), None);
        let off = Namer {
            choice: Choice::Off,
            model: String::new(),
        };
        assert_eq!(off.pick(AgentKind::Claude), None);
        let claude = Namer {
            choice: Choice::Claude,
            model: "sonnet".into(),
        };
        assert_eq!(
            claude.pick(AgentKind::OpenCode),
            Some((AgentKind::Claude, "sonnet".into()))
        );
        // A file from before the choice existed follows the session.
        let old: Namer = serde_json::from_str(r#"{"agent":"claude","model":"haiku"}"#).unwrap();
        assert_eq!(old.choice, Choice::Session);
    }
}
