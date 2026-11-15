//! Projects are git repositories; each environment is one of their worktrees and
//! owns its own tabs, sessions and terminals, so switching environments swaps the
//! whole set.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use gpui::{Entity, SharedString, Subscription, UniformListScrollHandle};

use crate::agent::{AgentKind, SessionMeta, Thread, ThreadView};
use crate::browser::BrowserView;
use crate::explorer::Explorer;
use crate::git;
use crate::terminal::{Terminal, TerminalView};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Agent {
    Claude,
    Droid,
    OpenCode,
    WorkersAi,
}

/// The agent picked last, anywhere, as its place in `Agent::ALL`: new environments start with it.
static LAST_AGENT: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(1);

/// The agent a new environment starts with: the last one picked (Droid until one is).
pub fn default_agent() -> Agent {
    let i = LAST_AGENT.load(std::sync::atomic::Ordering::Relaxed);
    Agent::ALL.get(i as usize).copied().unwrap_or(Agent::Droid)
}

pub fn set_default_agent(agent: Agent) {
    let i = Agent::ALL.iter().position(|a| *a == agent).unwrap_or(1);
    LAST_AGENT.store(i as u8, std::sync::atomic::Ordering::Relaxed);
}

impl Agent {
    pub const ALL: [Agent; 4] = [
        Agent::Claude,
        Agent::Droid,
        Agent::OpenCode,
        Agent::WorkersAi,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Agent::Claude => "Claude Code",
            Agent::Droid => "Droid",
            Agent::OpenCode => "OpenCode",
            Agent::WorkersAi => "Workers AI",
        }
    }

    pub fn key(self) -> &'static str {
        match self {
            Agent::Claude => "claude",
            Agent::Droid => "droid",
            Agent::OpenCode => "opencode",
            Agent::WorkersAi => "workers-ai",
        }
    }

    pub fn kind(self) -> AgentKind {
        match self {
            Agent::Claude => AgentKind::Claude,
            Agent::Droid => AgentKind::Droid,
            Agent::OpenCode => AgentKind::OpenCode,
            Agent::WorkersAi => AgentKind::WorkersAi,
        }
    }

    /// Unknown keys (and old files' missing ones) read as Droid.
    pub fn from_key(key: &str) -> Self {
        match key {
            "claude" => Agent::Claude,
            "opencode" => Agent::OpenCode,
            "workers-ai" => Agent::WorkersAi,
            _ => Agent::Droid,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Status {
    Idle,
    Working,
    NeedsInput,
    Error,
}

#[derive(Clone)]
pub enum TabKind {
    NewSession,
    Session {
        agent: Agent,
        id: SharedString,
    },
    Browser {
        url: SharedString,
    },
    File {
        path: PathBuf,
    },
    Diff {
        path: String,
        untracked: bool,
    },
    /// The project's scratch notes.
    Notes,
    /// The phone page: on/off, link and QR code (`phone_view`).
    Phone,
    /// The CoderSpace tasks assigned to the signed-in person.
    #[cfg(feature = "coderlabs")]
    Tasks,
    /// The GitHub Actions runs of the project: CI and deploys.
    Runs,
    /// Every branch of the project against the default one, with its PR (`branches_view`).
    Branches,
    /// The inbox of the user's GitHub notifications (`github_view`).
    GitHub,
    /// What agents remember about the project and the user (`arakun mem`).
    Memory,
    /// The signed-in Cloudflare account: Workers, domains, activity.
    Cloudflare,
    /// arakun.dev's live visitors and waitlist (`site_view`).
    #[cfg(feature = "coderlabs")]
    Site,
    /// The PostHog project of the open project (`posthog_view`).
    PostHog,
    /// What agent sessions used: cost, tokens and credits over time.
    Usage,
    /// The design system's tokens and components (`gallery_view`).
    Gallery,
    /// Where the sidebar is arranged (`shell::sidebar_builder`).
    SidebarBuilder,
    /// Isometric illustrations playing live (`iso_view`).
    Isometric,
    /// A design canvas's HTML frames rendered natively (`canvas_view`).
    #[cfg(feature = "coderlabs")]
    Canvas,
    /// MCP servers, plugins, skills and permissions of the agents (`extensions_view`).
    Extensions,
    /// What the team hub holds, to read with time (`team_view`).
    #[cfg(feature = "team")]
    Team,
}

/// A mono: a canned prompt that starts a new session.
#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub struct Snippet {
    pub name: String,
    pub body: String,
}

/// A saved task of a project: a shell command run in a terminal, or a prompt
/// an agent works on in a new session; by hand or on a schedule.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct Automation {
    pub name: String,
    /// The shell command, or the prompt when `agent` is set.
    pub body: String,
    /// Key of the agent that gets `body` as its prompt; None runs a command.
    #[serde(default)]
    pub agent: Option<String>,
    #[serde(default)]
    pub schedule: crate::schedule::Schedule,
    #[serde(default = "yes")]
    pub enabled: bool,
    /// The environment it runs in; None (or a removed one) is the main checkout.
    #[serde(default)]
    pub env: Option<PathBuf>,
    /// Scheduled times up to here (unix seconds) were handled, run or missed.
    #[serde(default)]
    pub checked: u64,
    #[serde(default)]
    pub last_run: Option<u64>,
    /// The session the last agent run started.
    #[serde(default)]
    pub last_session: Option<String>,
}

