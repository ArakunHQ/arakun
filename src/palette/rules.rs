//! Automations that wait for something to happen ("merge #64 when the checks
//! pass and tell me"), made from a sentence in the palette and confirmed
//! there. A rule waits for its trigger, runs its steps once, and expires.
//! The shell checks them where the data already arrives (GitHub's poll, CI
//! runs, sessions); this file is the pure part: the shapes, the checks, the
//! sentence parser and the JSON a model may answer with.

use std::path::PathBuf;

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
            Trigger::CiFailed { branch } => format!("CI fails on {branch}"),
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

/// What the app knows right now, gathered by the shell each check.
#[derive(Clone, Debug, Default)]
pub struct Facts {
    pub pulls: Vec<PullFacts>,
    /// Failed runs: (branch, run id, unix seconds it finished).
    pub failed_runs: Vec<(String, u64, u64)>,
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
    if now >= rule.expires {
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
        Trigger::CiFailed { branch } => {
            let failed = facts
                .failed_runs
                .iter()
                .any(|(b, _, at)| b == branch && *at >= rule.created);
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

/// Reads common Spanish and English phrasings. `repo` is the project's GitHub
/// repository, for `#64`; `sessions` are (id, title) to find "when X finishes".
pub fn parse(text: &str, repo: Option<&str>, sessions: &[(String, String)]) -> Option<Draft> {
    let now = crate::memory::now();
    let local = crate::platform::local_time(now).map_or((0, 0, 0), |l| {
        (l.hour as u32, l.minute as u32, (now % 60) as u32)
    });
    if let Some(d) = parse_reminder(text, now, local) {
        return Some(d);
    }
    let t = text.to_lowercase();
    let number = pr_number(&t);
    let when = has_any(
        &t,
        &["cuando", "when", "once ", "after ", "en cuanto", "si "],
    );
    if !when {
        return None;
    }
    let trigger = if let (Some(n), Some(repo)) = (number, repo)
        && has_any(
            &t,
            &[
                "checks pass",
                "checks are green",
                "pasen los checks",
                "pasen las pruebas",
                "pase el ci",
                "pasen",
                "esté verde",
                "este verde",
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
        && has_any(
            &t,
            &[
                "se mergee",
                "lo mergeen",
                "se fusione",
                "is merged",
                "gets merged",
                "merges",
                "se haga merge",
            ],
        )
    {
        Trigger::PrMerged {
            repo: repo.to_string(),
            number: n,
        }
    } else if has_any(&t, &["falle", "fails", "falla", "breaks", "se rompa"])
        && has_any(&t, &["ci", "build", "checks"])
    {
        let branch = word_after(text, &["de ", "on ", "en "]).unwrap_or_else(|| "main".into());
        Trigger::CiFailed { branch }
    } else if has_any(&t, &["termine", "acabe", "finishes", "is done", "finish"]) {
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
    let merge_words = [
        "mergea",
        "mergear",
        "merge it",
        "merge #",
        "fusiona",
        "haz merge",
        "squash",
    ];
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
    if let Some(prompt) = after_any(
        text,
        &[
            "pídele a un agente que ",
            "pide a un agente que ",
            "ask an agent to ",
            "have an agent ",
        ],
    ) {
        steps.push(Step::Agent { prompt });
    }
    let notify = has_any(
        &t,
        &[
            "avísame",
            "avisame",
            "notifícame",
            "notificame",
            "notify me",
            "tell me",
            "let me know",
            "ping me",
        ],
    );
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
    let t = text.to_lowercase();
    if REMIND_WORDS.iter().any(|w| t.contains(w)) {
        return true;
    }
    t.split_whitespace().count() >= 4
        && has_any(
            &t,
            &[
                "cuando ",
                "when ",
                "once ",
                "after ",
                "en cuanto",
                "as soon as",
            ],
        )
        && !text.trim_end().ends_with('?')
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
        Trigger::CiFailed { branch } => format!("CI failed on {branch}"),
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
        Trigger::CiFailed { branch } => format!("When CI fails on {branch}"),
        Trigger::SessionFinished { title, .. } => format!("When “{title}” finishes"),
        Trigger::At { .. } => match steps.first() {
            Some(Step::Notify { text }) => format!("Reminder: {text}"),
            _ => "Reminder".into(),
        },
    }
}

/// `17:05`, or `mañana 09:00` style when it is not today: the local time of `at`.
pub fn clock(at: u64) -> String {
    let now = crate::memory::now();
    let (Some(t), Some(n)) = (
        crate::platform::local_time(at),
        crate::platform::local_time(now),
    ) else {
        return String::new();
    };
    let hm = format!("{:02}:{:02}", t.hour, t.minute);
    if (t.year, t.month, t.day) == (n.year, n.month, n.day) {
        hm
    } else if at.saturating_sub(now) < 2 * 86_400 {
        format!("tomorrow {hm}")
    } else {
        format!("{:02}/{:02} {hm}", t.day, t.month + 1)
    }
}

/// Words that ask for a reminder or a timer.
const REMIND_WORDS: &[&str] = &[
    "recuérdame",
    "recuerdame",
    "recordatorio",
    "recuerda ",
    "remind me",
    "reminder",
    "temporizador",
    "timer",
    "alarma",
    "alarm",
    "avísame en",
    "avisame en",
    "avísame a las",
    "avisame a las",
];

/// A reminder or a timer in a sentence: "recuérdame en 20 min revisar el PR",
/// "recordatorio mañana a las 9 de llamar a Dani", "remind me at 5pm to stretch",
/// "timer 10 min". `now` and `local` (the local hour, minute and second of
/// `now`) place clock times on the local day.
pub fn parse_reminder(text: &str, now: u64, local: (u32, u32, u32)) -> Option<Draft> {
    let t = text.to_lowercase();
    if !REMIND_WORDS.iter().any(|w| t.contains(w)) {
        return None;
    }
    let (at, span) = relative_time(&t, now).or_else(|| clock_time(&t, now, local))?;
    let what = what_to_remember(&format!("{} {}", &text[..span.0], &text[span.1..]));
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

/// "créame un recordatorio de comprar leche": a reminder with no time in it.
/// What to remember, so it can be kept as a to-do instead of being lost.
pub fn timeless_reminder(text: &str) -> Option<String> {
    let t = text.to_lowercase();
    let timed = relative_time(&t, 0)
        .or_else(|| clock_time(&t, 0, (0, 0, 0)))
        .is_some();
    if timed || !REMIND_WORDS.iter().any(|w| t.contains(w)) {
        return None;
    }
    Some(what_to_remember(text)).filter(|w| !w.is_empty())
}

/// The sentence without its asking words: what the person wants to remember.
fn what_to_remember(text: &str) -> String {
    let mut rest = text.to_string();
    for w in REMIND_WORDS.iter().chain(&[
        "créame un",
        "creame un",
        "crea un",
        "pon un",
        "ponme un",
        "set a",
        "set an",
        "hazme un",
        "make a",
    ]) {
        if let Some(i) = rest.to_lowercase().find(w) {
            rest.replace_range(i..i + w.len(), " ");
        }
    }
    let mut what = rest.split_whitespace().collect::<Vec<_>>().join(" ");
    for lead in ["de ", "que ", "to ", "para ", "about ", "of ", "a ", "for "] {
        if what.to_lowercase().starts_with(lead) {
            what = what[lead.len()..].to_string();
        }
    }
    what.trim_matches(|c: char| c == ',' || c == '.' || c == ':' || c.is_whitespace())
        .to_string()
}

/// "en 20 min", "in 2 hours", "dentro de media hora": the moment and where it was in `t`.
fn relative_time(t: &str, now: u64) -> Option<(u64, (usize, usize))> {
    for head in ["dentro de ", "en ", "in "] {
        for (i, _) in t.match_indices(head) {
            let after = &t[i + head.len()..];
            let (n, used) = if let Some(r) = after
                .strip_prefix("media hora")
                .or_else(|| after.strip_prefix("half an hour"))
            {
                (30.0, after.len() - r.len())
            } else if let Some(r) = after
                .strip_prefix("una hora")
                .or_else(|| after.strip_prefix("an hour"))
            {
                (60.0, after.len() - r.len())
            } else {
                let num: String = after
                    .chars()
                    .take_while(|c| c.is_ascii_digit() || *c == '.')
                    .collect();
                let Ok(v) = num.parse::<f64>() else { continue };
                let unit_start =
                    num.len() + after[num.len()..].len() - after[num.len()..].trim_start().len();
                let unit: String = after[unit_start..]
                    .chars()
                    .take_while(|c| c.is_alphabetic())
                    .collect();
                let minutes = match unit.as_str() {
                    "s" | "seg" | "segs" | "segundo" | "segundos" | "sec" | "secs" | "second"
                    | "seconds" => v / 60.,
                    "m" | "min" | "mins" | "minuto" | "minutos" | "minute" | "minutes" => v,
                    "h" | "hr" | "hrs" | "hora" | "horas" | "hour" | "hours" => v * 60.,
                    "d" | "dia" | "día" | "dias" | "días" | "day" | "days" => v * 1440.,
                    _ => continue,
                };
                (minutes, unit_start + unit.len())
            };
            let secs = (n * 60.0).round() as u64;
            if secs == 0 {
                continue;
            }
            return Some((now + secs, (i, i + head.len() + used)));
        }
    }
    None
}

/// "a las 5pm", "at 17:30", "mañana a las 9", "tomorrow at 9am": the moment and its span.
fn clock_time(t: &str, now: u64, local: (u32, u32, u32)) -> Option<(u64, (usize, usize))> {
    for head in ["a las ", "a la ", "at "] {
        for (i, _) in t.match_indices(head) {
            let after = &t[i + head.len()..];
            let hour: String = after.chars().take_while(|c| c.is_ascii_digit()).collect();
            let Ok(mut h) = hour.parse::<u32>() else {
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
            for (word, is_pm) in [
                ("pm", true),
                ("p.m.", true),
                ("am", false),
                ("a.m.", false),
                ("de la tarde", true),
                ("de la noche", true),
                ("de la mañana", false),
                ("de la manana", false),
            ] {
                if tail.starts_with(word) {
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
            let (lh, lm, ls) = local;
            let midnight = now - u64::from(lh * 3600 + lm * 60 + ls);
            let mut at = midnight + u64::from(h * 3600 + m * 60);
            let tomorrow = t.contains("mañana") && !t.contains("de la mañana")
                || t.contains("manana") && !t.contains("de la manana")
                || t.contains("tomorrow");
            let mut start = i;
            if tomorrow {
                at += 86_400;
                for w in ["mañana ", "manana ", "tomorrow "] {
                    if let Some(j) = t[..i].rfind(w)
                        && i - (j + w.len()) <= 1
                    {
                        start = j;
                    }
                }
            } else if at <= now {
                // A time already gone today means the next one.
                at += 86_400;
            }
            return Some((at, (start, i + head.len() + used)));
        }
    }
    None
}

fn word_after(text: &str, heads: &[&str]) -> Option<String> {
    let lower = text.to_lowercase();
    for h in heads {
        for (i, _) in lower.match_indices(h) {
            let w: String = text[i + h.len()..]
                .chars()
                .take_while(|c| c.is_alphanumeric() || matches!(c, '-' | '/' | '_' | '.'))
                .collect();
            if w.contains('-') || w.contains('/') || w == "main" || w == "master" {
                return Some(w);
            }
        }
    }
    None
}

fn between(text: &str, quote: char) -> Option<String> {
    let start = text.find(quote)? + quote.len_utf8();
    let len = text[start..].find(quote)?;
    let inner = text[start..start + len].trim();
    (!inner.is_empty()).then(|| inner.to_string())
}

fn after_any(text: &str, heads: &[&str]) -> Option<String> {
    let lower = text.to_lowercase();
    heads.iter().find_map(|h| {
        let i = lower.find(h)?;
        let rest = text.get(i + h.len()..)?.trim();
        (!rest.is_empty()).then(|| rest.to_string())
    })
}

/// "por 2 horas", "for 3 hours", "48h".
fn hours_in(t: &str) -> Option<u64> {
    let words: Vec<&str> = t.split_whitespace().collect();
    for (i, w) in words.iter().enumerate() {
        if let Some(n) = w.strip_suffix('h').and_then(|n| n.parse().ok()) {
            return Some(n);
        }
        if let Ok(n) = w.parse::<u64>()
            && words
                .get(i + 1)
                .is_some_and(|u| u.starts_with("hora") || u.starts_with("hour"))
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
            // A reminder lasts until its moment and a little after, however far.
            expires: match expires_at {
                Some(at) => at.max(now) + 3600,
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

    pub fn waiting(&self) -> impl Iterator<Item = &Rule> {
        self.items.iter().filter(|r| r.state == State::Waiting)
    }

    /// Drops finished rules after a week so the file stays small.
    pub fn tidy(&mut self, now: u64) {
        self.items
            .retain(|r| r.state == State::Waiting || now.saturating_sub(r.expires) < 7 * 86_400);
    }

    pub fn load() -> Self {
        std::fs::read(file())
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default()
    }

    pub fn save(&self) {
        if let Ok(bytes) = serde_json::to_vec_pretty(self) {
            let _ = crate::durable::write(&file(), &bytes);
        }
    }
}

fn file() -> PathBuf {
    super::store_path("rules.json")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reminders_and_timers_are_read() {
        // 10:00:00 local, at unix 1_000_000.
        let (now, local) = (1_000_000, (10, 0, 0));
        let d = parse_reminder("recuérdame en 20 min revisar el PR de Rik", now, local).unwrap();
        assert_eq!(d.trigger, Trigger::At { at: now + 20 * 60 });
        assert_eq!(
            d.steps,
            vec![Step::Notify {
                text: "revisar el PR de Rik".into()
            }]
        );
        let d = parse_reminder(
            "créame un recordatorio de llamar a Dani a las 5pm",
            now,
            local,
        )
        .unwrap();
        assert_eq!(d.trigger, Trigger::At { at: now + 7 * 3600 });
        assert_eq!(
            d.steps,
            vec![Step::Notify {
                text: "llamar a Dani".into()
            }]
        );
        let d = parse_reminder("recordatorio mañana a las 9 de pagar la luz", now, local).unwrap();
        assert_eq!(
            d.trigger,
            Trigger::At {
                at: now - 3600 + 86_400
            }
        );
        assert_eq!(
            d.steps,
            vec![Step::Notify {
                text: "pagar la luz".into()
            }]
        );
        let d = parse_reminder("remind me at 17:30 to stretch", now, local).unwrap();
        assert_eq!(
            d.trigger,
            Trigger::At {
                at: now + 7 * 3600 + 1800
            }
        );
        assert_eq!(
            d.steps,
            vec![Step::Notify {
                text: "stretch".into()
            }]
        );
        // A time already gone today rings tomorrow.
        let d = parse_reminder("recuérdame a las 8 de la mañana correr", now, local).unwrap();
        assert_eq!(
            d.trigger,
            Trigger::At {
                at: now - 2 * 3600 + 86_400
            }
        );
        let d = parse_reminder("timer 10 min", now, local);
        assert!(d.is_none(), "a timer needs “en/in” before its length");
        let d = parse_reminder("pon un timer en 10 minutos", now, local).unwrap();
        assert_eq!(d.trigger, Trigger::At { at: now + 600 });
        assert_eq!(
            d.steps,
            vec![Step::Notify {
                text: "Time's up".into()
            }]
        );
        let d = parse_reminder("recuérdame en media hora sacar la ropa", now, local).unwrap();
        assert_eq!(d.trigger, Trigger::At { at: now + 1800 });
        assert!(parse_reminder("en 20 min revisa el PR", now, local).is_none());
        assert!(sounds_like_rule("créame un recordatorio de algo"));
        assert!(title_for(&d.trigger, &d.steps).starts_with("Reminder: "));
    }

    #[test]
    fn a_reminder_fires_at_its_moment_and_lasts_until_then() {
        let mut rules = Rules::default();
        let d =
            parse_reminder("recuérdame en 3 días renovar el dominio", 1_000, (0, 0, 0)).unwrap();
        let id = rules.add(d, None, 1_000);
        let r = rules.items.iter_mut().find(|r| r.id == id).unwrap();
        assert!(r.expires > 1_000 + 3 * 86_400, "it outlives the usual day");
        assert!(matches!(
            check(r, &Facts::default(), 2_000),
            Outcome::Wait(_)
        ));
        assert_eq!(
            check(r, &Facts::default(), 1_000 + 3 * 86_400),
            Outcome::Fire
        );
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
            "mergea #64 cuando pasen los checks y avísame",
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
        let d = parse("avísame cuando falle el CI de phone-local", Some(REPO), &[]).unwrap();
        assert_eq!(
            d.trigger,
            Trigger::CiFailed {
                branch: "phone-local".into()
            }
        );
        let sessions = vec![("s1".to_string(), "Sidebar redesign".to_string())];
        let d = parse(
            "cuando termine Sidebar redesign corre `cargo test` por 2 horas",
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
        assert!(parse("mergea #64", Some(REPO), &[]).is_none());
        // A PR trigger needs a repository to point at.
        assert!(parse("mergea #64 cuando pasen los checks", None, &[]).is_none());
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

    #[test]
    fn ci_failures_count_only_after_the_rule() {
        let mut r = rule(
            Trigger::CiFailed {
                branch: "phone-local".into(),
            },
            vec![],
        );
        let mut facts = Facts::default();
        facts.failed_runs.push(("phone-local".into(), 1, 900));
        assert!(matches!(check(&mut r, &facts, 1001), Outcome::Wait(_)));
        facts.failed_runs.push(("phone-local".into(), 2, 1100));
        assert_eq!(check(&mut r, &facts, 1101), Outcome::Fire);
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
            parse("avísame cuando #3 se mergee", Some(REPO), &[]).unwrap(),
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
                "mergea #64 cuando pasen los checks y avísame",
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
    fn a_reminder_with_no_time_is_kept_as_a_to_do() {
        assert_eq!(
            timeless_reminder("créame un recordatorio de comprar leche").as_deref(),
            Some("comprar leche")
        );
        assert_eq!(
            timeless_reminder("recuérdame llamar a Dani").as_deref(),
            Some("llamar a Dani")
        );
        assert_eq!(timeless_reminder("recuérdame en 5 min llamar a Dani"), None);
        assert_eq!(timeless_reminder("abre el PR 12"), None);
    }
}
