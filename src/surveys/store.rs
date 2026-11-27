//! Fetches PostHog's surveys, keeps them on disk, remembers which ones a
//! person has seen, and picks the one popover to show.
//!
//! The picking (`eligible`) is a pure function of its inputs, so the rules are
//! tested without a clock, a network or files. The functions around it do the
//! IO, and take the folder as an argument so tests can use a temp one.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use super::ids::{self, Builtin, FallbackBody};
use super::model::{self, Body, Branching, Conditions, Question, Survey, SurveyType};

/// How often the surveys are fetched at most, and the longest a failed fetch
/// waits before the next try.
const REFETCH: Duration = Duration::from_secs(6 * 3600);
/// The wait after the first failed fetch. Each further failure doubles it.
const FIRST_RETRY: Duration = Duration::from_secs(60);
const FIRST_POPOVER_DELAY: Duration = Duration::from_secs(120);
const TYPING_QUIET: Duration = Duration::from_secs(10);
const DAY: u64 = 86_400;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Seen {
    Shown,
    Dismissed,
    Answered,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct SeenEntry {
    pub state: Seen,
    /// Unix seconds when it was last marked.
    pub at: u64,
}

/// Survey id to what happened to it.
pub type SeenMap = HashMap<String, SeenEntry>;

/// What is going on right now, for `eligible`.
pub struct Moment<'a> {
    /// Usage telemetry is on. Popovers are a kind of usage telemetry.
    pub usage: bool,
    pub since_launch: Duration,
    pub since_typing: Duration,
    pub shown_this_launch: bool,
    pub now_unix: u64,
    /// How long ago this install was made, from its telemetry id file.
    pub installed_for: Duration,
    pub events_seen: &'a HashSet<String>,
}

/// The survey to pop up now, if any. At most one per launch, and never early,
/// or while a person is typing.
pub fn eligible<'a>(surveys: &'a [Survey], seen: &SeenMap, m: &Moment) -> Option<&'a Survey> {
    if !m.usage
        || m.shown_this_launch
        || m.since_launch < FIRST_POPOVER_DELAY
        || m.since_typing < TYPING_QUIET
    {
        return None;
    }
    let last_shown = seen.values().map(|e| e.at).max();
    surveys.iter().find(|s| {
        s.kind == SurveyType::Popover
            // The feedback forms have their own screens. Someone setting one to
            // popover in PostHog must not pop it up on its own.
            && !ids::is_builtin(&s.id)
            && !s.archived
            && !seen.contains_key(&s.id)
            && model::drawable(s)
            && in_schedule(s, m.now_unix)
            && conditions_met(&s.conditions, m, last_shown)
    })
}

fn in_schedule(s: &Survey, now: u64) -> bool {
    // A draft has no start date and is never shown.
    let Some(start) = s.start_date.as_deref().and_then(unix_from_rfc3339) else {
        return false;
    };
    let ended = match s.end_date.as_deref() {
        None => false,
        // A date we cannot read is safer as over.
        Some(e) => unix_from_rfc3339(e).is_none_or(|end| end <= now as i64),
    };
    start <= now as i64 && !ended
}

fn conditions_met(c: &Conditions, m: &Moment, last_shown: Option<u64>) -> bool {
    if c.unsupported || c.url.is_some() {
        return false;
    }
    if !c.events.is_empty() && !c.events.iter().any(|e| m.events_seen.contains(e)) {
        return false;
    }
    if let Some(days) = c.wait_days {
        let wait = u64::from(days) * DAY;
        // Not in the first days after install, and not soon after another survey.
        if m.installed_for.as_secs() < wait {
            return false;
        }
        if last_shown.is_some_and(|at| m.now_unix < at.saturating_add(wait)) {
            return false;
        }
    }
    true
}

