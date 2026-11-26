//! Automations that wait for something to happen ("merge #64 when the checks
//! pass and tell me"), made from a sentence in the palette and confirmed
//! there. A rule waits for its trigger, runs its steps once, and expires.
//! The shell checks them where the data already arrives (GitHub's poll, CI
//! runs, sessions); this file is the pure part: the shapes, the checks, the
//! sentence parser and the JSON a model may answer with.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

/// What a rule waits for.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Trigger {
    PrChecksPass {
        repo: String,
        number: u64,
    },
    PrMerged {
        repo: String,
        number: u64,
    },
    CiFailed {
        branch: String,
        /// The project whose CI it watches, filled in when the rule is
        /// added. A failure on the same branch of another project must not
        /// fire it. Rules saved before this was kept have none and never fire.
        #[serde(default)]
        project: Option<PathBuf>,
    },
    SessionFinished {
        id: String,
        title: String,
    },
    /// A moment: a reminder or a timer. Unix seconds.
    At {
        at: u64,
    },
}

impl Trigger {
    pub fn describe(&self) -> String {
        match self {
            Trigger::PrChecksPass { number, .. } => format!("#{number} checks pass"),
            Trigger::PrMerged { number, .. } => format!("#{number} is merged"),
            Trigger::CiFailed { branch, .. } => format!("CI fails on {branch}"),
            Trigger::SessionFinished { title, .. } => format!("“{title}” finishes"),
            Trigger::At { at } => format!("At {}", clock(*at)),
        }
    }

    pub fn icon(&self) -> &'static str {
        match self {
            Trigger::PrChecksPass { .. } | Trigger::PrMerged { .. } => "github",
            Trigger::CiFailed { .. } => "activity01",
            Trigger::SessionFinished { .. } => "message01",
            Trigger::At { .. } => "alarm-clock",
        }
    }

    fn pull(&self) -> Option<(&str, u64)> {
        match self {
            Trigger::PrChecksPass { repo, number } | Trigger::PrMerged { repo, number } => {
                Some((repo, *number))
            }
            _ => None,
        }
    }
}

/// What a rule does when its trigger happens, in order.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Step {
    /// Merges the trigger's pull request (squash unless said).
    Merge {
        #[serde(default = "squash")]
        method: String,
    },
    /// A notice on the island.
    Notify { text: String },
    /// A shell command in a terminal of the project.
    Run { command: String },
    /// A new agent session with this prompt.
    Agent { prompt: String },
}

fn squash() -> String {
    "squash".into()
}

impl Step {
    pub fn describe(&self) -> String {
        match self {
            Step::Merge { method } => match method.as_str() {
                "merge" => "Merge".into(),
                "rebase" => "Rebase and merge".into(),
                _ => "Squash and merge".into(),
            },
            Step::Notify { .. } => "Notify on the island".into(),
            Step::Run { command } => format!("Run `{command}`"),
            Step::Agent { prompt } => format!("Ask an agent: {prompt}"),
        }
    }

    pub fn icon(&self) -> &'static str {
        match self {
            Step::Merge { .. } => "git-merge",
            Step::Notify { .. } => "alarm-clock",
            Step::Run { .. } => "computer-terminal01",
            Step::Agent { .. } => "sparkles",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", content = "why", rename_all = "snake_case")]
pub enum State {
    Waiting,
    /// Fired and running its steps; `Done` or `Failed` once they ended.
    Running,
    Done,
    Expired,
    Cancelled,
    Failed(String),
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Rule {
    pub id: u64,
    pub title: String,
    pub trigger: Trigger,
    pub steps: Vec<Step>,
    /// Where commands and agents run.
    #[serde(default)]
    pub dir: Option<PathBuf>,
    pub created: u64,
    pub expires: u64,
    pub state: State,
    /// The last thing it noticed, for the Automations dialog: "Needs Rik's approval".
    #[serde(default)]
    pub note: String,
    /// A session rule fires on the change from working to done, so it must
    /// have seen the session working first.
    #[serde(default)]
    pub seen_busy: bool,
}

/// Rules last a day unless told otherwise.
pub const DEFAULT_HOURS: u64 = 24;

/// A pull request as the GitHub entity last read it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PullFacts {
    pub repo: String,
    pub number: u64,
    /// "passed", "failed", "running" or "" when unknown.
    pub checks: String,
    pub merged: bool,
    pub open: bool,
    /// GitHub's `mergeable`: MERGEABLE, CONFLICTING, UNKNOWN.
    pub mergeable: String,
    /// GitHub's `reviewDecision`: APPROVED, REVIEW_REQUIRED, CHANGES_REQUESTED, "".
    pub review: String,
    /// Who it waits on for a review, to say so.
    pub reviewers: Vec<String>,
}

/// A failed CI run as a rule sees it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FailedRun {
    /// The project whose CI ran it.
    pub project: PathBuf,
    pub branch: String,
    pub id: u64,
    /// Unix seconds it finished.
    pub at: u64,
}

/// What the app knows right now, gathered by the shell each check.
#[derive(Clone, Debug, Default)]
pub struct Facts {
    pub pulls: Vec<PullFacts>,
    pub failed_runs: Vec<FailedRun>,
    /// Sessions: (id, working).
    pub sessions: Vec<(String, bool)>,
}

impl Facts {
    fn pull(&self, repo: &str, number: u64) -> Option<&PullFacts> {
        self.pulls
            .iter()
            .find(|p| p.number == number && p.repo.eq_ignore_ascii_case(repo))
    }
}

/// What a check of one rule decided.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// Nothing yet; the note says what it waits for.
    Wait(String),
    /// Run the steps now.
    Fire,
    Expired,
}

/// Checks a waiting rule against what is known. Updates `seen_busy` and the note.
pub fn check(rule: &mut Rule, facts: &Facts, now: u64) -> Outcome {
    if rule.state != State::Waiting {
        return Outcome::Wait(rule.note.clone());
    }
    // A reminder never expires unrung: if the app was closed at its moment, it
    // rings on the first check after and says it was missed.
    if now >= rule.expires && !matches!(rule.trigger, Trigger::At { .. }) {
        return Outcome::Expired;
    }
    let merges = rule.steps.iter().any(|s| matches!(s, Step::Merge { .. }));
    let outcome = match &rule.trigger {
        Trigger::PrChecksPass { repo, number } => match facts.pull(repo, *number) {
            None => Outcome::Wait(format!("Waiting for #{number} to load")),
            Some(p) if p.merged => Outcome::Wait(format!("#{number} is already merged")),
            Some(p) if !p.open => Outcome::Wait(format!("#{number} is closed")),
            Some(p) if p.checks == "failed" => Outcome::Wait("Checks failed".into()),
            Some(p) if p.checks != "passed" => Outcome::Wait("Waiting for checks".into()),
            Some(p) if merges => match merge_blocker(p) {
                Some(why) => Outcome::Wait(why),
                None => Outcome::Fire,
            },
            Some(_) => Outcome::Fire,
        },
        Trigger::PrMerged { number, repo } => match facts.pull(repo, *number) {
            Some(p) if p.merged => Outcome::Fire,
            _ => Outcome::Wait(format!("Waiting for #{number} to merge")),
        },
        Trigger::CiFailed {
            project: None,
            branch,
        } => Outcome::Wait(format!(
            "No project was saved with it; add it again to watch {branch}"
        )),
        Trigger::CiFailed {
            project: Some(project),
            branch,
        } => {
            let failed = facts
                .failed_runs
                .iter()
                .any(|r| &r.project == project && &r.branch == branch && r.at >= rule.created);
            if failed {
                Outcome::Fire
            } else {
                Outcome::Wait(format!("Watching CI on {branch}"))
            }
        }
        Trigger::SessionFinished { id, .. } => match facts.sessions.iter().find(|(s, _)| s == id) {
            Some((_, true)) => {
                rule.seen_busy = true;
                Outcome::Wait("Session is working".into())
            }
            Some((_, false)) if rule.seen_busy => Outcome::Fire,
            Some((_, false)) => Outcome::Wait("Waiting for the session to start".into()),
            None => Outcome::Wait("Session not found".into()),
        },
        Trigger::At { at } if now >= *at => Outcome::Fire,
        Trigger::At { at } => Outcome::Wait(format!("Rings at {}", clock(*at))),
    };
    if let Outcome::Wait(note) = &outcome {
        rule.note = note.clone();
    }
    outcome
}

/// Why a pull request can't be merged yet, if it can't.
pub fn merge_blocker(p: &PullFacts) -> Option<String> {
    if p.mergeable == "CONFLICTING" {
        return Some("Conflicts with the base branch".into());
    }
    match p.review.as_str() {
        "REVIEW_REQUIRED" => Some(match p.reviewers.first() {
            Some(who) => format!("Needs {who}'s approval"),
            None => "Needs an approval".into(),
        }),
        "CHANGES_REQUESTED" => Some("Changes were requested".into()),
        _ => None,
    }
}

// ---- sentences -------------------------------------------------------------------------

/// A rule read from a sentence, before the user confirms it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Draft {
    pub title: String,
    pub trigger: Trigger,
    pub steps: Vec<Step>,
    pub hours: u64,
}

