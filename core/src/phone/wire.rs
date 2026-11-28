//! What goes down the WebSocket, as JSON. Every message has a `type`. Entries
//! are the transcript's, cut down for a phone: long tool output is clipped,
//! diffs travel as counts, and the fields only the desktop uses stay home.
//!
//! Server → phone:
//! - `inbox`: every session of every project, with its status.
//! - `session`: a session just opened: its tail of entries and its state.
//! - `entries`: entries from `from` on changed; `count` is the new total.
//! - `append`: only the words of the last entry `n` changed: they are their
//!   first `at` UTF-16 units as the client has them, then `text`. Only to a
//!   client that opened the session with `append` (see [`EntryStream`]).
//! - `state`: a watched session's status, permission or usage changed.
//! - `usage`: both agents' plan limits, on request and when they change.
//! - `subagents`: the subagents of a watched session, when the list changes.
//!   A phone gets each one's name, task, state and times (`brief`); the app
//!   gets them whole, with their transcripts.
//! - `subagent_entries`: a subagent's entries from `from` on, as `entries`
//!   are a session's: the tail when a phone opens it, then every change, and
//!   older pages on request (`older`).
//! - `transcript`: a session's entries whole, on request: for the app, which
//!   shows a server's session as its own.
//! - `part`: a piece of a message too big for one frame.
//! - `ack`: the server read the message the client numbered `seq`.
//! - `error`.
//!
//! A session's `session` and `state` carry the card it shows: `permission`,
//! `question` or `secret` (`agent::secret`). A `secret` says whether it is
//! `private`; only then does a device send what was typed, with `secret`.
//!
//! A client may number what it sends with `seq`. The server then answers
//! each with an `ack` once it has read it, so a client can tell a message
//! lost in a dropped connection from one that arrived. It says it does with
//! the [`ACKS_HEADER`] on its own socket, and with `acks` in its `ok` or
//! `paired` through the relay.

use std::cell::OnceCell;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde_json::{Value, json};

use crate::account::{self, Limits, UsageMode};
use crate::agent::plan::PlanUsage;
use crate::agent::{
    AskUserRequest, Choice, Entry, PermissionRequest, Setting, SettingOption, Subagent,
    SubagentRecord, SubagentState, Thread, ToolCall, ToolContent, ToolStatus, Usage, attach, diff,
    fmt_cost,
};
use crate::model::Status;

use super::ClientId;

/// What entries are read against: the session's directory, which the `@path`
/// mentions in prompts are relative to, and the key that signs image links.
/// No key for a session whose files are not on this machine: none are linked.
pub struct Scope {
    pub cwd: PathBuf,
    pub key: Option<String>,
}

/// Entries a `session` message carries; older ones come on request.
pub const TAIL: usize = 50;
/// Characters of tool output or thinking an entry carries at most.
const CLIP: usize = 1500;
const CLIP_COMMAND: usize = 300;
/// Lines of one diff a phone gets at most.
const DIFF_LINES: usize = 600;
/// Lines of unchanged text kept around each change in a diff.
const DIFF_CONTEXT: usize = 3;

pub fn status(s: Status) -> &'static str {
    match s {
        Status::Idle => "idle",
        Status::Working => "working",
        Status::NeedsInput => "needs_input",
        Status::Error => "error",
    }
}

/// Sent with the WebSocket upgrade by a server that acknowledges `seq`.
pub const ACKS_HEADER: &str = "x-arakun-acks";

/// The number a client gave its message, which the server acknowledges.
pub fn seq(msg: &Value) -> Option<u64> {
    msg.get("seq")?.as_u64()
}

pub fn ack(seq: u64) -> String {
    json!({ "type": "ack", "seq": seq }).to_string()
}

pub fn error(message: &str) -> String {
    json!({ "type": "error", "message": message }).to_string()
}

/// The answer to a page's `ping`, so it knows the computer is still there.
pub fn pong() -> String {
    json!({ "type": "pong" }).to_string()
}

/// The entries from `from` on, as the phone shows them.
pub fn entries(all: &[Arc<Entry>], from: usize, scope: &Scope) -> Vec<Value> {
    all.get(from..)
        .unwrap_or_default()
        .iter()
        .map(|e| entry(e, scope))
        .collect()
}

/// The agent's words with the images it names as markdown turned into signed
/// links, as a prompt's `@`-mentions already are. Only files that exist (and
/// are small enough to send) are rewritten; everything else stays as written.
fn agent_images(text: &str, scope: &Scope) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find("![") {
        let after = &rest[at + 2..];
        out.push_str(&rest[..at + 2]);
        rest = after;
        let Some(close) = after.find("](") else {
            continue;
        };
        let Some(end) = after[close + 2..].find(')') else {
            continue;
        };
        let alt = &after[..close];
        out.push_str(alt);
        out.push_str("](");
        match image_file(&after[close + 2..close + 2 + end], scope) {
            Some(link) => {
                out.push_str(&link);
                out.push(')');
                rest = &after[close + 2 + end + 1..];
            }
            None => rest = &after[close + 2..],
        }
    }
    out.push_str(rest);
    out
}

/// The image a markdown link names, when the file exists under the session's
/// directory (or is named absolutely) and the phone may be shown it.
fn image_file(path: &str, scope: &Scope) -> Option<String> {
    let path = path.trim();
    if path.starts_with("http") || path.starts_with("data:") {
        return None;
    }
    let file = if path.starts_with('/') {
        PathBuf::from(path)
    } else if path.starts_with('~') {
        return None;
    } else {
        scope.cwd.join(path)
    };
    attach::mime_type(&file)?;
    let len = std::fs::metadata(&file).ok().filter(|m| m.is_file())?.len();
    let key = scope.key.as_deref()?;
    (len <= attach::MAX_IMAGE_BYTES).then(|| super::image_link(key, &file))
}

/// The images a tool call returned that a phone may be shown, as
/// `{ name, url }` with signed links. None for a secret file's call, or
/// without a key.
fn tool_images(t: &ToolCall, scope: &Scope) -> Vec<Value> {
    let Some(key) = scope.key.as_deref().filter(|_| t.secret.is_none()) else {
        return Vec::new();
    };
    t.images
        .iter()
        .filter(|p| {
            std::fs::metadata(p).is_ok_and(|m| m.is_file() && m.len() <= attach::MAX_IMAGE_BYTES)
        })
        .map(|p| json!({ "name": file_name(p), "url": super::image_link(key, p) }))
        .collect()
}

/// The file of an image a session names (`link`, as written), when its agent
/// named it or a prompt mentioned it. Only those are sent, so asking cannot
/// read any file the server can.
pub fn named_image(all: &[Arc<Entry>], cwd: &Path, link: &str) -> Option<PathBuf> {
    let by_agent = all.iter().any(|e| match &**e {
        Entry::Agent { text } => crate::agent::markdown::image_files(text)
            .iter()
            .any(|l| l == link),
        _ => false,
    });
    if by_agent {
        return crate::agent::markdown::image_path(link, Some(cwd));
    }
    // A tool's image is named by its whole path, as the entry has it.
    let by_tool = all.iter().find_map(|e| match &**e {
        Entry::Tool(t) if t.secret.is_none() => {
            t.images.iter().find(|p| p.to_string_lossy() == link)
        }
        _ => None,
    });
    if let Some(file) = by_tool {
        return Some(file.clone());
    }
    let by_user = all.iter().any(|e| match &**e {
        Entry::User { text, .. } => attach::image_mentions(text).iter().any(|l| l == link),
        _ => false,
    });
    by_user
        .then(|| attach::mentioned_image(link, cwd))
        .flatten()
}

/// The answer to the app asking for an image: its bytes, or that there are
/// none to send. `at` is told back, so the app knows which question it answers.
/// It reads the file: call it off the thread that answers everyone else.
pub fn image_message(session: &str, link: &str, at: usize, file: Option<&Path>) -> Value {
    let image = file.and_then(|f| {
        let mime = attach::mime_type(f)?;
        let len = std::fs::metadata(f).ok().filter(|m| m.is_file())?.len();
        (len <= attach::MAX_IMAGE_BYTES).then_some(())?;
        Some((mime, std::fs::read(f).ok()?))
    });
    let mut msg = json!({ "type": "image", "id": session, "path": link, "at": at });
    match image {
        Some((mime, bytes)) => {
            use base64::Engine as _;
            msg["mime"] = json!(mime);
            msg["data"] = json!(base64::engine::general_purpose::STANDARD.encode(bytes));
        }
        None => msg["missing"] = json!(true),
    }
    msg
}

pub fn permission(p: &PermissionRequest) -> Value {
    json!({
        "tool_call_id": p.tool_call_id,
        "title": p.title,
        "options": p.options.iter().map(|o| json!({ "id": o.id, "name": o.name, "kind": o.kind })).collect::<Vec<_>>(),
        // The secret file it would read, and why that matters.
        "secret": p.secret,
        "why": p.secret.as_deref().map(crate::agent::secret_reads::why),
    })
}

pub fn question(q: &AskUserRequest) -> Value {
    json!({
        "tool_call_id": q.tool_call_id,
        "questions": q.questions.iter().map(|q| json!({
            "index": q.index, "topic": q.topic, "question": q.question, "options": q.options, "multi": q.multi,
            "secret": q.secret,
        })).collect::<Vec<_>>(),
    })
}

/// The secret card a session shows (`request_secret`), or null. `private`
/// says whether this connection is encrypted; with the thread's own
/// connection to its server, it decides whether the device may send what is
/// typed (`agent::secret::answer`).
pub fn secret(t: &Thread, private: bool) -> Value {
    t.pending_secret()
        .map_or(Value::Null, |r| r.to_wire(private && t.secret_private()))
}

pub fn usage(u: &Usage) -> Value {
    json!({
        "summary": u.summary(),
        "context_percent": u.context_percent(),
        "tokens": u.total_tokens(),
    })
}

/// The type and the text of an entry that is only words, as [`entry`] sends it.
fn words(e: &Entry, scope: &Scope) -> Option<(&'static str, String)> {
    match e {
        Entry::Agent { text } => Some(("agent", agent_images(text, scope))),
        Entry::Thought { text } => Some(("thought", tail(text, CLIP))),
        _ => None,
    }
}

