//! The project's check: a command arakun runs in the environment after every
//! agent turn that edited files. A failure goes back to the session as a
//! prompt, up to `CHECK_ROUNDS` times in a row; the transcript shows each run
//! as one notice that settles when the command ends.

use std::io::Read as _;
use std::path::Path;
use std::process::Stdio;
use std::time::{Duration, Instant};

use futures::channel::oneshot;
use gpui::{AppContext as _, Context, Entity, SharedString, Window};

use super::Shell;
use super::island::Open;
use crate::agent::{CheckAnswer, NoticeMark, NoticePlace, Thread, ThreadEvent, ThreadStatus};
use crate::island::AlertKind;
use crate::model::Project;

/// Tries an agent gets in a row before a failing check is left to the user.
const CHECK_ROUNDS: u8 = 3;
/// A check still running after this is stopped and counts as failed.
const CHECK_TIMEOUT: Duration = Duration::from_secs(10 * 60);
/// How much of the end of a failing check's output goes back to the agent.
const PROMPT_LINES: usize = 120;
const PROMPT_BYTES: usize = 12_000;
/// Output kept while a check runs; only its end is ever shown.
const KEEP_BYTES: usize = 256 * 1024;
/// Errors picked out of a failing check's output, with their file and line.
const MAX_PLACES: usize = 20;

#[derive(Default)]
pub(super) struct CheckLoop {
    /// Bumped by every check and by the user's own prompts: only the latest
    /// check of a turn nobody stepped into goes back to the agent.
    generation: u64,
    rounds: u8,
    running: u32,
}

/// A check a finished turn calls for. With `shell_since`, the turn only ran
/// shell commands, and the check is due if they changed files since then.
pub(super) struct CheckDue {
    command: String,
    shell_since: Option<u64>,
}

pub(super) struct CheckRun {
    code: Option<i32>,
    output: String,
    elapsed: Duration,
    timed_out: bool,
    /// Where its output says the errors are, in files of the environment.
    places: Vec<NoticePlace>,
}

impl CheckRun {
    fn passed(&self) -> bool {
        !self.timed_out && self.code == Some(0)
    }

    fn outcome(&self) -> String {
        match self.code {
            _ if self.timed_out => format!("timed out after {} min", CHECK_TIMEOUT.as_secs() / 60),
            Some(code) => format!("exit {code}"),
            None => "killed by a signal".into(),
        }
    }
}

impl Shell {
    pub(super) fn project_of_env(&self, env_path: &Path) -> Option<&Project> {
        self.projects
            .iter()
            .find(|p| p.env_index(env_path).is_some())
    }

    /// The check to run after a turn that ended with `stop_reason`, if it may be due.
    pub(super) fn check_for_turn(
        &self,
        env_path: &Path,
        thread: &Entity<Thread>,
        stop_reason: &str,
        cx: &gpui::App,
    ) -> Option<CheckDue> {
        if matches!(stop_reason, "cancelled" | "error") {
            return None;
        }
        let command = self.project_of_env(env_path)?.check.clone()?;
        let t = thread.read(cx);
        if t.turn_edited() {
            return Some(CheckDue {
                command,
                shell_since: None,
            });
        }
        t.turn_shell_since().map(|since| CheckDue {
            command,
            shell_since: Some(since),
        })
    }

    /// While a session's check runs its agent stays up, so a failure can go straight back.
    pub(super) fn check_running(&self, id: &str) -> bool {
        self.checks.get(id).is_some_and(|l| l.running > 0)
    }

    /// The user wrote to the session: a check still running won't answer for them.
    pub(super) fn user_took_over(&mut self, id: &str) {
        if let Some(lp) = self.checks.get_mut(id) {
            lp.generation += 1;
            lp.rounds = 0;
        }
    }

