//! Quick answers in the palette: a question goes to a small fast model with
//! what Arakun already knows about it (matching memory, the failed run on the
//! branch), and the answer streams in under the query. The network runs on a
//! thread of its own; the palette polls a channel, so typing never waits.

use std::sync::mpsc;

use serde_json::{Value, json};

use crate::agent::anthropic;

/// Small and fast: answers in a second or two for a fraction of a cent.
pub const MODEL: &str = "claude-haiku-5-5";
pub const MODEL_LABEL: &str = "Haiku 5.5";
/// Answers stay short: a paragraph, not an essay.
const MAX_TOKENS: u32 = 450;

const SYSTEM: &str = "You answer questions inside Arakun, a desktop app where people run coding agents. \
Answer in the language of the question, in at most four short sentences, plain text. \
Put code, file paths, commands and identifiers in backticks. Use only the context given and say \
plainly when it is not enough to be sure. Never invent file names or run numbers.";

/// What the question is about, gathered by the palette.
#[derive(Clone, Debug, Default)]
pub struct Context {
    pub question: String,
    pub project: String,
    pub branch: String,
    /// (id, title, body) of matching memories.
    pub memories: Vec<(String, String, String)>,
    /// (branch, "relay CI #4812 failed 1h ago", log tail).
    pub failures: Vec<(String, String, Vec<String>)>,
    /// (path, line, text) of a file the question names.
    pub file: Option<(String, u32, String)>,
}

