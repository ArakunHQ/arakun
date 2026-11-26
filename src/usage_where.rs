//! Where a Claude Code session's tokens go, from its transcript: what loads
//! with every message (skills, MCP servers, CLAUDE.md files, hook output), what
//! the session called, how full its context got and which tool results filled
//! it. [`report`] ranks what to fix first.
//!
//! Most of a bill is input, and most input is the setup each message carries
//! again. Claude Code writes that setup into the transcript as attachments
//! (`skill_listing`, `mcp_instructions_delta`, `deferred_tools_delta`,
//! `instructions`, `hook_additional_context`) and the tool list as an
//! `api-request-shape` line, so their sizes can be measured, not guessed.
//!
//! Every number is an estimate: about four bytes a token. Droid and Devin
//! don't log this detail, so only Claude Code is read. Files are read only:
//! nothing is sent anywhere and the setup is never changed.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::io::{BufRead as _, BufReader};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::SystemTime;

use serde::Serialize;
use serde_json::Value;

use crate::agent::ledger::{ToolUse, tool_key};

/// About four bytes a token, as `arakun usage tools` counts.
fn tokens(bytes: u64) -> u64 {
    bytes.div_ceil(4)
}

/// A piece of setup that loads with every message.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    Skill,
    /// An MCP server: its instructions and tools.
    Server,
    /// A CLAUDE.md or a file it imports.
    File,
    /// What a hook added to the session.
    Hook,
}

/// What one transcript says about its session.
#[derive(Clone, Debug, Default, PartialEq)]
struct Session {
    id: String,
    cwd: PathBuf,
    title: String,
    model: String,
    /// Unix seconds of the first response.
    at: u64,
    responses: u64,
    /// Context tokens of the first response: what the session sent before
    /// the task got going.
    first: u64,
    /// The most context tokens one response read.
    peak: u64,
    /// Context tokens of every response together.
    context: u64,
    /// Bytes of each piece of setup that loaded, keyed by kind and name.
    loaded: BTreeMap<(Kind, String), u64>,
    /// Calls to each skill or server.
    used: BTreeMap<(Kind, String), u64>,
    /// Bytes each tool's results put in the context, by [`tool_key`].
    tools: BTreeMap<String, ToolUse>,
}

impl Session {
    fn load(&mut self, kind: Kind, name: &str, bytes: u64) {
        let slot = self.loaded.entry((kind, name.to_string())).or_default();
        // A listing sent again (after a compaction) replaces the first.
        *slot = (*slot).max(bytes);
    }

    fn add(&mut self, kind: Kind, name: &str, bytes: u64) {
        *self.loaded.entry((kind, name.to_string())).or_default() += bytes;
    }

    fn call(&mut self, kind: Kind, name: &str) {
        *self.used.entry((kind, name.to_string())).or_default() += 1;
    }
}

