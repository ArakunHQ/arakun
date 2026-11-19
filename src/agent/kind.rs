use gpui::{AnyElement, IntoElement, Styled as _, img, px};
use serde::{Deserialize, Serialize};

use crate::ui;

/// Which agent CLI backs a thread. Mirrors `model::Agent`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AgentKind {
    Claude,
    Droid,
    OpenCode,
    /// OpenAI's Codex CLI, signed in with ChatGPT or an API key.
    Codex,
    /// Cognition's Devin CLI, which speaks ACP itself (`devin acp`).
    Devin,
    /// Cloudflare Workers AI, run by arakun itself (`workers_ai.rs`).
    #[serde(rename = "workers-ai")]
    WorkersAi,
}

impl AgentKind {
    /// The agent CLIs whose plans the sidebar's usage card can show. Codex is
    /// left out until its ChatGPT limits are read.
    pub const ALL: [AgentKind; 4] = [
        AgentKind::Claude,
        AgentKind::Droid,
        AgentKind::OpenCode,
        AgentKind::Devin,
    ];

    pub fn label(self) -> &'static str {
        match self {
            AgentKind::Claude => "Claude Code",
            AgentKind::Droid => "Droid",
            AgentKind::OpenCode => "OpenCode",
            AgentKind::Codex => "Codex",
            AgentKind::Devin => "Devin",
            AgentKind::WorkersAi => "Workers AI",
        }
    }

    /// Stable lowercase id, used for log file names and persistence.
    pub fn id(self) -> &'static str {
        match self {
            AgentKind::Claude => "claude",
            AgentKind::Droid => "droid",
            AgentKind::OpenCode => "opencode",
            AgentKind::Codex => "codex",
            AgentKind::Devin => "devin",
            AgentKind::WorkersAi => "workers-ai",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s.to_ascii_lowercase().as_str() {
            "claude" | "claude-code" | "claude code" => Some(AgentKind::Claude),
            "droid" | "factory" => Some(AgentKind::Droid),
            "opencode" | "open-code" => Some(AgentKind::OpenCode),
            "codex" | "chatgpt" | "openai" => Some(AgentKind::Codex),
            "devin" | "cognition" => Some(AgentKind::Devin),
            "workers-ai" | "workersai" | "workers ai" => Some(AgentKind::WorkersAi),
            _ => None,
        }
    }

    /// What the user should do when the agent says it is not authenticated.
    pub fn login_hint(self) -> &'static str {
        match self {
            AgentKind::Claude => "Run `claude` in a terminal and sign in with /login, then retry.",
            AgentKind::Droid => "Run `droid` in a terminal and sign in, then retry.",
            AgentKind::OpenCode => {
                "Run `opencode auth login` in a terminal, pick OpenCode Go (or another provider) and paste its API key, then retry."
            }
            AgentKind::Codex => {
                "Run `codex login` in a terminal and sign in with ChatGPT \
                 (or set CODEX_API_KEY or OPENAI_API_KEY), then retry."
            }
            AgentKind::Devin => {
                "Run `devin auth login` in a terminal and sign in in the browser \
                 (or set WINDSURF_API_KEY), then retry."
            }
            AgentKind::WorkersAi => {
                "Run `opencode auth login`, pick Cloudflare Workers AI and paste an API token with Workers AI access \
                 (or set CLOUDFLARE_API_TOKEN and CLOUDFLARE_ACCOUNT_ID), then retry."
            }
        }
    }

    /// The same logos the shell draws: Claude's starburst, Factory's mark for Droid,
    /// OpenCode's, Codex's and Devin's own; Workers AI gets the Cloudflare tab's globe.
    pub fn icon(self, size: f32) -> AnyElement {
        match self {
            AgentKind::Claude => img("providers/claude.svg")
                .size(px(size))
                .flex_none()
                .into_any_element(),
            AgentKind::Droid => ui::factory_logo(size).into_any_element(),
            AgentKind::OpenCode => ui::opencode_logo(size).into_any_element(),
            AgentKind::Codex => ui::codex_logo(size).into_any_element(),
            AgentKind::Devin => ui::devin_logo(size).into_any_element(),
            AgentKind::WorkersAi => ui::icon("internet", size)
                .text_color(crate::theme::content())
                .into_any_element(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_kind_parses_back_from_its_id() {
        for kind in [
            AgentKind::Claude,
            AgentKind::Droid,
            AgentKind::OpenCode,
            AgentKind::Codex,
            AgentKind::Devin,
            AgentKind::WorkersAi,
        ] {
            assert_eq!(AgentKind::parse(kind.id()), Some(kind));
            assert_eq!(AgentKind::parse(kind.label()), Some(kind));
            // The id is the serde name, so stored threads keep their agent.
            assert_eq!(
                serde_json::to_value(kind).unwrap(),
                serde_json::json!(kind.id())
            );
        }
        assert_eq!(AgentKind::parse("Devin"), Some(AgentKind::Devin));
    }
}
