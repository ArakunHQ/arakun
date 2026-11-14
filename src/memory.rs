//! Memory the agents keep across sessions: what was learned in a project
//! (decisions, gotchas, fixes, conventions) and about the user (preferences,
//! for every project). Agents write it with `arakun mem`; arakun puts an index of
//! it in each new session's system prompt, so the next session starts knowing.
//!
//! Plain JSON files under `$ARAKUN_MEMORY_DIR` (default: arakun's data dir):
//! `global.json` and `projects/<name>-<hash>.json`, one per repository (git
//! worktrees share their main checkout's). Works without the app running, so
//! headless agents can use it too. Writers take an exclusive `flock`.

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

pub const KINDS: &[&str] =
    &["decision", "bugfix", "gotcha", "convention", "preference", "discovery", "pattern", "config", "session", "note"];

/// Kinds whose text the index shows in full when short: they should change
/// what the agent does without it having to look them up first.
const INLINE: &[&str] = &["preference", "convention", "gotcha"];
const INLINE_MAX: usize = 280;
/// Roughly 2k tokens: room for the work itself.
const INDEX_BUDGET: usize = 8000;
const SESSIONS_KEPT: usize = 30;

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Memory {
    pub id: u64,
    pub title: String,
    pub kind: String,
    #[serde(default)]
    pub body: String,
    /// A stable key for knowledge that evolves (`auth/model`): saving the same
    /// topic again updates this memory instead of adding another.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub topic: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub files: Vec<String>,
    pub created: u64,
    pub updated: u64,
    /// Times it was read in full (`arakun mem get`).
    #[serde(default)]
    pub hits: u32,
    #[serde(default)]
    pub revisions: u32,
}

#[derive(Serialize, Deserialize, Default, Debug)]
pub struct Store {
    /// The repository this store is about (empty for the global one).
    #[serde(default)]
    pub path: String,
    #[serde(default)]
    pub next_id: u64,
    #[serde(default)]
    pub items: Vec<Memory>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Scope {
    Project,
    Global,
}

impl Scope {
    fn prefix(self) -> &'static str {
        match self {
            Scope::Project => "",
            Scope::Global => "g",
        }
    }
}

/// `12` is a project memory, `g3` a global one.
pub fn parse_id(s: &str) -> Option<(Scope, u64)> {
    let s = s.trim().trim_start_matches('#');
    match s.strip_prefix('g') {
        Some(n) => Some((Scope::Global, n.parse().ok()?)),
        None => Some((Scope::Project, s.parse().ok()?)),
    }
}

pub fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

/// Where memory lives.
pub fn root() -> PathBuf {
    std::env::var_os("ARAKUN_MEMORY_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| crate::state::data_dir().join("memory"))
}

/// The repository `dir` belongs to: a worktree's main checkout, else the
/// nearest directory with `.git`, else `dir` itself.
pub fn project_of(dir: &Path) -> PathBuf {
    let common = crate::platform::command("git")
        .args(["rev-parse", "--path-format=absolute", "--git-common-dir"])
        .current_dir(dir)
        .stderr(std::process::Stdio::null())
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| PathBuf::from(String::from_utf8_lossy(&o.stdout).trim()));
    if let Some(common) = common {
        if common.file_name().is_some_and(|n| n == ".git")
            && let Some(parent) = common.parent()
        {
            return parent.to_path_buf();
        }
        return common;
    }
    dir.ancestors().find(|d| d.join(".git").exists()).unwrap_or(dir).to_path_buf()
}

fn fnv(s: &str) -> u64 {
    s.bytes().fold(0xcbf29ce484222325, |h, b| (h ^ b as u64).wrapping_mul(0x100000001b3))
}

pub fn store_file(root: &Path, scope: Scope, project: &Path) -> PathBuf {
    match scope {
        Scope::Global => root.join("global.json"),
        Scope::Project => {
            let path = project.display().to_string();
            let name: String = project
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_default()
                .chars()
                .map(|c| if c.is_alphanumeric() || c == '-' || c == '_' { c } else { '-' })
                .collect();
            root.join("projects").join(format!("{name}-{:08x}.json", fnv(&path) as u32))
        }
    }
}

pub fn load(file: &Path) -> Store {
    std::fs::read(file).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()
}

/// Runs `f` on the store in `file` under an exclusive lock and writes it back
/// when `f` says it changed.
pub fn update<T>(file: &Path, project: &str, f: impl FnOnce(&mut Store) -> (bool, T)) -> std::io::Result<T> {
    if let Some(dir) = file.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let lock = std::fs::OpenOptions::new().create(true).truncate(false).write(true).open(file.with_extension("lock"))?;
    // Released when `lock` closes.
    lock.lock()?;
    let mut store = load(file);
    if store.path.is_empty() {
        store.path = project.to_string();
    }
    let (changed, out) = f(&mut store);
    if changed {
        let bytes = serde_json::to_vec_pretty(&store).map_err(std::io::Error::other)?;
        crate::durable::write(file, &bytes)?;
    }
    Ok(out)
}

/// What a save did.
#[derive(Debug, PartialEq)]
pub enum Saved {
    New(u64),
    Updated(u64),
    Same(u64),
}

pub struct Draft {
    pub title: String,
    pub kind: String,
    pub body: String,
    pub topic: Option<String>,
    pub files: Vec<String>,
}