fn has_any(text: &str, words: &[&str]) -> bool {
    words.iter().any(|w| text.contains(w))
}

/// Reads common English phrasings. `repo` is the project's GitHub
/// repository, for `#64`; `sessions` are (id, title) to find "when X finishes".
pub fn parse(text: &str, repo: Option<&str>, sessions: &[(String, String)]) -> Option<Draft> {
    if let Some(d) = parse_reminder(text, &Clock::system(crate::memory::now())) {
        return Some(d);
    }
    let t = text.to_lowercase();
    let number = pr_number(&t);
    let when = has_any(&t, &["when", "once ", "after "]);
    if !when {
        return None;
    }
    let trigger = if let (Some(n), Some(repo)) = (number, repo)
        && has_any(
            &t,
            &[
                "checks pass",
                "checks are green",
                "is green",
                "goes green",
                "ci passes",
            ],
        ) {
        Trigger::PrChecksPass {
            repo: repo.to_string(),
            number: n,
        }
    } else if let (Some(n), Some(repo)) = (number, repo)
        && has_any(&t, &["is merged", "gets merged", "merges"])
    {
        Trigger::PrMerged {
            repo: repo.to_string(),
            number: n,
        }
    } else if has_any(&t, &["fails", "breaks"]) && has_any(&t, &["ci", "build", "checks"]) {
        let branch = word_after(text, &["on "])
            .or_else(|| plain_branch(text))
            .unwrap_or_else(|| "main".into());
        Trigger::CiFailed {
            branch,
            project: None,
        }
    } else if has_any(&t, &["finishes", "is done", "finish"]) {
        let (id, title) = sessions
            .iter()
            .filter(|(_, title)| !title.is_empty() && t.contains(&title.to_lowercase()))
            .max_by_key(|(_, title)| title.len())?;
        Trigger::SessionFinished {
            id: id.clone(),
            title: title.clone(),
        }
    } else {
        return None;
    };
    let mut steps = Vec::new();
    let merge_words = ["merge it", "merge #", "squash"];
    if trigger.pull().is_some()
        && matches!(trigger, Trigger::PrChecksPass { .. })
        && (has_any(&t, &merge_words) || t.starts_with("merge"))
    {
        let method = if t.contains("rebase") {
            "rebase"
        } else if t.contains("merge commit") {
            "merge"
        } else {
            "squash"
        };
        steps.push(Step::Merge {
            method: method.into(),
        });
    }
    if let Some(cmd) = between(text, '`') {
        steps.push(Step::Run { command: cmd });
    }
    if let Some(prompt) = after_any(text, &["ask an agent to ", "have an agent "]) {
        steps.push(Step::Agent { prompt });
    }
    let notify = has_any(&t, &["notify me", "tell me", "let me know", "ping me"]);
    if notify || steps.is_empty() {
        steps.push(Step::Notify {
            text: notice_text(&trigger),
        });
    }
    let hours = hours_in(&t).unwrap_or(DEFAULT_HOURS);
    Some(Draft {
        title: title_for(&trigger, &steps),
        trigger,
        steps,
        hours,
    })
}

/// Whether a sentence reads like it asks for something to happen later.
pub fn sounds_like_rule(text: &str) -> bool {
    if asks_reminder(text) {
        return true;
    }
    let t = text.to_lowercase();
    t.split_whitespace().count() >= 4
        && has_any(&t, &["when ", "once ", "after ", "as soon as"])
        && !text.trim_end().ends_with('?')
}

/// Where a fired rule's commands and agents run: only the folder saved with
/// it. Another project would run a command meant for this one, so a rule
/// whose folder is gone fails instead of falling back to what is open.
pub fn workdir(rule: &Rule, exists: impl Fn(&Path) -> bool) -> Result<Option<PathBuf>, String> {
    if !rule
        .steps
        .iter()
        .any(|s| matches!(s, Step::Run { .. } | Step::Agent { .. }))
    {
        return Ok(None);
    }
    match &rule.dir {
        Some(d) if exists(d) => Ok(Some(d.clone())),
        Some(d) => Err(format!("folder is gone: {}", d.display())),
        None => Err("no folder was saved with it".into()),
    }
}

/// The `#64` in a sentence.
pub fn pr_number(text: &str) -> Option<u64> {
    text.split(|c: char| !(c.is_ascii_digit() || c == '#'))
        .find_map(|w| w.strip_prefix('#')?.parse::<u64>().ok())
}

fn notice_text(trigger: &Trigger) -> String {
    match trigger {
        Trigger::PrChecksPass { number, .. } => format!("#{number}: checks passed"),
        Trigger::PrMerged { number, .. } => format!("#{number} was merged"),
        Trigger::CiFailed { branch, .. } => format!("CI failed on {branch}"),
        Trigger::SessionFinished { title, .. } => format!("“{title}” finished"),
        Trigger::At { .. } => "Reminder".into(),
    }
}

/// A short name: "Merge #64 when green".
pub fn title_for(trigger: &Trigger, steps: &[Step]) -> String {
    let merges = steps.iter().any(|s| matches!(s, Step::Merge { .. }));
    match trigger {
        Trigger::PrChecksPass { number, .. } if merges => format!("Merge #{number} when green"),
        Trigger::PrChecksPass { number, .. } => format!("When #{number} is green"),
        Trigger::PrMerged { number, .. } => format!("When #{number} merges"),
        Trigger::CiFailed { branch, .. } => format!("When CI fails on {branch}"),
        Trigger::SessionFinished { title, .. } => format!("When “{title}” finishes"),
        Trigger::At { .. } => match steps.first() {
            Some(Step::Notify { text }) => format!("Reminder: {text}"),
            _ => "Reminder".into(),
        },
    }
}

/// `17:05`, `tomorrow 09:00`, `yesterday 14:05` or `21/11 09:00`: the local time of `at`.
pub fn clock(at: u64) -> String {
    clock_with(at, &Clock::system(crate::memory::now()))
}

fn clock_with(at: u64, c: &Clock) -> String {
    let (Some(t), Some(n)) = ((c.to_local)(at), (c.to_local)(c.now)) else {
        return String::new();
    };
    let hm = format!("{:02}:{:02}", t.hour, t.minute);
    let day = |l: &crate::platform::LocalTime| {
        crate::platform::days_from_civil(l.year.into(), (l.month + 1).into(), l.day.into())
    };
    match day(&t) - day(&n) {
        0 => hm,
        1 => format!("tomorrow {hm}"),
        -1 => format!("yesterday {hm}"),
        _ => format!("{:02}/{:02} {hm}", t.day, t.month + 1),
    }
}

/// A reminder checked this long after its moment says it is late: the app was
/// closed, or the machine slept, when it should have rung.
const LATE: u64 = 5 * 60;

/// How long after its moment a reminder that never rang is still kept.
const MISSED_KEPT: u64 = 7 * 86_400;

/// The notice's title when a reminder rings at `now`: "Reminder", or
/// "Reminder (missed at 14:05)" when it rings late.
pub fn reminder_title(at: u64, now: u64) -> String {
    reminder_title_with(at, &Clock::system(now))
}

fn reminder_title_with(at: u64, c: &Clock) -> String {
    if c.now.saturating_sub(at) >= LATE {
        format!("Reminder (missed at {})", clock_with(at, c))
    } else {
        "Reminder".into()
    }
}

/// Unix seconds to local wall time and back. The system's zone in the app; tests
/// pass a made-up one so they don't depend on the machine's.
pub struct Clock {
    pub now: u64,
    pub to_local: fn(u64) -> Option<crate::platform::LocalTime>,
    /// Year, 0-based month, day, hour, minute; days past the month's end roll over.
    pub from_local: fn(i32, i32, i32, i32, i32) -> Option<u64>,
}

impl Clock {
    pub fn system(now: u64) -> Self {
        Clock {
            now,
            to_local: crate::platform::local_time,
            from_local: crate::platform::from_local,
        }
    }
}

/// Words that ask for a reminder or a timer, matched as whole words.
const REMIND_WORDS: &[&str] = &["remind me", "reminder", "timer", "alarm"];

/// Words that ask for a timer, which may give its length bare: "timer 10 min".
const TIMER_WORDS: &[&str] = &["timer"];

/// The words of a sentence, lower case, without the punctuation around them.
/// `src/timer.rs` stays one word, so it does not read as "timer".
fn words(text: &str) -> Vec<String> {
    text.split_whitespace()
        .map(|w| {
            w.trim_matches(|c: char| !c.is_alphanumeric() && c != '-')
                .to_lowercase()
        })
        .filter(|w| !w.is_empty())
        .collect()
}

/// Where `phrase` (one or more words) first shows as whole words in `words`.
fn find_phrase(words: &[String], phrase: &str) -> Option<usize> {
    let p: Vec<&str> = phrase.split_whitespace().collect();
    if p.is_empty() || p.len() > words.len() {
        return None;
    }
    (0..=words.len() - p.len()).find(|&i| p.iter().zip(&words[i..]).all(|(a, b)| a == b))
}

fn has_phrase(words: &[String], phrases: &[&str]) -> bool {
    phrases.iter().any(|p| find_phrase(words, p).is_some())
}

