//! Telling a Linear issue what its delegated session does: a question, a
//! permission it waits for, the result, a failure, that it still works, and
//! that Arakun was closed while it ran.
//!
//! A session event only updates the run and queues a report; the outbox
//! sends it. Only the last reply, a question's text and a short name of what
//! a permission is for go to Linear: never a permission's arguments, which
//! can hold file contents, commands with secrets or whole diffs.

use std::time::Duration;

use gpui::{Context, Entity, Window};

use super::Shell;
use super::linear_run::{computer, failed_line};
use crate::agent::{Entry, Thread, ThreadEvent, ThreadStatus};
use crate::linear::runs::{ReportKind, Run, RunState, cut};
use crate::model::unix_now;
use crate::phone::notify;

/// The most characters of a question sent to Linear.
const QUESTION_LIMIT: usize = 2_000;
/// The most characters of what a permission is for.
const WHAT_LIMIT: usize = 80;
/// How often a quiet session is looked at and the outbox tried again.
const TICK: Duration = Duration::from_secs(5 * 60);

pub const STILL_WORKING: &str = "Still working.";
/// A finished turn that said nothing.
const DONE: &str = "Done.";

/// What a delegated session did, as Linear hears it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Seen {
    /// The question's text, with its choices.
    Question(String),
    /// What the permission is for, or empty when only a description with its
    /// arguments is known.
    Permission(String),
    Finished {
        reply: String,
    },
    Failed(String),
    Cancelled,
}

