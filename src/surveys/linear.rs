//! Files a sent response (a bug report, feature request, UX note or other
//! feedback) as a customer request in Arakun's Linear, through the cloud's
//! `POST /feedback`. The cloud holds the Linear key; the app never does.
//!
//! Linear syncs its issues to the public GitHub repository, so the issue's
//! title and text are the scrubbed copies. The text as typed goes in the
//! customer request, which stays in Linear, and the email only names the
//! customer. It only runs after `send::submit` passed the feedback consent
//! check, like `support`.

use std::time::Duration;

use serde_json::{Map, Value, json};

use super::model::Survey;
use super::report::{self, Answers};

/// Sends it on a background thread. Best effort: a failure is dropped, as
/// the `survey sent` event still carries the response.
pub fn open(s: &Survey, props: &Map<String, Value>) {
    let Some(body) = payload(s, props) else {
        return;
    };
    let Ok(base) = crate::cloud::base_url() else {
        return;
    };
    let _ = std::thread::Builder::new()
        .name("feedback-linear".into())
        .spawn(move || {
            let url = format!("{base}/feedback");
            let agent = crate::http::agent(Duration::from_secs(10));
            let _ =
                crate::http::json_call(agent, "POST", &url, None, Some(body.to_string()), 4 * 1024);
        });
}

/// What the cloud's `/feedback` takes, or None for a response it does not:
/// one with no kind, or a support question.
fn payload(s: &Survey, props: &Map<String, Value>) -> Option<Value> {
    let kind = report::kind(props).filter(|k| *k != "support")?;
    Some(json!({
        "kind": kind,
        "title": report::title(s, props),
        "description": report::message(s, props, Answers::Scrubbed),
        "request": report::message(s, props, Answers::AsTyped),
        "from": report::sender(props),
        "email": report::email_answer(s, props),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::surveys::ids;
    use crate::surveys::model::Answer;
    use crate::surveys::send::response_props;
    use crate::surveys::store;
    use crate::telemetry::Source;

    fn user(kind: &str, text: &str, email: &str) -> Option<Value> {
        let s = store::fallback(&ids::USER);
        let answers = vec![
            (0, Answer::Choice(kind.into())),
            (1, Answer::Text(text.into())),
            (2, Answer::Text(email.into())),
        ];
        payload(&s, &response_props(&s, &answers, Source::User, &[]))
    }

    #[test]
    fn issue_text_is_scrubbed_and_the_request_is_as_typed() {
        let v = user(
            "Feature request",
            "Open links like https://x.io/a?t=1 in a tab",
            "ann@example.com",
        )
        .unwrap();
        assert_eq!(v["kind"], "feature");
        assert_eq!(v["email"], "ann@example.com");
        assert_eq!(v["from"], "Arakun user");
        for public in ["title", "description"] {
            let t = v[public].as_str().unwrap();
            assert!(!t.contains("x.io"), "{public}: {t}");
            assert!(!t.contains("ann@example.com"), "{public}: {t}");
        }
        let request = v["request"].as_str().unwrap();
        assert!(request.contains("https://x.io/a?t=1"));
        assert!(!request.contains("ann@example.com"));
    }

    #[test]
    fn support_questions_stay_out_of_linear() {
        assert!(user("Help / support", "How do I?", "").is_none());
        assert!(user("Bug", "It broke", "").is_some());
    }

    #[test]
    fn error_feedback_is_a_bug() {
        let s = store::fallback(&ids::ERROR);
        let answers = vec![(0, Answer::Text("Opening a tab".into()))];
        let p = response_props(&s, &answers, Source::Error, &[]);
        let v = payload(&s, &p).unwrap();
        assert_eq!(v["kind"], "bug");
        assert_eq!(v["title"], "Opening a tab");
        assert_eq!(v["email"], Value::Null);
    }

    #[test]
    fn a_survey_without_a_kind_is_not_sent() {
        let s = store::fallback(&ids::USER);
        let answers = vec![(1, Answer::Text("hi".into()))];
        let p = response_props(&s, &answers, Source::Popover, &[]);
        assert!(payload(&s, &p).is_none());
    }
}