/// Adds `d`, or updates the memory with its topic, or the one with the same
/// title and text; session summaries beyond the newest few are dropped.
pub fn save(store: &mut Store, d: Draft, at: u64) -> Saved {
    let same_topic = d.topic.as_ref().and_then(|t| store.items.iter().position(|m| m.topic.as_ref() == Some(t)));
    if let Some(i) = same_topic {
        let m = &mut store.items[i];
        if m.title == d.title && m.body == d.body && m.kind == d.kind && m.files == d.files {
            return Saved::Same(m.id);
        }
        m.title = d.title;
        m.body = d.body;
        m.kind = d.kind;
        m.files = d.files;
        m.updated = at;
        m.revisions += 1;
        return Saved::Updated(m.id);
    }
    if let Some(m) =
        store.items.iter_mut().find(|m| m.title.eq_ignore_ascii_case(&d.title) && m.body.trim() == d.body.trim())
    {
        m.updated = at;
        return Saved::Same(m.id);
    }
    store.next_id = store.next_id.max(store.items.iter().map(|m| m.id).max().unwrap_or(0)) + 1;
    let id = store.next_id;
    let session = d.kind == "session";
    store.items.push(Memory {
        id,
        title: d.title,
        kind: d.kind,
        body: d.body,
        topic: d.topic,
        files: d.files,
        created: at,
        updated: at,
        hits: 0,
        revisions: 0,
    });
    if session {
        let mut sessions: Vec<(u64, u64)> =
            store.items.iter().filter(|m| m.kind == "session").map(|m| (m.updated, m.id)).collect();
        sessions.sort_unstable_by(|a, b| b.cmp(a));
        let drop: Vec<u64> = sessions.iter().skip(SESSIONS_KEPT).map(|&(_, id)| id).collect();
        store.items.retain(|m| !drop.contains(&m.id));
    }
    Saved::New(id)
}

// ---- search ---------------------------------------------------------------

/// Lowercase words with accents folded, so `configuración` finds `configuracion`;
/// a plural `s` is dropped from longer words.
pub fn tokens(text: &str) -> Vec<String> {
    let fold = |c: char| match c {
        'á' | 'à' | 'ä' | 'â' => 'a',
        'é' | 'è' | 'ë' | 'ê' => 'e',
        'í' | 'ì' | 'ï' | 'î' => 'i',
        'ó' | 'ò' | 'ö' | 'ô' => 'o',
        'ú' | 'ù' | 'ü' | 'û' => 'u',
        'ñ' => 'n',
        'ç' => 'c',
        c => c,
    };
    let mut out = Vec::new();
    // camelCase and snake_case parts count as words too, and so does the whole.
    for raw in text.split(|c: char| !(c.is_alphanumeric() || c == '_')) {
        if raw.is_empty() {
            continue;
        }
        let mut parts: Vec<String> = Vec::new();
        let mut cur = String::new();
        let mut prev_lower = false;
        for c in raw.chars() {
            if (c == '_' || (c.is_uppercase() && prev_lower)) && !cur.is_empty() {
                parts.push(std::mem::take(&mut cur));
            }
            if c != '_' {
                cur.push(c);
            }
            prev_lower = c.is_lowercase() || c.is_ascii_digit();
        }
        if !cur.is_empty() {
            parts.push(cur);
        }
        let whole: String = raw.chars().filter(|&c| c != '_').collect();
        if parts.len() > 1 {
            parts.push(whole);
        }
        for p in parts {
            let mut w: String = p.to_lowercase().chars().map(fold).collect();
            if w.chars().count() > 4 && w.ends_with('s') && !w.ends_with("ss") {
                w.pop();
            }
            if w.chars().count() >= 2 || w.chars().all(|c| c.is_ascii_digit()) {
                out.push(w);
            }
        }
    }
    out
}

fn doc_tokens(m: &Memory) -> Vec<String> {
    let mut t = tokens(&m.title);
    t.extend(tokens(&m.title));
    t.extend(tokens(&m.body));
    if let Some(topic) = &m.topic {
        t.extend(tokens(topic));
    }
    for f in &m.files {
        t.extend(tokens(f));
    }
    t.extend(tokens(&m.kind));
    t
}

/// BM25 over title (counted twice), text, topic, files and kind. A query word
/// of 4+ letters also matches words that start with it, at half weight.
pub fn search<'a>(items: &'a [Memory], query: &str) -> Vec<(f32, &'a Memory)> {
    let q = tokens(query);
    if q.is_empty() {
        return Vec::new();
    }
    let docs: Vec<Vec<String>> = items.iter().map(doc_tokens).collect();
    let n = docs.len() as f32;
    let avg = (docs.iter().map(Vec::len).sum::<usize>() as f32 / n.max(1.)).max(1.);
    let (k1, b) = (1.2, 0.75);
    let mut scored: Vec<(f32, &Memory)> = items
        .iter()
        .zip(&docs)
        .filter_map(|(m, d)| {
            let len = d.len() as f32;
            let mut score = 0.;
            for term in &q {
                let prefix = term.chars().count() >= 4;
                let weight = |w: &String| {
                    if w == term {
                        1.
                    } else if prefix && w.starts_with(term.as_str()) {
                        0.5
                    } else {
                        0.
                    }
                };
                let tf: f32 = d.iter().map(weight).sum();
                if tf == 0. {
                    continue;
                }
                let df = docs.iter().filter(|d| d.iter().any(|w| weight(w) > 0.)).count() as f32;
                let idf = (1. + (n - df + 0.5) / (df + 0.5)).ln();
                score += idf * tf * (k1 + 1.) / (tf + k1 * (1. - b + b * len / avg));
            }
            (score > 0.).then_some((score, m))
        })
        .collect();
    scored.sort_by(|a, b| b.0.total_cmp(&a.0).then(b.1.updated.cmp(&a.1.updated)));
    scored
}