fn yes() -> bool {
    true
}

/// A run missed by less than this (arakun was closed, the laptop asleep) still
/// happens once arakun notices; older ones are skipped.
pub const GRACE_SECS: u64 = 3600;

impl Automation {
    pub fn agent(&self) -> Option<Agent> {
        self.agent.as_deref().map(Agent::from_key)
    }

    /// The next scheduled run, if it is scheduled and enabled.
    pub fn next_run(&self, now: u64) -> Option<u64> {
        if !self.enabled {
            return None;
        }
        self.schedule
            .next_after(self.checked.max(now.saturating_sub(GRACE_SECS)))
    }
}

/// Read-only text shown by file and diff tabs, loaded off the main thread.
pub enum Document {
    Loading,
    Lines(Arc<Doc>),
    /// An image file, drawn instead of read as text.
    Image(std::path::PathBuf),
    /// A spreadsheet, drawn as a grid.
    Sheet(Arc<crate::sheet::Workbook>),
    Failed(SharedString),
}

/// A loaded document: its lines and, for known languages, per-line color spans.
pub struct Doc {
    pub lines: Vec<SharedString>,
    pub spans: Vec<crate::highlight::LineSpans>,
}

impl Doc {
    pub fn plain(lines: Vec<SharedString>) -> Self {
        let spans = vec![Vec::new(); lines.len()];
        Self { lines, spans }
    }
}

pub struct Tab {
    /// Stable identity for the back/forward history, which outlives index shifts.
    pub id: u64,
    pub title: SharedString,
    pub kind: TabKind,
    /// The page of a browser tab, created the first time the tab is shown.
    pub browser: Option<BrowserTab>,
    pub doc: Option<Document>,
    /// Scroll of a file or diff tab, so it can be moved to a line.
    pub scroll: UniformListScrollHandle,
    /// The line (0-based) a file tab was opened at, marked until the next jump.
    pub target_line: Option<usize>,
    /// A view opened from the rail and not used yet: the next one takes its place.
    pub preview: bool,
    /// A markdown file tab shown rendered (MD Viewer): its parsed blocks, redone
    /// when the file reloads. `None` shows the raw lines.
    pub markdown: Option<std::rc::Rc<Vec<crate::agent::markdown::Block>>>,
    /// The sheet shown by a spreadsheet tab.
    pub sheet: usize,
}

impl Tab {
    fn next_id() -> u64 {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
        NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    }

    pub fn new(title: impl Into<SharedString>, kind: TabKind) -> Self {
        Self {
            id: Self::next_id(),
            title: title.into(),
            kind,
            browser: None,
            doc: None,
            scroll: UniformListScrollHandle::new(),
            target_line: None,
            preview: false,
            markdown: None,
            sheet: 0,
        }
    }
}

/// One agent conversation in an environment. Its transcript lives in
/// `sessions/<id>.json`; the agent process only runs while `live` is set.
pub struct BrowserTab {
    pub view: Entity<BrowserView>,
    pub _events: Subscription,
}

