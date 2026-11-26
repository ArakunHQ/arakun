//! A reply or a stop from Linear for a session it started. The relay only
//! says that a reply exists: its text is read from Linear with the person's
//! own key, never taken from the relay, and only while the switch is on. The
//! text is data: it goes to the session as a message and never into a log.
//!
//! Like a new delivery, each one is recorded as handled in the same save as
//! its outcome, and accepted after that save.

use std::path::PathBuf;

use gpui::{AppContext as _, Context, Window};

use super::Shell;
use super::linear_run::{busy_line, computer};
use crate::linear::delegation::Delegation;
use crate::linear::runs::{ReportKind, RunState};
use crate::linear::{Session, issues};
use crate::phone::linear::Delivery;

pub(super) use crate::linear::follow::*;

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
                return self.linear_turn_away(&d, reply_off_line(&computer()), cx);
            }
            Source::NoSignIn => {
                self.linear_turn_away(&d, reply_no_sign_in_line(&computer()), cx);
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
            self.linear_turn_away(&d, reply_off_line(&computer()), cx);
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
            FollowUp::NoDirections => Some((ReportKind::Thought, no_directions_line(&computer()))),
            FollowUp::Send | FollowUp::Interrupt => {
                let interrupt = decided == FollowUp::Interrupt;
                let text = message(text.as_deref().unwrap_or_default()).to_string();
                match self.linear_pass_on(agent_session, text, interrupt, window, cx) {
                    Some((_, true)) if interrupt => {
                        Some((ReportKind::Thought, interrupted_line(&computer())))
                    }
                    Some((pending, busy)) => {
                        Some((ReportKind::Thought, passed_line(&computer(), pending, busy)))
                    }
                    None => Some((ReportKind::Error, no_session_line(&computer()))),
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
    /// busy queues it. No tab opens and focus stays where it is. Gives what
    /// the message waits behind, or None when there was no session to take it.
    fn linear_pass_on(
        &mut self,
        agent_session: &str,
        text: String,
        interrupt: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<(Pending, bool)> {
        let (env, sid) = self.linear_session_of(agent_session)?;
        // Another process runs it: the app only shows it, and a message to
        // that view would be dropped.
        if crate::headless::shown(&sid) {
            return None;
        }
        // A session is retired after its turn when no tab shows it.
        self.ensure_live(&env, &sid, window, cx);
        let thread = self.phone_thread(&env, &sid)?;
        let waits = thread.update(cx, |t, cx| {
            // A question that waits is answered by this reply: it is skipped,
            // and the reply goes to the agent as the next message.
            for _ in 0..SKIPS {
                if !skips_question(pending_in(t)) {
                    break;
                }
                t.skip_question(cx);
            }
            // What the message waits behind, before it starts a turn itself.
            let waits = (pending_in(t), t.is_busy());
            if interrupt {
                t.interrupt_with(text, cx);
            } else {
                t.send(text, cx);
            }
            // The cancelled turn still winds down: its end is not the run's.
            (
                waits,
                interrupt && waits.1 && t.is_busy() && t.is_cancelling(),
            )
        });
        let (waits, winding_down) = waits;
        if let Some(s) = self.session_mut(&env, &sid) {
            s.updated = crate::model::unix_now();
        }
        // The person asked for more after it ended. An explicit reply is
        // not queued or refused, so a run that ended can pass the limit of
        // three here; new work then waits until the running sessions drop
        // below it.
        let mut revived = false;
        if let Some(run) = self.linear_runs.by_agent_session(agent_session) {
            run.stopped = stopped_after_reply(run.stopped, run.state, winding_down);
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
        Some(waits)
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
