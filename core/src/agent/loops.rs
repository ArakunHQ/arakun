//! Notices an agent going in circles during a turn: one command failing the
//! same way again and again, one call repeated back to back with the same
//! result, or an edit applied, undone and applied again.

use std::collections::HashMap;
use std::path::Path;

use serde_json::Value;

use super::{Entry, ToolCall, ToolContent, ToolStatus};

/// Failures of one call with the same output before the turn counts as stuck.
const FAILS: usize = 3;
/// Identical calls in a row with identical results.
const REPEATS: usize = 4;
/// Finished calls after which a pattern that stopped recurring is let go.
const STALE: usize = 8;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Pattern {
    Failing,
    Repeating,
    Reverting,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Stuck {
    /// Names the pattern, so "Keep going" silences it for the rest of the turn.
    // Not a secret: the pattern's name.
    pub key: String,
    pattern: Pattern,
    subject: String,
    times: usize,
}

impl Stuck {
    fn describe(&self, open: &str, close: &str) -> String {
        let s = format!("{open}{}{close}", self.subject);
        match self.pattern {
            Pattern::Failing => format!("{s} failed {} times with the same output", self.times),
            Pattern::Repeating => {
                format!("{s} ran {} times in a row with the same result", self.times)
            }
            Pattern::Reverting => format!("the edits to {s} keep undoing each other"),
        }
    }

    pub fn message(&self) -> String {
        let text = self.describe("“", "”");
        let mut chars = text.chars();
        chars
            .next()
            .map_or_else(String::new, |c| c.to_uppercase().chain(chars).collect())
    }

    /// What "Stop and rethink" sends once the turn has stopped.
    pub fn prompt(&self) -> String {
        format!(
            "You seem to be going in circles: {}. Doing it again will not help. \
             Stop and explain in a few lines what you think is actually going wrong, \
             then try a different approach.",
            self.describe("`", "`")
        )
    }
}

/// The latest pattern among a turn's entries, unless it was dismissed or has
/// not recurred in the last few calls.
pub fn detect(entries: &[Entry], dismissed: &[String]) -> Option<Stuck> {
    let mut latest: Option<(usize, Stuck)> = None;
    let mut keep = |at: usize, s: Stuck| {
        if !dismissed.contains(&s.key) {
            latest = Some((at, s));
        }
    };
    let mut fails: HashMap<String, (String, usize)> = HashMap::new();
    let mut edits: HashMap<(&Path, &str, &str), usize> = HashMap::new();
    let mut run: Option<(String, String, usize)> = None;
    let mut n = 0;
    let tools = entries.iter().filter_map(|e| match e {
        Entry::Tool(t) => Some(t),
        _ => None,
    });
    for t in tools {
        if !matches!(t.status, ToolStatus::Completed | ToolStatus::Failed)
            || matches!(t.kind.as_str(), "think" | "switch_mode")
        {
            continue;
        }
        n += 1;
        let key = call_key(t);
        let output = fingerprint(output(t));
        if failed(t) {
            run = None;
            let (seen, times) = fails.entry(key.clone()).or_default();
            if *seen != output || *times == 0 {
                *seen = output;
                *times = 0;
            }
            *times += 1;
            if *times >= FAILS {
                let times = *times;
                keep(
                    n,
                    Stuck {
                        key: format!("fail {key}"),
                        pattern: Pattern::Failing,
                        subject: subject(t),
                        times,
                    },
                );
            }
            continue;
        }
        fails.remove(&key);
        match &mut run {
            Some((k, o, times)) if *k == key && *o == output => *times += 1,
            _ => run = Some((key.clone(), output, 1)),
        }
        if let Some((_, _, times)) = &run
            && *times >= REPEATS
        {
            let times = *times;
            keep(
                n,
                Stuck {
                    key: format!("repeat {key}"),
                    pattern: Pattern::Repeating,
                    subject: subject(t),
                    times,
                },
            );
        }
        for c in &t.content {
            if let ToolContent::Diff {
                path,
                old_text: Some(old),
                new_text,
            } = c
                && old != new_text
            {
                let times = edits
                    .entry((path.as_path(), old.as_str(), new_text.as_str()))
                    .or_default();
                *times += 1;
                if *times >= 2 {
                    let name = path.file_name().map_or_else(
                        || path.display().to_string(),
                        |f| f.to_string_lossy().into_owned(),
                    );
                    keep(
                        n,
                        Stuck {
                            key: format!("edit {}", path.display()),
                            pattern: Pattern::Reverting,
                            subject: name,
                            times: *times,
                        },
                    );
                }
            }
        }
    }
    latest.filter(|(at, _)| n - at < STALE).map(|(_, s)| s)
}

fn call_key(t: &ToolCall) -> String {
    if t.kind == "execute" {
        return format!("run {}", command(t).unwrap_or_else(|| normalize(&t.title)));
    }
    match &t.raw_input {
        Some(input) => format!("{} {input}", t.kind),
        None => format!("{} {}", t.kind, t.title),
    }
}

fn subject(t: &ToolCall) -> String {
    let text = match command(t).filter(|_| t.kind == "execute") {
        Some(cmd) => cmd,
        None if t.title.trim().is_empty() => t.kind.clone(),
        None => normalize(&t.title),
    };
    match text.char_indices().nth(60) {
        Some((i, _)) => format!("{}…", &text[..i]),
        None => text,
    }
}

/// The shell command a call ran, without a leading `cd … &&` or trailing `2>&1`.
fn command(t: &ToolCall) -> Option<String> {
    let input = t.raw_input.as_ref()?;
    let raw = match input.get("command").or_else(|| input.get("cmd"))? {
        Value::String(s) => s.clone(),
        Value::Array(parts) => {
            let parts: Vec<&str> = parts.iter().filter_map(Value::as_str).collect();
            match parts.as_slice() {
                [shell, flag, script]
                    if flag.starts_with('-')
                        && matches!(shell.rsplit('/').next(), Some("bash" | "sh" | "zsh")) =>
                {
                    script.to_string()
                }
                _ => parts.join(" "),
            }
        }
        _ => return None,
    };
    Some(normalize(&raw))
}

fn normalize(cmd: &str) -> String {
    let mut s = cmd.split_whitespace().collect::<Vec<_>>().join(" ");
    while let Some(rest) = s
        .strip_prefix("cd ")
        .and_then(|r| r.split_once(" && "))
        .map(|(_, r)| r.to_string())
    {
        s = rest;
    }
    match s.strip_suffix(" 2>&1") {
        Some(rest) => rest.to_string(),
        None => s,
    }
}

fn output(t: &ToolCall) -> &str {
    if !t.terminal_output.is_empty() {
        return &t.terminal_output;
    }
    if let Some(out) = &t.raw_output {
        return out;
    }
    t.content
        .iter()
        .rev()
        .find_map(|c| match c {
            ToolContent::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .unwrap_or("")
}

/// The last lines of an output with numbers masked, so timings and counts
/// don't make the same failure look new.
fn fingerprint(output: &str) -> String {
    let lines: Vec<String> = output
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .rev()
        .take(6)
        .map(|l| {
            l.chars()
                .map(|c| if c.is_ascii_digit() { '#' } else { c })
                .collect()
        })
        .collect();
    lines.join("\n")
}

fn failed(t: &ToolCall) -> bool {
    if t.status == ToolStatus::Failed {
        return true;
    }
    let code = t.exit_code.or_else(|| {
        (t.kind == "execute")
            .then(|| t.raw_output.as_deref().and_then(reported_exit))
            .flatten()
    });
    code.is_some_and(|c| c != 0)
}

/// Droid reports a command's exit in its output instead of failing the call.
fn reported_exit(out: &str) -> Option<i64> {
    let number = |s: &str| {
        let digits: String = s
            .chars()
            .take_while(|c| c.is_ascii_digit() || *c == '-')
            .collect();
        digits.parse().ok()
    };
    if let Some((_, rest)) = out.rsplit_once("[Process exited with code ") {
        return number(rest);
    }
    out.trim_start()
        .strip_prefix("Command failed (exit code: ")
        .and_then(number)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn tool(id: usize, kind: &str, input: Value, status: &str, output: &str) -> Entry {
        serde_json::from_value(json!({
            "type": "tool", "id": id.to_string(), "title": input["command"].as_str().unwrap_or("Read a.rs"),
            "kind": kind, "status": status, "raw_input": input, "raw_output": output,
        }))
        .unwrap()
    }

    fn run(id: usize, cmd: &str, status: &str, output: &str) -> Entry {
        tool(id, "execute", json!({"command": cmd}), status, output)
    }

    fn edit(id: usize, old: &str, new: &str) -> Entry {
        serde_json::from_value(json!({
            "type": "tool", "id": id.to_string(), "title": "Edit src/a.rs", "kind": "edit",
            "status": "completed", "raw_input": {"old_str": old, "new_str": new},
            "content": [{"type": "diff", "path": "/p/src/a.rs", "old_text": old, "new_text": new}],
        }))
        .unwrap()
    }

    const FAIL: &str =
        "test a ... FAILED\ntest result: FAILED. 3 passed; 1 failed; finished in 0.41s";
    const FAIL_SLOWER: &str =
        "test a ... FAILED\ntest result: FAILED. 3 passed; 1 failed; finished in 1.07s";

    #[test]
    fn same_failure_three_times_is_stuck_even_with_edits_between() {
        let entries = [
            run(1, "cargo test", "failed", FAIL),
            edit(2, "a", "b"),
            run(3, "cd /p && cargo   test", "failed", FAIL_SLOWER),
            edit(4, "b", "c"),
            run(5, "cargo test 2>&1", "failed", FAIL),
        ];
        let stuck = detect(&entries, &[]).unwrap();
        assert_eq!(
            stuck.message(),
            "“cargo test” failed 3 times with the same output"
        );
        assert!(stuck.prompt().contains("`cargo test` failed 3 times"));
        assert!(detect(&entries[..4], &[]).is_none());
        assert!(detect(&entries, &[stuck.key]).is_none());
    }

    #[test]
    fn changing_failures_and_successes_are_progress() {
        let entries = [
            run(1, "cargo test", "failed", "error: a"),
            run(2, "cargo test", "failed", "error: b"),
            run(3, "cargo test", "failed", "error: b"),
        ];
        assert!(detect(&entries, &[]).is_none());
        let entries = [
            run(1, "cargo test", "failed", FAIL),
            run(2, "cargo test", "failed", FAIL),
            run(3, "cargo test", "completed", "ok"),
            run(4, "cargo test", "failed", FAIL),
        ];
        assert!(detect(&entries, &[]).is_none());
    }

    #[test]
    fn droid_exit_codes_in_the_output_count_as_failures() {
        let out = "Command failed (exit code: 101)\nerror[E0308]: mismatched types\n\n[Process exited with code 101]";
        let entries: Vec<_> = (0..3)
            .map(|i| run(i, "cargo build", "completed", out))
            .collect();
        assert_eq!(detect(&entries, &[]).unwrap().pattern, Pattern::Failing);
        let ok = "Command completed successfully\n\n[Process exited with code 0]";
        let entries: Vec<_> = (0..3)
            .map(|i| run(i, "cargo build", "completed", ok))
            .collect();
        assert!(detect(&entries, &[]).is_none());
    }

    #[test]
    fn identical_calls_back_to_back_repeat() {
        let read = |i, out| tool(i, "read", json!({"file_path": "/p/a.rs"}), "completed", out);
        let entries: Vec<_> = (0..4).map(|i| read(i, "fn main() {}")).collect();
        let stuck = detect(&entries, &[]).unwrap();
        assert_eq!(
            stuck.message(),
            "“Read a.rs” ran 4 times in a row with the same result"
        );
        assert!(detect(&entries[..3], &[]).is_none());
        let mut changing = entries.clone();
        changing[2] = read(2, "fn main() { run() }");
        assert!(detect(&changing, &[]).is_none());
    }

    #[test]
    fn reapplying_an_undone_edit_is_reverting() {
        let entries = [edit(1, "a", "b"), edit(2, "b", "a"), edit(3, "a", "b")];
        let stuck = detect(&entries, &[]).unwrap();
        assert_eq!(
            stuck.message(),
            "The edits to “a.rs” keep undoing each other"
        );
        assert!(detect(&entries[..2], &[]).is_none());
    }

    #[test]
    fn a_pattern_that_stops_recurring_goes_stale() {
        let mut entries: Vec<_> = (0..3)
            .map(|i| run(i, "cargo test", "failed", FAIL))
            .collect();
        for i in 0..STALE - 1 {
            entries.push(run(10 + i, &format!("ls {i}"), "completed", ""));
        }
        assert!(detect(&entries, &[]).is_some());
        entries.push(run(99, "ls x", "completed", ""));
        assert!(detect(&entries, &[]).is_none());
    }

    #[test]
    fn commands_normalize_across_shapes() {
        assert_eq!(normalize("cd /p/x && cd sub &&  npm test 2>&1"), "npm test");
        let codex: Entry = tool(
            1,
            "execute",
            json!({"command": ["/bin/bash", "-lc", "npm  test"]}),
            "failed",
            "",
        );
        let Entry::Tool(t) = codex else {
            unreachable!()
        };
        assert_eq!(command(&t).as_deref(), Some("npm test"));
        assert_eq!(reported_exit("x\n[Process exited with code 2]"), Some(2));
        assert_eq!(reported_exit("all good"), None);
    }
}
