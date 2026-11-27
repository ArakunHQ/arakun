//! Telling a Linear issue what its delegated session does: a question, a
//! permission it waits for, the step it works on, the result, a failure,
//! that it still works, and that Arakun was closed while it ran.
//!
//! Only the last reply, a question's text, a short name of what a permission
//! is for and of the current step (with the file it works on) go to Linear:
//! never a tool's arguments, which can hold file contents, commands with
//! secrets or whole diffs.

use std::path::Path;

use super::runs::{ReportKind, Run, RunState, cut};
use super::start::failed_line;
use crate::agent::{AskUserRequest, Entry, Question, Thread, ThreadStatus, ToolStatus};
use crate::phone::notify;

/// The most characters of a question sent to Linear.
pub const QUESTION_LIMIT: usize = 2_000;
/// The most characters of what a permission is for.
pub const WHAT_LIMIT: usize = 80;

pub const STILL_WORKING: &str = "Still working.";
/// A finished turn that said nothing.
pub const DONE: &str = "Done.";

/// What a delegated session did, as Linear hears it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Seen {
    /// The question's text. `options` are its choices when Linear can show
    /// them to pick from; otherwise they are in the text, or there are none.
    Question {
        text: String,
        options: Vec<String>,
    },
    /// What the permission is for, or empty when only a description with its
    /// arguments is known.
    Permission(String),
    Finished {
        reply: String,
    },
    Failed(String),
    /// It waits on a person for something other than a question or a
    /// permission, such as a secret. What it is stays in Arakun.
    Waiting {
        computer: String,
    },
    Cancelled,
}

/// The once-per-ask key of a wait that is neither a question nor a permission.
pub const WAITING_KEY: &str = "waiting";

/// The report for what the session did, in the plan's words. A cancel says
/// nothing: a stop from Linear was told already, and one in Arakun is the
/// person's own.
pub fn to_report(ev: Seen) -> Option<(ReportKind, String)> {
    Some(match ev {
        Seen::Question { text, .. } => {
            let text = crate::secrets::scrub(text.trim());
            let text = match text.char_indices().nth(QUESTION_LIMIT) {
                Some((end, _)) => format!("{}…", &text[..end]),
                None if text.is_empty() => "Needs your input.".into(),
                None => text.into_owned(),
            };
            (ReportKind::Elicitation, text)
        }
        Seen::Permission(what) => {
            let what: String = what
                .lines()
                .next()
                .unwrap_or_default()
                .trim()
                .chars()
                .filter(|c| !c.is_control())
                .take(WHAT_LIMIT)
                .collect();
            let line = if what.is_empty() {
                "Waiting for your permission. Answer it in Arakun or on your phone.".into()
            } else {
                format!(
                    "Waiting for your permission: {what}. Answer it in Arakun or on your phone."
                )
            };
            (ReportKind::Elicitation, line)
        }
        Seen::Finished { reply } if reply.trim().is_empty() => {
            (ReportKind::Response, DONE.to_string())
        }
        Seen::Finished { reply } => (
            ReportKind::Response,
            cut(&crate::secrets::scrub(reply.trim())),
        ),
        Seen::Failed(reason) => {
            let reason = crate::secrets::scrub(&reason);
            let reason = match reason.lines().next().map(str::trim) {
                Some(line) if !line.is_empty() => line.to_string(),
                _ => "Stopped with an error.".into(),
            };
            (ReportKind::Error, failed_line(&reason))
        }
        Seen::Waiting { computer } => (
            ReportKind::Elicitation,
            format!("Waiting for you in Arakun on {computer}."),
        ),
        Seen::Cancelled => return None,
    })
}

pub fn removed_line(computer: &str) -> String {
    format!(
        "The session for this was removed in Arakun on {computer}. Delegate the issue again \
         to start over."
    )
}

pub fn closed_line(computer: &str) -> String {
    format!("Arakun was closed on {computer} while this ran. Reply here to continue.")
}

/// The most characters of a file's path in a progress line.
pub const PATH_LIMIT: usize = 120;

/// The step the session works on now, as Linear shows it while the session
/// runs: the latest tool call of the turn that has not ended, named by its
/// kind. A step on a file names the file, relative to `cwd`; any other step
/// says only what kind it is, never its command, query or address. None
/// when no step runs, or its kind says nothing.
pub fn progress_line(entries: &[Entry], cwd: &Path) -> Option<String> {
    let turn = entries
        .iter()
        .rposition(|e| matches!(e, Entry::User { .. }))
        .map_or(entries, |at| &entries[at + 1..]);
    let tc = turn.iter().rev().find_map(|e| match e {
        Entry::Tool(tc) => Some(tc),
        _ => None,
    })?;
    if !matches!(tc.status, ToolStatus::Pending | ToolStatus::InProgress) {
        return None;
    }
    let on_file = |verb: &str| {
        let path = tc.locations.first().map(|p| shown_path(p, cwd))?;
        (!path.is_empty()).then(|| format!("{verb} `{path}`"))
    };
    match tc.kind.as_str() {
        "read" => on_file("Reading"),
        "edit" => on_file("Editing"),
        "delete" => on_file("Deleting"),
        "move" => on_file("Moving"),
        "search" => Some("Searching the code".into()),
        "execute" => Some("Running a command".into()),
        "fetch" => Some("Reading a web page".into()),
        "think" => Some("Thinking it through".into()),
        _ => None,
    }
}

