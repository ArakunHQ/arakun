//! What the agents used, from their own logs rather than arakun's: Claude Code's
//! transcripts (`~/.claude/projects/**/*.jsonl`, one usage per API response) and
//! Droid's session settings (one running total per session, dated by its
//! transcript's last write), and Devin's SQLite response metrics. Sessions
//! run outside arakun count too, so the Usage tab has the whole history.
//!
//! Files are re-read only when they change; the first scan runs off the main
//! thread and takes a moment, the later ones only stat the files.

use std::collections::{HashMap, HashSet};
use std::io::{BufRead as _, BufReader};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::SystemTime;

use serde_json::Value;

use crate::agent::AgentKind;

/// What one API response (Claude) or one session (Droid) used.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Record {
    /// Unix seconds.
    pub at: u64,
    pub agent: Option<AgentKind>,
    pub session: String,
    pub cwd: PathBuf,
    /// The model id without a `-fast` suffix or date.
    pub model: String,
    pub fast: bool,
    pub input: u64,
    pub output: u64,
    pub cache_read: u64,
    pub cache_write_5m: u64,
    pub cache_write_1h: u64,
    /// Factory credits (Droid).
    pub credits: u64,
    /// The session's title as the agent named it, if it did.
    pub title: String,
}

impl Record {
    pub fn agent(&self) -> AgentKind {
        self.agent.unwrap_or(AgentKind::Claude)
    }

    pub fn cache_write(&self) -> u64 {
        self.cache_write_5m + self.cache_write_1h
    }

    /// Every token the model read or wrote.
    pub fn tokens(&self) -> u64 {
        self.input + self.output + self.cache_read + self.cache_write()
    }

    /// What the tokens cost at API prices, when the model has a known price.
    pub fn cost(&self) -> Option<f64> {
        let p = price(&self.model)?;
        let m = if self.fast { 2. } else { 1. } / 1e6;
        Some(
            m * (self.input as f64 * p.input
                + self.output as f64 * p.output
                + self.cache_read as f64 * p.cache_read
                + self.cache_write_5m as f64 * p.input * 1.25
                + self.cache_write_1h as f64 * p.input * 2.),
        )
    }

    /// What the cache reads saved against paying for that input in full.
    pub fn cache_savings(&self) -> f64 {
        let Some(p) = price(&self.model) else {
            return 0.;
        };
        let m = if self.fast { 2. } else { 1. } / 1e6;
        m * self.cache_read as f64 * (p.input - p.cache_read)
    }
}

/// API prices in USD per million tokens. Cache writes are 1.25× input (5 min)
/// and 2× (1 hour); fast mode doubles everything.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Price {
    pub input: f64,
    pub output: f64,
    pub cache_read: f64,
}

/// Anthropic's first-party prices, longest id first so `claude-opus-5-5`
/// is not taken for `claude-opus-5`.
const PRICES: &[(&str, Price)] = &[
    (
        "claude-fable-5-2",
        Price {
            input: 8.,
            output: 40.,
            cache_read: 0.2,
        },
    ),
    (
        "claude-mythos-5-2",
        Price {
            input: 8.,
            output: 40.,
            cache_read: 0.2,
        },
    ),
    (
        "claude-fable-5-1",
        Price {
            input: 10.,
            output: 50.,
            cache_read: 0.25,
        },
    ),
    (
        "claude-mythos-5-1",
        Price {
            input: 10.,
            output: 50.,
            cache_read: 0.25,
        },
    ),
    (
        "claude-fable-5",
        Price {
            input: 10.,
            output: 50.,
            cache_read: 1.,
        },
    ),
    (
        "claude-mythos-5",
        Price {
            input: 10.,
            output: 50.,
            cache_read: 1.,
        },
    ),
    (
        "claude-opus-5-5",
        Price {
            input: 4.,
            output: 20.,
            cache_read: 0.2,
        },
    ),
    (
        "claude-opus-5",
        Price {
            input: 5.,
            output: 25.,
            cache_read: 0.5,
        },
    ),
    (
        "claude-opus-4-8",
        Price {
            input: 5.,
            output: 25.,
            cache_read: 0.5,
        },
    ),
    (
        "claude-opus-4-7",
        Price {
            input: 5.,
            output: 25.,
            cache_read: 0.5,
        },
    ),
    (
        "claude-opus-4-6",
        Price {
            input: 5.,
            output: 25.,
            cache_read: 0.5,
        },
    ),
    (
        "claude-opus-4-5",
        Price {
            input: 5.,
            output: 25.,
            cache_read: 0.5,
        },
    ),
    (
        "claude-sonnet-5-5",
        Price {
            input: 2.,
            output: 10.,
            cache_read: 0.1,
        },
    ),
    (
        "claude-sonnet-5",
        Price {
            input: 2.,
            output: 10.,
            cache_read: 0.2,
        },
    ),
    (
        "claude-sonnet-4-6",
        Price {
            input: 3.,
            output: 15.,
            cache_read: 0.3,
        },
    ),
    (
        "claude-sonnet-4-5",
        Price {
            input: 3.,
            output: 15.,
            cache_read: 0.3,
        },
    ),
    (
        "claude-sonnet-4",
        Price {
            input: 3.,
            output: 15.,
            cache_read: 0.3,
        },
    ),
    (
        "claude-haiku-5-5",
        Price {
            input: 0.1,
            output: 0.5,
            cache_read: 0.01,
        },
    ),
    (
        "claude-haiku-4-5",
        Price {
            input: 1.,
            output: 5.,
            cache_read: 0.1,
        },
    ),
];

