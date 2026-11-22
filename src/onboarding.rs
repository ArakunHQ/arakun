//! The setup a new user walks through before the first project: its steps,
//! what it finds on the machine, and the note that it was done. The shell
//! shows it (`shell::onboarding`); nothing here draws.

use serde::{Deserialize, Serialize};

use crate::agent::AgentKind;
use crate::sidebar::View;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Step {
    /// The raccoon and the check of this machine: only the first time.
    Welcome,
    Agents,
    Project,
    Look,
    /// GitHub, the phone and crash reports.
    Connect,
}

impl Step {
    /// Its name in the step bar and in telemetry.
    pub fn label(self) -> &'static str {
        match self {
            Step::Welcome => "Welcome",
            Step::Agents => "Agents",
            Step::Project => "Project",
            Step::Look => "Look",
            Step::Connect => "Connect",
        }
    }

    pub fn title(self) -> &'static str {
        match self {
            Step::Welcome => "Welcome to Arakun",
            Step::Agents => "Which agent starts new sessions?",
            Step::Project => "Where do you work?",
            Step::Look => "Make it yours",
            Step::Connect => "Anything outside this machine?",
        }
    }

    pub fn blurb(self) -> &'static str {
        match self {
            Step::Welcome => "One window for the coding agents you already have.",
            Step::Agents => {
                "Arakun runs the agents you already have. You can switch for any session later."
            }
            Step::Project => {
                "Pick a git repository. Every branch you work on gets its own environment, with its own agents, terminals and browser."
            }
            Step::Look => {
                "The window changes as you pick. Corners, spacing and the rest are in Settings."
            }
            Step::Connect => {
                "All three are optional. Each one can wait, and each one is in Settings later."
            }
        }
    }
}

/// The steps in order. The welcome is for someone new; a setup opened again
/// from the palette starts at the questions.
pub fn steps(fresh: bool) -> Vec<Step> {
    let mut steps = Vec::with_capacity(5);
    if fresh {
        steps.push(Step::Welcome);
    }
    steps.extend([Step::Agents, Step::Project, Step::Look, Step::Connect]);
    steps
}

/// Whether the setup opens by itself at launch: for someone with no project
/// who never finished or skipped it. Whoever has a project is set up.
pub fn opens_at_launch(has_projects: bool, done: bool) -> bool {
    !has_projects && !done
}

// ---------------------------------------------------------------------------
// The note that it was done

#[derive(Serialize, Deserialize, Default)]
struct Saved {
    #[serde(default)]
    done: bool,
}

/// Whether the setup was finished or skipped before.
pub fn done() -> bool {
    std::fs::read(crate::state::onboarding_path())
        .ok()
        .and_then(|b| serde_json::from_slice::<Saved>(&b).ok())
        .is_some_and(|s| s.done)
}

/// Remembers that the setup was finished or skipped. Unsaved, it opens again
/// next launch, which is the lesser harm.
pub fn set_done() {
    let bytes = serde_json::to_vec_pretty(&Saved { done: true }).unwrap_or_default();
    if let Err(e) = crate::durable::write(&crate::state::onboarding_path(), &bytes) {
        eprintln!("arakun: could not save that the setup is done: {e}");
    }
}

// ---------------------------------------------------------------------------
// Agents