/// A path as a progress line shows it: inside `cwd` relative to it, else
/// only its file name, so a home folder or user name stays here. Only its
/// end when long, and never anything that breaks out of the code span.
fn shown_path(path: &Path, cwd: &Path) -> String {
    let shown = match path.strip_prefix(cwd) {
        Ok(rel) => rel.to_string_lossy(),
        Err(_) => path.file_name().unwrap_or_default().to_string_lossy(),
    };
    let shown = crate::secrets::scrub(&shown);
    let chars: Vec<char> = shown
        .chars()
        .filter(|c| !c.is_control() && *c != '`')
        .collect();
    match chars.len().checked_sub(PATH_LIMIT) {
        Some(cut) if cut > 0 => {
            let tail: String = chars[cut + 1..].iter().collect();
            format!("…{tail}")
        }
        _ => chars.into_iter().collect(),
    }
}

/// What a permission is for, from its tool's kind alone, as the session
/// shows the action. The request's title is not used: it often holds the
/// command or path itself. Empty when the kind says nothing.
pub fn permission_what(kind: Option<&str>, plan: bool) -> String {
    if plan {
        return "carry out its plan".into();
    }
    match kind.unwrap_or_default() {
        "execute" => "run a command",
        "edit" => "edit a file",
        "read" => "read a file",
        "delete" => "delete a file",
        "move" => "move a file",
        "search" => "search the files",
        "fetch" => "fetch a web page",
        "switch_mode" => "switch its mode",
        _ => "",
    }
    .into()
}

/// The most choices Linear is given to pick from, and the most characters
/// of one. The server refuses more.
pub const OPTIONS_LIMIT: usize = 10;
pub const OPTION_LIMIT: usize = 80;

/// The choices of a pending question, when Linear can show them to pick
/// from: one question, one answer, at least two short choices. Empty for
/// anything else, which goes as text. A pick comes back as a reply with the
/// choice as its text, as if the person had typed it.
pub fn choices(q: &AskUserRequest) -> Vec<String> {
    let [one] = q.questions.as_slice() else {
        return Vec::new();
    };
    if one.multi || one.secret {
        return Vec::new();
    }
    let options: Vec<String> = one
        .options
        .iter()
        .map(|o| crate::secrets::scrub(o.trim()).into_owned())
        .filter(|o| !o.is_empty())
        .collect();
    let fits = (2..=OPTIONS_LIMIT).contains(&options.len())
        && options.iter().all(|o| o.chars().count() <= OPTION_LIMIT);
    if fits { options } else { Vec::new() }
}

/// The choices a report carries for Linear to show.
pub fn options_of(seen: &Seen) -> Vec<String> {
    match seen {
        Seen::Question { options, .. } => options.clone(),
        _ => Vec::new(),
    }
}

/// A question as a notification shows it, then its choices one per line,
/// so the person can answer in Linear by typing one.
pub fn question_text(head: &str, options: &[String]) -> String {
    let choices: Vec<String> = options
        .iter()
        .map(|o| o.trim())
        .filter(|o| !o.is_empty())
        .map(|o| format!("- {o}"))
        .collect();
    if choices.is_empty() {
        head.to_string()
    } else {
        format!("{head}\n\n{}", choices.join("\n"))
    }
}

/// A question's own words, or its topic when it has none.
fn question_head(x: &Question) -> String {
    let text = notify::plain(&x.question).trim().to_string();
    if text.is_empty() {
        notify::plain(&x.topic).trim().to_string()
    } else {
        text
    }
}

/// Every question of a questionnaire with its choices, so the person can
/// answer all of them in one reply in Linear. One question reads as
/// [`question_text`]; several are numbered, each with its own choices.
pub fn questionnaire_text(q: &AskUserRequest) -> String {
    let head = question_head;
    match q.questions.as_slice() {
        [one] => question_text(&head(one), &one.options),
        many => many
            .iter()
            .enumerate()
            .map(|(i, x)| {
                let mut text = format!("{}. {}", i + 1, head(x));
                for o in x.options.iter().map(|o| o.trim()).filter(|o| !o.is_empty()) {
                    // Indented to sit under its number in Linear's Markdown.
                    text.push_str(&format!("\n   - {o}"));
                }
                text
            })
            .collect::<Vec<_>>()
            .join("\n\n"),
    }
}

/// A thread event, as far as Linear cares.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Event<'a> {
    /// The status changed, or a permission or question came in.
    Status,
    Finished(&'a str),
}

