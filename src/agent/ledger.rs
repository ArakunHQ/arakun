//! The usage log: one line per settled turn in `<app dir>/usage.jsonl`, with
//! what the turn used (tokens, cost, credits). It outlives the sessions, so
//! totals by day, project and model can be worked out later.

use std::io::{BufRead as _, Write as _};
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

pub fn append(turn: &Turn) -> Result<()> {
    let path = path();
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let mut line = serde_json::to_vec(turn)?;
    line.push(b'\n');
    // One write per line, so lines from two sessions never interleave.
    std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)?
        .write_all(&line)?;
    Ok(())
}

/// Every turn logged so far; lines that do not parse are skipped.
pub fn read() -> Vec<Turn> {
    let Ok(file) = std::fs::File::open(path()) else {
        return Vec::new();
    };
    std::io::BufReader::new(file)
        .lines()
        .map_while(Result::ok)
        .filter_map(|l| serde_json::from_str(&l).ok())
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
}