pub fn entry(e: &Entry, scope: &Scope) -> Value {
    match e {
        Entry::User {
            text,
            at,
            elapsed_ms,
            tokens,
            shell_changes,
            ..
        } => {
            // As the desktop shows it: the typed words, with the pasted blocks
            // and the mentioned images set apart.
            let (typed, pastes) = attach::split_pastes(text);
            // Without a key the mentions stay in the words, as typed.
            let (typed, images) = match &scope.key {
                Some(key) => {
                    let (typed, images) = attach::split_images(&typed, &scope.cwd);
                    let images: Vec<_> = images
                        .iter()
                        .map(|p| json!({ "name": file_name(p), "url": super::image_link(key, p) }))
                        .collect();
                    (typed, images)
                }
                None => (typed, Vec::new()),
            };
            json!({
                "type": "user", "text": typed, "at": at, "elapsed_ms": elapsed_ms, "tokens": tokens,
                "pastes": pastes.iter().map(|p| json!({ "lines": p.lines().count(), "text": head(p, CLIP) })).collect::<Vec<_>>(),
                "images": images,
                // What the turn's shell commands changed, for the turn's header.
                "edits": shell_changes.iter().map(|c| json!({ "path": c.path, "added": c.added, "removed": c.removed })).collect::<Vec<_>>(),
            })
        }
        Entry::Agent { .. } | Entry::Thought { .. } => {
            let (kind, text) = words(e, scope).unwrap_or_default();
            json!({ "type": kind, "text": text })
        }
        Entry::Tool(t) => {
            let mut texts = Vec::new();
            let mut diffs = Vec::new();
            for c in &t.content {
                match c {
                    ToolContent::Text { text } => texts.push(text.as_str()),
                    ToolContent::Diff {
                        path,
                        old_text,
                        new_text,
                    } => {
                        let (added, removed) = diff_counts(old_text.as_deref(), new_text);
                        diffs.push(json!({ "path": path, "added": added, "removed": removed }));
                    }
                    ToolContent::Terminal { .. } => {}
                }
            }
            // A secret file's content stays on the computer, shown only there.
            let output = if t.secret.is_some() {
                None
            } else if !t.terminal_output.is_empty() {
                Some(tail(&t.terminal_output, CLIP))
            } else if !texts.is_empty() {
                Some(tail(&texts.join("\n"), CLIP))
            } else if !t.images.is_empty() {
                // The raw output of an image is its bytes as base64.
                None
            } else {
                t.raw_output.as_deref().map(|o| tail(o, CLIP))
            };
            let mut v = json!({
                "type": "tool",
                "id": t.id,
                "title": t.title,
                "kind": t.kind,
                "status": t.status,
                "command": command(t),
                "output": output,
                "exit_code": t.exit_code,
                "added": t.added,
                "removed": t.removed,
                "diffs": diffs,
                "locations": t.locations.iter().map(|p| file_name(p)).collect::<Vec<_>>(),
                "secret": t.secret,
            });
            // The subagent it ran under, so a phone can group it there.
            if let Some(parent) = &t.parent {
                v["parent"] = json!(parent);
            }
            // What the tool returned as pictures, linked as a prompt's are.
            // Only when there are some: an older phone reads no such field.
            let images = tool_images(t, scope);
            if !images.is_empty() {
                v["images"] = json!(images);
            }
            v
        }
        Entry::Plan { entries } => json!({
            "type": "plan",
            "entries": entries.iter().map(|p| json!({ "content": p.content, "status": p.status })).collect::<Vec<_>>(),
        }),
        Entry::Notice {
            text, error, mark, ..
        } => json!({
            "type": "notice", "text": text, "error": error, "mark": mark,
        }),
        Entry::Compacted {
            removed,
            before,
            after,
        } => json!({
            "type": "compacted", "removed": removed, "before": before, "after": after,
        }),
    }
}

/// A file one turn changed, and where its diffs are: `(entry, index)` is the
/// diff's place in a tool's content, or a shell change's in the turn's user entry.
struct FileChange<'a> {
    path: &'a Path,
    added: u32,
    removed: u32,
    diffs: Vec<(usize, usize)>,
}

struct TurnChanges<'a> {
    start: usize,
    text: &'a str,
    at: u64,
    files: Vec<FileChange<'a>>,
}

/// The turns that changed files, oldest first, with each file in first-touched order.
fn turns_with_changes(all: &[Arc<Entry>]) -> Vec<TurnChanges<'_>> {
    fn add<'a>(
        turn: &mut TurnChanges<'a>,
        path: &'a Path,
        old: Option<&str>,
        new: &str,
        at: (usize, usize),
    ) {
        let (added, removed) = diff::stats(&diff::line_diff(old.unwrap_or(""), new));
        match turn.files.iter_mut().find(|f| f.path == path) {
            Some(f) => {
                f.added += added;
                f.removed += removed;
                f.diffs.push(at);
            }
            None => turn.files.push(FileChange {
                path,
                added,
                removed,
                diffs: vec![at],
            }),
        }
    }
    let mut turns: Vec<TurnChanges> = Vec::new();
    let mut current = TurnChanges {
        start: 0,
        text: "",
        at: 0,
        files: Vec::new(),
    };
    for (i, e) in all.iter().enumerate() {
        match &**e {
            Entry::User {
                text,
                at,
                shell_changes,
                ..
            } => {
                let done = std::mem::replace(
                    &mut current,
                    TurnChanges {
                        start: i,
                        text,
                        at: *at,
                        files: Vec::new(),
                    },
                );
                if !done.files.is_empty() {
                    turns.push(done);
                }
                for (k, c) in shell_changes.iter().enumerate() {
                    add(
                        &mut current,
                        c.path.as_path(),
                        c.old.as_deref(),
                        &c.new,
                        (i, k),
                    );
                }
            }
            Entry::Tool(t) if t.status == ToolStatus::Completed => {
                for (k, c) in t.content.iter().enumerate() {
                    if let ToolContent::Diff {
                        path,
                        old_text,
                        new_text,
                    } = c
                    {
                        add(
                            &mut current,
                            path.as_path(),
                            old_text.as_deref(),
                            new_text,
                            (i, k),
                        );
                    }
                }
            }
            _ => {}
        }
    }
    if !current.files.is_empty() {
        turns.push(current);
    }
    turns
}

/// How many files a session changed and the lines added and removed in all,
/// for the chip that opens the changes.
pub fn change_totals(all: &[Arc<Entry>]) -> Value {
    let mut files: Vec<&Path> = Vec::new();
    let (mut added, mut removed) = (0u32, 0u32);
    for e in all {
        match &**e {
            Entry::Tool(t) if t.status == ToolStatus::Completed => {
                let mut any = false;
                for c in &t.content {
                    if let ToolContent::Diff { path, .. } = c {
                        any = true;
                        if !files.contains(&path.as_path()) {
                            files.push(path);
                        }
                    }
                }
                if any {
                    added += t.added;
                    removed += t.removed;
                }
            }
            Entry::User { shell_changes, .. } => {
                for c in shell_changes {
                    if !files.contains(&c.path.as_path()) {
                        files.push(&c.path);
                    }
                    added += c.added;
                    removed += c.removed;
                }
            }
            _ => {}
        }
    }
    json!({ "files": files.len(), "added": added, "removed": removed })
}

/// Every file a session changed, turn by turn with the newest first; the
/// phone asks for a diff by its `(entry, index)`.
pub fn changes(all: &[Arc<Entry>], cwd: &Path) -> Value {
    let turns: Vec<Value> = turns_with_changes(all)
        .iter()
        .rev()
        .map(|t| {
            let (typed, _) = attach::split_pastes(t.text);
            let (typed, _) = attach::split_images(&typed, cwd);
            json!({
                "entry": t.start,
                "text": head(typed.lines().next().unwrap_or(""), 80),
                "at": t.at,
                "files": t.files.iter().map(|f| json!({
                    "path": rel_path(f.path, cwd), "name": file_name(f.path),
                    "added": f.added, "removed": f.removed,
                    "diffs": f.diffs.iter().map(|(e, i)| json!([e, i])).collect::<Vec<_>>(),
                })).collect::<Vec<_>>(),
            })
        })
        .collect();
    json!({ "type": "changes", "turns": turns })
}

/// One diff as the phone draws it: the changed lines with a little context,
/// each as `[mark, line number, text]`, every hunk opened by its `@@` header string.
pub fn diff_message(all: &[Arc<Entry>], entry: usize, index: usize, cwd: &Path) -> Option<Value> {
    let (path, old, new) = match &**all.get(entry)? {
        Entry::Tool(t) => match t.content.get(index)? {
            ToolContent::Diff {
                path,
                old_text,
                new_text,
            } => (path.as_path(), old_text.as_deref(), new_text.as_str()),
            _ => return None,
        },
        Entry::User { shell_changes, .. } => {
            let c = shell_changes.get(index)?;
            (c.path.as_path(), c.old.as_deref(), c.new.as_str())
        }
        _ => return None,
    };
    let lines = diff::line_diff(old.unwrap_or(""), new);
    let (added, removed) = diff::stats(&lines);
    // Each hunk opens with its `@@` header, as a string among the line triples.
    let mut shown: Vec<Value> = Vec::new();
    let mut total = 0usize;
    for range in diff::hunk_ranges(&lines, DIFF_CONTEXT) {
        total += 1 + range.len();
        if shown.len() >= DIFF_LINES {
            continue;
        }
        shown.push(Value::String(diff::hunk_header(&lines, range.clone(), new)));
        for l in &lines[range] {
            if shown.len() >= DIFF_LINES {
                break;
            }
            let mark = match l.kind {
                diff::LineKind::Add => "+",
                diff::LineKind::Del => "-",
                diff::LineKind::Context => " ",
            };
            shown.push(json!([mark, l.number, l.text]));
        }
    }
    Some(json!({
        "type": "diff", "entry": entry, "index": index,
        "path": rel_path(path, cwd), "name": file_name(path),
        "added": added, "removed": removed,
        "new_file": old.is_none(), "deleted": new.is_empty() && old.is_some(),
        "lines": shown, "cut": total.saturating_sub(shown.len()),
    }))
}

fn rel_path(path: &Path, cwd: &Path) -> String {
    path.strip_prefix(cwd)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/")
}

/// What the desktop knows of Droid's account, for `plan_usage`.
pub struct DroidPlan<'a> {
    pub connected: bool,
    pub limits: Option<&'a Limits>,
    pub error: Option<&'a str>,
    /// Unix time of the last successful read.
    pub fetched_at: Option<u64>,
}