/// Whether the text asks for a reminder or a timer at all.
fn asks_reminder(text: &str) -> bool {
    has_phrase(&words(text), REMIND_WORDS)
}

/// A reminder or a timer in a sentence: "remind me in 20 min to review the PR",
/// "reminder tomorrow at 9 to call Dani", "remind me at 5pm to stretch",
/// "timer 10 min". `clock` places clock times on the local day.
pub fn parse_reminder(text: &str, clock: &Clock) -> Option<Draft> {
    let w = words(text);
    if !has_phrase(&w, REMIND_WORDS) {
        return None;
    }
    let low = super::rank::Lowered::new(text);
    let t = low.text.as_str();
    let bare = has_phrase(&w, TIMER_WORDS);
    let (at, mut cuts) = relative_time(t, clock.now, bare)
        .map(|(at, span)| (at, vec![span]))
        .or_else(|| clock_time(t, clock))?;
    // The cuts are offsets in `t`, where some letters changed length (`İ`
    // grows, the Kelvin sign shrinks). Map them back and cut the typed text,
    // so the message keeps its case.
    cuts.sort();
    let mut kept = String::new();
    let mut from = 0;
    for (a, b) in cuts {
        let r = low.back(a..b);
        if r.start >= from {
            kept.push_str(&text[from..r.start]);
            kept.push(' ');
            from = r.end;
        }
    }
    kept.push_str(&text[from..]);
    let what = what_to_remember(&kept);
    let text = if what.is_empty() {
        "Time's up".to_string()
    } else {
        what
    };
    let steps = vec![Step::Notify { text }];
    let trigger = Trigger::At { at };
    Some(Draft {
        title: title_for(&trigger, &steps),
        trigger,
        steps,
        hours: 0,
    })
}

/// "set a reminder to buy milk": a reminder with no time in it.
/// What to remember, so it can be kept as a to-do instead of being lost.
pub fn timeless_reminder(text: &str) -> Option<String> {
    if !asks_reminder(text) {
        return None;
    }
    // Any clock does: only whether a time is there matters.
    if parse_reminder(text, &Clock::system(crate::memory::now())).is_some() {
        return None;
    }
    Some(what_to_remember(text)).filter(|w| !w.is_empty())
}

/// The sentence without its asking words: what the person wants to remember.
fn what_to_remember(text: &str) -> String {
    const ASKING: &[&str] = &["set a", "set an", "make a"];
    const LEADS: &[&str] = &["to", "about", "of", "for"];
    let mut kept: Vec<&str> = text.split_whitespace().collect();
    for phrase in REMIND_WORDS.iter().chain(ASKING) {
        let lower: Vec<String> = kept.iter().map(|k| words(k).concat()).collect();
        if let Some(i) = find_phrase(&lower, phrase) {
            kept.drain(i..i + phrase.split_whitespace().count());
        }
    }
    // A lone comma left where a time was cut ("remind me tomorrow, at 5pm,
    // to stretch") would hide the "to" after it. Only such separators go: an
    // emoji has no letters either, but it can be the whole message.
    while kept.first().is_some_and(|k| {
        k.chars().all(|c| matches!(c, ',' | ';' | ':' | '.'))
            || LEADS.contains(&words(k).concat().as_str())
    }) {
        kept.remove(0);
    }
    // A period ends the sentence but may also start the message (".env").
    kept.join(" ")
        .trim_start_matches(|c: char| c == ',' || c == ':' || c.is_whitespace())
        .trim_end_matches(|c: char| c == ',' || c == '.' || c == ':' || c.is_whitespace())
        .to_string()
}

/// Whether byte `i` of `t` starts a word.
fn word_start(t: &str, i: usize) -> bool {
    t[..i]
        .chars()
        .next_back()
        .is_none_or(|c| !c.is_alphanumeric())
}

/// "20 min", "2 hours", "half an hour" at the start of `after`: minutes and bytes used.
fn duration(after: &str) -> Option<(f64, usize)> {
    for (words, minutes) in [("half an hour", 30.0), ("an hour", 60.0)] {
        if after.starts_with(words) && !word_start_after(after, words.len()) {
            return Some((minutes, words.len()));
        }
    }
    let num: String = after
        .chars()
        .take_while(|c| c.is_ascii_digit() || *c == '.')
        .collect();
    let v = num.parse::<f64>().ok()?;
    let unit_start = num.len() + after[num.len()..].len() - after[num.len()..].trim_start().len();
    let unit: String = after[unit_start..]
        .chars()
        .take_while(|c| c.is_alphabetic())
        .collect();
    let minutes = match unit.as_str() {
        "s" | "sec" | "secs" | "second" | "seconds" => v / 60.,
        "m" | "min" | "mins" | "minute" | "minutes" => v,
        "h" | "hr" | "hrs" | "hour" | "hours" => v * 60.,
        "d" | "day" | "days" => v * 1440.,
        _ => return None,
    };
    Some((minutes, unit_start + unit.len()))
}

/// Whether a letter or digit follows byte `i` of `t`, so a word there goes on.
fn word_start_after(t: &str, i: usize) -> bool {
    t[i..].chars().next().is_some_and(char::is_alphanumeric)
}

/// "in 20 min", "in 2 hours", "in half an hour": the moment and where it was in `t`.
/// With `bare` (a timer), a length on its own counts too: "timer 10 min", "10 min timer".
fn relative_time(t: &str, now: u64, bare: bool) -> Option<(u64, (usize, usize))> {
    let heads: &[&str] = if bare { &["in ", ""] } else { &["in "] };
    for head in heads {
        let starts: Vec<usize> = if head.is_empty() {
            t.char_indices().map(|(i, _)| i).collect()
        } else {
            t.match_indices(head).map(|(i, _)| i).collect()
        };
        for i in starts {
            if !word_start(t, i) {
                continue;
            }
            let after = &t[i + head.len()..];
            let Some((minutes, used)) = duration(after) else {
                continue;
            };
            if word_start_after(after, used) {
                continue;
            }
            let secs = (minutes * 60.0).round() as u64;
            if secs == 0 {
                continue;
            }
            return Some((now + secs, (i, i + head.len() + used)));
        }
    }
    None
}

/// Where "tomorrow" shows in `t` as a whole word meaning the next day, with
/// its length.
fn tomorrow_words(t: &str) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    let word = "tomorrow";
    for (j, _) in t.match_indices(word) {
        let after = &t[j + word.len()..];
        if word_start(t, j)
            && !word_start_after(t, j + word.len())
            // "tomorrow's meeting" names a thing, not the day.
            && !after.starts_with("'s")
            && !after.starts_with("’s")
        {
            out.push((j, word.len()));
        }
    }
    out
}

/// "at 5pm", "at 17:30", "tomorrow at 9am": the moment and the
/// spans of `t` that said it. The day is counted in local dates and the time placed
/// with `clock`, so a day that changes to or from summer time still rings at the hour
/// said.
fn clock_time(t: &str, clock: &Clock) -> Option<(u64, Vec<(usize, usize)>)> {
    let head = "at ";
    for (i, _) in t.match_indices(head) {
        if !word_start(t, i) {
            continue;
        }
        let after = &t[i + head.len()..];
        let hour: String = after.chars().take_while(|c| c.is_ascii_digit()).collect();
        let Ok(mut h) = hour.parse::<i32>() else {
            continue;
        };
        let mut used = hour.len();
        let mut m = 0;
        if let Some(r) = after[used..].strip_prefix(':') {
            let min: String = r.chars().take_while(|c| c.is_ascii_digit()).collect();
            m = min.parse().unwrap_or(0);
            used += 1 + min.len();
        }
        let tail = after[used..].trim_start();
        let gap = after[used..].len() - tail.len();
        let mut pm = None;
        for (word, is_pm) in [("pm", true), ("p.m.", true), ("am", false), ("a.m.", false)] {
            if tail.starts_with(word) && !word_start_after(tail, word.len()) {
                pm = Some(is_pm);
                used += gap + word.len();
                break;
            }
        }
        match pm {
            Some(true) if h < 12 => h += 12,
            Some(false) if h == 12 => h = 0,
            _ => {}
        }
        if h > 23 || m > 59 {
            continue;
        }
        // "Tomorrow" sets the day only next to the time or to the asking
        // words: in "remind me at 5pm to plan tomorrow" it is the message.
        // A comma between them ("at 5pm, tomorrow") still keeps them together.
        let next_to = |gap: &str| gap.chars().all(|c| c.is_whitespace() || c == ',');
        let mut start = i;
        let mut end = i + head.len() + used;
        let mut extra = None;
        let mut tomorrow = false;
        for (j, len) in tomorrow_words(t) {
            if j + len <= start && next_to(&t[j + len..start]) {
                start = j;
                tomorrow = true;
            } else if j >= end && next_to(&t[end..j]) {
                end = j + len;
                tomorrow = true;
            } else {
                let pre = t[..j].trim_end();
                if REMIND_WORDS
                    .iter()
                    .any(|p| pre.ends_with(p) && word_start(pre, pre.len() - p.len()))
                {
                    extra = Some((j, j + len));
                    tomorrow = true;
                }
            }
        }
        let today = (clock.to_local)(clock.now)?;
        let on = |days: i32| (clock.from_local)(today.year, today.month, today.day + days, h, m);
        let at = if tomorrow {
            on(1)?
        } else {
            // A time already gone today means the next one.
            on(0).filter(|at| *at > clock.now).or_else(|| on(1))?
        };
        let mut spans = vec![(start, end)];
        // Said twice ("reminder tomorrow at 9") it is cut once.
        spans.extend(extra.filter(|&(j, _)| j < start || j >= end));
        return Some((at, spans));
    }
    None
}