/// How Claude Code names a server's tools: `mcp__<server>__<tool>`, with
/// what is not a letter, digit, `_` or `-` turned into `_`.
fn server_key(name: &str) -> String {
    name.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

/// The server of an `mcp__<server>__<tool>` name.
fn tool_server(tool: &str) -> Option<&str> {
    tool.strip_prefix("mcp__")?.split("__").next()
}

/// The plugin a skill or server comes with: `plugin:skill` for a skill,
/// `plugin:<plugin>:<server>` for a server.
fn plugin_of(kind: &Kind, name: &str) -> Option<String> {
    match kind {
        Kind::Skill => name.split_once(':').map(|(p, _)| p.to_string()),
        Kind::Server => {
            let rest = name.strip_prefix("plugin:")?;
            rest.split_once(':').map(|(p, _)| p.to_string())
        }
        Kind::File | Kind::Hook => None,
    }
}

/// The skills of a `skill_listing`: one `- name: description` line each,
/// with the bytes the line costs. Names alone (`- name`) are listed too.
fn listed_skills(listing: &str) -> Vec<(String, u64)> {
    let mut out: Vec<(String, u64)> = Vec::new();
    for line in listing.lines() {
        if let Some(entry) = line.strip_prefix("- ") {
            let name = entry.split_once(": ").map_or(entry, |(n, _)| n).trim();
            if !name.is_empty() && !name.contains(' ') {
                out.push((name.to_string(), line.len() as u64 + 1));
                continue;
            }
        }
        // A description that runs on to the next line belongs to the last skill.
        if let Some(last) = out.last_mut() {
            last.1 += line.len() as u64 + 1;
        }
    }
    out
}

/// The bytes a tool result puts in the context. An image costs about what
/// 1500 tokens of text do.
fn result_bytes(content: &Value) -> u64 {
    match content {
        Value::String(s) => s.len() as u64,
        Value::Array(blocks) => blocks
            .iter()
            .map(|b| match b["type"].as_str() {
                Some("text") => b["text"].as_str().map_or(0, |t| t.len() as u64),
                Some("image") => 6000,
                _ => b.to_string().len() as u64,
            })
            .sum(),
        _ => 0,
    }
}

/// Reads one transcript. Lines are checked for a marker before they are
/// parsed: most of a transcript is lines the report does not need.
fn read_session(path: &Path) -> Option<Session> {
    let file = std::fs::File::open(path).ok()?;
    let mut s = Session::default();
    // Tool call id → the key its result counts under.
    let mut calls: HashMap<String, String> = HashMap::new();
    let mut seen: HashSet<String> = HashSet::new();
    let (mut custom, mut ai) = (None, None);
    let mut buf = Vec::new();
    let mut reader = BufReader::new(file);
    loop {
        buf.clear();
        match reader.read_until(b'\n', &mut buf) {
            Ok(0) | Err(_) => break,
            Ok(_) => {}
        }
        let Ok(line) = std::str::from_utf8(&buf) else {
            continue;
        };
        let line = line.trim_end();
        let wanted = line.contains("\"usage\"")
            || line.contains("\"tool_use\"")
            || line.contains("\"tool_result\"")
            || line.contains("<command-name>")
            || line.contains("\"api-request-shape\"")
            || line.contains("-title\"")
            || (line.contains("\"attachment\"")
                && [
                    "skill_listing",
                    "mcp_instructions_delta",
                    "deferred_tools_delta",
                    "\"instructions\"",
                    "hook_additional_context",
                ]
                .iter()
                .any(|t| line.contains(t)));
        if !wanted {
            continue;
        }
        let Ok(v) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        if s.id.is_empty()
            && let Some(id) = v["sessionId"].as_str()
        {
            s.id = id.to_string();
        }
        if s.cwd.as_os_str().is_empty()
            && let Some(cwd) = v["cwd"].as_str()
        {
            s.cwd = PathBuf::from(cwd);
        }
        match v["type"].as_str().unwrap_or_default() {
            "custom-title" => custom = v["customTitle"].as_str().map(str::to_string),
            "ai-title" => ai = v["aiTitle"].as_str().map(str::to_string),
            "api-request-shape" => {
                for t in v["shape"]["tools"].as_array().into_iter().flatten() {
                    let name = t["name"].as_str().unwrap_or_default();
                    if let Some(server) = tool_server(name)
                        && t["defer_loading"].as_bool() != Some(true)
                    {
                        s.load(
                            Kind::Server,
                            &format!("{server}\u{0}{name}"),
                            t.to_string().len() as u64,
                        );
                    }
                }
            }
            "attachment" => read_attachment(&mut s, &v["attachment"]),
            "assistant" => {
                let m = &v["message"];
                for b in m["content"].as_array().into_iter().flatten() {
                    if b["type"].as_str() != Some("tool_use") {
                        continue;
                    }
                    let name = b["name"].as_str().unwrap_or_default();
                    let id = b["id"].as_str().unwrap_or_default();
                    let key = if name == "Bash" {
                        tool_key("bash", b["input"]["command"].as_str())
                    } else {
                        name.to_string()
                    };
                    calls.insert(id.to_string(), key);
                    if name == "Skill"
                        && let Some(skill) = b["input"]["skill"].as_str()
                    {
                        s.call(Kind::Skill, skill);
                    } else if let Some(server) = tool_server(name) {
                        s.call(Kind::Server, server);
                    }
                }
                let u = &m["usage"];
                let model = m["model"].as_str().unwrap_or_default();
                if !u.is_object() || model.is_empty() || model.starts_with('<') {
                    continue;
                }
                let key = format!(
                    "{}:{}",
                    m["id"].as_str().unwrap_or_default(),
                    v["requestId"].as_str().unwrap_or_default()
                );
                if !seen.insert(key) {
                    continue;
                }
                let n = |k: &str| u[k].as_u64().unwrap_or(0);
                let context = n("input_tokens")
                    + n("cache_read_input_tokens")
                    + n("cache_creation_input_tokens");
                if s.responses == 0 {
                    s.first = context;
                    s.at = v["timestamp"]
                        .as_str()
                        .and_then(crate::account::parse_rfc3339)
                        .unwrap_or(0);
                }
                s.responses += 1;
                s.peak = s.peak.max(context);
                s.context += context;
                s.model = model.to_string();
            }
            "user" => {
                for b in v["message"]["content"].as_array().into_iter().flatten() {
                    match b["type"].as_str() {
                        Some("tool_result") => {
                            let id = b["tool_use_id"].as_str().unwrap_or_default();
                            let key = calls.get(id).cloned().unwrap_or_else(|| "other".into());
                            s.tools.entry(key).or_default().add(ToolUse {
                                calls: 1,
                                bytes: result_bytes(&b["content"]),
                            });
                        }
                        Some("text") => slash_skills(&mut s, b["text"].as_str()),
                        _ => {}
                    }
                }
                slash_skills(&mut s, v["message"]["content"].as_str());
            }
            _ => {}
        }
    }
    if s.responses == 0 {
        return None;
    }
    s.title = custom.or(ai).unwrap_or_default();
    Some(s)
}

/// A skill run as a slash command (`/ship-pr`) is a use too.
fn slash_skills(s: &mut Session, text: Option<&str>) {
    let Some(text) = text else { return };
    for part in text.split("<command-name>").skip(1) {
        if let Some((name, _)) = part.split_once("</command-name>") {
            // Built-in commands (`/model`) count too, but match no skill.
            s.call(Kind::Skill, name.trim().trim_start_matches('/'));
        }
    }
}

fn read_attachment(s: &mut Session, a: &Value) {
    match a["type"].as_str().unwrap_or_default() {
        "skill_listing" => {
            for (name, bytes) in listed_skills(a["content"].as_str().unwrap_or_default()) {
                s.load(Kind::Skill, &name, bytes);
            }
        }
        "mcp_instructions_delta" => {
            let names = a["addedNames"].as_array().into_iter().flatten();
            let blocks = a["addedBlocks"].as_array().into_iter().flatten();
            for (name, block) in names.zip(blocks) {
                let (Some(name), Some(block)) = (name.as_str(), block.as_str()) else {
                    continue;
                };
                s.load(Kind::Server, name, block.len() as u64);
            }
        }
        "deferred_tools_delta" => {
            // A deferred tool costs its name in the list until it is loaded.
            for name in a["addedNames"].as_array().into_iter().flatten() {
                let Some(name) = name.as_str() else { continue };
                if let Some(server) = tool_server(name) {
                    s.load(
                        Kind::Server,
                        &format!("{server}\u{0}{name}"),
                        name.len() as u64 + 2,
                    );
                }
            }
        }
        "instructions" => {
            for f in a["files"].as_array().into_iter().flatten() {
                let (Some(path), Some(content)) = (f["path"].as_str(), f["content"].as_str())
                else {
                    continue;
                };
                s.load(Kind::File, path, content.len() as u64);
            }
        }
        "hook_additional_context" => {
            let bytes = match &a["content"] {
                Value::Array(parts) => parts
                    .iter()
                    .filter_map(Value::as_str)
                    .map(|p| p.len() as u64)
                    .sum(),
                Value::String(p) => p.len() as u64,
                _ => 0,
            };
            s.add(Kind::Hook, "hooks", bytes);
        }
        _ => {}
    }
}

/// A server's instructions are keyed by its name (`plugin:linear:linear`),
/// its tools by `<server key>\0<tool>`. Folds the tools into the server, so
/// one entry holds what the server costs.
fn fold_servers(s: &mut Session) {
    let names: HashMap<String, String> = s
        .loaded
        .keys()
        .filter(|(k, n)| *k == Kind::Server && !n.contains('\u{0}'))
        .map(|(_, n)| (server_key(n), n.clone()))
        .collect();
    let tools: Vec<((Kind, String), u64)> = s
        .loaded
        .iter()
        .filter(|((k, n), _)| *k == Kind::Server && n.contains('\u{0}'))
        .map(|(k, b)| (k.clone(), *b))
        .collect();
    for ((kind, name), bytes) in tools {
        s.loaded.remove(&(kind, name.clone()));
        let key = name.split('\u{0}').next().unwrap_or_default();
        let server = names.get(key).cloned().unwrap_or_else(|| key.to_string());
        s.add(Kind::Server, &server, bytes);
    }
    let used: Vec<((Kind, String), u64)> = s
        .used
        .iter()
        .filter(|((k, _), _)| *k == Kind::Server)
        .map(|(k, n)| (k.clone(), *n))
        .collect();
    for ((kind, key), n) in used {
        if let Some(server) = names.get(&key)
            && *server != key
        {
            s.used.remove(&(kind.clone(), key));
            *s.used.entry((kind, server.clone())).or_default() += n;
        }
    }
}

/// The context window of a model, as far as the transcript lets one tell: it
/// does not record it. Claude 5 models have a million tokens, others 200k,
/// and a session that read more than 200k had the larger one.
fn window(model: &str, peak: u64) -> u64 {
    let large = ["opus", "sonnet", "fable"]
        .iter()
        .any(|m| model.contains(m))
        && !["-3-", "-4-", "-3", "-4"].iter().any(|v| model.contains(v));
    if large || peak > 200_000 {
        1_000_000
    } else {
        200_000
    }
}

/// One project's first messages.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Project {
    pub cwd: PathBuf,
    pub sessions: u64,
    /// The median first-message size, in tokens.
    pub first: u64,
}

