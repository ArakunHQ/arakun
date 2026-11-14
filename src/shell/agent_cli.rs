//! `arakun agent …`: OpenCode and Workers AI sessions an agent starts and follows from its
//! shell (see `crate::control`). They are ordinary sessions, listed in their
//! environment's sidebar, so the user can open one and watch it work.

use std::path::{Path, PathBuf};

use gpui::{App, Context, SharedString, Window};

use super::Shell;
use super::sessions::{opencode_draft, status_of};
use crate::agent::{AgentKind, Entry, Setting, Thread, ThreadStatus, ToolCall, ToolContent, ToolStatus, workers_ai};
use crate::control::{self, Reply, Request};
use crate::model::{self, Status};
use crate::phone::wire;
use crate::state;

impl Shell {
    pub(super) fn agent_command(&mut self, req: &Request, window: &mut Window, cx: &mut Context<Self>) -> Reply {
        let arg = |i: usize| req.args.get(i).map_or("", String::as_str);
        let sub = arg(0);
        if sub == "new" {
            return self.agent_new(req, window, cx);
        }
        if sub == "ls" {
            return Reply::ok(self.agent_listing(arg(1) == "all", cx));
        }
        let (env_path, id) = match self.find_session(arg(1)) {
            Ok(found) => found,
            Err(e) => return Reply::err(e),
        };
        match sub {
            "status" => {
                let (status, _, error) = self.agent_state(&env_path, &id, cx);
                let last = match error {
                    Some(e) => e,
                    None if status == Status::Working => String::new(),
                    None => self
                        .with_entries(&id, cx, |entries| {
                            entries.iter().rev().find_map(|e| match e {
                                Entry::Agent { text } if !text.trim().is_empty() => Some(text.trim().to_string()),
                                _ => None,
                            })
                        })
                        .flatten()
                        .unwrap_or_default(),
                };
                Reply::ok(format!("{}\n{last}", wire::status(status)))
            }
            "log" => {
                let lines = arg(2).parse().unwrap_or(control::AGENT_LOG);
                let (status, model, error) = self.agent_state(&env_path, &id, cx);
                let title = self.find_env(&env_path).and_then(|e| Some(e.sessions[e.session_index(&id)?].title.clone()));
                let mut out = format!(
                    "{id}  {}  {model}  {}  {}",
                    wire::status(status),
                    title.unwrap_or_default(),
                    env_path.display()
                );
                if let Some(e) = error {
                    out.push_str(&format!("\nerror: {e}"));
                }
                let log = self.with_entries(&id, cx, |entries| render_log(entries, lines)).unwrap_or_default();
                out.push_str("\n\n");
                out.push_str(if log.is_empty() { "(nothing yet)" } else { &log });
                Reply::ok(out)
            }
            "send" => {
                if self.send_to_session(&env_path, &id, arg(2).to_string(), window, cx) {
                    Reply::ok(format!("sent to {id}"))
                } else {
                    Reply::err(format!("could not start the agent of {id}"))
                }
            }
            "stop" => {
                let thread = self.session_mut(&env_path, &id).and_then(|s| s.live.as_ref().map(|l| l.thread.clone()));
                match thread {
                    Some(t) if cli_status(t.read(cx)) == Status::Working => {
                        t.update(cx, |t, cx| t.cancel(cx));
                        Reply::ok(format!("stopping {id}"))
                    }
                    _ => Reply::ok(format!("{id} is not working")),
                }
            }
            other => Reply::err(format!("unknown command `arakun agent {other}` (see arakun help)")),
        }
    }