/// One answer as it streams.
#[derive(Clone, Debug, Default)]
pub struct Answer {
    pub question: String,
    pub text: String,
    pub done: bool,
    pub error: Option<String>,
    /// (icon, label) of what it read: `CI #4812`, `memory #325`.
    pub sources: Vec<(&'static str, String)>,
    /// The failed run it read, for "Fix it in a new session".
    pub failure: Option<String>,
    pub tokens: (u64, u64),
}

pub enum Event {
    Text(String),
    Done { input: u64, output: u64 },
    Failed(String),
}

impl Answer {
    /// Takes one event; true when the answer is over.
    pub fn take(&mut self, ev: Event) -> bool {
        match ev {
            Event::Text(t) => {
                self.text.push_str(&t);
                false
            }
            Event::Done { input, output } => {
                self.done = true;
                self.tokens = (input, output);
                true
            }
            Event::Failed(why) => {
                self.done = true;
                self.error = Some(why);
                true
            }
        }
    }
}

/// The user message: the question, then the context in tagged blocks.
pub fn prompt(ctx: &Context) -> String {
    let mut out = format!(
        "Question: {}\n\nProject: {} (branch {})\n",
        ctx.question, ctx.project, ctx.branch
    );
    for (branch, what, log) in &ctx.failures {
        out.push_str(&format!(
            "\n<failed_run branch=\"{branch}\">\n{what}\n{}\n</failed_run>\n",
            log.iter()
                .rev()
                .take(40)
                .rev()
                .cloned()
                .collect::<Vec<_>>()
                .join("\n")
        ));
    }
    for (id, title, body) in &ctx.memories {
        let body: String = body.chars().take(1200).collect();
        out.push_str(&format!(
            "\n<memory id=\"{id}\" title=\"{title}\">\n{body}\n</memory>\n"
        ));
    }
    if let Some((path, line, text)) = &ctx.file {
        out.push_str(&format!(
            "\n<file path=\"{path}\" line=\"{line}\">\n{text}\n</file>\n"
        ));
    }
    out
}

/// The Messages request.
pub fn body(ctx: &Context) -> Value {
    json!({
        "model": MODEL,
        "max_tokens": MAX_TOKENS,
        "stream": true,
        "output_config": {"effort": "low"},
        // A quick answer needs no reasoning pass: it would spend the budget before the text.
        "thinking": {"type": "disabled"},
        "system": SYSTEM,
        "messages": [{"role": "user", "content": prompt(ctx)}],
    })
}

/// Text and the end of one server-sent event's `data:` payload.
pub fn parse_event(data: &str) -> Option<Event> {
    let v: Value = serde_json::from_str(data).ok()?;
    match v["type"].as_str()? {
        "content_block_delta" if v["delta"]["type"] == "text_delta" => {
            Some(Event::Text(v["delta"]["text"].as_str()?.to_string()))
        }
        "message_delta" => Some(Event::Done {
            input: v["usage"]["input_tokens"].as_u64().unwrap_or(0),
            output: v["usage"]["output_tokens"].as_u64().unwrap_or(0),
        }),
        "error" => Some(Event::Failed(
            v["error"]["message"]
                .as_str()
                .unwrap_or("the API failed")
                .to_string(),
        )),
        _ => None,
    }
}

/// Starts answering on a thread; the answer fills from the channel.
pub fn start(ctx: Context) -> (Answer, mpsc::Receiver<Event>) {
    let mut sources: Vec<(&'static str, String)> = Vec::new();
    let mut failure = None;
    for (_, what, _) in &ctx.failures {
        let short = what.split(" failed").next().unwrap_or(what).to_string();
        sources.push(("activity01", short));
        failure.get_or_insert_with(|| what.clone());
    }
    for (id, _, _) in &ctx.memories {
        sources.push(("bot", format!("memory {id}")));
    }
    if let Some((path, line, _)) = &ctx.file {
        sources.push(("file01", format!("{path}:{line}")));
    }
    let answer = Answer {
        question: ctx.question.clone(),
        sources,
        failure,
        ..Default::default()
    };
    let (tx, rx) = mpsc::channel();
    let _ = std::thread::Builder::new()
        .name("palette-answer".into())
        .spawn(move || run(&ctx, &tx));
    (answer, rx)
}

const RULE_SYSTEM: &str = "You turn one sentence into an automation rule for Arakun, a desktop app for coding agents. \
Triggers: pr_checks_pass (a pull request's checks pass; needs its number), pr_merged (needs its number), \
ci_failed (CI fails on a branch; needs the branch), session_finished (an agent session finishes; give its title as session). \
Steps, in order: merge (only after pr_checks_pass; text is squash, merge or rebase), notify (text is the notice), \
run (text is a shell command), agent (text is the prompt for a new agent session). \
hours is how long the rule waits, 24 unless the sentence says. If the sentence is not a rule, set understood to false. \
Unused trigger fields are 0 or empty strings.";

/// Asks the small model to read a sentence as a rule; the answer is JSON
/// in `rules::schema()`, still to be checked with `rules::from_json`.
pub fn start_rule(
    sentence: String,
    sessions: Vec<String>,
) -> mpsc::Receiver<Result<Value, String>> {
    let (tx, rx) = mpsc::channel();
    let _ = std::thread::Builder::new()
        .name("palette-rule".into())
        .spawn(move || {
            let _ = tx.send(read_rule(&sentence, &sessions));
        });
    rx
}

pub fn rule_body(sentence: &str, sessions: &[String]) -> Value {
    let known = if sessions.is_empty() {
        String::new()
    } else {
        format!(
            "\n\nSessions: {}",
            sessions
                .iter()
                .take(40)
                .cloned()
                .collect::<Vec<_>>()
                .join("; ")
        )
    };
    json!({
        "model": MODEL,
        "max_tokens": 300,
        "output_config": {
            "effort": "low",
            "format": {"type": "json_schema", "schema": super::rules::schema()}
        },
        "thinking": {"type": "disabled"},
        "system": RULE_SYSTEM,
        "messages": [{"role": "user", "content": format!("{sentence}{known}")}],
    })
}

fn read_rule(sentence: &str, sessions: &[String]) -> Result<Value, String> {
    let (url, key) = anthropic::endpoint().map_err(|_| no_key())?;
    let request = ureq::http::Request::builder()
        .method("POST")
        .uri(&url)
        .header("x-api-key", key)
        .header("anthropic-version", anthropic::VERSION)
        .header("content-type", "application/json")
        .body(rule_body(sentence, sessions).to_string())
        .map_err(|e| e.to_string())?;
    let response = crate::agent::workers_ai::http()
        .run(request)
        .map_err(|e| format!("could not reach the API ({e})"))?;
    let status = response.status().as_u16();
    let text = response
        .into_body()
        .with_config()
        .limit(256 * 1024)
        .read_to_string()
        .map_err(|e| e.to_string())?;
    let v: Value = serde_json::from_str(&text).map_err(|e| e.to_string())?;
    if status != 200 {
        return Err(v["error"]["message"]
            .as_str()
            .unwrap_or("the API failed")
            .to_string());
    }
    let out = v["content"]
        .as_array()
        .and_then(|c| c.iter().find(|b| b["type"] == "text"))
        .and_then(|b| b["text"].as_str())
        .ok_or("no answer")?;
    serde_json::from_str(out).map_err(|e| format!("not JSON: {e}"))
}

/// How to add a key, when there is none.
pub fn no_key() -> String {
    format!(
        "Quick answers need an Anthropic API key: put it in {} (one line).",
        anthropic::key_path().display()
    )
}

fn run(ctx: &Context, tx: &mpsc::Sender<Event>) {
    let (url, key) = match anthropic::endpoint() {
        Ok(e) => e,
        Err(_) => {
            let _ = tx.send(Event::Failed(no_key()));
            return;
        }
    };
    let request = ureq::http::Request::builder()
        .method("POST")
        .uri(&url)
        .header("x-api-key", key)
        .header("anthropic-version", anthropic::VERSION)
        .header("content-type", "application/json")
        .header("accept", "text/event-stream")
        .body(body(ctx).to_string());
    let request = match request {
        Ok(r) => r,
        Err(e) => {
            let _ = tx.send(Event::Failed(e.to_string()));
            return;
        }
    };
    let response = match crate::agent::workers_ai::http().run(request) {
        Ok(r) => r,
        Err(e) => {
            let _ = tx.send(Event::Failed(format!("could not reach the API ({e})")));
            return;
        }
    };
    let status = response.status().as_u16();
    if status != 200 {
        let text = response
            .into_body()
            .with_config()
            .limit(64 * 1024)
            .read_to_string()
            .unwrap_or_default();
        let why = serde_json::from_str::<Value>(&text)
            .ok()
            .and_then(|v| v["error"]["message"].as_str().map(String::from))
            .unwrap_or_else(|| text.chars().take(200).collect());
        let _ = tx.send(Event::Failed(format!("HTTP {status}: {why}")));
        return;
    }
    use std::io::{BufRead as _, BufReader};
    let reader = BufReader::new(response.into_body().into_reader());
    let mut finished = false;
    for line in reader.lines() {
        let Ok(line) = line else {
            break;
        };
        let Some(data) = line.strip_prefix("data:") else {
            continue;
        };
        if let Some(ev) = parse_event(data.trim()) {
            finished |= matches!(ev, Event::Done { .. } | Event::Failed(_));
            if tx.send(ev).is_err() {
                // The palette closed or the question changed.
                return;
            }
        }
    }
    if !finished {
        let _ = tx.send(Event::Done {
            input: 0,
            output: 0,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_request_is_small_and_streams() {
        let ctx = Context {
            question: "¿por qué falló el CI de phone-local?".into(),
            project: "app".into(),
            branch: "phone-local".into(),
            memories: vec![(
                "#325".into(),
                "Relay deploy".into(),
                "wrangler deploy".into(),
            )],
            failures: vec![(
                "phone-local".into(),
                "relay CI #4812 failed 1h ago".into(),
                vec!["src/index.ts(88,14): error TS2345".into()],
            )],
            file: None,
        };
        let b = body(&ctx);
        assert_eq!(b["model"], MODEL);
        assert_eq!(b["stream"], true);
        assert!(b["max_tokens"].as_u64().unwrap() <= 500);
        let p = b["messages"][0]["content"].as_str().unwrap();
        assert!(p.contains("error TS2345"));
        assert!(p.contains("<memory id=\"#325\""));
        let (a, _rx) = {
            // Sources are known before any byte arrives; no thread needed to check them.
            let mut a = Answer::default();
            a.sources.push(("activity01", "relay CI #4812".into()));
            (a, ())
        };
        assert_eq!(a.sources[0].1, "relay CI #4812");
    }

    #[test]
    fn the_rule_request_asks_for_the_schema() {
        let b = rule_body("mergea #64 cuando pasen", &["Sidebar redesign".into()]);
        assert_eq!(b["output_config"]["format"]["type"], "json_schema");
        assert_eq!(
            b["output_config"]["format"]["schema"]["additionalProperties"],
            false
        );
        assert!(
            b["messages"][0]["content"]
                .as_str()
                .unwrap()
                .contains("Sidebar redesign")
        );
    }

    #[test]
    fn stream_events_are_read() {
        let t = parse_event(
            r#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"Falló"}}"#,
        );
        assert!(matches!(t, Some(Event::Text(ref s)) if s == "Falló"));
        let d = parse_event(
            r#"{"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"input_tokens":9,"output_tokens":40}}"#,
        );
        assert!(matches!(
            d,
            Some(Event::Done {
                input: 9,
                output: 40
            })
        ));
        let e = parse_event(
            r#"{"type":"error","error":{"type":"overloaded_error","message":"Overloaded"}}"#,
        );
        assert!(matches!(e, Some(Event::Failed(ref s)) if s == "Overloaded"));
        assert!(parse_event(r#"{"type":"ping"}"#).is_none());
        assert!(parse_event("not json").is_none());
    }

    #[test]
    fn an_answer_fills_and_ends() {
        let mut a = Answer::default();
        assert!(!a.take(Event::Text("a".into())));
        assert!(!a.take(Event::Text("b".into())));
        assert!(a.take(Event::Done {
            input: 1,
            output: 2
        }));
        assert_eq!(a.text, "ab");
        assert!(a.done);
        let mut b = Answer::default();
        assert!(b.take(Event::Failed("x".into())));
        assert_eq!(b.error.as_deref(), Some("x"));
    }
}
