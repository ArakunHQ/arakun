//! Watching the pull request of a finished delegated session. Its issue hears
//! when a required check fails or a reviewer asks for changes, so the person
//! who delegated need not find out on GitHub.
//!
//! One `gh api graphql` call reads a pull request, off the UI thread and at
//! most once a tick. Each head commit's failed checks and each review are
//! told once; the record of what was told is saved with the run, so a
//! restart does not tell it again. Only names, counts and links go to Linear,
//! never a check's log or a comment's text.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::pr::parse_url;
use crate::git;
use crate::platform::GIT_REMOTE;

/// The least time between two reads of one pull request.
pub const LOOK_SECS: u64 = 4 * 60;
/// A pull request with no change this long is not watched any more.
pub const IDLE_SECS: u64 = 7 * 24 * 3600;
/// How long every read waits after GitHub said to slow down.
pub const LIMITED_SECS: u64 = 30 * 60;
/// Events remembered as told; the oldest go first.
const TOLD_KEPT: usize = 60;
/// Checks named in one line; the rest are counted.
const NAMED: usize = 5;
/// The most characters of a check's or a reviewer's name.
const NAME_LIMIT: usize = 60;

/// What is known of a watched pull request. Saved with its run.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Watch {
    /// Reviews submitted before this unix second are not news: the session
    /// was at work on the pull request then.
    pub since: u64,
    /// When the pull request last changed, or the watch started.
    pub changed_at: u64,
    /// What the last read saw, to tell a change: the head commit, and how
    /// many reviews and finished checks it had.
    #[serde(default)]
    pub seen: String,
    /// Events told: `checks:<head commit>` and `review:<review id>`.
    #[serde(default)]
    pub told: Vec<String>,
    #[serde(default)]
    pub looked_at: u64,
    /// No read before this unix second: GitHub asked to slow down.
    #[serde(default)]
    pub not_before: u64,
    /// The pull request merged or closed, or its issue closed, or it was
    /// quiet for `IDLE_SECS`.
    #[serde(default)]
    pub ended: bool,
}

impl Watch {
    pub fn new(now: u64) -> Watch {
        Watch {
            since: now,
            changed_at: now,
            ..Watch::default()
        }
    }

    /// Whether it still needs reading, now or later.
    pub fn live(&self, now: u64) -> bool {
        !self.ended && now.saturating_sub(self.changed_at) < IDLE_SECS
    }

    pub fn due(&self, now: u64) -> bool {
        self.live(now)
            && now >= self.not_before
            && (self.looked_at == 0 || now.saturating_sub(self.looked_at) >= LOOK_SECS)
    }

    fn was_told(&self, key: &str) -> bool {
        self.told.iter().any(|t| t == key)
    }

    fn tell(&mut self, key: String) {
        if !self.was_told(&key) {
            self.told.push(key);
        }
        let extra = self.told.len().saturating_sub(TOLD_KEPT);
        self.told.drain(..extra);
    }
}

/// One pull request to read, with what its run knows of it.
#[derive(Clone, Debug)]
pub struct Look {
    pub agent_session: String,
    pub organization: String,
    pub issue: String,
    /// Where `gh` runs: the run's worktree, or its project.
    pub dir: PathBuf,
    pub url: String,
    pub watch: Watch,
}

/// A pull request as one read saw it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Snapshot {
    pub state: PrState,
    pub number: u64,
    pub author: String,
    pub head: String,
    pub checks: Vec<Check>,
    pub reviews: Vec<Review>,
    /// Review threads not resolved yet.
    pub open_threads: usize,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum PrState {
    #[default]
    Open,
    Closed,
    Merged,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    Pending,
    Passed,
    Failed,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Check {
    pub name: String,
    pub required: bool,
    pub outcome: Outcome,
    /// A check run (a CI job) rather than a commit status. Only check runs
    /// are waited for: a status such as a human approval can stay pending
    /// for days.
    pub run: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Review {
    pub id: String,
    pub author: String,
    /// APPROVED, CHANGES_REQUESTED, COMMENTED, DISMISSED or PENDING.
    pub state: String,
    pub submitted_at: Option<u64>,
}

/// What reading a pull request gave.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Read {
    Seen(Snapshot),
    /// GitHub's rate limit: every read waits a while.
    Limited,
    /// `gh` is missing, signed out or offline: tried again next time.
    Failed,
}

/// One look's outcome, for [`super::runs::Runs::looked`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Looked {
    pub agent_session: String,
    pub read: Read,
    /// Its issue is done or canceled in Linear. Only asked when there is
    /// news, so a quiet pull request costs no call to Linear.
    pub issue_closed: bool,
}