    /// `arakun agent new`, from `[new, model, dir, title, ask, prompt, effort]`
    /// (clients before `--effort` leave the effort out).
    fn agent_new(&mut self, req: &Request, window: &mut Window, cx: &mut Context<Self>) -> Reply {
        let (model, dir, title, ask, prompt, effort, mode) = match &req.args[..] {
            [_, model, dir, title, ask, prompt] => (model, dir, title, ask, prompt, "", ""),
            [_, model, dir, title, ask, prompt, effort] => (model, dir, title, ask, prompt, effort.as_str(), ""),
            [_, model, dir, title, ask, prompt, effort, mode] => (model, dir, title, ask, prompt, effort.as_str(), mode.as_str()),
            _ => return Reply::err("arakun agent new: bad request (see arakun help)"),
        };
        // `claude/<model>`: a Claude Code session, visible in the app, its permissions
        // asked there as always (never auto-allowed from here).
        if let Some(claude) = model.strip_prefix("claude/") {
            let offered: Vec<String> = crate::agent::acp_option(&crate::agent::catalog::claude_options(), Setting::Model)
                .map(|o| o.options.iter().map(|c| c.id.to_string()).collect())
                .unwrap_or_default();
            if !offered.is_empty() && !offered.iter().any(|m| m == claude) {
                return Reply::err(format!("Claude doesn't offer {claude}; pick one of: {}", offered.join(", ")));
            }
            let dir = if dir.is_empty() { req.cwd.clone() } else { PathBuf::from(dir) };
            let env_path = match self.env_holding(&dir, window, cx) {
                Ok(path) => path,
                Err(e) => return Reply::err(e),
            };
            let title: SharedString = match title.as_str() {
                "" => head(prompt.lines().map(str::trim).find(|l| !l.is_empty()).unwrap_or(""), TITLE_CHARS),
                t => t.to_string(),
            }
            .into();
            let mut extra = vec![(Setting::Model, claude.to_string())];
            if !effort.is_empty() {
                extra.push((Setting::Effort, effort.to_string()));
            }
            // The permission mode (default, plan, acceptEdits, bypassPermissions):
            // a session asked to bypass is still asked about its tools in the app.
            if !mode.is_empty() {
                extra.push((Setting::Mode, mode.to_string()));
            }
            let Some(id) = self.start_session_with(&env_path, model::Agent::Claude, title, prompt.clone(), false, extra, window, cx) else {
                return Reply::err(format!("could not start a session in {}", env_path.display()));
            };
            self.save(cx);
            cx.notify();
            return Reply::ok(format!("{id}  claude/{claude}  {}", env_path.display()));
        }
        let native = model.strip_prefix(workers_ai::CLI_PREFIX).and_then(workers_ai::model);
        if native.is_none() && !model.is_empty() && !crate::agent::opencode_all_models() {
            let offered = crate::agent::opencode_models();
            if !offered.contains(model) {
                let all = [offered, workers_ai::cli_models()].concat();
                return Reply::err(format!("arakun does not offer {model}; pick one of:\n  {}", all.join("\n  ")));
            }
        }
        let dir = if dir.is_empty() { req.cwd.clone() } else { PathBuf::from(dir) };
        let env_path = match self.env_holding(&dir, window, cx) {
            Ok(path) => path,
            Err(e) => return Reply::err(e),
        };
        let title: SharedString = match title.as_str() {
            "" => head(prompt.lines().map(str::trim).find(|l| !l.is_empty()).unwrap_or(""), TITLE_CHARS),
            t => t.to_string(),
        }
        .into();
        // Not remembered for the environment: the user's next session starts on their own pick.
        let (mut extra, picked) = if let Some(m) = native {
            (vec![(Setting::Model, m.id.to_string())], model.clone())
        } else if model.is_empty() {
            let draft = self
                .find_env(&env_path)
                .and_then(|e| opencode_draft(e).into_iter().find(|(s, _)| *s == Setting::Model).map(|(_, v)| v));
            let default = crate::agent::acp_option(&crate::agent::catalog::opencode_options(), Setting::Model)
                .map(|o| o.current.clone())
                .filter(|m| !m.is_empty());
            let pinned = crate::agent::opencode_models().into_iter().next().filter(|_| !crate::agent::opencode_all_models());
            (Vec::new(), draft.or(default).or(pinned).unwrap_or_else(|| "default".into()))
        } else {
            (vec![(Setting::Model, model.clone())], model.clone())
        };
        if let Err(e) = check_effort(native, &picked, effort) {
            return Reply::err(e);
        }
        if !effort.is_empty() {
            extra.push((Setting::Effort, effort.to_string()));
        }
        let agent = if native.is_some() { model::Agent::WorkersAi } else { model::Agent::OpenCode };
        let started = self.start_session_with(&env_path, agent, title, prompt.clone(), false, extra, window, cx);
        let Some(id) = started else {
            return Reply::err(format!("could not start a session in {}", env_path.display()));
        };
        let auto_allow = ask.is_empty();
        if let Some(s) = self.session_mut(&env_path, &id) {
            s.cli_model = Some(picked.clone());
            s.auto_allow = auto_allow;
            if let Some(live) = &s.live {
                live.thread.update(cx, |t, _| t.set_auto_allow(auto_allow));
            }
        }
        self.save(cx);
        cx.notify();
        Reply::ok(format!("{id}  {picked}  {}", env_path.display()))
    }

