//! Names for new environments. The "New environment" dialog fills in a random,
//! readable branch name (`env/brave-otter`), so work can start before the user
//! knows what to call it. After the first prompt, a small model run through an
//! agent CLI turns the prompt into a name that says what the work is
//! (`fix/terminal-tab-name`), and arakun renames the branch to it.

use std::io::{Read as _, Write as _};
use std::path::PathBuf;
use std::process::Stdio;
use std::time::{Duration, Instant};

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
    crate::platform::app_dir().join("branch-namer.json")
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

/// Whether `agent`'s CLI is there to name a branch. Looks on disk; call it off the UI thread.
pub fn installed(agent: AgentKind) -> bool {
    match agent {
        AgentKind::Claude => crate::agent::claude_cli().is_some(),
        AgentKind::Droid => crate::agent::droid_bin().is_ok(),
        AgentKind::OpenCode | AgentKind::Codex | AgentKind::WorkersAi => false,
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
    format!(
        "Name a git branch for the task below. Reply with the name only: <type>/<two to five words>, \
         lower case, words joined by hyphens, at most 40 characters. <type> is one of {}. \
         Example: fix/terminal-tab-name-on-resize\n\n<task>\n{task}\n</task>",
        TYPES.join(", "),
    )
}

/// Asks `agent` for a branch name for `task`. Blocks; run it in the background.
pub fn suggest(agent: AgentKind, model: &str, task: &str) -> Result<String> {
    let (program, args) = command(agent, model.trim())?;
    let mut cmd = crate::platform::command(program);
    if agent == AgentKind::Claude {
        crate::agent::claude_auth::one_shot(&mut cmd)
            .map_err(|e| anyhow!("could not hand Claude Code its key helper: {e}"))?;
    }
    // An empty folder: no project instructions, memory or tools to load.
    let dir = private_dir().map_err(|e| anyhow!("could not make a folder for the namer: {e}"))?;
    cmd.args(args)
        .current_dir(&dir)
        .env("PATH", crate::agent::child_path_env())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    // On stdin, not in argv: other local users can read a process's arguments, and
    // Windows refuses arguments with line breaks for a `.cmd` such as npm's `claude.cmd`.
    let out = run(cmd, instruction(task));
    let _ = std::fs::remove_dir_all(&dir);
    let out = out?;
    parse(&out).ok_or_else(|| anyhow!("the model answered with no usable name: {}", out.trim()))
}

/// A new folder only this user can open, for one naming run. A fixed name in a shared
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

/// The CLI and its arguments to answer the prompt on its stdin once, and exit.
fn command(agent: AgentKind, model: &str) -> Result<(PathBuf, Vec<String>)> {
    // Checked first, so an agent that can't name branches fails without a lookup.
    let args = args(agent, model)?;
    let program = match agent {
        AgentKind::Claude => {
            crate::agent::claude_cli().ok_or_else(|| anyhow!("Claude Code is not installed"))?
        }
        _ => crate::agent::droid_bin()?,
    };
    Ok((program, args))
}

/// The arguments [`command`] runs the CLI with. Kept apart from finding the CLI, which
/// reads the user's home folder, so tests can check them on any machine.
fn args(agent: AgentKind, model: &str) -> Result<Vec<String>> {
    let model = (!model.is_empty()).then(|| ["--model".to_string(), model.to_string()]);
    let args = match agent {
        AgentKind::Claude => {
            let mut args: Vec<String> = vec!["-p".into()];
            args.extend(
                [
                    "--tools",
                    "",
                    "--no-session-persistence",
                    "--strict-mcp-config",
                    "--setting-sources",
                    "",
                    "--output-format",
                    "text",
                ]
                .map(String::from),
            );
            args.extend(model.into_iter().flatten());
            args
        }
        // `droid exec` without `--auto` is read-only: it may read files and run read-only
        // commands, but not change anything. The reply is cut down to a branch name.
        AgentKind::Droid => {
            let mut args = vec!["exec".to_string()];
            args.extend(model.into_iter().flatten());
            args
        }
        AgentKind::OpenCode | AgentKind::Codex | AgentKind::WorkersAi => {
            bail!("{} cannot name branches", agent.label())
        }
    };
    Ok(args)
}

/// Runs `cmd` with `input` on its stdin and returns its stdout; kills it after [`TIMEOUT`].
fn run(mut cmd: std::process::Command, input: String) -> Result<String> {
    let mut child = cmd
        .spawn()
        .map_err(|e| anyhow!("could not start the namer: {e}"))?;
    if let Some(mut stdin) = child.stdin.take() {
        // Dropped at the end, which closes it: the CLI reads to the end before answering.
        std::thread::spawn(move || {
            let _ = stdin.write_all(input.as_bytes());
        });
    }
    let read = |pipe: Option<Box<dyn std::io::Read + Send>>| {
        std::thread::spawn(move || {
            let mut text = String::new();
            if let Some(mut pipe) = pipe {
                let _ = pipe.read_to_string(&mut text);
            }
            text
        })
    };
    let stdout = read(child.stdout.take().map(|p| Box::new(p) as _));
    let stderr = read(child.stderr.take().map(|p| Box::new(p) as _));
    let start = Instant::now();
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }
        if start.elapsed() > TIMEOUT {
            let _ = child.kill();
            let _ = child.wait();
            bail!("the namer did not answer within {} s", TIMEOUT.as_secs());
        }
        std::thread::sleep(Duration::from_millis(100));
    };
    let out = stdout.join().unwrap_or_default();
    if !status.success() {
        let err = stderr.join().unwrap_or_default();
        let err = err
            .lines()
            .rev()
            .find(|l| !l.trim().is_empty())
            .unwrap_or("")
            .trim();
        bail!(
            "the namer failed ({status}){}",
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
        assert!(args(AgentKind::OpenCode, "").is_err());
        assert!(command(AgentKind::OpenCode, "").is_err());
    }

    #[test]
    fn claude_command_has_no_tools() {
        let args = args(AgentKind::Claude, "haiku").unwrap();
        assert_eq!(args[0], "-p");
        assert!(args.windows(2).any(|w| w == ["--tools", ""]));
        assert!(args.ends_with(&["--model".into(), "haiku".into()]));
    }

    #[test]
    fn droid_command_reads_only() {
        let args = args(AgentKind::Droid, "").unwrap();
        assert_eq!(args, ["exec"], "no --auto: droid exec stays read-only");
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
