//! Headless agents: `arakun agent run` starts one in a process of its own
//! (`arakun agent-host <id>`, in a new session), so it goes on with the app,
//! the terminal and the island closed. The host drives an ordinary
//! [`Thread`] on a windowless GPUI app, so the tool loop, transcript and
//! resume are the app's own.
//!
//! Everything lives in `<data>/arakun/headless/<id>/`, never in `state.json`:
//! `meta.json` ([`Meta`]), `transcript.json` (an [`agent::Transcript`], what the
//! app renders), `pending.json` (a [`Pending`] tool call waiting for the user),
//! `inbox/` (prompts the host has yet to send) and `host.log`. Workers AI keeps
//! its API messages in `agents/workers-ai/<session>.json` as always.
//!
//! Tool calls go through [`policy`]: what it allows runs; the rest waits in
//! `pending.json` until `arakun agent approve|deny`, or 24 h (denied). The
//! policy only saves asking about harmless calls: the guarantee is the
//! [`sandbox`] the tools run in (no credentials, no network unless `--allow`
//! grants it), and the [`broker`], which runs what the agent can't (mail,
//! pushes, host commands) on the host, each only after the user approves it.
//!
//! Another agent kind is another arm of [`agent_for`]. Claude Code is left out
//! for now: its own permission settings (allow rules, bypass mode) answer
//! before ACP asks, so the policy could not be enforced through it.

pub mod policy;
pub use arakun_core::headless::{broker, sandbox};

use std::cell::Cell;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::{Duration, Instant};

use gpui::{App, AppContext as _, Context, Entity, Global};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::agent::{
    self, AgentKind, Entry, Setting, Thread, ThreadEvent, ThreadStatus, workers_ai,
};
use crate::control::{Stop, flags_end};
use policy::{Level, Policy};
use sandbox::{Cap, Jail, Sandbox};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    Working,
    NeedsInput,
    Idle,
    Error,
}

