//! Sending the reports of delegated sessions to Linear, through Arakun's
//! server. They are saved first and sent in order, so a report never blocks
//! or fails a session; one not sent stays for the next pass. Order holds
//! within an agent session: one whose report was not taken waits as a whole,
//! and the other sessions' reports go on.

use super::runs::{Report, ReportKind};
use crate::cloud::{self, LinearActivity, LinearLinkOut};

fn activity(kind: ReportKind) -> LinearActivity {
    match kind {
        ReportKind::Thought => LinearActivity::Thought,
        ReportKind::Elicitation => LinearActivity::Elicitation,
        ReportKind::Response => LinearActivity::Response,
        ReportKind::Error => LinearActivity::Error,
        ReportKind::Progress => LinearActivity::Progress,
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

/// The server answered, but could not pass this one report on (Linear
/// refused it upstream, or the server broke on it). The server itself is
/// there, so reports of other agent sessions can still go. A 503 is not:
/// then the server is down for everyone.
fn refused_for_now(status: u16) -> bool {
    (500..600).contains(&status) && status != 503
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
/// limit, no network or a server that is down (503) stops it and keeps the
/// rest for later: a hiccup must not lose a report. Any other server error
/// keeps that report and the later ones of its agent session, in order, and
/// goes on with the other sessions: one report the server chokes on must not
/// hold up everyone's for a day. Only a refusal (a 4xx other than 429) drops
/// that one report.
pub fn flush(base: &str, session: &str, reports: Vec<Report>) -> Flushed {
    let mut out = Flushed::default();
    // Agent sessions whose reports wait for the next pass.
    let mut waiting: Vec<String> = Vec::new();
    for r in reports {
        if !sendable(&r.agent_session) {
            out.dropped.push(r);
            continue;
        }
        if waiting.contains(&r.agent_session) {
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
            Err(cloud::Error::Status { status, .. }) if refused_for_now(status) => {
                waiting.push(r.agent_session)
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

#[cfg(test)]
mod tests {
    use super::*;

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
    fn a_server_error_keeps_the_rest_of_its_agent_session() {
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

    fn report_for(agent_session: &str, body: &str) -> Report {
        Report {
            agent_session: agent_session.into(),
            ..report(body)
        }
    }

    #[test]
    fn one_report_the_server_cannot_pass_on_does_not_hold_up_other_sessions() {
        for status in [500, 502, 504] {
            let (base, rx) = serve(vec![
                (status, r#"{"error": "upstream"}"#),
                (204, ""),
                (204, ""),
            ]);
            let out = flush(
                &base,
                "s",
                vec![
                    report_for("as-a", "a1"),
                    report_for("as-a", "a2"),
                    report_for("as-b", "b1"),
                    report_for("as-a", "a3"),
                    report_for("as-b", "b2"),
                ],
            );
            assert_eq!(bodies(&out.sent), ["b1", "b2"], "{status}");
            assert!(out.dropped.is_empty() && !out.signed_out, "{status}");
            let got: Vec<String> = rx.try_iter().collect();
            // a2 and a3 were not tried: they must not pass a1.
            assert_eq!(got.len(), 3, "{status}");
            assert!(got[0].starts_with("POST /linear/sessions/as-a/activity "));
            assert!(got[1].starts_with("POST /linear/sessions/as-b/activity "));
            assert!(got[2].starts_with("POST /linear/sessions/as-b/activity "));
        }
    }

    #[test]
    fn what_a_pass_did_not_send_stays_in_its_order() {
        use crate::linear::runs::Runs;
        let (base, _rx) = serve(vec![(502, r#"{"error": "upstream"}"#), (204, "")]);
        let mut runs = Runs::default();
        for (a, body) in [
            ("as-a", "a1"),
            ("as-a", "a2"),
            ("as-b", "b1"),
            ("as-a", "a3"),
        ] {
            runs.queue(report_for(a, body));
        }
        let out = flush(&base, "s", runs.due(1));
        for r in &out.sent {
            runs.sent(r);
        }
        assert_eq!(bodies(&runs.due(1)), ["a1", "a2", "a3"]);
    }

    #[test]
    fn a_server_that_is_down_stops_the_pass_for_everyone() {
        let (base, rx) = serve(vec![(503, r#"{"error": "down"}"#), (204, "")]);
        let out = flush(
            &base,
            "s",
            vec![report_for("as-a", "a1"), report_for("as-b", "b1")],
        );
        assert!(out.sent.is_empty() && out.dropped.is_empty() && !out.signed_out);
        assert_eq!(rx.try_iter().count(), 1);
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

    use std::io::{Read as _, Write as _};

    /// A server of our own that answers each request with the next of
    /// `answers`, then stops listening. Hands back every request it got.
    pub(crate) fn serve(
        answers: Vec<(u16, &'static str)>,
    ) -> (String, std::sync::mpsc::Receiver<String>) {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            for (status, body) in answers {
                let (mut s, _) = listener.accept().unwrap();
                let mut got = Vec::new();
                let mut buf = [0u8; 4096];
                let head_end = loop {
                    let n = s.read(&mut buf).unwrap();
                    got.extend_from_slice(&buf[..n]);
                    if let Some(i) = got.windows(4).position(|w| w == b"\r\n\r\n") {
                        break i + 4;
                    }
                };
                let head = String::from_utf8_lossy(&got[..head_end]).to_lowercase();
                let want: usize = head
                    .lines()
                    .find_map(|l| l.strip_prefix("content-length:"))
                    .map_or(0, |v| v.trim().parse().unwrap());
                while got.len() < head_end + want {
                    let n = s.read(&mut buf).unwrap();
                    got.extend_from_slice(&buf[..n]);
                }
                tx.send(String::from_utf8_lossy(&got).to_string()).unwrap();
                write!(
                    s,
                    "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                )
                .unwrap();
            }
        });
        (base, rx)
    }
}
