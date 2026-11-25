//! What a notice says: short plain text built from the session, so a phone
//! shows the answer or the question and not a fixed line.

use crate::agent::{AskUserRequest, Entry, PermissionRequest, ToolCall};

/// What the notice is about. The apps read it as `kind`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Done,
    Input,
    Error,
}

impl Kind {
    pub fn as_str(self) -> &'static str {
        match self {
            Kind::Done => "done",
            Kind::Input => "input",
            Kind::Error => "error",
        }
    }
}

const FALLBACK: &str = "Finished its turn.";
/// Lines of a plan a notice shows.
const PLAN_LINES: usize = 3;

/// The last thing the agent said, as plain text. `push::notice` cuts it to fit.
pub fn finished_body(entries: &[Entry]) -> String {
    let text = entries.iter().rev().find_map(|e| match e {
        Entry::Agent { text } => Some(plain(text)),
        _ => None,
    });
    match text {
        Some(t) if !t.is_empty() => t,
        _ => FALLBACK.to_string(),
    }
}

/// What the agent asks leave for: the plan, or the tool and what it works on.
pub fn permission_body(p: &PermissionRequest, tool: Option<&ToolCall>) -> String {
    if let Some(plan) = &p.plan {
        let lines: Vec<&str> = plan
            .plan
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .take(PLAN_LINES)
            .collect();
        return format!("Plan ready: {}", lines.join(" / "));
    }
    let title = tool
        .map(|t| t.title.as_str())
        .filter(|t| !t.is_empty())
        .unwrap_or(&p.title);
    match tool.and_then(subject) {
        Some(s) => format!("{title}: {s}"),
        None => title.to_string(),
    }
}

/// The first question, and how many more there are.
pub fn question_body(q: &AskUserRequest) -> String {
    let Some(first) = q.questions.first() else {
        return String::new();
    };
    let text = plain(&first.question);
    match q.questions.len() - 1 {
        0 => text,
        more => format!("{text} (+{more} more)"),
    }
}

/// How long the last turn took, in whole seconds, once it has settled.
pub fn turn_secs(entries: &[Entry]) -> Option<u64> {
    entries.iter().rev().find_map(|e| match e {
        Entry::User { elapsed_ms, .. } => elapsed_ms.map(|ms| ms / 1000),
        _ => None,
    })
}

/// The command, address or file a tool call works on.
fn subject(t: &ToolCall) -> Option<String> {
    let input = |key: &str| {
        t.raw_input
            .as_ref()?
            .get(key)?
            .as_str()
            .filter(|s| !s.is_empty())
            .map(str::to_string)
    };
    match t.kind.as_str() {
        "execute" => input("command"),
        "fetch" => input("url"),
        _ => None,
    }
    .or_else(|| t.locations.first().map(|p| p.display().to_string()))
    .or_else(|| input("file_path"))
    .or_else(|| input("path"))
}

/// Markdown as one line of plain text: no code blocks, links as their text,
/// no emphasis marks.
fn plain(md: &str) -> String {
    let mut kept = String::new();
    let mut fenced = false;
    for line in md.lines() {
        if line.trim_start().starts_with("```") {
            fenced = !fenced;
        } else if !fenced {
            kept.push_str(line);
            kept.push('\n');
        }
    }
    let mut out = String::new();
    let mut rest = kept.as_str();
    while let Some(open) = rest.find('[') {
        out.push_str(&rest[..open]);
        let after = &rest[open + 1..];
        let link = after
            .find("](")
            .filter(|&i| !after[..i].contains(['[', '\n']))
            .and_then(|i| after[i + 2..].find(')').map(|j| (i, i + 2 + j)));
        match link {
            Some((text_end, url_end)) => {
                out.push_str(&after[..text_end]);
                rest = &after[url_end + 1..];
            }
            None => {
                out.push('[');
                rest = after;
            }
        }
    }
    out.push_str(rest);
    let out: String = out
        .chars()
        .map(|c| {
            if matches!(c, '#' | '*' | '_' | '`') {
                ' '
            } else {
                c
            }
        })
        .collect();
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::{Question, SpecPlan, ToolStatus};
    use crate::phone::push;

    fn user(elapsed_ms: Option<u64>) -> Entry {
        Entry::User {
            text: "go".into(),
            at: 0,
            elapsed_ms,
            message_id: None,
            tokens: None,
            shell_changes: Vec::new(),
        }
    }

    fn agent(text: &str) -> Entry {
        Entry::Agent { text: text.into() }
    }

    fn call(kind: &str, title: &str) -> ToolCall {
        ToolCall {
            id: "t".into(),
            title: title.into(),
            kind: kind.into(),
            status: ToolStatus::Pending,
            content: Vec::new(),
            locations: Vec::new(),
            raw_input: None,
            raw_output: None,
            added: 0,
            removed: 0,
            terminal_output: String::new(),
            terminal_dropped_lines: 0,
            exit_code: None,
            parent: None,
            tool_name: None,
        }
    }

    fn permission(title: &str, plan: Option<SpecPlan>) -> PermissionRequest {
        PermissionRequest::for_test(title, plan)
    }

    #[test]
    fn finished_body_takes_last_agent_text() {
        let entries = [
            user(None),
            agent("**Fixed** the `login` bug.\n\nSee [PR](http://x)."),
        ];
        assert_eq!(finished_body(&entries), "Fixed the login bug. See PR.");
    }

    #[test]
    fn finished_body_falls_back_and_fits() {
        assert_eq!(finished_body(&[user(None)]), "Finished its turn.");
        assert_eq!(
            finished_body(&[agent("```\nlet x = 1;\n```")]),
            "Finished its turn."
        );
        let body = finished_body(&[agent(&"é".repeat(5000))]);
        let out = push::notice("done", "s", "t", &body, "", None);
        assert!(out.len() <= push::MAX_NOTICE);
    }

    #[test]
    fn permission_body_shows_subject() {
        let p = permission("Run", None);
        let mut t = call("execute", "Run");
        t.raw_input = Some(serde_json::json!({"command": "cargo test"}));
        assert_eq!(permission_body(&p, Some(&t)), "Run: cargo test");

        let mut t = call("edit", "Edit file");
        t.locations = vec!["/a/b.rs".into()];
        assert_eq!(permission_body(&p, Some(&t)), "Edit file: /a/b.rs");

        let mut t = call("fetch", "Fetch");
        t.raw_input = Some(serde_json::json!({"url": "https://x"}));
        assert_eq!(permission_body(&p, Some(&t)), "Fetch: https://x");

        assert_eq!(permission_body(&p, None), "Run");

        let plan = SpecPlan {
            title: "Plan".into(),
            plan: "\n# One\n\ntwo\nthree\nfour\n".into(),
        };
        let p = permission("Exit spec mode", Some(plan));
        assert_eq!(permission_body(&p, None), "Plan ready: # One / two / three");
    }

    #[test]
    fn question_body_first_and_more() {
        let q = |text: &str| Question {
            index: 1,
            topic: String::new(),
            question: text.into(),
            options: Vec::new(),
            multi: false,
        };
        let req = AskUserRequest::for_test(vec![q("Which database?"), q("Which port?")]);
        assert_eq!(question_body(&req), "Which database? (+1 more)");
    }

    #[test]
    fn turn_secs_from_last_user() {
        let entries = [user(Some(1000)), agent("x"), user(Some(65_400))];
        assert_eq!(turn_secs(&entries), Some(65));
        assert_eq!(turn_secs(&[agent("x")]), None);
    }
}