impl Status {
    pub fn name(self) -> &'static str {
        match self {
            Status::Working => "working",
            Status::NeedsInput => "needs_input",
            Status::Idle => "idle",
            Status::Error => "error",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Meta {
    pub id: String,
    pub agent: AgentKind,
    /// As `--model` names it: `workers-ai/@cf/…`.
    pub model: String,
    /// Empty: the model's default.
    #[serde(default)]
    pub effort: String,
    pub policy: Policy,
    pub cwd: PathBuf,
    pub title: String,
    pub created: u64,
    pub updated: u64,
    pub status: Status,
    /// The host process while it runs.
    #[serde(default)]
    pub pid: Option<u32>,
    /// What it did last, in a line.
    #[serde(default)]
    pub last: String,
    /// USD so far.
    #[serde(default)]
    pub cost: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// The agent's own session id, which `session/resume` reopens.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    /// What `--allow` granted beyond the sandbox.
    #[serde(default)]
    pub caps: Vec<Cap>,
    /// `--read` paths the sandbox sees, read-only, beyond the toolchains.
    #[serde(default)]
    pub reads: Vec<PathBuf>,
    /// `--no-sandbox`: tools run on the host once the user confirms it.
    #[serde(default)]
    pub no_sandbox: bool,
    /// The user confirmed `--no-sandbox`.
    #[serde(default)]
    pub unsandboxed: bool,
    /// `--wait`: nobody is there to answer, so what would ask is declined.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub unattended: bool,
    /// Calls declined that way.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub declined: u32,
    /// How the last turn ended: ACP's stop reason (end_turn, cancelled…) or "error".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stop_reason: Option<String>,
}

fn is_zero(n: &u32) -> bool {
    *n == 0
}

impl Meta {
    /// How its tools run, for `ls` and `log`.
    pub fn confinement(&self) -> String {
        let caps = sandbox::caps_names(&self.caps);
        let reads: String = self
            .reads
            .iter()
            .map(|r| format!(", read {}", r.display()))
            .collect();
        match (self.no_sandbox, self.unsandboxed) {
            (false, _) => format!("sandbox, allow {caps}{reads}"),
            (true, true) => format!("NO SANDBOX, allow {caps}"),
            (true, false) => "no sandbox (unconfirmed)".into(),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Answer {
    Approved,
    Denied,
}

/// A tool call waiting for the user.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Pending {
    pub req_id: String,
    pub session: String,
    /// ACP tool kind: execute, edit, delete, other…
    pub tool: String,
    pub title: String,
    #[serde(default)]
    pub command: Option<String>,
    /// The call's whole input.
    #[serde(default)]
    pub args: Value,
    /// `confirm` (the policy asks) or `always` (asks under every policy).
    pub level: Level,
    pub why: String,
    /// What it does, in a few words.
    pub summary: String,
    /// What the user says to approve it.
    pub phrase: String,
    pub created: u64,
    pub expires: u64,
    #[serde(default)]
    pub answer: Option<Answer>,
    /// Why it was denied, for the agent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

/// How long a pending call waits before it counts as denied.
pub const PENDING_TTL: u64 = 24 * 3600;

// ---- storage ------------------------------------------------------------------

pub fn root() -> PathBuf {
    #[cfg(test)]
    if let Some(dir) = TEST_ROOT.with(|r| r.borrow().clone()) {
        return dir;
    }
    crate::state::data_dir().join("headless")
}

// Tests that make session folders put them here, never in the real data folder:
// a running Arakun would list a leftover one as a session.
#[cfg(test)]
thread_local! {
    pub(crate) static TEST_ROOT: std::cell::RefCell<Option<PathBuf>> = const { std::cell::RefCell::new(None) };
}

fn valid(id: &str) -> bool {
    !id.is_empty() && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
}

pub fn dir(id: &str) -> PathBuf {
    root().join(id)
}

pub fn transcript_path(id: &str) -> PathBuf {
    dir(id).join("transcript.json")
}

fn meta_path(id: &str) -> PathBuf {
    dir(id).join("meta.json")
}

fn pending_path(id: &str) -> PathBuf {
    dir(id).join("pending.json")
}

fn inbox(id: &str) -> PathBuf {
    dir(id).join("inbox")
}

fn write_json<T: Serialize>(path: &Path, v: &T) -> std::io::Result<()> {
    crate::durable::write(
        path,
        &serde_json::to_vec_pretty(v).map_err(std::io::Error::other)?,
    )
}

/// `write_json` into a session folder that must still be there. The check and
/// the write can't race a delete: with the folder gone, the write fails.
fn save_json<T: Serialize>(path: &Path, v: &T) -> std::io::Result<()> {
    crate::durable::replace(
        path,
        &serde_json::to_vec_pretty(v).map_err(std::io::Error::other)?,
    )
}

pub fn read_meta(id: &str) -> Option<Meta> {
    crate::durable::load_json(meta_path(id))
}

/// Saves a session that already has its folder. A folder that is gone was
/// deleted on purpose, and saving would make it again and bring the session
/// back: a host still saving while it stops writes nothing.
fn write_meta(m: &Meta) {
    match save_json(&meta_path(&m.id), m) {
        Ok(()) => {}
        // Deleted: nothing to save into.
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => eprintln!("arakun: could not save {}: {e}", meta_path(&m.id).display()),
    }
}

/// Saves a session, making its folder if needed: only for a new session.
fn create_meta(m: &Meta) {
    if let Err(e) = write_json(&meta_path(&m.id), m) {
        eprintln!("arakun: could not save {}: {e}", meta_path(&m.id).display());
    }
}

pub fn read_pending(id: &str) -> Option<Pending> {
    crate::durable::load_json(pending_path(id))
}

/// Sessions being deleted: their folder is still there while the host stops.
static DISCARDING: std::sync::Mutex<Vec<String>> = std::sync::Mutex::new(Vec::new());

pub fn discarding(id: &str) -> bool {
    DISCARDING.lock().is_ok_and(|d| d.iter().any(|x| x == id))
}

/// Hides `id` from [`list`] at once, before [`discard`] removes it.
pub fn begin_discard(id: &str) {
    if let Ok(mut d) = DISCARDING.lock() {
        d.push(id.to_string());
    }
}

/// Deletes a headless session for good: stops its host, then removes its folder.
/// The folder is what lists a session, so leaving it would bring the session back
/// on the app's next scan. Call [`begin_discard`] first, on the UI thread; this one
/// blocks while the host stops.
pub fn discard(id: &str) {
    if !valid(id) || !dir(id).exists() {
        if let Ok(mut d) = DISCARDING.lock() {
            d.retain(|x| x != id);
        }
        return;
    }
    // Only the host of this very session is signalled: a stale pid can belong
    // to another session's host, or to anything else, by now.
    if let Some(pid) = read_meta(id).and_then(|m| m.pid.filter(|p| hosts(*p, id))) {
        #[cfg(unix)]
        // SAFETY: a plain signal to the host, which saves and exits on it.
        unsafe {
            libc::kill(pid as libc::pid_t, libc::SIGTERM);
        }
        let until = Instant::now() + Duration::from_secs(5);
        while alive(Some(pid)) && Instant::now() < until {
            std::thread::sleep(Duration::from_millis(100));
        }
        // Still saving after 5 s: stop it outright. A host that keeps going
        // writes nothing once the folder is gone (see `write_meta`), but it
        // would keep spending the person's tokens.
        #[cfg(unix)]
        if hosts(pid, id) {
            // SAFETY: a plain signal to the same host, checked just above.
            unsafe {
                libc::kill(pid as libc::pid_t, libc::SIGKILL);
            }
            let until = Instant::now() + Duration::from_secs(1);
            while alive(Some(pid)) && Instant::now() < until {
                std::thread::sleep(Duration::from_millis(50));
            }
        }
    }
    match std::fs::remove_dir_all(dir(id)) {
        // The id stays hidden: a scan that read the folder just before the delete
        // can report it after this returns, and a deleted id never comes back.
        Ok(()) => {}
        Err(e) => {
            eprintln!("arakun: could not remove {}: {e}", dir(id).display());
            if let Ok(mut d) = DISCARDING.lock() {
                d.retain(|x| x != id);
            }
        }
    }
}

/// Every headless session, newest first.
pub fn list() -> Vec<Meta> {
    let mut all: Vec<Meta> = std::fs::read_dir(root())
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| read_meta(&e.file_name().to_string_lossy()))
        .filter(|m| !discarding(&m.id))
        .collect();
    all.sort_by_key(|m| std::cmp::Reverse(m.updated));
    all
}

/// The session `id` names, by its id or the start of one.
fn find(id: &str) -> Result<Option<Meta>, String> {
    if id.is_empty() {
        return Ok(None);
    }
    let all = list();
    if let Some(m) = all.iter().find(|m| m.id == id) {
        return Ok(Some(m.clone()));
    }
    let hits: Vec<&Meta> = all.iter().filter(|m| m.id.starts_with(id)).collect();
    match hits[..] {
        [m] => Ok(Some(m.clone())),
        [] => Ok(None),
        _ => Err(format!("more than one headless session starts with {id}")),
    }
}

/// The host of `pid` is still running (and the pid was not reused).
pub fn alive(pid: Option<u32>) -> bool {
    let Some(pid) = pid else { return false };
    #[cfg(unix)]
    {
        // SAFETY: signal 0 only checks that the process exists.
        if unsafe { libc::kill(pid as libc::pid_t, 0) } != 0 {
            return false;
        }
        match std::fs::read(format!("/proc/{pid}/cmdline")) {
            Ok(cmd) => String::from_utf8_lossy(&cmd).contains("agent-host"),
            #[cfg(target_os = "linux")]
            Err(_) => false,
            // No /proc: a pid the OS gave to another program is not ours.
            #[cfg(not(target_os = "linux"))]
            Err(_) => same_program(pid),
        }
    }
    #[cfg(not(unix))]
    {
        let _ = pid;
        false
    }
}

/// `pid` is the host of session `id`: `arakun agent-host <id>`, read from its
/// arguments. Checked before a signal, so a reused pid is never hit.
fn hosts(pid: u32, id: &str) -> bool {
    if !alive(Some(pid)) {
        return false;
    }
    host_args(pid).is_some_and(|args| args.windows(2).any(|w| w[0] == "agent-host" && w[1] == id))
}

#[cfg(target_os = "linux")]
fn host_args(pid: u32) -> Option<Vec<String>> {
    let raw = std::fs::read(format!("/proc/{pid}/cmdline")).ok()?;
    Some(
        raw.split(|b| *b == 0)
            .map(|a| String::from_utf8_lossy(a).into_owned())
            .collect(),
    )
}

/// No /proc here: `ps` gives the arguments. Only on a delete, so the extra
/// process is rare.
#[cfg(all(unix, not(target_os = "linux")))]
fn host_args(pid: u32) -> Option<Vec<String>> {
    let out = std::process::Command::new("ps")
        .args(["-o", "args=", "-p", &pid.to_string()])
        .output()
        .ok()?;
    out.status.success().then(|| {
        String::from_utf8_lossy(&out.stdout)
            .split_whitespace()
            .map(str::to_string)
            .collect()
    })
}

#[cfg(not(unix))]
fn host_args(_: u32) -> Option<Vec<String>> {
    None
}

/// The process `pid` runs this same program (macOS has no /proc to read).
#[cfg(target_os = "macos")]
fn same_program(pid: u32) -> bool {
    use std::os::unix::ffi::OsStrExt as _;
    // After a reboot a stale pid can be the app's own, which has the same name.
    if pid == std::process::id() {
        return false;
    }
    let mut buf = vec![0u8; libc::PROC_PIDPATHINFO_MAXSIZE as usize];
    // SAFETY: the buffer is as long as the size passed with it.
    let n = unsafe {
        libc::proc_pidpath(
            pid as libc::c_int,
            buf.as_mut_ptr().cast(),
            buf.len() as u32,
        )
    };
    if n <= 0 {
        return false;
    }
    let path = Path::new(std::ffi::OsStr::from_bytes(&buf[..n as usize]));
    std::env::current_exe().is_ok_and(|me| me.file_name() == path.file_name())
}

/// Other Unix systems without /proc: trust the pid.
#[cfg(all(unix, not(any(target_os = "linux", target_os = "macos"))))]
fn same_program(_: u32) -> bool {
    true
}

/// Status as a reader should take it: a host that died mid-turn left it stale.
pub fn status(m: &Meta) -> Status {
    match m.status {
        Status::Working | Status::NeedsInput if !alive(m.pid) => Status::Error,
        s => s,
    }
}

fn now() -> u64 {
    crate::model::unix_now()
}

fn new_id() -> String {
    let t = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    format!(
        "h{:x}{:02x}",
        t.as_millis(),
        (t.subsec_nanos() ^ std::process::id()) & 0xff
    )
}

fn clip(s: &str, max: usize) -> String {
    let s = s.trim();
    match s.char_indices().nth(max) {
        Some((i, _)) => format!("{}…", &s[..i]),
        None => s.to_string(),
    }
}

/// Queues `text` for the host to send.
fn post(id: &str, text: &str) -> std::io::Result<()> {
    let t = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    crate::durable::write(
        &inbox(id).join(format!("{:020}.txt", t.as_nanos())),
        text.as_bytes(),
    )
}

/// Prompts waiting in the inbox, oldest first, taken out of it.
fn take_inbox(id: &str) -> Vec<String> {
    let mut files: Vec<PathBuf> = std::fs::read_dir(inbox(id))
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.extension().is_some_and(|x| x == "txt")
                && !p
                    .file_name()
                    .is_some_and(|n| n.to_string_lossy().starts_with('.'))
        })
        .collect();
    files.sort();
    files
        .into_iter()
        .filter_map(|p| {
            let text = std::fs::read_to_string(&p).ok();
            let _ = std::fs::remove_file(&p);
            text
        })
        .collect()
}

fn inbox_empty(id: &str) -> bool {
    !std::fs::read_dir(inbox(id)).is_ok_and(|mut d| {
        d.any(|e| e.is_ok_and(|e| e.path().extension().is_some_and(|x| x == "txt")))
    })
}

fn workers_ai_model(model: &str) -> Option<&'static workers_ai::Model> {
    let name = model
        .strip_prefix(workers_ai::CLI_PREFIX)
        .or_else(|| model.strip_prefix("cloudflare-workers-ai/"))
        .unwrap_or(model);
    workers_ai::model(name)
}

/// The kind and settings a `--model` asks for.
fn agent_for(model: &str, effort: &str) -> Result<(AgentKind, Vec<(Setting, String)>), String> {
    if let Some(m) = workers_ai_model(model) {
        if !effort.is_empty() && !m.efforts.contains(&effort) {
            return Err(format!(
                "{model} takes no --effort {effort}; pick one of: {}",
                m.efforts.join(", ")
            ));
        }
        let mut settings = vec![(Setting::Model, m.id.to_string())];
        if !effort.is_empty() {
            settings.push((Setting::Effort, effort.to_string()));
        }
        return Ok((AgentKind::WorkersAi, settings));
    }
    if model.starts_with("claude") {
        return Err("Claude Code can't run headless yet: its own permission settings would get around --policy".into());
    }
    Err(format!(
        "arakun can't run {model} headless; pick one of:\n  {}",
        workers_ai::cli_models().join("\n  ")
    ))
}

/// Starts the host of `id`, its output to `host.log`: in a session of its own,
/// or `attached` to this process (`--wait`), which takes it down if it dies.
/// `append` goes at the end of a new session's system prompt.
fn spawn_host(
    id: &str,
    cwd: &Path,
    attached: bool,
    append: Option<&Path>,
) -> Result<std::process::Child, String> {
    let exe = std::env::current_exe().map_err(|e| format!("can't find arakun's binary: {e}"))?;
    let log = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir(id).join("host.log"))
        .map_err(|e| e.to_string())?;
    let err = log.try_clone().map_err(|e| e.to_string())?;
    let mut cmd = crate::platform::command(exe);
    cmd.arg("agent-host")
        .arg(id)
        .current_dir(cwd)
        .stdin(std::process::Stdio::null())
        .stdout(log)
        .stderr(err);
    if let Some(path) = append {
        cmd.env("ARAKUN_SYSTEM_APPEND", path);
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt as _;
        // SAFETY: setsid and prctl are async-signal-safe. The child leaves the
        // terminal's session, or gets a SIGTERM (and stops cleanly) when this
        // process dies.
        unsafe {
            cmd.pre_exec(move || {
                if !attached {
                    libc::setsid();
                }
                #[cfg(target_os = "linux")]
                if attached {
                    libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM);
                }
                Ok(())
            });
        }
    }
    #[cfg(not(unix))]
    let _ = attached;
    cmd.spawn()
        .map_err(|e| format!("could not start the agent: {e}"))
}

