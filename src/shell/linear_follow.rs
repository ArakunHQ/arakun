//! A reply or a stop from Linear for a session it started. The relay only
//! says that a reply exists: its text is read from Linear with the person's
//! own key, never taken from the relay, and only while the switch is on. The
//! text is data: it goes to the session as a message and never into a log.
//!
//! Like a new delivery, each one is recorded as handled in the same save as
//! its outcome, and accepted after that save.

use std::collections::HashMap;
use std::path::PathBuf;

use gpui::{AppContext as _, Context, Window};

use super::Shell;
use super::linear_run::{busy_line, computer, no_sign_in_line, refusal_line};
use crate::linear::delegation::{Delegation, Refusal};
use crate::linear::runs::{ReportKind, Run, RunState};
use crate::linear::{Session, issues};
use crate::phone::linear::{Delivery, DeliveryKind};

/// Whether the first look at a delivery of `kind` refuses it for lack of a
/// Linear sign-in here. A stop needs no key. A reply needs one too, but the
/// switch is asked first: [`reply_source`] does both.
pub fn needs_sign_in(kind: DeliveryKind) -> bool {
    match kind {
        DeliveryKind::Created => true,
        DeliveryKind::Prompt | DeliveryKind::Stop => false,
    }
}

/// Where a reply's text comes from, or why it is not passed on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Source<'a> {
    /// The switch is off: nothing from Linear reaches a session.
    Off,
    /// No Linear sign-in for the reply's workspace, so no key to read it with.
    NoSignIn,
    /// The relay named no activity to read.
    Unreadable,
    /// Read this agent activity from Linear with the person's own key.
    Read(&'a str),
}

/// The checks a reply passes before its text is read. The relay's own copy
/// of the text (`body`) is never used: whoever holds the relay could
/// otherwise type into a session that commits and pushes.
pub fn reply_source(d: &Delivery, switch_on: bool, signed_in: bool) -> Source<'_> {
    if !switch_on {
        Source::Off
    } else if !signed_in {
        Source::NoSignIn
    } else {
        match d.activity.as_deref() {
            Some(activity) if !activity.is_empty() => Source::Read(activity),
            _ => Source::Unreadable,
        }
    }
}

/// The state of the run a follow-up names. A run of another organization
/// counts as none: the agent session id alone is not trusted.
pub fn run_state(run: Option<&Run>, organization: &str) -> Option<RunState> {
    run.filter(|r| r.organization == organization)
        .map(|r| r.state)
}

/// Whether a stop must wait: other work for its agent session is in flight
/// (a new delegation being read, or a reply whose text is being read).
/// Run now, it would find no run yet, or be undone by the reply.
pub fn hold_stop(in_flight: &HashMap<String, String>, agent_session: &str) -> bool {
    in_flight.values().any(|a| a == agent_session)
}

/// The held stops for `agent_session` once nothing else for it is in flight,
/// taken out of `held` and `in_flight`.
pub fn release_stops(
    held: &mut Vec<Delivery>,
    in_flight: &mut HashMap<String, String>,
    agent_session: &str,
) -> Vec<Delivery> {
    let busy = in_flight
        .iter()
        .any(|(id, a)| a == agent_session && !held.iter().any(|h| &h.id == id));
    if busy {
        return Vec::new();
    }
    let (out, keep): (Vec<Delivery>, Vec<Delivery>) = std::mem::take(held)
        .into_iter()
        .partition(|h| h.agent_session == agent_session);
    *held = keep;
    for h in &out {
        in_flight.remove(&h.id);
    }
    out
}

/// What a reply from Linear leads to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FollowUp {
    /// No run, or its session is gone here.
    NoRun,
    /// The run waits for a slot: there is no session to send it to yet.
    NotStarted,
    /// The run ended and another run works on its issue now: a reply here
    /// would put two sessions in one worktree.
    Busy,
    Unreadable,
    Send,
}

/// A run that started takes a reply, even one that ended: the person may
/// want more done. `text` is what was read from Linear, if anything was.
/// `issue_busy`: a run for the same issue is waiting, running or asking.
pub fn follow_up(
    run: Option<RunState>,
    session_exists: bool,
    issue_busy: bool,
    text: Option<&str>,
) -> FollowUp {
    match run {
        None => FollowUp::NoRun,
        Some(RunState::Waiting) => FollowUp::NotStarted,
        Some(_) if !session_exists => FollowUp::NoRun,
        // Busy with an ended run means another run: this one is not active.
        Some(RunState::Done | RunState::Interrupted) if issue_busy => FollowUp::Busy,
        Some(_) => match text {
            Some(t) if !t.trim().is_empty() => FollowUp::Send,
            _ => FollowUp::Unreadable,
        },
    }
}

