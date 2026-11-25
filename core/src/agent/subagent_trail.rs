//! What a Claude subagent did last, read from the transcript Claude Code
//! writes for it (`<project>/<session>/subagents/agent-<id>.jsonl`).
//!
//! Claude's adapter can drop every live update of a subagent: one that
//! SendMessage resumed, in the AIR form (claude-agent-acp#1649). Its child
//! thread then shows only that it is working. The file still grows, so it
//! tells a subagent that works from one that is stuck.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use serde_json::Value;

use super::catalog::claude_project_dir;
use crate::account::parse_rfc3339;

/// How much of the file's end is read. A record can be larger (a long tool
/// result); the file's time still says when it last wrote.
const TAIL: u64 = 256 * 1024;

/// The subagent's latest step and when it last wrote.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Activity {
    /// The tool it called last, or "Thinking" or "Writing". `None` when the
    /// tail holds no whole step.
    pub step: Option<String>,
    /// Unix seconds.
    pub at: u64,
}

impl Activity {
    /// "Bash · 20s ago", "Thinking · just now".
    pub fn label(&self, now: u64) -> String {
        let ago = match now.saturating_sub(self.at) {
            0..5 => "just now".to_string(),
            d @ 5..60 => format!("{d}s ago"),
            _ => crate::account::ago(self.at, now),
        };
        match &self.step {
            Some(step) => format!("{step} · {ago}"),
            None => format!("Active {ago}"),
        }
    }
}

/// The transcript of subagent `id` of Claude session `session` run in `cwd`.
/// A resumed generation (`<agent>:generation:<n>`) writes to its agent's file.
pub fn transcript_path(cwd: &Path, session: &str, id: &str) -> PathBuf {
    let agent = id.split(':').next().unwrap_or(id);
    claude_project_dir(cwd)
        .join(session)
        .join("subagents")
        .join(format!("agent-{agent}.jsonl"))
}

/// Reads the end of the transcript at `path`. `None` when there is no file.
pub fn read(path: &Path) -> Option<Activity> {
    let mut file = File::open(path).ok()?;
    let meta = file.metadata().ok()?;
    let written = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_secs());
    let start = meta.len().saturating_sub(TAIL);
    file.seek(SeekFrom::Start(start)).ok()?;
    let mut buf = Vec::new();
    file.take(TAIL).read_to_end(&mut buf).ok()?;
    let text = String::from_utf8_lossy(&buf);
    // The first line is cut unless the read began at the start.
    let text = if start > 0 {
        text.split_once('\n').map_or("", |(_, rest)| rest)
    } else {
        &text
    };
    let mut activity = last_activity(text).unwrap_or(Activity {
        step: None,
        at: written,
    });
    activity.at = activity.at.max(written);
    Some(activity)
}