// ---- formatting -----------------------------------------------------------

pub fn ago(then: u64, now: u64) -> String {
    let s = now.saturating_sub(then);
    match s {
        0..60 => "just now".into(),
        60..3600 => format!("{}m ago", s / 60),
        3600..86400 => format!("{}h ago", s / 3600),
        86400..2_592_000 => format!("{}d ago", s / 86400),
        _ => format!("{}mo ago", s / 2_592_000),
    }
}

fn first_line(s: &str, max: usize) -> String {
    let flat = s.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() <= max {
        return flat;
    }
    let cut: String = flat.chars().take(max).collect();
    format!("{}…", cut.trim_end())
}

fn label(scope: Scope, m: &Memory) -> String {
    format!("#{}{}", scope.prefix(), m.id)
}

/// One line per memory, for search results and lists.
pub fn brief(scope: Scope, m: &Memory, now: u64) -> String {
    let files = if m.files.is_empty() { String::new() } else { format!(" · {}", m.files.join(", ")) };
    let body = if m.body.is_empty() { String::new() } else { format!("\n    {}", first_line(&m.body, 140)) };
    format!(
        "{} [{}] {} · {} · ~{} tokens{files}{body}",
        label(scope, m),
        m.kind,
        m.title,
        ago(m.updated, now),
        (m.title.len() + m.body.len()) / 4 + 1
    )
}

pub fn full(scope: Scope, m: &Memory, now: u64) -> String {
    let mut out = format!("{} [{}] {}\n", label(scope, m), m.kind, m.title);
    let mut meta = vec![format!("updated {}", ago(m.updated, now))];
    if let Some(t) = &m.topic {
        meta.push(format!("topic {t}"));
    }
    if !m.files.is_empty() {
        meta.push(format!("files {}", m.files.join(", ")));
    }
    out.push_str(&format!("({})\n", meta.join(" · ")));
    if !m.body.is_empty() {
        out.push('\n');
        out.push_str(m.body.trim_end());
        out.push('\n');
    }
    out
}

/// How a memory ranks for the index: what changes behavior first, then what
/// has been useful, then what is recent.
fn rank(m: &Memory, now: u64) -> f32 {
    let kind = match m.kind.as_str() {
        "preference" | "convention" | "gotcha" => 3.,
        "decision" | "config" => 2.,
        "bugfix" | "pattern" => 1.5,
        _ => 1.,
    };
    let days = now.saturating_sub(m.updated) as f32 / 86400.;
    kind + (m.hits as f32).ln_1p() - days / 60.
}

fn index_lines(scope: Scope, items: &[Memory], now: u64) -> Vec<String> {
    let mut list: Vec<&Memory> = items.iter().filter(|m| m.kind != "session").collect();
    list.sort_by(|a, b| rank(b, now).total_cmp(&rank(a, now)));
    list.iter()
        .map(|m| {
            let files = if m.files.is_empty() { String::new() } else { format!(" ({})", m.files.join(", ")) };
            let inline = INLINE.contains(&m.kind.as_str()) && !m.body.is_empty() && m.body.len() <= INLINE_MAX;
            let body = if inline { format!(": {}", first_line(&m.body, INLINE_MAX)) } else { String::new() };
            format!("- {} [{}] {}{files}{body}", label(scope, m), m.kind, m.title)
        })
        .collect()
}

/// The index each new session starts with: the last session's handoff, then
/// what is known about this project and about the user, within a budget.
pub fn context(project: &Store, global: &Store, name: &str, now: u64) -> String {
    let mut out = String::new();
    let mut sessions: Vec<&Memory> = project.items.iter().filter(|m| m.kind == "session").collect();
    sessions.sort_by_key(|m| std::cmp::Reverse(m.updated));
    if let Some(s) = sessions.first() {
        out.push_str(&format!("Last session here ({}): {}\n", ago(s.updated, now), s.title));
        if !s.body.is_empty() {
            let body: String = s.body.trim().chars().take(2400).collect();
            for l in body.lines() {
                out.push_str(&format!("  {l}\n"));
            }
        }
        if !s.files.is_empty() {
            out.push_str(&format!("  Changed: {}\n", s.files.join(", ")));
        }
    }
    if sessions.len() > 1 {
        out.push_str("Earlier sessions here (`arakun mem get <id>` for what each was asked):\n");
        for s in sessions.iter().skip(1).take(EARLIER_SHOWN) {
            out.push_str(&format!("- {} {} · {}\n", label(Scope::Project, s), s.title, ago(s.updated, now)));
        }
    }
    let mut budget = INDEX_BUDGET.saturating_sub(out.len());
    let mut section = |title: &str, lines: Vec<String>, out: &mut String| {
        if lines.is_empty() {
            return;
        }
        let total = lines.len();
        let mut shown = 0;
        let mut text = format!("{title}\n");
        for l in lines {
            if l.len() + 1 > budget {
                break;
            }
            budget -= l.len() + 1;
            text.push_str(&l);
            text.push('\n');
            shown += 1;
        }
        if shown < total {
            text.push_str(&format!("  …and {} more: `arakun mem search <words>` finds them\n", total - shown));
        }
        out.push_str(&text);
    };
    section(
        &format!("Known about {name} (`arakun mem get <id>` for the full text):"),
        index_lines(Scope::Project, &project.items, now),
        &mut out,
    );
    section("About the user, in every project:", index_lines(Scope::Global, &global.items, now), &mut out);
    if out.is_empty() {
        out.push_str(&format!("Nothing saved yet for {name}.\n"));
    }
    out
}