/// The report for what the session did, in the plan's words. A cancel says
/// nothing: a stop from Linear was told already, and one in Arakun is the
/// person's own.
pub fn to_report(ev: Seen, _run: &Run) -> Option<(ReportKind, String)> {
    Some(match ev {
        Seen::Question(text) => {
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
        Seen::Cancelled => return None,
    })
}

pub fn closed_line(computer: &str) -> String {
    format!("Arakun was closed on {computer} while this ran. Reply here to continue.")
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
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Step {
    Ask,
    Working,
    Finished,
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
        Step::Cancelled if at_work => to(Done, false, true),
        Step::Working if matches!(state, Asked | Done | Interrupted) => to(Running, false, false),
        _ => None,
    }
}

/// The last thing the agent said in its latest turn, whole.
fn turn_reply(entries: &[Entry]) -> String {
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
fn pr_link(session: Option<&str>, ci: Option<&str>) -> Option<String> {
    [session, ci]
        .into_iter()
        .flatten()
        .find(|u| u.starts_with("https://"))
        .map(str::to_string)
}

/// Names one ask without keeping its text.
fn ask_key(parts: &[&str]) -> String {
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

/// What waits on a person in `t`, if anything: a key for it and what Linear
/// hears. A permission comes first, as on the desktop.
fn ask_of(t: &Thread) -> Option<(String, Seen)> {
    if let Some(p) = t.pending_permission() {
        let kind = t.entries().iter().find_map(|e| match e {
            Entry::Tool(tc) if tc.id == p.tool_call_id => Some(tc.kind.as_str()),
            _ => None,
        });
        let what = permission_what(kind, p.plan.is_some());
        let key = ask_key(&["permission", &p.tool_call_id, &p.title]);
        return Some((key, Seen::Permission(what)));
    }
    let q = t.pending_question()?;
    // A secret's field is answered in Arakun only.
    let options = q
        .questions
        .first()
        .filter(|first| !first.secret)
        .map(|first| first.options.clone())
        .unwrap_or_default();
    let text = question_text(&notify::question_body(q), &options);
    let key = ask_key(&["question", &q.tool_call_id, &text]);
    Some((key, Seen::Question(text)))
}

impl Shell {
    /// A session event, for a session that works on a Linear issue. Cheap
    /// for any other session: one lookup, then nothing.
    pub(super) fn linear_thread_event(
        &mut self,
        id: &str,
        thread: &Entity<Thread>,
        ev: &ThreadEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let event = match ev {
            ThreadEvent::StatusChanged | ThreadEvent::NeedsPermission => Event::Status,
            ThreadEvent::Finished { stop_reason } => Event::Finished(stop_reason),
            _ => return,
        };
        let Some(run) = self.linear_runs.by_session(id) else {
            return;
        };
        let (state, asked) = (run.state, run.asked.clone());
        let t = thread.read(cx);
        let ask = ask_of(t);
        let status = t.status();
        let now = Now {
            working: status == ThreadStatus::Working,
            failed: matches!(status, ThreadStatus::Error(_)),
            asking: ask.is_some(),
            continues: !t.queued().is_empty() && !t.queue_paused(),
        };
        let step = step(event, now);
        let Some(mv) = apply(state, step) else {
            return;
        };
        let mut report = mv.report;
        let mut ask_key = None;
        let seen = match step {
            Step::Ask => {
                let Some((key, seen)) = ask else {
                    return;
                };
                // Events repeat while one ask is pending: it is told once.
                report &= asked.as_deref() != Some(key.as_str());
                ask_key = Some(key);
                seen
            }
            Step::Finished => Seen::Finished {
                reply: turn_reply(t.entries()),
            },
            Step::Failed => Seen::Failed(match status {
                ThreadStatus::Error(msg) => msg,
                _ => String::new(),
            }),
            Step::Cancelled | Step::Working | Step::Nothing => Seen::Cancelled,
        };
        let session_pr = t.meta().pr.clone();
        if mv.to == state && !report {
            return;
        }
        let link = if step == Step::Finished {
            self.linear_pr(id, session_pr.as_deref(), cx)
        } else {
            None
        };
        let Some(run) = self.linear_runs.by_session(id) else {
            return;
        };
        run.state = mv.to;
        if ask_key.is_some() {
            run.asked = ask_key;
        }
        let agent_session = run.agent_session.clone();
        // The new state goes in the same save as the report.
        match report.then(|| to_report(seen, run)).flatten() {
            Some((kind, body)) => self.linear_report(&agent_session, kind, body, link, cx),
            None => self.linear_save(),
        }
        if mv.to == RunState::Running {
            self.linear_watch(cx);
        }
        if mv.frees {
            self.linear_start_waiting(window, cx);
        }
    }

    /// The pull request of a run's work, when one is known now. Nothing is
    /// fetched: the CI view may not know it yet.
    fn linear_pr(&mut self, id: &str, session: Option<&str>, cx: &Context<Self>) -> Option<String> {
        let run = self.linear_runs.by_session(id)?;
        let (project, branch) = (run.project.clone(), run.branch.clone());
        let ci = self.ci.read(cx);
        let ci_pr = ci
            .repo(&project)
            .and_then(|r| r.pr_of(&branch))
            .map(|p| p.url.as_str());
        pr_link(session, ci_pr)
    }

    /// At start-up, once the sessions are loaded: a run whose session has no
    /// live agent was cut off when Arakun closed. Its issue hears it, and
    /// the runs that wait start in the freed slots.
    pub(super) fn linear_recover(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let live: std::collections::HashSet<String> = self
            .projects
            .iter()
            .flat_map(|p| &p.environments)
            .flat_map(|e| &e.sessions)
            .filter(|s| s.live.is_some())
            .map(|s| s.id.to_string())
            .collect();
        let lost = self.linear_runs.interrupted(&|s| live.contains(s));
        if !lost.is_empty() {
            self.linear_save();
        }
        let line = closed_line(&computer());
        for agent_session in lost {
            self.linear_report(&agent_session, ReportKind::Error, line.clone(), None, cx);
        }
        self.linear_start_waiting(window, cx);
        self.linear_watch(cx);
    }

    /// Starts the one timer, while a run works or reports wait. It tells
    /// Linear a quiet session still works and sends the outbox again; it
    /// ends once neither holds.
    pub(super) fn linear_watch(&mut self, cx: &mut Context<Self>) {
        if self.linear_watching || !self.linear_needs_watch() {
            return;
        }
        self.linear_watching = true;
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(TICK).await;
                let more = this.update(cx, |this, cx| this.linear_tick(cx));
                if !more.unwrap_or(false) {
                    break;
                }
            }
        })
        .detach();
    }

    fn linear_needs_watch(&self) -> bool {
        self.linear_runs.any_running() || self.linear_runs.unsent()
    }

    fn linear_tick(&mut self, cx: &mut Context<Self>) -> bool {
        let now = unix_now();
        for agent_session in self.linear_runs.quiet(now) {
            let line = STILL_WORKING.to_string();
            self.linear_report(&agent_session, ReportKind::Thought, line, None, cx);
        }
        // Reports older than a day go, so a signed-out outbox ends the timer.
        self.linear_runs.due(now);
        self.linear_flush(cx);
        let more = self.linear_needs_watch();
        self.linear_watching = more;
        more
    }
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
        }
    }

    fn report(seen: Seen) -> Option<(ReportKind, String)> {
        to_report(seen, &run())
    }

    #[test]
    fn a_question_is_an_elicitation_with_its_choices() {
        let text = question_text("Which database?", &["Postgres".into(), "SQLite".into()]);
        assert_eq!(text, "Which database?\n\n- Postgres\n- SQLite");
        assert_eq!(question_text("Go on?", &[]), "Go on?");
        assert_eq!(
            report(Seen::Question(text.clone())),
            Some((ReportKind::Elicitation, text))
        );
    }

    #[test]
    fn a_long_question_is_cut() {
        let (_, body) = report(Seen::Question("é".repeat(QUESTION_LIMIT + 10))).unwrap();
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
            Seen::Question(format!("Is {key} right?")),
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
}