/// The latest step in `jsonl`, the lines of a transcript, with the time of
/// its newest message.
pub fn last_activity(jsonl: &str) -> Option<Activity> {
    let mut at = None;
    for line in jsonl.lines().rev() {
        let Ok(v) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        let kind = v["type"].as_str();
        if !matches!(kind, Some("assistant" | "user")) {
            continue;
        }
        let Some(ts) = v["timestamp"].as_str().and_then(parse_rfc3339) else {
            continue;
        };
        let at = *at.get_or_insert(ts);
        if kind != Some("assistant") {
            continue;
        }
        let Some(block) = v["message"]["content"]
            .as_array()
            .and_then(|blocks| blocks.last())
        else {
            continue;
        };
        let step = match block["type"].as_str() {
            Some("tool_use") => match block["name"].as_str() {
                Some(name) if !name.trim().is_empty() => name.to_string(),
                _ => continue,
            },
            Some("thinking" | "redacted_thinking") => "Thinking".into(),
            Some("text") => "Writing".into(),
            _ => continue,
        };
        return Some(Activity {
            step: Some(step),
            at,
        });
    }
    at.map(|at| Activity { step: None, at })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assistant(ts: &str, block: &str) -> String {
        format!(
            r#"{{"type":"assistant","timestamp":"{ts}","message":{{"role":"assistant","content":[{block}]}}}}"#
        )
    }

    fn tool_result(ts: &str) -> String {
        format!(
            r#"{{"type":"user","timestamp":"{ts}","message":{{"role":"user","content":[{{"type":"tool_result","tool_use_id":"t","content":"ok"}}]}}}}"#
        )
    }

    #[test]
    fn the_last_tool_call_names_the_step() {
        let lines = [
            assistant(
                "2026-11-23T00:30:40.000Z",
                r#"{"type":"thinking","thinking":"hm"}"#,
            ),
            assistant(
                "2026-11-23T00:30:48.000Z",
                r#"{"type":"tool_use","id":"t","name":"Bash","input":{}}"#,
            ),
            tool_result("2026-11-23T00:30:49.000Z"),
            r#"{"type":"api-request","timestamp":"2026-11-23T00:30:50.000Z"}"#.to_string(),
        ]
        .join("\n");
        let a = last_activity(&lines).unwrap();
        assert_eq!(a.step.as_deref(), Some("Bash"));
        // The tool's result is newer than the call: the subagent wrote then.
        assert_eq!(a.at, parse_rfc3339("2026-11-23T00:30:49.000Z").unwrap());
    }

    #[test]
    fn thinking_and_text_are_steps_too() {
        let think = assistant(
            "2026-11-23T00:30:40.000Z",
            r#"{"type":"thinking","thinking":"hm"}"#,
        );
        assert_eq!(
            last_activity(&think).unwrap().step.as_deref(),
            Some("Thinking")
        );
        let text = assistant(
            "2026-11-23T00:30:40.000Z",
            r#"{"type":"text","text":"done"}"#,
        );
        assert_eq!(
            last_activity(&text).unwrap().step.as_deref(),
            Some("Writing")
        );
    }

    #[test]
    fn a_cut_or_strange_tail_still_has_a_time() {
        let lines = format!(
            "{{\"type\":\"assistant\",\"timest\n{}",
            tool_result("2026-11-23T00:30:49.000Z")
        );
        assert_eq!(
            last_activity(&lines),
            Some(Activity {
                step: None,
                at: parse_rfc3339("2026-11-23T00:30:49.000Z").unwrap()
            })
        );
        assert_eq!(last_activity("not json\n"), None);
    }

    #[test]
    fn a_resumed_generation_reads_its_agents_file() {
        let a = transcript_path(Path::new("/p"), "s1", "a0d2:generation:2");
        assert!(a.ends_with("s1/subagents/agent-a0d2.jsonl"));
        assert_eq!(a, transcript_path(Path::new("/p"), "s1", "a0d2"));
    }

    #[test]
    fn the_label_says_what_and_when() {
        let a = Activity {
            step: Some("Bash".into()),
            at: 100,
        };
        assert_eq!(a.label(102), "Bash · just now");
        assert_eq!(a.label(120), "Bash · 20s ago");
        assert_eq!(a.label(400), "Bash · 5m ago");
        let quiet = Activity {
            step: None,
            at: 100,
        };
        assert_eq!(quiet.label(130), "Active 30s ago");
    }

    #[test]
    fn reads_the_end_of_a_large_file() {
        let dir = std::env::temp_dir().join(format!("arakun-trail-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("agent-x.jsonl");
        let filler = tool_result("2026-11-23T00:00:00.000Z");
        let mut body = String::new();
        while body.len() < (TAIL as usize) * 2 {
            body.push_str(&filler);
            body.push('\n');
        }
        body.push_str(&assistant(
            "2026-11-23T00:30:48.000Z",
            r#"{"type":"tool_use","id":"t","name":"Read","input":{}}"#,
        ));
        body.push('\n');
        std::fs::write(&path, body).unwrap();
        let a = read(&path).unwrap();
        assert_eq!(a.step.as_deref(), Some("Read"));
        assert!(read(&dir.join("missing.jsonl")).is_none());
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
