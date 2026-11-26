//! Opens a ticket in Arakun's PostHog Support inbox for a support question,
//! so the team can answer it, by email when there is one. The rest of the
//! feedback goes to Linear instead (`linear`).
//!
//! It uses the public widget endpoint, the one posthog-js's support widget
//! uses. It only runs after `send::submit` passed the feedback consent check,
//! so it follows the same rules as the `survey sent` event, and it holds no
//! more than that event does.

use std::time::Duration;

use serde_json::{Map, Value, json};

use super::model::Survey;
use super::report::{self, Answers};

/// The Support widget's public token: it can only open tickets in the
/// widget's name, so it is safe in the binary, like `telemetry::KEY`. A build
/// can point at another project with `ARAKUN_SUPPORT_TOKEN` (empty turns it off).
const TOKEN: &str = match option_env!("ARAKUN_SUPPORT_TOKEN") {
    Some(t) => t,
    None => "gLqqXWN1Xsy-2WH195ZoU14jiFqa1-VmhPPE-dTials",
};

/// Opens the ticket on a background thread. Best effort: a failure is
/// dropped, as the `survey sent` event still carries the response.
pub fn open(s: &Survey, props: &Map<String, Value>) {
    if TOKEN.is_empty() || crate::telemetry::KEY.is_empty() {
        return;
    }
    let body = payload(
        s,
        props,
        &crate::telemetry::install_id(),
        &crate::telemetry::uuid_v4(),
    )
    .to_string();
    let _ = std::thread::Builder::new()
        .name("support-ticket".into())
        .spawn(move || {
            let url = format!(
                "{}/api/conversations/v1/widget/message",
                crate::telemetry::HOST.trim_end_matches('/')
            );
            let request =
                crate::http::request("POST", &url, None).header("X-Conversations-Token", TOKEN);
            let agent = crate::http::agent(Duration::from_secs(10));
            let _ = crate::http::run(agent, request, Some(body), 64 * 1024);
        });
}

/// The widget request: a new ticket (`ticket_id` null) with the response as
/// its first message. `session` is a fresh id, since the app never reads the
/// ticket back. The inbox is private, so the answers go as typed.
fn payload(s: &Survey, props: &Map<String, Value>, install: &str, session: &str) -> Value {
    json!({
        "message": report::message(s, props, Answers::AsTyped),
        "traits": {"name": report::sender(props), "email": report::email_answer(s, props)},
        "ticket_id": null,
        "widget_session_id": session,
        "distinct_id": install,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::surveys::ids;
    use crate::surveys::model::Answer;
    use crate::surveys::send::response_props;
    use crate::surveys::store;
    use crate::telemetry::Source;

    #[test]
    fn ticket_has_the_question_and_email_only_as_contact() {
        let s = store::fallback(&ids::USER);
        let answers = vec![
            (0, Answer::Choice("Help / support".into())),
            (1, Answer::Text("How do I sync settings?".into())),
            (2, Answer::Text("ann@example.com".into())),
        ];
        let p = response_props(&s, &answers, Source::User, &[]);
        let v = payload(&s, &p, "install", "session");
        let msg = v["message"].as_str().unwrap();
        assert!(
            msg.starts_with("Support · user feedback · Arakun "),
            "{msg}"
        );
        assert!(msg.contains("How do I sync settings?"));
        assert!(!msg.contains("ann@example.com"));
        assert_eq!(v["traits"]["email"], "ann@example.com");
        assert_eq!(v["traits"]["name"], "Arakun user");
        assert_eq!(v["ticket_id"], Value::Null);
        assert_eq!(v["distinct_id"], "install");
        assert_eq!(v["widget_session_id"], "session");
    }
}