/// The agents that need something on the machine, in the order shown.
/// Workers AI is left out: it has no CLI to find.
pub const AGENTS: [AgentKind; 7] = [
    AgentKind::Claude,
    AgentKind::Codex,
    AgentKind::OpenCode,
    AgentKind::Droid,
    AgentKind::Devin,
    AgentKind::Cursor,
    AgentKind::Antigravity,
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Readiness {
    /// A session can start now.
    Ready,
    /// Arakun installs the agent's adapter with npm at the first session,
    /// after asking.
    OnFirstUse,
    Missing,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AgentRow {
    pub agent: AgentKind,
    pub state: Readiness,
}

/// Claude Code and Codex run through an adapter that Arakun installs with npm
/// and that brings the agent's own binary; `has` is that adapter. OpenCode,
/// Droid, Devin, Cursor and Antigravity are the user's own CLIs; `has` is the CLI.
fn readiness(agent: AgentKind, has: bool, npm: bool) -> Readiness {
    let through_npm = matches!(agent, AgentKind::Claude | AgentKind::Codex);
    match (has, through_npm && npm) {
        (true, _) => Readiness::Ready,
        (false, true) => Readiness::OnFirstUse,
        (false, false) => Readiness::Missing,
    }
}

/// Looks on disk for every agent; call it off the UI thread.
pub fn find_agents() -> Vec<AgentRow> {
    let npm = crate::agent::which("npm").is_some();
    AGENTS
        .iter()
        .map(|&agent| {
            let has = match agent {
                AgentKind::Claude => crate::agent::adapter_install(AgentKind::Claude).is_none(),
                AgentKind::Codex => crate::agent::adapter_install(AgentKind::Codex).is_none(),
                AgentKind::OpenCode => crate::agent::opencode_bin().is_ok(),
                AgentKind::Droid => crate::agent::droid_bin().is_ok(),
                AgentKind::Devin => crate::agent::devin_bin().is_ok(),
                AgentKind::Cursor => crate::agent::cursor_bin().is_ok(),
                AgentKind::Antigravity => crate::agent::agy_bin().is_ok(),
                AgentKind::WorkersAi => false,
            };
            AgentRow {
                agent,
                state: readiness(agent, has, npm),
            }
        })
        .collect()
}

/// Where to get what a missing agent needs, and what the link says.
pub fn install_link(agent: AgentKind) -> (&'static str, &'static str) {
    match agent {
        AgentKind::Claude | AgentKind::Codex => ("Needs Node.js for npm", "https://nodejs.org"),
        AgentKind::OpenCode => ("Get OpenCode", AgentKind::OpenCode.install_url()),
        AgentKind::Droid => ("Get Droid", AgentKind::Droid.install_url()),
        AgentKind::Devin => ("Get the Devin CLI", AgentKind::Devin.install_url()),
        AgentKind::Cursor => ("Get the Cursor CLI", AgentKind::Cursor.install_url()),
        AgentKind::Antigravity => (
            "Get the Antigravity CLI",
            AgentKind::Antigravity.install_url(),
        ),
        AgentKind::WorkersAi => ("", ""),
    }
}

/// The agent to start with: `current` when it can run, else the first that
/// is ready, else the first that only needs its adapter. None when nothing
/// can run, so the default stays as it was.
pub fn pick(rows: &[AgentRow], current: AgentKind) -> Option<AgentKind> {
    let usable = |a: AgentKind| {
        rows.iter()
            .any(|r| r.agent == a && r.state != Readiness::Missing)
    };
    if usable(current) {
        return Some(current);
    }
    [Readiness::Ready, Readiness::OnFirstUse]
        .into_iter()
        .find_map(|want| rows.iter().find(|r| r.state == want).map(|r| r.agent))
}

// ---------------------------------------------------------------------------
// GitHub

/// The GitHub tab reads through the GitHub CLI's own sign-in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GitHub {
    Connected,
    SignedOut,
    NoGh,
}

pub const GH_LOGIN: &str = "gh auth login";
pub const GH_URL: &str = "https://cli.github.com";

/// Asks `gh` whether it is signed in. Starts a process; call it off the UI
/// thread.
pub fn find_github() -> GitHub {
    // It checks the token with GitHub, so a stalled network would hold it.
    let status = crate::platform::output_within(
        crate::platform::command("gh").args(["auth", "status"]),
        None,
        crate::platform::GIT_REMOTE,
    )
    .map(|o| o.status);
    match status {
        Ok(s) if s.success() => GitHub::Connected,
        // Offline it fails; one that hangs is taken the same way.
        Ok(_) => GitHub::SignedOut,
        Err(e) if e.kind() == std::io::ErrorKind::TimedOut => GitHub::SignedOut,
        Err(_) => GitHub::NoGh,
    }
}

/// The sidebar tabs a finished setup turns on, for what the user set up in
/// it. A new install starts with every tab that is an extension off, so
/// without this the GitHub they just connected and the phone they chose to
/// pair have no row in the sidebar. A setup opened again leaves GitHub as it
/// is: a returning user may have turned it off on purpose.
pub fn views_set_up(fresh: bool, github: Option<GitHub>, pair_phone: bool) -> Vec<View> {
    let mut on = Vec::new();
    if fresh && github == Some(GitHub::Connected) {
        on.push(View::GitHub);
    }
    if pair_phone {
        on.push(View::Phone);
    }
    on
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(agent: AgentKind, state: Readiness) -> AgentRow {
        AgentRow { agent, state }
    }

    #[test]
    fn the_welcome_is_only_for_someone_new() {
        assert_eq!(
            steps(true),
            [
                Step::Welcome,
                Step::Agents,
                Step::Project,
                Step::Look,
                Step::Connect
            ]
        );
        assert_eq!(
            steps(false),
            [Step::Agents, Step::Project, Step::Look, Step::Connect]
        );
    }

    #[test]
    fn it_opens_only_for_someone_new() {
        assert!(opens_at_launch(false, false));
        // Skipped or finished before.
        assert!(!opens_at_launch(false, true));
        // Used Arakun before the setup existed.
        assert!(!opens_at_launch(true, false));
    }

    #[test]
    fn npm_agents_can_wait_for_their_adapter() {
        assert_eq!(readiness(AgentKind::Claude, true, false), Readiness::Ready);
        assert_eq!(
            readiness(AgentKind::Codex, false, true),
            Readiness::OnFirstUse
        );
        assert_eq!(
            readiness(AgentKind::Claude, false, false),
            Readiness::Missing
        );
        // npm does not bring the user's own CLIs.
        assert_eq!(readiness(AgentKind::Droid, false, true), Readiness::Missing);
        assert_eq!(readiness(AgentKind::Devin, false, true), Readiness::Missing);
        assert_eq!(
            readiness(AgentKind::Cursor, false, true),
            Readiness::Missing
        );
        assert_eq!(
            readiness(AgentKind::OpenCode, true, false),
            Readiness::Ready
        );
    }

    #[test]
    fn the_pick_keeps_a_default_that_can_run() {
        let rows = [
            row(AgentKind::Claude, Readiness::OnFirstUse),
            row(AgentKind::Codex, Readiness::Missing),
            row(AgentKind::OpenCode, Readiness::Ready),
            row(AgentKind::Droid, Readiness::Missing),
        ];
        assert_eq!(pick(&rows, AgentKind::Claude), Some(AgentKind::Claude));
        // Droid, the default until one is picked, is not here: a ready agent
        // goes before one that still needs its adapter.
        assert_eq!(pick(&rows, AgentKind::Droid), Some(AgentKind::OpenCode));
        let waiting = [
            row(AgentKind::Claude, Readiness::OnFirstUse),
            row(AgentKind::Droid, Readiness::Missing),
        ];
        assert_eq!(pick(&waiting, AgentKind::Droid), Some(AgentKind::Claude));
        let none = [row(AgentKind::Droid, Readiness::Missing)];
        assert_eq!(pick(&none, AgentKind::Droid), None);
        // Workers AI needs no CLI, so it is not listed and cannot be kept.
        assert_eq!(pick(&rows, AgentKind::WorkersAi), Some(AgentKind::OpenCode));
    }

    #[test]
    fn the_setup_turns_on_the_tabs_it_set_up() {
        let connected = Some(GitHub::Connected);
        assert_eq!(views_set_up(true, connected, false), [View::GitHub]);
        assert_eq!(
            views_set_up(true, connected, true),
            [View::GitHub, View::Phone]
        );
        // GitHub not signed in, or still being asked.
        assert!(views_set_up(true, Some(GitHub::SignedOut), false).is_empty());
        assert!(views_set_up(true, None, false).is_empty());
        // Opened again: only what was asked for there.
        assert!(views_set_up(false, connected, false).is_empty());
        assert_eq!(views_set_up(false, connected, true), [View::Phone]);
    }
}
