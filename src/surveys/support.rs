//! Opens a ticket in Arakun's PostHog Support inbox for each sent survey
//! response, so the team can triage it and reply when there is an email.
//! GitHub issues are made from there by a person, never by the app.
//!
//! It uses the public widget endpoint, the one posthog-js's support widget
//! uses. It only runs after `send::submit` passed the feedback consent check,
//! so it follows the same rules as the `survey sent` event, and it holds no
//! more than that event does.

use std::time::Duration;

use serde_json::{Map, Value, json};

use super::ids;
use super::model::Survey;

/// The Support widget's public token: it can only open tickets in the
/// widget's name, so it is safe in the binary, like `telemetry::KEY`. A build
/// can point at another project with `ARAKUN_SUPPORT_TOKEN` (empty turns it off).
const TOKEN: &str = match option_env!("ARAKUN_SUPPORT_TOKEN") {
    Some(t) => t,
    None => "gLqqXWN1Xsy-2WH195ZoU14jiFqa1-VmhPPE-dTials",
};

/// Longest message sent. Agent text is already capped lower.
const MAX_MESSAGE: usize = 16 * 1024;

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
/// ticket back.
fn payload(s: &Survey, props: &Map<String, Value>, install: &str, session: &str) -> Value {
    let email = email_answer(s, props);
    json!({
        "message": message(s, props),
        "traits": {"name": sender(props), "email": email},
        "ticket_id": null,
        "widget_session_id": session,
        "distinct_id": install,
    })
}

/// Who the ticket is from, as the inbox shows it.
fn sender(props: &Map<String, Value>) -> String {
    match str_prop(props, "source") {
        Some("agent") => match str_prop(props, "agent") {
            Some(a) if !a.is_empty() => format!("{a} (agent in Arakun)"),
            _ => "An agent in Arakun".into(),
        },
        _ => "Arakun user".into(),
    }
}

/// The answer to the user survey's optional email question. It goes in the
/// ticket's contact field only, never in the message.
fn email_answer(s: &Survey, props: &Map<String, Value>) -> Option<String> {
    if s.id != ids::USER.survey_id {
        return None;
    }
    let key = format!("$survey_response_{}", ids::USER.questions[2].id);
    str_prop(props, &key)
        .map(str::trim)
        .filter(|e| e.contains('@'))
        .map(str::to_string)
}

fn message(s: &Survey, props: &Map<String, Value>) -> String {
    let mut head = Vec::new();
    if let Some(kind) = str_prop(props, "kind") {
        head.push(format!("**{kind}**"));
    }
    head.push(match str_prop(props, "source") {
        Some("popover") => format!("survey \"{}\"", s.name),
        Some(src) => format!("{src} feedback"),
        None => "feedback".into(),
    });
    head.push(format!("Arakun {}", env!("ARAKUN_VERSION")));
    head.push(std::env::consts::OS.to_string());
    let mut out = head.join(" · ");

    let email_q = (s.id == ids::USER.survey_id).then(|| ids::USER.questions[2].id);
    for q in &s.questions {
        if Some(q.id.as_str()) == email_q {
            continue;
        }
        let Some(answer) = props
            .get(&format!("$survey_response_{}", q.id))
            .and_then(answer_text)
        else {
            continue;
        };
        out.push_str(&format!("\n\n**{}**\n{answer}", q.text));
    }
    if let Some(e) = str_prop(props, "error_event") {
        out.push_str(&format!("\n\nError event: `{e}`"));
    }
    if out.len() > MAX_MESSAGE {
        let mut end = MAX_MESSAGE;
        while !out.is_char_boundary(end) {
            end -= 1;
        }
        out.truncate(end);
        out.push('…');
    }
    out
}

fn answer_text(v: &Value) -> Option<String> {
    match v {
        Value::String(s) if !s.trim().is_empty() => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        Value::Array(a) => {
            let parts: Vec<&str> = a.iter().filter_map(Value::as_str).collect();
            (!parts.is_empty()).then(|| parts.join(", "))
        }
        _ => None,
    }
}

fn str_prop<'a>(props: &'a Map<String, Value>, key: &str) -> Option<&'a str> {
    props.get(key).and_then(Value::as_str)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::surveys::model::Answer;
    use crate::surveys::send::response_props;
    use crate::surveys::store;
    use crate::telemetry::Source;

    fn user_props(email: &str) -> (Survey, Map<String, Value>) {
        let s = store::fallback(&ids::USER);
        let answers = vec![
            (0, Answer::Choice("Bug".into())),
            (1, Answer::Text("The tab name resets.".into())),
            (2, Answer::Text(email.into())),
        ];
        let p = response_props(&s, &answers, Source::User, &[]);
        (s, p)
    }

    #[test]
    fn user_ticket_has_answers_and_email_only_as_contact() {
        let (s, p) = user_props("ann@example.com");
        let v = payload(&s, &p, "install", "session");
        let msg = v["message"].as_str().unwrap();
        assert!(msg.starts_with("**bug** · user feedback · Arakun "));
        assert!(msg.contains("**What's on your mind?**\nThe tab name resets."));
        assert!(!msg.contains("ann@example.com"));
        assert_eq!(v["traits"]["email"], "ann@example.com");
        assert_eq!(v["traits"]["name"], "Arakun user");
        assert_eq!(v["ticket_id"], Value::Null);
        assert_eq!(v["distinct_id"], "install");
        assert_eq!(v["widget_session_id"], "session");
    }

    #[test]
    fn text_that_is_not_an_email_is_not_a_contact() {
        let (s, p) = user_props("later");
        assert_eq!(payload(&s, &p, "i", "s")["traits"]["email"], Value::Null);
    }

    #[test]
    fn agent_ticket_names_the_agent() {
        let s = store::fallback(&ids::AGENT);
        let answers = vec![
            (0, Answer::Choice("UX".into())),
            (1, Answer::Text("Hard to find the diff.".into())),
        ];
        let p = response_props(&s, &answers, Source::Agent, &[("agent", "claude".into())]);
        let v = payload(&s, &p, "i", "s");
        assert_eq!(v["traits"]["name"], "claude (agent in Arakun)");
        assert!(
            v["message"]
                .as_str()
                .unwrap()
                .contains("Hard to find the diff.")
        );
    }

    #[test]
    fn error_ticket_links_the_error_and_rating() {
        let s = store::fallback(&ids::ERROR);
        let answers = vec![
            (0, Answer::Text("Opening a tab".into())),
            (1, Answer::Rating(4)),
        ];
        let p = response_props(
            &s,
            &answers,
            Source::Error,
            &[("error_event", "abc".into())],
        );
        let msg = payload(&s, &p, "i", "s")["message"]
            .as_str()
            .unwrap()
            .to_string();
        assert!(msg.starts_with("**bug** · error feedback"));
        assert!(msg.contains("**How bad was it?**\n4"));
        assert!(msg.ends_with("Error event: `abc`"));
    }

    #[test]
    fn long_message_is_cut_on_a_char_boundary() {
        let (s, mut p) = user_props("");
        let key = format!("$survey_response_{}", ids::USER.questions[1].id);
        p.insert(key, "é".repeat(MAX_MESSAGE).into());
        let msg = message(&s, &p);
        assert!(msg.len() <= MAX_MESSAGE + '…'.len_utf8());
        assert!(msg.ends_with('…'));
    }
}
