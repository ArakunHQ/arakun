//! The `arakun` command line, described once. `arakun help`, each command's
//! `--help` and usage line, the hint agents get and the README's command list
//! are all made from [`COMMANDS`], so they can't drift apart. Parsing stays
//! with each command.

/// Who a command is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Audience {
    /// People: listed by `arakun help`.
    User,
    /// People and agents: listed by `arakun help`, and its `hint` goes into
    /// the note agents get about the command line.
    Agent,
    /// Arakun itself and its tests: listed only by `arakun help --all`.
    Internal,
}

/// One form of a command.
#[derive(Debug)]
pub struct Command {
    /// The words that pick it (`agent run`); `--help` after them prints its
    /// rows, and the rows of the words it starts (`agent` prints them all).
    pub name: &'static str,
    /// What follows `arakun`.
    pub usage: &'static str,
    pub summary: &'static str,
    pub audience: Audience,
    /// The line about it in the agents' note: set for `Audience::Agent` only.
    pub hint: &'static str,
}

const fn user(name: &'static str, usage: &'static str, summary: &'static str) -> Command {
    Command {
        name,
        usage,
        summary,
        audience: Audience::User,
        hint: "",
    }
}

const fn agent(
    name: &'static str,
    usage: &'static str,
    summary: &'static str,
    hint: &'static str,
) -> Command {
    Command {
        name,
        usage,
        summary,
        audience: Audience::Agent,
        hint,
    }
}

const fn internal(name: &'static str, usage: &'static str, summary: &'static str) -> Command {
    Command {
        name,
        usage,
        summary,
        audience: Audience::Internal,
        hint: "",
    }
}