/// What a read has to tell the issue.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct News {
    /// The failed checks of the head commit, once they all finished.
    pub failed: Vec<String>,
    /// New reviews that ask for changes or leave comments.
    pub reviews: Vec<Review>,
}

impl News {
    pub fn is_empty(&self) -> bool {
        self.failed.is_empty() && self.reviews.is_empty()
    }
}

/// The checks that count: the required ones, or every one when the
/// repository marks none as required.
fn counted(checks: &[Check]) -> impl Iterator<Item = &Check> {
    let any_required = checks.iter().any(|c| c.required);
    checks.iter().filter(move |c| c.required || !any_required)
}

/// Whether every counted check run on the head finished.
fn finished(checks: &[Check]) -> bool {
    counted(checks).all(|c| !c.run || c.outcome != Outcome::Pending)
}

fn checks_key(head: &str) -> String {
    format!("checks:{head}")
}

fn review_key(id: &str) -> String {
    format!("review:{id}")
}

/// What `s` has that `w` has not told yet. Failed checks count once every
/// counted check run on the head commit finished, so one line names them all.
/// A review counts when someone other than the author asked for changes or
/// commented, after the watch's `since`.
pub fn news(w: &Watch, s: &Snapshot) -> News {
    let mut out = News::default();
    if s.state != PrState::Open {
        return out;
    }
    if !s.head.is_empty() && finished(&s.checks) && !w.was_told(&checks_key(&s.head)) {
        for c in counted(&s.checks).filter(|c| c.outcome == Outcome::Failed) {
            if !out.failed.contains(&c.name) {
                out.failed.push(c.name.clone());
            }
        }
    }
    out.reviews = s
        .reviews
        .iter()
        .filter(|r| matches!(r.state.as_str(), "CHANGES_REQUESTED" | "COMMENTED"))
        .filter(|r| !r.author.eq_ignore_ascii_case(&s.author))
        .filter(|r| r.submitted_at.is_some_and(|at| at >= w.since))
        .filter(|r| !w.was_told(&review_key(&r.id)))
        .cloned()
        .collect();
    out
}

/// What tells a change, so a pull request quiet for days stops being watched.
fn mark(s: &Snapshot) -> String {
    let finished = s
        .checks
        .iter()
        .filter(|c| c.outcome != Outcome::Pending)
        .count();
    format!("{}:{}:{}", s.head, s.reviews.len(), finished)
}

/// Applies one read to `w`, and gives the lines its issue hears.
pub fn apply(w: &mut Watch, looked: &Looked, now: u64) -> Vec<String> {
    let s = match &looked.read {
        Read::Seen(s) => s,
        Read::Limited => {
            w.not_before = now + LIMITED_SECS;
            return Vec::new();
        }
        Read::Failed => return Vec::new(),
    };
    let m = mark(s);
    if m != w.seen {
        w.seen = m;
        w.changed_at = now;
    }
    if s.state != PrState::Open || looked.issue_closed {
        w.ended = true;
        return Vec::new();
    }
    let news = news(w, s);
    let mut lines = Vec::new();
    // A head with all its checks done is told once, failed or not, so a
    // later failure of the same commit can't come from a re-run alone.
    if finished(&s.checks) && !s.head.is_empty() {
        w.tell(checks_key(&s.head));
    }
    if !news.failed.is_empty() {
        let required = s.checks.iter().any(|c| c.required);
        lines.push(checks_line(s.number, &news.failed, required));
    }
    if !news.reviews.is_empty() {
        lines.push(reviews_line(s.number, &news.reviews, s.open_threads));
        for r in &news.reviews {
            w.tell(review_key(&r.id));
        }
    }
    lines
}

