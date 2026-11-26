//! The reports a delegated session sends to Linear. They are saved first and
//! sent in order off the UI thread, so a report never blocks or fails a
//! session; one not sent stays for the next pass.

use gpui::{AppContext as _, Context};

use super::Shell;
use super::linear_sync::Gate;
use crate::cloud::{self, LinearActivity, LinearLinkOut};
use crate::linear::runs::{Report, ReportKind};
use crate::model::unix_now;

fn activity(kind: ReportKind) -> LinearActivity {
    match kind {
        ReportKind::Thought => LinearActivity::Thought,
        ReportKind::Elicitation => LinearActivity::Elicitation,
        ReportKind::Response => LinearActivity::Response,
        ReportKind::Error => LinearActivity::Error,
    }
}

/// An id the server takes in a path. Any other would be refused before it is
/// sent, every time, and hold up the reports behind it for a day.
fn sendable(agent_session: &str) -> bool {
    (1..=64).contains(&agent_session.len())
        && agent_session
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-')
}

/// Linear or the server will never take this report.
fn refused_for_good(status: u16) -> bool {
    (400..500).contains(&status) && status != 429
}

/// What one pass over the outbox did.
#[derive(Debug, Default)]
pub struct Flushed {
    pub sent: Vec<Report>,
    /// Linear refused these for good.
    pub dropped: Vec<Report>,
    pub signed_out: bool,
}

/// Sends `reports` in order. It blocks, so it runs off the UI thread. A
/// limit, no network or a server error (5xx) stops it and keeps the rest for
/// later: a hiccup must not lose the report. Only a refusal (a 4xx other than
/// 429) drops that one report.
pub fn flush(base: &str, session: &str, reports: Vec<Report>) -> Flushed {
    let mut out = Flushed::default();
    for r in reports {
        if !sendable(&r.agent_session) {
            out.dropped.push(r);
            continue;
        }
        let links: Vec<LinearLinkOut> = r
            .link
            .iter()
            .map(|url| LinearLinkOut {
                label: "Pull request".into(),
                url: url.clone(),
            })
            .collect();
        let kind = activity(r.kind);
        match cloud::linear_activity(base, session, &r.agent_session, kind, &r.body, &links) {
            Ok(()) => out.sent.push(r),
            Err(cloud::Error::Status { status, .. }) if refused_for_good(status) => {
                out.dropped.push(r)
            }
            Err(cloud::Error::SignedOut) => {
                out.signed_out = true;
                break;
            }
            Err(_) => break,
        }
    }
    out
}

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
        let now = unix_now();
        self.linear_runs.queue(Report {
            agent_session: agent_session.to_string(),
            kind,
            body,
            link,
            made_at: now,
        });
        if let Some(run) = self.linear_runs.by_agent_session(agent_session) {
            run.reported_at = now;
        }
        self.linear_save();
        self.linear_flush(cx);
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
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shell::linear_sync::tests::serve;

    fn report(body: &str) -> Report {
        Report {
            agent_session: "as-1".into(),
            kind: ReportKind::Thought,
            body: body.into(),
            link: None,
            made_at: 1,
        }
    }

    fn bodies(rs: &[Report]) -> Vec<&str> {
        rs.iter().map(|r| r.body.as_str()).collect()
    }

    #[test]
    fn the_outbox_goes_in_order() {
        let (base, rx) = serve(vec![(204, ""), (204, ""), (204, "")]);
        let mut last = report("three");
        last.kind = ReportKind::Response;
        last.link = Some("https://github.com/a/b/pull/1".into());
        let out = flush(&base, "s", vec![report("one"), report("two"), last]);
        assert_eq!(bodies(&out.sent), ["one", "two", "three"]);
        assert!(out.dropped.is_empty() && !out.signed_out);
        let got: Vec<String> = rx.try_iter().collect();
        assert_eq!(got.len(), 3);
        assert!(got[0].starts_with("POST /linear/sessions/as-1/activity "));
        assert!(got[0].contains(r#""body":"one""#) && got[1].contains(r#""body":"two""#));
        assert!(got[2].contains(r#""type":"response""#), "{}", got[2]);
        assert!(
            got[2].contains(r#"{"label":"Pull request","url":"https://github.com/a/b/pull/1"}"#),
            "{}",
            got[2]
        );
    }

    #[test]
    fn a_limit_stops_the_outbox_and_keeps_the_rest() {
        let (base, rx) = serve(vec![(204, ""), (429, r#"{"error": "slow down"}"#)]);
        let out = flush(
            &base,
            "s",
            vec![report("one"), report("two"), report("three")],
        );
        assert_eq!(bodies(&out.sent), ["one"]);
        assert!(out.dropped.is_empty() && !out.signed_out);
        assert_eq!(rx.try_iter().count(), 2);
    }

    #[test]
    fn a_server_error_stops_the_outbox_and_keeps_the_rest() {
        for status in [500, 502, 503] {
            let (base, rx) = serve(vec![(204, ""), (status, r#"{"error": "upstream"}"#)]);
            let out = flush(
                &base,
                "s",
                vec![report("one"), report("two"), report("three")],
            );
            assert_eq!(bodies(&out.sent), ["one"], "{status}");
            assert!(out.dropped.is_empty() && !out.signed_out, "{status}");
            assert_eq!(rx.try_iter().count(), 2, "{status}");
        }
    }

    #[test]
    fn a_refused_report_is_dropped_and_the_next_goes() {
        let (base, rx) = serve(vec![
            (204, ""),
            (404, r#"{"error": "no such session"}"#),
            (204, ""),
        ]);
        let out = flush(
            &base,
            "s",
            vec![report("one"), report("two"), report("three")],
        );
        assert_eq!(bodies(&out.sent), ["one", "three"]);
        assert_eq!(bodies(&out.dropped), ["two"]);
        assert_eq!(rx.try_iter().count(), 3);
    }

    #[test]
    fn signed_out_nothing_more_is_sent() {
        let (base, rx) = serve(vec![(401, r#"{"error": "not signed in"}"#), (204, "")]);
        let out = flush(&base, "s", vec![report("one"), report("two")]);
        assert!(out.signed_out && out.sent.is_empty() && out.dropped.is_empty());
        assert_eq!(rx.try_iter().count(), 1);
    }

    #[test]
    fn offline_the_outbox_keeps_everything() {
        // A port nobody listens on.
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        drop(listener);
        let out = flush(&base, "s", vec![report("one"), report("two")]);
        assert!(out.sent.is_empty() && out.dropped.is_empty() && !out.signed_out);
    }

    #[test]
    fn a_report_for_an_id_the_server_cannot_take_is_dropped_unsent() {
        let (base, rx) = serve(vec![(204, "")]);
        let mut bad = report("bad");
        bad.agent_session = "as/../x".into();
        let out = flush(&base, "s", vec![bad, report("good")]);
        assert_eq!(bodies(&out.dropped), ["bad"]);
        assert_eq!(bodies(&out.sent), ["good"]);
        assert_eq!(rx.try_iter().count(), 1);
    }
}