/// Every command, in the order `arakun help` lists them. The commands of the
/// private builds keep their own help text (`lab::HELP`, `team::HELP`).
pub const COMMANDS: &[Command] = &[
    user("", "", "start arakun, or bring up the one running"),
    user(
        "--version",
        "--version",
        "print the version (needs no running Arakun)",
    ),
    user(
        "add",
        "<dir> | add [dir]",
        "add a project (or switch to it) and show it; starts Arakun when it is not running",
    ),
    agent(
        "browse",
        "browse [-s] <url>",
        "open a browser tab (a path opens as file://)",
        "`arakun browse -s <url>` shows a page in the side pane.",
    ),
    agent(
        "open",
        "open [-s] <file[:N]>",
        "open a file tab, at line N",
        "`arakun open -s <file[:line]>` shows a file in the side pane.",
    ),
    user(
        "palette",
        "palette",
        "bring the window up with Ctrl+K open (bind it to a global key, such as Super+K)",
    ),
    agent(
        "run",
        "run [--ready <text>] [--port <n>] [--timeout <secs>] [--] <command…>",
        "run a command in a new terminal of the dock; with --ready, --port or both, wait \
         until its output shows the text (any case) and the port answers, and fail with its \
         output if it exits first or is not up after --timeout seconds (default 60)",
        "`arakun run [--port N] [--ready TEXT] <cmd>` starts a long-running process in a \
         visible dock terminal.",
    ),
    agent(
        "logs",
        "logs [-n N] [term]",
        "print a dock terminal's last N lines (200), by its number or a word of its title; \
         lists them when that is unclear",
        "`arakun logs [term] [--until TEXT]` reads a dock terminal.",
    ),
    user(
        "logs",
        "logs [term] --until <text> [--timeout <secs>]",
        "wait until the terminal prints the text (any case) after this call, and print what \
         it printed; fail if it is not there after --timeout (60 s)",
    ),
    agent(
        "check",
        "check",
        "run the project's check command here, as arakun does after agent turns that edit files",
        "`arakun check` runs the project check command (when configured, arakun also runs it \
         after each turn that edits files and sends you the output on failure).",
    ),
    user(
        "check",
        "check --set <command…>",
        "make it the project's check command",
    ),
    agent(
        "shrink",
        "shrink [--] <command…>",
        "run a command and print a summary of its output (failures of a test run, a log's \
         repeated lines collapsed, JSON's outline); the whole output goes to a file it names. \
         Needs no running Arakun",
        "`arakun shrink -- <cmd>` summarizes verbose test/build output and saves the full \
         log; use it rather than cutting with `| tail`.",
    ),
    agent(
        "rename",
        "rename <branch>",
        "rename this environment's branch (not yet pushed); its folder stays where it is",
        "`arakun rename <branch>` renames this environment's branch.",
    ),
    agent(
        "notify",
        "notify [--] <message…>",
        "send a desktop notification",
        "`arakun notify <msg>` sends a desktop notification.",
    ),
    agent(
        "feedback",
        "feedback --kind <bug|feature|ux|other> <text…|->",
        "tell the Arakun team about a problem or a missing feature in Arakun itself (up to \
         4000 characters; - reads stdin; needs usage telemetry on)",
        "`arakun feedback --kind <bug|feature|ux|other> \"<text>\"` tells the Arakun team \
         about Arakun itself, not this project (plain text, no Markdown/code/paths/secrets).",
    ),
    user(
        "notices",
        "notices [--unseen]",
        "the notices Arakun has shown, newest first",
    ),
    user("ls", "ls", "list projects, environments and their sessions"),
    user(
        "show",
        "show",
        "bring the window up, also when it was closed with Keep running in the background on",
    ),
    user("quit", "quit", "quit Arakun"),
    user(
        "restart",
        "restart",
        "quit and start again, on the installed build; agent turns running go on by \
         themselves after it",
    ),
    user(
        "update",
        "update [--check] [--channel [nightly|stable]]",
        "download the latest release and install it (a running Arakun installs it when it \
         quits); --check only says whether there is one; --channel says which channel this \
         install follows, or picks nightly builds of main (they can break) or releases",
    ),
    agent(
        "fetch",
        "fetch [--html | --links] <url>",
        "a web page as Markdown (or HTML, or its links), loaded in Cloudflare Browser Run: for \
         sites that turn curl away. --set-token <account-id> stores the API token, read from \
         stdin (arakun fetch --help)",
        "`arakun fetch <url>` reads a page. Stop on 403/429/bot challenges.",
    ),
    agent(
        "crawl",
        "crawl [--limit N] [--depth N] [--render] [--include <glob>] [--exclude <glob>] <url>",
        "a site's pages as Markdown, through Browser Run; honors robots.txt and Content Signals",
        "`arakun crawl <url>` reads a site's pages (honoring robots.txt and Content Signals).",
    ),
    agent(
        "agent start",
        "agent start [--agent <agent>] [--headless | --server] [--model <id>] \
         [--effort <level>] [--dir <dir>] [--title <text>] [<flags of new or run>] <prompt…>",
        "start an agent and print its id. Without --headless or --server it starts in the \
         window, listed in the sidebar, and takes the flags of agent new; --agent is claude, \
         opencode or workers-ai there. --headless starts it in a process of its own that goes \
         on with arakun closed (workers-ai only) and takes the flags of agent run. --server \
         starts it on the arakun server running on this machine, with any agent the server \
         runs (arakun server new)",
        "`arakun agent <start|wait|log|send|stop|ls>` runs background agents.",
    ),
    user(
        "agent new",
        "agent new [--model <provider/model>] [--effort <level>] [--mode <id>] [--dir <dir>] \
         [--title <text>] [--ask] <prompt…>",
        "agent start in the window, the agent picked by the model: OpenCode, arakun's own \
         Workers AI agent for a workers-ai/@cf/… model, or Claude Code for a claude/<model> \
         one; --effort sets the reasoning (none, low, high, max: per model); it allows its \
         own tool calls unless --ask (Claude Code always asks in the window)",
    ),
    user(
        "agent ls",
        "agent ls [--all]",
        "list the OpenCode, Workers AI and headless sessions and their status; --all adds \
         the Claude Code and Droid ones",
    ),
    user(
        "agent log",
        "agent log [-n N] <id>",
        "print a session's status and last N entries (30)",
    ),
    user(
        "agent wait",
        "agent wait [--timeout <secs>] <id>",
        "wait until it stops working (at most 3600 s) and print its status and last message; \
         fail on error",
    ),
    user(
        "agent send",
        "agent send <id> <text…>",
        "send it a follow-up",
    ),
    user("agent stop", "agent stop <id>", "stop its running turn"),
    user(
        "agent run",
        "agent run [--resume <id>] [--model <id>] [--dir <path>] [--title <t>] [--effort <e>] \
         [--policy readonly|workspace|full] [--allow net,modal,mail-read] [--read <path>]… \
         [--no-sandbox [--yes]] [--append-system-prompt <file>] \
         [--wait [--timeout <secs>] [--result <file>]] [--] <prompt…>",
        "agent start --headless: a headless agent (Workers AI) in a process of its own that \
         goes on with arakun closed; prints its id. ls, log, wait, send and stop work on it too. The policy \
         (default workspace) picks the tool calls it runs alone; the rest wait for approve or \
         deny, and mail, git push, publishing and system changes always do. It runs in a \
         sandbox without network or credentials: --allow grants those, --read lets it read a \
         path outside its folder, --no-sandbox --yes runs it without one. --resume gives a \
         session a new prompt. --wait blocks until the turn ends and prints its summary as \
         JSON (what would ask is declined)",
    ),
    user(
        "agent pending",
        "agent pending [--json]",
        "the tool calls headless agents wait on you for",
    ),
    user(
        "agent approve",
        "agent approve <id> <req_id>",
        "allow one (it times out denied after 24 h)",
    ),
    user(
        "agent deny",
        "agent deny <id> <req_id> [reason…]",
        "decline one; the agent reads the reason",
    ),
    user(
        "agent rm",
        "agent rm <id>",
        "delete a finished headless session",
    ),
    user(
        "island",
        "island alert <title> [subtitle]",
        "show a card on the Dynamic Island",
    ),
    user(
        "island",
        "island view | later | approve | deny",
        "press the card's button (approve and deny answer a headless agent's request)",
    ),
    user(
        "usage tools",
        "usage tools [--days N] [--json]",
        "where the tokens of Workers AI sessions' tool results go: each tool's calls and \
         tokens, bash by the program it runs (cargo, git…), largest first",
    ),
    user(
        "usage where",
        "usage where [--days N] [--json]",
        "where the tokens of Claude Code sessions go, from their transcripts: unused skills, \
         plugins and MCP servers, long CLAUDE.md files, noisy tools and full sessions, what \
         to fix first (estimates)",
    ),
    user(
        "seat add",
        "seat add <name>",
        "save a Claude seat: a token from `claude setup-token`, read from stdin and checked \
         with one small call; each new Claude Code session then runs on the seat with the \
         most room in its 5-hour window, for life",
    ),
    user(
        "seat ls",
        "seat ls",
        "the seats and their 5-hour and weekly usage",
    ),
    user("seat rm", "seat rm <name>", "forget a seat"),
    user(
        "mem",
        "mem …",
        "memory agents keep across sessions: save, search, get, context, list, forget \
         (arakun mem help)",
    ),
    user(
        "iso",
        "iso …",
        "isometric scenes (.rhai): render, svg, anim, check, docs, example (arakun iso help)",
    ),
    user(
        "peer-review",
        "peer-review <goal>",
        "use one review paired in Teams for this environment and print the team's \
         recommendation",
    ),
    user(
        "github draft",
        "github draft <owner/repo>#<n> --verdict <v> --body-file <file|-> [--head <sha>] \
         [--agent <name>]",
        "save a review as a draft for the GitHub tab (v: approve, request-changes or comment; \
         - reads stdin; --head is the commit read, which an approval needs); nothing is sent \
         to GitHub until you post it there",
    ),
    user(
        "github pilot",
        "github pilot connect <url> --token -",
        "connect arakun-pilot, the pull request agent: checks the Worker, then saves its \
         settings (also status, disconnect; arakun github pilot help)",
    ),
    user(
        "server",
        "server …",
        "arakun without a window, for a machine nobody sits at: it runs agent sessions there, \
         and paired devices follow and answer them (arakun server help)",
    ),
    agent(
        "page",
        "page [-t N]",
        "list the environment's browser tabs (-t N picks one for a command)",
        "`arakun page <screenshot|console|text|open|reload|click|type|press|eval>` drives \
         project browser tabs; use it to check UI changes you made.",
    ),
    user(
        "page",
        "page screenshot [file]",
        "save a PNG of the page and print where",
    ),
    user(
        "page",
        "page console",
        "what the page logged since it loaded",
    ),
    user(
        "page",
        "page text",
        "the page's title, address and visible text",
    ),
    user(
        "page",
        "page open <url> | reload",
        "load a page in the tab and wait for it",
    ),
    user(
        "page",
        "page click <selector|text>",
        "click an element like a user would",
    ),
    user(
        "page",
        "page type <field> <text>",
        "type into a field (CSS selector, label or placeholder)",
    ),
    user(
        "page",
        "page press <key>",
        "press Enter, Tab, Escape, Backspace, an arrow…",
    ),
    user(
        "page",
        "page eval <js>",
        "run JavaScript in the page and print the result. click, type, press and eval only \
         act on pages served from this machine: localhost, 127.0.0.1, file://",
    ),
    internal(
        "island",
        "island [sleeping|idle|working|speaking|listening|list|auto|fps]",
        "show the island in a state, to try it; list pins or unpins the agents' list; auto \
         follows the agents again; fps prints the frames it painted in the last 2 s; with no \
         state, print the one it shows. The island is a program of its own, so these work \
         with arakun closed",
    ),
    #[cfg(windows)]
    internal(
        "island",
        "island session <id> <working|waiting|done|error|ended> [project] [doing]",
        "tell the island about an agent session arakun did not start",
    ),
    #[cfg(windows)]
    internal(
        "island",
        "island hook [agent]",
        "the same from an agent's hook, which writes its event on stdin",
    ),
    internal(
        "island-host",
        "island-host",
        "the island's own program; arakun starts it when none runs",
    ),
    internal(
        "agent-host",
        "agent-host <id>",
        "the process of one headless session; agent run starts it",
    ),
    internal(
        "mcp",
        "mcp",
        "Arakun's MCP server on stdio (request_secret, run_with_secrets, secret_to_file); \
         Arakun starts it for each agent session",
    ),
    internal(
        "dev",
        "dev …",
        "a test arakun on a virtual display, to screenshot and click without touching this \
         screen (arakun dev help)",
    ),
];

