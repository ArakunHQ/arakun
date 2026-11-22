//! Ask: a question in plain words to PostHog AI, which writes a SQL query for
//! it and answers. Only the question is sent, to the project's own PostHog.

use super::{ApiError, Project, Session};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

#[derive(Clone, Serialize, Deserialize, PartialEq)]
pub struct Answer {
    pub question: String,
    pub text: String,
    pub sql: String,
}

fn parse(question: &str, v: &Value) -> Result<Answer, ApiError> {
    let mut text = String::new();
    let mut sql = String::new();
    for item in v.as_array().into_iter().flatten() {
        let m = &item["data"];
        match m["type"].as_str() {
            Some("ai") => {
                if let Some(reply) = m["content"].as_str().filter(|s| !s.trim().is_empty()) {
                    text = reply.chars().take(16000).collect();
                }
            }
            Some("ai/viz" | "ai/artifact") => {
                let node = if m["type"] == "ai/viz" {
                    &m["answer"]
                } else {
                    &m["content"]["query"]
                };
                let query = if node["kind"] == "DataVisualizationNode" {
                    &node["source"]
                } else {
                    node
                };
                if query["kind"] == "HogQLQuery"
                    && let Some(value) = query["query"].as_str().filter(|s| s.len() <= 64000)
                {
                    sql = value.to_owned();
                }
            }
            _ => {}
        }
    }
    if text.is_empty() && sql.is_empty() {
        return Err(ApiError::Failed(
            "PostHog AI did not return an answer or a SQL query.".into(),
        ));
    }
    Ok(Answer {
        question: question.to_owned(),
        text,
        sql,
    })
}

/// PostHog AI's endpoint: it writes a SQL insight for the question and runs it.
pub(super) const PATH: &str = "/max_tools/create_and_query_insight/";

/// The body PostHog AI gets: the question alone, trimmed, which PostHog
/// takes up to 1,000 characters long.
fn request(question: &str) -> Result<Value, ApiError> {
    let question = question.trim();
    if question.is_empty() || question.chars().count() > 1000 {
        return Err(ApiError::Failed(
            "Ask a question of up to 1,000 characters.".into(),
        ));
    }
    Ok(json!({ "query": question, "insight_type": "sql" }))
}

pub fn ask(s: &Session, project: u64, question: &str) -> Result<Answer, ApiError> {
    let body = request(question)?;
    let v = super::call(
        s,
        "POST",
        &format!("/api/environments/{project}{PATH}"),
        Some(&body),
    )?;
    parse(question.trim(), &v)
}

fn pins_file(session: &str, project: u64) -> std::path::PathBuf {
    super::answers_dir(session).join(format!("{project}-pins.json"))
}

/// The answers pinned in `p`. They live with the account's other answers,
/// so removing the account or replacing its key clears them too.
pub fn pins(p: &Project) -> Vec<Answer> {
    crate::durable::load_json(pins_file(&p.session, p.id)).unwrap_or_default()
}

pub fn save_pins(s: &Session, project: u64, pins: &[Answer]) -> std::io::Result<()> {
    super::cache_answer(s, &pins_file(&s.id, project), &pins)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn only_answer_messages_and_sql_visualizations_are_displayed() {
        let reply = json!([{ "data":{"type":"ack","content":"internal"}},{"data":{"type":"ai/artifact","content":{"query":{"kind":"DataVisualizationNode","source":{"kind":"HogQLQuery","query":"SELECT 7"}}}}},{"data":{"type":"ai","content":"Seven."}}]);
        let a = parse("How many?", &reply).unwrap();
        assert_eq!(a.text, "Seven.");
        assert_eq!(a.sql, "SELECT 7");
        assert!(parse("How many?", &json!([])).is_err());
    }

    #[test]
    fn only_the_trimmed_question_is_sent() {
        assert_eq!(
            request("  How many signups today? ").unwrap(),
            json!({ "query": "How many signups today?", "insight_type": "sql" })
        );
        assert!(request("   ").is_err());
        assert!(request(&"x".repeat(1001)).is_err());
        assert!(request(&"é".repeat(1000)).is_ok());
    }
}
