//! A reply or a stop from Linear for a session it started. The server has
//! checked that it comes from the person who delegated. A reply's text is
//! data: it goes to the session as a message and never into a log.
//!
//! Like a new delivery, each one is recorded as handled in the same save as
//! its outcome, and accepted after that save.

use std::path::PathBuf;

use gpui::{AppContext as _, Context, Window};

use super::Shell;
use super::linear_run::computer;
use crate::linear::runs::{ReportKind, RunState};
use crate::linear::{Session, issues};
use crate::phone::linear::Delivery;

/// What a reply from Linear leads to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FollowUp {
    /// No run, or its session is gone here.
    NoRun,
    /// The run waits for a slot: there is no session to send it to yet.
    NotStarted,
    Unreadable,
    Send,
}

/// A run that started takes a reply, even one that ended: the person may
/// want more done.
pub fn follow_up(run: Option<RunState>, session_exists: bool, text: Option<&str>) -> FollowUp {
    match run {
        None => FollowUp::NoRun,
        Some(RunState::Waiting) => FollowUp::NotStarted,
        Some(_) if !session_exists => FollowUp::NoRun,
        Some(_) => match text {
            Some(t) if !t.trim().is_empty() => FollowUp::Send,
            _ => FollowUp::Unreadable,
        },
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
    /// never as the answer to a permission or a question form.
    pub(super) fn linear_prompt(
        &mut self,
        d: Delivery,
        sign_in: Session,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let decided = self.linear_follow_up(&d.agent_session, d.body.as_deref());
        // The relay keeps no text: a reply queued while this computer was
        // offline is read from Linear with the person's own key.
        if decided == FollowUp::Unreadable
            && d.body.is_none()
            && let Some(activity) = d.activity.clone()
        {
            self.linear_in_flight.insert(d.id.clone());
            cx.spawn_in(window, async move |this, cx| {
                // Any error, a panic too, means the text could not be read.
                let text = cx
                    .background_spawn(crate::guard::task_or(
                        async move { issues::prompt_text(&sign_in, &activity).ok() },
                        |_| None,
                    ))
                    .await;
                let _ = this.update_in(cx, |this, window, cx| {
                    this.linear_prompt_read(d, text, window, cx)
                });
            })
            .detach();
            return;
        }
        self.linear_answer(&d.id, &d.agent_session, decided, d.body, window, cx);
    }

    fn linear_prompt_read(
        &mut self,
        d: Delivery,
        text: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.linear_in_flight.remove(&d.id);
        // Signed out of Arakun meanwhile: not handled, so the relay sends it
        // again once there is an account to accept it with.
        if self.linear_gate(cx).is_none() {
            return;
        }
        // The run may have changed during the read.
        let decided = self.linear_follow_up(&d.agent_session, text.as_deref());
        self.linear_answer(&d.id, &d.agent_session, decided, text, window, cx);
    }

    fn linear_follow_up(&mut self, agent_session: &str, text: Option<&str>) -> FollowUp {
        let state = self
            .linear_runs
            .by_agent_session(agent_session)
            .map(|r| r.state);
        let exists = self.linear_session_of(agent_session).is_some();
        follow_up(state, exists, text)
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
        // A session is retired after its turn when no tab shows it.
        self.ensure_live(&env, &sid, window, cx);
        let Some(thread) = self.phone_thread(&env, &sid) else {
            return false;
        };
        thread.update(cx, |t, cx| t.send(text, cx));
        if let Some(s) = self.session_mut(&env, &sid) {
            s.updated = crate::model::unix_now();
        }
        // The person answered in Linear, or asked for more after it ended.
        if let Some(run) = self.linear_runs.by_agent_session(agent_session) {
            run.state = RunState::Running;
        }
        cx.notify();
        true
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
        let state = self
            .linear_runs
            .by_agent_session(&d.agent_session)
            .map(|r| r.state);
        let decided = stop(state);
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
            thread.update(cx, |t, cx| t.cancel(cx));
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
            for text in [None, Some(""), Some("go on")] {
                assert_eq!(follow_up(None, exists, text), FollowUp::NoRun);
            }
        }
    }

    #[test]
    fn a_reply_to_a_waiting_run_is_not_passed_on() {
        for exists in [false, true] {
            for text in [None, Some("go on")] {
                assert_eq!(
                    follow_up(Some(RunState::Waiting), exists, text),
                    FollowUp::NotStarted
                );
            }
        }
    }

    #[test]
    fn a_reply_whose_session_was_deleted_has_no_session() {
        for state in ALL.into_iter().filter(|s| *s != RunState::Waiting) {
            for text in [None, Some("go on")] {
                assert_eq!(follow_up(Some(state), false, text), FollowUp::NoRun);
            }
        }
    }

    #[test]
    fn a_reply_without_readable_text_is_unreadable() {
        for state in ALL.into_iter().filter(|s| *s != RunState::Waiting) {
            for text in [None, Some(""), Some("  \n\t ")] {
                assert_eq!(
                    follow_up(Some(state), true, text),
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
                follow_up(Some(state), true, Some("Use the other API.")),
                FollowUp::Send
            );
        }
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