/// A piece of setup across the scanned sessions.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Setup {
    pub kind: Kind,
    pub name: String,
    /// The plugin it comes with, if any.
    pub plugin: Option<String>,
    /// Tokens it adds to each message (the largest seen).
    pub tokens: u64,
    /// Sessions it loaded in.
    pub sessions: u64,
    /// Sessions that called it (skills and servers).
    pub used_in: u64,
    pub calls: u64,
    /// Tokens it took across the sessions: its size times their messages.
    pub total: u64,
}

impl Setup {
    /// Files and hooks are not called; only a skill or a server can go unused.
    pub fn unused(&self) -> bool {
        matches!(self.kind, Kind::Skill | Kind::Server) && self.calls == 0
    }
}

/// One session's peak context.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Peak {
    pub session: String,
    pub title: String,
    pub cwd: PathBuf,
    pub model: String,
    pub peak: u64,
    /// The context window it is measured against (an estimate).
    pub window: u64,
}

impl Peak {
    pub fn share(&self) -> f64 {
        self.peak as f64 / self.window.max(1) as f64
    }
}

/// A tool's results across the sessions.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Tool {
    pub tool: String,
    pub calls: u64,
    pub tokens: u64,
}

/// What a finding suggests opening. Arakun opens the place; the person
/// makes the change.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "snake_case", tag = "open", content = "path")]
pub enum Place {
    /// A file, such as a CLAUDE.md.
    File(PathBuf),
    /// Claude Code's plugins.
    Plugins,
    /// Claude Code's MCP servers.
    Servers,
    /// Claude Code's skills.
    Skills,
    Nothing,
}

/// One thing to fix, with what it costs.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Finding {
    pub title: String,
    pub suggestion: String,
    pub place: Place,
    /// Tokens it took across the scanned sessions.
    pub tokens: u64,
}

/// Where the tokens of the scanned sessions went.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct Report {
    pub sessions: u64,
    pub messages: u64,
    /// Every input token the sessions read.
    pub context: u64,
    pub projects: Vec<Project>,
    pub setup: Vec<Setup>,
    pub peaks: Vec<Peak>,
    /// Sessions whose context passed half its window.
    pub past_half: u64,
    pub tools: Vec<Tool>,
    pub findings: Vec<Finding>,
}

