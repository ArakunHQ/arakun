//! Reads of secret files by an agent: `.env`, `~/.aws/credentials`, a key.
//! What the agent reads goes to its model and shows in the chat and on the
//! phone, so Arakun asks first, also when auto-allow is on and when the
//! sandbox is off, and hides the output until the user shows it.
//!
//! Which files count is [`crate::secrets::asks_to_read`]. Claude Code reads
//! files itself, not through ACP's `fs/read_text_file`, so it gets permission
//! rules that make it ask ([`claude_settings`]).

use std::path::Path;

use serde_json::{Value, json};

use super::thread::{PermissionOption, ToolCall};

/// The id of the option that lets the agent read secret files for the rest
/// of the session. Arakun adds it; the agent never sees it.
pub const SESSION: &str = "_arakun/secret-files-session";

/// "Allow for this session", added to a secret file's card.
pub fn session_option() -> PermissionOption {
    PermissionOption {
        id: SESSION.into(),
        name: "Allow secret files this session".into(),
        kind: "allow_always".into(),
    }
}

/// The options of the card Arakun shows before it answers an
/// `fs/read_text_file` of a secret file.
pub fn fs_read_options() -> Vec<PermissionOption> {
    vec![
        PermissionOption {
            id: "allow".into(),
            name: "Allow".into(),
            kind: "allow_once".into(),
        },
        session_option(),
        PermissionOption {
            id: "reject".into(),
            name: "Deny".into(),
            kind: "reject_once".into(),
        },
    ]
}

/// What Arakun knows when an agent asks to run a tool call.
#[derive(Clone, Copy, Debug)]
pub struct Ask {
    /// The call reads a secret file.
    pub secret: bool,
    /// The call runs a command, which may do more than read.
    pub command: bool,
    /// The session answers for itself (`arakun agent new`).
    pub auto_allow: bool,
    /// The user let the agent read secret files this session.
    pub secrets_allowed: bool,
}

impl Ask {
    /// Whether Arakun allows the call without a card. Auto-allow doesn't
    /// cover a secret file: what it holds would go to the agent unseen. A
    /// read the user allowed for the session goes through; a command is
    /// still asked about.
    pub fn answers_itself(self) -> bool {
        if self.secret {
            self.secrets_allowed && !self.command
        } else {
            self.auto_allow
        }
    }

    /// Whether the card offers [`session_option`].
    pub fn offers_session(self) -> bool {
        self.secret && !self.answers_itself()
    }
}

/// What a secret file's card says under its question: which file, and why
/// it matters.
pub fn why(file: &str) -> String {
    format!(
        "{file} looks like it holds secrets. What the agent reads goes to its model, \
         and shows in this chat and on your phone."
    )
}

/// Settings Claude Code takes on top of the user's own: ask before the Read
/// tool, and the commands Claude checks the same way, read a secret file.
pub fn claude_settings() -> Value {
    json!({"permissions": {"ask": crate::secrets::claude_ask_rules()}})
}

/// The secret file a tool call reads: a command that prints one, or a read
/// tool (Claude's Read, an ACP `read` call) given one. As the agent wrote it.
pub fn tool_reads(t: &ToolCall) -> Option<String> {
    reads(
        &t.kind,
        t.raw_input.as_ref(),
        t.locations.first().map(|p| p.as_path()),
    )
}

/// [`tool_reads`] for the `toolCall` of a `session/request_permission`.
pub fn request_reads(tool: &Value) -> Option<String> {
    let location = tool["locations"][0]["path"].as_str().map(Path::new);
    let kind = tool["kind"].as_str().unwrap_or("other");
    reads(kind, tool.get("rawInput"), location)
}

fn reads(kind: &str, input: Option<&Value>, location: Option<&Path>) -> Option<String> {
    let input = input.filter(|i| i.is_object());
    if let Some(command) = input.and_then(|i| i["command"].as_str()) {
        return crate::secrets::command_reads(command);
    }
    // Read, NotebookRead and Grep name a file; an edit's diff would show it too,
    // but that is the file's content the user asked to change.
    if !matches!(kind, "read" | "search") {
        return None;
    }
    let named = input.and_then(|i| {
        ["file_path", "path", "notebook_path"]
            .iter()
            .find_map(|k| i[k].as_str())
    });
    let path = named.map(Path::new).or(location)?;
    crate::secrets::asks_to_read(path).then(|| path.display().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_read_or_a_command_of_a_secret_file_is_found() {
        let read = json!({"kind": "read", "rawInput": {"file_path": "/p/.env"}});
        assert_eq!(request_reads(&read).as_deref(), Some("/p/.env"));
        let located = json!({"kind": "read", "locations": [{"path": "/p/.env.local"}]});
        assert_eq!(request_reads(&located).as_deref(), Some("/p/.env.local"));
        let bash = json!({"kind": "execute", "rawInput": {"command": "cat .env | grep KEY"}});
        assert_eq!(request_reads(&bash).as_deref(), Some(".env"));
        for plain in [
            json!({"kind": "read", "rawInput": {"file_path": "/p/src/main.rs"}}),
            json!({"kind": "read", "rawInput": {"file_path": "/p/.env.example"}}),
            json!({"kind": "execute", "rawInput": {"command": "cargo test"}}),
            json!({"kind": "edit", "rawInput": {"file_path": "/p/.env"}}),
            json!({"kind": "read"}),
        ] {
            assert_eq!(request_reads(&plain), None, "{plain}");
        }
    }

    #[test]
    fn auto_allow_never_answers_for_a_secret_file() {
        let ask = |secret, command, auto_allow, secrets_allowed| Ask {
            secret,
            command,
            auto_allow,
            secrets_allowed,
        };
        // Plain calls: as auto-allow says.
        assert!(ask(false, true, true, false).answers_itself());
        assert!(!ask(false, false, false, true).answers_itself());
        // A secret file: always a card, with "this session" on it.
        let secret = ask(true, false, true, false);
        assert!(!secret.answers_itself());
        assert!(secret.offers_session());
        // Allowed for the session: reads go through, commands still ask.
        assert!(ask(true, false, false, true).answers_itself());
        let command = ask(true, true, true, true);
        assert!(!command.answers_itself());
        assert!(command.offers_session());
    }

    #[test]
    fn the_session_option_allows_and_claude_gets_ask_rules() {
        assert!(session_option().is_allow());
        assert!(fs_read_options().iter().any(|o| o.id == SESSION));
        let rules = &claude_settings()["permissions"]["ask"];
        assert!(
            rules
                .as_array()
                .is_some_and(|r| r.iter().any(|r| r == "Read(**/.env)"))
        );
    }
}