/// The price of a model id, a dated one (`claude-haiku-4-5-20251001`) included.
pub fn price(model: &str) -> Option<Price> {
    PRICES
        .iter()
        .find(|(id, _)| {
            model == *id
                || model
                    .strip_prefix(id)
                    .is_some_and(|rest| rest.starts_with('-'))
        })
        .map(|(_, p)| *p)
}

/// `claude-opus-5-5-fast` → (`claude-opus-5-5`, true).
fn split_fast(model: &str) -> (String, bool) {
    match model.strip_suffix("-fast") {
        Some(base) => (base.to_string(), true),
        None => (model.to_string(), false),
    }
}

struct Cached {
    modified: Option<SystemTime>,
    len: u64,
    wal: Option<(Option<SystemTime>, u64)>,
    records: Vec<Record>,
    /// Claude: the (message id, request id) of each record, for dropping the
    /// copies a resumed or forked transcript repeats.
    keys: Vec<String>,
}

static CACHE: Mutex<Option<HashMap<PathBuf, Cached>>> = Mutex::new(None);

fn home() -> PathBuf {
    crate::platform::home()
}

/// Everything both agents logged, oldest first. Blocking: run it off the main thread.
pub fn scan() -> Vec<Record> {
    let mut files: Vec<(PathBuf, AgentKind)> = Vec::new();
    let claude = home().join(".claude/projects");
    for dir in read_dir(&claude) {
        files.extend(
            jsonl_files(&dir)
                .into_iter()
                .map(|f| (f, AgentKind::Claude)),
        );
        // Subagent transcripts live one level down, next to the session's.
        for sub in read_dir(&dir) {
            for inner in read_dir(&sub)
                .into_iter()
                .chain(std::iter::once(sub.clone()))
            {
                files.extend(
                    jsonl_files(&inner)
                        .into_iter()
                        .map(|f| (f, AgentKind::Claude)),
                );
            }
        }
    }
    for dir in read_dir(&home().join(".factory/sessions")) {
        for f in read_dir(&dir) {
            if f.to_string_lossy().ends_with(".settings.json") {
                files.push((f, AgentKind::Droid));
            }
        }
    }
    let devin = devin_dir().join("sessions.db");
    if std::env::var("DEVIN_SESSION_STORE").as_deref() != Ok("files") && devin.is_file() {
        files.push((devin, AgentKind::Devin));
    }
    files.sort_by(|a, b| a.0.cmp(&b.0));
    files.dedup_by(|a, b| a.0 == b.0);

    let mut slot = CACHE.lock().unwrap();
    let cache = slot.get_or_insert_with(HashMap::new);
    let present: HashSet<&PathBuf> = files.iter().map(|(f, _)| f).collect();
    cache.retain(|p, _| present.contains(p));
    for (file, agent) in &files {
        let Ok(meta) = std::fs::metadata(file) else {
            continue;
        };
        let modified = meta.modified().ok();
        // Droid's total is dated by its transcript, which changes on its own.
        let stamp = match agent {
            AgentKind::Droid => droid_transcript(file)
                .and_then(|t| std::fs::metadata(t).ok()?.modified().ok())
                .or(modified),
            AgentKind::Claude
            | AgentKind::OpenCode
            | AgentKind::Codex
            | AgentKind::Devin
            | AgentKind::Cursor
            | AgentKind::WorkersAi => modified,
        };
        let wal = (*agent == AgentKind::Devin)
            .then(|| std::fs::metadata(file.with_extension("db-wal")).ok())
            .flatten()
            .map(|m| (m.modified().ok(), m.len()));
        if cache
            .get(file)
            .is_some_and(|c| c.modified == stamp && c.len == meta.len() && c.wal == wal)
        {
            continue;
        }
        let (records, keys) = match agent {
            AgentKind::Claude => read_claude(file),
            AgentKind::Droid => (read_droid(file, stamp).into_iter().collect(), Vec::new()),
            AgentKind::Devin => match read_devin(file) {
                Ok(records) => (records, Vec::new()),
                Err(e) => {
                    eprintln!("arakun: could not read Devin usage: {e}");
                    continue;
                }
            },
            // OpenCode keeps its history in a database arakun does not read, and
            // Codex's rollouts and Cursor's chats are not read yet.
            AgentKind::OpenCode | AgentKind::Codex | AgentKind::Cursor | AgentKind::WorkersAi => {
                (Vec::new(), Vec::new())
            }
        };
        cache.insert(
            file.clone(),
            Cached {
                modified: stamp,
                len: meta.len(),
                wal,
                records,
                keys,
            },
        );
    }
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for c in cache.values() {
        for (i, r) in c.records.iter().enumerate() {
            if let Some(key) = c.keys.get(i)
                && !seen.insert(key.clone())
            {
                continue;
            }
            out.push(r.clone());
        }
    }
    out.sort_by_key(|r| r.at);
    out
}