struct Cached {
    modified: Option<SystemTime>,
    len: u64,
    session: Option<Session>,
}

static CACHE: Mutex<Option<HashMap<PathBuf, Cached>>> = Mutex::new(None);

/// The sessions of Claude Code's transcripts. Subagents' transcripts are left
/// out: they run on their own context and repeat their parent's session id.
fn sessions(root: &Path) -> Vec<Session> {
    let mut files: Vec<PathBuf> = Vec::new();
    for dir in std::fs::read_dir(root).into_iter().flatten().flatten() {
        for f in std::fs::read_dir(dir.path())
            .into_iter()
            .flatten()
            .flatten()
        {
            let p = f.path();
            if p.extension().is_some_and(|x| x == "jsonl") {
                files.push(p);
            }
        }
    }
    let mut slot = CACHE.lock().unwrap();
    let cache = slot.get_or_insert_with(HashMap::new);
    let present: HashSet<&PathBuf> = files.iter().collect();
    cache.retain(|p, _| present.contains(p));
    for file in &files {
        let Ok(meta) = std::fs::metadata(file) else {
            continue;
        };
        let modified = meta.modified().ok();
        if cache
            .get(file)
            .is_some_and(|c| c.modified == modified && c.len == meta.len())
        {
            continue;
        }
        let session = read_session(file).map(|mut s| {
            fold_servers(&mut s);
            s
        });
        cache.insert(
            file.clone(),
            Cached {
                modified,
                len: meta.len(),
                session,
            },
        );
    }
    cache.values().filter_map(|c| c.session.clone()).collect()
}

/// Where the tokens of the Claude Code sessions since `since` (Unix seconds)
/// went. Blocking: run it off the main thread.
pub fn scan(since: u64) -> Report {
    let root = crate::platform::home().join(".claude/projects");
    let all = sessions(&root);
    report(all.iter().filter(|s| s.at >= since))
}

fn median(mut v: Vec<u64>) -> u64 {
    v.sort_unstable();
    v.get(v.len() / 2).copied().unwrap_or(0)
}

/// A finding is worth showing when it took at least this many tokens.
const WORTH: u64 = 100_000;
/// A file is worth splitting from this many tokens.
const LARGE_FILE: u64 = 2_500;

/// The main checkout of the repository `path` is in. A worktree's `.git` is
/// a file naming `<main>/.git/worktrees/<name>`, so each environment of one
/// project counts as that project, and its files as the main checkout's.
fn repo_of(path: &Path) -> Option<PathBuf> {
    for dir in path.ancestors() {
        let git = dir.join(".git");
        let Ok(meta) = std::fs::metadata(&git) else {
            continue;
        };
        if meta.is_dir() {
            return Some(dir.to_path_buf());
        }
        let text = std::fs::read_to_string(&git).ok()?;
        let gitdir = Path::new(text.strip_prefix("gitdir:")?.trim());
        let main = gitdir
            .ancestors()
            .find(|a| a.file_name().is_some_and(|n| n == ".git"))?
            .parent()?;
        return Some(main.to_path_buf());
    }
    None
}

/// Puts a path in a worktree in its main checkout, when it can tell.
struct Repos(HashMap<PathBuf, Option<PathBuf>>);

impl Repos {
    fn root(&mut self, dir: &Path) -> Option<PathBuf> {
        self.0
            .entry(dir.to_path_buf())
            .or_insert_with(|| repo_of(dir))
            .clone()
    }

    /// `path` as it is in the main checkout.
    fn main(&mut self, path: &Path) -> PathBuf {
        let Some(dir) = path.parent() else {
            return path.to_path_buf();
        };
        let Some(root) = self.root(dir) else {
            return path.to_path_buf();
        };
        // The worktree's own root: where `.git` is.
        let top = dir.ancestors().find(|a| a.join(".git").exists());
        match top.and_then(|t| path.strip_prefix(t).ok()) {
            Some(rel) => root.join(rel),
            None => path.to_path_buf(),
        }
    }
}

fn report<'a>(sessions: impl IntoIterator<Item = &'a Session>) -> Report {
    let mut r = Report::default();
    let mut repos = Repos(HashMap::new());
    let mut firsts: BTreeMap<PathBuf, Vec<u64>> = BTreeMap::new();
    let mut setup: BTreeMap<(Kind, String), Setup> = BTreeMap::new();
    let mut tools: BTreeMap<String, ToolUse> = BTreeMap::new();
    for s in sessions {
        r.sessions += 1;
        r.messages += s.responses;
        r.context += s.context;
        let project = repos.root(&s.cwd).unwrap_or_else(|| s.cwd.clone());
        firsts.entry(project).or_default().push(s.first);
        for ((kind, name), bytes) in &s.loaded {
            let name = &match kind {
                Kind::File => repos.main(Path::new(name)).to_string_lossy().into_owned(),
                _ => name.clone(),
            };
            let e = setup
                .entry((kind.clone(), name.clone()))
                .or_insert_with(|| Setup {
                    kind: kind.clone(),
                    name: name.clone(),
                    plugin: plugin_of(kind, name),
                    tokens: 0,
                    sessions: 0,
                    used_in: 0,
                    calls: 0,
                    total: 0,
                });
            e.tokens = e.tokens.max(tokens(*bytes));
            e.sessions += 1;
            e.total += tokens(*bytes) * s.responses;
            if let Some(n) = s.used.get(&(kind.clone(), name.clone())) {
                e.used_in += 1;
                e.calls += n;
            }
        }
        for (k, u) in &s.tools {
            tools.entry(k.clone()).or_default().add(*u);
        }
        let window = window(&s.model, s.peak);
        r.peaks.push(Peak {
            session: s.id.clone(),
            title: s.title.clone(),
            cwd: s.cwd.clone(),
            model: s.model.clone(),
            peak: s.peak,
            window,
        });
    }
    r.projects = firsts
        .into_iter()
        .map(|(cwd, v)| Project {
            cwd,
            sessions: v.len() as u64,
            first: median(v),
        })
        .collect();
    r.projects.sort_by_key(|a| std::cmp::Reverse(a.first));
    r.past_half = r.peaks.iter().filter(|p| p.share() > 0.5).count() as u64;
    r.peaks.sort_by_key(|a| std::cmp::Reverse(a.peak));
    r.setup = setup.into_values().collect();
    r.setup.sort_by_key(|a| std::cmp::Reverse(a.total));
    r.tools = tools
        .into_iter()
        .map(|(tool, u)| Tool {
            tool,
            calls: u.calls,
            tokens: u.tokens(),
        })
        .collect();
    r.tools.sort_by_key(|a| std::cmp::Reverse(a.tokens));
    r.findings = findings(&r);
    r
}