    pub(super) fn check_after_turn(
        &mut self,
        env_path: &Path,
        id: &SharedString,
        thread: &Entity<Thread>,
        stop_reason: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(due) = self.check_for_turn(env_path, thread, stop_reason, cx) else {
            self.offer_check(env_path, thread, stop_reason, cx);
            return;
        };
        let Some(since) = due.shell_since else {
            return self.start_check(env_path, id, thread, due.command, window, cx);
        };
        // Only shell commands ran: the check is due if they changed files of
        // the checkout. Counted as running meanwhile so the agent stays up.
        self.checks.entry(id.to_string()).or_default().running += 1;
        let dir = env_path.to_path_buf();
        let changed = cx.background_spawn(crate::guard::task(async move {
            !crate::memory::changed_since(&dir, since).is_empty()
        }));
        let (env_path, id, thread, stop_reason) = (
            env_path.to_path_buf(),
            id.clone(),
            thread.clone(),
            stop_reason.to_string(),
        );
        cx.spawn_in(window, async move |this, cx| {
            let changed = changed.await;
            let _ = this.update_in(cx, |this, window, cx| {
                if let Some(lp) = this.checks.get_mut(id.as_ref()) {
                    lp.running = lp.running.saturating_sub(1);
                }
                if changed {
                    return this.start_check(&env_path, &id, &thread, due.command, window, cx);
                }
                // The turn's own notice waited for this.
                let done = ThreadEvent::Finished { stop_reason };
                this.notify_desktop(&env_path, &id, &thread, &done, window, cx);
                this.retire_if_hidden(&env_path, &id, cx);
            });
        })
        .detach();
    }