fn devin_dir() -> PathBuf {
    std::env::var_os("DEVIN_SESSIONS_DIR")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .unwrap_or_else(|| crate::platform::data_home().join("devin/cli"))
}

fn read_devin(path: &Path) -> rusqlite::Result<Vec<Record>> {
    let db = rusqlite::Connection::open_with_flags(
        path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    db.busy_timeout(std::time::Duration::from_millis(250))?;
    read_devin_db(&db)
}

fn read_devin_db(db: &rusqlite::Connection) -> rusqlite::Result<Vec<Record>> {
    let mut query = db.prepare(
        "WITH messages AS (
            SELECT session_id, node_id, created_at,
                CASE WHEN json_valid(chat_message) THEN chat_message ELSE '{}' END AS message
            FROM message_nodes
         )
         SELECT s.id, s.working_directory, COALESCE(s.title, ''),
            COALESCE(NULLIF(json_extract(m.message, '$.metadata.generation_model'), ''), s.model),
            m.created_at, json_extract(m.message, '$.metadata.created_at'),
            COALESCE(NULLIF(json_extract(m.message, '$.metadata.request_id'), ''),
                NULLIF(json_extract(m.message, '$.message_id'), ''), s.id || ':' || m.node_id),
            json_extract(m.message, '$.metadata.metrics')
         FROM messages m JOIN sessions s ON s.id = m.session_id
         WHERE json_extract(m.message, '$.role') = 'assistant'
            AND json_type(m.message, '$.metadata.metrics') = 'object'
         ORDER BY s.id, m.node_id",
    )?;
    let rows = query.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, String>(3)?,
            row.get::<_, i64>(4)?,
            row.get::<_, Option<String>>(5)?,
            row.get::<_, String>(6)?,
            row.get::<_, String>(7)?,
        ))
    })?;
    let mut seen = HashSet::new();
    let mut records = Vec::new();
    for row in rows {
        let (session, cwd, title, model, created, timestamp, key, metrics) = row?;
        let Ok(usage) = serde_json::from_str::<Value>(&metrics) else {
            continue;
        };
        let n = |key: &str| usage[key].as_u64().unwrap_or(0);
        if ![
            "input_tokens",
            "output_tokens",
            "cache_read_tokens",
            "cache_creation_tokens",
        ]
        .iter()
        .any(|key| n(key) > 0)
            || !seen.insert(key)
        {
            continue;
        }
        let (model, fast) = split_fast(&model);
        records.push(Record {
            at: timestamp
                .as_deref()
                .and_then(crate::account::parse_rfc3339)
                .unwrap_or(created.max(0) as u64),
            agent: Some(AgentKind::Devin),
            session,
            cwd: cwd.into(),
            model,
            fast,
            input: n("input_tokens"),
            output: n("output_tokens"),
            cache_read: n("cache_read_tokens"),
            cache_write_5m: n("cache_creation_tokens"),
            cache_write_1h: 0,
            credits: 0,
            title,
        });
    }
    Ok(records)
}

