//! Made-up PostHog answers for looking at the tab in a test Arakun
//! (`arakun dev`), where there is no real sign-in. Only a test Arakun reads
//! `ARAKUN_POSTHOG_FIXTURE`; a normal one never does, so nothing here can
//! stand in for a real account.
//!
//! The file: `{"sessions": [Session…], "replies": [{"path": "/persons/",
//! "query": "uniq(person_id)", "status": 200, "body": …}…]}`. The first reply
//! whose `path` is in the call's path (and `query` in its HogQL, `method` is
//! its method and `request` its whole body, when given) answers.
//! `tools/posthog-fixture.json` is one to start from.

use std::sync::OnceLock;

use serde::Deserialize;
use serde_json::Value;

use super::{Reply, Session};

#[derive(Deserialize)]
struct Rule {
    path: String,
    #[serde(default)]
    method: Option<String>,
    #[serde(default)]
    request: Option<Value>,
    #[serde(default)]
    query: Option<String>,
    #[serde(default = "ok")]
    status: u16,
    #[serde(default)]
    body: Value,
}

fn ok() -> u16 {
    200
}

#[derive(Deserialize, Default)]
struct Fixture {
    #[serde(default)]
    sessions: Vec<Session>,
    #[serde(default)]
    replies: Vec<Rule>,
}

fn loaded() -> Option<&'static Fixture> {
    static F: OnceLock<Option<Fixture>> = OnceLock::new();
    F.get_or_init(|| {
        crate::dev::size()?;
        let file = std::env::var_os("ARAKUN_POSTHOG_FIXTURE")?;
        // A broken fixture still answers (with nothing) rather than letting
        // the test instance reach the real PostHog.
        Some(crate::durable::load_json(file).unwrap_or_default())
    })
    .as_ref()
}

pub(super) fn sessions() -> Option<Vec<Session>> {
    loaded().map(|f| f.sessions.clone())
}

pub(super) fn reply(method: &str, path: &str, body: Option<&Value>) -> Option<Reply> {
    let f = loaded()?;
    let query = body
        .and_then(|b| b["query"]["query"].as_str())
        .unwrap_or_default();
    let rule = f.replies.iter().find(|r| {
        r.method.as_deref().is_none_or(|m| m == method)
            && r.request.as_ref().is_none_or(|v| Some(v) == body)
            && path.contains(&r.path)
            && r.query.as_deref().is_none_or(|q| query.contains(q))
    });
    Some(match rule {
        Some(r) => Reply {
            status: r.status,
            retry_after: None,
            remaining: None,
            limit: None,
            text: r.body.to_string(),
        },
        None => Reply {
            status: 404,
            retry_after: None,
            remaining: None,
            limit: None,
            text: r#"{"detail":"Not in the fixture."}"#.into(),
        },
    })
}