    /// After a turn that edited a project with no check, offers the one its
    /// files suggest, unless the user said not to.
    fn offer_check(
        &self,
        env_path: &Path,
        thread: &Entity<Thread>,
        stop_reason: &str,
        cx: &mut Context<Self>,
    ) {
        if matches!(stop_reason, "cancelled" | "error") {
            return;
        }
        let Some(project) = self.project_of_env(env_path) else {
            return;
        };
        let t = thread.read(cx);
        if project.check.is_some()
            || project.check_declined
            || t.check_offered()
            || !t.turn_edited()
        {
            return;
        }
        if let Some(command) = suggest_check(env_path) {
            thread.update(cx, |t, cx| t.offer_check(command, cx));
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn check_offer_answered(
        &mut self,
        env_path: &Path,
        id: &SharedString,
        thread: &Entity<Thread>,
        command: String,
        answer: CheckAnswer,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(pi) = self
            .projects
            .iter()
            .position(|p| p.env_index(env_path).is_some())
        else {
            return;
        };
        match answer {
            CheckAnswer::Use => {
                self.projects[pi].check = Some(command.clone());
                self.save(cx);
                // The turn that prompted the offer gets checked too.
                self.start_check(env_path, id, thread, command, window, cx);
            }
            CheckAnswer::Edit => self.open_project_command(pi, true, Some(command), window, cx),
            CheckAnswer::Never => {
                self.projects[pi].check_declined = true;
                self.save(cx);
            }
        }
    }

    fn start_check(
        &mut self,
        env_path: &Path,
        id: &SharedString,
        thread: &Entity<Thread>,
        command: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // The agent can edit what the check runs, so in a sandboxed project the
        // check runs in the agent's sandbox, or not at all.
        let sandbox = thread.read(cx).sandbox();
        if sandbox.is_none() && crate::agent::sandbox::wanted(env_path).unwrap_or(true) {
            thread.update(cx, |t, cx| {
                t.post_notice(
                    format!("Not checking · {command}: the agent's sandbox is not running"),
                    true,
                    cx,
                )
            });
            return;
        }
        let lp = self.checks.entry(id.to_string()).or_default();
        lp.generation += 1;
        lp.running += 1;
        let generation = lp.generation;
        let notice = thread.update(cx, |t, cx| {
            t.post_marked_notice(
                format!("Checking · {command}"),
                false,
                Some(NoticeMark::Busy),
                cx,
            )
        });
        // A check that writes files (a lockfile, a formatter) mustn't count as the turn's shell changes.
        let shell_pending = thread.read(cx).shell_pending();
        let (dir, cmd) = (env_path.to_path_buf(), command.clone());
        let (env_path, id) = (env_path.to_path_buf(), id.clone());
        cx.spawn_in(window, async move |this, cx| {
            if let Some(pending) = shell_pending {
                pending.await;
            }
            let (tx, rx) = oneshot::channel();
            let _ = std::thread::Builder::new()
                .name("arakun-check".into())
                .spawn(move || tx.send(run_check(&dir, &cmd, sandbox.as_deref(), CHECK_TIMEOUT)));
            let result = rx
                .await
                .unwrap_or_else(|_| Err(std::io::Error::other("the check stopped")));
            let _ = this.update_in(cx, |this, window, cx| {
                this.check_done(
                    &env_path, &id, notice, generation, &command, result, window, cx,
                )
            });
        })
        .detach();
    }

    #[allow(clippy::too_many_arguments)]
    fn check_done(
        &mut self,
        env_path: &Path,
        id: &SharedString,
        notice: usize,
        generation: u64,
        command: &str,
        result: std::io::Result<CheckRun>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let (latest, rounds) = match self.checks.get_mut(id.as_ref()) {
            Some(lp) => {
                lp.running = lp.running.saturating_sub(1);
                (lp.generation == generation, lp.rounds)
            }
            None => (false, 0),
        };
        let Some(thread) = self
            .session_mut(env_path, id)
            .and_then(|s| s.live.as_ref().map(|l| l.thread.clone()))
        else {
            return;
        };
        let (agent, waiting) = {
            let t = thread.read(cx);
            (
                t.kind().label(),
                t.status() == ThreadStatus::Idle && t.queued().is_empty(),
            )
        };
        // A turn the user (or its queue) already moved past keeps only the notice.
        let answer = latest && waiting;
        let mut prompt = None;
        let mut gave_up = false;
        let places = match &result {
            Ok(run) if !run.passed() => run.places.clone(),
            _ => Vec::new(),
        };
        let (text, error, mark) = match &result {
            Ok(run) if run.passed() => {
                if latest && let Some(lp) = self.checks.get_mut(id.as_ref()) {
                    lp.rounds = 0;
                }
                (
                    format!("Check passed · {command} · {}", took(run.elapsed)),
                    false,
                    Some(NoticeMark::Passed),
                )
            }
            Ok(run) if answer && rounds < CHECK_ROUNDS => {
                let round = rounds + 1;
                if let Some(lp) = self.checks.get_mut(id.as_ref()) {
                    lp.rounds = round;
                }
                prompt = Some(failure_prompt(command, run, round));
                let text = format!(
                    "Check failed · {command} · {} · sent back to {agent} (try {round} of {CHECK_ROUNDS})",
                    run.outcome()
                );
                (text, true, None)
            }
            Ok(run) if answer => {
                if let Some(lp) = self.checks.get_mut(id.as_ref()) {
                    lp.rounds = 0;
                }
                gave_up = true;
                let text = format!(
                    "Check still fails after {CHECK_ROUNDS} tries · {command} · {} · over to you",
                    run.outcome()
                );
                (text, true, None)
            }
            Ok(run) => (
                format!("Check failed · {command} · {}", run.outcome()),
                true,
                None,
            ),
            Err(e) => (format!("Check could not run · {command} · {e}"), true, None),
        };
        thread.update(cx, |t, cx| {
            t.update_notice(notice, text, error, mark, places, cx)
        });
        if let Some(prompt) = prompt {
            self.send_to_session(env_path, id, prompt, window, cx);
            return;
        }
        if gave_up {
            let title = self
                .session_mut(env_path, id)
                .map(|s| s.title.to_string())
                .unwrap_or_default();
            let body = format!("The check still fails after {CHECK_ROUNDS} tries.");
            self.notify(
                AlertKind::Failure,
                &title,
                &body,
                Some(Open::Session(id.clone())),
                cx,
            );
        } else if waiting {
            // The turn's own notice waited for the check.
            let done = ThreadEvent::Finished {
                stop_reason: "end_turn".into(),
            };
            self.notify_desktop(env_path, id, &thread, &done, window, cx);
        }
        self.save_transcript(env_path, id, cx);
        self.retire_if_hidden(env_path, id, cx);
    }
}

/// A check the files of `dir` suggest: the Makefile's `check` or `test`
/// target, else the usual compile-and-test command of the project's language.
fn suggest_check(dir: &Path) -> Option<String> {
    let read = |f: &str| std::fs::read_to_string(dir.join(f)).ok();
    let has = |f: &str| dir.join(f).exists();
    if let Some(make) = read("Makefile").or_else(|| read("makefile")) {
        for target in ["check", "test"] {
            if make
                .lines()
                .any(|l| l.strip_prefix(target).is_some_and(|r| r.starts_with(':')))
            {
                return Some(format!("make {target}"));
            }
        }
    }
    if has("Cargo.toml") {
        return Some("cargo check --all-targets".into());
    }
    if let Some(package) = read("package.json") {
        let pm = if has("pnpm-lock.yaml") {
            "pnpm"
        } else if has("yarn.lock") {
            "yarn"
        } else if has("bun.lock") || has("bun.lockb") {
            "bun"
        } else {
            "npm"
        };
        let package: serde_json::Value = serde_json::from_str(&package).unwrap_or_default();
        let script = |name: &str| {
            package
                .get("scripts")
                .and_then(|s| s.get(name))
                .and_then(|v| v.as_str())
        };
        let mut steps = Vec::new();
        if let Some(name) = ["typecheck", "type-check", "check-types", "tsc"]
            .into_iter()
            .find(|n| script(n).is_some())
        {
            steps.push(format!("{pm} run {name}"));
        } else if has("tsconfig.json") {
            let tsc = match pm {
                "pnpm" => "pnpm exec tsc --noEmit",
                "yarn" => "yarn tsc --noEmit",
                "bun" => "bunx tsc --noEmit",
                _ => "npx tsc --noEmit",
            };
            steps.push(tsc.to_string());
        }
        // npm init's placeholder test script only fails.
        if script("test").is_some_and(|t| !t.contains("no test specified")) {
            steps.push(format!("{pm} run test"));
        }
        return (!steps.is_empty()).then(|| steps.join(" && "));
    }
    if has("go.mod") {
        return Some("go test ./...".into());
    }
    if has("pyproject.toml") || has("setup.py") {
        let pyproject = read("pyproject.toml").unwrap_or_default();
        if pyproject.contains("pytest") || has("pytest.ini") || has("tests") {
            return Some(
                if has("uv.lock") {
                    "uv run pytest -q"
                } else {
                    "python -m pytest -q"
                }
                .into(),
            );
        }
    }
    None
}

/// One check at a time across every session and project: several agents
/// finishing a turn together each started a full build and test run, which
/// took every core and filled the memory until the whole desktop crawled.
static ONE_AT_A_TIME: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Build jobs a check may use: half the cores (at least 2), so a build
/// leaves room for the desktop and its peak memory stays lower.
fn check_jobs() -> usize {
    std::thread::available_parallelism().map_or(2, |n| (n.get() / 2).max(2))
}

/// Runs `command` with the system shell (`sh`, `cmd` on Windows) in `dir`
/// without a terminal, stdout and stderr together, stopping it (and whatever
/// it started) after `timeout`.
fn run_check(
    dir: &Path,
    command: &str,
    sandbox: Option<&crate::agent::sandbox::Session>,
    timeout: Duration,
) -> std::io::Result<CheckRun> {
    // Waits for the one running; the timeout counts from when this one starts.
    let _turn = ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner());
    let start = Instant::now();
    let (mut stdout, writer) = std::io::pipe()?;
    let mut cmd = match sandbox {
        Some(s) => s.shell_command(command),
        None => crate::platform::shell_command(command),
    };
    cmd.current_dir(dir)
        // Test runners and package scripts take it as "no watch mode, no prompts".
        .env("CI", "1")
        .stdin(Stdio::null())
        .stdout(writer.try_clone()?)
        .stderr(writer);
    crate::platform::ProcessTree::prepare(&mut cmd);
    // A full test run shouldn't make the window stutter.
    crate::platform::lower_priority(&mut cmd);
    if std::env::var_os("CARGO_BUILD_JOBS").is_none() {
        cmd.env("CARGO_BUILD_JOBS", check_jobs().to_string());
    }
    let spawned = cmd.spawn();
    // The pipe ends only when every writer is gone, ours included.
    drop(cmd);
    let mut child = spawned?;
    let tree = crate::platform::ProcessTree::adopt(&child);
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut kept = Vec::new();
        let mut buf = [0u8; 8192];
        while let Ok(n) = stdout.read(&mut buf) {
            if n == 0 {
                break;
            }
            kept.extend_from_slice(&buf[..n]);
            if kept.len() > 2 * KEEP_BYTES {
                kept.drain(..kept.len() - KEEP_BYTES);
            }
        }
        let _ = tx.send(kept);
    });
    let mut timed_out = false;
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }
        if start.elapsed() >= timeout {
            timed_out = true;
            tree.kill();
            // Without a job object (Windows, if creating it failed) the tree
            // kill does nothing. Ending only cmd.exe would leave its cargo
            // running beside the next check, and an endless wait here would
            // hold every project's checks behind this one.
            if cfg!(windows) {
                let _ = crate::platform::command("taskkill")
                    .args(["/T", "/F", "/PID", &child.id().to_string()])
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .status();
            }
            let _ = child.kill();
            break child.wait()?;
        }
        std::thread::sleep(Duration::from_millis(100));
    };
    // Anything it left running in its group would keep the output open.
    if cfg!(windows) {
        tree.kill();
    } else {
        tree.terminate();
    }
    let bytes = rx.recv_timeout(Duration::from_secs(2)).unwrap_or_default();
    let output = clean_output(&bytes);
    let places = if status.success() {
        Vec::new()
    } else {
        error_places(&output, dir)
    };
    Ok(CheckRun {
        code: status.code(),
        output,
        elapsed: start.elapsed(),
        timed_out,
        places,
    })
}