/// A name from GitHub, safe in a Linear line: no backticks or line breaks,
/// and not too long.
fn clean(name: &str) -> String {
    let one: String = name
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .filter(|c| *c != '`')
        .collect();
    let one = one.trim();
    match one.char_indices().nth(NAME_LIMIT) {
        Some((end, _)) => format!("{}…", &one[..end]),
        None => one.to_string(),
    }
}

fn listed(names: &[String]) -> String {
    let mut shown: Vec<String> = names
        .iter()
        .take(NAMED)
        .map(|n| format!("`{}`", clean(n)))
        .collect();
    if names.len() > NAMED {
        shown.push(format!("{} more", names.len() - NAMED));
    }
    match shown.split_last() {
        Some((last, rest)) if !rest.is_empty() => format!("{} and {last}", rest.join(", ")),
        _ => shown.join(""),
    }
}

pub fn checks_line(number: u64, failed: &[String], required: bool) -> String {
    let what = match (failed.len() == 1, required) {
        (true, true) => "A required check failed",
        (false, true) => "Required checks failed",
        (true, false) => "A check failed",
        (false, false) => "Checks failed",
    };
    format!(
        "{what} on pull request #{number}: {}. Reply `fix checks` to let Arakun fix it.",
        listed(failed)
    )
}

pub fn reviews_line(number: u64, reviews: &[Review], open_threads: usize) -> String {
    let mut who: Vec<String> = Vec::new();
    for r in reviews {
        let name = clean(&r.author);
        let name = if name.is_empty() {
            "someone".to_string()
        } else {
            name
        };
        if !who.contains(&name) {
            who.push(name);
        }
    }
    let changes = reviews.iter().any(|r| r.state == "CHANGES_REQUESTED");
    let did = if changes {
        "asked for changes on"
    } else {
        "left comments on"
    };
    let threads = match open_threads {
        0 => "No review thread is open.".to_string(),
        1 => "1 review thread is open.".to_string(),
        n => format!("{n} review threads are open."),
    };
    format!(
        "{} {did} pull request #{number}. {threads} Reply `fix comments` to let Arakun \
         work through them.",
        listed_plain(&who)
    )
}

fn listed_plain(names: &[String]) -> String {
    match names.split_last() {
        Some((last, rest)) if !rest.is_empty() => format!("{} and {last}", rest.join(", ")),
        Some((last, _)) => last.clone(),
        None => "Someone".into(),
    }
}

const QUERY: &str = "query($owner:String!,$name:String!,$number:Int!){\
repository(owner:$owner,name:$name){pullRequest(number:$number){\
number state headRefOid author{login} \
commits(last:1){nodes{commit{statusCheckRollup{contexts(first:100){nodes{__typename \
...on CheckRun{name status conclusion isRequired(pullRequestNumber:$number)} \
...on StatusContext{context state isRequired(pullRequestNumber:$number)}}}}}}} \
reviews(last:50){nodes{id state submittedAt author{login}}} \
reviewThreads(first:100){nodes{isResolved}}}}}";

/// Reads the pull request at `url` with `gh`. It blocks: off the UI thread.
pub fn read(dir: &Path, url: &str) -> Read {
    let Some((slug, number)) = parse_url(url) else {
        return Read::Failed;
    };
    let Some((owner, name)) = slug.split_once('/') else {
        return Read::Failed;
    };
    let out = git::output(
        git::gh(dir).args([
            "api",
            "graphql",
            "-f",
            &format!("query={QUERY}"),
            "-f",
            &format!("owner={owner}"),
            "-f",
            &format!("name={name}"),
            "-F",
            &format!("number={number}"),
        ]),
        GIT_REMOTE,
    );
    let Ok(out) = out else {
        return Read::Failed;
    };
    let text = String::from_utf8_lossy(&out.stdout);
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        return if limited(&err) || limited(&text) {
            Read::Limited
        } else {
            Read::Failed
        };
    }
    parse(&text).map_or(Read::Failed, Read::Seen)
}

fn limited(text: &str) -> bool {
    text.to_ascii_lowercase().contains("rate limit")
}

