//! The reports a delegated session sends to Linear. They are saved first and
//! sent in order off the UI thread, so a report never blocks or fails a
//! session; one not sent stays for the next pass. Order holds within an
//! agent session: one whose report was not taken waits as a whole, and the
//! other sessions' reports go on.

use gpui::{AppContext as _, Context};

use super::Shell;
use super::linear_sync::Gate;
use crate::cloud;
use crate::linear::outbox::{Flushed, flush};
use crate::linear::runs::{Report, ReportKind};
use crate::model::unix_now;

/// The outbox's sender: one pass at a time.
#[derive(Default)]
pub(super) struct Outbox {
    flushing: bool,
    /// A report came in during a pass: one more pass after it.
    owed: bool,
}

impl Shell {
    /// Queues a report to Linear and sends the outbox. A report never waits
    /// on the network, and one not sent stays for the next pass.
    pub(super) fn linear_report(
        &mut self,
        agent_session: &str,
        kind: ReportKind,
        body: String,
        link: Option<String>,
        cx: &mut Context<Self>,
    ) {
        self.linear_report_with(agent_session, kind, body, link, Vec::new(), cx);
    }

    /// [`Self::linear_report`] for a question with `options`: its choices,
    /// which Linear shows to pick from.
    pub(super) fn linear_report_with(
        &mut self,
        agent_session: &str,
        kind: ReportKind,
        body: String,
        link: Option<String>,
        options: Vec<String>,
        cx: &mut Context<Self>,
    ) {
        self.linear_queue(Report {
            agent_session: agent_session.to_string(),
            kind,
            body,
            link,
            options,
            started: false,
            made_at: unix_now(),
        });
        self.linear_flush(cx);
    }

    /// The thought that says the session started: with it, Arakun's server
    /// marks the issue as being worked on in Linear.
    pub(super) fn linear_report_started(
        &mut self,
        agent_session: &str,
        body: String,
        cx: &mut Context<Self>,
    ) {
        self.linear_queue(Report {
            agent_session: agent_session.to_string(),
            kind: ReportKind::Thought,
            body,
            link: None,
            options: Vec::new(),
            started: true,
            made_at: unix_now(),
        });
        self.linear_flush(cx);
    }

    fn linear_queue(&mut self, r: Report) {
        let (kind, now) = (r.kind, r.made_at);
        let agent_session = r.agent_session.clone();
        self.linear_runs.queue(r);
        // A link is no activity: Linear still needs to hear the run works.
        if kind != ReportKind::Link
            && let Some(run) = self.linear_runs.by_agent_session(&agent_session)
        {
            run.reported_at = now;
        }
        self.linear_save();
    }

    /// Sends what the outbox holds, in order, off the UI thread.
    pub(super) fn linear_flush(&mut self, cx: &mut Context<Self>) {
        if self.linear_outbox.flushing {
            self.linear_outbox.owed = true;
            return;
        }
        let Some(Gate { base, session, .. }) = self.linear_gate(cx) else {
            return;
        };
        let due = self.linear_runs.due(unix_now());
        if due.is_empty() {
            return;
        }
        self.linear_outbox.flushing = true;
        self.linear_outbox.owed = false;
        cx.spawn(async move |this, cx| {
            let out = cx
                .background_spawn(crate::guard::task_or(
                    async move { flush(&base, &session, due) },
                    |_| Flushed::default(),
                ))
                .await;
            let _ = this.update(cx, |this, cx| this.linear_flushed(out, cx));
        })
        .detach();
    }

    fn linear_flushed(&mut self, out: Flushed, cx: &mut Context<Self>) {
        self.linear_outbox.flushing = false;
        for r in &out.sent {
            self.linear_runs.sent(r);
        }
        for r in &out.dropped {
            self.linear_runs.dropped(r);
        }
        if !out.sent.is_empty() || !out.dropped.is_empty() {
            self.linear_save();
        }
        if out.signed_out {
            self.account_call_failed(cloud::Error::SignedOut, cx);
            return;
        }
        if std::mem::take(&mut self.linear_outbox.owed) {
            self.linear_flush(cx);
        }
        // What could not go is tried again by the timer.
        self.linear_watch(cx);
    }
}