fn word_after(text: &str, heads: &[&str]) -> Option<String> {
    for h in heads {
        for end in ends_of(text, h) {
            let raw = text[end..].split_whitespace().next().unwrap_or("");
            let Some(w) = branch_token(raw) else {
                continue;
            };
            if w.contains('-') || w.contains('/') || w == "main" || w == "master" {
                return Some(w);
            }
        }
    }
    None
}

/// A typed word as a branch name, without the punctuation of the sentence
/// around it. A word with other characters is no branch at all: a prefix
/// of it ("release" of "release+hotfix") would watch the wrong one.
fn branch_token(raw: &str) -> Option<String> {
    let w = raw
        .trim_start_matches(['(', '[', '"', '\'', '«', '“'])
        .trim_end_matches(['.', ',', ';', ':', '!', '?', ')', ']', '"', '\'', '»', '”']);
    (!w.is_empty()
        && w.chars()
            .all(|c| c.is_alphanumeric() || matches!(c, '-' | '/' | '_' | '.' | '+' | '@')))
    .then(|| w.to_string())
}

/// A branch named without `-` or `/` ("CI fails on develop"), read only
/// after the CI is named, so "notify me on Slack when CI fails" has none.
fn plain_branch(text: &str) -> Option<String> {
    const CI: &[&str] = &["ci", "build", "checks", "fails", "breaks"];
    const HEADS: &[&str] = &["on", "branch"];
    // Skipped between a head and the branch: "on the develop branch".
    const ARTICLES: &[&str] = &["the", "my", "our"];
    const NOT_BRANCHES: &[&str] = &[
        "ci", "build", "checks", "branch", "project", "repo", "a", "an", "any", "this",
    ];
    let words: Vec<String> = text
        .split_whitespace()
        .map(|w| branch_token(w).unwrap_or_default())
        .collect();
    let lower = |w: &str| w.to_lowercase();
    let start = words.iter().position(|w| CI.contains(&lower(w).as_str()))?;
    let mut rest = words[start + 1..].iter();
    while let Some(w) = rest.next() {
        if !HEADS.contains(&lower(w).as_str()) {
            continue;
        }
        let next = rest
            .clone()
            .find(|w| !ARTICLES.contains(&lower(w).as_str()))?;
        if !next.is_empty() && !NOT_BRANCHES.contains(&lower(next).as_str()) {
            return Some(next.clone());
        }
    }
    None
}

/// The byte offsets in `text` just past each place `head` (lower case) shows,
/// ignoring case. Offsets found in `text.to_lowercase()` can't be used on
/// `text`: lower case changes some letters' length ("İ" grows a byte), and
/// slicing there could cut a letter in half and panic.
fn ends_of<'a>(text: &'a str, head: &'a str) -> impl Iterator<Item = usize> + 'a {
    text.char_indices().filter_map(move |(i, _)| {
        let mut want = head;
        let mut used = 0;
        for c in text[i..].chars() {
            if want.is_empty() {
                break;
            }
            let mut buf = [0; 4];
            let mut lower = c.to_lowercase();
            // A letter whose lower case is more than one letter is compared
            // letter by letter.
            let rest = lower.try_fold(want, |w, l| w.strip_prefix(&*l.encode_utf8(&mut buf)))?;
            want = rest;
            used += c.len_utf8();
        }
        want.is_empty().then_some(i + used)
    })
}

fn between(text: &str, quote: char) -> Option<String> {
    let start = text.find(quote)? + quote.len_utf8();
    let len = text[start..].find(quote)?;
    let inner = text[start..start + len].trim();
    (!inner.is_empty()).then(|| inner.to_string())
}

fn after_any(text: &str, heads: &[&str]) -> Option<String> {
    heads.iter().find_map(|h| {
        let end = ends_of(text, h).next()?;
        let rest = text[end..].trim();
        (!rest.is_empty()).then(|| rest.to_string())
    })
}

/// "for 3 hours", "48h".
fn hours_in(t: &str) -> Option<u64> {
    let words: Vec<&str> = t.split_whitespace().collect();
    for (i, w) in words.iter().enumerate() {
        if let Some(n) = w.strip_suffix('h').and_then(|n| n.parse().ok()) {
            return Some(n);
        }
        if let Ok(n) = w.parse::<u64>()
            && words.get(i + 1).is_some_and(|u| u.starts_with("hour"))
        {
            return Some(n);
        }
    }
    None
}

// ---- the model's JSON ------------------------------------------------------------------

/// The JSON schema the small model must answer with (structured output).
pub fn schema() -> Value {
    json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["understood", "trigger", "steps", "hours"],
        "properties": {
            "understood": {"type": "boolean"},
            "trigger": {
                "type": "object",
                "additionalProperties": false,
                "required": ["type", "number", "branch", "session", "minutes"],
                "properties": {
                    "type": {"type": "string", "enum": ["pr_checks_pass", "pr_merged", "ci_failed", "session_finished", "at"]},
                    "minutes": {"type": "integer"},
                    "number": {"type": "integer"},
                    "branch": {"type": "string"},
                    "session": {"type": "string"}
                }
            },
            "steps": {
                "type": "array",
                "items": {
                    "type": "object",
                    "additionalProperties": false,
                    "required": ["type", "text"],
                    "properties": {
                        "type": {"type": "string", "enum": ["merge", "notify", "run", "agent"]},
                        "text": {"type": "string"}
                    }
                }
            },
            "hours": {"type": "integer"}
        }
    })
}

/// Checks the model's answer and turns it into a draft. Anything off (an
/// unknown type, a merge without a pull request, a session that does not
/// exist, an empty command) refuses the whole answer.
pub fn from_json(
    v: &Value,
    repo: Option<&str>,
    sessions: &[(String, String)],
) -> Result<Draft, String> {
    if v["understood"] != json!(true) {
        return Err("not an automation".into());
    }
    let tr = &v["trigger"];
    let number = tr["number"].as_u64().filter(|n| *n > 0);
    let trigger = match tr["type"].as_str().unwrap_or("") {
        kind @ ("pr_checks_pass" | "pr_merged") => {
            let n = number.ok_or("a pull request trigger needs its number")?;
            let repo = repo
                .ok_or("this project has no GitHub repository")?
                .to_string();
            if kind == "pr_merged" {
                Trigger::PrMerged { repo, number: n }
            } else {
                Trigger::PrChecksPass { repo, number: n }
            }
        }
        "ci_failed" => {
            let b = tr["branch"].as_str().unwrap_or("").trim();
            if b.is_empty() || b.contains(char::is_whitespace) {
                return Err("a CI trigger needs a branch".into());
            }
            Trigger::CiFailed {
                branch: b.to_string(),
                project: None,
            }
        }
        "session_finished" => {
            let want = tr["session"].as_str().unwrap_or("").trim().to_lowercase();
            let (id, title) = sessions
                .iter()
                .find(|(id, title)| {
                    !want.is_empty() && (id.to_lowercase() == want || title.to_lowercase() == want)
                })
                .ok_or("no session by that name")?;
            Trigger::SessionFinished {
                id: id.clone(),
                title: title.clone(),
            }
        }
        "at" => {
            let m = tr["minutes"]
                .as_u64()
                .filter(|m| *m > 0 && *m <= 60 * 24 * 30)
                .ok_or("a reminder needs minutes from now")?;
            Trigger::At {
                at: crate::memory::now() + m * 60,
            }
        }
        other => return Err(format!("unknown trigger {other:?}")),
    };
    let mut steps = Vec::new();
    for s in v["steps"].as_array().ok_or("steps must be a list")? {
        let text = s["text"].as_str().unwrap_or("").trim().to_string();
        steps.push(match s["type"].as_str().unwrap_or("") {
            "merge" => {
                if !matches!(trigger, Trigger::PrChecksPass { .. }) {
                    return Err("only a checks-pass rule can merge".into());
                }
                let method = match text.as_str() {
                    "merge" | "rebase" => text,
                    _ => "squash".into(),
                };
                Step::Merge { method }
            }
            "notify" => Step::Notify {
                text: if text.is_empty() {
                    notice_text(&trigger)
                } else {
                    text
                },
            },
            "run" if !text.is_empty() => Step::Run { command: text },
            "agent" if !text.is_empty() => Step::Agent { prompt: text },
            other => return Err(format!("bad step {other:?}")),
        });
    }
    if steps.is_empty() || steps.len() > 4 {
        return Err("a rule needs one to four steps".into());
    }
    let hours = v["hours"]
        .as_u64()
        .unwrap_or(DEFAULT_HOURS)
        .clamp(1, 24 * 7);
    Ok(Draft {
        title: title_for(&trigger, &steps),
        trigger,
        steps,
        hours,
    })
}