/// GitHub's answer to [`QUERY`].
pub fn parse(text: &str) -> Option<Snapshot> {
    let v: Value = serde_json::from_str(text).ok()?;
    let pr = v.pointer("/data/repository/pullRequest")?;
    if pr.is_null() {
        return None;
    }
    let s = |v: &Value| v.as_str().unwrap_or_default().to_string();
    let state = match pr["state"].as_str()? {
        "OPEN" => PrState::Open,
        "MERGED" => PrState::Merged,
        _ => PrState::Closed,
    };
    let contexts = pr
        .pointer("/commits/nodes/0/commit/statusCheckRollup/contexts/nodes")
        .and_then(Value::as_array);
    let checks = contexts
        .into_iter()
        .flatten()
        .filter_map(|c| {
            let required = c["isRequired"].as_bool().unwrap_or(false);
            let run = c["__typename"].as_str() == Some("CheckRun");
            let (name, outcome) = match c["__typename"].as_str()? {
                "CheckRun" => {
                    let outcome = if c["status"].as_str() != Some("COMPLETED") {
                        Outcome::Pending
                    } else {
                        match c["conclusion"].as_str().unwrap_or_default() {
                            "FAILURE" | "TIMED_OUT" | "STARTUP_FAILURE" | "ACTION_REQUIRED" => {
                                Outcome::Failed
                            }
                            _ => Outcome::Passed,
                        }
                    };
                    (s(&c["name"]), outcome)
                }
                "StatusContext" => {
                    let outcome = match c["state"].as_str().unwrap_or_default() {
                        "FAILURE" | "ERROR" => Outcome::Failed,
                        "SUCCESS" => Outcome::Passed,
                        _ => Outcome::Pending,
                    };
                    (s(&c["context"]), outcome)
                }
                _ => return None,
            };
            Some(Check {
                name,
                required,
                outcome,
                run,
            })
        })
        .collect();
    let reviews = pr
        .pointer("/reviews/nodes")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|r| r.is_object())
        .map(|r| Review {
            id: s(&r["id"]),
            author: s(&r["author"]["login"]),
            state: s(&r["state"]),
            submitted_at: r["submittedAt"].as_str().and_then(crate::when::parse_iso),
        })
        .filter(|r| !r.id.is_empty())
        .collect();
    let open_threads = pr
        .pointer("/reviewThreads/nodes")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|t| t["isResolved"].as_bool() == Some(false))
        .count();
    Some(Snapshot {
        state,
        number: pr["number"].as_u64().unwrap_or_default(),
        author: s(&pr["author"]["login"]),
        head: s(&pr["headRefOid"]),
        checks,
        reviews,
        open_threads,
    })
}