    /// The environment holding `dir`. A folder of no project becomes one,
    /// quietly: what is on screen stays as it is.
    fn env_holding(&mut self, dir: &Path, window: &mut Window, cx: &mut Context<Self>) -> Result<PathBuf, String> {
        if let Some(env) = self.env_for(dir) {
            return Ok(env);
        }
        if !dir.is_dir() {
            return Err(format!("not a directory: {}", dir.display()));
        }
        let root = crate::git::main_root(dir).unwrap_or_else(|_| dir.to_path_buf());
        let pi = match self.projects.iter().position(|p| p.path == root) {
            Some(pi) => pi,
            None => {
                self.projects.push(state::new_project(&root));
                self.projects.len() - 1
            }
        };
        // Its worktrees now, so a folder in one of them finds its environment.
        if let Ok(trees) = crate::git::worktrees(&root) {
            self.projects[pi].sync_worktrees(trees);
        }
        self.refresh_project(pi, cx);
        self.sync_ci_roots(cx);
        self.sync_poll(window, cx);
        self.save(cx);
        cx.notify();
        self.env_for(dir).ok_or_else(|| format!("{} is in no environment of {}", dir.display(), root.display()))
    }

    /// The session `id` names, by its id or the start of one, in any environment.
    fn find_session(&self, id: &str) -> Result<(PathBuf, SharedString), String> {
        if id.is_empty() {
            return Err("say which session (`arakun agent ls` lists them)".into());
        }
        let all: Vec<(&Path, &SharedString)> = self
            .projects
            .iter()
            .flat_map(|p| &p.environments)
            .flat_map(|e| e.sessions.iter().map(move |s| (e.path.as_path(), &s.id)))
            .collect();
        let hits: Vec<_> = match all.iter().find(|(_, s)| s.as_ref() == id) {
            Some(exact) => vec![exact],
            None => all.iter().filter(|(_, s)| s.starts_with(id)).collect(),
        };
        match hits[..] {
            [(path, sid)] => Ok((path.to_path_buf(), (*sid).clone())),
            [] => Err(format!("no session {id} (`arakun agent ls` lists them)")),
            _ => Err(format!("more than one session starts with {id}")),
        }
    }

    /// A session's status, model and error, from its agent when it is running.
    fn agent_state(&self, env_path: &Path, id: &str, cx: &App) -> (Status, String, Option<String>) {
        let Some(s) = self.find_env(env_path).and_then(|e| Some(&e.sessions[e.session_index(id)?])) else {
            return (Status::Idle, "-".into(), None);
        };
        let saved = s.cli_model.clone().filter(|m| !m.is_empty()).unwrap_or_else(|| "-".into());
        let Some(live) = &s.live else {
            return (s.status, saved, None);
        };
        let t = live.thread.read(cx);
        let model = t
            .setting(Setting::Model)
            .map(|(m, _)| m)
            .filter(|m| !m.is_empty())
            .map(|m| if t.kind() == AgentKind::WorkersAi { format!("{}{m}", workers_ai::CLI_PREFIX) } else { m })
            .unwrap_or(saved);
        let error = match t.status() {
            ThreadStatus::Error(e) => Some(e.lines().next().unwrap_or("").trim().to_string()),
            _ => None,
        };
        (cli_status(t), model, error)
    }

    /// `f` over a session's entries: its agent's when it is running, else its transcript's.
    fn with_entries<R>(&self, id: &str, cx: &App, f: impl FnOnce(&[Entry]) -> R) -> Option<R> {
        let live = self
            .projects
            .iter()
            .flat_map(|p| &p.environments)
            .flat_map(|e| &e.sessions)
            .find(|s| s.id.as_ref() == id)
            .and_then(|s| s.live.as_ref());
        if let Some(live) = live {
            return Some(f(live.thread.read(cx).entries()));
        }
        let path = state::transcript_path(id);
        let transcript = path.exists().then(|| Thread::read_transcript(&path).ok()).flatten()?;
        Some(f(&transcript.entries))
    }