/// The errors in `output` that name a file of `dir` and a line: rustc's
/// `error[E…]: …` with its `-->` line, Rust panics, and the
/// `file:line:col: error` and `file(line,col): error` most tools print.
fn error_places(output: &str, dir: &Path) -> Vec<NoticePlace> {
    let lines: Vec<&str> = output.lines().collect();
    let mut out: Vec<NoticePlace> = Vec::new();
    for (i, raw) in lines.iter().enumerate() {
        let t = raw.trim();
        let found = rust_error(t, &lines[i + 1..])
            .or_else(|| rust_panic(t, lines.get(i + 1).copied()))
            .or_else(|| crate::ci::error_at(t));
        let Some((path, line, message)) = found else {
            continue;
        };
        let file = dir.join(path.trim_start_matches("./"));
        if !file.is_file() {
            continue;
        }
        let path = file
            .strip_prefix(dir)
            .unwrap_or(&file)
            .to_string_lossy()
            .into_owned();
        if out.iter().any(|p| p.path == path && p.line == line) {
            continue;
        }
        out.push(NoticePlace {
            path,
            line,
            message,
        });
        if out.len() == MAX_PLACES {
            break;
        }
    }
    out
}

/// `error[E0425]: message` with ` --> path:line:col` on the next line.
fn rust_error(t: &str, next: &[&str]) -> Option<(String, u32, String)> {
    let rest = t.strip_prefix("error")?;
    let rest = match rest.strip_prefix('[') {
        Some(code) => code.split_once(']')?.1,
        None => rest,
    };
    let message = rest.strip_prefix(':')?.trim();
    let at = next.first()?.trim().strip_prefix("--> ")?;
    let (path, line) = path_line(at)?;
    Some((path, line, message.to_string()))
}

