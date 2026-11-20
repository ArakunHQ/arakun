//! The explorer's way of asking PostHog: HogQL whose user input only ever
//! travels as bound values, pages of LIMIT/OFFSET, one hourly budget per
//! account that every tab spends from, and answers kept on disk by project
//! and query so the same view opens at once and costs nothing.
//!
//! PostHog fills `{name}` placeholders from `values` on its side, after it
//! parses the query, so a filter value can never change what the query does.

use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::{ApiError, Session};

/// PostHog's query budget for an account when it sends no headers that say.
pub const HOURLY_QUERIES: u64 = 120;
/// How long a filter waits for the next keystroke before it reads.
pub const DEBOUNCE: Duration = Duration::from_millis(450);
/// A cached answer this young is shown without asking PostHog again.
pub const FRESH: u64 = 15 * 60;
/// Cached answers older than this are deleted.
const KEEP: u64 = 7 * 86_400;

/// A HogQL query and the values bound to its placeholders.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Hogql {
    pub text: String,
    /// Sorted, so the same query always has the same cache key.
    pub values: BTreeMap<String, Value>,
}

impl Hogql {
    pub fn new(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            values: BTreeMap::new(),
        }
    }

    /// Binds `value` and returns the placeholder to write in its place.
    /// Names are generated, so input never reaches the query text.
    pub fn bind(&mut self, value: impl Into<Value>) -> String {
        let name = format!("v{}", self.values.len());
        self.values.insert(name.clone(), value.into());
        format!("{{{name}}}")
    }

    pub fn push(&mut self, sql: &str) {
        self.text.push_str(sql);
    }

    /// The body of a `/query/` call.
    pub fn body(&self) -> Value {
        let mut q = json!({ "kind": "HogQLQuery", "query": self.text });
        if !self.values.is_empty() {
            q["values"] = json!(self.values);
        }
        json!({ "query": q })
    }
}

/// `%text%` for ILIKE, with LIKE's own wildcards in `text` taken literally.
pub fn contains_pattern(text: &str) -> String {
    let mut out = String::from("%");
    for c in text.chars() {
        if matches!(c, '%' | '_' | '\\') {
            out.push('\\');
        }
        out.push(c);
    }
    out.push('%');
    out
}

/// One page of a list.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Paging {
    pub size: u32,
    pub offset: u32,
}

impl Paging {
    pub fn first(size: u32) -> Self {
        Self { size, offset: 0 }
    }

    pub fn next(self) -> Self {
        Self {
            offset: self.offset + self.size,
            ..self
        }
    }

    /// Asks for one row more than the page, to know whether another follows.
    pub fn clause(self) -> String {
        format!(" LIMIT {} OFFSET {}", self.size + 1, self.offset)
    }

    /// The page's rows, and whether there are more after them.
    pub fn split<T>(self, mut rows: Vec<T>) -> (Vec<T>, bool) {
        let more = rows.len() > self.size as usize;
        rows.truncate(self.size as usize);
        (rows, more)
    }
}

// ---------------------------------------------------------------------------
// The hourly budget.

/// What one account has spent in the last hour, and what PostHog last said.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Budget {
    /// Unix seconds of each query sent.
    spent: Vec<u64>,
    /// (remaining, limit, when) from PostHog's rate-limit headers.
    told: Option<(u64, u64, u64)>,
}

impl Budget {
    pub fn spend(&mut self, now: u64) {
        self.spent.retain(|t| now.saturating_sub(*t) < 3600);
        self.spent.push(now);
    }

    pub fn note(&mut self, remaining: Option<u64>, limit: Option<u64>, now: u64) {
        if let Some(r) = remaining {
            let limit = limit.or(self.told.map(|t| t.1)).unwrap_or(HOURLY_QUERIES);
            self.told = Some((r, limit, now));
        }
    }

    /// Queries left this hour: PostHog's count minus what went since it
    /// said so, or our own count against the default budget.
    pub fn left(&self, now: u64) -> u64 {
        let recent = |since: u64| {
            self.spent
                .iter()
                .filter(|t| **t > since && now.saturating_sub(**t) < 3600)
                .count() as u64
        };
        match self.told {
            Some((remaining, _, at)) if now.saturating_sub(at) < 3600 => {
                remaining.saturating_sub(recent(at))
            }
            _ => HOURLY_QUERIES.saturating_sub(recent(0)),
        }
    }
}