fn read_dir(dir: &Path) -> Vec<PathBuf> {
    std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .collect()
}

fn jsonl_files(dir: &Path) -> Vec<PathBuf> {
    read_dir(dir)
        .into_iter()
        .filter(|p| p.extension().is_some_and(|x| x == "jsonl"))
        .collect()
}

/// One record per API response. Claude Code writes a response once per content
/// block with the same usage; the message id keeps it to one.
fn read_claude(path: &Path) -> (Vec<Record>, Vec<String>) {
    let Ok(file) = std::fs::File::open(path) else {
        return Default::default();
    };
    let mut records = Vec::new();
    let mut keys = Vec::new();
    let mut seen = HashSet::new();
    // A title the user gave beats the one Claude Code wrote.
    let (mut custom, mut ai) = (None, None);
    for line in BufReader::new(file).lines() {
        let Ok(line) = line else { break };
        if line.starts_with("{\"type\":\"custom-title\"")
            || line.starts_with("{\"type\":\"ai-title\"")
        {
            if let Ok(v) = serde_json::from_str::<Value>(&line) {
                if let Some(t) = v["customTitle"].as_str() {
                    custom = Some(t.to_string());
                }
                if let Some(t) = v["aiTitle"].as_str() {
                    ai = Some(t.to_string());
                }
            }
            continue;
        }
        if !line.contains("\"usage\"") || !line.contains("\"assistant\"") {
            continue;
        }
        let Ok(v) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        if v["type"].as_str() != Some("assistant") {
            continue;
        }
        let m = &v["message"];
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
        if !seen.insert(key.clone()) {
            continue;
        }
        let n = |k: &str| u[k].as_u64().unwrap_or(0);
        let write = n("cache_creation_input_tokens");
        let write_1h = u["cache_creation"]["ephemeral_1h_input_tokens"]
            .as_u64()
            .unwrap_or(0)
            .min(write);
        let (model, fast) = split_fast(model);
        let Some(at) = v["timestamp"]
            .as_str()
            .and_then(crate::account::parse_rfc3339)
        else {
            continue;
        };
        records.push(Record {
            at,
            agent: Some(AgentKind::Claude),
            session: v["sessionId"].as_str().unwrap_or_default().to_string(),
            cwd: PathBuf::from(v["cwd"].as_str().unwrap_or_default()),
            model,
            fast: fast || u["speed"].as_str() == Some("fast"),
            input: n("input_tokens"),
            output: n("output_tokens"),
            cache_read: n("cache_read_input_tokens"),
            cache_write_5m: write - write_1h,
            cache_write_1h: write_1h,
            credits: 0,
            title: String::new(),
        });
        keys.push(key);
    }
    if let Some(title) = custom.or(ai) {
        for r in &mut records {
            r.title = title.clone();
        }
    }
    (records, keys)
}

fn droid_transcript(settings: &Path) -> Option<PathBuf> {
    let name = settings.to_str()?.strip_suffix(".settings.json")?;
    Some(PathBuf::from(format!("{name}.jsonl")))
}