/// The state a run takes once a reply from Linear is passed on to its
/// session. An asked run stays asked: the message waits in the session's
/// queue behind the question or permission. A running one is unchanged.
pub fn after_reply(state: RunState) -> Option<RunState> {
    match state {
        RunState::Done | RunState::Interrupted => Some(RunState::Running),
        RunState::Waiting | RunState::Running | RunState::Asked => None,
    }
}

/// What a stop from Linear leads to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stop {
    /// No run: nothing to stop and nobody to tell.
    Nothing,
    /// Already over: Linear hears it stopped.
    Report,
    /// Waiting for a slot: it leaves the queue.
    Dequeue,
    /// At work: its turn is cancelled.
    Cancel,
}

pub fn stop(run: Option<RunState>) -> Stop {
    match run {
        None => Stop::Nothing,
        Some(RunState::Waiting) => Stop::Dequeue,
        Some(RunState::Running | RunState::Asked) => Stop::Cancel,
        Some(RunState::Done | RunState::Interrupted) => Stop::Report,
    }
}

pub const STOPPED: &str = "Stopped.";

pub fn no_session_line(computer: &str) -> String {
    format!("{computer} has no session for this any more. Delegate the issue again.")
}

pub fn unread_line(computer: &str) -> String {
    format!(
        "Arakun could not read that message, because {computer} was offline when you sent it. \
         Send it again."
    )
}

pub fn not_started_line(computer: &str) -> String {
    format!(
        "This has not started yet on {computer}. Your message will not be passed on; send it \
         again once it has started."
    )
}

