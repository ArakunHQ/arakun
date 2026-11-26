//! A sent survey response as text for a person to read: the PostHog Support
//! ticket (`support`) and the Linear customer request (`linear`) both use it.
//! It is plain text with line breaks and labels, never Markdown, since the
//! Support inbox shows it as it is.

use serde_json::{Map, Value};

use super::ids;
use super::model::Survey;

/// Longest message sent. Agent text is already capped lower.
pub const MAX_MESSAGE: usize = 16 * 1024;
/// Longest title, in characters.
const MAX_TITLE: usize = 80;

/// Which copy of the free-text answers goes in.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Answers {
    /// As typed: for places only the team sees.
    AsTyped,
    /// The `_scrubbed` copies, with no paths, links, keys or emails: for
    /// places that may become public.
    Scrubbed,
}

/// Who it is from, as the team sees it.
pub fn sender(props: &Map<String, Value>) -> String {
    match str_prop(props, "source") {
        Some("agent") => match str_prop(props, "agent") {
            Some(a) if !a.is_empty() => format!("{a} (agent in Arakun)"),
            _ => "An agent in Arakun".into(),
        },
        _ => "Arakun user".into(),
    }
}

/// The answer to the user survey's optional email question. It only ever
/// names who to reply to, never goes in the message.
pub fn email_answer(s: &Survey, props: &Map<String, Value>) -> Option<String> {
    if s.id != ids::USER.survey_id {
        return None;
    }
    let key = format!("$survey_response_{}", ids::USER.questions[2].id);
    str_prop(props, &key)
        .map(str::trim)
        .filter(|e| e.contains('@'))
        .map(str::to_string)
}

/// The kind, where the survey has one, such as `bug`.
pub fn kind(props: &Map<String, Value>) -> Option<&str> {
    str_prop(props, "kind")
}

/// A short title: the first line of the first answer typed, else the kind.
/// From the scrubbed copy, as a title may become public.
pub fn title(s: &Survey, props: &Map<String, Value>) -> String {
    let typed = s
        .questions
        .iter()
        .filter(|q| Some(q.id.as_str()) != email_question(s))
        .filter_map(|q| str_prop(props, &format!("$survey_response_{}_scrubbed", q.id)))
        .find_map(|t| t.lines().map(str::trim).find(|l| !l.is_empty()));
    match typed {
        Some(line) if line.chars().count() > MAX_TITLE => {
            let cut: String = line.chars().take(MAX_TITLE - 1).collect();
            format!("{}…", cut.trim_end())
        }
        Some(line) => line.to_string(),
        None => format!(
            "{} from Arakun",
            capitalized(kind(props).unwrap_or("feedback"))
        ),
    }
}