/// Both agents' plan limits as the desktop's Usage tab shows them: a card per
/// agent with a row per window that is running (`label`, `percent`, `detail`),
/// the windows not running folded into `idle`, and a `note` under it all.
///
/// `access` names the API key or endpoint Claude Code runs on without a plan.
pub fn plan_usage(
    plan: Option<&PlanUsage>,
    access: Option<&str>,
    droid: &DroidPlan,
    now: u64,
) -> Value {
    json!({ "type": "usage", "agents": [claude_card(plan, access, now), droid_card(droid, now)] })
}

fn limit_row(label: &str, percent: f32, detail: Option<String>) -> Value {
    json!({ "label": label, "percent": percent.clamp(0., 100.).ceil() as u64, "detail": detail })
}

fn claude_card(plan: Option<&PlanUsage>, access: Option<&str>, now: u64) -> Value {
    if let (None, Some(access)) = (plan, access) {
        return json!({
            "agent": "claude", "name": "Claude Code", "signed_in": true, "plan": access,
            "rows": [], "idle": [], "tone": "normal",
            "note": "Claude Code runs on an API key or another endpoint, which has no plan limits.",
        });
    }
    let Some(plan) = plan else {
        return json!({
            "agent": "claude", "name": "Claude Code", "signed_in": false,
            "note": "Sign in to Claude Code on the computer and its limits show up here.",
        });
    };
    let mut rows = Vec::new();
    let mut idle = Vec::new();
    for w in &plan.windows {
        if w.active || w.percent > 0 {
            rows.push(limit_row(&w.label, w.percent as f32, w.reset_text(now)));
        } else {
            idle.push(w.label.clone());
        }
    }
    if let Some(s) = &plan.spend
        && (s.used > 0. || s.limit.is_some())
    {
        let detail = match s.limit {
            Some(limit) => format!(
                "{} of {}",
                fmt_cost(s.used, &s.currency, false),
                fmt_cost(limit, &s.currency, false)
            ),
            None => format!("{} this month", fmt_cost(s.used, &s.currency, false)),
        };
        rows.push(limit_row("Extra usage", s.percent as f32, Some(detail)));
    }
    json!({
        "agent": "claude", "name": "Claude Code", "signed_in": true, "plan": plan.plan_label(),
        "rows": rows, "idle": idle, "note": null, "tone": "normal", "at": plan.at,
    })
}

fn droid_card(d: &DroidPlan, now: u64) -> Value {
    if !d.connected {
        return json!({
            "agent": "droid", "name": "Droid", "signed_in": false,
            "note": "Sign in to droid or connect a Factory API key on the computer and its limits show up here.",
        });
    }
    let Some(limits) = d.limits else {
        return json!({
            "agent": "droid", "name": "Droid", "signed_in": true, "plan": "", "rows": [], "idle": [],
            "note": d.error.unwrap_or("Reading usage…"), "tone": if d.error.is_some() { "danger" } else { "normal" }, "at": d.fetched_at,
        });
    };
    let mode = limits.mode(now);
    let core_first = mode == UsageMode::DroidCore && limits.core.is_some();
    let (main, other) = if core_first {
        (
            limits.core.as_ref().map(|p| ("Core", p)),
            limits.standard.as_ref().map(|p| ("Standard", p)),
        )
    } else {
        (
            limits.standard.as_ref().map(|p| ("Standard", p)),
            limits.core.as_ref().map(|p| ("Core", p)),
        )
    };
    let reset = |b: &account::Bucket| {
        b.window_end
            .filter(|end| *end > now)
            .map(|end| format!("resets {}", account::when(end, now)))
    };
    let mut rows = Vec::new();
    let mut idle = Vec::new();
    if let Some((name, pool)) = main {
        for (label, b) in pool.buckets() {
            if b.active(now) {
                rows.push(limit_row(
                    &format!("{name} · {label}"),
                    b.current(now),
                    reset(&b),
                ));
            } else {
                idle.push(format!("{name} · {label}"));
            }
        }
    }
    // The pool not in use: its fullest window, in one row.
    if let Some((name, pool)) = other
        && let Some((label, b)) = pool
            .buckets()
            .into_iter()
            .filter(|(_, b)| b.active(now))
            .max_by(|a, b| a.1.used_percent.total_cmp(&b.1.used_percent))
    {
        rows.push(limit_row(
            &format!("{name} · {label}"),
            b.current(now),
            reset(&b),
        ));
    }
    let plan = match mode {
        UsageMode::Standard => "Standard",
        UsageMode::DroidCore => "Droid Core",
        UsageMode::ExtraUsage => "Extra usage",
        UsageMode::Blocked => "Limit reached",
    };
    let tone = match mode {
        UsageMode::Blocked => "danger",
        UsageMode::Standard => "normal",
        _ => "warn",
    };
    let balance = (limits.extra_usage_cents > 0)
        .then(|| format!("${:.2}", limits.extra_usage_cents as f64 / 100.));
    json!({
        "agent": "droid", "name": "Droid", "signed_in": true, "plan": plan, "rows": rows, "idle": idle,
        "balance": balance, "note": limits.summary(now), "tone": tone, "at": d.fetched_at,
    })
}

/// The last `max` characters, marked when something was cut.
fn tail(text: &str, max: usize) -> String {
    let n = text.chars().count();
    if n <= max {
        return text.to_string();
    }
    let skip = n - max;
    let cut: String = text.chars().skip(skip).collect();
    format!("… [{skip} chars] {cut}")
}

fn head(text: &str, max: usize) -> String {
    match text.char_indices().nth(max) {
        Some((i, _)) => format!("{}…", &text[..i]),
        None => text.to_string(),
    }
}

/// The shell command a tool ran, when its input says.
fn command(t: &crate::agent::ToolCall) -> Option<String> {
    let input = t.raw_input.as_ref()?;
    let raw = match input.get("command").or_else(|| input.get("cmd"))? {
        Value::String(s) => s.clone(),
        Value::Array(parts) => parts
            .iter()
            .filter_map(Value::as_str)
            .collect::<Vec<_>>()
            .join(" "),
        _ => return None,
    };
    Some(head(raw.trim(), CLIP_COMMAND))
}

fn file_name(p: &std::path::Path) -> String {
    p.file_name().map_or_else(
        || p.display().to_string(),
        |n| n.to_string_lossy().into_owned(),
    )
}

/// Lines a diff adds and removes, by counting lines; a new file is all adds.
fn diff_counts(old: Option<&str>, new: &str) -> (usize, usize) {
    let new_lines = new.lines().count();
    match old {
        None | Some("") => (new_lines, 0),
        Some(old) => {
            let old_lines = old.lines().count();
            if new_lines >= old_lines {
                (new_lines - old_lines, 0)
            } else {
                (0, old_lines - new_lines)
            }
        }
    }
}

// ---- whole messages --------------------------------------------------------
// Built here so the app and `arakun server` send the very same ones.

/// `entries`: a watched session's entries from `from` on, and how many it has now.
pub fn entries_message(id: &str, all: &[Arc<Entry>], from: usize, scope: &Scope) -> String {
    json!({
        "type": "entries", "id": id, "from": from.min(all.len()), "count": all.len(),
        "entries": entries(all, from, scope),
    })
    .to_string()
}

/// The last entry of a session as its watchers last got it, when it is only
/// words: the next flush may send just what they grew by.
#[derive(Debug, Clone, PartialEq)]
struct Streamed {
    n: usize,
    count: usize,
    kind: &'static str,
    text: String,
}

/// Sends a session's changed entries to the clients watching it. A reply
/// streams in as one entry whose words grow every flush; sending that entry
/// whole each time costs the square of its length. A client that opened the
/// session with `append` gets only the new words instead; the rest still get
/// `entries`, as before.
#[derive(Default)]
pub struct EntryStream {
    appenders: HashSet<ClientId>,
    last: HashMap<String, Streamed>,
}

/// What one flush of a session sends: an `append` when it can, the `entries`
/// message (built once, when a client needs it) otherwise.
pub struct EntryFlush<'a> {
    id: &'a str,
    all: &'a [Arc<Entry>],
    from: usize,
    scope: &'a Scope,
    /// `Some(None)`: the words are as they were, so an appender gets nothing.
    append: Option<Option<String>>,
    entries: OnceCell<String>,
}

impl EntryStream {
    /// `client` watches `session` now, taking `append` or not. The next flush
    /// of it goes whole: what the client was sent on opening may be newer than
    /// what the others last got.
    pub fn opened(&mut self, client: ClientId, session: &str, append: bool) {
        if append {
            self.appenders.insert(client);
        } else {
            self.appenders.remove(&client);
        }
        self.last.remove(session);
    }

    pub fn forget_client(&mut self, client: ClientId) {
        self.appenders.remove(&client);
    }

    /// Nobody watches `session` any more.
    pub fn forget_session(&mut self, session: &str) {
        self.last.remove(session);
    }

    /// The entries of session `id` changed from `from` on. Call it once per
    /// flush, whoever watches: it notes what they now have.
    pub fn flush<'a>(
        &mut self,
        id: &'a str,
        all: &'a [Arc<Entry>],
        from: usize,
        scope: &'a Scope,
    ) -> EntryFlush<'a> {
        let before = self.last.remove(id);
        let now = all.last().and_then(|e| {
            let (kind, text) = words(e, scope)?;
            Some(Streamed {
                n: all.len() - 1,
                count: all.len(),
                kind,
                text,
            })
        });
        let append = match (&before, &now) {
            (Some(b), Some(n))
                if from == n.n && b.n == n.n && b.count == n.count && b.kind == n.kind =>
            {
                Some(append_message(id, n.n, &b.text, &n.text))
            }
            _ => None,
        };
        if let Some(now) = now {
            self.last.insert(id.to_string(), now);
        }
        EntryFlush {
            id,
            all,
            from,
            scope,
            append,
            entries: OnceCell::new(),
        }
    }

    /// What `client` gets of `flush`, if anything.
    pub fn message_for(&self, client: ClientId, flush: &EntryFlush) -> Option<String> {
        if self.appenders.contains(&client)
            && let Some(append) = &flush.append
        {
            return append.clone();
        }
        let entries = flush
            .entries
            .get_or_init(|| entries_message(flush.id, flush.all, flush.from, flush.scope));
        Some(entries.clone())
    }
}