pub struct Session {
    pub id: SharedString,
    pub title: SharedString,
    pub agent: Agent,
    pub status: Status,
    /// Unix seconds of the last activity.
    pub updated: u64,
    pub added: u32,
    pub removed: u32,
    /// Lets a later run resume the agent's own session.
    pub meta: Option<SessionMeta>,
    /// The CoderSpace task it was started from.
    #[cfg(feature = "coderlabs")]
    pub task: Option<String>,
    /// The GitHub Actions run it was started to fix.
    pub ci_run: Option<u64>,
    /// Started by `arakun agent new`: the model it asked for.
    pub cli_model: Option<String>,
    /// Its agent's permission requests are allowed without asking.
    pub auto_allow: bool,
    /// The sessions it took over from other agents ("Continue with"), oldest
    /// first; their history is already in this one's transcript.
    pub continues: Vec<Earlier>,
    pub live: Option<LiveSession>,
}

/// A session another agent went on with: its transcript file and its agent's
/// own session, which must not come back as a session of its own.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Earlier {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_session: Option<String>,
}

pub struct LiveSession {
    pub thread: Entity<Thread>,
    pub view: Entity<ThreadView>,
    pub _events: Subscription,
    pub _view_events: Subscription,
}

impl Session {
    pub fn new(id: SharedString, title: SharedString, agent: Agent) -> Self {
        Self {
            id,
            title,
            agent,
            status: Status::Idle,
            updated: unix_now(),
            added: 0,
            removed: 0,
            meta: None,
            #[cfg(feature = "coderlabs")]
            task: None,
            ci_run: None,
            cli_model: None,
            auto_allow: false,
            continues: Vec::new(),
            live: None,
        }
    }
}

pub fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// "now", "5m", "3h", "2d" for the session list.
pub fn age(updated: u64) -> SharedString {
    let secs = unix_now().saturating_sub(updated);
    match secs {
        0..60 => "now".into(),
        60..3600 => format!("{}m", secs / 60).into(),
        3600..86400 => format!("{}h", secs / 3600).into(),
        _ => format!("{}d", secs / 86400).into(),
    }
}

/// A live shell in the dock. Dropping it hangs up the shell.
pub struct TermTab {
    pub terminal: Entity<Terminal>,
    pub view: Entity<TerminalView>,
    /// Its number in the dock, `arakun run` and `arakun logs`. Not reused while
    /// arakun runs, so closing a terminal never makes another answer to it.
    pub number: usize,
    /// What `arakun run` or an automation started in it.
    pub command: Option<SharedString>,
    /// Output arrived while this tab was not on screen.
    pub unseen: bool,
    pub _events: Subscription,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SplitDir {
    Right,
    Down,
}

/// A second pane beside the active tab. The focused pane always shows
/// `active_tab`, so everything that acts on "the tab on screen" keeps working;
/// the other pane only remembers which tab it holds.
#[derive(Clone, Copy, Debug)]
pub struct Split {
    /// Id of the tab in the pane that is not focused.
    pub other: u64,
    pub dir: SplitDir,
    /// Share of the width (or height) taken by the first (left or top) pane.
    pub ratio: f32,
    /// The focused pane is the first one.
    pub focus_first: bool,
}

impl Split {
    pub const MIN_RATIO: f32 = 0.2;
    pub const MAX_RATIO: f32 = 0.8;