/// What to fix first: the setup nobody calls, the files that are too long,
/// the noisy tools and the sessions that run too long, largest first.
fn findings(r: &Report) -> Vec<Finding> {
    let mut out = Vec::new();
    // A plugin none of whose skills or servers were called.
    let mut plugins: BTreeMap<&str, (u64, u64, u64)> = BTreeMap::new();
    for s in &r.setup {
        if let Some(p) = &s.plugin {
            let e = plugins.entry(p).or_default();
            e.0 += s.total;
            e.1 += s.calls;
            e.2 += s.tokens;
        }
    }
    for (plugin, (total, calls, per)) in &plugins {
        if *calls == 0 {
            out.push(Finding {
                title: format!(
                    "The {plugin} plugin was never used (~{} tokens a message)",
                    count(*per)
                ),
                suggestion: format!("Turn off the {plugin} plugin if you don't need it."),
                place: Place::Plugins,
                tokens: *total,
            });
        }
    }
    // Of a plugin that is used, its unused servers can go on their own.
    for s in &r.setup {
        let plugin_unused = s
            .plugin
            .as_deref()
            .is_some_and(|p| plugins.get(p).is_some_and(|e| e.1 == 0));
        if s.kind == Kind::Server && s.unused() && !plugin_unused {
            out.push(Finding {
                title: format!(
                    "The {} server was never called (~{} tokens a message)",
                    s.name,
                    count(s.tokens)
                ),
                suggestion: format!("Turn off the {} server if you don't need it.", s.name),
                place: Place::Servers,
                tokens: s.total,
            });
        }
    }
    // Skills outside plugins, and unused ones of used plugins, as one finding.
    let skills: Vec<&Setup> = r
        .setup
        .iter()
        .filter(|s| {
            s.kind == Kind::Skill
                && s.unused()
                && !s
                    .plugin
                    .as_deref()
                    .is_some_and(|p| plugins.get(p).is_some_and(|e| e.1 == 0))
        })
        .collect();
    if !skills.is_empty() {
        let names: Vec<&str> = skills.iter().take(5).map(|s| s.name.as_str()).collect();
        let more = skills.len().saturating_sub(names.len());
        out.push(Finding {
            title: format!(
                "{} skills were never called (~{} tokens a message)",
                skills.len(),
                count(skills.iter().map(|s| s.tokens).sum())
            ),
            suggestion: format!(
                "Remove the ones you don't need: {}{}.",
                names.join(", "),
                if more > 0 {
                    format!(" and {more} more")
                } else {
                    String::new()
                }
            ),
            place: Place::Skills,
            tokens: skills.iter().map(|s| s.total).sum(),
        });
    }
    for s in &r.setup {
        match s.kind {
            Kind::File if s.tokens >= LARGE_FILE => out.push(Finding {
                title: format!(
                    "{} is ~{} tokens, sent with every message",
                    s.name,
                    count(s.tokens)
                ),
                suggestion: "Split it: keep what every task needs, and move the rest to files \
                             the agent reads when it needs them."
                    .into(),
                place: Place::File(PathBuf::from(&s.name)),
                tokens: s.total,
            }),
            Kind::Hook => out.push(Finding {
                title: format!(
                    "Hooks add ~{} tokens to each session's context",
                    count(s.tokens)
                ),
                suggestion: "Check which plugin's hook adds it, and turn that plugin off if \
                             you don't need it."
                    .into(),
                place: Place::Plugins,
                tokens: s.total,
            }),
            _ => {}
        }
    }
    let results: u64 = r.tools.iter().map(|t| t.tokens).sum();
    for t in r.tools.iter().take(3) {
        if t.tokens * 5 >= results {
            out.push(Finding {
                title: format!(
                    "{} results put ~{} tokens in the context ({} calls)",
                    t.tool,
                    count(t.tokens),
                    t.calls
                ),
                suggestion: if t.tool.starts_with("bash") {
                    "Ask for less output: quieter flags, a filter, or arakun shrink.".into()
                } else {
                    "Ask for less at a time: a narrower search or a range of lines.".into()
                },
                place: Place::Nothing,
                tokens: t.tokens,
            });
        }
    }
    if r.sessions > 0 && r.past_half * 5 >= r.sessions && r.past_half > 0 {
        out.push(Finding {
            title: format!(
                "{} of {} sessions used more than half their context",
                r.past_half, r.sessions
            ),
            suggestion: "Start a new session for a new task: every message reads the whole \
                         context again."
                .into(),
            place: Place::Nothing,
            tokens: r
                .peaks
                .iter()
                .filter(|p| p.share() > 0.5)
                .map(|p| p.peak)
                .sum(),
        });
    }
    out.retain(|f| f.tokens >= WORTH);
    out.sort_by_key(|a| std::cmp::Reverse(a.tokens));
    out
}