/// Where a summary starts, and how wide the help is.
const COLUMN: usize = 31;
const WIDTH: usize = 80;

/// The rows `--help` after `name` prints: its own and those of the words it
/// starts. `None` when no command is called that.
pub fn rows(name: &str) -> Option<Vec<&'static Command>> {
    let rows: Vec<&Command> = COMMANDS
        .iter()
        .filter(|c| {
            c.name == name
                || c.name
                    .strip_prefix(name)
                    .is_some_and(|rest| rest.starts_with(' '))
        })
        .collect();
    (!name.is_empty() && !rows.is_empty()).then_some(rows)
}

/// The usage lines of `name`, for `usage: …` after a mistake.
pub fn usage(name: &str) -> Option<String> {
    let rows = rows(name)?;
    Some(
        rows.iter()
            .map(|c| format!("arakun {}", c.usage))
            .collect::<Vec<_>>()
            .join("\n       "),
    )
}

/// What `--help` after `name` prints.
pub fn help(name: &str) -> Option<String> {
    Some(format(&rows(name)?))
}

/// The command list of `arakun help`: everything but the internal commands,
/// unless `all`.
pub fn list(all: bool) -> String {
    let rows: Vec<&Command> = COMMANDS
        .iter()
        .filter(|c| all || c.audience != Audience::Internal)
        .collect();
    format(&rows)
}