    /// Puts each pane on the other side (left/right or top/bottom); each keeps its size.
    pub fn swap(&mut self) {
        self.focus_first = !self.focus_first;
        self.ratio = (1. - self.ratio).clamp(Self::MIN_RATIO, Self::MAX_RATIO);
    }
}

pub struct Environment {
    pub path: PathBuf,
    pub name: SharedString,
    pub branch: SharedString,
    pub is_checkout: bool,
    pub tabs: Vec<Tab>,
    pub active_tab: usize,
    /// Tab ids visited, newest last (the active tab on top), for Back.
    pub tab_back: Vec<u64>,
    /// Tabs gone Back from, for Forward.
    pub tab_fwd: Vec<u64>,
    pub split: Option<Split>,
    pub sessions: Vec<Session>,
    pub terminals: Vec<TermTab>,
    pub active_terminal: usize,
    /// Terminals opened here since arakun started; the next one's number.
    pub terminals_opened: usize,
    pub changes: Vec<git::Change>,
    pub draft: crate::draft::Draft,
    /// Images waiting in the composer, sent along with the draft.
    pub draft_images: Vec<PathBuf>,
    pub agent: Agent,
    /// Native Droid settings picked on the New session tab, for the next session.
    pub draft_settings: Vec<(crate::agent::Setting, String)>,
    /// Claude Code settings for the next session: picked on the New session tab,
    /// or last changed in one of this environment's sessions.
    pub claude_settings: Vec<(crate::agent::Setting, String)>,
    /// The same for OpenCode.
    pub opencode_settings: Vec<(crate::agent::Setting, String)>,
    /// The same for Workers AI.
    pub workers_ai_settings: Vec<(crate::agent::Setting, String)>,
    pub explorer: Explorer,
    pub sync: git::Sync,
    pub pr: Option<git::PullRequest>,
    /// Branch and time of the last `gh pr view`, so the network is asked rarely.
    pub pr_checked: Option<(SharedString, std::time::Instant)>,
    /// A push, pull or PR running from the Changes panel.
    pub git_busy: Option<GitOp>,
    pub git_error: Option<SharedString>,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum GitOp {
    Push,
    Pull,
    PullRequest,
}

impl Environment {
    /// Back/forward assume the last history entry is the visible tab; tabs restored
    /// or opened without going through the shell start the history here.
    pub fn seed_tab_history(&mut self) {
        let Some(id) = self.tabs.get(self.active_tab).map(|t| t.id) else {
            return;
        };
        if self.tab_back.last() != Some(&id) {
            self.tab_back.push(id);
        }
    }

    pub fn new(path: PathBuf, branch: Option<String>, is_checkout: bool) -> Self {
        let mut env = Self {
            name: SharedString::default(),
            branch: SharedString::default(),
            explorer: Explorer::new(path.clone()),
            path,
            is_checkout,
            tabs: vec![Tab::new("New session", TabKind::NewSession)],
            active_tab: 0,
            tab_back: Vec::new(),
            tab_fwd: Vec::new(),
            split: None,
            sessions: Vec::new(),
            terminals: Vec::new(),
            active_terminal: 0,
            terminals_opened: 0,
            changes: Vec::new(),
            draft: Default::default(),
            draft_images: Vec::new(),
            agent: default_agent(),
            draft_settings: Vec::new(),
            claude_settings: Vec::new(),
            opencode_settings: Vec::new(),
            workers_ai_settings: Vec::new(),
            sync: git::Sync::default(),
            pr: None,
            pr_checked: None,
            git_busy: None,
            git_error: None,
        };
        env.set_branch(branch);
        env.seed_tab_history();
        env
    }

    pub fn set_branch(&mut self, branch: Option<String>) {
        let branch: SharedString = branch.unwrap_or_else(|| "detached HEAD".into()).into();
        self.name = if self.is_checkout {
            branch.clone()
        } else {
            dir_name(&self.path).into()
        };
        self.branch = branch;
    }

    /// Index of the tab in the unfocused pane, when the split is still meaningful.
    pub fn split_other(&self) -> Option<usize> {
        let split = self.split.as_ref()?;
        let i = self.tabs.iter().position(|t| t.id == split.other)?;
        (i != self.active_tab).then_some(i)
    }