pub const USAGE: &str = "arakun usage where [--days N] [--json]";

const HELP: &str = "\
Where the tokens of Claude Code sessions go, from their transcripts on this
computer: what to fix first, then each project's first-message size, how full
sessions got and which tool results filled them. Setup is what loads with
every message: skills, MCP servers, CLAUDE.md files and hook output. Every
number is an estimate (about 4 bytes a token). Nothing is sent anywhere.
--days N counts the sessions of the last N days (30 by default; 0 for all);
--json prints the whole report as JSON.";

/// `arakun usage where`: reads the transcripts, so it needs no running Arakun.
pub fn cli(args: &[String]) -> i32 {
    let mut days: u64 = 30;
    let mut json = false;
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--json" => json = true,
            "--days" => match it.next().and_then(|n| n.parse().ok()) {
                Some(n) => days = n,
                None => {
                    eprintln!("usage: {USAGE}");
                    return 2;
                }
            },
            "-h" | "--help" => {
                println!("usage: {USAGE}\n\n{HELP}");
                return 0;
            }
            _ => {
                eprintln!("usage: {USAGE}");
                return 2;
            }
        }
    }
    let now = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    let since = if days == 0 {
        0
    } else {
        now.saturating_sub(days * 24 * 3600)
    };
    let r = scan(since);
    if json {
        println!("{}", serde_json::to_string_pretty(&r).unwrap_or_default());
    } else {
        print!("{}", text(&r, days));
    }
    0
}

/// Findings the terminal report lists; `--json` has them all.
const SHOWN: usize = 10;

/// The report for a terminal: findings first, then the numbers behind them.
fn text(r: &Report, days: u64) -> String {
    use std::fmt::Write as _;
    let mut out = String::new();
    let span = if days == 0 {
        String::new()
    } else {
        format!(" in the last {days} days")
    };
    if r.sessions == 0 {
        return format!("No Claude Code sessions{span}.\n");
    }
    let _ = writeln!(
        out,
        "{} Claude Code sessions{span}, {} messages, ~{} input tokens. All numbers are estimates.\n",
        r.sessions,
        r.messages,
        count(r.context)
    );
    if r.findings.is_empty() {
        out.push_str("Nothing stands out to fix.\n");
    } else {
        out.push_str("Fix first\n");
        for (i, f) in r.findings.iter().take(SHOWN).enumerate() {
            let _ = writeln!(
                out,
                "{:>2}. {}\n    ~{} tokens across these sessions. {}",
                i + 1,
                f.title,
                count(f.tokens),
                f.suggestion
            );
        }
        if r.findings.len() > SHOWN {
            let _ = writeln!(
                out,
                "    and {} smaller ones (--json lists them all)",
                r.findings.len() - SHOWN
            );
        }
    }
    out.push_str("\nFirst message, median by project\n");
    for p in r.projects.iter().take(5) {
        let _ = writeln!(
            out,
            "  {:>6}  {} ({} sessions)",
            count(p.first),
            p.cwd.display(),
            p.sessions
        );
    }
    let _ = writeln!(
        out,
        "\nHow full sessions got: {} of {} passed half their context window",
        r.past_half, r.sessions
    );
    for p in r.peaks.iter().take(3) {
        let name = if p.title.is_empty() {
            &p.session
        } else {
            &p.title
        };
        let _ = writeln!(
            out,
            "  {:>3.0}%  {} of {}  {name}",
            p.share() * 100.,
            count(p.peak),
            count(p.window)
        );
    }
    if !r.tools.is_empty() {
        out.push_str("\nNoisiest tool results\n");
        for t in r.tools.iter().take(8) {
            let _ = writeln!(
                out,
                "  {:>6}  {} ({} calls)",
                count(t.tokens),
                t.tool,
                t.calls
            );
        }
    }
    out
}