/// The lines about the command line in the note agents get.
pub fn hint() -> String {
    COMMANDS
        .iter()
        .filter(|c| c.audience == Audience::Agent)
        .map(|c| format!("- {}\n", c.hint))
        .collect()
}

/// Two columns: the usage, wrapped after the command's words, and the summary
/// from `COLUMN` on, below the usage when it is too long to share its line.
fn format(rows: &[&Command]) -> String {
    let mut out = String::new();
    for c in rows {
        let lead = format!("  arakun {}", c.name);
        let indent = if c.name.is_empty() {
            9
        } else {
            width(&lead) + 1
        };
        let mut usage = wrap(&words(c.usage), "  arakun ", indent);
        let last = usage.pop().unwrap_or_default();
        for line in usage {
            out.push_str(&line);
            out.push('\n');
        }
        let summary = wrap(
            &c.summary.split(' ').map(str::to_string).collect::<Vec<_>>(),
            &" ".repeat(COLUMN),
            COLUMN,
        );
        let mut summary = summary.into_iter().map(|l| l.trim_start().to_string());
        if width(&last) < COLUMN - 1 {
            let first = summary.next().unwrap_or_default();
            out.push_str(format!("{last:<COLUMN$}{first}").trim_end());
        } else {
            out.push_str(&last);
        }
        out.push('\n');
        for line in summary {
            out.push_str(&format!("{:COLUMN$}{line}\n", ""));
        }
    }
    out
}

