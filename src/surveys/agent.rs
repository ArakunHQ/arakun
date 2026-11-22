//! What `arakun feedback` does with a report from an agent, apart from the
//! window: the checks, the rate limit and the scrubbing. `Shell` only wires it.

use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use super::ids;
use super::model::{Answer, Body, Survey};
use crate::telemetry::{self, Refused};

/// The most characters of a report.
pub const MAX_CHARS: usize = 4000;
/// Reports one environment may send in `WINDOW`.
pub const PER_HOUR: usize = 5;
const WINDOW: Duration = Duration::from_secs(3600);

/// When each environment last sent a report, newest last.
#[derive(Default)]
pub struct RateLimit {
    sent: HashMap<PathBuf, VecDeque<Instant>>,
}

impl RateLimit {
    /// Whether `env` may send now, without counting it.
    fn has_room(&mut self, env: &Path, now: Instant) -> bool {
        let times = self.sent.entry(env.to_path_buf()).or_default();
        while times
            .front()
            .is_some_and(|t| now.saturating_duration_since(*t) >= WINDOW)
        {
            times.pop_front();
        }
        times.len() < PER_HOUR
    }

    pub fn record(&mut self, env: &Path, now: Instant) {
        self.sent
            .entry(env.to_path_buf())
            .or_default()
            .push_back(now);
    }
}

/// A report that passed every check, ready for `send::submit`.
#[derive(Debug, PartialEq)]
pub struct Prepared {
    pub answers: Vec<(usize, Answer)>,
    /// The agent's name, a label only.
    pub agent: String,
    /// The first line of the scrubbed text, for the notice.
    pub first_line: String,
    /// The whole scrubbed text, for the Inbox.
    pub text: String,
}

/// A label of lowercase letters, digits and dashes, else `agent`. It is sent
/// to PostHog, so nothing else may ride along in it.
pub fn clean_agent(name: &str) -> String {
    let ok = (1..=32).contains(&name.len())
        && name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-');
    if ok { name.into() } else { "agent".into() }
}