fn budgets() -> std::sync::MutexGuard<'static, HashMap<String, Budget>> {
    static B: OnceLock<Mutex<HashMap<String, Budget>>> = OnceLock::new();
    B.get_or_init(Default::default)
        .lock()
        .unwrap_or_else(|e| e.into_inner())
}

/// Counts one query against `session`'s hour.
pub fn spend(session: &str, now: u64) {
    budgets().entry(session.to_string()).or_default().spend(now);
}

pub fn note_headers(session: &str, remaining: Option<u64>, limit: Option<u64>, now: u64) {
    budgets()
        .entry(session.to_string())
        .or_default()
        .note(remaining, limit, now);
}

pub fn left(session: &str, now: u64) -> u64 {
    budgets()
        .get(session)
        .map_or(HOURLY_QUERIES, |b| b.left(now))
}

/// "2 queries · 116 left this hour", or "cached · …" when nothing was sent.
pub fn budget_line(spent: u32, left: u64) -> String {
    let what = match spent {
        0 => "cached".to_string(),
        1 => "1 query".to_string(),
        n => format!("{n} queries"),
    };
    format!("{what} · {left} left this hour")
}

// ---------------------------------------------------------------------------
// Answers on disk.

/// The rows of one query, and whether they cost a query.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Answer {
    pub rows: Vec<Value>,
    pub fetched_at: u64,
    /// 1 when PostHog was asked, 0 when it came from the cache.
    #[serde(skip)]
    pub spent: u32,
}

/// The file name of a query's answer: host, project, text and values hashed.
pub fn cache_key(host: &str, project: u64, q: &Hogql) -> String {
    let values = serde_json::to_string(&q.values).unwrap_or_default();
    // FNV-1a: stable across runs and builds, unlike the std hasher.
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for part in [host, &project.to_string(), &q.text, &values] {
        for b in part.bytes().chain(std::iter::once(0)) {
            h ^= u64::from(b);
            h = h.wrapping_mul(0x0100_0000_01b3);
        }
    }
    format!("{h:016x}")
}

fn dir() -> PathBuf {
    super::cache_dir().join("posthog").join("q")
}

fn read_cached(key: &str) -> Option<Answer> {
    crate::durable::load_json(dir().join(format!("{key}.json")))
}

/// Deletes answers no one has asked for in a week, once per run.
fn prune(now: u64) {
    static DONE: OnceLock<()> = OnceLock::new();
    if DONE.set(()).is_err() {
        return;
    }
    for e in std::fs::read_dir(dir()).into_iter().flatten().flatten() {
        let old = e
            .metadata()
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .is_some_and(|t| now.saturating_sub(t.as_secs()) > KEEP);
        if old {
            let _ = std::fs::remove_file(e.path());
        }
    }
}

/// Runs `q` in `project` as `s`: from the cache when it is younger than
/// `fresh` seconds, else from PostHog (one query from the budget).
pub fn run(s: &Session, project: u64, q: &Hogql, fresh: u64) -> Result<Answer, ApiError> {
    let now = crate::model::unix_now();
    let key = cache_key(&s.host, project, q);
    if let Some(a) = read_cached(&key).filter(|a| now.saturating_sub(a.fetched_at) < fresh) {
        return Ok(a);
    }
    spend(&s.id, now);
    let v = super::call(
        s,
        "POST",
        &format!("/api/projects/{project}/query/"),
        Some(&q.body()),
    )?;
    let a = Answer {
        rows: v["results"].as_array().cloned().unwrap_or_default(),
        fetched_at: now,
        spent: 1,
    };
    crate::durable::cache_json(&dir().join(format!("{key}.json")), &a);
    prune(now);
    Ok(a)
}

/// Turns stale replies away: each change of a filter takes a number, and only
/// the newest one's read may land.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Debounce {
    generation: u64,
}

impl Debounce {
    pub fn bump(&mut self) -> u64 {
        self.generation += 1;
        self.generation
    }