// ---- the command line --------------------------------------------------------

pub const RUN_USAGE: &str = "arakun agent run [--resume <id>] [--model <id>] [--dir <path>] [--title <t>] [--effort <e>] \
     [--policy readonly|workspace|full] [--allow net,modal,mail-read] [--read <path>]… [--no-sandbox [--yes]] \
     [--append-system-prompt <file>] [--wait [--timeout <secs>] [--result <file>]] [--] <prompt…>";

/// The options of `agent run`, flags only before the prompt.
#[derive(Debug, Default, PartialEq)]
struct Run {
    resume: String,
    model: String,
    dir: String,
    title: String,
    effort: String,
    policy: String,
    allow: String,
    reads: Vec<String>,
    no_sandbox: bool,
    /// Confirms `--no-sandbox` right here, for a script.
    yes: bool,
    /// A file whose text goes at the end of the system prompt.
    append: String,
    /// Blocks until the turn ends and prints its summary as JSON.
    wait: bool,
    timeout: String,
    result: String,
    prompt: String,
}

fn run_args(rest: &[String]) -> Result<Run, Stop> {
    let bad = || Stop::Usage;
    let mut r = Run::default();
    let mut i = 0;
    while let Some(flag) = rest.get(i) {
        let slot = match flag.as_str() {
            "--resume" => &mut r.resume,
            "--model" => &mut r.model,
            "--dir" | "--cwd" => &mut r.dir,
            "--title" => &mut r.title,
            "--effort" => &mut r.effort,
            "--policy" => &mut r.policy,
            "--allow" => &mut r.allow,
            "--append-system-prompt" => &mut r.append,
            "--timeout" => &mut r.timeout,
            "--result" => &mut r.result,
            "--no-sandbox" | "--yes" | "--wait" => {
                match flag.as_str() {
                    "--no-sandbox" => r.no_sandbox = true,
                    "--yes" => r.yes = true,
                    _ => r.wait = true,
                }
                i += 1;
                continue;
            }
            "--read" => {
                r.reads.push(
                    rest.get(i + 1)
                        .filter(|v| !v.is_empty())
                        .ok_or_else(bad)?
                        .clone(),
                );
                i += 2;
                continue;
            }
            _ => {
                i += flags_end(flag)?;
                break;
            }
        };
        *slot = rest
            .get(i + 1)
            .filter(|v| !v.is_empty())
            .ok_or_else(bad)?
            .clone();
        i += 2;
    }
    r.prompt = rest[i..].join(" ");
    if r.prompt.trim().is_empty() {
        return Err(bad());
    }
    Ok(r)
}

/// `[-n N] [--timeout S] id [text…]`: the id and the rest.
fn id_args(rest: &[String]) -> Option<(String, usize, u64, Vec<String>)> {
    let (mut lines, mut secs) = (crate::control::AGENT_LOG, crate::control::AGENT_WAIT);
    let mut it = rest.iter();
    let mut id = None;
    let mut tail = Vec::new();
    while let Some(a) = it.next() {
        if id.is_some() {
            tail.push(a.clone());
            continue;
        }
        match a.as_str() {
            "-n" | "--lines" => lines = it.next()?.parse().ok().filter(|n| *n > 0)?,
            "--timeout" => secs = it.next()?.parse().ok().filter(|s| *s > 0)?,
            _ if !a.starts_with('-') => id = Some(a.clone()),
            _ => return None,
        }
    }
    Some((id?, lines, secs, tail))
}

fn fail(message: &str) -> i32 {
    eprintln!("arakun: {message}");
    1
}

/// `arakun agent …` for headless sessions; `None` leaves it to the app.
pub fn cli(rest: &[String], cwd: &Path) -> Option<i32> {
    let (sub, args) = rest.split_first()?;
    type Found = Option<(Meta, usize, u64, Vec<String>)>;
    let session = |args: &[String]| -> Result<Found, String> {
        let Some((id, lines, secs, tail)) = id_args(args) else {
            return Ok(None);
        };
        Ok(find(&id)?.map(|m| (m, lines, secs, tail)))
    };
    match sub.as_str() {
        "run" => Some(match run_args(args) {
            Ok(r) => run(r, cwd),
            Err(stop) => crate::control::report(stop, RUN_USAGE),
        }),
        "ls" if args.is_empty() => Some(ls()),
        "pending" => Some(pending_cmd(args.iter().any(|a| a == "--json"))),
        "approve" | "deny" => Some(match args {
            [id, req, note @ ..] => answer(id, req, sub == "approve", &note.join(" ")),
            _ => {
                eprintln!(
                    "usage: arakun agent {sub} <id> <req_id>{}",
                    if sub == "deny" { " [reason…]" } else { "" }
                );
                2
            }
        }),
        "log" | "wait" | "send" | "stop" | "status" | "rm" => match session(args) {
            Err(e) => Some(fail(&e)),
            Ok(None) => None,
            Ok(Some((m, lines, secs, tail))) => Some(match sub.as_str() {
                "log" => {
                    println!("{}", log(&m, lines));
                    0
                }
                "status" => {
                    println!("{}", status(&m).name());
                    0
                }
                "wait" => wait(&m.id, Duration::from_secs(secs)),
                "send" if tail.join(" ").trim().is_empty() => {
                    eprintln!("usage: arakun agent send <id> <text…>");
                    2
                }
                "send" => send(&m, &tail.join(" ")),
                "stop" => stop(&m),
                _ => remove(&m),
            }),
        },
        _ => None,
    }
}

fn run(r: Run, cwd: &Path) -> i32 {
    let timeout = match r.timeout.parse::<u64>() {
        _ if r.timeout.is_empty() => None,
        Ok(s) if s > 0 => Some(Duration::from_secs(s)),
        _ => return fail(&format!("--timeout {}: give it in seconds", r.timeout)),
    };
    if !r.wait && (timeout.is_some() || !r.result.is_empty()) {
        return fail("--timeout and --result go with --wait");
    }
    let append = (!r.append.is_empty()).then(|| cwd.join(&r.append));
    if let Some(p) = &append
        && let Err(e) = std::fs::read_to_string(p)
    {
        return fail(&format!("--append-system-prompt {}: {e}", p.display()));
    }
    if !r.resume.is_empty() {
        return resume_run(&r, cwd, timeout, append.as_deref());
    }
    let model = if r.model.is_empty() {
        format!("{}{}", workers_ai::CLI_PREFIX, workers_ai::MODELS[0].id)
    } else {
        r.model.clone()
    };
    let (agent, _) = match agent_for(&model, &r.effort) {
        Ok(a) => a,
        Err(e) => return fail(&e),
    };
    let policy = match r.policy.as_str() {
        "" => Policy::Workspace,
        p => match Policy::parse(p) {
            Some(p) => p,
            None => {
                return fail(&format!(
                    "unknown --policy {p}: readonly, workspace or full"
                ));
            }
        },
    };
    let dir = if r.dir.is_empty() {
        cwd.to_path_buf()
    } else {
        cwd.join(&r.dir)
    };
    let dir = match crate::platform::canonicalize(&dir) {
        Ok(d) if d.is_dir() => d,
        _ => return fail(&format!("not a directory: {}", dir.display())),
    };
    let caps = match sandbox::parse_caps(&r.allow) {
        Ok(c) => c,
        Err(e) => return fail(&e),
    };
    if r.wait && r.no_sandbox && !r.yes {
        return fail("--wait --no-sandbox needs --yes: nobody is there to confirm it");
    }
    if agent == AgentKind::WorkersAi
        && let Some(m) = workers_ai_model(&model)
        && let Err(e) = workers_ai::endpoint_for(m)
    {
        return fail(&e);
    }
    let id = new_id();
    let reads: Vec<PathBuf> = r.reads.iter().map(|p| cwd.join(p)).collect();
    // A sandbox that won't start stops the run here, not halfway.
    let reads = if r.no_sandbox {
        reads
    } else {
        match Sandbox::new(&dir, &self::dir(&id), &caps, &reads) {
            Ok(sb) => sb.reads,
            Err(e) => {
                let _ = std::fs::remove_dir_all(self::dir(&id));
                return fail(&format!(
                    "{e}\nHeadless agents run sandboxed; nothing was started."
                ));
            }
        }
    };
    let title = if r.title.is_empty() {
        clip(
            r.prompt
                .lines()
                .map(str::trim)
                .find(|l| !l.is_empty())
                .unwrap_or(""),
            60,
        )
    } else {
        r.title.clone()
    };
    let mut meta = Meta {
        id: id.clone(),
        agent,
        model: model.clone(),
        effort: r.effort,
        policy,
        cwd: dir.clone(),
        title,
        created: now(),
        updated: now(),
        status: Status::Working,
        pid: None,
        last: String::new(),
        cost: 0.,
        error: None,
        session_id: None,
        caps,
        reads,
        no_sandbox: r.no_sandbox,
        unsandboxed: r.no_sandbox && r.yes,
        unattended: r.wait,
        declined: 0,
        stop_reason: None,
    };
    create_meta(&meta);
    if let Err(e) = post(&id, &r.prompt) {
        return fail(&format!("could not queue the prompt: {e}"));
    }
    let started = Instant::now();
    let host = match spawn_host(&id, &dir, r.wait, append.as_deref()) {
        Ok(child) => {
            // The host writes it too; this covers a reader in between.
            if let Some(mut m) = read_meta(&id).filter(|m| m.pid.is_none()) {
                m.pid = Some(child.id());
                write_meta(&m);
            }
            child
        }
        Err(e) => {
            meta.status = Status::Error;
            meta.error = Some(e.clone());
            write_meta(&meta);
            return fail(&e);
        }
    };
    let line = format!("{id}  {model}  {}  [{}]", dir.display(), meta.confinement());
    if r.wait {
        eprintln!("{line}");
        let result = (!r.result.is_empty()).then(|| cwd.join(&r.result));
        return wait_for_host(&id, host, started, timeout, result.as_deref());
    }
    println!("{line}");
    if r.no_sandbox && !meta.unsandboxed {
        println!("waiting for you to confirm --no-sandbox: arakun agent pending");
    }
    0
}

