//! Made-up Linear answers for looking at the tab in a test Arakun
//! (`arakun dev`), where there is no real sign-in. Only a test Arakun reads
//! `ARAKUN_LINEAR_FIXTURE`; a normal one never does, so nothing here can
//! stand in for a real account.
//!
//! The file: `{"sessions": [Session…], "replies": [{"query": "<text the
//! GraphQL query contains>", "status": 200, "body": …}…]}`. The first reply
//! whose `query` is in the call's query answers.

use std::sync::OnceLock;

use serde::Deserialize;
use serde_json::Value;

use super::Session;

#[derive(Deserialize)]
struct Rule {
    query: String,
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
        let file = std::env::var_os("ARAKUN_LINEAR_FIXTURE")?;
        // A broken fixture still answers (with nothing) rather than letting
        // the test instance reach the real Linear.
        Some(crate::durable::load_json(file).unwrap_or_default())
    })
    .as_ref()
}

pub(super) fn sessions() -> Option<Vec<Session>> {
    loaded().map(|f| f.sessions.clone())
}

/// The status and body a fixture gives `query`, or None outside a test Arakun.
pub(super) fn reply(query: &str) -> Option<(u16, String)> {
    let f = loaded()?;
    Some(match f.replies.iter().find(|r| query.contains(&r.query)) {
        Some(r) => (r.status, r.body.to_string()),
        None => (
            200,
            r#"{"errors":[{"message":"Not in the fixture."}]}"#.into(),
        ),
    })
}