/// Unix seconds from an RFC 3339 date such as `2025-11-17T14:34:56.789Z` or
/// `2025-11-17T15:34:56+01:00`.
fn unix_from_rfc3339(s: &str) -> Option<i64> {
    let b = s.as_bytes();
    let num = |from: usize, len: usize| -> Option<i64> {
        let part = s.get(from..from + len)?;
        part.bytes()
            .all(|c| c.is_ascii_digit())
            .then(|| part.parse().ok())?
    };
    if b.len() < 20 || b[4] != b'-' || b[7] != b'-' || !matches!(b[10], b'T' | b't' | b' ') {
        return None;
    }
    if b[13] != b':' || b[16] != b':' {
        return None;
    }
    let (y, mo, d) = (num(0, 4)?, num(5, 2)?, num(8, 2)?);
    let (h, mi, sec) = (num(11, 2)?, num(14, 2)?, num(17, 2)?);
    if !(1..=12).contains(&mo) || !(1..=31).contains(&d) || h > 23 || mi > 59 || sec > 60 {
        return None;
    }
    let mut i = 19;
    if b[i] == b'.' {
        i += 1;
        while b.get(i).is_some_and(u8::is_ascii_digit) {
            i += 1;
        }
    }
    let offset = match b.get(i)? {
        b'Z' | b'z' if i + 1 == b.len() => 0,
        sign @ (b'+' | b'-') if i + 6 == b.len() && b[i + 3] == b':' => {
            let secs = num(i + 1, 2)? * 3600 + num(i + 4, 2)? * 60;
            if *sign == b'+' { secs } else { -secs }
        }
        _ => return None,
    };
    let days = crate::platform::days_from_civil(y, mo, d);
    Some(days * 86_400 + h * 3600 + mi * 60 + sec - offset)
}

/// The survey as the app shows it when PostHog cannot be reached.
pub(super) fn fallback(b: &Builtin) -> Survey {
    let questions = b
        .questions
        .iter()
        .map(|f| Question {
            id: f.id.into(),
            text: f.text.into(),
            description: String::new(),
            optional: f.optional,
            button_text: None,
            body: match &f.body {
                FallbackBody::Open => Body::Open,
                FallbackBody::SingleChoice { choices } => Body::Single {
                    choices: choices.iter().map(|c| (*c).into()).collect(),
                    open_choice: false,
                },
                FallbackBody::Rating { scale } => Body::Rating {
                    scale: *scale,
                    emoji: false,
                    lower: String::new(),
                    upper: String::new(),
                },
            },
            branching: Branching::Next,
        })
        .collect();
    Survey {
        id: b.survey_id.into(),
        name: String::new(),
        kind: SurveyType::Api,
        questions,
        start_date: None,
        end_date: None,
        archived: false,
        conditions: Conditions::default(),
    }
}

/// The cached survey for `b` when this version can draw it, else the fallback.
fn builtin_from(b: &Builtin, cached: &[Survey]) -> Survey {
    cached
        .iter()
        .find(|s| s.id == b.survey_id && model::drawable(s))
        .cloned()
        .unwrap_or_else(|| fallback(b))
}

pub fn builtin(b: &Builtin) -> Survey {
    builtin_from(b, &load_cache(&dir()))
}

/// The surveys from the last good fetch.
pub fn cached() -> Vec<Survey> {
    load_cache(&dir())
}

fn dir() -> PathBuf {
    crate::platform::app_dir()
}

fn load_cache(dir: &Path) -> Vec<Survey> {
    std::fs::read_to_string(dir.join(crate::paths::SURVEYS.name()))
        .map(|j| model::parse(&j))
        .unwrap_or_default()
}

pub fn seen() -> SeenMap {
    load_seen(&dir())
}

fn load_seen(dir: &Path) -> SeenMap {
    std::fs::read(dir.join(crate::paths::SURVEYS_SEEN.name()))
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
}

pub fn mark(id: &str, seen: Seen) {
    if cfg!(test) {
        return;
    }
    mark_in(&dir(), id, seen, now_unix());
}