// ---- the store -------------------------------------------------------------------------

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Rules {
    #[serde(default)]
    pub next_id: u64,
    #[serde(default)]
    pub items: Vec<Rule>,
    /// The last save failed: what is on disk is older than `items`, and the
    /// next check saves again.
    #[serde(skip)]
    pub unsaved: bool,
}

impl Rules {
    pub fn add(&mut self, d: Draft, dir: Option<PathBuf>, now: u64) -> u64 {
        let expires_at = match d.trigger {
            Trigger::At { at } => Some(at),
            _ => None,
        };
        self.next_id += 1;
        let id = self.next_id;
        self.items.push(Rule {
            id,
            title: d.title,
            trigger: d.trigger,
            steps: d.steps,
            dir,
            created: now,
            // A reminder is kept a week past its moment; it still rings once if
            // the app was closed then (see `check`).
            expires: match expires_at {
                Some(at) => at.max(now) + MISSED_KEPT,
                None => now + d.hours * 3600,
            },
            state: State::Waiting,
            note: String::new(),
            seen_busy: false,
        });
        id
    }

    pub fn cancel(&mut self, id: u64) -> bool {
        match self.items.iter_mut().find(|r| r.id == id) {
            Some(r) if r.state == State::Waiting => {
                r.state = State::Cancelled;
                true
            }
            _ => false,
        }
    }

    /// Ends a running rule from how its steps went: `Done` only when they
    /// all ran, `Failed` with why otherwise.
    pub fn finish(&mut self, id: u64, result: Result<(), String>) -> bool {
        match self.items.iter_mut().find(|r| r.id == id) {
            Some(r) if r.state == State::Running => {
                match result {
                    Ok(()) => r.state = State::Done,
                    Err(why) => {
                        r.note = why.clone();
                        r.state = State::Failed(why);
                    }
                }
                true
            }
            _ => false,
        }
    }

    pub fn waiting(&self) -> impl Iterator<Item = &Rule> {
        self.items.iter().filter(|r| r.state == State::Waiting)
    }

    /// Drops finished rules after a week so the file stays small.
    pub fn tidy(&mut self, now: u64) {
        self.items
            .retain(|r| r.state == State::Waiting || now.saturating_sub(r.expires) < 7 * 86_400);
    }

    pub fn load() -> Self {
        Self::load_from(&file())
    }

    /// Rules this build can't read (a trigger from a newer Arakun) are set
    /// aside: saved over, their record of having run would be lost and they
    /// would run again once the file was restored.
    fn load_from(path: &Path) -> Self {
        let mut rules: Self = crate::stored::load(path).unwrap_or_default();
        rules.interrupted();
        rules
    }

    /// A rule still running when the app last closed may have done only some
    /// of its steps. It is not run again (a command could run twice); it
    /// says it was cut short.
    fn interrupted(&mut self) {
        for r in &mut self.items {
            if r.state == State::Running {
                let why = "Arakun closed before its steps finished".to_string();
                r.note = why.clone();
                r.state = State::Failed(why);
            }
        }
    }

    /// Writes the rules to disk. A rule's state counts only once it is there:
    /// after a restart the file is all that is left.
    pub fn save(&mut self) -> Result<(), String> {
        self.save_with(|bytes| crate::stored::write(&file(), bytes))
    }

    fn save_with(
        &mut self,
        write: impl FnOnce(&[u8]) -> std::io::Result<()>,
    ) -> Result<(), String> {
        let done = serde_json::to_vec_pretty(&*self)
            .map_err(|e| e.to_string())
            .and_then(|bytes| write(&bytes).map_err(|e| e.to_string()));
        self.unsaved = done.is_err();
        done
    }

    /// Saves after `fired` were set Running, and says whether they may run.
    /// A rule runs only once Running is on disk: else the file still says
    /// Waiting, and after a restart it would run again. When the save fails
    /// they go back to Waiting, and the next check tries again.
    pub fn save_fired(&mut self, fired: &[u64]) -> Result<(), String> {
        self.save_fired_with(fired, |bytes| crate::stored::write(&file(), bytes))
    }

    fn save_fired_with(
        &mut self,
        fired: &[u64],
        write: impl FnOnce(&[u8]) -> std::io::Result<()>,
    ) -> Result<(), String> {
        let done = self.save_with(write);
        if done.is_err() {
            for r in &mut self.items {
                if fired.contains(&r.id) && r.state == State::Running {
                    r.state = State::Waiting;
                }
            }
        }
        done
    }
}