/// Checks a report against `env`'s limit. `decide` says whether telemetry
/// accepts it. The caller calls `limits.record` once the report is sent, so a
/// refused one costs no slot.
pub fn agent_feedback(
    survey: &Survey,
    kind: &str,
    text: &str,
    agent: &str,
    env: &Path,
    now: Instant,
    limits: &mut RateLimit,
    decide: impl Fn() -> Result<(), Refused>,
) -> Result<Prepared, String> {
    let Some(kind_at) = ids::KINDS.iter().position(|k| *k == kind) else {
        return Err(format!("kind must be one of {}", ids::KINDS.join(", ")));
    };
    let text = text.trim();
    if text.is_empty() {
        return Err("the feedback is empty".into());
    }
    if text.chars().count() > MAX_CHARS {
        return Err(format!("the feedback is over {MAX_CHARS} characters"));
    }
    decide().map_err(|r| r.to_string())?;
    if !limits.has_room(env, now) {
        return Err(format!(
            "this environment already sent {PER_HOUR} reports in the last hour"
        ));
    }
    // The kind is the choice at that position, however PostHog words it.
    let choice = match survey.questions.first().map(|q| &q.body) {
        Some(Body::Single { choices, .. }) => choices.get(kind_at).cloned(),
        _ => None,
    }
    .ok_or("the feedback survey has no kind question")?;
    let text = telemetry::scrub(text, &telemetry::home());
    Ok(Prepared {
        first_line: text.lines().next().unwrap_or_default().trim().to_string(),
        answers: vec![(0, Answer::Choice(choice)), (1, Answer::Text(text.clone()))],
        text,
        agent: clean_agent(agent),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::surveys::model::kind_of;
    use crate::surveys::store;

    fn run(
        kind: &str,
        text: &str,
        env: &str,
        now: Instant,
        limits: &mut RateLimit,
        decide: Result<(), Refused>,
    ) -> Result<Prepared, String> {
        let s = store::fallback(&ids::AGENT);
        let p = agent_feedback(&s, kind, text, "codex", Path::new(env), now, limits, || {
            decide
        })?;
        // As the shell does once the submit went through.
        limits.record(Path::new(env), now);
        Ok(p)
    }

    #[test]
    fn refused_submit_keeps_slot() {
        let s = store::fallback(&ids::AGENT);
        let mut l = RateLimit::default();
        let t = Instant::now();
        for _ in 0..10 {
            // Checked but never recorded, as when `submit` is refused.
            agent_feedback(&s, "bug", "x", "a", Path::new("/e"), t, &mut l, || Ok(())).unwrap();
        }
        assert!(run("bug", "x", "/e", t, &mut l, Ok(())).is_ok());
    }

    #[test]
    fn rate_limit_sixth_in_hour_refused() {
        let mut l = RateLimit::default();
        let t = Instant::now();
        for i in 0..5 {
            assert!(run("bug", "x", "/e", t + Duration::from_secs(i), &mut l, Ok(())).is_ok());
        }
        let sixth = run(
            "bug",
            "x",
            "/e",
            t + Duration::from_secs(10),
            &mut l,
            Ok(()),
        );
        assert!(sixth.unwrap_err().contains("5 reports"));
        // An hour after the first, a slot is free again.
        assert!(run("bug", "x", "/e", t + WINDOW, &mut l, Ok(())).is_ok());
    }

    #[test]
    fn rate_limit_per_environment() {
        let mut l = RateLimit::default();
        let t = Instant::now();
        for _ in 0..5 {
            run("bug", "x", "/a", t, &mut l, Ok(())).unwrap();
        }
        assert!(run("bug", "x", "/a", t, &mut l, Ok(())).is_err());
        assert!(run("bug", "x", "/b", t, &mut l, Ok(())).is_ok());
    }

    #[test]
    fn feedback_refused_without_usage() {
        let mut l = RateLimit::default();
        let t = Instant::now();
        for _ in 0..10 {
            let e = run("bug", "x", "/e", t, &mut l, Err(Refused::NoUsage)).unwrap_err();
            assert_eq!(e, Refused::NoUsage.to_string());
        }
        // None of the refusals used a slot.
        for _ in 0..5 {
            run("bug", "x", "/e", t, &mut l, Ok(())).unwrap();
        }
    }

    #[test]
    fn kind_maps_by_position_and_text_is_scrubbed() {
        let s = store::fallback(&ids::AGENT);
        let mut l = RateLimit::default();
        let home = telemetry::home();
        let text = format!("  see {home}/secret/file.rs and https://x.io/a?t=1 \nmore");
        for (i, k) in ids::KINDS.iter().enumerate() {
            let p = run(k, &text, "/e", Instant::now(), &mut l, Ok(())).unwrap();
            let got = kind_of(&s.questions[0], &p.answers[0].1).unwrap();
            assert_eq!(got.as_str(), ids::KINDS[i]);
            assert_eq!(p.agent, "codex");
            if home.len() >= 2 {
                let Answer::Text(t) = &p.answers[1].1 else {
                    panic!()
                };
                assert!(!t.contains(&home), "{t}");
                assert!(!p.first_line.contains(&home));
            }
        }
    }

    #[test]
    fn rejects_bad_kind_empty_and_long_text() {
        let mut l = RateLimit::default();
        let t = Instant::now();
        assert!(run("nope", "x", "/e", t, &mut l, Ok(())).is_err());
        assert!(run("bug", " \n ", "/e", t, &mut l, Ok(())).is_err());
        assert!(run("bug", &"a".repeat(4001), "/e", t, &mut l, Ok(())).is_err());
        assert!(run("bug", &"é".repeat(4000), "/e", t, &mut l, Ok(())).is_ok());
    }

    #[test]
    fn agent_name_is_only_a_label() {
        assert_eq!(clean_agent("claude"), "claude");
        assert_eq!(clean_agent("Claude Code"), "agent");
        assert_eq!(clean_agent(""), "agent");
        assert_eq!(clean_agent(&"a".repeat(33)), "agent");
    }
}