    /// Shows the tab `id` in the first (left or top) pane or the second, splitting with
    /// `companion` when only one pane shows. The two shown tabs swap when one is dropped on
    /// the other's side; a third tab takes over the pane it lands on. False when nothing moved.
    pub fn place_tab(
        &mut self,
        id: u64,
        first: bool,
        companion: impl FnOnce(&mut Self) -> usize,
    ) -> bool {
        let Some(index) = self.tabs.iter().position(|t| t.id == id) else {
            return false;
        };
        let Some(active_id) = self.tabs.get(self.active_tab).map(|t| t.id) else {
            return false;
        };
        let other_id = self.split_other().map(|i| self.tabs[i].id);
        if let (Some(other_id), Some(split)) = (other_id, self.split.as_mut()) {
            if id == active_id || id == other_id {
                let on_first = (id == active_id) == split.focus_first;
                if on_first == first {
                    return false;
                }
                split.swap();
            } else {
                if split.focus_first != first {
                    split.other = active_id;
                    split.focus_first = first;
                }
                self.active_tab = index;
            }
            return true;
        }
        let ratio = self.split.map_or(0.5, |s| s.ratio);
        let other = if index == self.active_tab {
            companion(self)
        } else {
            self.active_tab
        };
        let other_id = self.tabs[other].id;
        self.split = Some(Split {
            other: other_id,
            dir: SplitDir::Right,
            ratio,
            focus_first: first,
        });
        self.active_tab = index;
        true
    }

    /// Keeps the split consistent after `active_tab` changed from the tab `prev`:
    /// activating the tab of the other pane swaps the panes' roles, and a split
    /// that lost its other tab closes.
    pub fn settle_split(&mut self, prev: Option<u64>) {
        let Some(split) = self.split.as_mut() else {
            return;
        };
        let active = self.tabs.get(self.active_tab).map(|t| t.id);
        if active == Some(split.other) {
            match prev.filter(|p| Some(*p) != active && self.tabs.iter().any(|t| t.id == *p)) {
                Some(p) => {
                    split.other = p;
                    split.focus_first = !split.focus_first;
                }
                None => self.split = None,
            }
        } else if !self.tabs.iter().any(|t| t.id == split.other) {
            self.split = None;
        }
    }

    pub fn session_index(&self, id: &str) -> Option<usize> {
        self.sessions.iter().position(|s| s.id.as_ref() == id)
    }

    /// The most urgent state of any session, for the sidebar dot.
    pub fn aggregate_status(&self) -> Status {
        let any = |st: Status| self.sessions.iter().any(|s| s.status == st);
        if any(Status::NeedsInput) {
            Status::NeedsInput
        } else if any(Status::Working) {
            Status::Working
        } else {
            Status::Idle
        }
    }

    pub fn diff_totals(&self) -> (u32, u32) {
        self.changes
            .iter()
            .fold((0, 0), |(a, r), c| (a + c.added, r + c.removed))
    }
}

pub struct Project {
    pub path: PathBuf,
    pub name: SharedString,
    pub logo: u32,
    pub art: u8,
    pub environments: Vec<Environment>,
    pub active_env: usize,
    /// Shell command run in a terminal when a new environment is created.
    pub setup: Option<String>,
    /// Shell command run after agent turns that edit files; failures go back to the agent.
    pub check: Option<String>,
    /// The user doesn't want arakun to suggest a check here.
    pub check_declined: bool,
    /// Scratch notes, shown by the Notes tab.
    pub notes: Vec<String>,
    /// Canned prompts. Nothing shows them for now; they stay in the saved
    /// state so they come back when the feature does.
    pub monos: Vec<Snippet>,
    pub automations: Vec<Automation>,
    /// CoderSpace project (`rp<id>`) this project works on; set the first time
    /// one of its tasks starts a session here.
    #[cfg(feature = "coderlabs")]
    pub coderspace: Option<String>,
}

impl Project {
    pub fn env_index(&self, path: &Path) -> Option<usize> {
        self.environments.iter().position(|e| e.path == path)
    }