/// How agents are told to use memory; the index follows it.
pub const PROTOCOL: &str = "\
Memory: `arakun mem` keeps what was learned here across sessions, for you and the next agent. \
The index below is what is known so far. arakun itself records what each session was asked, how \
those turns ended, what it committed and which files it changed, so you never need to write session summaries.
- Before working in an area the index mentions, read the entry: `arakun mem get <id>`. When the user \
refers to earlier work, a task looks like something solved before, or you hit an error that may be \
known, `arakun mem search <words>` first.
- Save sparingly: only what a later session would otherwise get wrong or spend real time rediscovering, \
right when you learn it, with a short searchable title and What/Why/Where text: \
`arakun mem save \"<title>\" --kind <kind> [--files a,b] [--topic area/name] --body \"<text>\"`. \
Kinds: gotcha (a non-obvious trap: env vars, ordering, commands that must be run a certain way), \
bugfix (root cause and fix), decision (what was chosen and why), convention (how this project wants \
things done), config, pattern, discovery. Reuse a --topic to update knowledge that evolves instead of adding another.
- When the user corrects you or states how they want things, save it: --kind preference, and \
--global when it is about them rather than this project.
- Do not save what the code, the git log or the session record already says, raw output, or temporary state.
- If an entry is wrong or outdated, fix it with the same --topic or `arakun mem forget <id>`.";

/// The memory part of a session's system prompt for work in `dir`.
pub fn prompt_for(dir: &Path) -> String {
    let root = root();
    let project = project_of(dir);
    let name = project.file_name().map_or_else(|| project.display().to_string(), |n| n.to_string_lossy().to_string());
    let p = load(&store_file(&root, Scope::Project, &project));
    let g = load(&store_file(&root, Scope::Global, &project));
    prompt(&p, &g, &name, now())
}

fn prompt(project: &Store, global: &Store, name: &str, now: u64) -> String {
    format!("{PROTOCOL}\n\n{}", context(project, global, name, now))
}

// ---- session record -------------------------------------------------------

/// Requests a session record keeps: the first, which usually says what the
/// session is about, and the latest ones.
const ASKED_LATEST: usize = 3;
const ASKED_MAX: usize = 220;
const REPLY_MAX: usize = 200;
const COMMITS_KEPT: usize = 8;
const FILES_KEPT: usize = 12;
const EARLIER_SHOWN: usize = 4;

/// One request of a session and the last thing the agent said in that turn.
#[derive(Clone, Debug, Default)]
pub struct Turn {
    pub asked: String,
    pub reply: String,
}

/// The opening paragraph of a reply, where agents put the outcome, skipping
/// code fences and rendered blocks.
fn reply_lead(reply: &str) -> Option<String> {
    reply
        .split("\n\n")
        .map(str::trim)
        .find(|p| !p.is_empty() && !p.starts_with("```") && !p.starts_with('<'))
        .map(|p| first_line(p.trim_start_matches('#'), REPLY_MAX))
}

/// The record arakun keeps of a session without the agent's help: what the user
/// asked, how those turns ended, what was committed and which files changed.
/// `key` names the session, so each turn updates the same memory.
pub fn session_draft(key: &str, title: &str, turns: &[Turn], commits: &[String], files: &[String]) -> Option<Draft> {
    let turns: Vec<&Turn> = turns.iter().filter(|t| !t.asked.trim().is_empty()).collect();
    let first = turns.first()?;
    let title = match title.trim() {
        "" | "New session" => first_line(&first.asked, 80),
        t => t.to_string(),
    };
    let skip_from = 1.max(turns.len().saturating_sub(ASKED_LATEST));
    let mut body = String::from("Asked:\n");
    let turn = |t: &Turn, body: &mut String| {
        body.push_str(&format!("- {}\n", first_line(&t.asked, ASKED_MAX)));
        if let Some(lead) = reply_lead(&t.reply) {
            body.push_str(&format!("  → {lead}\n"));
        }
    };
    turn(first, &mut body);
    if skip_from > 1 {
        body.push_str(&format!("- …{} more\n", skip_from - 1));
    }
    for t in &turns[skip_from..] {
        turn(t, &mut body);
    }
    if !commits.is_empty() {
        body.push_str("Commits:\n");
        let skip = commits.len().saturating_sub(COMMITS_KEPT);
        if skip > 0 {
            body.push_str(&format!("- …{skip} earlier\n"));
        }
        for c in &commits[skip..] {
            body.push_str(&format!("- {}\n", first_line(c, ASKED_MAX)));
        }
    }
    let files = files.iter().take(FILES_KEPT).cloned().collect();
    Some(Draft { title, kind: "session".into(), body: body.trim_end().to_string(), topic: Some(format!("session/{key}")), files })
}