    pub fn is_current(&self, generation: u64) -> bool {
        self.generation == generation
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn values_never_reach_the_query_text() {
        let evil = "x' OR 1=1 --";
        let mut q = Hogql::new("SELECT 1 FROM events WHERE event = ");
        let p = q.bind(evil);
        q.push(&p);
        let p2 = q.bind("{v0}");
        q.push(&format!(" AND distinct_id = {p2}"));
        assert_eq!(
            q.text,
            "SELECT 1 FROM events WHERE event = {v0} AND distinct_id = {v1}"
        );
        assert!(!q.text.contains("OR 1=1"));
        let body = q.body();
        assert_eq!(body["query"]["kind"], "HogQLQuery");
        assert_eq!(body["query"]["values"]["v0"], evil);
        // A value that looks like a placeholder is still only a value.
        assert_eq!(body["query"]["values"]["v1"], "{v0}");
        // No values: no `values` key.
        assert!(
            Hogql::new("SELECT 1").body()["query"]
                .get("values")
                .is_none()
        );
    }

    #[test]
    fn like_patterns_take_wildcards_literally() {
        assert_eq!(contains_pattern("rik"), "%rik%");
        assert_eq!(contains_pattern("50%_a\\b"), "%50\\%\\_a\\\\b%");
    }

    #[test]
    fn paging_asks_one_more_to_know_the_rest() {
        let p = Paging::first(50);
        assert_eq!(p.clause(), " LIMIT 51 OFFSET 0");
        assert_eq!(p.next().clause(), " LIMIT 51 OFFSET 50");
        assert_eq!(p.next().next().offset, 100);
        let (rows, more) = p.split((0..51).collect::<Vec<_>>());
        assert_eq!((rows.len(), more), (50, true));
        let (rows, more) = p.split((0..7).collect::<Vec<_>>());
        assert_eq!((rows.len(), more), (7, false));
    }

    #[test]
    fn budget_counts_the_last_hour() {
        let mut b = Budget::default();
        assert_eq!(b.left(10_000), HOURLY_QUERIES);
        b.spend(10_000);
        b.spend(10_100);
        b.spend(10_200);
        assert_eq!(b.left(10_200), HOURLY_QUERIES - 3);
        // An hour later the first one no longer counts.
        assert_eq!(b.left(13_650), HOURLY_QUERIES - 2);
        // PostHog's own count wins, minus what went after it said so.
        b.note(Some(40), Some(240), 10_200);
        b.spend(10_300);
        assert_eq!(b.left(10_300), 39);
        // Headers without a remaining count change nothing.
        b.note(None, Some(10), 10_400);
        assert_eq!(b.left(10_400), 39);
        // An old header is forgotten.
        assert_eq!(b.left(10_200 + 3600), HOURLY_QUERIES - 1);
        assert_eq!(budget_line(1, 117), "1 query · 117 left this hour");
        assert_eq!(budget_line(3, 5), "3 queries · 5 left this hour");
        assert_eq!(budget_line(0, 5), "cached · 5 left this hour");
    }

    #[test]
    fn cache_keys_follow_project_query_and_values() {
        let mut a = Hogql::new("SELECT 1 WHERE x = ");
        let p = a.bind("one");
        a.push(&p);
        let mut b = a.clone();
        b.values.insert("v0".into(), json!("two"));
        let host = "https://us.posthog.com";
        assert_eq!(cache_key(host, 1, &a), cache_key(host, 1, &a.clone()));
        assert_ne!(cache_key(host, 1, &a), cache_key(host, 2, &a));
        assert_ne!(
            cache_key(host, 1, &a),
            cache_key("https://eu.posthog.com", 1, &a)
        );
        assert_ne!(cache_key(host, 1, &a), cache_key(host, 1, &b));
        assert_eq!(cache_key(host, 1, &a).len(), 16);
        // Parts can't run into each other.
        assert_ne!(
            cache_key(host, 1, &Hogql::new("1")),
            cache_key(host, 11, &Hogql::new(""))
        );
    }

    #[test]
    fn debounce_keeps_only_the_newest() {
        let mut d = Debounce::default();
        let a = d.bump();
        let b = d.bump();
        assert!(!d.is_current(a));
        assert!(d.is_current(b));
    }
}
