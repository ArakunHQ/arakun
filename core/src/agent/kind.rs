use gpui::{AnyElement, IntoElement, Styled as _, img, px};
use serde::{Deserialize, Serialize};

use crate::ui;

/// Which agent backs a session. What Arakun knows about each one is in its
/// [`AgentSpec`]: read a field of [`AgentKind::spec`] before adding a `match`.
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
    /// The Cursor CLI (`cursor-agent`), which speaks ACP itself (`cursor-agent acp`).
    Cursor,
    /// Google's Antigravity CLI (`agy`), which speaks ACP itself (`agy acp`).
    Antigravity,
    /// Cloudflare Workers AI, run by arakun itself (`workers_ai.rs`).
    #[serde(rename = "workers-ai")]
    WorkersAi,
}

/// Everything Arakun knows about one agent that is data, not code. Code that
/// has to run something (find the CLI, start it) stays in `launch.rs` and
/// reads this.
#[derive(Debug)]
pub struct AgentSpec {
    /// The name the interface shows.
    pub label: &'static str,
    /// Stable lowercase id: the serde name, log file names and stored files.
    pub id: &'static str,
    /// Other names [`AgentKind::parse`] takes, lowercase.
    pub aliases: &'static [&'static str],
    /// The program the user installs. Workers AI signs in through OpenCode.
    pub cli: &'static str,
    /// Where to get [`AgentSpec::cli`].
    pub install_url: &'static str,
    /// The arguments after the CLI that sign in, in a terminal.
    pub login: Option<&'static [&'static str]>,
    /// The arguments after the CLI that sign out.
    pub logout: Option<&'static [&'static str]>,
    /// What to do when the agent says it is not authenticated.
    pub login_hint: &'static str,
    /// The hosts the CLI's sign-in may be sent to (`cli_login::check`).
    pub sign_in_hosts: &'static [&'static str],
    /// Why there is no Arakun account, under the agent's Sign in button. It
    /// says what that button runs.
    pub sign_in_notice: &'static str,
    /// The pinned npm package of the ACP adapter it runs through, if it does
    /// not speak ACP itself.
    pub adapter: Option<&'static str>,
    /// The variable whose command starts the agent instead.
    pub override_var: Option<&'static str>,
    /// Whether the sidebar's usage card can show its plan's limits. Codex,
    /// Cursor and Antigravity wait until their plans' limits are read.
    pub plan: bool,
}

const CLAUDE: AgentSpec = AgentSpec {
    label: "Claude Code",
    id: "claude",
    aliases: &["claude-code", "claude code"],
    cli: "Claude Code",
    install_url: "https://code.claude.com/docs/en/setup",
    login: Some(&["/login"]),
    logout: Some(&["/logout"]),
    login_hint: "Run `claude` in a terminal and sign in with /login, then retry.",
    sign_in_hosts: &["api.anthropic.com"],
    sign_in_notice: "Arakun has no account of its own and never asks you to approve access. Sign in runs Claude Code's own login. Arakun only reads what Claude Code saves, sends it only to api.anthropic.com, and never changes it.",
    adapter: Some(super::launch::CLAUDE_ACP_PACKAGE),
    override_var: Some("ARAKUN_CLAUDE_ACP"),
    plan: true,
};

const DROID: AgentSpec = AgentSpec {
    label: "Droid",
    id: "droid",
    aliases: &["factory"],
    cli: "Droid",
    install_url: "https://docs.factory.com/cli/getting-started/quickstart",
    // Plain `droid` asks to sign in.
    login: Some(&[]),
    // Droid is signed out from Factory's side.
    logout: None,
    login_hint: "Run `droid` in a terminal and sign in, then retry.",
    sign_in_hosts: &["api.factory.ai"],
    sign_in_notice: "Arakun has no account of its own and never asks you to approve access. It reads the login droid already has and never changes it; droid renews it itself. A Factory API key you connect here, or FACTORY_API_KEY, is used instead; a connected key is kept in the system keyring. Either one goes only to api.factory.ai.",
    adapter: None,
    override_var: Some("ARAKUN_DROID"),
    plan: true,
};

/// OpenCode's sign-in serves OpenCode and Workers AI alike.
const OPENCODE_NOTICE: &str = "Arakun has no account of its own and never asks you to approve access. Sign in and Connect run OpenCode's own login. Arakun only reads the keys OpenCode saves, sends the Go key only to opencode.ai and the Workers AI key only to api.cloudflare.com, and never changes them.";

const OPENCODE: AgentSpec = AgentSpec {
    label: "OpenCode",
    id: "opencode",
    aliases: &["open-code"],
    cli: "OpenCode",
    install_url: "https://opencode.ai",
    // Asks for a provider and its key.
    login: Some(&["auth", "login"]),
    // Asks which provider.
    logout: Some(&["auth", "logout"]),
    login_hint: "Run `opencode auth login` in a terminal, pick OpenCode Go (or another provider) and paste its API key, then retry.",
    sign_in_hosts: &["opencode.ai"],
    sign_in_notice: OPENCODE_NOTICE,
    adapter: None,
    override_var: Some("ARAKUN_OPENCODE_ACP"),
    plan: true,
};