/// Resumes an existing headless session (`--resume <id>`) with a new prompt, keeping
/// its conversation history and KV-cache prefix warm.
fn resume_run(r: &Run, cwd: &Path, timeout: Option<Duration>, append: Option<&Path>) -> i32 {
    let m = match find(&r.resume) {
        Ok(Some(m)) => m,
        Ok(None) => return fail(&format!("no headless session {}", r.resume)),
        Err(e) => return fail(&e),
    };
    if alive(m.pid) {
        return fail(&format!("{} is still running", m.id));
    }
    let id = m.id.clone();
    let mut meta = m;
    meta.status = Status::Working;
    meta.error = None;
    meta.stop_reason = None;
    meta.pid = None;
    meta.unattended = r.wait || meta.unattended;
    meta.updated = now();
    write_meta(&meta);
    if let Err(e) = post(&id, &r.prompt) {
        return fail(&format!("could not queue the prompt: {e}"));
    }
    let started = Instant::now();
    let host = match spawn_host(&id, &meta.cwd, r.wait, append) {
        Ok(child) => {
            if let Some(mut cur) = read_meta(&id).filter(|cur| cur.pid.is_none()) {
                cur.pid = Some(child.id());
                write_meta(&cur);
            }
            child
        }
        Err(e) => {
            meta.status = Status::Error;
            meta.error = Some(e.clone());
            write_meta(&meta);
            return fail(&e);
        }
    };
    let line = format!(
        "{id}  {}  {}  [resumed; {}]",
        meta.model,
        meta.cwd.display(),
        meta.confinement()
    );
    if r.wait {
        eprintln!("{line}");
        let result = (!r.result.is_empty()).then(|| cwd.join(&r.result));
        return wait_for_host(&id, host, started, timeout, result.as_deref());
    }
    println!("{line}");
    0
}

/// `--wait`: blocks until the host is done, then prints the run's summary as
/// JSON (and writes it to `result`). A timeout or a signal stops the turn
/// first, the way `arakun agent stop` does. Exits 0 when the turn ended on its
/// own, 1 on an error, 124 on the timeout, 130 on a signal.
fn wait_for_host(
    id: &str,
    mut host: std::process::Child,
    started: Instant,
    timeout: Option<Duration>,
    result: Option<&Path>,
) -> i32 {
    let mut signal = crate::signals::first_signal();
    let mut stopped: Option<(Instant, &str)> = None;
    let exited = loop {
        match host.try_wait() {
            Ok(Some(s)) => break Some(s),
            Ok(None) => {}
            Err(_) => break None,
        }
        match stopped {
            None => {
                let why = if timeout.is_some_and(|t| started.elapsed() >= t) {
                    Some("timeout")
                } else if signal
                    .as_mut()
                    .is_some_and(|rx| matches!(rx.try_recv(), Ok(Some(()))))
                {
                    Some("signal")
                } else {
                    None
                };
                if let Some(why) = why {
                    eprintln!("arakun: stopping {id} ({why})");
                    #[cfg(unix)]
                    // SAFETY: a plain signal to our own child, which saves and exits on it.
                    unsafe {
                        libc::kill(host.id() as libc::pid_t, libc::SIGTERM);
                    }
                    #[cfg(not(unix))]
                    let _ = host.kill();
                    stopped = Some((Instant::now(), why));
                }
            }
            // It saves within seconds of a stop; past that, it is stuck.
            Some((at, _)) if at.elapsed() > Duration::from_secs(30) => {
                let _ = host.kill();
            }
            Some(_) => {}
        }
        std::thread::sleep(Duration::from_millis(200));
    };
    let wall = started.elapsed().as_secs_f64();
    let Some(m) = read_meta(id) else {
        return fail(&format!("{id} is gone"));
    };
    let s = status(&m);
    let usage = read_transcript(id).map(|t| t.usage).unwrap_or_default();
    let counts = m.session_id.as_deref().and_then(workers_ai::history_counts);
    let calls_by_effort = m.session_id.as_deref().and_then(workers_ai::effort_counts);
    let effort = match m.effort.as_str() {
        "" => workers_ai_model(&m.model).map_or("", |w| w.default_effort),
        e => e,
    };
    let summary = serde_json::json!({
        "id": m.id,
        "session_id": m.session_id,
        "status": s.name(),
        "stop_reason": m.stop_reason,
        "error": m.error,
        "interrupted": stopped.map(|(_, why)| why),
        "model": m.model,
        "effort": effort,
        "tokens": {
            "input": usage.input_tokens,
            "cached_input": usage.cache_read_tokens,
            "output": usage.output_tokens,
            "reasoning": usage.thinking_tokens,
        },
        "cost_usd": m.cost,
        "wall_secs": (wall * 10.).round() / 10.,
        "steps": counts.map(|c| c.0),
        "tool_calls": counts.map(|c| c.1),
        "effort_calls": calls_by_effort.map(|c| serde_json::json!({"high": c.0, "low": c.1})),
        "declined": m.declined,
        "cwd": m.cwd,
        "transcript": transcript_path(id),
        "history": m.session_id.as_deref().and_then(workers_ai::history_path),
        "host_log": dir(id).join("host.log"),
    });
    if let Some(path) = result
        && let Err(e) = write_json(path, &summary)
    {
        eprintln!("arakun: could not write {}: {e}", path.display());
    }
    println!(
        "{}",
        serde_json::to_string_pretty(&summary).unwrap_or_default()
    );
    match stopped {
        Some((_, "timeout")) => 124,
        Some(_) => 130,
        None if s == Status::Error || !exited.is_some_and(|e| e.success()) => 1,
        None => 0,
    }
}

fn row(m: &Meta) -> String {
    let pending = if read_pending(&m.id).is_some_and(|p| p.answer.is_none()) {
        "  [pending]"
    } else {
        ""
    };
    format!(
        "{}  {}  {}  {}  {}  [headless {}; {}]{pending}",
        m.id,
        status(m).name(),
        m.model,
        m.title,
        m.cwd.display(),
        m.policy.name(),
        m.confinement()
    )
}

fn ls() -> i32 {
    let rows: Vec<String> = list().iter().map(row).collect();
    for r in &rows {
        println!("{r}");
    }
    let req = crate::control::Request {
        cmd: "agent".into(),
        args: vec!["ls".into()],
        cwd: std::env::current_dir().unwrap_or_default(),
        side: false,
    };
    match crate::control::send(&req) {
        Ok(reply)
            if reply.ok && !(!rows.is_empty() && reply.message.starts_with("no OpenCode")) =>
        {
            println!("{}", reply.message)
        }
        Ok(reply) if !reply.ok => eprintln!("arakun: {}", reply.message),
        Err(_) if rows.is_empty() => println!("no agents; `arakun agent run <prompt>` starts one"),
        _ => {}
    }
    0
}

pub fn log(m: &Meta, lines: usize) -> String {
    let mut out = format!(
        "{}  {}  {}  {}  {}  [headless {}; {}]",
        m.id,
        status(m).name(),
        m.model,
        m.title,
        m.cwd.display(),
        m.policy.name(),
        m.confinement()
    );
    if let Some(e) = &m.error {
        out.push_str(&format!("\nerror: {e}"));
    }
    if m.cost > 0. {
        out.push_str(&format!("\ncost: ${:.4}", m.cost));
    }
    if let Some(p) = read_pending(&m.id).filter(|p| p.answer.is_none()) {
        out.push_str(&format!(
            "\npending {}: {} ({})",
            p.req_id, p.summary, p.why
        ));
    }
    let entries = Thread::read_transcript(&transcript_path(&m.id))
        .map(|t| t.entries)
        .unwrap_or_default();
    let log = agent::log::render_log(&entries, lines);
    out.push_str("\n\n");
    out.push_str(if log.is_empty() {
        "(nothing yet)"
    } else {
        &log
    });
    out
}

