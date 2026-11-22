//! Made-up Doop answers for looking at the tab in a test Arakun
//! (`arakun dev`), where there is no real sign-in. Only a test Arakun reads
//! `ARAKUN_DOOP_FIXTURE`; a normal one never does, so nothing here can stand
//! in for a real instance.
//!
//! The file: `{"sessions": [Session…], "replies": [{"method": "tools/call",
//! "tool": "get_canvas", "result": …}…]}`. The first reply with the call's
//! method (and tool, when it names one) answers with its `result`.

use std::sync::OnceLock;

use serde::Deserialize;
use serde_json::Value;

use super::Session;

#[derive(Deserialize)]
struct Rule {
    method: String,
    #[serde(default)]
    tool: Option<String>,
    #[serde(default)]
    result: Value,
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
        let file = std::env::var_os("ARAKUN_DOOP_FIXTURE")?;
        serde_json::from_slice(&std::fs::read(file).ok()?).ok()
    })
    .as_ref()
}

pub(super) fn on() -> bool {
    loaded().is_some()
}

pub(super) fn sessions() -> Option<Vec<Session>> {
    loaded().map(|f| f.sessions.clone())
}

/// The made-up result of a call. A call no rule covers answers with nothing,
/// so a test Arakun never reaches a real instance.
pub(super) fn reply(method: &str, params: &Value) -> Option<Value> {
    let f = loaded()?;
    let tool = params["name"].as_str();
    Some(
        f.replies
            .iter()
            .find(|r| r.method == method && r.tool.as_deref().is_none_or(|t| Some(t) == tool))
            .map(|r| r.result.clone())
            .unwrap_or(Value::Null),
    )
}