/// A token count for a sentence: `840`, `12k`, `3.4M`.
pub fn count(n: u64) -> String {
    match n {
        0..1_000 => n.to_string(),
        1_000..10_000 => format!("{:.1}k", n as f64 / 1e3).replace(".0k", "k"),
        10_000..1_000_000 => format!("{}k", n / 1_000),
        _ => format!("{:.1}M", n as f64 / 1e6).replace(".0M", "M"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A transcript as Claude Code writes it: setup attachments, a tool list,
    /// two responses that call a skill, a server and Bash, and their results.
    fn fixture() -> String {
        let lines = [
            r#"{"type":"attachment","sessionId":"s1","cwd":"/p","attachment":{"type":"skill_listing","content":"- ship-pr: Ship a pull request.\n- superpowers:brainstorming: Think first.\n- superpowers:tdd: Tests\n  first, then code.\n- loop"}}"#,
            r#"{"type":"attachment","sessionId":"s1","cwd":"/p","attachment":{"type":"mcp_instructions_delta","addedNames":["plugin:linear:linear","doop"],"addedBlocks":["linear:\nUse it for issues.","doop:\nA design canvas."]}}"#,
            r#"{"type":"attachment","sessionId":"s1","cwd":"/p","attachment":{"type":"deferred_tools_delta","addedNames":["mcp__plugin_linear_linear__get_issue","mcp__doop__get_canvas","WebFetch"]}}"#,
            r#"{"type":"attachment","sessionId":"s1","cwd":"/p","attachment":{"type":"instructions","files":[{"path":"/p/CLAUDE.md","type":"Project","content":"Be brief."}]}}"#,
            r#"{"type":"attachment","sessionId":"s1","cwd":"/p","attachment":{"type":"hook_additional_context","content":["You have superpowers."]}}"#,
            r#"{"type":"api-request-shape","sessionId":"s1","shape":{"tools":[{"name":"Bash"},{"name":"mcp__doop__set_frame","description":"Sets a frame."}]}}"#,
            r#"{"type":"ai-title","aiTitle":"Fix the build","sessionId":"s1"}"#,
            r#"{"type":"user","sessionId":"s1","cwd":"/p","message":{"role":"user","content":"<command-name>/ship-pr</command-name>"}}"#,
            r#"{"type":"assistant","timestamp":"2026-11-20T10:00:00Z","sessionId":"s1","requestId":"r1","cwd":"/p","message":{"id":"m1","model":"claude-opus-5-5","content":[{"type":"tool_use","id":"t1","name":"Bash","input":{"command":"cd /p && cargo test"}},{"type":"tool_use","id":"t2","name":"mcp__plugin_linear_linear__get_issue","input":{}}],"usage":{"input_tokens":10,"cache_creation_input_tokens":30000,"cache_read_input_tokens":0,"output_tokens":50}}}"#,
            r#"{"type":"assistant","timestamp":"2026-11-20T10:00:00Z","sessionId":"s1","requestId":"r1","cwd":"/p","message":{"id":"m1","model":"claude-opus-5-5","content":[],"usage":{"input_tokens":10,"cache_creation_input_tokens":30000,"cache_read_input_tokens":0,"output_tokens":50}}}"#,
            r#"{"type":"user","sessionId":"s1","cwd":"/p","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"t1","content":"0123456789abcdef"},{"type":"tool_result","tool_use_id":"t2","content":[{"type":"text","text":"issue"},{"type":"image","source":{}}]}]}}"#,
            r#"{"type":"assistant","timestamp":"2026-11-20T10:01:00Z","sessionId":"s1","requestId":"r2","cwd":"/p","message":{"id":"m2","model":"claude-opus-5-5","content":[{"type":"tool_use","id":"t3","name":"Skill","input":{"skill":"superpowers:brainstorming"}}],"usage":{"input_tokens":5,"cache_creation_input_tokens":100,"cache_read_input_tokens":600000,"output_tokens":20}}}"#,
            r#"{"type":"queue-operation","sessionId":"s1"}"#,
        ];
        lines.join("\n") + "\n"
    }

    fn read_fixture() -> Session {
        // Tests run at once: each read gets its own folder.
        static N: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("arakun-where-{}-{n}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("s1.jsonl");
        std::fs::write(&path, fixture()).unwrap();
        let mut s = read_session(&path).unwrap();
        fold_servers(&mut s);
        let _ = std::fs::remove_dir_all(&dir);
        s
    }

    #[test]
    fn a_transcript_says_what_loaded_and_what_was_called() {
        let s = read_fixture();
        assert_eq!(s.id, "s1");
        assert_eq!(s.title, "Fix the build");
        assert_eq!(s.responses, 2, "a repeated response counts once");
        assert_eq!(s.first, 30_010);
        assert_eq!(s.peak, 600_105);
        let loaded: Vec<(Kind, &str)> = s
            .loaded
            .keys()
            .map(|(k, n)| (k.clone(), n.as_str()))
            .collect();
        assert_eq!(
            loaded,
            [
                (Kind::Skill, "loop"),
                (Kind::Skill, "ship-pr"),
                (Kind::Skill, "superpowers:brainstorming"),
                (Kind::Skill, "superpowers:tdd"),
                (Kind::Server, "doop"),
                (Kind::Server, "plugin:linear:linear"),
                (Kind::File, "/p/CLAUDE.md"),
                (Kind::Hook, "hooks"),
            ]
        );
        // A description over two lines counts both.
        assert_eq!(
            s.loaded[&(Kind::Skill, "superpowers:tdd".into())],
            "- superpowers:tdd: Tests\n  first, then code.\n".len() as u64
        );
        // The server's instructions, its deferred tool's name, and the schema
        // of the tool that loads up front.
        let doop = "doop:\nA design canvas.".len()
            + "mcp__doop__get_canvas".len()
            + 2
            + r#"{"description":"Sets a frame.","name":"mcp__doop__set_frame"}"#.len();
        assert_eq!(s.loaded[&(Kind::Server, "doop".into())], doop as u64);
        let used: Vec<&str> = s.used.keys().map(|(_, n)| n.as_str()).collect();
        assert_eq!(
            used,
            [
                "ship-pr",
                "superpowers:brainstorming",
                "plugin:linear:linear"
            ]
        );
        assert_eq!(
            s.tools["bash cargo"],
            ToolUse {
                calls: 1,
                bytes: 16
            }
        );
        assert_eq!(
            s.tools["mcp__plugin_linear_linear__get_issue"],
            ToolUse {
                calls: 1,
                bytes: 6005
            }
        );
    }

    #[test]
    fn the_report_ranks_unused_setup_and_full_sessions() {
        let mut s = read_fixture();
        // Many messages, so the small fixture setup is worth a finding.
        s.responses = 100_000;
        let r = report([&s]);
        assert_eq!((r.sessions, r.past_half), (1, 1));
        assert_eq!(r.projects[0].first, 30_010);
        assert_eq!(r.peaks[0].window, 1_000_000);
        let doop = r.setup.iter().find(|x| x.name == "doop").unwrap();
        assert!(doop.unused() && doop.plugin.is_none());
        let linear = r
            .setup
            .iter()
            .find(|x| x.name == "plugin:linear:linear")
            .unwrap();
        assert_eq!(linear.plugin.as_deref(), Some("linear"));
        assert!(!linear.unused());
        let titles: Vec<&str> = r.findings.iter().map(|f| f.title.as_str()).collect();
        assert!(
            titles
                .iter()
                .any(|t| t.starts_with("The doop server was never called")),
            "{titles:?}"
        );
        assert!(
            titles
                .iter()
                .any(|t| t.starts_with("2 skills were never called")),
            "{titles:?}"
        );
        assert!(
            titles.iter().any(|t| t.starts_with("1 of 1 sessions")),
            "{titles:?}"
        );
        // superpowers was used through one skill: its plugin stays.
        assert!(
            !titles.iter().any(|t| t.contains("superpowers plugin")),
            "{titles:?}"
        );
        // Ranked largest first.
        assert!(r.findings.windows(2).all(|w| w[0].tokens >= w[1].tokens));
    }

    #[test]
    fn a_plugin_nothing_called_is_one_finding() {
        let mut s = Session {
            id: "s".into(),
            model: "claude-opus-5-5".into(),
            responses: 1_000,
            ..Default::default()
        };
        s.load(Kind::Skill, "posthog:a", 40_000);
        s.load(Kind::Skill, "posthog:b", 40_000);
        s.load(Kind::Server, "plugin:posthog:posthog", 40_000);
        let r = report([&s]);
        assert_eq!(r.findings.len(), 1, "{:?}", r.findings);
        assert_eq!(r.findings[0].place, Place::Plugins);
        assert!(
            r.findings[0]
                .title
                .starts_with("The posthog plugin was never used")
        );
        assert_eq!(r.findings[0].tokens, 30_000 * 1_000);
    }

    #[test]
    fn a_long_instructions_file_is_worth_splitting() {
        let mut s = Session {
            responses: 100,
            ..Default::default()
        };
        s.load(Kind::File, "/p/AGENTS.md", 40_000);
        s.load(Kind::File, "/p/CLAUDE.md", 40);
        let r = report([&s]);
        assert_eq!(r.findings.len(), 1, "{:?}", r.findings);
        assert_eq!(r.findings[0].place, Place::File("/p/AGENTS.md".into()));
    }

    #[test]
    fn a_worktree_counts_as_its_main_checkout() {
        let root = std::env::temp_dir().join(format!("arakun-where-repo-{}", std::process::id()));
        let main = root.join("main");
        let tree = root.join("env/fox");
        std::fs::create_dir_all(main.join(".git/worktrees/fox")).unwrap();
        std::fs::create_dir_all(tree.join("src")).unwrap();
        std::fs::write(
            tree.join(".git"),
            format!("gitdir: {}\n", main.join(".git/worktrees/fox").display()),
        )
        .unwrap();
        assert_eq!(repo_of(&tree.join("src")), Some(main.clone()));
        assert_eq!(repo_of(&main), Some(main.clone()));
        let mut repos = Repos(HashMap::new());
        assert_eq!(repos.main(&tree.join("AGENTS.md")), main.join("AGENTS.md"));
        assert_eq!(
            repos.main(&tree.join("src/CLAUDE.md")),
            main.join("src/CLAUDE.md")
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn windows_and_names() {
        assert_eq!(window("claude-opus-5-5", 10), 1_000_000);
        assert_eq!(window("claude-haiku-5-5", 10), 200_000);
        assert_eq!(window("claude-3-5-haiku", 300_000), 1_000_000);
        assert_eq!(server_key("plugin:linear:linear"), "plugin_linear_linear");
        assert_eq!(tool_server("mcp__doop__get_canvas"), Some("doop"));
        assert_eq!(plugin_of(&Kind::Server, "doop"), None);
        assert_eq!(count(840), "840");
        assert_eq!(count(1_000), "1k");
        assert_eq!(count(12_345), "12k");
        assert_eq!(count(3_400_000), "3.4M");
    }
}

#[cfg(test)]
mod real {
    /// `cargo test usage_where::real -- --ignored --nocapture`: this machine's report.
    #[test]
    #[ignore]
    fn real_report() {
        let t = std::time::Instant::now();
        let r = super::scan(0);
        println!(
            "{} sessions, {} messages in {:?}",
            r.sessions,
            r.messages,
            t.elapsed()
        );
        for f in &r.findings {
            println!(
                "{:>12}  {}\n              {}",
                f.tokens, f.title, f.suggestion
            );
        }
        for p in r.projects.iter().take(5) {
            println!(
                "first {:>8}  {}  ({} sessions)",
                p.first,
                p.cwd.display(),
                p.sessions
            );
        }
        for t in r.tools.iter().take(8) {
            println!("tool {:>10}  {} ({} calls)", t.tokens, t.tool, t.calls);
        }
    }
}
