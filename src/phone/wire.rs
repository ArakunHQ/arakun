//! What goes down the WebSocket, as JSON. Every message has a `type`. Entries
//! are the transcript's, cut down for a phone: long tool output is clipped,
//! diffs travel as counts, and the fields only the desktop uses stay home.
//!
//! Server → phone:
//! - `inbox`: every session of every project, with its status.
//! - `session`: a session just opened: its tail of entries and its state.
//! - `entries`: entries from `from` on changed; `count` is the new total.
//! - `state`: a watched session's status, permission or usage changed.
//! - `usage`: both agents' plan limits, on request and when they change.
//! - `error`.

use std::path::{Path, PathBuf};

use serde_json::{Value, json};

use crate::account::{self, Limits, UsageMode};
use crate::agent::plan::PlanUsage;
use crate::agent::{
    AskUserRequest, Entry, PermissionRequest, ToolContent, ToolStatus, Usage, attach, diff,
    fmt_cost,
};
use crate::model::Status;

/// What entries are read against: the session's directory, which the `@path`
/// mentions in prompts are relative to, and the key that signs image links.
pub struct Scope {
    pub cwd: PathBuf,
    pub key: String,
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

pub fn error(message: &str) -> String {
    json!({ "type": "error", "message": message }).to_string()
}

/// The entries from `from` on, as the phone shows them.
pub fn entries(all: &[Entry], from: usize, scope: &Scope) -> Vec<Value> {
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
    (len <= attach::MAX_IMAGE_BYTES).then(|| super::image_link(&scope.key, &file))
}

pub fn permission(p: &PermissionRequest) -> Value {
    json!({
        "tool_call_id": p.tool_call_id,
        "title": p.title,
        "options": p.options.iter().map(|o| json!({ "id": o.id, "name": o.name, "kind": o.kind })).collect::<Vec<_>>(),
    })
}

pub fn question(q: &AskUserRequest) -> Value {
    json!({
        "tool_call_id": q.tool_call_id,
        "questions": q.questions.iter().map(|q| json!({
            "index": q.index, "topic": q.topic, "question": q.question, "options": q.options, "multi": q.multi,
        })).collect::<Vec<_>>(),
    })
}

pub fn usage(u: &Usage) -> Value {
    json!({
        "summary": u.summary(),
        "context_percent": u.context_percent(),
        "tokens": u.total_tokens(),
    })
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
            let (typed, images) = attach::split_images(&typed, &scope.cwd);
            json!({
                "type": "user", "text": typed, "at": at, "elapsed_ms": elapsed_ms, "tokens": tokens,
                "pastes": pastes.iter().map(|p| json!({ "lines": p.lines().count(), "text": head(p, CLIP) })).collect::<Vec<_>>(),
                "images": images.iter().map(|p| json!({ "name": file_name(p), "url": super::image_link(&scope.key, p) })).collect::<Vec<_>>(),
                // What the turn's shell commands changed, for the turn's header.
                "edits": shell_changes.iter().map(|c| json!({ "path": c.path, "added": c.added, "removed": c.removed })).collect::<Vec<_>>(),
            })
        }
        Entry::Agent { text } => json!({ "type": "agent", "text": agent_images(text, scope) }),
        Entry::Thought { text } => json!({ "type": "thought", "text": tail(text, CLIP) }),
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
            let output = if !t.terminal_output.is_empty() {
                Some(tail(&t.terminal_output, CLIP))
            } else if !texts.is_empty() {
                Some(tail(&texts.join("\n"), CLIP))
            } else {
                t.raw_output.as_deref().map(|o| tail(o, CLIP))
            };
            json!({
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
            })
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
fn turns_with_changes(all: &[Entry]) -> Vec<TurnChanges<'_>> {
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
        match e {
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
pub fn change_totals(all: &[Entry]) -> Value {
    let mut files: Vec<&Path> = Vec::new();
    let (mut added, mut removed) = (0u32, 0u32);
    for e in all {
        match e {
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
pub fn changes(all: &[Entry], cwd: &Path) -> Value {
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
pub fn diff_message(all: &[Entry], entry: usize, index: usize, cwd: &Path) -> Option<Value> {
    let (path, old, new) = match all.get(entry)? {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::{ToolCall, ToolStatus};

    fn scope(cwd: &std::path::Path) -> Scope {
        Scope {
            cwd: cwd.to_path_buf(),
            key: "k".into(),
        }
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
            exit_code: Some(0),
            parent: None,
            tool_name: None,
        };
        let v = entry(&Entry::Tool(tool), &scope(std::path::Path::new(".")));
        assert_eq!(v["type"], "tool");
        assert_eq!(v["command"], "cargo test");
        assert_eq!(v["output"], "ok\n");
        assert_eq!(v["locations"][0], "lib.rs");
        assert_eq!(v["status"], "completed");
    }

    fn edit(id: &str, path: &str, old: Option<&str>, new: &str, status: ToolStatus) -> Entry {
        let lines = diff::line_diff(old.unwrap_or(""), new);
        let (added, removed) = diff::stats(&lines);
        Entry::Tool(ToolCall {
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
            exit_code: None,
            parent: None,
            tool_name: None,
        })
    }

    fn user(text: &str, shell_changes: Vec<crate::agent::ShellChange>) -> Entry {
        Entry::User {
            text: text.into(),
            at: 7,
            elapsed_ms: None,
            message_id: None,
            tokens: None,
            shell_changes,
        }
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
        let all = vec![
            Entry::Agent { text: "a".into() },
            Entry::Agent { text: "b".into() },
            Entry::Agent { text: "c".into() },
        ];
        let s = scope(std::path::Path::new("."));
        assert_eq!(entries(&all, 1, &s).len(), 2);
        assert_eq!(entries(&all, 1, &s)[0]["text"], "b");
        assert!(entries(&all, 7, &s).is_empty());
    }
}