/// `append` for entry `n`, whose words went from `old` to `new`; `None` when
/// they are the same.
fn append_message(id: &str, n: usize, old: &str, new: &str) -> Option<String> {
    let same = old
        .char_indices()
        .zip(new.chars())
        .find(|((_, a), b)| a != b)
        .map_or(old.len().min(new.len()), |((i, _), _)| i);
    if same == old.len() && same == new.len() {
        return None;
    }
    // UTF-16 units: what JavaScript, Kotlin and Swift strings count with ease.
    let at = new[..same].encode_utf16().count();
    Some(json!({ "type": "append", "id": id, "n": n, "at": at, "text": &new[same..] }).to_string())
}

/// `state`: what changed around a watched session's entries. `private`
/// says whether the connection it goes over is encrypted.
pub fn state_message(id: &str, t: &Thread, status: Status, private: bool) -> String {
    json!({
        "type": "state", "id": id, "status": self::status(status),
        "permission": t.pending_permission().map(permission),
        "question": t.pending_question().map(question),
        "secret": secret(t, private),
        "usage": usage(t.usage()),
        "elapsed_ms": t.turn_elapsed().map(|d| d.as_millis() as u64),
        "title": t.title(),
        "changes": change_totals(t.entries()),
        "queue": t.queued(),
        "queue_paused": t.queue_paused(),
        "sending_now": t.sending_now(),
        "sign_in": sign_in(t),
    })
    .to_string()
}

/// `sign_in`: the line that signs the session's agent in, in a terminal on
/// this computer, while the agent says it is not signed in. The app offers to
/// run it in a terminal here; phones ignore it. Null otherwise, and when this
/// computer has no CLI of the agent to sign in with.
fn sign_in(t: &Thread) -> Option<String> {
    t.needs_login()
        .then(|| crate::agent::login_shell_line(t.kind()))
        .flatten()
}

/// The longest description a slash command carries: the popup shows one line.
const CLIP_COMMAND_DETAIL: usize = 160;

/// `commands`: the slash commands a watched session's agent offers, for the
/// composer of an app that shows the session. Phones ignore it.
pub fn commands_message(id: &str, commands: &[Choice]) -> String {
    let list: Vec<Value> = commands
        .iter()
        .map(|c| {
            json!({
                "name": c.name,
                "description": c.description.as_deref().map(|d| head(d, CLIP_COMMAND_DETAIL)),
            })
        })
        .collect();
    json!({ "type": "commands", "id": id, "commands": list }).to_string()
}

/// The commands of a [`commands_message`].
pub fn read_commands(v: &Value) -> Vec<Choice> {
    v.as_array()
        .into_iter()
        .flatten()
        .filter_map(|c| {
            let name = c["name"].as_str().filter(|n| !n.is_empty())?.to_string();
            Some(Choice {
                id: name.clone(),
                name,
                description: c["description"].as_str().map(str::to_string),
            })
        })
        .collect()
}

/// What the list a session is in says about it, for a `session` message.
pub struct SessionHead<'a> {
    pub id: &'a str,
    pub title: &'a str,
    /// The agent's name, as shown.
    pub agent: &'a str,
    pub status: Status,
    pub place: &'a str,
}

fn session_head(head: &SessionHead, all: &[Arc<Entry>], scope: &Scope) -> Value {
    let from = all.len().saturating_sub(TAIL);
    json!({
        "type": "session", "id": head.id, "title": head.title, "agent": head.agent,
        "status": status(head.status), "place": head.place,
        "permission": null, "question": null, "secret": null, "usage": null, "elapsed_ms": null,
        "count": all.len(), "from": from, "entries": entries(all, from, scope),
        "changes": change_totals(all),
    })
}

/// `session`: a session that is running, as a phone opens it. `status` is the
/// thread's own, which is fresher than the list's. `private` as for
/// [`state_message`].
pub fn session_live(
    head: &SessionHead,
    t: &Thread,
    status: Status,
    scope: &Scope,
    private: bool,
) -> String {
    let mut msg = session_head(head, t.entries(), scope);
    msg["permission"] = t.pending_permission().map_or(Value::Null, permission);
    msg["question"] = t.pending_question().map_or(Value::Null, question);
    msg["secret"] = secret(t, private);
    msg["usage"] = usage(t.usage());
    msg["elapsed_ms"] = json!(t.turn_elapsed().map(|d| d.as_millis() as u64));
    msg["status"] = json!(self::status(status));
    msg["queue"] = json!(t.queued());
    msg["queue_paused"] = json!(t.queue_paused());
    msg["sending_now"] = json!(t.sending_now());
    msg["sign_in"] = json!(sign_in(t));
    if !t.title().is_empty() {
        msg["title"] = json!(t.title());
    }
    msg.to_string()
}

/// `session`: one that is not running, from its transcript's entries and
/// the follow-ups that waited in it. Its thread holds those paused once it
/// is back, so they show paused.
pub fn session_saved(
    head: &SessionHead,
    all: &[Arc<Entry>],
    queue: &[String],
    scope: &Scope,
) -> String {
    let mut msg = session_head(head, all, scope);
    msg["queue"] = json!(queue);
    msg["queue_paused"] = json!(!queue.is_empty());
    msg["sending_now"] = json!(0);
    msg.to_string()
}

/// What a waiting session asks, for its line in the inbox.
pub fn asks(t: &Thread) -> Option<Value> {
    t.pending_permission()
        .map(|p| json!({ "kind": "permission", "text": p.title }))
        .or_else(|| {
            let q = t.pending_question()?;
            Some(json!({ "kind": "question", "text": q.questions.first()?.question }))
        })
        .or_else(|| {
            let r = t.pending_secret()?;
            Some(json!({ "kind": "secret", "text": r.label }))
        })
}

/// A session's line in the inbox.
pub struct InboxSession<'a> {
    pub id: &'a str,
    pub title: &'a str,
    /// The agent's name, as shown.
    pub agent: &'a str,
    pub status: Status,
    pub updated: u64,
    pub added: u32,
    pub removed: u32,
    pub project: &'a str,
    pub env: &'a str,
    /// The folder it runs in: the same path as its place's, so a device can
    /// put the session under that place.
    pub place: &'a Path,
    pub pr: Option<&'a str>,
    pub asks: Option<Value>,
}

/// Hashes what the line shows, so a caller can tell the inbox changed
/// without building it. Every field is named: a new one must be added here.
impl std::hash::Hash for InboxSession<'_> {
    fn hash<H: std::hash::Hasher>(&self, h: &mut H) {
        let Self {
            id,
            title,
            agent,
            status: st,
            updated,
            added,
            removed,
            project,
            env,
            place,
            pr,
            asks,
        } = self;
        (id, title, agent, status(*st), updated, added, removed).hash(h);
        (project, env, place, pr).hash(h);
        asks.as_ref().map(Value::to_string).hash(h);
    }
}

pub fn inbox_session(s: InboxSession) -> Value {
    json!({
        "id": s.id, "title": s.title, "agent": s.agent, "status": status(s.status),
        "updated": s.updated, "added": s.added, "removed": s.removed,
        "project": s.project, "env": s.env, "place": s.place, "pr": s.pr, "asks": s.asks,
    })
}

/// Where a session can start: the folder, the repository it is a checkout
/// of, and the agent that place picks (its key).
pub struct InboxPlace<'a> {
    pub path: &'a Path,
    pub project: &'a str,
    pub env: &'a str,
    pub branch: Option<&'a str>,
    pub agent: &'a str,
    /// The repository's main folder, shared by all its worktrees.
    pub repo: Option<&'a Path>,
    /// Its `origin`, by which the app finds the same project on its side.
    pub origin: Option<&'a str>,
}

/// Hashes what the place shows, as for [`InboxSession`].
impl std::hash::Hash for InboxPlace<'_> {
    fn hash<H: std::hash::Hasher>(&self, h: &mut H) {
        let Self {
            path,
            project,
            env,
            branch,
            agent,
            repo,
            origin,
        } = self;
        (path, project, env, branch, agent, repo, origin).hash(h);
    }
}

pub fn inbox_place(p: InboxPlace) -> Value {
    json!({
        "path": p.path, "project": p.project, "env": p.env, "branch": p.branch,
        "agent": p.agent, "repo": p.repo, "origin": p.origin,
    })
}

/// `inbox`: every session, newest first, and the places.
pub fn inbox(host: &str, sessions: Vec<Value>, places: Vec<Value>) -> String {
    inbox_value(host, sessions, places).to_string()
}

/// A server's `inbox`. It also says the server sends a device several
/// sessions at once (`open` with `keep`): an app follows several there only
/// then, since a server from before sends the last one opened alone.
pub fn server_inbox(host: &str, sessions: Vec<Value>, places: Vec<Value>) -> String {
    let mut inbox = inbox_value(host, sessions, places);
    inbox["watch_several"] = json!(true);
    inbox.to_string()
}

fn inbox_value(host: &str, mut sessions: Vec<Value>, places: Vec<Value>) -> Value {
    sessions.sort_by_key(|s| std::cmp::Reverse(s["updated"].as_u64().unwrap_or(0)));
    json!({ "type": "inbox", "host": host, "sessions": sessions, "places": places })
}

/// Settings with their value and choices, as the phone shows them.
pub fn settings(list: Vec<(Setting, (String, Vec<SettingOption>))>) -> Value {
    let list = list.into_iter().map(|(s, (current, options))| {
        json!({
            "key": s.key(),
            "label": s.label(),
            "current": current,
            "options": options.iter().map(|o| json!({ "id": o.id, "name": o.name, "detail": o.detail, "disabled": o.disabled })).collect::<Vec<_>>(),
        })
    });
    Value::Array(list.collect())
}

/// The settings a server sends as [`settings`] writes them; one it does not
/// know of here is left out.
pub fn read_settings(v: &Value) -> Vec<(Setting, (String, Vec<SettingOption>))> {
    let str_of = |v: &Value| v.as_str().map(str::to_string);
    v.as_array()
        .into_iter()
        .flatten()
        .filter_map(|set| {
            let s = Setting::from_key(set["key"].as_str()?)?;
            let options = set["options"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|o| {
                    Some(SettingOption {
                        id: str_of(&o["id"])?,
                        name: str_of(&o["name"]).unwrap_or_default(),
                        detail: str_of(&o["detail"]),
                        description: None,
                        disabled: str_of(&o["disabled"]),
                    })
                })
                .collect();
            Some((s, (str_of(&set["current"]).unwrap_or_default(), options)))
        })
        .collect()
}