    /// Brings the environment list in line with `git worktree list`, keeping the
    /// state (tabs, terminals, drafts) of worktrees that still exist.
    pub fn sync_worktrees(&mut self, trees: Vec<git::Worktree>) {
        let active = self
            .environments
            .get(self.active_env)
            .map(|e| e.path.clone());
        let mut old = std::mem::take(&mut self.environments);
        for tree in trees {
            let env = match old.iter().position(|e| e.path == tree.path) {
                Some(i) => {
                    let mut env = old.remove(i);
                    env.is_checkout = tree.is_main;
                    env.set_branch(tree.branch);
                    env
                }
                None => Environment::new(tree.path, tree.branch, tree.is_main),
            };
            self.environments.push(env);
        }
        self.active_env = active
            .and_then(|p| self.env_index(&p))
            .unwrap_or(0)
            .min(self.environments.len().saturating_sub(1));
    }
}

pub fn dir_name(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string())
}

/// `/home/me/x` → `~/x`.
pub fn pretty_path(path: &Path) -> String {
    let home = home();
    match path.strip_prefix(&home) {
        Ok(rest) if rest.as_os_str().is_empty() => "~".into(),
        Ok(rest) => format!("~/{}", rest.display()),
        Err(_) => path.display().to_string(),
    }
}

pub fn home() -> PathBuf {
    crate::platform::home()
}

/// Worktrees arakun creates live in `~/arakun-workspaces/<project>/<branch>`: visible to
/// editors and file managers, and outside every repository.
pub fn workspaces_root() -> PathBuf {
    home().join("arakun-workspaces")
}

pub fn worktree_path(project: &str, branch: &str) -> PathBuf {
    let slug: String = branch
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '-' || c == '_' || c == '.' {
                c
            } else {
                '-'
            }
        })
        .collect();
    let slug = slug.trim_matches('-');
    let base = workspaces_root().join(project);
    let mut path = base.join(slug);
    let mut n = 2;
    while path.exists() {
        path = base.join(format!("{slug}-{n}"));
        n += 1;
    }
    path
}

/// Logo colors and pixel-art for new projects, picked from the path so a project
/// keeps its look if it is removed and added again.
const LOGO_COLORS: [u32; 8] = {
    let c = crate::theme::CATEGORY;
    [c[7], c[0], c[2], c[6], c[3], c[1], c[5], c[4]]
};

/// The colors projects were given before `LOGO_COLORS`, index for index, so a
/// saved project keeps its hue in the new palette.
const OLD_LOGO_COLORS: [u32; 8] = [
    0xe8c547, 0x38bdf8, 0x4ade80, 0xf472b6, 0xa78bfa, 0xfb923c, 0x2dd4bf, 0xf87171,
];

/// A saved logo color, moved to the current palette if it is an old one.
pub fn current_logo(logo: u32) -> u32 {
    OLD_LOGO_COLORS
        .iter()
        .position(|&c| c == logo)
        .map_or(logo, |i| LOGO_COLORS[i])
}