/// The session as it is when the event comes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Now {
    pub working: bool,
    pub failed: bool,
    /// A permission or question waits on a person.
    pub asking: bool,
    /// Queued messages start the next turn at once.
    pub continues: bool,
    /// The turn left work running that the agent comes back to by itself:
    /// subagents, or a command in the background.
    pub background: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Step {
    Ask,
    Working,
    Finished,
    /// The turn ended, but the work it left running is not done: the
    /// session stays open in Linear until the agent's own turn after it.
    Background,
    Failed,
    Cancelled,
    Nothing,
}

pub fn step(ev: Event, now: Now) -> Step {
    match ev {
        Event::Finished("cancelled") => Step::Cancelled,
        Event::Finished("error") => Step::Failed,
        Event::Finished(_) if now.asking => Step::Ask,
        Event::Finished(_) if now.continues => Step::Nothing,
        Event::Finished(_) if now.background => Step::Background,
        Event::Finished(_) => Step::Finished,
        Event::Status if now.failed => Step::Failed,
        Event::Status if now.asking => Step::Ask,
        Event::Status if now.working => Step::Working,
        Event::Status => Step::Nothing,
    }
}

/// What a step does to a run.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Move {
    pub to: RunState,
    pub report: bool,
    /// Its slot goes to the next run waiting.
    pub frees: bool,
}

/// Only a run at work hears how its session goes. A run stopped from
/// Linear is `Done` before the cancel, so the turn's end changes nothing.
pub fn apply(state: RunState, step: Step) -> Option<Move> {
    use RunState::*;
    let at_work = matches!(state, Running | Asked);
    let to = |to, report, frees| Some(Move { to, report, frees });
    match step {
        Step::Ask if at_work => to(Asked, true, false),
        Step::Finished | Step::Failed if at_work => to(Done, true, true),
        Step::Background if at_work => to(Running, false, false),
        Step::Cancelled if at_work => to(Done, false, true),
        Step::Working if matches!(state, Asked | Done | Interrupted) => to(Running, false, false),
        _ => None,
    }
}

/// What a step did to a run.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Effect {
    pub step: Step,
    pub report: bool,
    pub frees: bool,
}

/// Moves `run` for one session event. `key` names the pending ask, if any.
/// Gives what happened when the run changed or Linear must hear it.
pub fn advance(run: &mut Run, ev: Event, now: Now, key: Option<&str>) -> Option<Effect> {
    let step = step(ev, now);
    if run.stopped {
        // A stop from Linear is winding the turn down: only its end counts,
        // and it says nothing.
        if !matches!(ev, Event::Finished(_)) {
            return None;
        }
        run.stopped = false;
        return Some(Effect {
            step: Step::Nothing,
            report: false,
            frees: false,
        });
    }
    let mv = apply(run.state, step)?;
    let before = run.clone();
    if step != Step::Background {
        // It moved on: what it said before the background work ended is old.
        run.held = None;
    }
    let mut report = mv.report;
    if step == Step::Ask {
        let key = key?;
        // Events repeat while one ask is pending: it is told once.
        report &= run.asked.as_deref() != Some(key);
        run.asked = Some(key.to_string());
    }
    run.state = mv.to;
    if mv.to == RunState::Running {
        // Back at work: the next ask is a new one, even with the same words.
        run.asked = None;
    }
    // A held turn's reply is kept each time, even when the run stays as it was.
    (report || step == Step::Background || *run != before).then_some(Effect {
        step,
        report,
        frees: mv.frees,
    })
}

/// The last thing the agent said in its latest turn, whole.
pub fn turn_reply(entries: &[Entry]) -> String {
    let turn = entries
        .iter()
        .rposition(|e| matches!(e, Entry::User { .. }))
        .map_or(entries, |at| &entries[at + 1..]);
    turn.iter()
        .rev()
        .find_map(|e| match e {
            Entry::Agent { text } if !text.trim().is_empty() => Some(text.trim().to_string()),
            _ => None,
        })
        .unwrap_or_default()
}

/// The pull request to link: the session's own, else the one the CI view
/// knows for the branch. Only a web address.
pub fn pr_link(session: Option<&str>, ci: Option<&str>) -> Option<String> {
    [session, ci]
        .into_iter()
        .flatten()
        .find(|u| u.starts_with("https://"))
        .map(str::to_string)
}

/// Names one ask without keeping its text.
pub fn ask_key(parts: &[&str]) -> String {
    // FNV-1a: the same across versions, so a saved key still matches.
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for part in parts {
        for b in part.bytes().chain([0xff]) {
            h ^= u64::from(b);
            h = h.wrapping_mul(0x0100_0000_01b3);
        }
    }
    format!("{h:016x}")
}

/// A pending permission as Linear hears it, with its key. The request's
/// title only tells one ask from the next, hashed into the key: it and the
/// tool call's arguments often hold the command or the file, and stay here.
pub fn permission_ask(
    tool_call_id: &str,
    title: &str,
    plan: bool,
    entries: &[Entry],
) -> (String, Seen) {
    let kind = entries.iter().find_map(|e| match e {
        Entry::Tool(tc) if tc.id == tool_call_id => Some(tc.kind.as_str()),
        _ => None,
    });
    let what = permission_what(kind, plan);
    let key = ask_key(&["permission", tool_call_id, title]);
    (key, Seen::Permission(what))
}