fn git(dir: &Path, args: &[&str]) -> Option<Vec<u8>> {
    crate::platform::command("git")
        .args(args)
        .current_dir(dir)
        .stderr(std::process::Stdio::null())
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| o.stdout)
}

/// Commits on the checkout at `top` since `since`, oldest first, as
/// "hash subject".
fn commits_since(top: &Path, since: u64) -> Vec<String> {
    let Some(log) = git(top, &["log", &format!("--since=@{since}"), "--reverse", "--format=%h %s"]) else {
        return Vec::new();
    };
    String::from_utf8_lossy(&log).lines().map(str::trim).filter(|l| !l.is_empty()).map(String::from).collect()
}

/// Files of the checkout at `top` changed since `since`: committed since then,
/// or uncommitted and written since then. Agents often edit through the shell,
/// which their edit tools never see.
pub(crate) fn changed_since(top: &Path, since: u64) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut add = |p: &str| {
        if !p.is_empty() && !out.iter().any(|o| o == p) {
            out.push(p.to_string());
        }
    };
    if let Some(log) = git(top, &["log", &format!("--since=@{since}"), "--name-only", "--format="]) {
        String::from_utf8_lossy(&log).lines().for_each(|l| add(l.trim()));
    }
    if let Some(status) = git(top, &["status", "--porcelain=v1", "-z", "--untracked-files=all"]) {
        let status = String::from_utf8_lossy(&status);
        let mut fields = status.split('\0');
        while let Some(f) = fields.next() {
            if f.len() < 4 {
                continue;
            }
            let (code, path) = f.split_at(3);
            if code.starts_with('R') || code.starts_with('C') {
                fields.next();
            }
            let written = std::fs::metadata(top.join(path))
                .and_then(|m| m.modified())
                .ok()
                .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                .is_some_and(|d| d.as_secs() >= since);
            if written {
                add(path);
            }
        }
    }
    out
}

/// Saves the record of the session `key` working in `dir` since `since`,
/// unless memory is off. `edited` are the files its edit tools changed; those
/// outside the project (scratch files in /tmp) are left out.
pub fn record_session(
    dir: &Path,
    key: &str,
    title: &str,
    turns: &[Turn],
    edited: &[PathBuf],
    since: u64,
) -> std::io::Result<()> {
    if std::env::var_os("ARAKUN_MEMORY").is_some_and(|v| v == "off") {
        return Ok(());
    }
    let project = project_of(dir);
    let top = git(dir, &["rev-parse", "--show-toplevel"])
        .map(|o| PathBuf::from(String::from_utf8_lossy(&o).trim()))
        .unwrap_or_else(|| project.clone());
    let mut files: Vec<String> = edited
        .iter()
        .filter_map(|f| {
            if f.is_relative() {
                return Some(f.display().to_string());
            }
            f.strip_prefix(&top).or_else(|_| f.strip_prefix(&project)).ok().map(|p| p.display().to_string())
        })
        .collect();
    for f in changed_since(&top, since) {
        if !files.contains(&f) {
            files.push(f);
        }
    }
    let commits = commits_since(&top, since);
    let path = project.display().to_string();
    update(&store_file(&root(), Scope::Project, &project), &path, |s| {
        // A file changed in an earlier turn and committed since still counts.
        if let Some(old) = s.items.iter().find(|m| m.topic.as_deref() == Some(&format!("session/{key}"))) {
            let mut merged: Vec<String> = old.files.iter().filter(|f| !f.starts_with('/')).cloned().collect();
            for f in files.drain(..) {
                if !merged.contains(&f) {
                    merged.push(f);
                }
            }
            files = merged;
        }
        let Some(draft) = session_draft(key, title, turns, &commits, &files) else {
            return (false, ());
        };
        let r = save(s, draft, now());
        (!matches!(r, Saved::Same(_)), ())
    })
}
// ---- command line ---------------------------------------------------------

pub const USAGE: &str = "\
arakun mem save <title> [--kind K] [--body text | --body -] [--files a,b] [--topic key] [--global]
                                  remember something (--body - reads stdin; words after
                                  the title also count as the text)