fn mark_in(dir: &Path, id: &str, seen: Seen, now: u64) {
    let mut map = load_seen(dir);
    map.insert(
        id.into(),
        SeenEntry {
            state: seen,
            at: now,
        },
    );
    if let Ok(json) = serde_json::to_vec(&map) {
        let _ = write_file(dir, crate::paths::SURVEYS_SEEN.name(), &json);
    }
}

/// Writes through a temp file, so a crash never leaves half a file.
fn write_file(dir: &Path, name: &str, bytes: &[u8]) -> bool {
    let _ = std::fs::create_dir_all(dir);
    let tmp = dir.join(format!("{name}.tmp"));
    if std::fs::write(&tmp, bytes).is_err() {
        return false;
    }
    if std::fs::rename(&tmp, dir.join(name)).is_err() {
        let _ = std::fs::remove_file(&tmp);
        return false;
    }
    true
}

pub fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// How long ago this install was made, from the telemetry id file. Zero when
/// there is none, which keeps a "wait" survey away.
pub fn installed_for() -> Duration {
    std::fs::metadata(crate::paths::shared(crate::paths::TELEMETRY_ID))
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.elapsed().ok())
        .unwrap_or_default()
}

/// The wait after a failed fetch: twice the last one, up to the refetch time.
fn backoff(previous: Option<Duration>) -> Duration {
    previous.map_or(FIRST_RETRY, |p| (p * 2).min(REFETCH))
}

/// Whether to fetch now: the cache is old or missing, and no failed try is
/// still being waited out.
fn due(cache_age: Option<Duration>, retry_in: Duration) -> bool {
    retry_in.is_zero() && cache_age.is_none_or(|a| a >= REFETCH)
}

/// Holds the in-flight flag; clears it when dropped, even on a panic.
struct InFlight(&'static AtomicBool);

impl InFlight {
    fn begin(flag: &'static AtomicBool) -> Option<InFlight> {
        flag.compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .ok()
            .map(|_| InFlight(flag))
    }
}

impl Drop for InFlight {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}

static FETCHING: AtomicBool = AtomicBool::new(false);

/// Updates the retry state after a try: cleared on success, doubled on failure.
fn record(retry: &mut Option<(Instant, Duration)>, ok: bool, now: Instant) {
    *retry = if ok {
        None
    } else {
        let wait = backoff(retry.map(|(_, w)| w));
        Some((now + wait, wait))
    };
}

/// When the next try may start, and how long the last wait was.
static RETRY: Mutex<Option<(Instant, Duration)>> = Mutex::new(None);

/// Whether surveys may be fetched from PostHog at all.
fn should_fetch(blocked: bool, usage: bool) -> bool {
    !blocked && usage
}

/// Fetches the surveys in the background, at most every 6 hours. A failure
/// keeps the cache. Nothing is fetched when telemetry is blocked, which covers
/// tests, debug builds and `arakun dev`, or when usage counts are off: the
/// request is a kind of usage telemetry. The built-in forms then use their
/// compiled-in questions.
pub fn refresh() {
    if !should_fetch(
        crate::telemetry::blocked(),
        crate::telemetry::effective().usage,
    ) {
        return;
    }
    let dir = dir();
    let retry_in = RETRY
        .lock()
        .ok()
        .and_then(|r| *r)
        .map_or(Duration::ZERO, |(at, _)| {
            at.saturating_duration_since(Instant::now())
        });
    let age = std::fs::metadata(dir.join(crate::paths::SURVEYS.name()))
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.elapsed().ok());
    if !due(age, retry_in) {
        return;
    }
    // One fetch at a time, or two threads would write the same temp file.
    let Some(guard) = InFlight::begin(&FETCHING) else {
        return;
    };
    let _ = std::thread::Builder::new()
        .name("surveys-fetch".into())
        .spawn(move || {
            let _guard = guard;
            // A cache that cannot be written counts as a failure, or an
            // unwritable disk would refetch on every call.
            let ok = fetch().is_some_and(|body| {
                write_file(&dir, crate::paths::SURVEYS.name(), body.as_bytes())
            });
            if let Ok(mut r) = RETRY.lock() {
                record(&mut r, ok, Instant::now());
            }
        });
}