/// Off the UI thread: reads one pull request and, when it has news, asks
/// `issue_closed(organization, issue)` whether its issue closed meanwhile.
/// None from that is taken as open: a line too many beats one lost.
pub fn look(l: &Look, issue_closed: impl Fn(&str, &str) -> Option<bool>) -> Looked {
    let read = read(&l.dir, &l.url);
    let closed = match &read {
        Read::Seen(s) if !news(&l.watch, s).is_empty() => {
            issue_closed(&l.organization, &l.issue).unwrap_or(false)
        }
        _ => false,
    };
    Looked {
        agent_session: l.agent_session.clone(),
        read,
        issue_closed: closed,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn check(name: &str, required: bool, outcome: Outcome) -> Check {
        Check {
            name: name.into(),
            required,
            outcome,
            run: true,
        }
    }

    fn status(name: &str, required: bool, outcome: Outcome) -> Check {
        Check {
            run: false,
            ..check(name, required, outcome)
        }
    }

    fn review(id: &str, author: &str, state: &str, at: u64) -> Review {
        Review {
            id: id.into(),
            author: author.into(),
            state: state.into(),
            submitted_at: Some(at),
        }
    }

    fn snap(head: &str, checks: Vec<Check>, reviews: Vec<Review>) -> Snapshot {
        Snapshot {
            state: PrState::Open,
            number: 7,
            author: "rik".into(),
            head: head.into(),
            checks,
            reviews,
            open_threads: 2,
        }
    }

    fn seen(s: Snapshot) -> Looked {
        Looked {
            agent_session: "a".into(),
            read: Read::Seen(s),
            issue_closed: false,
        }
    }

    #[test]
    fn failed_checks_are_told_once_per_head_once_all_finished() {
        let mut w = Watch::new(100);
        let running = snap(
            "h1",
            vec![
                check("test", true, Outcome::Failed),
                check("clippy", true, Outcome::Pending),
                check("macos", false, Outcome::Failed),
            ],
            vec![],
        );
        assert!(
            apply(&mut w, &seen(running), 200).is_empty(),
            "one still runs"
        );
        let done = snap(
            "h1",
            vec![
                check("test", true, Outcome::Failed),
                check("clippy", true, Outcome::Failed),
                check("macos", false, Outcome::Failed),
            ],
            vec![],
        );
        let lines = apply(&mut w, &seen(done.clone()), 300);
        assert_eq!(
            lines,
            [
                "Required checks failed on pull request #7: `test` and `clippy`. \
              Reply `fix checks` to let Arakun fix it."
            ]
        );
        assert!(
            apply(&mut w, &seen(done.clone()), 400).is_empty(),
            "told once"
        );
        // A saved watch remembers it across a restart.
        let mut back: Watch = serde_json::from_str(&serde_json::to_string(&w).unwrap()).unwrap();
        assert!(apply(&mut back, &seen(done), 500).is_empty());
        // A new head is news again.
        let next = snap("h2", vec![check("test", true, Outcome::Failed)], vec![]);
        assert_eq!(apply(&mut back, &seen(next), 600).len(), 1);
    }

    #[test]
    fn a_pending_status_does_not_hold_back_a_failed_job() {
        let mut w = Watch::new(0);
        let s = snap(
            "h1",
            vec![
                check("test", true, Outcome::Failed),
                check("test", true, Outcome::Failed),
                status("Human approval", true, Outcome::Pending),
            ],
            vec![],
        );
        assert_eq!(
            apply(&mut w, &seen(s), 10),
            ["A required check failed on pull request #7: `test`. \
              Reply `fix checks` to let Arakun fix it."]
        );
    }

    #[test]
    fn a_head_that_passed_is_not_told_when_a_rerun_fails() {
        let mut w = Watch::new(0);
        let pass = snap("h1", vec![check("test", false, Outcome::Passed)], vec![]);
        assert!(apply(&mut w, &seen(pass), 10).is_empty());
        let fail = snap("h1", vec![check("test", false, Outcome::Failed)], vec![]);
        assert!(apply(&mut w, &seen(fail), 20).is_empty());
    }

    #[test]
    fn with_no_required_check_every_check_counts() {
        let mut w = Watch::new(0);
        let s = snap("h1", vec![check("lint", false, Outcome::Failed)], vec![]);
        let lines = apply(&mut w, &seen(s), 10);
        assert_eq!(
            lines,
            ["A check failed on pull request #7: `lint`. \
              Reply `fix checks` to let Arakun fix it."]
        );
    }

    #[test]
    fn new_reviews_from_others_are_told_in_one_line() {
        let mut w = Watch::new(100);
        let s = snap(
            "h1",
            vec![],
            vec![
                review("r0", "cubic-dev-ai", "COMMENTED", 50),
                review("r1", "cubic-dev-ai", "COMMENTED", 150),
                review("r2", "ana", "CHANGES_REQUESTED", 160),
                review("r3", "rik", "COMMENTED", 170),
                review("r4", "bo", "APPROVED", 180),
            ],
        );
        let lines = apply(&mut w, &seen(s.clone()), 200);
        assert_eq!(
            lines,
            [
                "cubic-dev-ai and ana asked for changes on pull request #7. 2 review threads \
              are open. Reply `fix comments` to let Arakun work through them."
            ]
        );
        assert!(apply(&mut w, &seen(s.clone()), 300).is_empty(), "told once");
        let mut more = s;
        more.reviews
            .push(review("r5", "cubic-dev-ai", "COMMENTED", 400));
        more.open_threads = 1;
        assert_eq!(
            apply(&mut w, &seen(more), 500),
            [
                "cubic-dev-ai left comments on pull request #7. 1 review thread is open. \
              Reply `fix comments` to let Arakun work through them."
            ]
        );
    }

    #[test]
    fn watching_ends_on_merge_close_closed_issue_or_a_quiet_week() {
        let mut w = Watch::new(0);
        let mut merged = snap("h1", vec![check("t", true, Outcome::Failed)], vec![]);
        merged.state = PrState::Merged;
        assert!(apply(&mut w, &seen(merged), 10).is_empty());
        assert!(w.ended && !w.live(10));

        let mut w = Watch::new(0);
        let mut looked = seen(snap("h1", vec![check("t", true, Outcome::Failed)], vec![]));
        looked.issue_closed = true;
        assert!(apply(&mut w, &looked, 10).is_empty());
        assert!(w.ended);

        let mut w = Watch::new(0);
        let quiet = snap("h1", vec![], vec![]);
        apply(&mut w, &seen(quiet.clone()), 10);
        apply(&mut w, &seen(quiet), 10 + IDLE_SECS - 1);
        assert!(w.live(10 + IDLE_SECS - 1));
        assert!(!w.live(10 + IDLE_SECS), "nothing changed for a week");
    }

    #[test]
    fn a_rate_limit_holds_every_read_back() {
        let mut w = Watch::new(0);
        assert!(w.due(0));
        let limited = Looked {
            agent_session: "a".into(),
            read: Read::Limited,
            issue_closed: false,
        };
        assert!(apply(&mut w, &limited, 100).is_empty());
        w.looked_at = 100;
        assert!(!w.due(100 + LOOK_SECS));
        assert!(w.due(100 + LIMITED_SECS));
    }

    #[test]
    fn names_are_cleaned_and_long_lists_counted() {
        let names: Vec<String> = ["a`b", "two\nlines", "c", "d", "e", "f", "g"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert_eq!(
            listed(&names),
            "`ab`, `two lines`, `c`, `d`, `e` and 2 more"
        );
        assert_eq!(clean(&"x".repeat(80)).chars().count(), NAME_LIMIT + 1);
    }

    #[test]
    fn github_s_answer_is_read() {
        let text = r#"{"data":{"repository":{"pullRequest":{"number":7,"state":"OPEN",
          "headRefOid":"abc","author":{"login":"rik"},
          "commits":{"nodes":[{"commit":{"statusCheckRollup":{"contexts":{"nodes":[
            {"__typename":"CheckRun","name":"test","status":"COMPLETED","conclusion":"FAILURE","isRequired":true},
            {"__typename":"CheckRun","name":"lint","status":"IN_PROGRESS","conclusion":null,"isRequired":false},
            {"__typename":"StatusContext","context":"cubic","state":"SUCCESS","isRequired":false}]}}}}]},
          "reviews":{"nodes":[{"id":"R1","state":"COMMENTED","submittedAt":"2026-11-28T10:00:00Z","author":{"login":"cubic-dev-ai"}},null]},
          "reviewThreads":{"nodes":[{"isResolved":false},{"isResolved":true}]}}}}}"#;
        let s = parse(text).unwrap();
        assert_eq!(
            (s.number, s.head.as_str(), s.author.as_str()),
            (7, "abc", "rik")
        );
        assert_eq!(
            s.checks,
            [
                check("test", true, Outcome::Failed),
                check("lint", false, Outcome::Pending),
                status("cubic", false, Outcome::Passed),
            ]
        );
        assert_eq!(s.reviews.len(), 1);
        assert_eq!(
            s.reviews[0].submitted_at,
            crate::when::parse_iso("2026-11-28T10:00:00Z")
        );
        assert_eq!(s.open_threads, 1);
        let none = r#"{"data":{"repository":{"pullRequest":null}}}"#;
        assert_eq!(parse(none), None);
    }

    #[test]
    fn a_quiet_pull_request_does_not_ask_linear() {
        // `look` asks about the issue only with news; `news` is the gate.
        let w = Watch::new(0);
        assert!(
            news(
                &w,
                &snap("h", vec![check("t", true, Outcome::Pending)], vec![])
            )
            .is_empty()
        );
    }
}