fn last_message(id: &str) -> String {
    Thread::read_transcript(&transcript_path(id))
        .ok()
        .and_then(|t| {
            t.entries.iter().rev().find_map(|e| match e {
                Entry::Agent { text } if !text.trim().is_empty() => Some(text.trim().to_string()),
                _ => None,
            })
        })
        .unwrap_or_default()
}

fn wait(id: &str, timeout: Duration) -> i32 {
    let start = Instant::now();
    loop {
        let Some(m) = read_meta(id) else {
            return fail(&format!("{id} is gone"));
        };
        let s = status(&m);
        // Answered, and the host has yet to take it up.
        let answered =
            s == Status::NeedsInput && !read_pending(id).is_some_and(|p| p.answer.is_none());
        if s != Status::Working && !answered {
            println!("{}", s.name());
            match s {
                Status::NeedsInput => {
                    if let Some(p) = read_pending(id) {
                        println!(
                            "pending {}: {}\n  {}\n  arakun agent approve {id} {}  |  arakun agent deny {id} {}",
                            p.req_id, p.summary, p.why, p.req_id, p.req_id
                        );
                    }
                }
                Status::Error => println!(
                    "{}",
                    m.error
                        .as_deref()
                        .unwrap_or("the agent's process ended mid-turn")
                ),
                _ => {
                    let last = last_message(id);
                    if !last.is_empty() {
                        println!("{last}");
                    }
                }
            }
            return if s == Status::Error { 1 } else { 0 };
        }
        if start.elapsed() >= timeout {
            return fail(&format!(
                "the session is still working after {} s",
                timeout.as_secs()
            ));
        }
        std::thread::sleep(Duration::from_millis(500));
    }
}

fn send(m: &Meta, text: &str) -> i32 {
    match continue_with(m, text) {
        Ok(()) => {
            println!("sent to {}", m.id);
            0
        }
        Err(e) => fail(&e),
    }
}

/// Queues `text` for the session, starting its host again if it ended: a
/// headless session goes on sandboxed, under its own policy and caps.
pub fn continue_with(m: &Meta, text: &str) -> Result<(), String> {
    post(&m.id, text).map_err(|e| format!("could not queue it: {e}"))?;
    if !alive(m.pid) {
        let mut m = m.clone();
        m.status = Status::Working;
        m.error = None;
        m.pid = None;
        m.updated = now();
        write_meta(&m);
        let pid = spawn_host(&m.id, &m.cwd, false, None)?.id();
        if let Some(mut m) = read_meta(&m.id).filter(|m| m.pid.is_none()) {
            m.pid = Some(pid);
            write_meta(&m);
        }
    }
    Ok(())
}

fn stop(m: &Meta) -> i32 {
    match m.pid.filter(|_| alive(m.pid)) {
        #[cfg_attr(not(unix), allow(unused_variables))]
        Some(pid) => {
            #[cfg(unix)]
            // SAFETY: a plain signal to the host, which saves and exits on it.
            unsafe {
                libc::kill(pid as libc::pid_t, libc::SIGTERM);
            }
            println!("stopping {}", m.id);
        }
        None => {
            if matches!(m.status, Status::Working | Status::NeedsInput) {
                let mut m = m.clone();
                m.status = Status::Idle;
                m.pid = None;
                write_meta(&m);
            }
            println!("{} is not working", m.id);
        }
    }
    0
}

fn remove(m: &Meta) -> i32 {
    if alive(m.pid) {
        return fail(&format!(
            "{} is still running; `arakun agent stop {}` first",
            m.id, m.id
        ));
    }
    match std::fs::remove_dir_all(dir(&m.id)) {
        Ok(()) => {
            println!("removed {}", m.id);
            0
        }
        Err(e) => fail(&e.to_string()),
    }
}

/// Pending calls of every session, unanswered and not expired.
pub fn pending_all() -> Vec<Pending> {
    let t = now();
    list()
        .iter()
        .filter(|m| alive(m.pid))
        .filter_map(|m| read_pending(&m.id))
        .filter(|p| p.answer.is_none() && p.expires > t)
        .collect()
}

fn pending_cmd(json: bool) -> i32 {
    let all = pending_all();
    if json {
        println!("{}", serde_json::to_string_pretty(&all).unwrap_or_default());
        return 0;
    }
    if all.is_empty() {
        println!("nothing is waiting for you");
    }
    for p in all {
        let title = read_meta(&p.session).map(|m| m.title).unwrap_or_default();
        let level = if p.level == Level::Always {
            "always"
        } else {
            "confirm"
        };
        println!(
            "{}  {}  [{level}]  {title}\n  {}\n  why: {}\n  say: \"{}\"",
            p.session, p.req_id, p.summary, p.why, p.phrase
        );
        if let Some(c) = &p.command {
            println!("  $ {}", clip(c, 300));
        }
    }
    0
}

/// Writes the user's answer for the host, which picks it up within a second.
pub fn resolve(id: &str, req: &str, approve: bool, note: &str) -> Result<String, String> {
    let m = find(id)?.ok_or_else(|| format!("no headless session {id}"))?;
    let path = pending_path(&m.id);
    let mut p: Pending =
        crate::durable::load_json(&path).ok_or_else(|| format!("{} has nothing pending", m.id))?;
    if p.req_id != req {
        return Err(format!("{} is waiting on {}, not {req}", m.id, p.req_id));
    }
    if p.answer.is_some() {
        return Err(format!("{req} was already answered"));
    }
    if p.expires <= now() {
        return Err(format!("{req} expired (denied)"));
    }
    if !alive(m.pid) {
        let _ = std::fs::remove_file(&path);
        return Err(format!(
            "{}'s agent is no longer running; nothing to answer",
            m.id
        ));
    }
    p.answer = Some(if approve {
        Answer::Approved
    } else {
        Answer::Denied
    });
    p.note = (!note.trim().is_empty()).then(|| note.trim().to_string());
    write_json(&path, &p).map_err(|e| e.to_string())?;
    Ok(format!(
        "{} {req}: {}",
        if approve { "approved" } else { "denied" },
        p.summary
    ))
}

fn answer(id: &str, req: &str, approve: bool, note: &str) -> i32 {
    match resolve(id, req, approve, note) {
        Ok(msg) => {
            println!("{msg}");
            0
        }
        Err(e) => fail(&e),
    }
}

// ---- the host ------------------------------------------------------------------