impl Shell {
    /// A reply from Linear: passed to the run's session as a new message,
    /// never as the answer to a permission. Its text is read from Linear.
    pub(super) fn linear_prompt(
        &mut self,
        d: Delivery,
        sign_in: Option<Session>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let on = Delegation::load().enabled;
        let activity = match reply_source(&d, on, sign_in.is_some()) {
            Source::Off => {
                let line = refusal_line(&Refusal::Off, &computer());
                return self.linear_turn_away(&d, line, cx);
            }
            Source::NoSignIn => {
                self.linear_turn_away(&d, no_sign_in_line(&computer()), cx);
                // The server must stop sending this workspace's replies here.
                return self.sync_linear(window, cx);
            }
            Source::Unreadable => None,
            Source::Read(activity) => Some(activity.to_string()),
        };
        // Without its text yet: is there a session to read it for?
        let decided = self.linear_follow_up(&d, None);
        let (Some(activity), Some(sign_in), FollowUp::Unreadable) = (activity, sign_in, decided)
        else {
            return self.linear_answer(&d.id, &d.agent_session, decided, None, window, cx);
        };
        let flight = d.agent_session.clone();
        self.linear_in_flight.insert(d.id.clone(), flight);
        let agent_session = d.agent_session.clone();
        cx.spawn_in(window, async move |this, cx| {
            // Any error, a panic too, means the text could not be read.
            let text = cx
                .background_spawn(crate::guard::task_or(
                    async move { issues::prompt_text(&sign_in, &activity, &agent_session).ok() },
                    |_| None,
                ))
                .await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.linear_prompt_read(d, text, window, cx)
            });
        })
        .detach();
    }

    /// A reply that is refused whatever its run: the issue hears why.
    fn linear_turn_away(&mut self, d: &Delivery, line: String, cx: &mut Context<Self>) {
        // Saved with the report.
        self.linear_runs.mark_seen(&d.id);
        self.linear_report(&d.agent_session, ReportKind::Error, line, None, cx);
        self.linear_accept(&d.id, cx);
    }

    fn linear_prompt_read(
        &mut self,
        d: Delivery,
        text: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.linear_in_flight.remove(&d.id);
        if self.linear_gate(cx).is_none() {
            // Signed out of Arakun meanwhile: not handled. It is kept, or the
            // relay sends it again, for when there is an account to accept
            // it with. The text is read again then.
            self.linear_keep(d.clone(), cx);
        } else if !Delegation::load().enabled {
            // Turned off while the text was read.
            let line = refusal_line(&Refusal::Off, &computer());
            self.linear_turn_away(&d, line, cx);
        } else {
            // The run may have changed during the read.
            let decided = self.linear_follow_up(&d, text.as_deref());
            self.linear_answer(&d.id, &d.agent_session, decided, text, window, cx);
        }
        // A stop sent after this reply goes after it too.
        self.linear_release_stops(&d.agent_session, window, cx);
    }

    fn linear_follow_up(&mut self, d: &Delivery, text: Option<&str>) -> FollowUp {
        let run = self.linear_runs.by_agent_session(&d.agent_session);
        let state = run_state(run.as_deref(), &d.organization);
        // The run's own issue, not the one the relay names.
        let issue = run.map(|r| r.issue.clone());
        let exists = self.linear_session_of(&d.agent_session).is_some();
        let busy = issue.is_some_and(|i| self.linear_runs.active_for_issue(&d.organization, &i));
        follow_up(state, exists, busy, text)
    }

    /// Runs the stops held for `agent_session`, once nothing else for it is
    /// in flight. Signed out of Arakun meanwhile, they are not recorded: they
    /// are kept, or the relay sends them again.
    pub(super) fn linear_release_stops(
        &mut self,
        agent_session: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let stops = release_stops(
            &mut self.linear_held_stops,
            &mut self.linear_in_flight,
            agent_session,
        );
        let open = self.linear_gate(cx).is_some();
        for d in stops {
            if open {
                self.linear_stop(&d, window, cx);
            } else {
                self.linear_keep(d, cx);
            }
        }
    }

    fn linear_answer(
        &mut self,
        id: &str,
        agent_session: &str,
        decided: FollowUp,
        text: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // Saved with the outcome, below.
        self.linear_runs.mark_seen(id);
        let report = match decided {
            FollowUp::NoRun => Some((ReportKind::Error, no_session_line(&computer()))),
            FollowUp::NotStarted => Some((ReportKind::Thought, not_started_line(&computer()))),
            FollowUp::Busy => Some((ReportKind::Thought, busy_line(&computer()))),
            FollowUp::Unreadable => Some((ReportKind::Thought, unread_line(&computer()))),
            FollowUp::Send => {
                let text = text.unwrap_or_default();
                if self.linear_pass_on(agent_session, text, window, cx) {
                    None
                } else {
                    Some((ReportKind::Error, no_session_line(&computer())))
                }
            }
        };
        match report {
            Some((kind, line)) => self.linear_report(agent_session, kind, line, None, cx),
            None => self.linear_save(),
        }
        self.linear_accept(id, cx);
    }

    /// Sends `text` to the run's session as the composer would: one that is
    /// busy queues it. No tab opens and focus stays where it is.
    fn linear_pass_on(
        &mut self,
        agent_session: &str,
        text: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some((env, sid)) = self.linear_session_of(agent_session) else {
            return false;
        };
        // Another process runs it: the app only shows it, and a message to
        // that view would be dropped.
        if crate::headless::shown(&sid) {
            return false;
        }
        // A session is retired after its turn when no tab shows it.
        self.ensure_live(&env, &sid, window, cx);
        let Some(thread) = self.phone_thread(&env, &sid) else {
            return false;
        };
        thread.update(cx, |t, cx| t.send(text, cx));
        if let Some(s) = self.session_mut(&env, &sid) {
            s.updated = crate::model::unix_now();
        }
        // The person asked for more after it ended. An explicit reply is
        // not queued or refused, so a run that ended can pass the limit of
        // three here; new work then waits until the running sessions drop
        // below it.
        let mut revived = false;
        if let Some(run) = self.linear_runs.by_agent_session(agent_session) {
            // A new turn on purpose: a stop's cancel is behind it.
            run.stopped = false;
            if let Some(to) = after_reply(run.state) {
                run.state = to;
                run.asked = None;
                revived = true;
            }
        }
        if revived {
            self.linear_watch(cx);
        }
        cx.notify();
        true
    }

    fn linear_mark_stopped(&mut self, agent_session: &str, stopped: bool) {
        if let Some(run) = self.linear_runs.by_agent_session(agent_session) {
            run.stopped = stopped;
        }
    }

    /// The environment and id of the run's session, while both are still in
    /// Arakun: the person may have deleted either.
    fn linear_session_of(&mut self, agent_session: &str) -> Option<(PathBuf, String)> {
        let run = self.linear_runs.by_agent_session(agent_session)?;
        let (env, sid) = (run.environment.clone()?, run.session.clone()?);
        self.find_env(&env)?.session_index(&sid)?;
        Some((env, sid))
    }

    /// A stop from Linear: the session's turn is cancelled and the run ends.
    pub(super) fn linear_stop(
        &mut self,
        d: &Delivery,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if hold_stop(&self.linear_in_flight, &d.agent_session) {
            // In flight too, so a frame sent again is ignored, not accepted.
            let flight = d.agent_session.clone();
            self.linear_in_flight.insert(d.id.clone(), flight);
            self.linear_held_stops.push(d.clone());
            return;
        }
        let run = self.linear_runs.by_agent_session(&d.agent_session);
        let decided = stop(run_state(run.as_deref(), &d.organization));
        // Saved with the outcome, below.
        self.linear_runs.mark_seen(&d.id);
        if matches!(decided, Stop::Dequeue | Stop::Cancel)
            && let Some(run) = self.linear_runs.by_agent_session(&d.agent_session)
        {
            // Done before the cancel, so the turn's end is not taken for a
            // finish. A run still being made sees it and does not start.
            run.state = RunState::Done;
        }
        if decided == Stop::Cancel
            && let Some((env, sid)) = self.linear_session_of(&d.agent_session)
            && let Some(thread) = self.phone_thread(&env, &sid)
        {
            // While the cancel's turn winds down its events (back to work,
            // then the turn's end) must not move the run again.
            let busy = thread.read(cx).is_busy();
            self.linear_mark_stopped(&d.agent_session, busy);
            thread.update(cx, |t, cx| t.cancel(cx));
            // No turn end is coming: nothing to wait for.
            if busy && !thread.read(cx).is_cancelling() {
                self.linear_mark_stopped(&d.agent_session, false);
            }
        }
        match decided {
            Stop::Nothing => self.linear_save(),
            Stop::Report | Stop::Dequeue | Stop::Cancel => {
                let line = STOPPED.to_string();
                self.linear_report(&d.agent_session, ReportKind::Response, line, None, cx);
            }
        }
        self.linear_accept(&d.id, cx);
        if decided == Stop::Cancel {
            self.linear_start_waiting(window, cx);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ALL: [RunState; 5] = [
        RunState::Waiting,
        RunState::Running,
        RunState::Asked,
        RunState::Done,
        RunState::Interrupted,
    ];

    #[test]
    fn a_reply_without_a_run_has_no_session() {
        for exists in [false, true] {
            for busy in [false, true] {
                for text in [None, Some(""), Some("go on")] {
                    assert_eq!(follow_up(None, exists, busy, text), FollowUp::NoRun);
                }
            }
        }
    }

    #[test]
    fn a_reply_to_a_waiting_run_is_not_passed_on() {
        for exists in [false, true] {
            for text in [None, Some("go on")] {
                // A waiting run makes its own issue busy.
                assert_eq!(
                    follow_up(Some(RunState::Waiting), exists, true, text),
                    FollowUp::NotStarted
                );
            }
        }
    }

    #[test]
    fn a_reply_whose_session_was_deleted_has_no_session() {
        for state in ALL.into_iter().filter(|s| *s != RunState::Waiting) {
            for text in [None, Some("go on")] {
                assert_eq!(follow_up(Some(state), false, false, text), FollowUp::NoRun);
                assert_eq!(follow_up(Some(state), false, true, text), FollowUp::NoRun);
            }
        }
    }

    #[test]
    fn a_reply_without_readable_text_is_unreadable() {
        for state in ALL.into_iter().filter(|s| *s != RunState::Waiting) {
            for text in [None, Some(""), Some("  \n\t ")] {
                assert_eq!(
                    follow_up(Some(state), true, false, text),
                    FollowUp::Unreadable,
                    "{state:?} {text:?}"
                );
            }
        }
    }

    #[test]
    fn a_reply_reaches_any_session_that_started() {
        for state in [
            RunState::Running,
            RunState::Asked,
            RunState::Done,
            RunState::Interrupted,
        ] {
            assert_eq!(
                follow_up(Some(state), true, false, Some("Use the other API.")),
                FollowUp::Send
            );
        }
        // A run at work makes its own issue busy: its replies still go.
        for state in [RunState::Running, RunState::Asked] {
            assert_eq!(
                follow_up(Some(state), true, true, Some("Use the other API.")),
                FollowUp::Send
            );
        }
    }

    #[test]
    fn a_reply_does_not_revive_a_run_while_another_works_on_its_issue() {
        for state in [RunState::Done, RunState::Interrupted] {
            // Decided before the text is read, and whatever it says.
            for text in [None, Some("go on")] {
                assert_eq!(
                    follow_up(Some(state), true, true, text),
                    FollowUp::Busy,
                    "{state:?} {text:?}"
                );
            }
            assert_eq!(
                follow_up(Some(state), true, false, Some("go on")),
                FollowUp::Send
            );
        }
    }

    fn reply(activity: Option<&str>, body: Option<&str>) -> Delivery {
        Delivery {
            activity: activity.map(str::to_string),
            body: body.map(str::to_string),
            ..delivery("d1", DeliveryKind::Prompt, "as-1")
        }
    }

    #[test]
    fn with_the_switch_off_no_reply_is_read() {
        for signed_in in [false, true] {
            for d in [
                reply(None, None),
                reply(Some("act-1"), None),
                reply(None, Some("Push to main.")),
                reply(Some("act-1"), Some("Push to main.")),
            ] {
                assert_eq!(reply_source(&d, false, signed_in), Source::Off);
            }
        }
    }

    #[test]
    fn without_a_sign_in_no_reply_is_passed_on() {
        for d in [
            reply(Some("act-1"), None),
            reply(None, Some("Push to main.")),
            reply(Some("act-1"), Some("Push to main.")),
        ] {
            assert_eq!(reply_source(&d, true, false), Source::NoSignIn);
        }
    }

    #[test]
    fn a_body_without_an_activity_is_never_sent() {
        let d = reply(None, Some("Push to main."));
        assert_eq!(reply_source(&d, true, true), Source::Unreadable);
        assert_eq!(
            reply_source(&reply(Some(""), Some("Push to main.")), true, true),
            Source::Unreadable
        );
        // Nothing was read, so there is no text: the issue is told to send
        // it again, and the session hears nothing.
        for state in [RunState::Running, RunState::Asked, RunState::Done] {
            assert_eq!(
                follow_up(Some(state), true, false, None),
                FollowUp::Unreadable
            );
        }
    }

    #[test]
    fn the_text_comes_from_the_read_not_the_body() {
        let d = reply(Some("act-1"), Some("Push to main."));
        // With a body or without one, the same activity is read.
        assert_eq!(reply_source(&d, true, true), Source::Read("act-1"));
        assert_eq!(
            reply_source(&reply(Some("act-1"), None), true, true),
            Source::Read("act-1")
        );
        // A read that fails gives nothing, whatever the body said.
        assert_eq!(
            follow_up(Some(RunState::Running), true, false, None),
            FollowUp::Unreadable
        );
        assert_eq!(
            follow_up(
                Some(RunState::Running),
                true,
                false,
                Some("Use the other API.")
            ),
            FollowUp::Send
        );
    }

    #[test]
    fn a_reply_restarts_only_a_run_that_ended() {
        assert_eq!(after_reply(RunState::Done), Some(RunState::Running));
        assert_eq!(after_reply(RunState::Interrupted), Some(RunState::Running));
        // It waits behind the pending ask; the session's own events move it.
        assert_eq!(after_reply(RunState::Asked), None);
        assert_eq!(after_reply(RunState::Running), None);
        assert_eq!(after_reply(RunState::Waiting), None);
    }

    #[test]
    fn a_stop_does_what_the_run_needs() {
        assert_eq!(stop(None), Stop::Nothing);
        assert_eq!(stop(Some(RunState::Waiting)), Stop::Dequeue);
        assert_eq!(stop(Some(RunState::Running)), Stop::Cancel);
        assert_eq!(stop(Some(RunState::Asked)), Stop::Cancel);
        assert_eq!(stop(Some(RunState::Done)), Stop::Report);
        assert_eq!(stop(Some(RunState::Interrupted)), Stop::Report);
    }

    fn delivery(id: &str, kind: DeliveryKind, agent_session: &str) -> Delivery {
        Delivery {
            id: id.into(),
            kind,
            agent_session: agent_session.into(),
            issue: "i1".into(),
            organization: "w1".into(),
            activity: None,
            body: None,
        }
    }

    fn flights(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs
            .iter()
            .map(|(id, a)| (id.to_string(), a.to_string()))
            .collect()
    }

    #[test]
    fn a_stop_needs_no_sign_in_and_a_reply_asks_the_switch_first() {
        use super::super::linear_run::{First, first_step};
        assert!(needs_sign_in(DeliveryKind::Created));
        assert!(!needs_sign_in(DeliveryKind::Prompt));
        assert!(!needs_sign_in(DeliveryKind::Stop));
        // Not signed in to the workspace: what the first look decides.
        let first = |kind| first_step(false, false, true, !needs_sign_in(kind));
        assert_eq!(first(DeliveryKind::Stop), First::Read);
        assert_eq!(first(DeliveryKind::Created), First::RefuseNoSignIn);
        // A reply goes on to its own checks, which still refuse it.
        assert_eq!(first(DeliveryKind::Prompt), First::Read);
        let d = reply(Some("act-1"), Some("Push to main."));
        assert_eq!(reply_source(&d, true, false), Source::NoSignIn);
        assert_eq!(reply_source(&d, false, false), Source::Off);
    }

    #[test]
    fn a_run_of_another_organization_counts_as_none() {
        let run = Run {
            agent_session: "as-1".into(),
            organization: "w1".into(),
            issue: "i1".into(),
            identifier: "ENG-1".into(),
            project: PathBuf::from("/p"),
            environment: None,
            branch: String::new(),
            session: None,
            state: RunState::Running,
            reported_at: 0,
            asked: None,
            stopped: false,
        };
        assert_eq!(run_state(Some(&run), "w1"), Some(RunState::Running));
        assert_eq!(run_state(Some(&run), "w2"), None);
        assert_eq!(run_state(None, "w1"), None);
        assert_eq!(stop(run_state(Some(&run), "w2")), Stop::Nothing);
        assert_eq!(
            follow_up(run_state(Some(&run), "w2"), true, false, Some("go on")),
            FollowUp::NoRun
        );
    }

    #[test]
    fn a_stop_waits_for_work_in_flight_on_its_agent_session() {
        let in_flight = flights(&[("d1", "as-1")]);
        assert!(hold_stop(&in_flight, "as-1"));
        assert!(!hold_stop(&in_flight, "as-2"));
        assert!(!hold_stop(&HashMap::new(), "as-1"));
    }

    #[test]
    fn held_stops_go_once_the_work_has_landed() {
        // A delegation (d1) is read; two stops (s1, s2) for it wait, and one
        // for another agent session (s3) too.
        let mut in_flight = flights(&[
            ("d1", "as-1"),
            ("s1", "as-1"),
            ("s2", "as-1"),
            ("d2", "as-2"),
            ("s3", "as-2"),
        ]);
        let mut held = vec![
            delivery("s1", DeliveryKind::Stop, "as-1"),
            delivery("s3", DeliveryKind::Stop, "as-2"),
            delivery("s2", DeliveryKind::Stop, "as-1"),
        ];
        // d1 still in flight: nothing goes.
        assert!(release_stops(&mut held, &mut in_flight, "as-1").is_empty());
        assert_eq!(held.len(), 3);
        in_flight.remove("d1");
        let out = release_stops(&mut held, &mut in_flight, "as-1");
        let ids: Vec<&str> = out.iter().map(|d| d.id.as_str()).collect();
        assert_eq!(ids, ["s1", "s2"], "in the order they came");
        assert_eq!(
            held.iter().map(|d| d.id.as_str()).collect::<Vec<_>>(),
            ["s3"]
        );
        assert_eq!(in_flight, flights(&[("d2", "as-2"), ("s3", "as-2")]));
        // Released ones are no longer in flight, so they now run at once.
        assert!(!hold_stop(&in_flight, "as-1"));
        assert!(release_stops(&mut held, &mut in_flight, "as-1").is_empty());
    }

    #[test]
    fn the_lines_read_as_in_the_plan() {
        assert_eq!(STOPPED, "Stopped.");
        assert_eq!(
            no_session_line("rik-mac"),
            "rik-mac has no session for this any more. Delegate the issue again."
        );
        assert_eq!(
            unread_line("rik-mac"),
            "Arakun could not read that message, because rik-mac was offline when you sent \
             it. Send it again."
        );
        assert_eq!(
            not_started_line("rik-mac"),
            "This has not started yet on rik-mac. Your message will not be passed on; send it \
             again once it has started."
        );
    }
}