/// The most text one message carries. The relay passes frames of 256 KiB at
/// most, and sealing adds to what goes in one.
pub const FRAME_TEXT: usize = 180 * 1024;
/// What a `part` carries of the message it is a piece of, before base64.
const PART_BYTES: usize = 120 * 1024;
/// About how much one `transcript` answer holds; the rest comes when asked for.
const TRANSCRIPT_BYTES: usize = 96 * 1024;
/// The most pieces, and bytes, one message may come in: a peer that says more
/// is not believed.
const MAX_PARTS: usize = 4096;
const MAX_WHOLE: usize = 256 * 1024 * 1024;

/// `transcript`: a session's entries from `from` on, whole, as its transcript
/// file holds them. A phone gets them cut down (`entries`); the app, looking at
/// a server, shows the session as one of its own and needs all of each. The
/// answer stops near [`TRANSCRIPT_BYTES`] (`to` says where): the asker comes
/// back for the rest. What goes out is this message, in `part`s when one entry
/// alone is too much for a frame.
pub fn transcript_message(
    id: &str,
    agent: &str,
    cwd: &Path,
    all: &[Arc<Entry>],
    from: usize,
) -> Vec<String> {
    let from = from.min(all.len());
    let mut entries: Vec<Value> = Vec::new();
    let mut size = 0;
    for e in &all[from..] {
        let v = serde_json::to_value(e).unwrap_or(Value::Null);
        let len = v.to_string().len();
        if !entries.is_empty() && size + len > TRANSCRIPT_BYTES {
            break;
        }
        size += len;
        entries.push(v);
    }
    let msg = json!({
        "type": "transcript", "id": id, "agent": agent, "cwd": cwd,
        "from": from, "to": from + entries.len(), "count": all.len(), "entries": entries,
    });
    parts(msg.to_string())
}

/// `subagents`: the subagents a session ran as child sessions, each with its
/// whole transcript. Those live in the server's thread alone, not among the
/// session's entries, so the app following the session gets them here. Sent
/// whole, in `part`s when it is too much for a frame.
pub fn subagents_message(id: &str, subagents: &[SubagentRecord]) -> Vec<String> {
    parts(json!({ "type": "subagents", "id": id, "subagents": subagents }).to_string())
}

/// The subagents of a `subagents` message, or why they could not be read.
/// A phone's `brief` list has no transcripts, so it is not read as one.
pub fn read_subagents(msg: &Value) -> Result<Vec<SubagentRecord>, String> {
    if msg["brief"].as_bool() == Some(true) {
        return Err("a brief list has no transcripts".into());
    }
    serde_json::from_value(msg["subagents"].clone()).map_err(|e| e.to_string())
}

/// A subagent as a phone lists it: what it is, not what it said.
pub struct SubagentHead<'a> {
    pub id: &'a str,
    pub name: &'a str,
    pub task: &'a str,
    pub state: SubagentState,
    pub started: u64,
    pub ended: Option<u64>,
    /// The tool call in the session that started it.
    pub call: Option<&'a str>,
    /// The subagent that started it, when another one did.
    pub host: Option<&'a str>,
}

impl<'a> From<&'a Subagent> for SubagentHead<'a> {
    fn from(s: &'a Subagent) -> Self {
        SubagentHead {
            id: &s.id,
            name: &s.name,
            task: &s.task,
            state: s.state,
            started: s.started,
            ended: s.ended,
            // A mirrored one is its Task call.
            call: s.call.as_deref().or(s.mirrored.then_some(s.id.as_str())),
            host: s.host(),
        }
    }
}

impl<'a> From<&'a SubagentRecord> for SubagentHead<'a> {
    fn from(r: &'a SubagentRecord) -> Self {
        SubagentHead {
            id: &r.id,
            name: &r.name,
            task: &r.task,
            state: r.state,
            started: r.started,
            ended: r.ended,
            call: r.call.as_deref(),
            host: r.host.as_deref(),
        }
    }
}

/// `subagents` as a phone gets it: each one `brief`, without its transcript,
/// which it asks for when it opens one. A phone takes no `part`s, so when the
/// list is too long for a frame the oldest go first.
pub fn brief_subagents_message(id: &str, subagents: &[SubagentHead]) -> String {
    let list: Vec<Value> = subagents
        .iter()
        .map(|s| {
            json!({
                "id": s.id, "name": s.name, "task": head(s.task, CLIP_TASK), "state": s.state,
                "started": s.started, "ended": s.ended, "call": s.call, "host": s.host,
            })
        })
        .collect();
    let mut skip = 0;
    loop {
        let msg =
            json!({ "type": "subagents", "id": id, "brief": true, "subagents": &list[skip..] })
                .to_string();
        if msg.len() <= FRAME_TEXT || skip == list.len() {
            return msg;
        }
        skip += 1;
    }
}

/// The longest task a brief subagent carries: the list shows a line or two.
const CLIP_TASK: usize = 300;

/// `subagent_entries`: subagent `subagent`'s entries `from..to` (to the end
/// without `to`), and how many it has. Only a page asked for has a `to`.
pub fn subagent_entries_message(
    id: &str,
    subagent: &str,
    all: &[Arc<Entry>],
    from: usize,
    to: Option<usize>,
    scope: &Scope,
) -> String {
    let asked = to;
    let to = to.unwrap_or(all.len()).min(all.len());
    let from = from.min(to);
    let entries: Vec<Value> = all[from..to].iter().map(|e| entry(e, scope)).collect();
    let mut msg = json!({
        "type": "subagent_entries", "id": id, "subagent": subagent,
        "from": from, "count": all.len(), "entries": entries,
    });
    // A page asked for, which a phone puts before what it has.
    if asked.is_some() {
        msg["older"] = json!(true);
    }
    msg.to_string()
}

/// `msg` as the messages to send for it: itself when it fits a frame, else
/// `part`s in order, each a piece of its bytes.
pub fn parts(msg: String) -> Vec<String> {
    use base64::Engine as _;
    if msg.len() <= FRAME_TEXT {
        return vec![msg];
    }
    let pieces: Vec<&[u8]> = msg.as_bytes().chunks(PART_BYTES).collect();
    let of = pieces.len();
    pieces
        .into_iter()
        .enumerate()
        .map(|(i, piece)| {
            let data = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(piece);
            json!({ "type": "part", "i": i, "of": of, "data": data }).to_string()
        })
        .collect()
}

/// Puts the `part`s of a message back together on the side that receives them.
/// The pieces of one message come one after another, with nothing between.
#[derive(Default)]
pub struct Parts {
    pieces: Vec<u8>,
    next: usize,
}