/// `thread 'x' panicked at path:line:col:` with the message on the next line,
/// or older Rust's `panicked at 'message', path:line:col`.
fn rust_panic(t: &str, next: Option<&str>) -> Option<(String, u32, String)> {
    let (_, at) = t.split_once("panicked at ")?;
    if let Some(at) = at.strip_suffix(':') {
        let (path, line) = path_line(at)?;
        let message = next
            .map(str::trim)
            .filter(|m| !m.is_empty())
            .unwrap_or("panicked");
        return Some((path, line, message.to_string()));
    }
    let (message, at) = at.rsplit_once(", ")?;
    let (path, line) = path_line(at)?;
    Some((path, line, message.trim_matches('\'').to_string()))
}

/// `path:line:col`.
fn path_line(at: &str) -> Option<(String, u32)> {
    let mut parts = at.trim().rsplitn(3, ':');
    let (col, line, path) = (parts.next()?, parts.next()?, parts.next()?);
    col.parse::<u32>().ok()?;
    let line = line.parse().ok()?;
    (!path.is_empty() && !path.contains(' ')).then(|| (path.to_string(), line))
}

/// Output as a terminal would have left it: colors gone, lines redrawn with
/// `\r` (progress bars) reduced to what they ended as.
fn clean_output(bytes: &[u8]) -> String {
    let text = String::from_utf8_lossy(bytes);
    let mut out = String::with_capacity(text.len());
    for line in text.split('\n') {
        let line = line.strip_suffix('\r').unwrap_or(line);
        let line = line.rsplit('\r').next().unwrap_or(line);
        out.push_str(&strip_ansi(line));
        out.push('\n');
    }
    out.trim_end().to_string()
}