const CODEX: AgentSpec = AgentSpec {
    label: "Codex",
    id: "codex",
    aliases: &["chatgpt", "openai"],
    cli: "Codex",
    install_url: "https://github.com/openai/codex",
    // Signs in with ChatGPT in the browser.
    login: Some(&["login"]),
    logout: Some(&["logout"]),
    login_hint: "Run `codex login` in a terminal and sign in with ChatGPT \
                 (or set CODEX_API_KEY or OPENAI_API_KEY), then retry.",
    // Arakun reads only how Codex is signed in, never its tokens.
    sign_in_hosts: &[],
    sign_in_notice: "Arakun has no account of its own and never asks you to approve access. Sign in runs the codex CLI's own login. Arakun only checks which kind of sign-in Codex has and never reads the tokens.",
    adapter: Some(super::launch::CODEX_ACP_PACKAGE),
    override_var: Some("ARAKUN_CODEX_ACP"),
    plan: false,
};

const DEVIN: AgentSpec = AgentSpec {
    label: "Devin",
    id: "devin",
    aliases: &["cognition"],
    cli: "Devin",
    install_url: "https://docs.devin.ai/work-with-devin/devin-cli",
    login: Some(&["auth", "login"]),
    logout: Some(&["auth", "logout"]),
    login_hint: "Run `devin auth login` in a terminal and sign in in the browser \
                 (or set WINDSURF_API_KEY), then retry.",
    // The server `devin auth login` signs in with; `devin_plan` reads the quota there.
    sign_in_hosts: &["server.codeium.com"],
    sign_in_notice: "Arakun has no account of its own and never asks you to approve access. Sign in runs the devin CLI's own login. Arakun reads the key Devin saves only to show your plan's quota, sends it only to server.codeium.com, and never changes it.",
    adapter: None,
    override_var: Some("ARAKUN_DEVIN_ACP"),
    plan: true,
};

const CURSOR: AgentSpec = AgentSpec {
    label: "Cursor",
    id: "cursor",
    aliases: &["cursor-agent", "cursor agent"],
    cli: "Cursor",
    install_url: "https://cursor.com/docs/cli/installation",
    login: Some(&["login"]),
    logout: Some(&["logout"]),
    login_hint: "Run `cursor-agent login` in a terminal and sign in in the browser \
                 (or set CURSOR_API_KEY), then retry.",
    // Arakun checks only whether Cursor is signed in.
    sign_in_hosts: &[],
    sign_in_notice: "Arakun has no account of its own and never asks you to approve access. Sign in runs the cursor-agent CLI's own login. Arakun only checks that Cursor has a sign-in and never reads the tokens.",
    adapter: None,
    override_var: Some("ARAKUN_CURSOR_ACP"),
    plan: false,
};

const ANTIGRAVITY: AgentSpec = AgentSpec {
    label: "Antigravity",
    id: "antigravity",
    aliases: &["agy", "google-antigravity"],
    cli: "Antigravity",
    install_url: "https://antigravity.google/docs/install?tab=cli",
    // `agy acp --login` rather than plain `agy`: it only signs in and exits,
    // and works without a browser on this computer.
    login: Some(&["acp", "--login"]),
    // agy signs out from inside its own session (`/logout`).
    logout: None,
    login_hint: "Run `agy acp --login` in a terminal and sign in with Google \
                 (or set GEMINI_API_KEY and pick the Gemini API in agy), then retry.",
    // Arakun checks only whether agy's sign-in file is there.
    sign_in_hosts: &[],
    sign_in_notice: "Arakun has no account of its own and never asks you to approve access. Sign in runs the agy CLI's own login. Arakun only checks that agy's sign-in file is there and never reads it.",
    adapter: None,
    override_var: Some("ARAKUN_ANTIGRAVITY_ACP"),
    plan: false,
};

const WORKERS_AI: AgentSpec = AgentSpec {
    label: "Workers AI",
    id: "workers-ai",
    aliases: &["workersai", "workers ai"],
    cli: "OpenCode",
    install_url: "https://opencode.ai",
    login: Some(&["auth", "login"]),
    logout: None,
    login_hint: "Run `opencode auth login`, pick Cloudflare Workers AI and paste an API token with Workers AI access \
                 (or set CLOUDFLARE_API_TOKEN and CLOUDFLARE_ACCOUNT_ID), then retry.",
    sign_in_hosts: &["api.cloudflare.com"],
    sign_in_notice: OPENCODE_NOTICE,
    adapter: None,
    // Nothing to start: it runs in arakun (`Connection::in_process`).
    override_var: None,
    plan: false,
};

impl AgentKind {
    /// Every agent, in the order pickers show them.
    pub const ALL: [AgentKind; 8] = [
        AgentKind::Claude,
        AgentKind::Droid,
        AgentKind::OpenCode,
        AgentKind::Codex,
        AgentKind::Devin,
        AgentKind::Cursor,
        AgentKind::Antigravity,
        AgentKind::WorkersAi,
    ];