/// Columns on a terminal: `…` is one, though three bytes.
fn width(s: &str) -> usize {
    s.chars().count()
}

/// A usage split at spaces outside brackets, so `[--model <id>]` stays whole.
fn words(usage: &str) -> Vec<String> {
    let mut out = Vec::new();
    let (mut word, mut depth) = (String::new(), 0i32);
    for ch in usage.chars() {
        match ch {
            '[' | '<' => depth += 1,
            ']' | '>' => depth -= 1,
            ' ' if depth == 0 => {
                if !word.is_empty() {
                    out.push(std::mem::take(&mut word));
                }
                continue;
            }
            _ => {}
        }
        word.push(ch);
    }
    if !word.is_empty() {
        out.push(word);
    }
    out
}

/// `words` after `first` on lines of up to `WIDTH`, the next ones indented by
/// `indent`. A word longer than a line gets a line of its own.
fn wrap(words: &[String], first: &str, indent: usize) -> Vec<String> {
    let mut lines = vec![first.to_string()];
    let mut fresh = true;
    for w in words {
        let line = lines.last_mut().expect("one line at least");
        if !fresh && width(line) + 1 + width(w) > WIDTH {
            lines.push(format!("{:indent$}{w}", ""));
            continue;
        }
        if !fresh {
            line.push(' ');
        }
        line.push_str(w);
        fresh = false;
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn agent_commands_have_a_hint_and_the_others_none() {
        for c in COMMANDS {
            assert_eq!(
                c.audience == Audience::Agent,
                !c.hint.is_empty(),
                "{}",
                c.usage
            );
            assert!(c.usage.contains(c.name), "{} {}", c.name, c.usage);
        }
    }

    #[test]
    fn help_after_a_word_lists_the_forms_it_starts() {
        let agent = usage("agent").unwrap();
        assert!(agent.starts_with("arakun agent start "));
        assert!(agent.contains("\n       arakun agent new "));
        assert!(agent.contains("\n       arakun agent deny <id> <req_id> [reason…]"));
        assert_eq!(usage("agent rm").unwrap(), "arakun agent rm <id>");
        // `agent` does not start `agent-host`.
        assert!(!agent.contains("agent-host"));
        assert!(usage("nope").is_none());
        assert!(usage("").is_none());
    }

    #[test]
    fn the_list_leaves_the_internal_commands_to_all() {
        assert!(!list(false).contains("island-host"));
        assert!(list(true).contains("  arakun island-host"));
        assert!(!list(false).contains("fps"));
    }

    #[test]
    fn lines_fit_and_summaries_line_up() {
        let all = list(true);
        for line in all.lines() {
            // A line longer than the width is one word too long to break.
            assert!(
                line.chars().count() <= WIDTH || !line.trim().contains(' '),
                "{line}"
            );
        }
        assert_eq!(
            help("open").unwrap(),
            "  arakun open [-s] <file[:N]>  open a file tab, at line N\n"
        );
        // A long usage wraps after the command's words and the summary
        // goes below it.
        let run = help("agent run").unwrap();
        let lines: Vec<&str> = run.lines().collect();
        assert!(lines[0].starts_with("  arakun agent run [--resume <id>]"));
        assert!(lines[1].starts_with("                   ["));
        assert!(
            lines
                .iter()
                .any(|l| l.starts_with(&format!("{:COLUMN$}agent start --headless", "")))
        );
    }

    #[test]
    fn the_hint_names_only_agent_commands() {
        let hint = hint();
        assert!(hint.contains("`arakun shrink -- <cmd>`"));
        assert!(!hint.contains("arakun seat"));
        assert_eq!(
            hint.lines().count(),
            COMMANDS
                .iter()
                .filter(|c| c.audience == Audience::Agent)
                .count()
        );
    }
}