/// The message: a head line with the kind, the source, the version and the
/// OS, then each answer under its question.
pub fn message(s: &Survey, props: &Map<String, Value>, copy: Answers) -> String {
    let mut head = Vec::new();
    if let Some(kind) = kind(props) {
        head.push(capitalized(kind));
    }
    head.push(match str_prop(props, "source") {
        Some("popover") => format!("survey \"{}\"", s.name),
        Some(src) => format!("{src} feedback"),
        None => "feedback".into(),
    });
    head.push(format!("Arakun {}", env!("ARAKUN_VERSION")));
    head.push(std::env::consts::OS.to_string());
    let mut out = head.join(" · ");

    let email_q = email_question(s);
    for q in &s.questions {
        if Some(q.id.as_str()) == email_q {
            continue;
        }
        let key = format!("$survey_response_{}", q.id);
        let scrubbed = props.get(&format!("{key}_scrubbed"));
        let value = match (copy, scrubbed) {
            (Answers::Scrubbed, Some(v)) => Some(v),
            _ => props.get(&key),
        };
        let Some(answer) = value.and_then(answer_text) else {
            continue;
        };
        out.push_str(&format!("\n\n{}\n{answer}", label(&q.text)));
    }
    if let Some(e) = str_prop(props, "error_event") {
        out.push_str(&format!("\n\nError event:\n{e}"));
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

fn email_question(s: &Survey) -> Option<&'static str> {
    (s.id == ids::USER.survey_id).then(|| ids::USER.questions[2].id)
}

/// A question as a label line: it keeps its own `?` or `:`, else ends in a
/// `:`, in place of a `.` or `!`, so the answer below reads as its answer.
fn label(question: &str) -> String {
    let q = question.trim();
    if q.ends_with(['?', ':']) {
        q.to_string()
    } else {
        format!("{}:", q.trim_end_matches(['.', '!']))
    }
}

fn capitalized(word: &str) -> String {
    let mut chars = word.chars();
    match chars.next() {
        Some(c) => c.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
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

    fn user_props(text: &str, email: &str) -> (Survey, Map<String, Value>) {
        let s = store::fallback(&ids::USER);
        let answers = vec![
            (0, Answer::Choice("Bug".into())),
            (1, Answer::Text(text.into())),
            (2, Answer::Text(email.into())),
        ];
        let p = response_props(&s, &answers, Source::User, &[]);
        (s, p)
    }

    #[test]
    fn message_has_answers_but_never_the_email() {
        let (s, p) = user_props("The tab name resets.", "ann@example.com");
        let msg = message(&s, &p, Answers::AsTyped);
        assert!(msg.starts_with("Bug · user feedback · Arakun "));
        assert!(msg.contains("\n\nWhat's on your mind?\nThe tab name resets."));
        assert!(!msg.contains("ann@example.com"));
        assert_eq!(email_answer(&s, &p).as_deref(), Some("ann@example.com"));
        assert_eq!(sender(&p), "Arakun user");
    }

    #[test]
    fn scrubbed_copy_has_no_links() {
        let (s, p) = user_props("Broken, see https://x.io/a?t=1", "");
        assert!(message(&s, &p, Answers::AsTyped).contains("https://x.io"));
        let public = message(&s, &p, Answers::Scrubbed);
        assert!(!public.contains("x.io"), "{public}");
        assert!(public.starts_with("Bug · user feedback"));
        assert!(!title(&s, &p).contains("x.io"));
    }

    #[test]
    fn text_that_is_not_an_email_is_not_a_contact() {
        let (s, p) = user_props("x", "later");
        assert_eq!(email_answer(&s, &p), None);
    }

    #[test]
    fn agent_sender_names_the_agent() {
        let s = store::fallback(&ids::AGENT);
        let answers = vec![
            (0, Answer::Choice("UX".into())),
            (1, Answer::Text("Hard to find the diff.".into())),
        ];
        let p = response_props(&s, &answers, Source::Agent, &[("agent", "claude".into())]);
        assert_eq!(sender(&p), "claude (agent in Arakun)");
        assert_eq!(title(&s, &p), "Hard to find the diff.");
    }

    #[test]
    fn error_message_links_the_error_and_rating() {
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
        let msg = message(&s, &p, Answers::Scrubbed);
        assert!(msg.starts_with("Bug · error feedback"));
        assert!(msg.contains("\n\nHow bad was it?\n4"));
        assert!(msg.ends_with("\n\nError event:\nabc"));
    }

    #[test]
    fn title_is_the_first_line_cut_short() {
        let (s, p) = user_props("\n  First line  \nsecond", "");
        assert_eq!(title(&s, &p), "First line");
        let (s, p) = user_props(&"é".repeat(200), "");
        let t = title(&s, &p);
        assert_eq!(t.chars().count(), MAX_TITLE);
        assert!(t.ends_with('…'));
    }

    #[test]
    fn title_without_text_names_the_kind() {
        let (s, mut p) = user_props("x", "");
        p.retain(|k, _| !k.ends_with("_scrubbed"));
        assert_eq!(title(&s, &p), "Bug from Arakun");
    }

    #[test]
    fn message_has_no_markdown() {
        let (s, p) = user_props("plain", "");
        let msg = message(&s, &p, Answers::AsTyped);
        for mark in ["**", "`", "#", "> "] {
            assert!(!msg.contains(mark), "{mark} in {msg}");
        }
    }

    #[test]
    fn question_without_mark_gets_a_colon() {
        assert_eq!(label("Steps to reproduce"), "Steps to reproduce:");
        assert_eq!(label("How bad was it? "), "How bad was it?");
        assert_eq!(label("Tell us more."), "Tell us more:");
        assert_eq!(label("Anything else!"), "Anything else:");
    }

    #[test]
    fn long_message_is_cut_on_a_char_boundary() {
        let (s, mut p) = user_props("", "");
        let key = format!("$survey_response_{}", ids::USER.questions[1].id);
        p.insert(key, "é".repeat(MAX_MESSAGE).into());
        let msg = message(&s, &p, Answers::AsTyped);
        assert!(msg.len() <= MAX_MESSAGE + '…'.len_utf8());
        assert!(msg.ends_with('…'));
    }
}
