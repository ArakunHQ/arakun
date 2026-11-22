//! The setup a new user walks through before the first project: its steps,
//! what it finds on the machine, and the note that it was done. The shell
//! shows it (`shell::onboarding`); nothing here draws.

use serde::{Deserialize, Serialize};

use crate::agent::AgentKind;
use crate::model::Agent;
use crate::sidebar::View;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Step {
    Agents,
    Look,
    Connections,
    /// Crash reports and usage counts: only while the question is unanswered.
    Reports,
    Project,
}

impl Step {
    /// Its name in the list of steps.
    pub fn label(self) -> &'static str {
        match self {
            Step::Agents => "Agents",
            Step::Look => "Preferences",
            Step::Connections => "Connections",
            Step::Reports => "Crash reports",
            Step::Project => "First project",
        }
    }

    pub fn title(self) -> &'static str {
        match self {
            Step::Agents => "Which agent do you start with?",
            Step::Look => "Make it yours",
            Step::Connections => "Connect the rest",
            Step::Reports => "Send crash reports and usage counts?",
            Step::Project => "Add your first project",
        }
    }

    pub fn blurb(self) -> &'static str {
        match self {
            Step::Agents => {
                "Arakun runs the agents you already have. New environments start with the one you pick, and you can switch for any session."
            }
            Step::Look => {
                "Pick a mode and when old environments go. Color schemes, corners, spacing and the rest are in Settings."
            }
            Step::Connections => "Both are optional, and both can wait.",
            Step::Reports => {
                "They tell us what breaks and what gets used. Nothing is sent unless you say yes."
            }
            Step::Project => {
                "Pick a git repository. Every branch you work on gets its own environment: a git worktree with its own agents, terminals and browser."
            }
        }
    }
}

/// The steps in order. The question about reports is one of them only while
/// it is unanswered, so nobody is asked twice.
pub fn steps(ask_reports: bool) -> Vec<Step> {
    let mut steps = vec![Step::Agents, Step::Look, Step::Connections];
    if ask_reports {
        steps.push(Step::Reports);
    }
    steps.push(Step::Project);
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
pub const AGENTS: [Agent; 6] = [
    Agent::Claude,
    Agent::Codex,
    Agent::OpenCode,
    Agent::Droid,
    Agent::Devin,
    Agent::Antigravity,
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
    pub agent: Agent,
    pub state: Readiness,
}

/// Claude Code and Codex run through an adapter that Arakun installs with npm
/// and that brings the agent's own binary; `has` is that adapter. OpenCode,
/// Droid, Devin and Antigravity are the user's own CLIs; `has` is the CLI.
fn readiness(agent: Agent, has: bool, npm: bool) -> Readiness {
    let through_npm = matches!(agent, Agent::Claude | Agent::Codex);
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
                Agent::Claude => crate::agent::adapter_install(AgentKind::Claude).is_none(),
                Agent::Codex => crate::agent::adapter_install(AgentKind::Codex).is_none(),
                Agent::OpenCode => crate::agent::opencode_bin().is_ok(),
                Agent::Droid => crate::agent::droid_bin().is_ok(),
                Agent::Devin => crate::agent::devin_bin().is_ok(),
                Agent::Antigravity => crate::agent::agy_bin().is_ok(),
                Agent::WorkersAi => false,
            };
            AgentRow {
                agent,
                state: readiness(agent, has, npm),
            }
        })
        .collect()
}

/// Where to get what a missing agent needs, and what the link says.
pub fn install_link(agent: Agent) -> (&'static str, &'static str) {
    match agent {
        Agent::Claude | Agent::Codex => ("Needs Node.js for npm", "https://nodejs.org"),
        Agent::OpenCode => ("Get OpenCode", AgentKind::OpenCode.install_url()),
        Agent::Droid => ("Get Droid", AgentKind::Droid.install_url()),
        Agent::Devin => ("Get the Devin CLI", AgentKind::Devin.install_url()),
        Agent::Antigravity => (
            "Get the Antigravity CLI",
            AgentKind::Antigravity.install_url(),
        ),
        Agent::WorkersAi => ("", ""),
    }
}

/// The agent to start with: `current` when it can run, else the first that
/// is ready, else the first that only needs its adapter. None when nothing
/// can run, so the default stays as it was.
pub fn pick(rows: &[AgentRow], current: Agent) -> Option<Agent> {
    let usable = |a: Agent| {
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

    fn row(agent: Agent, state: Readiness) -> AgentRow {
        AgentRow { agent, state }
    }

    #[test]
    fn reports_are_a_step_only_while_unanswered() {
        assert_eq!(
            steps(true),
            [
                Step::Agents,
                Step::Look,
                Step::Connections,
                Step::Reports,
                Step::Project
            ]
        );
        assert!(!steps(false).contains(&Step::Reports));
        // The project is what the setup is for: always last.
        assert_eq!(steps(false).last(), Some(&Step::Project));
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
        assert_eq!(readiness(Agent::Claude, true, false), Readiness::Ready);
        assert_eq!(readiness(Agent::Codex, false, true), Readiness::OnFirstUse);
        assert_eq!(readiness(Agent::Claude, false, false), Readiness::Missing);
        // npm does not bring the user's own CLIs.
        assert_eq!(readiness(Agent::Droid, false, true), Readiness::Missing);
        assert_eq!(readiness(Agent::Devin, false, true), Readiness::Missing);
        assert_eq!(readiness(Agent::OpenCode, true, false), Readiness::Ready);
    }

    #[test]
    fn the_pick_keeps_a_default_that_can_run() {
        let rows = [
            row(Agent::Claude, Readiness::OnFirstUse),
            row(Agent::Codex, Readiness::Missing),
            row(Agent::OpenCode, Readiness::Ready),
            row(Agent::Droid, Readiness::Missing),
        ];
        assert_eq!(pick(&rows, Agent::Claude), Some(Agent::Claude));
        // Droid, the default until one is picked, is not here: a ready agent
        // goes before one that still needs its adapter.
        assert_eq!(pick(&rows, Agent::Droid), Some(Agent::OpenCode));
        let waiting = [
            row(Agent::Claude, Readiness::OnFirstUse),
            row(Agent::Droid, Readiness::Missing),
        ];
        assert_eq!(pick(&waiting, Agent::Droid), Some(Agent::Claude));
        let none = [row(Agent::Droid, Readiness::Missing)];
        assert_eq!(pick(&none, Agent::Droid), None);
        // Workers AI needs no CLI, so it is not listed and cannot be kept.
        assert_eq!(pick(&rows, Agent::WorkersAi), Some(Agent::OpenCode));
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