arakun mem search <words…> [-n N]   find memories here and about the user (compact)
arakun mem get <id…>                the full text (12 is this project's, g3 a global one)
arakun mem context                  the index new sessions start with
arakun mem prompt                   how-to and index, for an agent arakun does not start
arakun mem list [--global]          everything saved, newest first
arakun mem forget <id…>             delete
kinds: decision bugfix gotcha convention preference discovery pattern config session note";

pub fn cli(args: &[String], cwd: &Path) -> i32 {
    match run(args, cwd, &root(), now()) {
        Ok(out) => {
            if !out.is_empty() {
                println!("{}", out.trim_end());
            }
            0
        }
        Err(e) => {
            eprintln!("arakun mem: {e}");
            if e.starts_with("usage") {
                2
            } else {
                1
            }
        }
    }
}

fn read_stdin() -> Result<String, String> {
    use std::io::Read as _;
    let mut s = String::new();
    std::io::stdin().read_to_string(&mut s).map_err(|e| e.to_string())?;
    Ok(s)
}

pub fn run(args: &[String], cwd: &Path, root: &Path, at: u64) -> Result<String, String> {
    let project = project_of(cwd);
    let name = project.file_name().map_or_else(|| project.display().to_string(), |n| n.to_string_lossy().to_string());
    let path = project.display().to_string();
    let file = |scope| store_file(root, scope, &project);
    let rest = &args[1.min(args.len())..];
    match args.first().map(String::as_str) {
        None | Some("help" | "-h" | "--help") => Ok(USAGE.into()),
        Some("save") => {
            let (mut title, mut words) = (None::<String>, Vec::new());
            let (mut kind, mut body, mut files, mut topic, mut scope) =
                ("note".to_string(), None::<String>, Vec::new(), None, Scope::Project);
            let mut it = rest.iter();
            while let Some(a) = it.next() {
                let mut val = || it.next().cloned().ok_or_else(|| format!("usage: {a} needs a value"));
                match a.as_str() {
                    "--kind" | "-k" => kind = val()?.to_lowercase(),
                    "--body" | "-b" => body = Some(val()?),
                    "--files" | "-f" => {
                        files = val()?.split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect()
                    }
                    "--topic" | "-t" => topic = Some(val()?).filter(|t| !t.trim().is_empty()),
                    "--global" | "-g" => scope = Scope::Global,
                    _ if title.is_none() => title = Some(a.clone()),
                    _ => words.push(a.clone()),
                }
            }
            let title = title.map(|t| t.trim().to_string()).filter(|t| !t.is_empty()).ok_or("usage: save <title> …")?;
            if !KINDS.contains(&kind.as_str()) {
                return Err(format!("unknown kind {kind}; one of: {}", KINDS.join(" ")));
            }
            let body = match body.as_deref() {
                Some("-") => read_stdin()?,
                Some(b) => b.to_string(),
                None => words.join(" "),
            };
            let draft = Draft { title, kind, body: body.trim().to_string(), topic, files };
            let saved = update(&file(scope), if scope == Scope::Global { "" } else { &path }, |s| {
                let r = save(s, draft, at);
                (!matches!(r, Saved::Same(_)), r)
            })
            .map_err(|e| e.to_string())?;
            let whose = if scope == Scope::Global { "about the user".to_string() } else { format!("in {name}") };
            Ok(match saved {
                Saved::New(id) => format!("saved #{}{id} {whose}", scope.prefix()),
                Saved::Updated(id) => format!("updated #{}{id} {whose}", scope.prefix()),
                Saved::Same(id) => format!("already saved as #{}{id}", scope.prefix()),
            })
        }
        Some("search") => {
            let (mut words, mut limit) = (Vec::new(), 10usize);
            let mut it = rest.iter();
            while let Some(a) = it.next() {
                if a == "-n" {
                    limit = it.next().and_then(|n| n.parse().ok()).filter(|n| *n > 0).ok_or("usage: -n N")?;
                } else {
                    words.push(a.as_str());
                }
            }
            if words.is_empty() {
                return Err("usage: search <words…>".into());
            }
            let query = words.join(" ");
            let (p, g) = (load(&file(Scope::Project)), load(&file(Scope::Global)));
            let mut hits: Vec<(f32, Scope, &Memory)> = search(&p.items, &query)
                .into_iter()
                .map(|(s, m)| (s, Scope::Project, m))
                .chain(search(&g.items, &query).into_iter().map(|(s, m)| (s, Scope::Global, m)))
                .collect();
            hits.sort_by(|a, b| b.0.total_cmp(&a.0));
            if hits.is_empty() {
                return Ok(format!("nothing about \"{query}\" in {name} or about the user"));
            }
            let total = hits.len();
            let mut out: Vec<String> = hits.iter().take(limit).map(|(_, s, m)| brief(*s, m, at)).collect();
            if total > limit {
                out.push(format!("…{} more (-n {total})", total - limit));
            }
            out.push("`arakun mem get <id>` shows one in full".into());
            Ok(out.join("\n"))
        }
        Some("get") if !rest.is_empty() => {
            let mut out = Vec::new();
            for raw in rest {
                let (scope, id) = parse_id(raw).ok_or_else(|| format!("usage: {raw} is not an id (12 or g3)"))?;
                let found = update(&file(scope), if scope == Scope::Global { "" } else { &path }, |s| {
                    match s.items.iter_mut().find(|m| m.id == id) {
                        Some(m) => {
                            m.hits += 1;
                            (true, Some(full(scope, m, at)))
                        }
                        None => (false, None),
                    }
                })
                .map_err(|e| e.to_string())?;
                out.push(found.unwrap_or_else(|| format!("#{}{id}: no such memory", scope.prefix())));
            }
            Ok(out.join("\n"))
        }
        Some("forget") if !rest.is_empty() => {
            let mut out = Vec::new();
            for raw in rest {
                let (scope, id) = parse_id(raw).ok_or_else(|| format!("usage: {raw} is not an id (12 or g3)"))?;
                let gone = update(&file(scope), if scope == Scope::Global { "" } else { &path }, |s| {
                    let before = s.items.len();
                    s.items.retain(|m| m.id != id);
                    (s.items.len() != before, s.items.len() != before)
                })
                .map_err(|e| e.to_string())?;
                out.push(if gone {
                    format!("forgot #{}{id}", scope.prefix())
                } else {
                    format!("#{}{id}: no such memory", scope.prefix())
                });
            }
            Ok(out.join("\n"))
        }
        Some("list") => {
            let scope = if rest.iter().any(|a| a == "--global" || a == "-g") { Scope::Global } else { Scope::Project };
            let mut s = load(&file(scope));
            s.items.sort_by_key(|m| std::cmp::Reverse(m.updated));
            if s.items.is_empty() {
                return Ok("nothing saved yet".into());
            }
            Ok(s.items.iter().map(|m| brief(scope, m, at)).collect::<Vec<_>>().join("\n"))
        }
        Some("context") => Ok(context(&load(&file(Scope::Project)), &load(&file(Scope::Global)), &name, at)),
        Some("prompt") => Ok(prompt(&load(&file(Scope::Project)), &load(&file(Scope::Global)), &name, at)),
        Some(other) => Err(format!("usage: unknown `{other}`\n{USAGE}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("arakun-mem-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn s(a: &[&str]) -> Vec<String> {
        a.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn words_fold_accents_and_split_identifiers() {
        assert_eq!(tokens("Configuración de Tests"), vec!["configuracion", "de", "test"]);
        let t = tokens("ARAKUN_CI parseJsonBody");
        assert!(t.contains(&"arakun".to_string()) && t.contains(&"ci".to_string()) && t.contains(&"arakunci".to_string()));
        assert!(t.contains(&"json".to_string()) && t.contains(&"parsejsonbody".to_string()));
    }

    #[test]
    fn search_ranks_the_matching_memory_first() {
        let mk = |id, title: &str, body: &str| Memory {
            id,
            title: title.into(),
            kind: "note".into(),
            body: body.into(),
            topic: None,
            files: vec![],
            created: 0,
            updated: id,
            hits: 0,
            revisions: 0,
        };
        let items = vec![
            mk(1, "Release build needs CEF", "Run tools/install-cef.sh before cargo build --release"),
            mk(2, "Tests must run with ARAKUN_CI=off", "Otherwise the CI poller hits the network"),
            mk(3, "Composer caret", "blinks like VS Code expand"),
        ];
        assert_eq!(search(&items, "ci tests network")[0].1.id, 2);
        assert_eq!(search(&items, "instalar cef release")[0].1.id, 1);
        // Prefix: "compos" finds "composer".
        assert_eq!(search(&items, "compos")[0].1.id, 3);
        assert!(search(&items, "zzz").is_empty());
    }

    #[test]
    fn saving_dedupes_and_topics_update() {
        let mut st = Store::default();
        let d = |title: &str, body: &str, topic: Option<&str>| Draft {
            title: title.into(),
            kind: "decision".into(),
            body: body.into(),
            topic: topic.map(Into::into),
            files: vec![],
        };
        assert_eq!(save(&mut st, d("Auth uses JWT", "short lived", Some("auth/model")), 1), Saved::New(1));
        assert_eq!(save(&mut st, d("Auth uses sessions", "moved off JWT", Some("auth/model")), 2), Saved::Updated(1));
        assert_eq!(st.items.len(), 1);
        assert_eq!(st.items[0].revisions, 1);
        assert_eq!(save(&mut st, d("Other", "x", None), 3), Saved::New(2));
        assert_eq!(save(&mut st, d("other", "x", None), 4), Saved::Same(2));
        for i in 0..40 {
            let draft = Draft { kind: "session".into(), ..d(&format!("s{i}"), "", None) };
            save(&mut st, draft, 10 + i);
        }
        assert_eq!(st.items.iter().filter(|m| m.kind == "session").count(), SESSIONS_KEPT);
        assert!(st.items.iter().any(|m| m.title == "s39") && !st.items.iter().any(|m| m.title == "s0"));
    }

    #[test]
    fn the_cli_saves_finds_and_forgets() {
        let root = tmp("cli");
        let proj = tmp("proj");
        let run = |a: &[&str]| run(&s(a), &proj, &root, 1_000_000);
        assert_eq!(
            run(&["save", "Tests need ARAKUN_CI=off", "--kind", "gotcha", "--files", "src/ci/mod.rs", "--body", "or it polls"]),
            Ok(format!("saved #1 in arakun-mem-proj-{}", std::process::id()))
        );
        assert_eq!(run(&["save", "Reply in Spanish", "-k", "preference", "-g", "the", "user", "asks"]).unwrap(), "saved #g1 about the user");
        assert!(run(&["save", "x", "--kind", "nope"]).is_err());
        assert!(run(&["save"]).unwrap_err().starts_with("usage"));
        let found = run(&["search", "ci", "polls"]).unwrap();
        assert!(found.starts_with("#1 [gotcha] Tests need ARAKUN_CI=off"), "{found}");
        let found = run(&["search", "spanish"]).unwrap();
        assert!(found.starts_with("#g1 [preference]"), "{found}");
        let got = run(&["get", "1"]).unwrap();
        assert!(got.contains("files src/ci/mod.rs") && got.contains("or it polls"), "{got}");
        assert_eq!(load(&store_file(&root, Scope::Project, &project_of(&proj))).items[0].hits, 1);
        let ctx = run(&["context"]).unwrap();
        assert!(ctx.contains("- #1 [gotcha] Tests need ARAKUN_CI=off (src/ci/mod.rs): or it polls"), "{ctx}");
        assert!(ctx.contains("About the user") && ctx.contains("#g1 [preference] Reply in Spanish: the user asks"), "{ctx}");
        run(&["save", "Added memory", "--kind", "session", "--body", "Goal: memory\nNext: benchmark"]).unwrap();
        let ctx = run(&["context"]).unwrap();
        assert!(ctx.starts_with("Last session here (just now): Added memory\n  Goal: memory\n  Next: benchmark"), "{ctx}");
        assert_eq!(run(&["forget", "1", "g9"]).unwrap(), "forgot #1\n#g9: no such memory");
        assert!(run(&["search", "polls"]).unwrap().starts_with("nothing"));
        let _ = std::fs::remove_dir_all(&root);
        let _ = std::fs::remove_dir_all(&proj);
    }

    #[test]
    fn the_index_keeps_to_its_budget() {
        let mut p = Store::default();
        for i in 0..400 {
            let d = Draft { title: format!("Memory number {i} about something"), kind: "note".into(), body: String::new(), topic: None, files: vec![] };
            save(&mut p, d, i);
        }
        let ctx = context(&p, &Store::default(), "x", 500);
        assert!(ctx.len() <= INDEX_BUDGET + 200, "{}", ctx.len());
        assert!(ctx.contains("more: `arakun mem search"));
    }

    #[test]
    fn arakun_records_sessions_by_itself() {
        let turns: Vec<Turn> = [
            ("Fix the login bug", "The token expired early.\n\nDetails follow."),
            ("", ""),
            ("now add a test", ""),
            ("and the docs", "```sh\nx\n```\n\n## Docs updated in README"),
            ("rename it", "<json-render>{}</json-render>"),
            ("commit", "Committed   as\nabc123."),
        ]
        .iter()
        .map(|(a, r)| Turn { asked: a.to_string(), reply: r.to_string() })
        .collect();
        let commits = vec!["abc123 Fix login".to_string()];
        let d = session_draft("s1", "New session", &turns, &commits, &["src/auth.rs".into()]).unwrap();
        assert_eq!(d.title, "Fix the login bug");
        assert_eq!(d.topic.as_deref(), Some("session/s1"));
        assert_eq!(
            d.body,
            "Asked:\n- Fix the login bug\n  → The token expired early.\n- …1 more\n- and the docs\n  → Docs updated in README\n\
             - rename it\n- commit\n  → Committed as abc123.\nCommits:\n- abc123 Fix login"
        );
        let blank = [Turn { asked: "  ".into(), reply: String::new() }];
        assert!(session_draft("s1", "x", &blank, &[], &[]).is_none());

        let mut p = Store::default();
        save(&mut p, session_draft("s1", "Login bug", &turns[..1], &[], &[]).unwrap(), 10);
        save(&mut p, session_draft("s2", "Docs", &turns[..1], &[], &[]).unwrap(), 20);
        // The next turn of s1 updates its record instead of adding another.
        save(&mut p, session_draft("s1", "Login bug", &turns, &commits, &["src/auth.rs".into()]).unwrap(), 30);
        assert_eq!(p.items.len(), 2);
        let ctx = context(&p, &Store::default(), "x", 40);
        assert!(ctx.starts_with("Last session here (just now): Login bug\n  Asked:\n  - Fix the login bug\n"), "{ctx}");
        assert!(ctx.contains("  Changed: src/auth.rs\nEarlier sessions here"), "{ctx}");
        assert!(ctx.contains("- #2 Docs · just now\n"), "{ctx}");
    }

    #[test]
    fn changes_since_the_session_started_count_however_they_were_made() {
        let repo = tmp("changed");
        let sh = |c: &str| {
            let ok = crate::git::unpinned(&mut std::process::Command::new("sh")).arg("-c").arg(c).current_dir(&repo).status().unwrap().success();
            assert!(ok, "{c}");
        };
        sh("export GIT_COMMITTER_DATE=2001-01-01T00:00:00 \
            && git init -q && git -c core.hooksPath=/dev/null -c user.name=t -c user.email=t@t commit -q --allow-empty -m root \
            && echo a > old.txt && git add old.txt && git -c core.hooksPath=/dev/null -c user.name=t -c user.email=t@t commit -q -m old \
            && echo dirty > before.txt && touch -t 200101010000 before.txt");
        let since = now();
        sh("echo b > new.txt && mkdir -p src && echo c > src/lib.rs && git add src && \
            git -c core.hooksPath=/dev/null -c user.name=t -c user.email=t@t commit -q -m lib && echo d >> old.txt");
        let mut got = changed_since(&repo, since);
        got.sort();
        assert_eq!(got, vec!["new.txt", "old.txt", "src/lib.rs"]);
        let commits = commits_since(&repo, since);
        assert_eq!(commits.len(), 1, "{commits:?}");
        assert!(commits[0].ends_with(" lib"), "{commits:?}");
        let _ = std::fs::remove_dir_all(&repo);
    }

    #[test]
    fn ids_name_their_store() {
        assert_eq!(parse_id("12"), Some((Scope::Project, 12)));
        assert_eq!(parse_id("#g3"), Some((Scope::Global, 3)));
        assert_eq!(parse_id("x"), None);
    }
}