struct Keep(#[allow(dead_code)] Entity<Host>);
impl Global for Keep {}

/// `arakun agent-host <id>`: runs the session until it has nothing left to do.
pub fn host_main(args: &[String]) -> i32 {
    let [id] = args else {
        eprintln!("usage: arakun agent-host <id>");
        return 2;
    };
    if !valid(id) {
        return fail("bad session id");
    }
    let Some(mut meta) = read_meta(id) else {
        return fail(&format!("no headless session {id}"));
    };
    workers_ai::keep_warm(false);
    let me = std::process::id();
    if meta.pid.is_some_and(|p| p != me) && alive(meta.pid) {
        // Another host has it; the prompt in the inbox is its to send.
        return 0;
    }
    meta.pid = Some(me);
    meta.status = Status::Working;
    meta.error = None;
    // No sandbox, no run: never a quiet fallback to the host.
    let sandbox = if meta.no_sandbox {
        None
    } else {
        match Sandbox::new(&meta.cwd, &dir(id), &meta.caps, &meta.reads) {
            Ok(sb) => Some(sb),
            Err(e) => {
                meta.status = Status::Error;
                meta.error = Some(e.clone());
                meta.pid = None;
                write_meta(&meta);
                return fail(&e);
            }
        }
    };
    sandbox::set(Some(Jail {
        sandbox,
        caps: meta.caps.clone(),
        workspace: meta.cwd.clone(),
    }));
    write_meta(&meta);
    let code = Rc::new(Cell::new(0));
    let out = code.clone();
    gpui_platform::headless().run(move |cx: &mut App| {
        let host = cx.new(|cx| Host::new(meta, out, cx));
        cx.set_global(Keep(host));
    });
    code.get()
}

struct Host {
    meta: Meta,
    thread: Entity<Thread>,
    ctx: policy::Ctx,
    /// The call waiting in pending.json, by its tool call id.
    waiting: Option<(String, Pending)>,
    dirty: bool,
    saved: Instant,
    started: Instant,
    /// A turn ran (or failed) since the host started.
    worked: bool,
    stopping: Option<Instant>,
    code: Rc<Cell<i32>>,
    done: bool,
    _events: gpui::Subscription,
    _tick: Option<gpui::Task<()>>,
}

const TICK: Duration = Duration::from_millis(300);
/// What `waiting` holds while `--no-sandbox` waits for the user.
const NO_SANDBOX: &str = "arakun:no-sandbox";
const SAVE_EVERY: Duration = Duration::from_secs(1);

impl Host {
    fn new(meta: Meta, code: Rc<Cell<i32>>, cx: &mut Context<Self>) -> Self {
        let transcript = transcript_path(&meta.id);
        let restored = transcript
            .exists()
            .then(|| Thread::restore(&transcript, cx))
            .and_then(|r| {
                r.map_err(|e| eprintln!("arakun: transcript {}: {e:#}", meta.id))
                    .ok()
            });
        let thread = match restored {
            Some(t) => t,
            None => {
                let (kind, settings) =
                    agent_for(&meta.model, &meta.effort).unwrap_or((meta.agent, Vec::new()));
                let t = Thread::start(kind, meta.cwd.clone(), None, cx);
                t.update(cx, |t, cx| {
                    for (s, v) in &settings {
                        t.set_setting(*s, v, cx);
                    }
                    if !meta.title.is_empty() {
                        t.rename(&meta.title, cx);
                    }
                });
                t
            }
        };
        let events = cx.subscribe(&thread, |this, _, ev: &ThreadEvent, cx| {
            this.thread_event(ev, cx)
        });
        let mut ctx = policy::Ctx::new(&meta.cwd);
        ctx.jailed = !meta.no_sandbox;
        ctx.tmp = ctx.jailed.then(|| dir(&meta.id).join("tmp"));
        let mut host = Host {
            meta,
            thread,
            ctx,
            waiting: None,
            dirty: true,
            saved: Instant::now(),
            started: Instant::now(),
            worked: false,
            stopping: None,
            code,
            done: false,
            _events: events,
            _tick: None,
        };
        if host.meta.no_sandbox && !host.meta.unsandboxed {
            host.ask_unsandboxed(cx);
        }
        if let Some(rx) = crate::signals::first_signal() {
            cx.spawn(async move |this, cx| {
                if rx.await.is_ok() {
                    let _ = this.update(cx, |this, cx| this.stop(cx));
                }
            })
            .detach();
        }
        host._tick = Some(crate::poll::poll_every(cx, TICK, TICK, Self::tick));
        host
    }

    fn thread_event(&mut self, ev: &ThreadEvent, cx: &mut Context<Self>) {
        self.dirty = true;
        match ev {
            ThreadEvent::NeedsPermission => self.permission(cx),
            ThreadEvent::Finished { stop_reason } => {
                self.worked = true;
                self.meta.stop_reason = Some(stop_reason.clone());
                self.save(cx);
            }
            ThreadEvent::StatusChanged => {
                if matches!(self.thread.read(cx).status(), ThreadStatus::Error(_)) {
                    self.worked = true;
                }
                self.save(cx);
            }
            _ => {}
        }
    }

    fn stop(&mut self, cx: &mut Context<Self>) {
        if self.stopping.is_some() {
            return;
        }
        self.stopping = Some(Instant::now());
        if self.waiting.take().is_some() {
            let _ = std::fs::remove_file(pending_path(&self.meta.id));
        }
        self.thread.update(cx, |t, cx| t.cancel(cx));
    }

    /// `--no-sandbox` runs nothing until the user confirms it.
    fn ask_unsandboxed(&mut self, cx: &mut Context<Self>) {
        let created = now();
        let p = Pending {
            req_id: format!("r{:x}", (created as u32) ^ std::process::id()),
            session: self.meta.id.clone(),
            tool: "sandbox".into(),
            title: "--no-sandbox".into(),
            command: None,
            args: Value::Null,
            level: Level::Always,
            why: "no sandbox: its commands run with your credentials and your network".into(),
            summary: format!(
                "Let the agent \"{}\" run without a sandbox in {}",
                self.meta.title,
                self.meta.cwd.display()
            ),
            phrase: "confirm: no sandbox".into(),
            created,
            expires: created + PENDING_TTL,
            answer: None,
            note: None,
        };
        if let Err(e) = write_json(&pending_path(&self.meta.id), &p) {
            eprintln!("arakun: could not write the pending request: {e}");
        }
        eprintln!("arakun: waiting for the user: {} ({})", p.summary, p.req_id);
        self.waiting = Some((NO_SANDBOX.into(), p));
        self.save(cx);
    }

    fn unsandboxed(&mut self, approved: bool, cx: &mut Context<Self>) {
        if approved {
            self.meta.unsandboxed = true;
            self.save(cx);
            return;
        }
        self.done = true;
        let mut m = self.meta.clone();
        m.pid = None;
        m.status = Status::Error;
        m.error = Some("you did not let it run without a sandbox; nothing ran".into());
        write_meta(&m);
        self.code.set(1);
        cx.quit();
    }

    /// The agent asks to run a tool call: the policy lets it, or the user is asked.
    fn permission(&mut self, cx: &mut Context<Self>) {
        let t = self.thread.read(cx);
        let Some(req) = t.pending_permission() else {
            return;
        };
        if self
            .waiting
            .as_ref()
            .is_some_and(|(id, _)| *id == req.tool_call_id)
        {
            return;
        }
        let tool = t.entries().iter().rev().find_map(|e| match e {
            Entry::Tool(tc) if tc.id == req.tool_call_id => Some(tc.clone()),
            _ => None,
        });
        let (kind, title, input) = match &tool {
            Some(tc) => (
                tc.kind.clone(),
                tc.title.clone(),
                tc.raw_input.clone().unwrap_or(Value::Null),
            ),
            None => ("other".to_string(), req.title.clone(), Value::Null),
        };
        let call_id = req.tool_call_id.clone();
        let verdict = policy::judge_tool(self.meta.policy, &self.ctx, &kind, &title, &input);
        let lab_allow = self.meta.unattended
            && self.meta.no_sandbox
            && self.meta.unsandboxed
            && self.meta.policy == Policy::Full
            && std::env::var_os("ARAKUN_LAB_ALLOW_ALL").is_some_and(|v| v == "1");
        if verdict.level == Level::Allow || lab_allow {
            self.thread.update(cx, |t, cx| t.allow_permission(cx));
            return;
        }
        if self.meta.unattended {
            // The agent reads this reason: `told`, never `why`, which may quote
            // files the agent can't read.
            let why = format!(
                "nobody can approve it in this unattended run: {}",
                verdict.told
            );
            eprintln!("arakun: declined: {} ({})", verdict.summary, verdict.why);
            self.meta.declined += 1;
            self.thread
                .update(cx, |t, cx| t.decline_permission(&why, cx));
            return;
        }
        let command = match input.get("command") {
            Some(Value::String(s)) => Some(s.clone()),
            Some(Value::Array(a)) => Some(
                a.iter()
                    .filter_map(Value::as_str)
                    .collect::<Vec<_>>()
                    .join(" "),
            ),
            _ => None,
        };
        let created = now();
        let p = Pending {
            req_id: format!(
                "r{:x}",
                (created as u32) ^ (call_id.len() as u32).rotate_left(13) ^ std::process::id()
            ),
            session: self.meta.id.clone(),
            tool: kind,
            title,
            command,
            args: input,
            level: verdict.level,
            why: verdict.why,
            summary: verdict.summary,
            phrase: verdict.phrase,
            created,
            expires: created + PENDING_TTL,
            answer: None,
            note: None,
        };
        if let Err(e) = write_json(&pending_path(&self.meta.id), &p) {
            eprintln!("arakun: could not write the pending request: {e}");
            self.thread.update(cx, |t, cx| {
                t.decline_permission("arakun could not ask the user", cx)
            });
            return;
        }
        eprintln!("arakun: waiting for the user: {} ({})", p.summary, p.req_id);
        self.waiting = Some((call_id, p));
        self.save(cx);
    }

    /// The user's answer, or the wait running out.
    fn check_answer(&mut self, cx: &mut Context<Self>) {
        let Some((call, waiting)) = &self.waiting else {
            return;
        };
        let gate = call == NO_SANDBOX;
        let path = pending_path(&self.meta.id);
        let seen: Option<Pending> = crate::durable::load_json(&path);
        let expired = waiting.expires <= now();
        let decision = match seen.filter(|p| p.req_id == waiting.req_id) {
            Some(p) if p.answer.is_some() => Some((p.answer == Some(Answer::Approved), p.note)),
            // Deleted, or out of time: denied.
            None => Some((false, None)),
            Some(_) if expired => Some((false, Some("no answer within 24 hours".into()))),
            Some(_) => None,
        };
        let Some((approved, note)) = decision else {
            return;
        };
        let _ = std::fs::remove_file(&path);
        let summary = waiting.summary.clone();
        self.waiting = None;
        eprintln!(
            "arakun: {}: {summary}",
            if approved { "approved" } else { "denied" }
        );
        if gate {
            return self.unsandboxed(approved, cx);
        }
        // The agent may have moved on (a stop); only answer the call still asked.
        if self.thread.read(cx).pending_permission().is_none() {
            return;
        }
        self.thread.update(cx, |t, cx| {
            if approved {
                t.allow_permission(cx)
            } else {
                t.decline_permission(note.as_deref().unwrap_or(""), cx)
            }
        });
        self.save(cx);
    }

    fn tick(&mut self, cx: &mut Context<Self>) {
        if self.done {
            return;
        }
        // The session was deleted, maybe before this host's pid was saved, so
        // nothing could signal it: stop on our own, saving nothing.
        if !dir(&self.meta.id).is_dir() {
            self.done = true;
            self.code.set(0);
            cx.quit();
            return;
        }
        let gated = self.waiting.as_ref().is_some_and(|(c, _)| c == NO_SANDBOX);
        if self.stopping.is_none() && !gated {
            for mut text in take_inbox(&self.meta.id) {
                // Notes for the agent (a branch rename) arrive wrapped in the
                // prompt: hand them over as notes, so the chat shows only the
                // prompt.
                let notes = crate::agent::chart::take_hints(&mut text);
                self.thread.update(cx, |t, cx| {
                    for note in notes {
                        t.add_note(note);
                    }
                    t.send(text, cx)
                });
                self.dirty = true;
            }
        }
        self.check_answer(cx);
        if self.dirty && self.saved.elapsed() >= SAVE_EVERY {
            self.save(cx);
        }
        let t = self.thread.read(cx);
        // Subagents the agent left running in the background are still its work.
        let settled = matches!(t.status(), ThreadStatus::Idle | ThreadStatus::Error(_))
            && !t.is_busy()
            && (!t.in_background() || matches!(t.status(), ThreadStatus::Error(_)))
            && (t.queued().is_empty() || t.queue_paused())
            && self.waiting.is_none();
        let stuck_stopping = self
            .stopping
            .is_some_and(|s| s.elapsed() > Duration::from_secs(10));
        let finished = settled
            && (self.worked
                || self.stopping.is_some()
                || self.started.elapsed() > Duration::from_secs(30));
        if (finished && (inbox_empty(&self.meta.id) || self.stopping.is_some())) || stuck_stopping {
            self.done = true;
            self.save(cx);
            let mut m = self.meta.clone();
            m.pid = None;
            if m.status == Status::Working || m.status == Status::NeedsInput {
                m.status = Status::Idle;
            }
            write_meta(&m);
            self.code.set(if m.status == Status::Error { 1 } else { 0 });
            cx.quit();
        }
    }

    /// meta.json and transcript.json as the session is now.
    fn save(&mut self, cx: &mut Context<Self>) {
        let t = self.thread.read(cx);
        let transcript = t.transcript(cx);
        let status = if self.waiting.is_some() {
            Status::NeedsInput
        } else {
            match t.status() {
                ThreadStatus::Error(_) => Status::Error,
                ThreadStatus::NeedsInput => Status::NeedsInput,
                ThreadStatus::Idle
                    if !t.is_busy()
                        && !t.in_background()
                        && (t.queued().is_empty() || t.queue_paused())
                        && inbox_empty(&self.meta.id) =>
                {
                    Status::Idle
                }
                _ => Status::Working,
            }
        };
        let m = &mut self.meta;
        m.status = status;
        m.error = match t.status() {
            ThreadStatus::Error(e) => Some(e.lines().next().unwrap_or("").trim().to_string()),
            _ => None,
        };
        if !t.title().is_empty() {
            m.title = t.title().to_string();
        }
        m.session_id = t.session_id().map(str::to_string).or(m.session_id.take());
        m.cost = t.usage().cost.as_ref().map_or(m.cost, |(c, _)| *c);
        m.last = last_line(t.entries());
        m.updated = now();
        let pid = std::process::id();
        m.pid = Some(pid);
        // Deleted while running: saving would bring the folder back.
        if !dir(&m.id).is_dir() {
            self.dirty = false;
            return;
        }
        if let Err(e) = save_json(&transcript_path(&m.id), &transcript) {
            eprintln!("arakun: could not save the transcript: {e}");
        }
        write_meta(m);
        self.dirty = false;
        self.saved = Instant::now();
    }
}

/// What the session did last, in a line.
fn last_line(entries: &[Entry]) -> String {
    entries
        .iter()
        .rev()
        .find_map(|e| match e {
            Entry::Agent { text } | Entry::Thought { text } if !text.trim().is_empty() => Some(
                text.trim()
                    .lines()
                    .rev()
                    .find(|l| !l.trim().is_empty())
                    .unwrap_or("")
                    .to_string(),
            ),
            Entry::Tool(t) => Some(format!("[{}] {:?}", t.title, t.status).to_lowercase()),
            Entry::Notice { text, .. } => Some(text.clone()),
            Entry::User { text, .. } => Some(format!("you: {text}")),
            _ => None,
        })
        .map(|l| clip(&l, 160))
        .unwrap_or_default()
}

/// What a viewer shows of a session's status.
pub fn thread_status(m: &Meta) -> ThreadStatus {
    match status(m) {
        Status::Working => ThreadStatus::Working,
        Status::NeedsInput => ThreadStatus::NeedsInput,
        Status::Idle => ThreadStatus::Idle,
        Status::Error => ThreadStatus::Error(
            m.error
                .clone()
                .unwrap_or_else(|| "The agent's process ended mid-turn.".into()),
        ),
    }
}

pub fn read_transcript(id: &str) -> Option<agent::Transcript> {
    Thread::read_transcript(&transcript_path(id)).ok()
}

thread_local! {
    /// Headless sessions the app lists: never saved in its state.
    static SHOWN: std::cell::RefCell<std::collections::HashSet<String>> = Default::default();
}

/// The app lists `id` as a headless session (it isn't its own to save).
pub fn shown(id: &str) -> bool {
    SHOWN.with(|s| s.borrow().contains(id))
}

pub fn set_shown(id: &str, on: bool) {
    SHOWN.with(|s| {
        let mut s = s.borrow_mut();
        if on {
            s.insert(id.to_string());
        } else {
            s.remove(id);
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(a: &[&str]) -> Vec<String> {
        a.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn run_takes_its_flags_before_the_prompt() {
        let r = run_args(&s(&[
            "--model",
            "workers-ai/@cf/zai-org/glm-5.3",
            "--dir",
            "/tmp/x",
            "--policy",
            "readonly",
            "fix",
            "--policy",
            "full",
        ]))
        .unwrap();
        assert_eq!(r.model, "workers-ai/@cf/zai-org/glm-5.3");
        assert_eq!(r.dir, "/tmp/x");
        assert_eq!(r.policy, "readonly");
        assert_eq!(r.prompt, "fix --policy full");
        let r = run_args(&s(&["--title", "T", "--effort", "max", "go"])).unwrap();
        assert_eq!(
            (r.title.as_str(), r.effort.as_str(), r.prompt.as_str()),
            ("T", "max", "go")
        );
        assert!(run_args(&s(&[])).is_err());
        assert!(run_args(&s(&["--model"])).is_err());
        assert!(run_args(&s(&["--model", "", "go"])).is_err());
        assert!(run_args(&s(&["--dir", "x"])).is_err());
        assert!(run_args(&s(&["  "])).is_err());
        let r = run_args(&s(&["--read", "/a", "--read", "b", "go"])).unwrap();
        assert_eq!(r.reads, s(&["/a", "b"]));
        assert!(run_args(&s(&["--read"])).is_err());
        let r = run_args(&s(&[
            "--no-sandbox",
            "--allow",
            "net,modal",
            "go",
            "--no-sandbox",
        ]))
        .unwrap();
        assert!(r.no_sandbox);
        assert_eq!(
            (r.allow.as_str(), r.prompt.as_str()),
            ("net,modal", "go --no-sandbox")
        );
        assert!(!run_args(&s(&["go"])).unwrap().no_sandbox);
        let r = run_args(&s(&[
            "--wait",
            "--yes",
            "--no-sandbox",
            "--cwd",
            "w",
            "--timeout",
            "600",
            "--result",
            "r.json",
            "--append-system-prompt",
            "v.md",
            "go",
        ]))
        .unwrap();
        assert!(r.wait && r.yes && r.no_sandbox);
        assert_eq!(
            (
                r.dir.as_str(),
                r.timeout.as_str(),
                r.result.as_str(),
                r.append.as_str()
            ),
            ("w", "600", "r.json", "v.md")
        );
        let r = run_args(&s(&[
            "--resume",
            "h123",
            "--wait",
            "--timeout",
            "300",
            "--result",
            "r2.json",
            "fix",
            "it",
        ]))
        .unwrap();
        assert_eq!((r.resume.as_str(), r.prompt.as_str()), ("h123", "fix it"));
        assert!(!run_args(&s(&["go", "--wait"])).unwrap().wait);
    }

    #[test]
    fn run_stops_at_help_or_a_flag_it_does_not_know() {
        // `arakun agent run --help` started a paid run with `--help` as the prompt.
        assert_eq!(run_args(&s(&["--help"])), Err(Stop::Help));
        assert_eq!(run_args(&s(&["--model", "m", "-h", "go"])), Err(Stop::Help));
        assert_eq!(
            run_args(&s(&["--modle", "m", "go"])),
            Err(Stop::Flag("--modle".into()))
        );
        assert_eq!(run_args(&s(&["go", "--help"])).unwrap().prompt, "go --help");
        assert_eq!(run_args(&s(&["--", "--help"])).unwrap().prompt, "--help");
        assert_eq!(run_args(&s(&["- fix it"])).unwrap().prompt, "- fix it");
    }

    #[test]
    fn ids_come_with_their_options() {
        assert_eq!(
            id_args(&s(&["abc"])),
            Some((
                "abc".into(),
                crate::control::AGENT_LOG,
                crate::control::AGENT_WAIT,
                vec![]
            ))
        );
        assert_eq!(id_args(&s(&["-n", "5", "abc"])).map(|a| a.1), Some(5));
        assert_eq!(
            id_args(&s(&["--timeout", "9", "abc"])).map(|a| a.2),
            Some(9)
        );
        assert_eq!(
            id_args(&s(&["abc", "hello", "-n", "x"])).map(|a| a.3),
            Some(s(&["hello", "-n", "x"]))
        );
        assert_eq!(id_args(&s(&["-n", "0", "abc"])), None);
        assert_eq!(id_args(&s(&["-x"])), None);
        assert_eq!(id_args(&s(&[])), None);
    }

    #[test]
    fn models_map_to_agents() {
        let (kind, settings) = agent_for("workers-ai/@cf/zai-org/glm-5.3", "high").unwrap();
        assert_eq!(kind, AgentKind::WorkersAi);
        assert_eq!(
            settings,
            vec![
                (Setting::Model, "@cf/zai-org/glm-5.3".to_string()),
                (Setting::Effort, "high".to_string())
            ]
        );
        assert!(agent_for("workers-ai/@cf/zai-org/glm-5.3", "none").is_err());
        assert!(agent_for("workers-ai/@cf/deepseek-ai/deepseek-v4.1-flash-1017", "").is_ok());
        assert!(agent_for("claude", "").unwrap_err().contains("Claude Code"));
        assert!(agent_for("openai/gpt", "").is_err());
    }

    #[test]
    fn a_session_being_deleted_stays_hidden_until_it_is_gone() {
        // No folder by this name, so discard has nothing to stop or remove.
        let id = "h-test-discard-0";
        assert!(!discarding(id));
        begin_discard(id);
        assert!(discarding(id));
        discard(id);
        assert!(!discarding(id));
    }

    // Linux only: on macOS `alive` also wants the process to be this very
    // program (proc_pidpath), which a `sh` stand-in can't be.
    #[cfg(target_os = "linux")]
    #[test]
    fn only_the_sessions_own_host_is_signalled() {
        // `sh -c … agent-host h-1` has the arguments a host for h-1 has. The
        // trailing `; :` keeps sh from exec'ing into `sleep`, which would drop
        // those arguments, and the wait covers the moment before sh starts.
        let mut host = std::process::Command::new("sh")
            .args(["-c", "sleep 30; :", "agent-host", "h-1"])
            .spawn()
            .unwrap();
        let until = Instant::now() + Duration::from_secs(5);
        while !hosts(host.id(), "h-1") && Instant::now() < until {
            std::thread::sleep(Duration::from_millis(10));
        }
        let mut other = std::process::Command::new("sleep")
            .arg("30")
            .spawn()
            .unwrap();
        assert!(hosts(host.id(), "h-1"));
        assert!(!hosts(host.id(), "h-2"));
        assert!(!hosts(other.id(), "h-1"));
        assert!(!hosts(std::process::id(), "h-1"));
        for c in [&mut host, &mut other] {
            let _ = c.kill();
            let _ = c.wait();
        }
    }

    #[cfg(unix)]
    #[test]
    fn a_process_that_is_no_host_is_never_signalled() {
        let mut other = std::process::Command::new("sleep")
            .arg("30")
            .spawn()
            .unwrap();
        assert!(!hosts(other.id(), "h-1"));
        assert!(!hosts(std::process::id(), "h-1"));
        let _ = other.kill();
        let _ = other.wait();
    }

    #[test]
    fn a_deleted_session_folder_stays_gone() {
        let tmp =
            std::env::temp_dir().join(format!("arakun-headless-discard-{}", std::process::id()));
        TEST_ROOT.with(|r| *r.borrow_mut() = Some(tmp.clone()));
        let id = "h-test-discard-1".to_string();
        let m = Meta {
            id: id.clone(),
            agent: AgentKind::WorkersAi,
            model: String::new(),
            effort: String::new(),
            policy: Policy::ReadOnly,
            cwd: "/tmp/x".into(),
            title: "t".into(),
            created: 1,
            updated: 2,
            status: Status::Idle,
            pid: None,
            last: String::new(),
            cost: 0.,
            error: None,
            session_id: None,
            caps: Vec::new(),
            reads: Vec::new(),
            no_sandbox: false,
            unsandboxed: false,
            unattended: false,
            declined: 0,
            stop_reason: None,
        };
        create_meta(&m);
        assert!(dir(&id).is_dir());
        begin_discard(&id);
        discard(&id);
        assert!(!dir(&id).exists());
        // Still hidden, so a scan from just before the delete can't bring it back.
        assert!(discarding(&id));
        // A host still saving afterwards does not make it again.
        write_meta(&m);
        assert!(!dir(&id).exists());
        assert!(save_json(&transcript_path(&id), &1).is_err());
        assert!(!dir(&id).exists());
        TEST_ROOT.with(|r| *r.borrow_mut() = None);
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn meta_and_pending_round_trip() {
        let m = Meta {
            id: "h1".into(),
            agent: AgentKind::WorkersAi,
            model: "workers-ai/@cf/zai-org/glm-5.3".into(),
            effort: String::new(),
            policy: Policy::ReadOnly,
            cwd: "/tmp/x".into(),
            title: "t".into(),
            created: 1,
            updated: 2,
            status: Status::NeedsInput,
            pid: Some(7),
            last: "[ls] completed".into(),
            cost: 0.25,
            error: None,
            session_id: Some("s".into()),
            caps: vec![Cap::Modal, Cap::MailRead],
            reads: vec!["/srv/data".into()],
            no_sandbox: false,
            unsandboxed: false,
            unattended: true,
            declined: 2,
            stop_reason: Some("end_turn".into()),
        };
        let v = serde_json::to_value(&m).unwrap();
        assert_eq!(v["caps"], serde_json::json!(["modal", "mail-read"]));
        assert_eq!(
            m.confinement(),
            "sandbox, allow modal,mail-read, read /srv/data"
        );
        // Sessions from before the sandbox load sandboxed.
        let mut old = v.clone();
        for k in [
            "caps",
            "reads",
            "no_sandbox",
            "unsandboxed",
            "unattended",
            "declined",
            "stop_reason",
        ] {
            old.as_object_mut().unwrap().remove(k);
        }
        let old: Meta = serde_json::from_value(old).unwrap();
        assert!(
            old.caps.is_empty() && !old.no_sandbox && !old.unattended && old.stop_reason.is_none()
        );
        assert_eq!(v["status"], "needs_input");
        assert_eq!(v["policy"], "readonly");
        assert_eq!(v["agent"], "workers-ai");
        assert_eq!(serde_json::from_value::<Meta>(v).unwrap(), m);
        let p = Pending {
            req_id: "r1".into(),
            session: "h1".into(),
            tool: "execute".into(),
            title: "git push".into(),
            command: Some("git push".into()),
            args: serde_json::json!({"command": "git push"}),
            level: Level::Always,
            why: "w".into(),
            summary: "s".into(),
            phrase: "confirm: git push".into(),
            created: 1,
            expires: 2,
            answer: Some(Answer::Denied),
            note: None,
        };
        let v = serde_json::to_value(&p).unwrap();
        assert_eq!(v["level"], "always");
        assert_eq!(v["answer"], "denied");
        assert_eq!(serde_json::from_value::<Pending>(v).unwrap(), p);
    }

    #[test]
    fn transcripts_round_trip_in_the_app_format() {
        let t = agent::Transcript {
            version: 1,
            meta: agent::SessionMeta {
                agent: AgentKind::WorkersAi,
                session_id: Some("s".into()),
                title: "t".into(),
                cwd: "/tmp/x".into(),
                created: 1,
                updated: 2,
                pr: None,
                seat: None,
                seat_key: None,
            },
            entries: vec![
                Entry::User {
                    text: "hi".into(),
                    at: 1,
                    elapsed_ms: None,
                    message_id: None,
                    tokens: None,
                    shell_changes: Vec::new(),
                },
                Entry::Agent {
                    text: "done\n".into(),
                },
            ],
            usage: agent::Usage::default(),
            fork_point: None,
            interrupted: false,
            resume: false,
            queue: Vec::new(),
            handoff: None,
            subagents: Vec::new(),
        };
        let dir = std::env::temp_dir().join(format!("arakun-headless-{}", std::process::id()));
        let path = dir.join("transcript.json");
        write_json(&path, &t).unwrap();
        let back = Thread::read_transcript(&path).unwrap();
        assert_eq!(back.meta.session_id.as_deref(), Some("s"));
        assert_eq!(back.entries.len(), 2);
        assert_eq!(last_line(&back.entries), "done");
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