    /// The agents whose plans the sidebar's usage card can show ([`AgentSpec::plan`]).
    pub const WITH_PLAN: [AgentKind; Self::count_with_plan()] = Self::with_plan();

    pub const fn spec(self) -> &'static AgentSpec {
        match self {
            AgentKind::Claude => &CLAUDE,
            AgentKind::Droid => &DROID,
            AgentKind::OpenCode => &OPENCODE,
            AgentKind::Codex => &CODEX,
            AgentKind::Devin => &DEVIN,
            AgentKind::Cursor => &CURSOR,
            AgentKind::Antigravity => &ANTIGRAVITY,
            AgentKind::WorkersAi => &WORKERS_AI,
        }
    }

    const fn count_with_plan() -> usize {
        let (mut n, mut i) = (0, 0);
        while i < Self::ALL.len() {
            if Self::ALL[i].spec().plan {
                n += 1;
            }
            i += 1;
        }
        n
    }

    const fn with_plan() -> [AgentKind; Self::count_with_plan()] {
        let mut out = [AgentKind::Claude; Self::count_with_plan()];
        let (mut n, mut i) = (0, 0);
        while i < Self::ALL.len() {
            if Self::ALL[i].spec().plan {
                out[n] = Self::ALL[i];
                n += 1;
            }
            i += 1;
        }
        out
    }

    pub fn label(self) -> &'static str {
        self.spec().label
    }

    /// Stable lowercase id, used for log file names and persistence.
    pub fn id(self) -> &'static str {
        self.spec().id
    }

    pub fn parse(s: &str) -> Option<Self> {
        let s = s.to_ascii_lowercase();
        Self::ALL.into_iter().find(|kind| {
            let spec = kind.spec();
            spec.id == s || spec.label.eq_ignore_ascii_case(&s) || spec.aliases.contains(&&*s)
        })
    }

    /// The agent a stored id names. Unknown ids (and old files' missing ones)
    /// read as Droid.
    pub fn from_id(id: &str) -> Self {
        Self::ALL
            .into_iter()
            .find(|kind| kind.id() == id)
            .unwrap_or(AgentKind::Droid)
    }

    /// Where to get the agent's CLI. Workers AI runs through OpenCode's sign-in.
    pub fn install_url(self) -> &'static str {
        self.spec().install_url
    }

    /// What to say when the agent's CLI is not on this computer: what is
    /// missing and where to get it, the same wherever it is noticed.
    pub fn missing(self) -> String {
        let spec = self.spec();
        format!(
            "{} was not found. Install it from {}, then try again.",
            spec.cli, spec.install_url
        )
    }

    /// What the user should do when the agent says it is not authenticated.
    pub fn login_hint(self) -> &'static str {
        self.spec().login_hint
    }

    /// The same logos the shell draws: Claude's starburst, Factory's mark for Droid,
    /// OpenCode's, Codex's, Devin's, Cursor's and Antigravity's own; Workers AI gets the Cloudflare tab's globe.
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
            AgentKind::Cursor => ui::cursor_logo(size).into_any_element(),
            AgentKind::Antigravity => ui::antigravity_logo(size).into_any_element(),
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
        for kind in AgentKind::ALL {
            assert_eq!(AgentKind::parse(kind.id()), Some(kind));
            assert_eq!(AgentKind::parse(kind.label()), Some(kind));
            assert_eq!(AgentKind::from_id(kind.id()), kind);
            // The id is the serde name, so stored threads keep their agent.
            assert_eq!(
                serde_json::to_value(kind).unwrap(),
                serde_json::json!(kind.id())
            );
        }
        assert_eq!(AgentKind::parse("Devin"), Some(AgentKind::Devin));
        assert_eq!(AgentKind::parse("cursor-agent"), Some(AgentKind::Cursor));
        assert_eq!(AgentKind::parse("agy"), Some(AgentKind::Antigravity));
        assert_eq!(AgentKind::parse("ChatGPT"), Some(AgentKind::Codex));
        assert_eq!(AgentKind::parse("nobody"), None);
        assert_eq!(AgentKind::from_id("nobody"), AgentKind::Droid);
        assert_eq!(AgentKind::from_id(""), AgentKind::Droid);
    }

    #[test]
    fn with_plan_follows_the_table() {
        assert_eq!(
            AgentKind::WITH_PLAN,
            [
                AgentKind::Claude,
                AgentKind::Droid,
                AgentKind::OpenCode,
                AgentKind::Devin
            ]
        );
    }

    #[test]
    fn only_agents_with_an_adapter_need_npm() {
        for kind in AgentKind::ALL {
            let npm = kind.spec().adapter.is_some();
            assert_eq!(
                npm,
                matches!(kind, AgentKind::Claude | AgentKind::Codex),
                "{kind:?}"
            );
        }
    }
}