fn file() -> PathBuf {
    crate::paths::store("rules.json")
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::platform::{LocalTime, civil_from_days, days_from_civil};

    /// A made-up zone, so tests don't depend on the machine's: UTC-8, and UTC-7
    /// from 2026-03-08 10:00 UTC (02:00 local), when summer time starts.
    const SPRING: i64 = 20_520 * 86_400 + 10 * 3600;

    fn offset(unix: i64) -> i64 {
        if unix < SPRING { -8 * 3600 } else { -7 * 3600 }
    }

    fn zone_to_local(unix: u64) -> Option<LocalTime> {
        let local = unix as i64 + offset(unix as i64);
        let days = local.div_euclid(86_400);
        let secs = local.rem_euclid(86_400);
        let (y, m, d) = civil_from_days(days);
        Some(LocalTime {
            year: y as i32,
            month: m as i32 - 1,
            day: d as i32,
            hour: (secs / 3600) as i32,
            minute: (secs % 3600 / 60) as i32,
            wday: (days + 4).rem_euclid(7) as i32,
        })
    }

    fn zone_from_local(y: i32, mo: i32, d: i32, h: i32, mi: i32) -> Option<u64> {
        let days = days_from_civil(y.into(), (mo + 1).into(), 1) + i64::from(d) - 1;
        let naive = days * 86_400 + i64::from(h * 3600 + mi * 60);
        let winter = naive + 8 * 3600;
        let at = if winter < SPRING {
            winter
        } else {
            naive + 7 * 3600
        };
        u64::try_from(at).ok()
    }

    /// Unix seconds of a local wall time in the made-up zone.
    fn local(y: i32, mo: i32, d: i32, h: i32, mi: i32) -> u64 {
        zone_from_local(y, mo - 1, d, h, mi).unwrap()
    }

    fn zone(now: u64) -> Clock {
        Clock {
            now,
            to_local: zone_to_local,
            from_local: zone_from_local,
        }
    }

    fn notice(d: &Draft) -> &str {
        match d.steps.as_slice() {
            [Step::Notify { text }] => text,
            _ => panic!("not one notice: {:?}", d.steps),
        }
    }

    #[test]
    fn reminders_and_timers_are_read() {
        // Thursday 15 January 2026, 10:00 local.
        let now = local(2026, 1, 15, 10, 0);
        let c = zone(now);
        let d = parse_reminder("remind me in 20 min to review Rik's PR", &c).unwrap();
        assert_eq!(d.trigger, Trigger::At { at: now + 20 * 60 });
        assert_eq!(notice(&d), "review Rik's PR");
        let d = parse_reminder("set a reminder to call Dani at 5pm", &c).unwrap();
        assert_eq!(d.trigger, Trigger::At { at: now + 7 * 3600 });
        assert_eq!(notice(&d), "call Dani");
        let d = parse_reminder("reminder tomorrow at 9 to pay the power bill", &c).unwrap();
        assert_eq!(
            d.trigger,
            Trigger::At {
                at: local(2026, 1, 16, 9, 0)
            }
        );
        assert_eq!(notice(&d), "pay the power bill");
        let d = parse_reminder("remind me at 17:30 to stretch", &c).unwrap();
        assert_eq!(
            d.trigger,
            Trigger::At {
                at: local(2026, 1, 15, 17, 30)
            }
        );
        assert_eq!(notice(&d), "stretch");
        // A time already gone today rings tomorrow.
        let d = parse_reminder("remind me at 8 am to run", &c).unwrap();
        assert_eq!(
            d.trigger,
            Trigger::At {
                at: local(2026, 1, 16, 8, 0)
            }
        );
        assert_eq!(notice(&d), "run");
        let d = parse_reminder("set a timer in 10 minutes", &c).unwrap();
        assert_eq!(d.trigger, Trigger::At { at: now + 600 });
        assert_eq!(notice(&d), "Time's up");
        let d = parse_reminder("remind me in half an hour to take out the laundry", &c).unwrap();
        assert_eq!(d.trigger, Trigger::At { at: now + 1800 });
        assert_eq!(notice(&d), "take out the laundry");
        let d = parse_reminder("alarm in 5 min", &c).unwrap();
        assert_eq!(notice(&d), "Time's up");
        assert!(parse_reminder("in 20 min review the PR", &c).is_none());
        assert!(sounds_like_rule("set a reminder for something"));
        assert!(title_for(&d.trigger, &d.steps).starts_with("Reminder: "));
    }

    #[test]
    fn reminders_with_letters_that_change_length_do_not_panic() {
        let now = local(2026, 1, 15, 10, 0);
        let c = zone(now);
        // `İ` grows and the Kelvin sign shrinks when lowered: the totals match,
        // but the span's end falls inside `é` in the typed text.
        let d = parse_reminder("İİ reminder in 5 min é\u{212A}", &c).unwrap();
        assert_eq!(d.trigger, Trigger::At { at: now + 300 });
        assert!(notice(&d).contains('é'));
        // Letters that keep their length keep their case too.
        let d = parse_reminder("remind me in 5 min to call José 🦀", &c).unwrap();
        assert_eq!(notice(&d), "call José 🦀");
        let d = parse_reminder("İstanbul: remind me in 10 min", &c).unwrap();
        assert_eq!(d.trigger, Trigger::At { at: now + 600 });
        // The message is cut from the typed text, so one `İ` keeps the case
        // of the rest, paths included.
        let d = parse_reminder("remind me in 5 min Review İstanbul/README.md", &c).unwrap();
        assert_eq!(notice(&d), "Review İstanbul/README.md");
        let d = parse_reminder("İİ reminder in 5 min é\u{212A}", &c).unwrap();
        assert_eq!(notice(&d), "İİ é\u{212A}");
        let d = parse_reminder("remind me tomorrow at 9 to see İzmir", &c).unwrap();
        assert_eq!(notice(&d), "see İzmir");
    }

    #[test]
    fn tomorrow_counts_only_next_to_the_time() {
        // Thursday 15 January 2026, 10:00 local.
        let c = zone(local(2026, 1, 15, 10, 0));
        let at = |text: &str| match parse_reminder(text, &c).unwrap().trigger {
            Trigger::At { at } => at,
            t => panic!("{t:?}"),
        };
        let today = local(2026, 1, 15, 17, 0);
        let tomorrow = local(2026, 1, 16, 17, 0);
        let d = parse_reminder("remind me at 5pm to plan tomorrow", &c).unwrap();
        assert_eq!(d.trigger, Trigger::At { at: today });
        assert_eq!(notice(&d), "plan tomorrow");
        assert_eq!(at("remind me tomorrow at 5pm to stretch"), tomorrow);
        let d = parse_reminder("remind me at 5pm tomorrow to stretch", &c).unwrap();
        assert_eq!(d.trigger, Trigger::At { at: tomorrow });
        assert_eq!(notice(&d), "stretch");
        let d = parse_reminder("remind me tomorrow to call Dani at 5pm", &c).unwrap();
        assert_eq!(d.trigger, Trigger::At { at: tomorrow });
        assert_eq!(notice(&d), "call Dani");
        let d = parse_reminder("remind me to stretch at 5pm, tomorrow", &c).unwrap();
        assert_eq!(d.trigger, Trigger::At { at: tomorrow });
        assert_eq!(notice(&d), "stretch");
        let d = parse_reminder("remind me tomorrow, at 5pm, to stretch", &c).unwrap();
        assert_eq!(d.trigger, Trigger::At { at: tomorrow });
        assert_eq!(notice(&d), "stretch");
        let d = parse_reminder("remind me in 5 minutes 🔑", &c).unwrap();
        assert_eq!(notice(&d), "🔑");
        let d = parse_reminder("remind me in 5 minutes 🔑 keys", &c).unwrap();
        assert_eq!(notice(&d), "🔑 keys");
        let d = parse_reminder("remind me in 5 minutes .env", &c).unwrap();
        assert_eq!(notice(&d), ".env");
        let d = parse_reminder("remind me at 5pm, tomorrow's meeting starts at 9", &c).unwrap();
        assert_eq!(d.trigger, Trigger::At { at: today });
        assert_eq!(notice(&d), "tomorrow's meeting starts at 9");
    }

    #[test]
    fn a_timer_can_give_its_length_alone() {
        let now = local(2026, 1, 15, 10, 0);
        let c = zone(now);
        for (text, mins) in [
            ("timer 10 min", 10),
            ("Timer 10min", 10),
            ("10 min timer", 10),
            ("set a timer for 1 hour", 60),
            ("timer half an hour", 30),
        ] {
            let d = parse_reminder(text, &c).unwrap_or_else(|| panic!("{text:?}"));
            assert_eq!(
                d.trigger,
                Trigger::At {
                    at: now + mins * 60
                },
                "{text:?}"
            );
            assert_eq!(notice(&d), "Time's up", "{text:?}");
        }
        // A bare length is only a timer's: a reminder needs "in" or a clock time.
        assert!(parse_reminder("remind me to check the 2 days of logs", &c).is_none());
    }

    #[test]
    fn reminder_words_match_whole_words_only() {
        let c = zone(local(2026, 1, 15, 10, 0));
        for text in [
            "fix the timeranges bug in 10 min",
            "open src/alarms.rs in 5 min",
            "open src/timer.rs",
            "the alarmed users at 5pm",
            "reminders.rs fails in 2 min",
        ] {
            assert!(parse_reminder(text, &c).is_none(), "{text:?}");
            assert!(!sounds_like_rule(text), "{text:?}");
        }
        // Heads of times are whole words too: "chat 5pm" is not "at 5pm".
        assert!(parse_reminder("reminder: chat 5pm", &c).is_none());
        let d = parse_reminder("Reminder, at 5pm: deploy", &c).unwrap();
        assert_eq!(notice(&d), "deploy");
    }

    #[test]
    fn a_clock_time_across_summer_time_keeps_its_hour() {
        // Saturday 7 March 2026, 18:00 winter time; summer time starts overnight.
        let now = local(2026, 3, 7, 18, 0);
        let c = zone(now);
        let d = parse_reminder("remind me tomorrow at 9 to pay the power bill", &c).unwrap();
        let at = local(2026, 3, 8, 9, 0);
        assert_eq!(d.trigger, Trigger::At { at });
        // Counting a day in seconds from today's offset would land an hour late.
        assert_eq!(at, now + 15 * 3600 - 3600);
        let l = zone_to_local(at).unwrap();
        assert_eq!((l.day, l.hour, l.minute), (8, 9, 0));
        // "at 8am" when 8 is gone today: tomorrow's 8, in summer time.
        let d = parse_reminder("remind me at 8am to run", &c).unwrap();
        assert_eq!(
            d.trigger,
            Trigger::At {
                at: local(2026, 3, 8, 8, 0)
            }
        );
    }

    #[test]
    fn a_reminder_fires_at_its_moment_and_lasts_until_then() {
        let mut rules = Rules::default();
        let now = local(2026, 1, 15, 10, 0);
        let d = parse_reminder("remind me in 3 days to renew the domain", &zone(now)).unwrap();
        let id = rules.add(d, None, now);
        let r = rules.items.iter_mut().find(|r| r.id == id).unwrap();
        assert!(r.expires > now + 3 * 86_400, "it outlives the usual day");
        assert!(matches!(
            check(r, &Facts::default(), now + 1000),
            Outcome::Wait(_)
        ));
        assert_eq!(check(r, &Facts::default(), now + 3 * 86_400), Outcome::Fire);
    }

    #[test]
    fn a_missed_reminder_still_rings_once_and_says_so() {
        let mut rules = Rules::default();
        let now = local(2026, 1, 15, 14, 0);
        let d = parse_reminder("remind me in 5 min to call Dani", &zone(now)).unwrap();
        let id = rules.add(d, None, now);
        let at = now + 300;
        let r = rules.items.iter_mut().find(|r| r.id == id).unwrap();
        // The app was closed at its moment, and opens again ten days later.
        let late = at + 10 * 86_400;
        assert!(late > r.expires);
        assert_eq!(check(r, &Facts::default(), late), Outcome::Fire);
        assert_eq!(
            reminder_title_with(at, &zone(at + 2 * 3600)),
            "Reminder (missed at 14:05)"
        );
        assert_eq!(
            reminder_title_with(at, &zone(at + 10 * 86_400)),
            "Reminder (missed at 15/01 14:05)"
        );
        assert_eq!(reminder_title_with(at, &zone(at + 15)), "Reminder");
        // Once it rang it is done, and tidied away a week after.
        r.state = State::Done;
        rules.tidy(late + 7 * 86_400);
        assert!(rules.items.is_empty());
    }

    #[test]
    fn clock_says_the_day_when_it_is_not_today() {
        let now = local(2026, 1, 15, 10, 0);
        let c = zone(now);
        assert_eq!(clock_with(local(2026, 1, 15, 17, 5), &c), "17:05");
        assert_eq!(clock_with(local(2026, 1, 16, 9, 0), &c), "tomorrow 09:00");
        assert_eq!(clock_with(local(2026, 1, 14, 9, 0), &c), "yesterday 09:00");
        assert_eq!(clock_with(local(2026, 1, 21, 9, 0), &c), "21/01 09:00");
    }

    const REPO: &str = "ArakunHQ/arakun";

    fn pr(checks: &str) -> PullFacts {
        PullFacts {
            repo: REPO.into(),
            number: 64,
            checks: checks.into(),
            open: true,
            mergeable: "MERGEABLE".into(),
            review: "APPROVED".into(),
            ..Default::default()
        }
    }

    fn rule(trigger: Trigger, steps: Vec<Step>) -> Rule {
        let mut r = Rules::default();
        r.add(
            Draft {
                title: "t".into(),
                trigger,
                steps,
                hours: 24,
            },
            None,
            1000,
        );
        r.items.remove(0)
    }

    #[test]
    fn the_mock_sentence_reads_as_merge_when_green() {
        let d = parse(
            "merge #64 when the checks pass and let me know",
            Some(REPO),
            &[],
        )
        .unwrap();
        assert_eq!(
            d.trigger,
            Trigger::PrChecksPass {
                repo: REPO.into(),
                number: 64
            }
        );
        assert_eq!(d.steps.len(), 2);
        assert!(matches!(d.steps[0], Step::Merge { ref method } if method == "squash"));
        assert!(matches!(d.steps[1], Step::Notify { .. }));
        assert_eq!(d.hours, 24);
        assert_eq!(d.title, "Merge #64 when green");
    }

    #[test]
    fn english_and_other_triggers() {
        let d = parse("merge #12 when checks pass, notify me", Some(REPO), &[]).unwrap();
        assert!(matches!(d.steps[0], Step::Merge { .. }));
        let d = parse("tell me when #12 is merged", Some(REPO), &[]).unwrap();
        assert!(matches!(d.trigger, Trigger::PrMerged { number: 12, .. }));
        assert_eq!(d.steps.len(), 1);
        let d = parse("tell me when CI fails on phone-local", Some(REPO), &[]).unwrap();
        assert_eq!(
            d.trigger,
            Trigger::CiFailed {
                branch: "phone-local".into(),
                project: None,
            }
        );
        let sessions = vec![("s1".to_string(), "Sidebar redesign".to_string())];
        let d = parse(
            "when Sidebar redesign finishes run `cargo test` for 2 hours",
            None,
            &sessions,
        )
        .unwrap();
        assert_eq!(
            d.trigger,
            Trigger::SessionFinished {
                id: "s1".into(),
                title: "Sidebar redesign".into()
            }
        );
        assert_eq!(
            d.steps,
            vec![Step::Run {
                command: "cargo test".into()
            }]
        );
        assert_eq!(d.hours, 2);
    }

    #[test]
    fn rule_like_sentences() {
        assert!(sounds_like_rule("ping me when the relay deploy is done"));
        assert!(!sounds_like_rule("when is it done?"));
        assert!(!sounds_like_rule("sidebar"));
    }

    #[test]
    fn plain_searches_are_not_rules() {
        assert!(parse("sidebar", Some(REPO), &[]).is_none());
        assert!(parse("merge #64", Some(REPO), &[]).is_none());
        // A PR trigger needs a repository to point at.
        assert!(parse("merge #64 when checks pass", None, &[]).is_none());
    }

    #[test]
    fn checks_pass_fires_only_when_green_and_mergeable() {
        let mut r = rule(
            Trigger::PrChecksPass {
                repo: REPO.into(),
                number: 64,
            },
            vec![Step::Merge {
                method: "squash".into(),
            }],
        );
        let mut facts = Facts::default();
        assert!(matches!(check(&mut r, &facts, 1001), Outcome::Wait(_)));
        facts.pulls.push(pr("running"));
        assert_eq!(
            check(&mut r, &facts, 1001),
            Outcome::Wait("Waiting for checks".into())
        );
        facts.pulls[0].checks = "passed".into();
        facts.pulls[0].review = "REVIEW_REQUIRED".into();
        facts.pulls[0].reviewers = vec!["rikvisser-dev".into()];
        assert_eq!(
            check(&mut r, &facts, 1001),
            Outcome::Wait("Needs rikvisser-dev's approval".into())
        );
        assert_eq!(r.note, "Needs rikvisser-dev's approval");
        facts.pulls[0].review = "APPROVED".into();
        assert_eq!(check(&mut r, &facts, 1001), Outcome::Fire);
    }

    #[test]
    fn a_notify_only_rule_does_not_wait_for_approval() {
        let mut r = rule(
            Trigger::PrChecksPass {
                repo: REPO.into(),
                number: 64,
            },
            vec![Step::Notify { text: "x".into() }],
        );
        let mut facts = Facts::default();
        let mut p = pr("passed");
        p.review = "REVIEW_REQUIRED".into();
        facts.pulls.push(p);
        assert_eq!(check(&mut r, &facts, 1001), Outcome::Fire);
    }

    #[test]
    fn rules_expire() {
        let mut r = rule(
            Trigger::PrMerged {
                repo: REPO.into(),
                number: 1,
            },
            vec![],
        );
        assert_eq!(r.expires, 1000 + 24 * 3600);
        let at = r.expires;
        assert_eq!(check(&mut r, &Facts::default(), at), Outcome::Expired);
    }

    fn failed(project: &str, branch: &str, id: u64, at: u64) -> FailedRun {
        FailedRun {
            project: project.into(),
            branch: branch.into(),
            id,
            at,
        }
    }

    #[test]
    fn ci_failures_count_only_after_the_rule() {
        let mut r = rule(
            Trigger::CiFailed {
                branch: "phone-local".into(),
                project: Some("/work/app".into()),
            },
            vec![],
        );
        let mut facts = Facts::default();
        facts
            .failed_runs
            .push(failed("/work/app", "phone-local", 1, 900));
        assert!(matches!(check(&mut r, &facts, 1001), Outcome::Wait(_)));
        facts
            .failed_runs
            .push(failed("/work/app", "phone-local", 2, 1100));
        assert_eq!(check(&mut r, &facts, 1101), Outcome::Fire);
    }

    #[test]
    fn ci_failures_of_another_project_dont_fire() {
        let mut r = rule(
            Trigger::CiFailed {
                branch: "main".into(),
                project: Some("/work/app".into()),
            },
            vec![Step::Run {
                command: "make deploy".into(),
            }],
        );
        let mut facts = Facts::default();
        facts
            .failed_runs
            .push(failed("/work/other", "main", 1, 1100));
        assert!(matches!(check(&mut r, &facts, 1101), Outcome::Wait(_)));
        // One saved without its project never fires, whatever fails.
        let mut old = r.clone();
        old.trigger = Trigger::CiFailed {
            branch: "main".into(),
            project: None,
        };
        facts.failed_runs.push(failed("/work/app", "main", 2, 1100));
        assert!(matches!(check(&mut old, &facts, 1101), Outcome::Wait(_)));
        assert_eq!(check(&mut r, &facts, 1101), Outcome::Fire);
        // A rule saved before the project was kept still loads.
        let json = r#"{"type":"ci_failed","branch":"main"}"#;
        assert_eq!(
            serde_json::from_str::<Trigger>(json).unwrap(),
            Trigger::CiFailed {
                branch: "main".into(),
                project: None
            }
        );
    }

    #[test]
    fn a_rule_is_done_only_once_its_steps_ran() {
        let mut rules = Rules::default();
        let draft = |n| Draft {
            title: "t".into(),
            trigger: Trigger::PrChecksPass {
                repo: REPO.into(),
                number: n,
            },
            steps: vec![Step::Merge {
                method: "squash".into(),
            }],
            hours: 1,
        };
        let ok = rules.add(draft(1), None, 5);
        let bad = rules.add(draft(2), None, 5);
        let cut = rules.add(draft(3), None, 5);
        // Only a running rule is finished.
        assert!(!rules.finish(ok, Ok(())));
        for r in &mut rules.items {
            r.state = State::Running;
        }
        assert!(rules.finish(ok, Ok(())));
        assert!(rules.finish(bad, Err("Could not merge #2: conflict".into())));
        assert_eq!(rules.items[0].state, State::Done);
        assert_eq!(
            rules.items[1].state,
            State::Failed("Could not merge #2: conflict".into())
        );
        assert!(!rules.finish(ok, Err("late".into())));
        // Still running when the app closed: failed, not run again.
        rules.interrupted();
        assert!(matches!(rules.items[2].state, State::Failed(_)));
        assert_eq!(rules.items[2].id, cut);
        assert_eq!(rules.waiting().count(), 0);
    }

    #[test]
    fn a_rule_runs_only_once_running_is_saved() {
        let mut rules = Rules::default();
        let draft = Draft {
            title: "Reminder".into(),
            trigger: Trigger::At { at: 10 },
            steps: vec![Step::Notify { text: "hi".into() }],
            hours: 0,
        };
        let id = rules.add(draft, None, 5);
        let full = |_: &[u8]| Err(std::io::Error::other("disk full"));
        rules.items[0].state = State::Running;
        assert!(rules.save_fired_with(&[id], full).is_err());
        // Not on disk as Running: back to Waiting, not run, saved again later.
        assert_eq!(rules.items[0].state, State::Waiting);
        assert!(rules.unsaved);
        assert_eq!(
            check(&mut rules.items[0], &Facts::default(), 20),
            Outcome::Fire
        );
        rules.items[0].state = State::Running;
        let mut written = Vec::new();
        assert!(
            rules
                .save_fired_with(&[id], |b| {
                    written = b.to_vec();
                    Ok(())
                })
                .is_ok()
        );
        assert_eq!(rules.items[0].state, State::Running);
        assert!(!rules.unsaved);
        let on_disk: Rules = serde_json::from_slice(&written).unwrap();
        assert_eq!(on_disk.items[0].state, State::Running);
    }

    #[test]
    fn an_unsaved_cancel_still_stops_the_rule_here() {
        let mut rules = Rules::default();
        let draft = Draft {
            title: "Reminder".into(),
            trigger: Trigger::At { at: 10 },
            steps: vec![Step::Notify { text: "hi".into() }],
            hours: 0,
        };
        let id = rules.add(draft, None, 5);
        assert!(rules.cancel(id));
        let full = |_: &[u8]| Err(std::io::Error::other("disk full"));
        assert!(rules.save_with(full).is_err());
        // The person asked to stop it: it never fires in this run, and the
        // next check saves again.
        assert!(rules.unsaved);
        assert_eq!(rules.items[0].state, State::Cancelled);
        assert!(matches!(
            check(&mut rules.items[0], &Facts::default(), 20),
            Outcome::Wait(_)
        ));
        assert_eq!(rules.waiting().count(), 0);
    }

    #[test]
    fn a_session_must_be_seen_working_first() {
        let mut r = rule(
            Trigger::SessionFinished {
                id: "s".into(),
                title: "S".into(),
            },
            vec![],
        );
        let mut facts = Facts {
            sessions: vec![("s".into(), false)],
            ..Default::default()
        };
        assert!(matches!(check(&mut r, &facts, 1001), Outcome::Wait(_)));
        facts.sessions[0].1 = true;
        assert!(matches!(check(&mut r, &facts, 1001), Outcome::Wait(_)));
        facts.sessions[0].1 = false;
        assert_eq!(check(&mut r, &facts, 1001), Outcome::Fire);
    }

    #[test]
    fn cancelled_rules_do_not_fire() {
        let mut rules = Rules::default();
        let id = rules.add(
            parse("tell me when #3 is merged", Some(REPO), &[]).unwrap(),
            None,
            0,
        );
        assert!(rules.cancel(id));
        assert!(!rules.cancel(id));
        assert_eq!(rules.waiting().count(), 0);
    }

    #[test]
    fn model_json_is_validated() {
        let ok = json!({
            "understood": true,
            "trigger": {"type": "pr_checks_pass", "number": 64, "branch": "", "session": ""},
            "steps": [{"type": "merge", "text": ""}, {"type": "notify", "text": ""}],
            "hours": 24
        });
        let d = from_json(&ok, Some(REPO), &[]).unwrap();
        assert_eq!(d.title, "Merge #64 when green");
        assert!(matches!(&d.steps[1], Step::Notify { text } if text == "#64: checks passed"));

        let mut bad = ok.clone();
        bad["trigger"]["type"] = json!("deploy");
        assert!(from_json(&bad, Some(REPO), &[]).is_err());

        let mut bad = ok.clone();
        bad["trigger"]["type"] = json!("pr_merged");
        assert!(
            from_json(&bad, Some(REPO), &[]).is_err(),
            "merge needs a checks rule"
        );

        let mut bad = ok.clone();
        bad["trigger"]["number"] = json!(0);
        assert!(from_json(&bad, Some(REPO), &[]).is_err());

        assert!(from_json(&ok, None, &[]).is_err());

        let mut bad = ok.clone();
        bad["steps"] = json!([{"type": "run", "text": ""}]);
        assert!(from_json(&bad, Some(REPO), &[]).is_err());

        let mut bad = ok.clone();
        bad["steps"] = json!([]);
        assert!(from_json(&bad, Some(REPO), &[]).is_err());

        assert!(from_json(&json!({"understood": false}), Some(REPO), &[]).is_err());

        let sess = json!({
            "understood": true,
            "trigger": {"type": "session_finished", "number": 0, "branch": "", "session": "sidebar redesign"},
            "steps": [{"type": "notify", "text": "done"}],
            "hours": 999
        });
        let sessions = vec![("s1".to_string(), "Sidebar redesign".to_string())];
        let d = from_json(&sess, None, &sessions).unwrap();
        assert_eq!(d.hours, 24 * 7);
        assert!(from_json(&sess, None, &[]).is_err());
    }

    #[test]
    fn rules_round_trip_through_json() {
        let mut rules = Rules::default();
        rules.add(
            parse(
                "merge #64 when the checks pass and let me know",
                Some(REPO),
                &[],
            )
            .unwrap(),
            Some("/tmp".into()),
            5,
        );
        let back: Rules = serde_json::from_slice(&serde_json::to_vec(&rules).unwrap()).unwrap();
        assert_eq!(back, rules);
    }

    #[test]
    fn a_rule_runs_only_in_its_own_folder() {
        let run = |dir: Option<&str>| {
            let mut r = rule(
                Trigger::CiFailed {
                    branch: "main".into(),
                    project: None,
                },
                vec![Step::Run {
                    command: "make deploy".into(),
                }],
            );
            r.dir = dir.map(PathBuf::from);
            r
        };
        let here = |p: &Path| p == Path::new("/work/app");
        assert_eq!(
            workdir(&run(Some("/work/app")), here),
            Ok(Some(PathBuf::from("/work/app")))
        );
        assert_eq!(
            workdir(&run(Some("/work/gone")), here),
            Err("folder is gone: /work/gone".into())
        );
        assert!(workdir(&run(None), here).is_err());
        let mut agent = run(Some("/work/gone"));
        agent.steps = vec![Step::Agent {
            prompt: "fix it".into(),
        }];
        assert!(workdir(&agent, here).is_err());
        // Merging and telling need no folder.
        let mut notify = run(Some("/work/gone"));
        notify.steps = vec![Step::Notify { text: "hi".into() }];
        assert_eq!(workdir(&notify, here), Ok(None));
    }

    #[test]
    fn a_reminder_with_no_time_is_kept_as_a_to_do() {
        assert_eq!(
            timeless_reminder("set a reminder to buy milk").as_deref(),
            Some("buy milk")
        );
        assert_eq!(
            timeless_reminder("remind me to call Dani").as_deref(),
            Some("call Dani")
        );
        assert_eq!(timeless_reminder("remind me in 5 min to call Dani"), None);
        assert_eq!(timeless_reminder("timer 10 min"), None);
        assert_eq!(timeless_reminder("open PR 12"), None);
        // Not whole reminder words: not to-dos either.
        assert_eq!(timeless_reminder("open src/alarms.rs"), None);
        assert_eq!(timeless_reminder("open src/timer.rs"), None);
        assert_eq!(timeless_reminder("fix the timeranges bug"), None);
    }

    #[test]
    fn a_plain_branch_name_is_read_after_the_ci() {
        let branch = |text: &str| match parse(text, None, &[]).unwrap().trigger {
            Trigger::CiFailed { branch, .. } => branch,
            t => panic!("{t:?}"),
        };
        assert_eq!(branch("tell me when CI fails on develop"), "develop");
        assert_eq!(branch("tell me when the CI fails on staging."), "staging");
        assert_eq!(
            branch("when the build breaks on the develop branch"),
            "develop"
        );
        assert_eq!(
            branch("tell me when the CI fails on the release branch"),
            "release"
        );
        // Not a prefix of the name: the whole of it.
        assert_eq!(
            branch("tell me when CI fails on release+hotfix"),
            "release+hotfix"
        );
        assert_eq!(branch("tell me when CI fails on (develop)."), "develop");
        // A word a branch can't be is none, not a piece of it.
        assert_eq!(branch("tell me when CI fails on feat/x*y"), "main");
        assert_eq!(
            branch("tell me when CI fails on feature/user@domain"),
            "feature/user@domain"
        );
        // "on Slack" comes before the CI: it isn't a branch.
        assert_eq!(branch("notify me on Slack when CI fails"), "main");
        assert_eq!(branch("tell me when CI fails on this project"), "main");
    }

    #[test]
    fn letters_that_grow_in_lower_case_dont_break_the_branch() {
        // "İ" is two bytes and its lower case three: offsets from the lower
        // case text once cut "é" in half here and panicked.
        let d = parse("İ when CI fails on é-main", None, &[]).unwrap();
        assert_eq!(
            d.trigger,
            Trigger::CiFailed {
                branch: "é-main".into(),
                project: None,
            }
        );
        assert_eq!(
            word_after("İİ CI ON feat/x", &["on "]).as_deref(),
            Some("feat/x")
        );
        assert_eq!(
            after_any("İ: Ask An Agent To fix it", &["ask an agent to "]).as_deref(),
            Some("fix it")
        );
        assert_eq!(after_any("İ", &["ask an agent to "]), None);
    }

    #[test]
    fn rules_from_a_newer_arakun_are_kept() {
        let dir = crate::stored::tests::scratch("rules");
        let path = dir.join("rules.json");
        let newer = r#"{"next_id":2,"items":[{"id":1,"trigger":{"kind":"from-the-future"}}]}"#;
        std::fs::write(&path, newer).unwrap();
        let mut rules = Rules::load_from(&path);
        assert!(rules.items.is_empty());
        rules
            .save_with(|bytes| crate::stored::write(&path, bytes))
            .unwrap();
        assert_eq!(crate::stored::tests::kept(&path).as_deref(), Some(newer));
        let _ = std::fs::remove_dir_all(dir);
    }
}