    fn agent_listing(&self, all: bool, cx: &App) -> String {
        let mut rows: Vec<(u64, String)> = Vec::new();
        for e in self.projects.iter().flat_map(|p| &p.environments) {
            let listed = |s: &&model::Session| {
                (all || matches!(s.agent, model::Agent::OpenCode | model::Agent::WorkersAi)) && !crate::headless::shown(&s.id)
            };
            for s in e.sessions.iter().filter(listed) {
                let (status, model, _) = self.agent_state(&e.path, &s.id, cx);
                let cli = if s.cli_model.is_some() { "  [cli]" } else { "" };
                let kind = if all { format!("{:?}  ", s.agent) } else { String::new() };
                rows.push((
                    s.updated,
                    format!("{}  {}  {kind}{model}  {}  {}{cli}", s.id, wire::status(status), s.title, e.path.display()),
                ));
            }
        }
        if rows.is_empty() {
            return "no OpenCode or Workers AI sessions; `arakun agent new <prompt>` starts one".into();
        }
        rows.sort_by_key(|(updated, _)| std::cmp::Reverse(*updated));
        rows.into_iter().map(|(_, row)| row).collect::<Vec<_>>().join("\n")
    }
}

/// What a session is doing, for an agent waiting on it: a prompt still waiting
/// for the agent to connect already counts as work.
fn cli_status(t: &Thread) -> Status {
    match status_of(t) {
        Status::Idle if !t.queued().is_empty() && !t.queue_paused() => Status::Working,
        s => s,
    }
}

/// An `--effort` the model can't take, with the ones it can. OpenCode's levels
/// are known only for the model its sessions start on.
fn check_effort(native: Option<&workers_ai::Model>, model: &str, effort: &str) -> Result<(), String> {
    if effort.is_empty() {
        return Ok(());
    }
    let offered: Vec<String> = match native {
        Some(m) => m.efforts.iter().map(|e| e.to_string()).collect(),
        None => {
            let options = crate::agent::catalog::opencode_options();
            let default = crate::agent::acp_option(&options, Setting::Model).map(|o| o.current.clone());
            if !model.is_empty() && default.as_deref() != Some(model) {
                return Ok(());
            }
            match crate::agent::acp_option(&options, Setting::Effort) {
                Some(o) => o.options.iter().map(|c| c.id.clone()).collect(),
                None => return Err(format!("{} takes no --effort", if model.is_empty() { "this model" } else { model })),
            }
        }
    };
    if offered.iter().any(|e| e == effort) {
        Ok(())
    } else {
        let name = if model.is_empty() { "the default model" } else { model };
        Err(format!("{name} takes no --effort {effort}; pick one of: {}", offered.join(", ")))
    }
}

const TITLE_CHARS: usize = 60;
const USER_CHARS: usize = 1000;
const THOUGHT_CHARS: usize = 300;
const OUTPUT_LINES: usize = 8;
const OUTPUT_CHARS: usize = 800;

/// A session's last `n` entries as plain text, for `arakun agent log`.
pub(crate) fn render_log(entries: &[Entry], n: usize) -> String {
    let shown: Vec<String> = entries.iter().filter_map(render_entry).collect();
    shown[shown.len().saturating_sub(n)..].join("\n\n")
}

fn render_entry(e: &Entry) -> Option<String> {
    Some(match e {
        Entry::User { text, .. } => format!("you: {}", head(text.trim(), USER_CHARS)),
        Entry::Agent { text } if !text.trim().is_empty() => format!("agent: {}", text.trim()),
        Entry::Thought { text } if !text.trim().is_empty() => format!("thought: {}", head(text.trim(), THOUGHT_CHARS)),
        Entry::Tool(t) => {
            let status = match t.status {
                ToolStatus::Pending => "pending",
                ToolStatus::InProgress => "in_progress",
                ToolStatus::Completed => "completed",
                ToolStatus::Failed => "failed",
            };
            let mut out = format!("[{}] {status}", t.title);
            if let Some(code) = t.exit_code.filter(|c| *c != 0) {
                out.push_str(&format!(" (exit {code})"));
            }
            if let Some(command) = tool_command(t) {
                out.push_str(&format!("\n$ {command}"));
            }
            let output = tool_output(t);
            if !output.trim().is_empty() {
                out.push('\n');
                out.push_str(&tail(output.trim_end(), OUTPUT_LINES, OUTPUT_CHARS));
            }
            out
        }
        Entry::Plan { entries } => {
            let mut out = "plan:".to_string();
            for p in entries {
                let mark = match p.status.as_str() {
                    "completed" => "x",
                    "in_progress" => "~",
                    _ => " ",
                };
                out.push_str(&format!("\n  [{mark}] {}", p.content));
            }
            out
        }
        Entry::Notice { text, error, .. } => format!("{}: {}", if *error { "error" } else { "notice" }, text.trim()),
        Entry::Compacted { .. } => "(context compacted)".into(),
        _ => return None,
    })
}