impl Parts {
    /// A message as it came: itself, unless it is a `part`; then nothing until
    /// the last piece, which gives the whole message.
    pub fn take(&mut self, msg: Value) -> Option<Value> {
        use base64::Engine as _;
        if msg["type"] != "part" {
            *self = Parts::default();
            return Some(msg);
        }
        let (i, of) = (msg["i"].as_u64()? as usize, msg["of"].as_u64()? as usize);
        let piece = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(msg["data"].as_str()?)
            .ok()?;
        if i == 0 {
            *self = Parts::default();
        }
        // Out of order, or more than anyone sends: start over.
        if i != self.next || of > MAX_PARTS || self.pieces.len() + piece.len() > MAX_WHOLE {
            *self = Parts::default();
            return None;
        }
        self.pieces.extend_from_slice(&piece);
        self.next += 1;
        if self.next < of {
            return None;
        }
        let whole = std::mem::take(self).pieces;
        serde_json::from_slice(&whole).ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::{ToolCall, ToolStatus};

    #[test]
    fn commands_read_back_with_long_descriptions_clipped() {
        let long = "x".repeat(CLIP_COMMAND_DETAIL + 50);
        let sent = [
            Choice {
                id: "ship-pr".into(),
                name: "ship-pr".into(),
                description: Some(long.clone()),
            },
            Choice {
                id: "compact".into(),
                name: "compact".into(),
                description: None,
            },
        ];
        let msg: Value = serde_json::from_str(&commands_message("s1", &sent)).unwrap();
        assert_eq!(msg["type"], "commands");
        assert_eq!(msg["id"], "s1");
        let got = read_commands(&msg["commands"]);
        assert_eq!(got.len(), 2);
        assert_eq!(got[0].id, "ship-pr");
        assert_eq!(got[0].name, "ship-pr");
        let detail = got[0].description.as_deref().unwrap();
        assert!(detail.ends_with('…') && detail.chars().count() == CLIP_COMMAND_DETAIL + 1);
        assert_eq!(got[1].description, None);
        // A nameless entry is left out.
        assert!(read_commands(&json!([{"description": "d"}])).is_empty());
    }

    #[test]
    fn settings_read_back_as_they_were_written() {
        let option = |id: &str, disabled: Option<&str>| SettingOption {
            id: id.into(),
            name: id.to_uppercase(),
            detail: Some("1x".into()),
            description: None,
            disabled: disabled.map(Into::into),
        };
        let list = vec![
            (
                Setting::Model,
                (
                    "a".to_string(),
                    vec![option("a", None), option("b", Some("no"))],
                ),
            ),
            (
                Setting::Effort,
                ("high".to_string(), vec![option("high", None)]),
            ),
        ];
        let mut sent = settings(list.clone());
        assert_eq!(read_settings(&sent), list);
        // One this app does not know is left out; the rest still read.
        sent.as_array_mut()
            .unwrap()
            .push(json!({ "key": "newThing", "current": "x", "options": [] }));
        assert_eq!(read_settings(&sent), list);
        assert!(read_settings(&json!("nonsense")).is_empty());
    }

    fn scope(cwd: &std::path::Path) -> Scope {
        Scope {
            cwd: cwd.to_path_buf(),
            key: Some("k".into()),
        }
    }

    #[test]
    fn only_a_servers_inbox_says_it_sends_several_sessions() {
        let server: Value = serde_json::from_str(&server_inbox("h", vec![], vec![])).unwrap();
        assert_eq!(server["watch_several"], true);
        assert_eq!(server["type"], "inbox");
        let desktop: Value = serde_json::from_str(&inbox("h", vec![], vec![])).unwrap();
        assert!(desktop["watch_several"].is_null());
    }

    #[test]
    fn a_prompt_sets_its_image_and_paste_apart() {
        let dir = std::env::temp_dir().join(format!("arakun-wire-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("shot.png"), b"png").unwrap();
        let text = format!("look at @shot.png and {}", attach::wrap_paste("one\ntwo"));
        let e = Entry::User {
            text,
            at: 1,
            elapsed_ms: None,
            message_id: None,
            tokens: None,
            shell_changes: Vec::new(),
        };
        let v = entry(&e, &scope(&dir));
        assert_eq!(v["text"], "look at  and");
        assert_eq!(v["images"][0]["name"], "shot.png");
        let url = v["images"][0]["url"].as_str().unwrap();
        assert!(url.starts_with("/image?s="), "{url}");
        assert!(url.contains("shot.png"));
        assert_eq!(v["pastes"][0]["lines"], 2);
        assert_eq!(v["pastes"][0]["text"], "one\ntwo");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn an_agent_names_its_images_as_signed_links() {
        let dir = std::env::temp_dir().join(format!("arakun-wire-agent-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("shot.png"), b"png").unwrap();
        let e = Entry::Agent {
            text: "before ![the shot](shot.png) mid [link](https://arakun.dev) after ![gone](missing.png) ![remote](https://x.dev/a.png) last".into(),
        };
        let v = entry(&e, &scope(&dir));
        let text = v["text"].as_str().unwrap();
        assert!(text.starts_with("before ![the shot](/image?s="), "{text}");
        assert!(
            text.contains("shot.png) mid [link](https://arakun.dev) after ![gone](missing.png)"),
            "{text}"
        );
        assert!(
            text.ends_with("![remote](https://x.dev/a.png) last"),
            "{text}"
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn the_app_gets_only_images_the_agent_named() {
        let dir = std::env::temp_dir().join(format!("arakun-wire-image-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("shot.png"), b"png").unwrap();
        std::fs::write(dir.join("other.png"), b"other").unwrap();
        let all = vec![
            Arc::new(Entry::Agent {
                text: "Here:\n\n![the shot](shot.png)\n\n![gone](missing.png)".into(),
            }),
            // Only what the agent says is shown as images.
            Arc::new(Entry::Thought {
                text: "![x](other.png)".into(),
            }),
        ];
        let file = named_image(&all, &dir, "shot.png");
        assert_eq!(file, Some(dir.join("shot.png")));
        let v = image_message("s1", "shot.png", 3, file.as_deref());
        assert_eq!(v["type"], "image");
        assert_eq!(
            (&v["id"], &v["path"], &v["at"]),
            (&json!("s1"), &json!("shot.png"), &json!(3))
        );
        assert_eq!(v["mime"], "image/png");
        use base64::Engine as _;
        let data = base64::engine::general_purpose::STANDARD
            .decode(v["data"].as_str().unwrap())
            .unwrap();
        assert_eq!(data, b"png");
        assert_eq!(named_image(&all, &dir, "other.png"), None);
        assert_eq!(named_image(&all, &dir, "../shot.png"), None);
        // And the ones a prompt mentioned, as the prompt wrote them.
        let mut all = all;
        all.push(Arc::new(Entry::User {
            text: "look at @other.png and email@shot.png".into(),
            at: 1,
            elapsed_ms: None,
            message_id: None,
            tokens: None,
            shell_changes: Vec::new(),
        }));
        assert_eq!(
            named_image(&all, &dir, "other.png"),
            Some(dir.join("other.png"))
        );
        assert_eq!(named_image(&all, &dir, "../other.png"), None);
        let v = image_message(
            "s1",
            "missing.png",
            0,
            named_image(&all, &dir, "missing.png").as_deref(),
        );
        assert_eq!(v["missing"], true);
        assert!(v.get("data").is_none());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_servers_session_links_no_file_of_this_machine() {
        let dir = std::env::temp_dir().join(format!("arakun-wire-remote-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("shot.png"), b"png").unwrap();
        let unsigned = Scope {
            cwd: dir.clone(),
            key: None,
        };
        let said = Entry::Agent {
            text: "see ![the shot](shot.png)".into(),
        };
        assert_eq!(entry(&said, &unsigned)["text"], "see ![the shot](shot.png)");
        let asked = Entry::User {
            text: "look at @shot.png".into(),
            at: 1,
            elapsed_ms: None,
            message_id: None,
            tokens: None,
            shell_changes: Vec::new(),
        };
        let v = entry(&asked, &unsigned);
        assert_eq!(v["text"], "look at @shot.png");
        assert_eq!(v["images"], json!([]));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn long_output_keeps_its_end() {
        let long: String = (0..2000)
            .map(|i| char::from(b'a' + (i % 26) as u8))
            .collect();
        let t = tail(&long, 100);
        assert!(t.starts_with("… [1900 chars] "));
        assert!(t.ends_with(&long[1900..]));
        assert_eq!(tail("short", 100), "short");
    }

    #[test]
    fn a_tool_entry_carries_its_command_and_output() {
        let tool = ToolCall {
            id: "t1".into(),
            title: "Run tests".into(),
            kind: "execute".into(),
            status: ToolStatus::Completed,
            content: vec![],
            locations: vec!["/a/b/lib.rs".into()],
            raw_input: Some(json!({ "command": "cargo test" })),
            raw_output: None,
            added: 0,
            removed: 0,
            terminal_output: "ok\n".into(),
            terminal_dropped_lines: 0,
            exit_code: Some(0),
            parent: None,
            tool_name: None,
            secret: None,
            images: Vec::new(),
        };
        let v = entry(&Entry::Tool(tool), &scope(std::path::Path::new(".")));
        assert_eq!(v["type"], "tool");
        assert_eq!(v["command"], "cargo test");
        assert_eq!(v["output"], "ok\n");
        assert_eq!(v["locations"][0], "lib.rs");
        assert_eq!(v["status"], "completed");
        // Not made under a subagent: no parent at all, as older phones knew it.
        assert!(v.get("parent").is_none());
        // No images: no field, as older phones knew it.
        assert!(v.get("images").is_none());
    }

    #[test]
    fn a_tools_images_go_as_signed_links_and_to_the_app_by_path() {
        let dir =
            std::env::temp_dir().join(format!("arakun-wire-tool-image-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let shot = dir.join("paste-7.png");
        std::fs::write(&shot, b"png").unwrap();
        let mut tool = ToolCall {
            id: "t1".into(),
            title: "Screenshot".into(),
            kind: "other".into(),
            status: ToolStatus::Completed,
            content: vec![],
            locations: vec![],
            raw_input: None,
            // What an agent sends as the raw output of an image: its base64.
            raw_output: Some("[{\"type\":\"image\",\"data\":\"aGk=\"}]".into()),
            added: 0,
            removed: 0,
            terminal_output: String::new(),
            terminal_dropped_lines: 0,
            exit_code: None,
            parent: None,
            tool_name: None,
            secret: None,
            images: vec![shot.clone(), dir.join("gone.png")],
        };
        let v = entry(&Entry::Tool(tool.clone()), &scope(&dir));
        // Only the file that is there, linked as a prompt's image is.
        assert_eq!(v["images"].as_array().unwrap().len(), 1);
        assert_eq!(v["images"][0]["name"], "paste-7.png");
        assert!(
            v["images"][0]["url"]
                .as_str()
                .unwrap()
                .starts_with("/image?s=")
        );
        assert!(v["output"].is_null());
        let link = shot.to_string_lossy().into_owned();
        let all = vec![Arc::new(Entry::Tool(tool.clone()))];
        assert_eq!(named_image(&all, &dir, &link), Some(shot.clone()));
        assert_eq!(named_image(&all, &dir, "paste-7.png"), None);
        // A secret file's call shows none.
        tool.secret = Some(".env".into());
        let v = entry(&Entry::Tool(tool.clone()), &scope(&dir));
        assert!(v.get("images").is_none());
        let all = vec![Arc::new(Entry::Tool(tool))];
        assert_eq!(named_image(&all, &dir, &link), None);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_call_made_under_a_subagent_names_it() {
        let call = ToolCall {
            id: "t2".into(),
            title: "Read".into(),
            kind: "read".into(),
            status: ToolStatus::Completed,
            content: vec![],
            locations: vec![],
            raw_input: None,
            raw_output: None,
            added: 0,
            removed: 0,
            terminal_output: String::new(),
            terminal_dropped_lines: 0,
            exit_code: None,
            parent: Some("task1".into()),
            tool_name: None,
            secret: None,
            images: Vec::new(),
        };
        let v = entry(&Entry::Tool(call), &scope(std::path::Path::new(".")));
        assert_eq!(v["parent"], "task1");
    }

    #[test]
    fn a_secret_files_output_stays_off_the_phone() {
        let tool = ToolCall {
            id: "t1".into(),
            title: "cat .env".into(),
            kind: "execute".into(),
            status: ToolStatus::Completed,
            content: vec![ToolContent::Text {
                text: "KEY=hunter2".into(),
            }],
            locations: vec![],
            raw_input: Some(json!({ "command": "cat .env" })),
            raw_output: Some("KEY=hunter2".into()),
            added: 0,
            removed: 0,
            terminal_output: "KEY=hunter2\n".into(),
            terminal_dropped_lines: 0,
            exit_code: Some(0),
            parent: None,
            tool_name: None,
            secret: Some(".env".into()),
            images: Vec::new(),
        };
        let v = entry(&Entry::Tool(tool), &scope(std::path::Path::new(".")));
        assert_eq!(v["output"], Value::Null);
        assert_eq!(v["secret"], ".env");
        assert!(!v.to_string().contains("hunter2"));
    }

    fn edit(id: &str, path: &str, old: Option<&str>, new: &str, status: ToolStatus) -> Arc<Entry> {
        let lines = diff::line_diff(old.unwrap_or(""), new);
        let (added, removed) = diff::stats(&lines);
        Arc::new(Entry::Tool(ToolCall {
            id: id.into(),
            title: format!("Edit {path}"),
            kind: "edit".into(),
            status,
            content: vec![ToolContent::Diff {
                path: path.into(),
                old_text: old.map(str::to_string),
                new_text: new.into(),
            }],
            locations: vec![],
            raw_input: None,
            raw_output: None,
            added,
            removed,
            terminal_output: String::new(),
            terminal_dropped_lines: 0,
            exit_code: None,
            parent: None,
            tool_name: None,
            secret: None,
            images: Vec::new(),
        }))
    }

    fn user(text: &str, shell_changes: Vec<crate::agent::ShellChange>) -> Arc<Entry> {
        Arc::new(Entry::User {
            text: text.into(),
            at: 7,
            elapsed_ms: None,
            message_id: None,
            tokens: None,
            shell_changes,
        })
    }

    #[test]
    fn changes_group_files_by_turn_and_diffs_come_with_context() {
        let cwd = std::path::Path::new("/w");
        let all = vec![
            user("first", vec![]),
            edit(
                "t1",
                "/w/src/a.rs",
                Some("a\nb\nc\nd\ne\nf\ng\nh\n"),
                "a\nb\nc\nd\nX\nf\ng\nh\n",
                ToolStatus::Completed,
            ),
            edit(
                "t2",
                "/w/src/a.rs",
                Some("x\n"),
                "x\ny\n",
                ToolStatus::Completed,
            ),
            edit("t3", "/w/src/lost.rs", None, "never\n", ToolStatus::Failed),
            user(
                "second @shot.png",
                vec![crate::agent::ShellChange {
                    path: "/w/gen.txt".into(),
                    old: None,
                    new: "1\n2\n".into(),
                    added: 2,
                    removed: 0,
                }],
            ),
            edit("t4", "/w/src/b.rs", None, "new\n", ToolStatus::Completed),
            user("third, nothing changed", vec![]),
        ];
        // The prompt carries what its shell commands changed, so the turn's header can count it.
        let prompt = entry(&all[4], &scope(cwd));
        assert_eq!(
            prompt["edits"],
            json!([{ "path": "/w/gen.txt", "added": 2, "removed": 0 }])
        );
        assert_eq!(entry(&all[0], &scope(cwd))["edits"], json!([]));

        let totals = change_totals(&all);
        assert_eq!(totals["files"], 3);
        assert_eq!(totals["added"], 5);
        assert_eq!(totals["removed"], 1);

        let v = changes(&all, cwd);
        let turns = v["turns"].as_array().unwrap();
        assert_eq!(turns.len(), 2, "turns without changes are left out");
        assert_eq!(turns[0]["text"], "second @shot.png");
        assert_eq!(turns[0]["files"][0]["path"], "gen.txt");
        assert_eq!(turns[0]["files"][0]["diffs"], json!([[4, 0]]));
        assert_eq!(turns[0]["files"][1]["path"], "src/b.rs");
        assert_eq!(turns[1]["entry"], 0);
        assert_eq!(
            turns[1]["files"].as_array().unwrap().len(),
            1,
            "the failed edit does not count"
        );
        assert_eq!(turns[1]["files"][0]["name"], "a.rs");
        assert_eq!(turns[1]["files"][0]["added"], 2);
        assert_eq!(turns[1]["files"][0]["removed"], 1);
        assert_eq!(turns[1]["files"][0]["diffs"], json!([[1, 0], [2, 0]]));

        let d = diff_message(&all, 1, 0, cwd).unwrap();
        assert_eq!(d["path"], "src/a.rs");
        assert_eq!(d["added"], 1);
        assert_eq!(d["removed"], 1);
        let lines = d["lines"].as_array().unwrap();
        // The hunk opens with its header; three lines of context around the change, the first line left out.
        assert_eq!(lines[0], json!("@@ -2,7 +2,7 @@ a"));
        assert_eq!(lines[1], json!([" ", 2, "b"]));
        assert_eq!(lines[4], json!(["+", 5, "X"]));
        assert_eq!(lines[5], json!(["-", 5, "e"]));
        assert_eq!(lines.len(), 9);
        assert_eq!(d["cut"], 0);
        let shell = diff_message(&all, 4, 0, cwd).unwrap();
        assert_eq!(shell["new_file"], true);
        assert_eq!(shell["lines"][0], json!("@@ -0,0 +1,2 @@"));
        assert_eq!(shell["lines"][1], json!(["+", 1, "1"]));
        assert!(
            diff_message(&all, 3, 0, cwd).is_some(),
            "a failed edit still has a diff to look at"
        );
        assert!(diff_message(&all, 0, 0, cwd).is_none());
        assert!(diff_message(&all, 99, 0, cwd).is_none());
    }

    #[test]
    fn plan_usage_shows_each_agent_as_the_desktop_does() {
        use crate::account::{Bucket, Pool};
        use crate::agent::plan::Window;
        let now = 1_000_000;
        let window = |label: &str, percent: u64, active: bool| Window {
            label: label.into(),
            percent,
            severity: "normal".into(),
            resets_at: Some(now + 2 * 3600 + 30 * 60),
            active,
        };
        let plan = PlanUsage {
            at: now - 60,
            subscription: "team".into(),
            tier: "default_claude_max_5x".into(),
            windows: vec![
                window("5-hour limit", 32, true),
                window("Weekly · Sonnet", 0, false),
            ],
            spend: None,
            borrowed: false,
        };
        let bucket = |pct: f32, end: u64| Bucket {
            used_percent: pct,
            window_end: Some(end),
        };
        let limits = Limits {
            standard: Some(Pool {
                five_hour: bucket(100., now + 600),
                weekly: bucket(44., now + 86_400 * 3),
                monthly: bucket(10., now + 86_400 * 20),
            }),
            core: Some(Pool {
                five_hour: bucket(12., now + 600),
                weekly: bucket(5., now + 86_400 * 3),
                monthly: Bucket::default(),
            }),
            extra_usage_cents: 1250,
            overage_preference: Some("droidCore".into()),
        };
        let droid = DroidPlan {
            connected: true,
            limits: Some(&limits),
            error: None,
            fetched_at: Some(now - 10),
        };
        let v = plan_usage(Some(&plan), None, &droid, now);
        assert_eq!(v["type"], "usage");
        let claude = &v["agents"][0];
        assert_eq!(claude["signed_in"], true);
        assert_eq!(claude["plan"], "Team · Max 5x");
        assert_eq!(
            claude["rows"],
            json!([{ "label": "5-hour limit", "percent": 32, "detail": "resets in 2h 30m" }])
        );
        assert_eq!(claude["idle"], json!(["Weekly · Sonnet"]));
        let d = &v["agents"][1];
        assert_eq!(
            d["plan"], "Droid Core",
            "standard is used up and the account falls back to core"
        );
        assert_eq!(d["tone"], "warn");
        assert_eq!(d["balance"], "$12.50");
        let rows = d["rows"].as_array().unwrap();
        assert_eq!(rows[0]["label"], "Core · 5 hours");
        assert_eq!(rows[0]["percent"], 12);
        assert_eq!(rows[0]["detail"], "resets in 10m");
        assert_eq!(rows[1]["label"], "Core · Week");
        assert_eq!(
            rows[2]["label"], "Standard · 5 hours",
            "the other pool shows only its fullest window"
        );
        assert_eq!(rows[2]["percent"], 100);
        assert_eq!(rows.len(), 3);
        assert_eq!(d["idle"], json!(["Core · Month"]));

        let signed_out = DroidPlan {
            connected: false,
            limits: None,
            error: None,
            fetched_at: None,
        };
        let proxy = plan_usage(None, Some("Endpoint · localhost:8317"), &signed_out, now);
        assert_eq!(
            proxy["agents"][0]["signed_in"], true,
            "a key or endpoint runs Claude Code without a sign-in"
        );
        assert_eq!(proxy["agents"][0]["plan"], "Endpoint · localhost:8317");
        let out = plan_usage(None, None, &signed_out, now);
        assert_eq!(out["agents"][0]["signed_in"], false);
        assert_eq!(out["agents"][1]["signed_in"], false);
        assert!(
            out["agents"][1]["note"]
                .as_str()
                .unwrap()
                .contains("Factory API key")
        );
    }

    #[test]
    fn entries_from_an_index() {
        let all = vec![said("a"), said("b"), said("c")];
        let s = scope(std::path::Path::new("."));
        assert_eq!(entries(&all, 1, &s).len(), 2);
        assert_eq!(entries(&all, 1, &s)[0]["text"], "b");
        assert!(entries(&all, 7, &s).is_empty());
    }

    fn said(text: &str) -> Arc<Entry> {
        Arc::new(Entry::Agent { text: text.into() })
    }

    #[test]
    fn a_transcript_answer_stops_near_its_size_and_says_where() {
        let all: Vec<Arc<Entry>> = (0..5).map(|_| said(&"x".repeat(40 * 1024))).collect();
        let frames = transcript_message("s1", "claude", Path::new("/w/app"), &all, 1);
        assert_eq!(frames.len(), 1);
        let msg: Value = serde_json::from_str(&frames[0]).unwrap();
        assert_eq!(msg["type"], "transcript");
        assert_eq!(
            (msg["agent"].as_str(), msg["cwd"].as_str()),
            (Some("claude"), Some("/w/app"))
        );
        // Two of 40 KiB fit; the third comes when asked for, from `to`.
        assert_eq!(
            (
                msg["from"].as_u64(),
                msg["to"].as_u64(),
                msg["count"].as_u64()
            ),
            (Some(1), Some(3), Some(5))
        );
        // Whole entries, as the transcript file holds them.
        let back: Vec<Entry> = serde_json::from_value(msg["entries"].clone()).unwrap();
        assert_eq!(
            serde_json::to_value(&back).unwrap(),
            serde_json::to_value(&all[1..3]).unwrap()
        );

        // Asked from past the end: nothing, and the count to go by.
        let past = transcript_message("s1", "claude", Path::new("/w/app"), &all, 9);
        let msg: Value = serde_json::from_str(&past[0]).unwrap();
        assert_eq!(
            (
                msg["from"].as_u64(),
                msg["to"].as_u64(),
                msg["count"].as_u64()
            ),
            (Some(5), Some(5), Some(5))
        );
        assert_eq!(msg["entries"], json!([]));
    }

    #[test]
    fn a_message_too_big_for_a_frame_goes_in_parts_and_comes_back_whole() {
        // Two bytes a letter, so pieces cut letters in half.
        let big = said(&"é".repeat(200 * 1024));
        let frames = transcript_message("s1", "claude", Path::new("/w"), &[big], 0);
        assert!(frames.len() > 1);
        assert!(frames.iter().all(|f| f.len() <= FRAME_TEXT));
        let mut parts = Parts::default();
        let mut whole = None;
        for (i, frame) in frames.iter().enumerate() {
            whole = parts.take(serde_json::from_str(frame).unwrap());
            assert_eq!(whole.is_some(), i + 1 == frames.len());
        }
        let whole = whole.unwrap();
        assert_eq!(whole["type"], "transcript");
        let text = whole["entries"][0]["text"].as_str().unwrap();
        assert_eq!(text.chars().count(), 200 * 1024);

        // A message that is no part passes as it is.
        let small = json!({ "type": "inbox", "sessions": [] });
        assert_eq!(parts.take(small.clone()), Some(small));
        // A piece that is not the next one drops what came before it.
        let first: Value = serde_json::from_str(&frames[0]).unwrap();
        let third: Value = serde_json::from_str(&frames[2]).unwrap();
        assert_eq!(parts.take(first), None);
        assert_eq!(parts.take(third), None);
        let last: Value = serde_json::from_str(frames.last().unwrap()).unwrap();
        assert_eq!(parts.take(last), None);
    }

    #[test]
    fn subagents_travel_whole_with_their_transcripts() {
        use crate::agent::SubagentState;
        let record = |id: &str, text: &str| SubagentRecord {
            id: id.into(),
            name: "Explore".into(),
            task: "Find the bug".into(),
            state: SubagentState::Running,
            started: 7,
            ended: None,
            host: None,
            call: None,
            earlier: Vec::new(),
            entries: vec![said(text)],
        };
        // One small and one too big for a frame: it all comes in parts.
        let sent = vec![
            record("a", "found it"),
            record("b", &"x".repeat(200 * 1024)),
        ];
        let frames = subagents_message("s1", &sent);
        assert!(frames.len() > 1);
        let mut parts = Parts::default();
        let whole = frames
            .iter()
            .find_map(|f| parts.take(serde_json::from_str(f).unwrap()))
            .unwrap();
        assert_eq!(
            (whole["type"].as_str(), whole["id"].as_str()),
            (Some("subagents"), Some("s1"))
        );
        let back = read_subagents(&whole).unwrap();
        assert_eq!(back.len(), 2);
        assert_eq!(
            (back[0].id.as_str(), back[0].state),
            ("a", SubagentState::Running)
        );
        assert!(
            matches!(back[0].entries.as_slice(), [e] if matches!(&**e, Entry::Agent { text } if text == "found it"))
        );
        assert_eq!(
            serde_json::to_value(&back[1].entries).unwrap(),
            serde_json::to_value(&sent[1].entries).unwrap()
        );

        // A message without them is not read as none.
        assert!(read_subagents(&json!({ "type": "subagents", "id": "s1" })).is_err());
    }

    fn record(id: &str, task: &str, entries: Vec<Arc<Entry>>) -> SubagentRecord {
        SubagentRecord {
            id: id.into(),
            name: "Explore".into(),
            task: task.into(),
            state: SubagentState::Completed,
            started: 7,
            ended: Some(9),
            host: None,
            call: Some(format!("call-{id}")),
            earlier: Vec::new(),
            entries,
        }
    }

    #[test]
    fn a_phone_gets_subagents_brief_without_their_transcripts() {
        let sent = [
            record("a", "Find the bug", vec![said("found it")]),
            record("b", &"y".repeat(5000), Vec::new()),
        ];
        let heads: Vec<SubagentHead> = sent.iter().map(Into::into).collect();
        let msg: Value = serde_json::from_str(&brief_subagents_message("s1", &heads)).unwrap();
        assert_eq!(
            (
                msg["type"].as_str(),
                msg["id"].as_str(),
                msg["brief"].as_bool()
            ),
            (Some("subagents"), Some("s1"), Some(true))
        );
        let a = &msg["subagents"][0];
        assert_eq!(
            a,
            &json!({
                "id": "a", "name": "Explore", "task": "Find the bug", "state": "completed",
                "started": 7, "ended": 9, "call": "call-a", "host": null,
            })
        );
        // A long task is cut to a line or two.
        let task = msg["subagents"][1]["task"].as_str().unwrap();
        assert!(task.chars().count() <= CLIP_TASK + 1 && task.ends_with('…'));

        // The app, which reads them whole, does not take a brief list for one.
        assert!(read_subagents(&msg).is_err());
    }

    #[test]
    fn a_brief_list_too_long_for_a_frame_drops_the_oldest() {
        let sent: Vec<SubagentRecord> = (0..1000)
            .map(|i| record(&i.to_string(), &"t".repeat(400), Vec::new()))
            .collect();
        let heads: Vec<SubagentHead> = sent.iter().map(Into::into).collect();
        let text = brief_subagents_message("s1", &heads);
        assert!(text.len() <= FRAME_TEXT);
        let msg: Value = serde_json::from_str(&text).unwrap();
        let list = msg["subagents"].as_array().unwrap();
        assert!(!list.is_empty() && list.len() < 1000);
        assert_eq!(list.last().unwrap()["id"], "999");
    }

    #[test]
    fn a_subagents_transcript_comes_as_its_tail_then_older_pages() {
        let all: Vec<Arc<Entry>> = (0..80).map(|i| said(&i.to_string())).collect();
        let at = scope(std::path::Path::new("."));
        let read = |s: String| serde_json::from_str::<Value>(&s).unwrap();

        // Opened: the tail, not marked older.
        let tail = read(subagent_entries_message(
            "s1",
            "a",
            &all,
            80 - TAIL,
            None,
            &at,
        ));
        assert_eq!(
            (
                tail["type"].as_str(),
                tail["id"].as_str(),
                tail["subagent"].as_str()
            ),
            (Some("subagent_entries"), Some("s1"), Some("a"))
        );
        assert_eq!(
            (tail["from"].as_u64(), tail["count"].as_u64()),
            (Some(30), Some(80))
        );
        assert_eq!(tail["entries"].as_array().unwrap().len(), TAIL);
        assert_eq!(tail["entries"][0]["text"], "30");
        assert!(tail.get("older").is_none());

        // A page asked for: that range, marked older, and kept inside the transcript.
        let page = read(subagent_entries_message("s1", "a", &all, 10, Some(20), &at));
        assert_eq!(page["from"], 10);
        assert_eq!(page["entries"].as_array().unwrap().len(), 10);
        assert_eq!(page["entries"][0]["text"], "10");
        assert_eq!(page["older"], true);
        let past = read(subagent_entries_message("s1", "a", &all, 90, Some(99), &at));
        assert_eq!(
            (
                past["from"].as_u64(),
                past["entries"].as_array().unwrap().len()
            ),
            (Some(80), 0)
        );
    }

    #[test]
    fn an_inbox_line_hashes_what_it_shows() {
        use std::hash::{DefaultHasher, Hash, Hasher};
        let line = |status, asks: Option<&str>| {
            let mut h = DefaultHasher::new();
            InboxSession {
                id: "s1",
                title: "Fix it",
                agent: "Claude",
                status,
                updated: 5,
                added: 1,
                removed: 2,
                project: "app",
                env: "main",
                place: Path::new("/work"),
                pr: None,
                asks: asks.map(|t| json!({ "kind": "permission", "text": t })),
            }
            .hash(&mut h);
            h.finish()
        };
        assert_eq!(line(Status::Idle, None), line(Status::Idle, None));
        assert_ne!(line(Status::Idle, None), line(Status::Working, None));
        assert_ne!(
            line(Status::Idle, None),
            line(Status::Idle, Some("Run ls?"))
        );
        assert_ne!(
            line(Status::Idle, Some("Run ls?")),
            line(Status::Idle, Some("Run rm?"))
        );
    }

    fn agent(text: &str) -> Arc<Entry> {
        Arc::new(Entry::Agent { text: text.into() })
    }

    fn sent(stream: &EntryStream, client: ClientId, flush: &EntryFlush) -> Option<Value> {
        stream
            .message_for(client, flush)
            .map(|m| serde_json::from_str(&m).unwrap())
    }

    #[test]
    fn a_streaming_reply_goes_as_its_new_words_to_a_client_that_takes_append() {
        let scope = scope(std::path::Path::new("/p"));
        let mut stream = EntryStream::default();
        stream.opened(1, "s", true);
        stream.opened(2, "s", false);
        let mut all = vec![agent("hi")];
        // The first flush after opening goes whole to both.
        let f = stream.flush("s", &all, 0, &scope);
        assert_eq!(sent(&stream, 1, &f).unwrap()["type"], "entries");
        assert_eq!(sent(&stream, 2, &f).unwrap()["type"], "entries");

        all[0] = agent("hi there");
        let f = stream.flush("s", &all, 0, &scope);
        assert_eq!(
            sent(&stream, 1, &f).unwrap(),
            json!({ "type": "append", "id": "s", "n": 0, "at": 2, "text": " there" })
        );
        assert_eq!(
            sent(&stream, 2, &f).unwrap()["entries"][0]["text"],
            "hi there"
        );

        // Nothing new in the words: nothing for the one that takes append.
        let f = stream.flush("s", &all, 0, &scope);
        assert_eq!(sent(&stream, 1, &f), None);
        assert_eq!(sent(&stream, 2, &f).unwrap()["type"], "entries");
    }

    #[test]
    fn an_append_counts_utf16_units_and_can_take_words_back() {
        // "é" is one unit and "😀" two; the words changed after them.
        let m: Value =
            serde_json::from_str(&append_message("s", 3, "é😀ab", "é😀ax!").unwrap()).unwrap();
        assert_eq!(m["at"], 4);
        assert_eq!(m["text"], "x!");
        assert_eq!(m["n"], 3);
        assert_eq!(append_message("s", 0, "same", "same"), None);
    }

    #[test]
    fn a_new_entry_or_an_earlier_change_goes_whole() {
        let scope = scope(std::path::Path::new("/p"));
        let mut stream = EntryStream::default();
        stream.opened(1, "s", true);
        let mut all = vec![agent("a")];
        stream.flush("s", &all, 0, &scope);
        // A new entry: the count changed.
        all.push(agent("b"));
        let f = stream.flush("s", &all, 1, &scope);
        assert_eq!(sent(&stream, 1, &f).unwrap()["type"], "entries");
        // An earlier entry changed along with the last.
        all[0] = agent("a2");
        all[1] = agent("b2");
        let f = stream.flush("s", &all, 0, &scope);
        assert_eq!(sent(&stream, 1, &f).unwrap()["type"], "entries");
        // A thought in place of the words: another kind.
        all[1] = Arc::new(Entry::Thought { text: "b2".into() });
        let f = stream.flush("s", &all, 1, &scope);
        assert_eq!(sent(&stream, 1, &f).unwrap()["type"], "entries");
        // Opened again: whole, since what it got on opening may be newer.
        stream.opened(1, "s", true);
        let f = stream.flush("s", &all, 1, &scope);
        assert_eq!(sent(&stream, 1, &f).unwrap()["type"], "entries");
    }
}