fn strip_ansi(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c != '\x1b' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('[') => {
                for c in chars.by_ref() {
                    if ('@'..='~').contains(&c) {
                        break;
                    }
                }
            }
            Some(']') => {
                while let Some(c) = chars.next() {
                    if c == '\x07' {
                        break;
                    }
                    if c == '\x1b' {
                        chars.next();
                        break;
                    }
                }
            }
            _ => {}
        }
    }
    out
}

/// The last `max_lines` lines of `text`, at most about `max_bytes` of them.
fn tail(text: &str, max_lines: usize, max_bytes: usize) -> String {
    let lines: Vec<&str> = text.lines().collect();
    let mut kept: Vec<&str> = Vec::new();
    let mut size = 0;
    for line in lines.iter().rev().take(max_lines) {
        size += line.len() + 1;
        if size > max_bytes {
            if kept.is_empty() {
                let mut cut = line.len().saturating_sub(max_bytes);
                while !line.is_char_boundary(cut) {
                    cut += 1;
                }
                kept.push(&line[cut..]);
            }
            break;
        }
        kept.push(line);
    }
    kept.reverse();
    let earlier = lines.len() - kept.len();
    let mut out = if earlier > 0 {
        format!("[… {earlier} earlier lines]\n")
    } else {
        String::new()
    };
    out.push_str(&kept.join("\n"));
    out
}

fn failure_prompt(command: &str, run: &CheckRun, round: u8) -> String {
    let output = if run.output.trim().is_empty() {
        "(no output)".to_string()
    } else {
        tail(&run.output, PROMPT_LINES, PROMPT_BYTES)
    };
    // The end of the output may have cut the first errors off.
    let places: String = run
        .places
        .iter()
        .map(|p| format!("- {}:{}: {}\n", p.path, p.line, p.message))
        .collect();
    let places = if places.is_empty() {
        places
    } else {
        format!("Errors it reported:\n{places}\n")
    };
    format!(
        "arakun ran this project's check after your turn and it failed (try {round} of {CHECK_ROUNDS}).\n\n\
         $ {command}\n{}\n\n{places}```\n{output}\n```\n\n\
         Fix the cause and end your turn; arakun runs the check again. If the failure has nothing \
         to do with your changes, say so instead of working around it.",
        run.outcome()
    )
}