/// The shell command a tool ran, when its input says.
fn tool_command(t: &ToolCall) -> Option<String> {
    let input = t.raw_input.as_ref()?;
    let raw = match input.get("command").or_else(|| input.get("cmd"))? {
        serde_json::Value::String(s) => s.clone(),
        serde_json::Value::Array(parts) => {
            parts.iter().filter_map(serde_json::Value::as_str).collect::<Vec<_>>().join(" ")
        }
        _ => return None,
    };
    Some(head(raw.trim(), OUTPUT_CHARS))
}

fn tool_output(t: &ToolCall) -> String {
    if !t.terminal_output.is_empty() {
        return t.terminal_output.clone();
    }
    let texts: Vec<&str> = t
        .content
        .iter()
        .filter_map(|c| match c {
            ToolContent::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect();
    if texts.is_empty() { t.raw_output.clone().unwrap_or_default() } else { texts.join("\n") }
}

fn head(text: &str, max: usize) -> String {
    match text.char_indices().nth(max) {
        Some((i, _)) => format!("{}…", &text[..i]),
        None => text.to_string(),
    }
}

/// The last `lines` lines of `text`, and of those at most the last `max` characters.
fn tail(text: &str, lines: usize, max: usize) -> String {
    let all: Vec<&str> = text.lines().collect();
    let kept = all[all.len().saturating_sub(lines)..].join("\n");
    let n = kept.chars().count();
    if n <= max {
        return kept;
    }
    format!("…{}", kept.chars().skip(n - max).collect::<String>())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::PlanEntry;

    fn tool(status: ToolStatus, command: &str, output: &str, exit_code: Option<i64>) -> Entry {
        Entry::Tool(ToolCall {
            id: "t".into(),
            title: "Run tests".into(),
            kind: "execute".into(),
            status,
            content: Vec::new(),
            locations: Vec::new(),
            raw_input: Some(serde_json::json!({ "command": command })),
            raw_output: None,
            added: 0,
            removed: 0,
            terminal_output: output.into(),
            exit_code,
            parent: None,
            tool_name: None,
        })
    }

    #[test]
    fn log_reads_as_plain_text() {
        let entries = vec![
            Entry::User {
                text: "fix the build".into(),
                at: 1,
                elapsed_ms: None,
                message_id: None,
                tokens: None,
                shell_changes: Vec::new(),
            },
            Entry::Thought { text: "x".repeat(400) },
            tool(ToolStatus::Failed, "cargo test", &(1..=20).map(|i| format!("line {i}\n")).collect::<String>(), Some(101)),
            Entry::Agent { text: "  ".into() },
            Entry::Plan {
                entries: vec![
                    PlanEntry { content: "find it".into(), priority: String::new(), status: "completed".into() },
                    PlanEntry { content: "fix it".into(), priority: String::new(), status: "pending".into() },
                ],
            },
            Entry::Notice { text: "Check failed".into(), error: true, mark: None, places: Vec::new() },
            Entry::Agent { text: "Fixed.\n".into() },
        ];
        let log = render_log(&entries, 30);
        let thought = format!("thought: {}…", "x".repeat(300));
        let output = (13..=20).map(|i| format!("line {i}")).collect::<Vec<_>>().join("\n");
        assert_eq!(
            log,
            format!(
                "you: fix the build\n\n{thought}\n\n[Run tests] failed (exit 101)\n$ cargo test\n{output}\n\n\
                 plan:\n  [x] find it\n  [ ] fix it\n\nerror: Check failed\n\nagent: Fixed."
            )
        );
        assert_eq!(render_log(&entries, 2), "error: Check failed\n\nagent: Fixed.");
        assert_eq!(render_log(&[], 5), "");
        assert_eq!(tail("abcdef", 5, 3), "…def");
        assert_eq!(head("abcdef", 3), "abc…");
    }
}