/// What waits on a person in `t`, if anything: a key for it and what Linear
/// hears. A permission comes first, as on the desktop.
pub fn ask_of(t: &Thread, computer: &str) -> Option<(String, Seen)> {
    if let Some(p) = t.pending_permission() {
        return Some(permission_ask(
            &p.tool_call_id,
            &p.title,
            p.plan.is_some(),
            t.entries(),
        ));
    }
    let Some(q) = t.pending_question() else {
        // A secret, or anything else it waits for: never named in Linear.
        let waiting = Seen::Waiting {
            computer: computer.to_string(),
        };
        return (t.status() == ThreadStatus::NeedsInput)
            .then(|| (WAITING_KEY.to_string(), waiting));
    };
    if q.has_secret() {
        // A secret's field is answered in Arakun only, and a reply from
        // Linear never answers the questionnaire it is in: Linear only
        // hears that it waits.
        let waiting = Seen::Waiting {
            computer: computer.to_string(),
        };
        let key = ask_key(&["secret question", &q.tool_call_id]);
        return Some((key, waiting));
    }
    let text = questionnaire_text(q);
    let key = ask_key(&["question", &q.tool_call_id, &text]);
    let options = choices(q);
    // Choices Linear shows itself are not listed in the text as well.
    let text = match q.questions.as_slice() {
        [one] if !options.is_empty() => question_text(&question_head(one), &[]),
        _ => text,
    };
    Some((key, Seen::Question { text, options }))
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    fn run() -> Run {
        Run {
            agent_session: "as-1".into(),
            organization: "w1".into(),
            issue: "i1".into(),
            identifier: "ENG-412".into(),
            project: PathBuf::from("/p/a"),
            environment: None,
            branch: "rik/eng-412".into(),
            session: Some("s1".into()),
            state: RunState::Running,
            reported_at: 0,
            asked: None,
            stopped: false,
            held: None,
            progress: None,
            linked: None,
            kept: Vec::new(),
            guidance: Vec::new(),
        }
    }

    fn report(seen: Seen) -> Option<(ReportKind, String)> {
        to_report(seen)
    }

    #[test]
    fn a_question_is_an_elicitation_with_its_choices() {
        let text = question_text("Which database?", &["Postgres".into(), "SQLite".into()]);
        assert_eq!(text, "Which database?\n\n- Postgres\n- SQLite");
        assert_eq!(question_text("Go on?", &[]), "Go on?");
        assert_eq!(
            report(Seen::Question {
                text: text.clone(),
                options: Vec::new()
            }),
            Some((ReportKind::Elicitation, text))
        );
    }

    fn ask(question: &str, options: &[&str]) -> Question {
        Question {
            index: 1,
            topic: String::new(),
            question: question.into(),
            options: options.iter().map(|o| o.to_string()).collect(),
            multi: false,
            secret: false,
        }
    }

    #[test]
    fn one_question_reads_as_before() {
        let q = AskUserRequest::for_test(vec![ask("Which database?", &["Postgres", "SQLite"])]);
        assert_eq!(
            questionnaire_text(&q),
            "Which database?\n\n- Postgres\n- SQLite"
        );
    }

    #[test]
    fn every_question_goes_to_linear_with_its_choices() {
        let q = AskUserRequest::for_test(vec![
            ask("Which database?", &["Postgres", "SQLite"]),
            ask("Which port?", &[]),
            ask("Add a cache?", &["Yes", " ", "No"]),
        ]);
        assert_eq!(
            questionnaire_text(&q),
            "1. Which database?\n   - Postgres\n   - SQLite\n\n2. Which port?\n\n\
             3. Add a cache?\n   - Yes\n   - No"
        );
    }

    #[test]
    fn one_question_with_short_choices_gives_them_to_pick_from() {
        let q =
            AskUserRequest::for_test(vec![ask("Which database?", &["Postgres", " ", "SQLite"])]);
        assert_eq!(choices(&q), ["Postgres", "SQLite"]);
        // Anything else goes as text: several questions, several answers, a
        // secret, one choice, too many, or one too long.
        let many = AskUserRequest::for_test(vec![ask("A?", &["x", "y"]), ask("B?", &["x", "y"])]);
        let mut multi = ask("Which?", &["x", "y"]);
        multi.multi = true;
        let mut secret = ask("Token?", &["x", "y"]);
        secret.secret = true;
        let long = "x".repeat(OPTION_LIMIT + 1);
        let lots: Vec<String> = (0..=OPTIONS_LIMIT).map(|i| i.to_string()).collect();
        let lots: Vec<&str> = lots.iter().map(String::as_str).collect();
        for q in [
            many,
            AskUserRequest::for_test(vec![multi]),
            AskUserRequest::for_test(vec![secret]),
            AskUserRequest::for_test(vec![ask("Go on?", &["Yes"])]),
            AskUserRequest::for_test(vec![ask("Go on?", &[])]),
            AskUserRequest::for_test(vec![ask("Which?", &["a", &long])]),
            AskUserRequest::for_test(vec![ask("Which?", &lots)]),
        ] {
            assert!(choices(&q).is_empty());
        }
        // A secret in a choice never leaves.
        let key = "sk-ant-api03-abcdefghijklmnopqrstuvwxyz0123456789ABCDEFGHIJ";
        let q = AskUserRequest::for_test(vec![ask("Which key?", &[key, "none"])]);
        assert!(!choices(&q).iter().any(|o| o.contains(key)));
        let seen = Seen::Question {
            text: "Which database?".into(),
            options: vec!["Postgres".into(), "SQLite".into()],
        };
        assert_eq!(options_of(&seen), ["Postgres", "SQLite"]);
        assert!(options_of(&Seen::Cancelled).is_empty());
    }

    #[test]
    fn a_question_without_text_uses_its_topic() {
        let mut one = ask("", &["A", "B"]);
        one.topic = "Approach".into();
        let q = AskUserRequest::for_test(vec![one]);
        assert_eq!(questionnaire_text(&q), "Approach\n\n- A\n- B");
    }

    #[test]
    fn a_long_question_is_cut() {
        let (_, body) = report(Seen::Question {
            text: "é".repeat(QUESTION_LIMIT + 10),
            options: Vec::new(),
        })
        .unwrap();
        assert_eq!(body.matches('é').count(), QUESTION_LIMIT);
        assert!(body.ends_with('…'));
    }

    #[test]
    fn a_permission_says_what_and_where_to_answer() {
        assert_eq!(
            report(Seen::Permission(permission_what(Some("execute"), false))),
            Some((
                ReportKind::Elicitation,
                "Waiting for your permission: run a command. Answer it in Arakun or on your \
                 phone."
                    .into()
            ))
        );
        assert_eq!(
            report(Seen::Permission(String::new())),
            Some((
                ReportKind::Elicitation,
                "Waiting for your permission. Answer it in Arakun or on your phone.".into()
            ))
        );
    }

    #[test]
    fn a_permission_never_carries_its_arguments() {
        // Only the tool's kind is known to the report; an unknown kind gets
        // the fixed line, never the request's title.
        for kind in [
            Some("execute"),
            Some("edit"),
            Some("read"),
            Some("delete"),
            Some("move"),
            Some("search"),
            Some("fetch"),
            Some("other"),
            Some("think"),
            None,
        ] {
            let what = permission_what(kind, false);
            assert!(what.chars().count() <= WHAT_LIMIT && !what.contains('/'));
            let (_, body) = report(Seen::Permission(what)).unwrap();
            assert!(
                body.ends_with("Answer it in Arakun or on your phone."),
                "{body}"
            );
        }
        assert_eq!(permission_what(Some("other"), false), "");
        assert_eq!(permission_what(None, true), "carry out its plan");
        // Whatever reaches it is one short line.
        let (_, body) = report(Seen::Permission(format!("{}\nrm -rf ~", "x".repeat(200)))).unwrap();
        assert!(!body.contains("rm -rf") && body.contains(&"x".repeat(WHAT_LIMIT)));
        assert!(!body.contains(&"x".repeat(WHAT_LIMIT + 1)));
    }

    fn tool(kind: &str, status: &str, locations: &[&str]) -> Entry {
        Entry::Tool(
            serde_json::from_value(serde_json::json!({
                "id": "tc", "title": "Run `curl -H 'Authorization: Bearer hunter2'`",
                "kind": kind, "status": status, "locations": locations,
                "raw_input": {"command": "curl -H 'Authorization: Bearer hunter2'"},
            }))
            .unwrap(),
        )
    }

    fn user() -> Entry {
        serde_json::from_value(serde_json::json!({"type": "user", "text": "go", "at": 0})).unwrap()
    }

    #[test]
    fn progress_names_the_running_step_and_its_file() {
        let cwd = Path::new("/home/rik/app");
        let line = |e: Entry| progress_line(&[user(), e], cwd);
        assert_eq!(
            line(tool("edit", "in_progress", &["/home/rik/app/src/main.rs"])).as_deref(),
            Some("Editing `src/main.rs`")
        );
        assert_eq!(
            line(tool("read", "pending", &["/etc/hosts"])).as_deref(),
            Some("Reading `hosts`"),
            "outside the folder only the file name goes"
        );
        for (kind, want) in [
            ("execute", "Running a command"),
            ("search", "Searching the code"),
            ("fetch", "Reading a web page"),
        ] {
            let got = line(tool(kind, "in_progress", &["/home/rik/app/secrets.env"])).unwrap();
            assert_eq!(got, want);
            assert!(!got.contains("hunter2") && !got.contains("curl"));
        }
        // A step on a file with no file known yet, or a kind that says
        // nothing, is not shown.
        assert_eq!(line(tool("edit", "in_progress", &[])), None);
        assert_eq!(line(tool("other", "in_progress", &[])), None);
    }

    #[test]
    fn progress_is_only_the_latest_step_of_this_turn_while_it_runs() {
        let cwd = Path::new("/a");
        let done = tool("edit", "completed", &["/a/x.rs"]);
        let running = tool("execute", "in_progress", &[]);
        assert_eq!(progress_line(&[user(), done.clone()], cwd), None);
        assert_eq!(progress_line(&[running.clone(), user()], cwd), None);
        let text = Entry::Agent {
            text: "Now the tests.".into(),
        };
        assert_eq!(
            progress_line(&[user(), running, text], cwd).as_deref(),
            Some("Running a command")
        );
    }

    #[test]
    fn a_progress_path_cannot_break_out_or_run_long() {
        let cwd = Path::new("/a");
        let odd = tool("edit", "in_progress", &["/a/x`y\nz.rs"]);
        assert_eq!(
            progress_line(&[odd], cwd).as_deref(),
            Some("Editing `xyz.rs`")
        );
        let deep = format!("/a/{}/end.rs", "d/".repeat(200));
        let line = progress_line(&[tool("read", "pending", &[&deep])], cwd).unwrap();
        assert!(line.ends_with("/end.rs`") && line.contains('…'), "{line}");
        assert_eq!(line.chars().count(), "Reading ``".len() + PATH_LIMIT);
    }

    #[test]
    fn a_pending_permission_never_sends_its_title_or_arguments() {
        let title = "Run `curl -H 'Authorization: Bearer hunter2' https://internal.example/x`";
        let tool: crate::agent::ToolCall = serde_json::from_value(serde_json::json!({
            "id": "tc1",
            "title": title,
            "kind": "execute",
            "status": "pending",
            "raw_input": {"command": "curl -H 'Authorization: Bearer hunter2'", "cwd": "/home/rik/app"},
            "locations": ["/home/rik/app/secrets.env"],
        }))
        .unwrap();
        let entries = [Entry::Tool(tool)];
        let (key, seen) = permission_ask("tc1", title, false, &entries);
        assert_eq!(seen, Seen::Permission("run a command".into()));
        let (_, body) = report(seen).unwrap();
        assert_eq!(
            body,
            "Waiting for your permission: run a command. Answer it in Arakun or on your phone."
        );
        for text in [&body, &key] {
            for private in [
                "curl",
                "hunter2",
                "internal.example",
                "secrets.env",
                "/home/rik",
            ] {
                assert!(!text.contains(private), "{text}");
            }
        }
        // A tool call the session does not list, or of an unknown kind: the
        // fixed line, never the title in its place.
        for entries in [&entries[..0], &entries[..]] {
            let (_, seen) = permission_ask("tc-other", title, false, entries);
            assert_eq!(seen, Seen::Permission(String::new()));
            let (_, body) = report(seen).unwrap();
            assert_eq!(
                body,
                "Waiting for your permission. Answer it in Arakun or on your phone."
            );
        }
        // A plan to carry out is named as that, not by its text.
        let (_, seen) = permission_ask("tc1", "Implement: drop the users table", true, &entries);
        assert_eq!(seen, Seen::Permission("carry out its plan".into()));
        // Another ask of the same tool call is a new one.
        assert_ne!(key, permission_ask("tc1", "Run `ls`", false, &entries).0);
    }

    #[test]
    fn a_finished_turn_reports_its_reply() {
        assert_eq!(
            report(Seen::Finished {
                reply: "Opened the pull request.".into()
            }),
            Some((ReportKind::Response, "Opened the pull request.".into()))
        );
        assert_eq!(
            report(Seen::Finished {
                reply: " \n".into()
            }),
            Some((ReportKind::Response, "Done.".into()))
        );
    }

    #[test]
    fn a_long_reply_is_cut_with_a_note() {
        let long = "x".repeat(8_000);
        let (_, body) = report(Seen::Finished {
            reply: long.clone(),
        })
        .unwrap();
        assert_eq!(body, cut(&long));
        assert!(body.ends_with("…\n\nThe rest is in Arakun."));
    }

    #[test]
    fn a_secret_never_leaves_in_a_reply_or_question() {
        let key = "sk-ant-api03-abcdefghijklmnopqrstuvwxyz0123456789ABCDEFGHIJ";
        for seen in [
            Seen::Finished {
                reply: format!("Used {key}"),
            },
            Seen::Question {
                text: format!("Is {key} right?"),
                options: Vec::new(),
            },
            Seen::Failed(format!("bad key {key}")),
        ] {
            let (_, body) = report(seen).unwrap();
            assert!(!body.contains(key), "{body}");
        }
    }

    #[test]
    fn a_failure_says_why() {
        assert_eq!(
            report(Seen::Failed("Claude exited with code 1\nmore".into())),
            Some((
                ReportKind::Error,
                "The session failed: Claude exited with code 1".into()
            ))
        );
        assert_eq!(
            report(Seen::Failed(String::new())),
            Some((
                ReportKind::Error,
                "The session failed: Stopped with an error.".into()
            ))
        );
    }

    #[test]
    fn a_cancel_says_nothing() {
        assert_eq!(report(Seen::Cancelled), None);
    }

    #[test]
    fn the_fixed_lines_read_as_in_the_plan() {
        assert_eq!(
            closed_line("rik-mac"),
            "Arakun was closed on rik-mac while this ran. Reply here to continue."
        );
        assert_eq!(STILL_WORKING, "Still working.");
    }

    #[test]
    fn events_are_told_apart() {
        let idle = Now::default();
        let asking = Now {
            asking: true,
            ..idle
        };
        assert_eq!(step(Event::Finished("end_turn"), idle), Step::Finished);
        assert_eq!(step(Event::Finished("max_tokens"), idle), Step::Finished);
        assert_eq!(step(Event::Finished("cancelled"), asking), Step::Cancelled);
        assert_eq!(step(Event::Finished("error"), idle), Step::Failed);
        // A turn that ends with a question open waits on the person.
        assert_eq!(step(Event::Finished("end_turn"), asking), Step::Ask);
        // Its queue starts the next turn: not over yet.
        let queued = Now {
            continues: true,
            ..idle
        };
        assert_eq!(step(Event::Finished("end_turn"), queued), Step::Nothing);
        assert_eq!(step(Event::Status, asking), Step::Ask);
        let working = Now {
            working: true,
            ..idle
        };
        assert_eq!(step(Event::Status, working), Step::Working);
        let failed = Now {
            failed: true,
            ..idle
        };
        assert_eq!(step(Event::Status, failed), Step::Failed);
        assert_eq!(step(Event::Status, idle), Step::Nothing);
    }

    #[test]
    fn a_run_moves_with_its_session() {
        use RunState::*;
        let to = |state, s| apply(state, s).map(|m| (m.to, m.report, m.frees));
        for at in [Running, Asked] {
            assert_eq!(to(at, Step::Ask), Some((Asked, true, false)));
            assert_eq!(to(at, Step::Finished), Some((Done, true, true)));
            assert_eq!(to(at, Step::Failed), Some((Done, true, true)));
            // Cancelled in Arakun: over, and nothing said.
            assert_eq!(to(at, Step::Cancelled), Some((Done, false, true)));
        }
        assert_eq!(to(Asked, Step::Working), Some((Running, false, false)));
        assert_eq!(to(Running, Step::Working), None);
        // Typed to in Arakun after it ended: its next result is reported too.
        assert_eq!(to(Done, Step::Working), Some((Running, false, false)));
        assert_eq!(
            to(Interrupted, Step::Working),
            Some((Running, false, false))
        );
        // Stopped from Linear: the cancel's turn end changes nothing.
        for s in [Step::Ask, Step::Finished, Step::Failed, Step::Cancelled] {
            assert_eq!(to(Done, s), None, "{s:?}");
            assert_eq!(to(Waiting, s), None, "{s:?}");
        }
        assert_eq!(to(Running, Step::Nothing), None);
    }

    #[test]
    fn the_reply_is_the_last_words_of_the_latest_turn() {
        let user = || Entry::User {
            text: "go".into(),
            at: 1,
            elapsed_ms: None,
            message_id: None,
            tokens: None,
            shell_changes: Vec::new(),
        };
        let agent = |t: &str| Entry::Agent { text: t.into() };
        assert_eq!(
            turn_reply(&[
                user(),
                agent("old"),
                user(),
                agent("**First**"),
                agent("Last\n\nwith a list"),
                agent("  ")
            ]),
            "Last\n\nwith a list"
        );
        assert_eq!(turn_reply(&[agent("old"), user()]), "");
    }

    #[test]
    fn the_link_is_a_web_address() {
        let pr = "https://github.com/a/b/pull/1";
        assert_eq!(pr_link(Some(pr), None).as_deref(), Some(pr));
        assert_eq!(
            pr_link(Some("file:///x"), Some(pr)).as_deref(),
            Some(pr),
            "the CI view's when the session's is not one"
        );
        assert_eq!(pr_link(None, Some("javascript:x")), None);
        assert_eq!(pr_link(None, None), None);
    }

    #[test]
    fn an_ask_is_named_without_its_text() {
        let a = ask_key(&["q", "tc1", "Which database?"]);
        assert_eq!(a, ask_key(&["q", "tc1", "Which database?"]));
        assert_ne!(a, ask_key(&["q", "tc2", "Which database?"]));
        assert_ne!(a, ask_key(&["q", "tc1Which", " database?"]));
        assert!(!a.contains("database") && !a.is_empty());
    }

    fn ran(state: RunState) -> Run {
        Run { state, ..run() }
    }

    fn working() -> Now {
        Now {
            working: true,
            ..Now::default()
        }
    }

    fn asking() -> Now {
        Now {
            asking: true,
            ..Now::default()
        }
    }

    /// The steps that report, in order, as `(step, report, frees)`.
    fn feed(r: &mut Run, events: &[(Event, Now, Option<&str>)]) -> Vec<(Step, bool, bool)> {
        events
            .iter()
            .filter_map(|(ev, now, key)| advance(r, *ev, *now, *key))
            .map(|e| (e.step, e.report, e.frees))
            .collect()
    }

    #[test]
    fn an_asked_run_stopped_from_linear_stays_stopped() {
        // `linear_stop` marks it Done and stopped, then cancels the turn.
        let mut r = Run {
            stopped: true,
            ..ran(RunState::Done)
        };
        let seen = feed(
            &mut r,
            &[
                (Event::Status, working(), None),
                (Event::Finished("end_turn"), Now::default(), None),
            ],
        );
        assert!(
            seen.iter().all(|(_, report, frees)| !report && !frees),
            "{seen:?}"
        );
        assert_eq!((r.state, r.stopped), (RunState::Done, false));
        // After that turn, the person typing in Arakun brings it back.
        feed(&mut r, &[(Event::Status, working(), None)]);
        assert_eq!(r.state, RunState::Running);
    }

    #[test]
    fn a_stopped_run_ignores_every_event_until_its_turn_ends() {
        for reason in ["cancelled", "end_turn", "error"] {
            let mut r = Run {
                stopped: true,
                ..ran(RunState::Done)
            };
            assert!(advance(&mut r, Event::Status, asking(), Some("k")).is_none());
            assert!(advance(&mut r, Event::Status, working(), None).is_none());
            let end = advance(&mut r, Event::Finished(reason), Now::default(), None);
            assert!(end.is_some_and(|e| !e.report && !e.frees), "{reason}");
            assert_eq!((r.state, r.stopped), (RunState::Done, false), "{reason}");
        }
    }

    #[test]
    fn a_cancel_in_arakun_still_ends_the_run_quietly() {
        let mut r = ran(RunState::Asked);
        let seen = feed(
            &mut r,
            &[
                (Event::Status, working(), None),
                (Event::Finished("cancelled"), Now::default(), None),
            ],
        );
        assert_eq!(seen.last(), Some(&(Step::Cancelled, false, true)));
        assert!(seen.iter().all(|(_, report, _)| !report));
        assert_eq!(r.state, RunState::Done);
    }

    #[test]
    fn a_turn_that_left_work_in_the_background_keeps_the_run_open() {
        let background = Now {
            background: true,
            ..Now::default()
        };
        // Asking or a queue still go first.
        assert_eq!(
            step(Event::Finished("end_turn"), background),
            Step::Background
        );
        let asking_too = Now {
            asking: true,
            ..background
        };
        assert_eq!(step(Event::Finished("end_turn"), asking_too), Step::Ask);
        assert_eq!(step(Event::Finished("error"), background), Step::Failed);

        let mut r = ran(RunState::Running);
        r.held = Some(crate::linear::runs::Held {
            since: 1,
            body: "old".into(),
            link: None,
        });
        let seen = feed(
            &mut r,
            &[
                // The turn ends while a command runs in the background.
                (Event::Finished("end_turn"), background, None),
                // The agent's own turn once the command is done.
                (Event::Status, working(), None),
            ],
        );
        assert_eq!(seen, [(Step::Background, false, false)]);
        assert_eq!(r.state, RunState::Running);
        assert!(r.held.is_some(), "kept until the work is done");
        let seen = feed(
            &mut r,
            &[(Event::Finished("end_turn"), Now::default(), None)],
        );
        assert_eq!(seen, [(Step::Finished, true, true)]);
        assert_eq!((r.state, r.held.is_none()), (RunState::Done, true));
    }

    #[test]
    fn the_same_ask_again_after_an_answer_is_told_again() {
        let mut r = ran(RunState::Running);
        let seen = feed(
            &mut r,
            &[
                (Event::Status, asking(), Some("proceed")),
                (Event::Status, asking(), Some("proceed")),
                (Event::Status, working(), None),
                (Event::Status, asking(), Some("proceed")),
            ],
        );
        let told = seen
            .iter()
            .filter(|(s, report, _)| *s == Step::Ask && *report);
        assert_eq!(told.count(), 2, "{seen:?}");
        assert_eq!(r.state, RunState::Asked);
    }

    #[test]
    fn a_session_waiting_for_you_is_told_once() {
        assert_eq!(
            report(Seen::Waiting {
                computer: "rik-mac".into()
            }),
            Some((
                ReportKind::Elicitation,
                "Waiting for you in Arakun on rik-mac.".into()
            ))
        );
        let mut r = ran(RunState::Running);
        let seen = feed(
            &mut r,
            &[
                (Event::Status, asking(), Some(WAITING_KEY)),
                (Event::Finished("end_turn"), asking(), Some(WAITING_KEY)),
            ],
        );
        assert_eq!(
            seen,
            [(Step::Ask, true, false)],
            "the second changes nothing"
        );
        assert_eq!(r.state, RunState::Asked);
    }

    #[test]
    fn a_removed_session_has_its_line() {
        assert_eq!(
            removed_line("rik-mac"),
            "The session for this was removed in Arakun on rik-mac. Delegate the issue \
             again to start over."
        );
    }
}
