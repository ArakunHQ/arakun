//! A transcript as plain text, for the commands that print one in a terminal.

use super::{Entry, PlanStatus, ToolCall, ToolContent, ToolStatus};

const USER_CHARS: usize = 1000;
const THOUGHT_CHARS: usize = 300;
const OUTPUT_LINES: usize = 8;
const OUTPUT_CHARS: usize = 800;

/// A session's last `n` entries as plain text, for `arakun agent log` and `arakun server log`.
pub fn render_log(entries: &[Entry], n: usize) -> String {
    let shown: Vec<String> = entries.iter().filter_map(render_entry).collect();
    shown[shown.len().saturating_sub(n)..].join("\n\n")
}

fn render_entry(e: &Entry) -> Option<String> {
    Some(match e {
        Entry::User { text, .. } => format!("you: {}", head(text.trim(), USER_CHARS)),
        Entry::Agent { text } if !text.trim().is_empty() => format!("agent: {}", text.trim()),
        Entry::Thought { text } if !text.trim().is_empty() => {
            format!("thought: {}", head(text.trim(), THOUGHT_CHARS))
        }
        Entry::Tool(t) => {
            let status = match t.status {
                ToolStatus::Pending => "pending",
                ToolStatus::InProgress => "in_progress",
                ToolStatus::Completed => "completed",
                ToolStatus::Failed => "failed",
            };
            let mut out = format!("[{}] {status}", t.title);
            if let Some(code) = t.exit_code.filter(|c| *c != 0) {
                out.push_str(&format!(" (exit {code})"));
            }
            if let Some(command) = tool_command(t) {
                out.push_str(&format!("\n$ {command}"));
            }
            let output = tool_output(t);
            if !output.trim().is_empty() {
                out.push('\n');
                out.push_str(&tail(output.trim_end(), OUTPUT_LINES, OUTPUT_CHARS));
            }
            out
        }
        Entry::Plan { entries } => {
            let mut out = "plan:".to_string();
            for p in entries {
                let mark = match p.status {
                    PlanStatus::Completed => "x",
                    PlanStatus::InProgress => "~",
                    PlanStatus::Pending | PlanStatus::Unknown => " ",
                };
                out.push_str(&format!("\n  [{mark}] {}", p.content));
            }
            out
        }
        Entry::Notice { text, error, .. } => format!(
            "{}: {}",
            if *error { "error" } else { "notice" },
            text.trim()
        ),
        Entry::Compacted { .. } => "(context compacted)".into(),
        _ => return None,
    })
}

/// The shell command a tool ran, when its input says.
fn tool_command(t: &ToolCall) -> Option<String> {
    let input = t.raw_input.as_ref()?;
    let raw = match input.get("command").or_else(|| input.get("cmd"))? {
        serde_json::Value::String(s) => s.clone(),
        serde_json::Value::Array(parts) => parts
            .iter()
            .filter_map(serde_json::Value::as_str)
            .collect::<Vec<_>>()
            .join(" "),
        _ => return None,
    };
    Some(head(raw.trim(), OUTPUT_CHARS))
}

fn tool_output(t: &ToolCall) -> String {
    if !t.terminal_output.is_empty() {
        return t.terminal_output.clone();
    }
    let texts: Vec<&str> = t
        .content
        .iter()
        .filter_map(|c| match c {
            ToolContent::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect();
    if texts.is_empty() {
        t.raw_output.clone().unwrap_or_default()
    } else {
        texts.join("\n")
    }
}

pub fn head(text: &str, max: usize) -> String {
    match text.char_indices().nth(max) {
        Some((i, _)) => format!("{}…", &text[..i]),
        None => text.to_string(),
    }
}

/// The last `lines` lines of `text`, and of those at most the last `max` characters.
fn tail(text: &str, lines: usize, max: usize) -> String {
    let all: Vec<&str> = text.lines().collect();
    let kept = all[all.len().saturating_sub(lines)..].join("\n");
    let n = kept.chars().count();
    if n <= max {
        return kept;
    }
    format!("…{}", kept.chars().skip(n - max).collect::<String>())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::PlanEntry;

    fn tool(status: ToolStatus, command: &str, output: &str, exit_code: Option<i64>) -> Entry {
        Entry::Tool(ToolCall {
            id: "t".into(),
            title: "Run tests".into(),
            kind: "execute".into(),
            status,
            content: Vec::new(),
            locations: Vec::new(),
            raw_input: Some(serde_json::json!({ "command": command })),
            raw_output: None,
            added: 0,
            removed: 0,
            terminal_output: output.into(),
            terminal_dropped_lines: 0,
            exit_code,
            parent: None,
            tool_name: None,
            secret: None,
        })
    }

    #[test]
    fn log_reads_as_plain_text() {
        let entries = vec![
            Entry::User {
                text: "fix the build".into(),
                at: 1,
                elapsed_ms: None,
                message_id: None,
                tokens: None,
                shell_changes: Vec::new(),
            },
            Entry::Thought {
                text: "x".repeat(400),
            },
            tool(
                ToolStatus::Failed,
                "cargo test",
                &(1..=20).map(|i| format!("line {i}\n")).collect::<String>(),
                Some(101),
            ),
            Entry::Agent { text: "  ".into() },
            Entry::Plan {
                entries: vec![
                    PlanEntry {
                        content: "find it".into(),
                        priority: String::new(),
                        status: PlanStatus::Completed,
                    },
                    PlanEntry {
                        content: "fix it".into(),
                        priority: String::new(),
                        status: PlanStatus::Pending,
                    },
                ],
            },
            Entry::Notice {
                text: "Check failed".into(),
                error: true,
                mark: None,
                places: Vec::new(),
            },
            Entry::Agent {
                text: "Fixed.\n".into(),
            },
        ];
        let log = render_log(&entries, 30);
        let thought = format!("thought: {}…", "x".repeat(300));
        let output = (13..=20)
            .map(|i| format!("line {i}"))
            .collect::<Vec<_>>()
            .join("\n");
        assert_eq!(
            log,
            format!(
                "you: fix the build\n\n{thought}\n\n[Run tests] failed (exit 101)\n$ cargo test\n{output}\n\n\
                 plan:\n  [x] find it\n  [ ] fix it\n\nerror: Check failed\n\nagent: Fixed."
            )
        );
        assert_eq!(
            render_log(&entries, 2),
            "error: Check failed\n\nagent: Fixed."
        );
        assert_eq!(render_log(&[], 5), "");
        assert_eq!(tail("abcdef", 5, 3), "…def");
        assert_eq!(head("abcdef", 3), "abc…");
    }
}