/// A Droid session's running total, dated by its transcript's last write.
fn read_droid(path: &Path, stamp: Option<SystemTime>) -> Option<Record> {
    let v: Value = crate::durable::load_json(path)?;
    let u = &v["tokenUsage"];
    let n = |k: &str| u[k].as_u64().unwrap_or(0);
    let credits = n("factoryCredits");
    if credits == 0 && n("outputTokens") == 0 {
        return None;
    }
    let at = stamp?
        .duration_since(SystemTime::UNIX_EPOCH)
        .ok()?
        .as_secs();
    let (model, fast) = split_fast(v["model"].as_str().unwrap_or("unknown"));
    let transcript = droid_transcript(path);
    let (cwd, title) = transcript
        .as_deref()
        .and_then(droid_start)
        .unwrap_or_default();
    let session = path
        .file_name()?
        .to_str()?
        .strip_suffix(".settings.json")?
        .to_string();
    Some(Record {
        at,
        agent: Some(AgentKind::Droid),
        session,
        cwd,
        model,
        fast,
        input: n("inputTokens"),
        output: n("outputTokens") + n("thinkingTokens"),
        cache_read: n("cacheReadTokens"),
        cache_write_5m: n("cacheCreationTokens"),
        cache_write_1h: 0,
        credits,
        title,
    })
}