/// The reply body, if it is a survey list.
fn fetch() -> Option<String> {
    let url = format!(
        "{}/api/surveys/?token={}",
        crate::telemetry::HOST.trim_end_matches('/'),
        crate::telemetry::KEY
    );
    let agent = crate::http::agent(Duration::from_secs(10));
    let reply = crate::http::json_call(agent, "GET", &url, None, None, 10 * 1024 * 1024).ok()?;
    if !reply.ok() {
        return None;
    }
    let body = reply.text();
    let v: serde_json::Value = serde_json::from_str(&body).ok()?;
    v.get("surveys")?.is_array().then_some(body)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::surveys::ids::{AGENT, ERROR, USER};

    // 2025-11-17T14:34:56Z.
    const NOW: u64 = 1_763_390_096;

    fn popover(id: &str) -> Survey {
        let mut s = fallback(&USER);
        s.id = id.into();
        s.kind = SurveyType::Popover;
        s.start_date = Some("2025-11-01T00:00:00Z".into());
        s
    }

    fn events() -> &'static HashSet<String> {
        static E: std::sync::OnceLock<HashSet<String>> = std::sync::OnceLock::new();
        E.get_or_init(HashSet::new)
    }

    fn moment() -> Moment<'static> {
        Moment {
            usage: true,
            since_launch: Duration::from_secs(600),
            since_typing: Duration::from_secs(60),
            shown_this_launch: false,
            now_unix: NOW,
            installed_for: Duration::from_secs(30 * DAY),
            events_seen: events(),
        }
    }

    fn pick<'a>(s: &'a [Survey], seen: &SeenMap, m: &Moment) -> Option<&'a Survey> {
        eligible(s, seen, m)
    }

    #[test]
    fn eligible_picks_a_popover() {
        let all = [popover("a")];
        assert_eq!(pick(&all, &SeenMap::new(), &moment()).unwrap().id, "a");
    }

    #[test]
    fn builtin_falls_back_without_cache() {
        let s = builtin_from(&USER, &[]);
        let ids: Vec<&str> = s.questions.iter().map(|q| q.id.as_str()).collect();
        let want: Vec<&str> = USER.questions.iter().map(|q| q.id).collect();
        assert_eq!(ids, want);
        assert_eq!(s.id, USER.survey_id);
        assert!(model::drawable(&s));
        let e = fallback(&ERROR);
        assert!(matches!(e.questions[1].body, Body::Rating { scale: 5, .. }));
    }

    #[test]
    fn builtin_uses_a_drawable_cached_survey() {
        let mut cached = fallback(&AGENT);
        cached.name = "from posthog".into();
        assert_eq!(builtin_from(&AGENT, &[cached]).name, "from posthog");
    }

    #[test]
    fn builtin_with_required_unsupported_uses_fallback() {
        let mut cached = fallback(&AGENT);
        cached.name = "from posthog".into();
        cached.questions[0].body = Body::Unsupported;
        let s = builtin_from(&AGENT, &[cached]);
        assert!(s.name.is_empty());
        assert!(matches!(s.questions[0].body, Body::Single { .. }));
    }

    #[test]
    fn eligible_needs_usage() {
        let all = [popover("a")];
        let m = Moment {
            usage: false,
            ..moment()
        };
        assert!(pick(&all, &SeenMap::new(), &m).is_none());
    }

    #[test]
    fn eligible_not_in_first_two_minutes() {
        let all = [popover("a")];
        let early = Moment {
            since_launch: Duration::from_secs(119),
            ..moment()
        };
        assert!(pick(&all, &SeenMap::new(), &early).is_none());
        let ok = Moment {
            since_launch: Duration::from_secs(120),
            ..moment()
        };
        assert!(pick(&all, &SeenMap::new(), &ok).is_some());
    }

    #[test]
    fn eligible_waits_while_typing() {
        let all = [popover("a")];
        let m = Moment {
            since_typing: Duration::from_secs(3),
            ..moment()
        };
        assert!(pick(&all, &SeenMap::new(), &m).is_none());
    }

    #[test]
    fn eligible_once_per_launch() {
        let all = [popover("a")];
        let m = Moment {
            shown_this_launch: true,
            ..moment()
        };
        assert!(pick(&all, &SeenMap::new(), &m).is_none());
    }

    #[test]
    fn dismissed_never_returns() {
        let all = [popover("a")];
        let mut seen = SeenMap::new();
        seen.insert(
            "a".into(),
            SeenEntry {
                state: Seen::Dismissed,
                at: 1,
            },
        );
        assert!(pick(&all, &seen, &moment()).is_none());
    }

    #[test]
    fn api_type_never_pops() {
        let mut s = popover("a");
        s.kind = SurveyType::Api;
        assert!(pick(&[s], &SeenMap::new(), &moment()).is_none());
        assert_eq!(builtin_from(&USER, &[]).kind, SurveyType::Api);
        assert!(pick(&[builtin_from(&USER, &[])], &SeenMap::new(), &moment()).is_none());
    }

    #[test]
    fn outside_schedule_is_skipped() {
        let mut draft = popover("a");
        draft.start_date = None;
        let mut future = popover("b");
        future.start_date = Some("2025-12-01T00:00:00Z".into());
        let mut over = popover("c");
        over.end_date = Some("2025-11-17T14:34:56Z".into());
        let mut garbled = popover("d");
        garbled.end_date = Some("soon".into());
        let mut live = popover("e");
        live.end_date = Some("2025-11-17T15:34:57+01:00".into());
        let all = [draft, future, over, garbled, live];
        assert_eq!(pick(&all, &SeenMap::new(), &moment()).unwrap().id, "e");
    }

    #[test]
    fn flag_or_url_condition_is_skipped() {
        let parsed = model::parse(
            r#"{"surveys":[
            {"id":"u","type":"popover","start_date":"2025-11-01T00:00:00Z","questions":[
              {"id":"q","type":"open","question":"?"}],
             "conditions":{"url":"/settings"}},
            {"id":"f","type":"popover","start_date":"2025-11-01T00:00:00Z","questions":[
              {"id":"q","type":"open","question":"?"}],"linked_flag_key":"beta"},
            {"id":"d","type":"popover","start_date":"2025-11-01T00:00:00Z","questions":[
              {"id":"q","type":"open","question":"?"}],
             "conditions":{"deviceTypes":["Mobile"]}},
            {"id":"ok","type":"popover","start_date":"2025-11-01T00:00:00Z","questions":[
              {"id":"q","type":"open","question":"?"}],
             "conditions":{"url":null,"selector":null,"deviceTypes":[]}}]}"#,
        );
        assert_eq!(parsed.len(), 4);
        assert_eq!(pick(&parsed, &SeenMap::new(), &moment()).unwrap().id, "ok");
        assert!(parsed[0].conditions.unsupported);
        assert!(parsed[1].conditions.unsupported);
        assert!(parsed[2].conditions.unsupported);
    }

    #[test]
    fn event_condition_waits_for_event() {
        let mut s = popover("a");
        s.conditions.events = vec!["$exception".into()];
        let all = [s];
        assert!(pick(&all, &SeenMap::new(), &moment()).is_none());
        let mut seen_events = HashSet::new();
        seen_events.insert("$exception".to_string());
        let m = Moment {
            events_seen: &seen_events,
            ..moment()
        };
        assert!(pick(&all, &SeenMap::new(), &m).is_some());
    }

    #[test]
    fn wait_days_counts_from_install_and_last_survey() {
        let mut s = popover("a");
        s.conditions.wait_days = Some(7);
        let all = [s];
        let young = Moment {
            installed_for: Duration::from_secs(2 * DAY),
            ..moment()
        };
        assert!(pick(&all, &SeenMap::new(), &young).is_none());
        let mut seen = SeenMap::new();
        seen.insert(
            "other".into(),
            SeenEntry {
                state: Seen::Shown,
                at: NOW - 3 * DAY,
            },
        );
        assert!(pick(&all, &seen, &moment()).is_none());
        seen.get_mut("other").unwrap().at = NOW - 8 * DAY;
        assert!(pick(&all, &seen, &moment()).is_some());
    }

    #[test]
    fn dates_parse_with_offsets() {
        assert_eq!(
            unix_from_rfc3339("2025-11-17T14:34:56.789Z"),
            Some(1_763_390_096)
        );
        assert_eq!(
            unix_from_rfc3339("2025-11-17T15:34:56+01:00"),
            Some(1_763_390_096)
        );
        assert_eq!(unix_from_rfc3339("2025-11-17"), None);
        assert_eq!(unix_from_rfc3339("2025-13-17T14:34:56Z"), None);
        assert_eq!(unix_from_rfc3339("2025-11-17T14:34:56"), None);
    }

    #[test]
    fn retries_back_off_up_to_six_hours() {
        let mut w = backoff(None);
        assert_eq!(w, FIRST_RETRY);
        for _ in 0..20 {
            w = backoff(Some(w));
        }
        assert_eq!(w, REFETCH);
        assert!(due(None, Duration::ZERO));
        assert!(!due(None, Duration::from_secs(5)));
        assert!(!due(Some(Duration::from_secs(60)), Duration::ZERO));
        assert!(due(Some(REFETCH), Duration::ZERO));
    }

    #[test]
    fn builtin_ids_never_pop() {
        for b in [&USER, &AGENT, &ERROR] {
            let mut s = popover(b.survey_id);
            s.kind = SurveyType::Popover;
            assert!(pick(&[s], &SeenMap::new(), &moment()).is_none());
        }
    }

    #[test]
    fn surveys_are_fetched_only_with_usage_on() {
        assert!(should_fetch(false, true));
        assert!(!should_fetch(false, false));
        assert!(!should_fetch(true, true));
        assert!(!should_fetch(true, false));
    }

    #[test]
    fn refresh_skips_while_in_flight() {
        static FLAG: AtomicBool = AtomicBool::new(false);
        let first = InFlight::begin(&FLAG).expect("first starts");
        assert!(InFlight::begin(&FLAG).is_none());
        drop(first);
        assert!(InFlight::begin(&FLAG).is_some());
        // The guard cleared the flag on drop, so another can start.
        assert!(InFlight::begin(&FLAG).is_some());
    }

    #[test]
    fn failed_fetch_or_write_backs_off() {
        let now = Instant::now();
        let mut retry = None;
        record(&mut retry, false, now);
        assert_eq!(retry, Some((now + FIRST_RETRY, FIRST_RETRY)));
        record(&mut retry, false, now);
        assert_eq!(retry.unwrap().1, FIRST_RETRY * 2);
        record(&mut retry, true, now);
        assert!(retry.is_none());
        // A folder that cannot be written is a failure.
        let file = std::env::temp_dir().join(format!("arakun-surveys-file-{}", std::process::id()));
        std::fs::write(&file, b"x").unwrap();
        assert!(!write_file(&file, "surveys.json", b"{}"));
        let _ = std::fs::remove_file(&file);
    }

    #[test]
    fn seen_state_round_trips_in_a_folder() {
        let dir = std::env::temp_dir().join(format!("arakun-surveys-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        assert!(load_seen(&dir).is_empty());
        mark_in(&dir, "a", Seen::Shown, 5);
        mark_in(&dir, "a", Seen::Answered, 9);
        mark_in(&dir, "b", Seen::Dismissed, 7);
        let map = load_seen(&dir);
        assert_eq!(map.len(), 2);
        assert_eq!(map["a"].state, Seen::Answered);
        assert_eq!(map["a"].at, 9);
        write_file(
            &dir,
            "surveys.json",
            br#"{"surveys":[{"id":"x","type":"api"}]}"#,
        );
        assert_eq!(load_cache(&dir).len(), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