pub fn logo_for(path: &Path) -> (u32, u8) {
    let mut h: u32 = 2166136261;
    for b in path.as_os_str().as_encoded_bytes() {
        h = (h ^ *b as u32).wrapping_mul(16777619);
    }
    (LOGO_COLORS[(h % 8) as usize], ((h >> 8) % 2) as u8)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn old_logo_colors_move_to_the_palette() {
        assert_eq!(current_logo(0x38bdf8), crate::theme::CATEGORY[0]);
        assert_eq!(current_logo(0xf87171), crate::theme::CATEGORY[4]);
        assert_eq!(
            current_logo(crate::theme::CATEGORY[2]),
            crate::theme::CATEGORY[2]
        );
        let mut seen = LOGO_COLORS.to_vec();
        seen.sort();
        seen.dedup();
        assert_eq!(seen.len(), 8, "every old color gets its own new one");
    }

    fn env_with(kinds: Vec<TabKind>) -> Environment {
        let mut env =
            Environment::new(PathBuf::from("/tmp/arakun-test"), Some("main".into()), true);
        env.tabs = kinds.into_iter().map(|k| Tab::new("t", k)).collect();
        env
    }

    fn browser(url: &str) -> TabKind {
        TabKind::Browser {
            url: url.to_string().into(),
        }
    }

    #[test]
    fn activating_the_other_pane_swaps_roles() {
        let mut env = env_with(vec![browser("a"), browser("b"), browser("c")]);
        let ids: Vec<u64> = env.tabs.iter().map(|t| t.id).collect();
        env.split = Some(Split {
            other: ids[1],
            dir: SplitDir::Right,
            ratio: 0.5,
            focus_first: true,
        });
        assert_eq!(env.split_other(), Some(1));

        env.active_tab = 1;
        env.settle_split(Some(ids[0]));
        let split = env.split.unwrap();
        assert_eq!(split.other, ids[0]);
        assert!(
            !split.focus_first,
            "the focused pane moved to the second slot"
        );
        assert_eq!(env.split_other(), Some(0));

        // Picking a third tab replaces the focused pane's content only.
        env.active_tab = 2;
        env.settle_split(Some(ids[1]));
        assert_eq!(env.split.unwrap().other, ids[0]);
        assert_eq!(env.split_other(), Some(0));
    }

    #[test]
    fn swapping_moves_each_pane_with_its_size() {
        let mut split = Split {
            other: 7,
            dir: SplitDir::Right,
            ratio: 0.3,
            focus_first: true,
        };
        split.swap();
        assert!(!split.focus_first, "the focused tab is now on the right");
        assert_eq!(split.other, 7, "the panes keep their tabs");
        assert!(
            (split.ratio - 0.7).abs() < 1e-6,
            "the left pane now has the right one's width"
        );
        split.swap();
        assert!(split.focus_first);
        assert!((split.ratio - 0.3).abs() < 1e-6);
    }

    #[test]
    fn dropping_tabs_on_panes() {
        let mut env = env_with(vec![browser("a"), browser("b"), browser("c")]);
        let ids: Vec<u64> = env.tabs.iter().map(|t| t.id).collect();
        let no_companion = |_: &mut Environment| -> usize { unreachable!() };
        // Not split: b dropped on the right opens beside the active a.
        assert!(env.place_tab(ids[1], false, no_companion));
        let left_right = |env: &Environment| {
            let split = env.split.unwrap();
            let (active, other) = (env.tabs[env.active_tab].id, split.other);
            if split.focus_first {
                (active, other)
            } else {
                (other, active)
            }
        };
        assert_eq!(left_right(&env), (ids[0], ids[1]));
        // Dropping a shown tab on its own side does nothing; on the other side, they swap.
        assert!(!env.place_tab(ids[1], false, no_companion));
        assert!(env.place_tab(ids[1], true, no_companion));
        assert_eq!(left_right(&env), (ids[1], ids[0]));
        // A third tab takes over the pane it lands on, either side.
        assert!(env.place_tab(ids[2], false, no_companion));
        assert_eq!(left_right(&env), (ids[1], ids[2]));
        assert!(env.place_tab(ids[0], true, no_companion));
        assert_eq!(left_right(&env), (ids[0], ids[2]));
        assert_eq!(
            env.tabs[env.active_tab].id, ids[0],
            "the dropped tab gets the focus"
        );
    }

    #[test]
    fn dropping_the_only_shown_tab_pairs_it_with_a_companion() {
        let mut env = env_with(vec![browser("a"), browser("b")]);
        let ids: Vec<u64> = env.tabs.iter().map(|t| t.id).collect();
        assert!(env.place_tab(ids[0], false, |_| 1));
        let split = env.split.unwrap();
        assert_eq!(
            (env.tabs[env.active_tab].id, split.other, split.focus_first),
            (ids[0], ids[1], false)
        );
    }

    #[test]
    fn split_closes_when_its_other_tab_goes_away() {
        let mut env = env_with(vec![browser("a"), browser("b")]);
        let other = env.tabs[1].id;
        env.split = Some(Split {
            other,
            dir: SplitDir::Down,
            ratio: 0.3,
            focus_first: true,
        });
        env.tabs.remove(1);
        assert_eq!(env.split_other(), None);
        env.settle_split(None);
        assert!(env.split.is_none());
    }

    #[test]
    fn landing_on_the_other_tab_without_a_previous_one_unsplits() {
        let mut env = env_with(vec![browser("a"), browser("b")]);
        let ids: Vec<u64> = env.tabs.iter().map(|t| t.id).collect();
        env.split = Some(Split {
            other: ids[1],
            dir: SplitDir::Right,
            ratio: 0.5,
            focus_first: true,
        });
        // The active tab was closed, and its neighbour is the other pane's tab.
        env.tabs.remove(0);
        env.active_tab = 0;
        env.settle_split(Some(ids[0]));
        assert!(env.split.is_none());
    }
}