/// The `cwd` and title of the `session_start` line a Droid transcript opens with.
fn droid_start(transcript: &Path) -> Option<(PathBuf, String)> {
    let file = std::fs::File::open(transcript).ok()?;
    let mut first = String::new();
    BufReader::new(std::io::Read::take(file, 64 * 1024))
        .read_line(&mut first)
        .ok()?;
    let v: Value = serde_json::from_str(first.trim()).ok()?;
    let title = v["title"].as_str().unwrap_or_default().trim().to_string();
    Some((PathBuf::from(v["cwd"].as_str().unwrap_or_default()), title))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn devin_fixture() -> rusqlite::Connection {
        let db = rusqlite::Connection::open_in_memory().unwrap();
        db.execute_batch(
            "CREATE TABLE sessions (id TEXT, working_directory TEXT, model TEXT, title TEXT);
             CREATE TABLE message_nodes (session_id TEXT, node_id INTEGER, created_at INTEGER, chat_message TEXT);
             INSERT INTO sessions VALUES ('s', '/work', 'adaptive', 'Devin session');",
        )
        .unwrap();
        db
    }

    #[test]
    fn devin_history_counts_responses_once_without_reading_message_content() {
        let db = devin_fixture();
        let message = serde_json::json!({
            "role": "assistant", "message_id": "m", "content": "private",
            "metadata": {
                "request_id": "r", "generation_model": "claude-opus-5-5-medium",
                "created_at": "2026-10-25T10:00:00Z",
                "metrics": { "input_tokens": 10, "output_tokens": 5,
                    "cache_read_tokens": 100, "cache_creation_tokens": 40 }
            }
        });
        for node in 1..=2 {
            db.execute(
                "INSERT INTO message_nodes VALUES ('s', ?1, 1, ?2)",
                rusqlite::params![node, message.to_string()],
            )
            .unwrap();
        }
        db.execute(
            "INSERT INTO message_nodes VALUES ('s', 3, 1, 'not json')",
            [],
        )
        .unwrap();
        let records = read_devin_db(&db).unwrap();
        assert_eq!(records.len(), 1);
        let r = &records[0];
        assert_eq!(r.agent(), AgentKind::Devin);
        assert_eq!(
            (&*r.session, r.cwd.as_path(), &*r.title),
            ("s", Path::new("/work"), "Devin session")
        );
        assert_eq!(r.model, "claude-opus-5-5-medium");
        assert_eq!(r.at, 1792922400);
        assert_eq!(
            (r.input, r.output, r.cache_read, r.cache_write()),
            (10, 5, 100, 40)
        );
        assert!(r.cost().is_some());
    }

    #[test]
    fn devin_history_uses_message_ids_and_keeps_unknown_models_unpriced() {
        let db = devin_fixture();
        for (node, id) in [(1, "a"), (2, "b"), (3, "a")] {
            let message = serde_json::json!({
                "role": "assistant", "message_id": id,
                "metadata": { "metrics": { "input_tokens": 12, "output_tokens": 3 } }
            });
            db.execute(
                "INSERT INTO message_nodes VALUES ('s', ?1, 42, ?2)",
                rusqlite::params![node, message.to_string()],
            )
            .unwrap();
        }
        let records = read_devin_db(&db).unwrap();
        assert_eq!(records.len(), 2);
        assert!(
            records
                .iter()
                .all(|r| r.at == 42 && r.cost().is_none() && r.tokens() == 15)
        );
    }

    #[test]
    fn a_missing_devin_database_is_not_created() {
        let dir = std::env::temp_dir().join(format!("arakun-devin-missing-{}", std::process::id()));
        let path = dir.join("sessions.db");
        assert!(read_devin(&path).is_err());
        assert!(!dir.exists());
    }

    #[test]
    fn prices_match_the_longest_id() {
        assert_eq!(price("claude-opus-5-5").unwrap().input, 4.);
        assert_eq!(price("claude-opus-5").unwrap().input, 5.);
        assert_eq!(price("claude-haiku-4-5-20251001").unwrap().input, 1.);
        assert_eq!(price("claude-fable-5-1").unwrap().cache_read, 0.25);
        // Fable 5.2 is cheaper than Fable 5, which its id also starts with.
        for id in ["claude-fable-5-2", "claude-mythos-5-2"] {
            let p = price(id).unwrap();
            assert_eq!((p.input, p.output, p.cache_read), (8., 40., 0.2), "{id}");
        }
        assert_eq!(price("claude-sonnet-5-5").unwrap().cache_read, 0.1);
        assert!(price("kimi-k3").is_none());
        assert!(price("claude-opus-50").is_none());
        assert_eq!(
            split_fast("claude-opus-5-5-fast"),
            ("claude-opus-5-5".into(), true)
        );
    }

    #[test]
    fn cost_counts_cache_writes_and_fast_mode() {
        let r = Record {
            model: "claude-opus-5-5".into(),
            input: 1_000_000,
            output: 1_000_000,
            cache_read: 1_000_000,
            cache_write_5m: 1_000_000,
            cache_write_1h: 1_000_000,
            ..Default::default()
        };
        // 4 + 20 + 0.2 + 5 + 8
        assert!((r.cost().unwrap() - 37.2).abs() < 1e-9);
        assert!(
            (Record {
                fast: true,
                ..r.clone()
            }
            .cost()
            .unwrap()
                - 74.4)
                .abs()
                < 1e-9
        );
        assert!((r.cache_savings() - 3.8).abs() < 1e-9);
    }

    #[test]
    fn claude_transcripts_count_each_response_once() {
        let dir = std::env::temp_dir().join(format!("arakun-history-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("s.jsonl");
        let line = r#"{"type":"assistant","timestamp":"2026-10-25T10:00:00Z","sessionId":"s","requestId":"r1","cwd":"/p","message":{"id":"m1","model":"claude-opus-5-5","usage":{"input_tokens":10,"output_tokens":5,"cache_read_input_tokens":100,"cache_creation_input_tokens":40,"cache_creation":{"ephemeral_1h_input_tokens":30}}}}"#;
        std::fs::write(&path, format!("{line}\n{line}\n{{\"type\":\"user\"}}\n")).unwrap();
        let (records, _) = read_claude(&path);
        assert_eq!(records.len(), 1);
        let r = &records[0];
        assert_eq!(
            (
                r.input,
                r.output,
                r.cache_read,
                r.cache_write_5m,
                r.cache_write_1h
            ),
            (10, 5, 100, 10, 30)
        );
        assert_eq!(r.at, 1792922400);
        let _ = std::fs::remove_dir_all(&dir);
    }
}

#[cfg(test)]
mod real {
    #[test]
    #[ignore]
    fn real_devin_history() {
        let records = super::read_devin(&super::devin_dir().join("sessions.db")).unwrap();
        assert!(!records.is_empty());
        assert!(
            records
                .iter()
                .all(|r| r.agent() == crate::agent::AgentKind::Devin)
        );
        println!("{} Devin usage records", records.len());
    }

    /// `cargo test real_history -- --ignored --nocapture`: totals from this machine.
    #[test]
    #[ignore]
    fn real_history() {
        for pass in 0..2 {
            let t = std::time::Instant::now();
            let all = super::scan();
            let cost: f64 = all.iter().filter_map(|r| r.cost()).sum();
            let unpriced = all
                .iter()
                .filter(|r| r.cost().is_none())
                .map(|r| r.tokens())
                .sum::<u64>();
            let tokens = all.iter().map(|r| r.tokens()).sum::<u64>();
            println!(
                "pass {pass}: {} records in {:?}, ${cost:.2}, {tokens} tokens, {unpriced} unpriced",
                all.len(),
                t.elapsed()
            );
        }
    }
}