fn took(d: Duration) -> String {
    match d.as_secs() {
        s @ 0..60 => format!("{}s", s.max(1)),
        s => format!("{}m {}s", s / 60, s % 60),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn output_loses_colors_and_redrawn_progress() {
        let raw =
            b"\x1b[1;31merror\x1b[0m: boom\r\nbuild 10%\rbuild 100%\n\x1b]0;title\x07done\n\n";
        assert_eq!(clean_output(raw), "error: boom\nbuild 100%\ndone");
    }

    #[test]
    fn tail_keeps_the_end_and_says_what_it_cut() {
        let text = (1..=10)
            .map(|i| format!("line {i}"))
            .collect::<Vec<_>>()
            .join("\n");
        assert_eq!(
            tail(&text, 3, 1000),
            "[… 7 earlier lines]\nline 8\nline 9\nline 10"
        );
        assert_eq!(tail(&text, 100, 15), "[… 8 earlier lines]\nline 9\nline 10");
        assert_eq!(tail("short", 5, 1000), "short");
        assert_eq!(tail("ééééé", 5, 4), "éé");
    }

    #[test]
    fn checks_capture_both_streams_and_the_exit_code() {
        let dir = std::env::temp_dir();
        let (failing, passing) = if cfg!(windows) {
            ("echo out& (echo err)1>&2& exit /b 3", "exit /b 0")
        } else {
            ("echo out; echo err >&2; exit 3", "true")
        };
        let run = run_check(&dir, failing, None, Duration::from_secs(10)).unwrap();
        assert_eq!(run.code, Some(3));
        assert!(!run.passed());
        assert_eq!(run.output, "out\nerr");
        assert!(
            run_check(&dir, passing, None, Duration::from_secs(10))
                .unwrap()
                .passed()
        );
        let prompt = failure_prompt("make test", &run, 1);
        assert!(
            prompt.contains("try 1 of 3")
                && prompt.contains("$ make test\nexit 3")
                && prompt.contains("out\nerr")
        );
    }

    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[test]
    fn a_sandboxed_check_runs_inside() {
        let dir = crate::platform::canonicalize(std::env::temp_dir())
            .unwrap()
            .join(format!("arakun-check-sbx-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(".env"), "KEY=1").unwrap();
        let Ok(s) = crate::agent::sandbox::Session::new(crate::agent::AgentKind::Codex, &dir)
        else {
            // No runner here, or a bubblewrap that can't start (no user namespaces in CI).
            std::fs::remove_dir_all(&dir).unwrap();
            return;
        };
        // An agent that edits the check's script still can't read what the sandbox hides.
        let run = run_check(&dir, "cat .env; echo ok", Some(&s), Duration::from_secs(30)).unwrap();
        assert!(!run.output.contains("KEY=1"), "{}", run.output);
        assert!(run.output.ends_with("ok"), "{}", run.output);
        drop(s);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn errors_point_at_files_of_the_environment() {
        let dir = std::env::temp_dir().join(format!("arakun-places-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("src")).unwrap();
        for f in ["src/main.rs", "src/a.ts", "lib.c"] {
            std::fs::write(dir.join(f), "").unwrap();
        }
        let output = "   Compiling arakun v0.1.0\n\
            error[E0425]: cannot find value `cx` in this scope\n  --> src/main.rs:70:31\n   |\n\
            error: could not compile `arakun`\n\
            warning: unused\n  --> src/main.rs:3:1\n\
            thread 'tests::x' panicked at src/main.rs:12:5:\nassertion failed: ok\n\
            thread 'main' panicked at 'boom', src/main.rs:20:9\n\
            thread 'y' panicked at /rustc/abc/library/core/src/ops.rs:1:1:\n\
            src/a.ts(4,2): error TS2304: Cannot find name 'x'.\n\
            ./lib.c:9:1: error: expected ';'\n\
            gone.rs:1:1: error: not here\n\
            error[E0425]: cannot find value `cx` in this scope\n  --> src/main.rs:70:31";
        let places = error_places(output, &dir);
        let got: Vec<(&str, u32, &str)> = places
            .iter()
            .map(|p| (p.path.as_str(), p.line, p.message.as_str()))
            .collect();
        assert_eq!(
            got,
            [
                ("src/main.rs", 70, "cannot find value `cx` in this scope"),
                ("src/main.rs", 12, "assertion failed: ok"),
                ("src/main.rs", 20, "boom"),
                ("src/a.ts", 4, "TS2304: Cannot find name 'x'."),
                ("lib.c", 9, "expected ';'"),
            ]
        );
        let abs = format!("{}:2:3: error: absolute", dir.join("lib.c").display());
        assert_eq!(error_places(&abs, &dir)[0].path, "lib.c");
        let run = CheckRun {
            code: Some(1),
            output: output.into(),
            elapsed: Duration::ZERO,
            timed_out: false,
            places,
        };
        assert!(
            failure_prompt("cargo test", &run, 1)
                .contains("Errors it reported:\n- src/main.rs:70: cannot find")
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn checks_run_at_lower_priority() {
        // /proc first: ps prints "-" for the nice of a process under SCHED_IDLE.
        let run = run_check(
            &std::env::temp_dir(),
            crate::git::NICE_OF_SHELL,
            None,
            Duration::from_secs(10),
        )
        .unwrap();
        let ours = unsafe { libc::getpriority(libc::PRIO_PROCESS, 0) };
        assert_eq!(
            run.output.trim().parse::<i32>().unwrap(),
            (ours + 10).min(19)
        );
    }

    #[test]
    fn projects_suggest_their_usual_check() {
        let dir = std::env::temp_dir().join(format!("arakun-suggest-{}", std::process::id()));
        let at = |files: &[(&str, &str)]| {
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            for (f, body) in files {
                std::fs::write(dir.join(f), body).unwrap();
            }
            suggest_check(&dir)
        };
        assert_eq!(at(&[]), None);
        assert_eq!(
            at(&[("Cargo.toml", "")]).as_deref(),
            Some("cargo check --all-targets")
        );
        assert_eq!(
            at(&[
                ("Cargo.toml", ""),
                ("Makefile", ".PHONY: test\ntest: build\n\tcargo test\n")
            ])
            .as_deref(),
            Some("make test")
        );
        let pkg = r#"{"scripts": {"typecheck": "tsc", "test": "vitest"}}"#;
        assert_eq!(
            at(&[("package.json", pkg), ("pnpm-lock.yaml", "")]).as_deref(),
            Some("pnpm run typecheck && pnpm run test")
        );
        let placeholder = r#"{"scripts": {"test": "echo \"Error: no test specified\" && exit 1"}}"#;
        assert_eq!(
            at(&[("package.json", placeholder), ("tsconfig.json", "{}")]).as_deref(),
            Some("npx tsc --noEmit")
        );
        assert_eq!(at(&[("package.json", placeholder)]), None);
        assert_eq!(at(&[("go.mod", "")]).as_deref(), Some("go test ./..."));
        assert_eq!(
            at(&[
                ("pyproject.toml", "[tool.pytest.ini_options]"),
                ("uv.lock", "")
            ])
            .as_deref(),
            Some("uv run pytest -q")
        );
        assert_eq!(at(&[("pyproject.toml", "")]), None);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn slow_checks_time_out() {
        let slow = if cfg!(windows) {
            "echo started& ping -n 30 127.0.0.1 >nul"
        } else {
            "echo started; sleep 30"
        };
        let run = run_check(
            &std::env::temp_dir(),
            slow,
            None,
            Duration::from_millis(300),
        )
        .unwrap();
        assert!(run.timed_out && !run.passed());
        assert_eq!(run.output, "started");
        assert!(run.elapsed < Duration::from_secs(5));
    }

    #[cfg(unix)]
    #[test]
    fn a_waiting_check_runs_after_the_other_with_its_full_time() {
        let dir = std::env::temp_dir().join(format!("arakun-checks-queue-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        // The first takes about a second; the second may only run 0.5 s, so it
        // would time out if its time counted while it waited.
        let first = {
            let dir = dir.clone();
            std::thread::spawn(move || {
                let first = "touch first-start; sleep 1; touch first-end";
                run_check(&dir, first, None, Duration::from_secs(10)).unwrap()
            })
        };
        // The second must queue behind the first, so wait until the first holds
        // the turn. Other tests take it too, so this can take a while.
        let deadline = Instant::now() + Duration::from_secs(60);
        while !dir.join("first-start").exists() {
            assert!(Instant::now() < deadline, "the first check never started");
            std::thread::sleep(Duration::from_millis(20));
        }
        // It fails if it starts while the first one is still going.
        let second =
            run_check(&dir, "test -f first-end", None, Duration::from_millis(500)).unwrap();
        let first = first.join().unwrap();
        assert_eq!(first.code, Some(0));
        assert!(!second.timed_out);
        assert_eq!(second.code, Some(0), "the two checks overlapped");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
