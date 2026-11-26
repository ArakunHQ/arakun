//! Telling a Linear issue what its delegated session does: a question, a
//! permission it waits for, the result, a failure, that it still works, and
//! that Arakun was closed while it ran.
//!
//! A session event only updates the run and queues a report; the outbox
//! sends it. Only the last reply, a question's text and a short name of what
//! a permission is for go to Linear: never a permission's arguments, which
//! can hold file contents, commands with secrets or whole diffs.

use std::collections::HashSet;
use std::time::Duration;

use gpui::{Context, Entity, Window};

use super::Shell;
use super::linear_run::computer;
use crate::agent::{Thread, ThreadEvent, ThreadStatus};
use crate::linear::runs::{ReportKind, RunState};
use crate::model::unix_now;

pub(super) use crate::linear::events::*;

/// How often a quiet session is looked at and the outbox tried again.
const TICK: Duration = Duration::from_secs(5 * 60);

/// The sessions `resume_after_restart` will bring back. Read, not taken:
/// that runs after the sweep.
fn resumed_after_restart() -> Vec<String> {
    std::fs::read(crate::state::resume_path())
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
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
        if self.linear_runs.by_session(id).is_none() {
            return;
        }
        let t = thread.read(cx);
        let ask = ask_of(t, &computer());
        let status = t.status();
        let now = Now {
            working: status == ThreadStatus::Working,
            failed: matches!(status, ThreadStatus::Error(_)),
            asking: ask.is_some(),
            continues: !t.queued().is_empty() && !t.queue_paused(),
        };
        let reply = matches!(event, Event::Finished(_)).then(|| turn_reply(t.entries()));
        let session_pr = t.meta().pr.clone();
        let Some(run) = self.linear_runs.by_session(id) else {
            return;
        };
        let key = ask.as_ref().map(|(k, _)| k.as_str());
        let Some(effect) = advance(run, event, now, key) else {
            return;
        };
        let to = run.state;
        let seen = match effect.step {
            Step::Ask => ask.map(|(_, seen)| seen),
            Step::Finished => Some(Seen::Finished {
                reply: reply.unwrap_or_default(),
            }),
            Step::Failed => Some(Seen::Failed(match status {
                ThreadStatus::Error(msg) => msg,
                _ => String::new(),
            })),
            Step::Cancelled => Some(Seen::Cancelled),
            Step::Working | Step::Nothing => None,
        };
        let report = seen.filter(|_| effect.report).and_then(to_report);
        let agent_session = run.agent_session.clone();
        let link = if effect.step == Step::Finished {
            self.linear_pr(id, session_pr.as_deref(), cx)
        } else {
            None
        };
        // The new state goes in the same save as the report.
        match report {
            Some((kind, body)) => self.linear_report(&agent_session, kind, body, link, cx),
            None => self.linear_save(),
        }
        if to == RunState::Running {
            self.linear_watch(cx);
        }
        if effect.frees {
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
        let sessions: Vec<&crate::model::Session> = self
            .projects
            .iter()
            .flat_map(|p| &p.environments)
            .flat_map(|e| &e.sessions)
            .collect();
        let listed: HashSet<String> = sessions.iter().map(|s| s.id.to_string()).collect();
        // `arakun restart` cut these off mid-turn; they go on by themselves
        // once the app is up, so they were not lost.
        let mut live: HashSet<String> = resumed_after_restart().into_iter().collect();
        live.extend(
            sessions
                .iter()
                .filter(|s| s.live.is_some())
                .map(|s| s.id.to_string()),
        );
        // A session removed while Arakun was closed: nothing to continue.
        let gone: Vec<String> = self
            .linear_runs
            .session_ids()
            .into_iter()
            .filter(|s| !listed.contains(s))
            .collect();
        let removed = self.linear_runs.forget_sessions(&gone);
        let before = self.linear_runs.clone();
        // A run with no session yet goes back to waiting, and starts below.
        let lost = self.linear_runs.interrupted(&|s| live.contains(s));
        self.linear_runs.clear_stopped();
        if !removed.is_empty() || self.linear_runs != before {
            self.linear_save();
        }
        let line = removed_line(&computer());
        for agent_session in removed {
            self.linear_report(&agent_session, ReportKind::Error, line.clone(), None, cx);
        }
        let line = closed_line(&computer());
        for agent_session in lost {
            self.linear_report(&agent_session, ReportKind::Error, line.clone(), None, cx);
        }
        self.linear_start_waiting(window, cx);
        self.linear_watch(cx);
    }

    /// Sessions taken off Arakun's list: their runs end, their issues hear
    /// it, and the freed slots go to the runs that wait.
    pub(super) fn linear_sessions_removed(
        &mut self,
        ids: &[String],
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if ids.is_empty() {
            return;
        }
        let before = self.linear_runs.clone();
        let told = self.linear_runs.forget_sessions(ids);
        if self.linear_runs == before {
            return;
        }
        self.linear_save();
        let line = removed_line(&computer());
        for agent_session in told {
            self.linear_report(&agent_session, ReportKind::Error, line.clone(), None, cx);
        }
        self.linear_start_waiting(window, cx);
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
        // With no account the line could not be sent: it would only pile up,
        // one per tick, and all arrive together later.
        let quiet = if self.linear_gate(cx).is_some() {
            self.linear_runs.quiet(now)
        } else {
            Vec::new()
        };
        for agent_session in quiet {
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
