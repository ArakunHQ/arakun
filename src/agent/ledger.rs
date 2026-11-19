//! The usage log: one line per settled turn in `<app dir>/usage.jsonl`, with
//! what the turn used (tokens, cost, credits). It outlives the sessions, so
//! totals by day, project and model can be worked out later.

use std::path::PathBuf;

use anyhow::Result;
use serde::{Deserialize, Serialize};

use super::AgentKind;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Turn {
    /// Unix seconds when the turn settled.
    pub at: u64,
    pub agent: AgentKind,
    /// The agent's session id.
    pub session: String,
    pub cwd: PathBuf,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default)]
    pub input: u64,
    #[serde(default)]
    pub output: u64,
    #[serde(default)]
    pub cache_read: u64,
    #[serde(default)]
    pub cache_write: u64,
    /// USD.
    #[serde(default)]
    pub cost: f64,
    /// `cost` is an API-price estimate, not a charge (a claude.ai plan).
    #[serde(default)]
    pub cost_estimate: bool,
    #[serde(default)]
    pub credits: u64,
    #[serde(default)]
    pub ms: u64,
}

impl Turn {
    pub fn is_empty(&self) -> bool {
        self.input + self.output + self.cache_read + self.cache_write + self.credits == 0
            && self.cost <= 0.
    }
}

pub fn path() -> PathBuf {
    std::env::var_os("ARAKUN_USAGE_LOG")
        .map(PathBuf::from)
        .unwrap_or_else(|| crate::platform::app_dir().join("usage.jsonl"))
}

/// About 60,000 turns. Past it the log moves to `usage.old.jsonl`, so the
/// two keep the latest turns without growing for ever.
const MAX_BYTES: u64 = 16 << 20;

pub fn append(turn: &Turn) -> Result<()> {
    let mut line = serde_json::to_vec(turn)?;
    line.push(b'\n');
    crate::rotate::append(&path(), &line, MAX_BYTES)?;
    Ok(())
}

/// Every turn still logged, oldest first. A line that does not parse, such
/// as one cut short by a crash, is skipped and the lines after it still read.
pub fn read() -> Vec<Turn> {
    let path = path();
    [crate::rotate::old_path(&path), path]
        .iter()
        .filter_map(|p| std::fs::read(p).ok())
        .flat_map(|bytes| parse(&bytes))
        .collect()
}

fn parse(bytes: &[u8]) -> Vec<Turn> {
    bytes
        .split(|&b| b == b'\n')
        .filter_map(|l| serde_json::from_slice(l).ok())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn turns_round_trip_and_old_lines_still_read() {
        let t = Turn {
            at: 1,
            agent: AgentKind::Claude,
            session: "s".into(),
            cwd: "/p".into(),
            model: Some("claude-opus-4-7".into()),
            input: 10,
            output: 5,
            cache_read: 100,
            cache_write: 20,
            cost: 0.25,
            cost_estimate: true,
            credits: 0,
            ms: 900,
        };
        let line = serde_json::to_string(&t).unwrap();
        assert_eq!(serde_json::from_str::<Turn>(&line).unwrap(), t);
        let bare: Turn = serde_json::from_str(
            r#"{"at":2,"agent":"droid","session":"x","cwd":"/q","credits":7}"#,
        )
        .unwrap();
        assert_eq!(
            (bare.credits, bare.cost, bare.model.as_deref()),
            (7, 0., None)
        );
        assert!(!bare.is_empty());
    }

    #[test]
    fn a_bad_line_does_not_hide_the_turns_after_it() {
        let ok = r#"{"at":1,"agent":"droid","session":"x","cwd":"/q","credits":1}"#;
        let bytes = [ok.as_bytes(), b"\n{\"at\":2,\xff\n", ok.as_bytes(), b"\n"].concat();
        assert_eq!(parse(&bytes).len(), 2);
    }
}
