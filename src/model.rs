//! Projects are git repositories; each environment is one of their worktrees and
//! owns its own sessions and terminals. The window has one row of tabs
//! (`TabStrip`) for all of them: a tab says which environment it works in, and
//! showing it shows that environment.

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
    Codex,
    Devin,
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
    pub const ALL: [Agent; 6] = [
        Agent::Claude,
        Agent::Droid,
        Agent::OpenCode,
        Agent::Codex,
        Agent::Devin,
        Agent::WorkersAi,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Agent::Claude => "Claude Code",
            Agent::Droid => "Droid",
            Agent::OpenCode => "OpenCode",
            Agent::Codex => "Codex",
            Agent::Devin => "Devin",
            Agent::WorkersAi => "Workers AI",
        }
    }

    pub fn key(self) -> &'static str {
        match self {
            Agent::Claude => "claude",
            Agent::Droid => "droid",
            Agent::OpenCode => "opencode",
            Agent::Codex => "codex",
            Agent::Devin => "devin",
            Agent::WorkersAi => "workers-ai",
        }
    }

    pub fn kind(self) -> AgentKind {
        match self {
            Agent::Claude => AgentKind::Claude,
            Agent::Droid => AgentKind::Droid,
            Agent::OpenCode => AgentKind::OpenCode,
            Agent::Codex => AgentKind::Codex,
            Agent::Devin => AgentKind::Devin,
            Agent::WorkersAi => AgentKind::WorkersAi,
        }
    }

    /// Unknown keys (and old files' missing ones) read as Droid.
    pub fn from_key(key: &str) -> Self {
        match key {
            "claude" => Agent::Claude,
            "opencode" => Agent::OpenCode,
            "codex" => Agent::Codex,
            "devin" => Agent::Devin,
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
    /// The machines running `arakun server` and their sessions (`servers_view`).
    Servers,
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
    /// A diff too large to load unless asked: building it on every change of
    /// a growing file would stall the app.
    TooLarge(SharedString),
}

/// A load that panicked shows as one that failed.
impl crate::guard::Fallback for Document {
    fn fallback(panicked: crate::guard::Panicked) -> Self {
        Document::Failed(panicked.to_string().into())
    }
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
    /// The environment a session, file, diff or browser tab works in, by its
    /// path. Showing the tab shows that environment. A view (GitHub, Usage…)
    /// has none and follows the environment on screen.
    pub env: Option<PathBuf>,
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
    /// A diff tab loads its diff even past the size limit ("Show anyway").
    pub large: bool,
}

impl Tab {
    fn next_id() -> u64 {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
        NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    }

    pub fn new(title: impl Into<SharedString>, kind: TabKind) -> Self {
        Self {
            id: Self::next_id(),
            env: None,
            title: title.into(),
            kind,
            browser: None,
            doc: None,
            scroll: UniformListScrollHandle::new(),
            target_line: None,
            preview: false,
            markdown: None,
            sheet: 0,
            large: false,
        }
    }

    /// A tab that works in the environment at `env`.
    pub fn of(env: &Path, title: impl Into<SharedString>, kind: TabKind) -> Self {
        Self {
            env: Some(env.to_path_buf()),
            ..Self::new(title, kind)
        }
    }

    /// The blank tab an environment starts a session from.
    pub fn new_session(env: &Path) -> Self {
        Self::of(env, "New session", TabKind::NewSession)
    }

    pub fn is_in(&self, env: &Path) -> bool {
        self.env.as_deref() == Some(env)
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
    /// Archived, by hand or once its pull request merged (`crate::settle`):
    /// the sessions list leaves it out until it is used again or restored.
    pub settled: bool,
    /// It finished a turn while the user looked elsewhere. Until they open
    /// it, it waits on them like a question would.
    pub unread: bool,
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
            settled: false,
            unread: false,
            live: None,
        }
    }

    /// Its state as the sidebar and the Inbox show it: a finished turn not
    /// yet read needs the user.
    pub fn shown_status(&self) -> Status {
        if self.unread && self.status == Status::Idle {
            Status::NeedsInput
        } else {
            self.status
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
    /// The random name the "New environment" dialog gave the branch. The first
    /// prompt sent here renames it to one that says what the work is, as long as
    /// the branch still has this name: a name picked by hand is the user's.
    pub placeholder: Option<String>,
    /// Shown by its branch, not its folder: arakun named or renamed the branch, so
    /// the folder no longer says what the environment is.
    pub follow_branch: bool,
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
    /// The same for Codex.
    pub codex_settings: Vec<(crate::agent::Setting, String)>,
    /// The same for Devin.
    pub devin_settings: Vec<(crate::agent::Setting, String)>,
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
    /// Unix seconds it was last on screen, for the cleanup of old environments.
    pub used: u64,
    /// A folder on a paired server, not here. Its sessions run there; nothing
    /// of it is saved, the server's inbox brings it back each time.
    pub remote: Option<Remote>,
}

/// Where a remote environment is: the server (`crate::servers`) and the
/// folder's path on it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Remote {
    pub server: String,
    /// What the server calls itself, shown beside the environment.
    pub machine: SharedString,
    pub place: String,
}

impl Remote {
    /// The path a remote environment goes by here. No folder has it: the
    /// scheme keeps it from ever naming one, and it is unique per folder.
    pub fn env_path(server: &str, place: &str) -> PathBuf {
        PathBuf::from(format!("{}{server}{place}", Self::SCHEME))
    }

    /// The path of a project only servers have, under the key it is found by.
    pub fn project_path(key: &str) -> PathBuf {
        PathBuf::from(format!("{}project/{key}", Self::SCHEME))
    }

    /// Whether `path` is one of the two above.
    pub fn is_path(path: &Path) -> bool {
        path.to_string_lossy().starts_with(Self::SCHEME)
    }

    const SCHEME: &str = "arakun-server://";
}

/// A tab that was closed: where it was, what it showed and in which environment.
pub struct ClosedTab {
    pub index: usize,
    pub kind: TabKind,
    pub env: Option<PathBuf>,
}

/// The window's one row of tabs, for every project and environment. A tab that
/// works in an environment says which (`Tab::env`), and the tabs of one
/// environment stay next to each other until the user moves them.
#[derive(Default)]
pub struct TabStrip {
    pub tabs: Vec<Tab>,
    pub active_tab: usize,
    /// Tab ids visited, newest last (the active tab on top), for Back.
    pub tab_back: Vec<u64>,
    /// Tabs gone Back from, for Forward.
    pub tab_fwd: Vec<u64>,
    /// Tabs closed, newest last: Reopen closed tab takes them back in turn.
    pub closed_tabs: Vec<ClosedTab>,
    /// The split of the environment on screen, `split_env`.
    pub split: Option<Split>,
    pub split_env: Option<PathBuf>,
    /// The splits of the environments that are not on screen, each with the tab
    /// its focused pane showed. One comes back with its environment.
    pub parked: std::collections::HashMap<PathBuf, (Split, u64)>,
    /// The tab each environment showed last, which going to the environment shows again.
    pub front: std::collections::HashMap<PathBuf, u64>,
}

impl TabStrip {
    pub fn active(&self) -> Option<&Tab> {
        self.tabs.get(self.active_tab)
    }

    pub fn position(&self, id: u64) -> Option<usize> {
        self.tabs.iter().position(|t| t.id == id)
    }

    /// The first tab of the environment at `env` that `is` picks.
    pub fn find_in(&self, env: &Path, is: impl Fn(&Tab) -> bool) -> Option<usize> {
        self.tabs.iter().position(|t| t.is_in(env) && is(t))
    }

    /// The tab of session `id`. Session ids are unique across environments.
    pub fn session_tab(&self, id: &str) -> Option<usize> {
        self.tabs
            .iter()
            .position(|t| matches!(&t.kind, TabKind::Session { id: tid, .. } if tid.as_ref() == id))
    }

    /// Whether the tab at `index` is the active one and `env`, the environment
    /// on screen, has no other tab. Closing it leaves `env` nothing to show.
    pub fn is_last_on_screen(&self, index: usize, env: &Path) -> bool {
        index == self.active_tab
            && !self
                .tabs
                .iter()
                .enumerate()
                .any(|(i, t)| i != index && t.is_in(env))
    }

    /// The tab to show for the environment at `env`: the one it showed last,
    /// else its first.
    pub fn front_of(&self, env: &Path) -> Option<usize> {
        self.front
            .get(env)
            .and_then(|id| self.position(*id))
            .filter(|i| self.tabs[*i].is_in(env))
            .or_else(|| self.find_in(env, |_| true))
    }

    /// Adds a tab after the last one of its environment (a view: after the last
    /// view), or at the end when it is the first. Returns its index.
    pub fn add(&mut self, tab: Tab) -> usize {
        let at = self
            .tabs
            .iter()
            .rposition(|t| t.env == tab.env)
            .map_or(self.tabs.len(), |i| i + 1);
        if !self.tabs.is_empty() && at <= self.active_tab {
            self.active_tab += 1;
        }
        self.tabs.insert(at, tab);
        at
    }

    /// Takes the tab at `index` out. Another active tab stays active. In place of
    /// a closed active tab comes its right neighbour, or the left one when only
    /// that one is of the same environment or nothing is to the right.
    pub fn remove(&mut self, index: usize) -> Option<Tab> {
        if index >= self.tabs.len() {
            return None;
        }
        let tab = self.tabs.remove(index);
        if self.active_tab > index {
            self.active_tab -= 1;
        } else if self.active_tab == index && index > 0 {
            let same = |i: usize| self.tabs.get(i).is_some_and(|t| t.env == tab.env);
            if index >= self.tabs.len() || (!same(index) && same(index - 1)) {
                self.active_tab = index - 1;
            }
        }
        Some(tab)
    }

    /// Takes out every tab `gone` picks, keeping the active tab when it stays.
    pub fn remove_where(&mut self, gone: impl Fn(&Tab) -> bool) -> Vec<Tab> {
        let active_id = self.active().map(|t| t.id);
        let (removed, kept) = std::mem::take(&mut self.tabs)
            .into_iter()
            .partition(|t| gone(t));
        self.tabs = kept;
        self.active_tab = active_id
            .and_then(|id| self.position(id))
            .unwrap_or(self.active_tab)
            .min(self.tabs.len().saturating_sub(1));
        self.front
            .retain(|_, id| self.tabs.iter().any(|t| t.id == *id));
        removed
    }

    /// Back/forward assume the last history entry is the visible tab; tabs restored
    /// or opened without going through the shell start the history here.
    pub fn seed_tab_history(&mut self) {
        let Some(id) = self.active().map(|t| t.id) else {
            return;
        };
        if self.tab_back.last() != Some(&id) {
            self.tab_back.push(id);
        }
    }

    /// Index of the tab in the unfocused pane, when the split is still meaningful.
    /// The two panes show one environment, `env`: a split whose other tab works
    /// in another one waits until that environment is on screen again.
    pub fn split_other(&self, env: &Path) -> Option<usize> {
        let split = self.split.as_ref()?;
        let i = self.position(split.other)?;
        let fits = self.tabs[i].env.as_deref().is_none_or(|e| e == env);
        (i != self.active_tab && fits).then_some(i)
    }

    /// Makes `split` the one of the environment `env`, which came on screen
    /// after the tab `prev` was active: the split of the environment that left
    /// is put aside, and the one `env` had comes back. Arriving on the tab of
    /// its other pane swaps the panes' roles, as selecting that tab does.
    pub fn bring_split(&mut self, env: &Path, prev: Option<u64>) {
        match self.split_env.take() {
            Some(old) if old == env => {
                self.split_env = Some(old);
                return;
            }
            Some(old) => {
                if let Some(split) = self.split.take() {
                    let fits = |id: &u64| {
                        *id != split.other
                            && self.position(*id).is_some_and(|i| {
                                self.tabs[i].env.as_deref().is_none_or(|e| e == old)
                            })
                    };
                    let focus = prev
                        .filter(&fits)
                        .or_else(|| self.front.get(&old).copied().filter(&fits));
                    if let Some(focus) = focus {
                        self.parked.insert(old, (split, focus));
                    }
                }
            }
            // The first environment on screen: a split made so far is its own.
            None if self.split.is_some() => {
                self.split_env = Some(env.to_path_buf());
                return;
            }
            // No split on screen (the environment that had one is gone): the
            // one this environment put aside comes back below.
            None => {}
        }
        self.split_env = Some(env.to_path_buf());
        let Some((mut split, focus)) = self.parked.remove(env) else {
            return;
        };
        if self.active().map(|t| t.id) == Some(split.other) {
            if self.position(focus).is_none() {
                return;
            }
            split.other = focus;
            split.focus_first = !split.focus_first;
        }
        if self.position(split.other).is_some() {
            self.split = Some(split);
        }
    }

    /// Shows the tab `id` in the first (left or top) pane or the second, splitting with
    /// `companion` when only one pane shows. The two shown tabs swap when one is dropped on
    /// the other's side; a third tab takes over the pane it lands on. False when nothing moved.
    pub fn place_tab(
        &mut self,
        env: &Path,
        id: u64,
        first: bool,
        companion: impl FnOnce(&mut Self) -> usize,
    ) -> bool {
        let Some(index) = self.position(id) else {
            return false;
        };
        let Some(active_id) = self.active().map(|t| t.id) else {
            return false;
        };
        let other_id = self.split_other(env).map(|i| self.tabs[i].id);
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
        // The companion may have been added before the dropped tab.
        self.active_tab = self.position(id).unwrap_or(index);
        self.split = Some(Split {
            other: other_id,
            dir: SplitDir::Right,
            ratio,
            focus_first: first,
        });
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

    /// Remembers a closed tab for Reopen closed tab; the oldest go past a limit.
    pub fn remember_closed(&mut self, index: usize, kind: TabKind, env: Option<PathBuf>) {
        const KEEP: usize = 25;
        self.closed_tabs.push(ClosedTab { index, kind, env });
        let excess = self.closed_tabs.len().saturating_sub(KEEP);
        self.closed_tabs.drain(..excess);
    }

    /// Takes out the tabs at the indices `close` picks, keeping `anchor` and the
    /// active tab when it stays. Returns the closed tabs with their old indices.
    /// A split whose other tab went closes.
    pub fn remove_tabs(
        &mut self,
        anchor: usize,
        close: impl Fn(usize) -> bool,
    ) -> Vec<(usize, Tab)> {
        let active_id = self.tabs.get(self.active_tab).map(|t| t.id);
        let anchor_id = self.tabs.get(anchor).map(|t| t.id);
        let mut closed = Vec::new();
        for (i, tab) in std::mem::take(&mut self.tabs).into_iter().enumerate() {
            if i != anchor && close(i) {
                closed.push((i, tab));
            } else {
                self.tabs.push(tab);
            }
        }
        let at = |id: Option<u64>| self.tabs.iter().position(|t| Some(t.id) == id);
        self.active_tab = at(active_id).or(at(anchor_id)).unwrap_or(0);
        if self
            .split
            .is_some_and(|s| !self.tabs.iter().any(|t| t.id == s.other))
        {
            self.split = None;
        }
        closed
    }

    /// Moves the tab at `from` to `to` (clamped), keeping the same tab active.
    /// False when nothing moved.
    pub fn move_tab(&mut self, from: usize, to: usize) -> bool {
        let len = self.tabs.len();
        let to = to.min(len.saturating_sub(1));
        if from >= len || from == to {
            return false;
        }
        let active_id = self.tabs[self.active_tab.min(len - 1)].id;
        let tab = self.tabs.remove(from);
        self.tabs.insert(to, tab);
        self.active_tab = self
            .tabs
            .iter()
            .position(|t| t.id == active_id)
            .unwrap_or(0);
        true
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum GitOp {
    Push,
    Pull,
    PullRequest,
}

impl Environment {
    pub fn new(path: PathBuf, branch: Option<String>, is_checkout: bool) -> Self {
        let mut env = Self {
            name: SharedString::default(),
            branch: SharedString::default(),
            explorer: Explorer::new(path.clone()),
            path,
            is_checkout,
            placeholder: None,
            follow_branch: false,
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
            codex_settings: Vec::new(),
            devin_settings: Vec::new(),
            workers_ai_settings: Vec::new(),
            sync: git::Sync::default(),
            pr: None,
            pr_checked: None,
            git_busy: None,
            git_error: None,
            used: unix_now(),
            remote: None,
        };
        env.set_branch(branch);
        env
    }

    /// The branch still has the dialog's random name, so the first prompt may rename it.
    pub fn awaits_name(&self) -> bool {
        self.placeholder.as_deref() == Some(self.branch.as_ref())
    }

    /// Shows the environment by its branch from now on, and relabels it.
    pub fn follow_branch(&mut self) {
        self.follow_branch = true;
        self.name = self.branch.clone();
    }

    pub fn set_branch(&mut self, branch: Option<String>) {
        let branch: SharedString = branch.unwrap_or_else(|| "detached HEAD".into()).into();
        self.name = if self.is_checkout || self.follow_branch {
            branch.clone()
        } else {
            dir_name(&self.path).into()
        };
        self.branch = branch;
    }

    /// A folder outside any repository has no branch, detached or not: `branch`
    /// stays empty and the folder's name labels it.
    pub fn set_no_branch(&mut self) {
        self.branch = SharedString::default();
        self.name = dir_name(&self.path).into();
    }

    /// The branch checked out here: None on a detached HEAD and outside a
    /// repository.
    pub fn checked_out(&self) -> Option<&str> {
        Some(self.branch.as_ref()).filter(|b| !b.is_empty() && *b != "detached HEAD")
    }

    pub fn session_index(&self, id: &str) -> Option<usize> {
        self.sessions.iter().position(|s| s.id.as_ref() == id)
    }

    /// Every session here is archived (`Session::settled`), so the sidebar
    /// leaves it out. A worktree with no session yet is not.
    pub fn archived(&self) -> bool {
        !self.sessions.is_empty() && self.sessions.iter().all(|s| s.settled)
    }

    /// The most urgent state of any session, for the sidebar dot.
    pub fn aggregate_status(&self) -> Status {
        let any = |st: Status| self.sessions.iter().any(|s| s.shown_status() == st);
        if any(Status::NeedsInput) {
            Status::NeedsInput
        } else if any(Status::Working) {
            Status::Working
        } else if any(Status::Error) {
            // With one thread per worktree, a failed thread is the row's state.
            Status::Error
        } else {
            Status::Idle
        }
    }

    /// When it was last opened or one of its sessions last moved.
    pub fn last_used(&self) -> u64 {
        self.sessions
            .iter()
            .map(|s| s.updated)
            .fold(self.used, u64::max)
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
    /// The project's 8×8 pixel-art icon (`ui::project_icon`), drawn in `logo`.
    pub icon: u64,
    pub environments: Vec<Environment>,
    pub active_env: usize,
    /// Shell command run in a terminal when a new environment is created.
    pub setup: Option<String>,
    /// Shell command run after agent turns that edit files; failures go back to the agent.
    pub check: Option<String>,
    /// The user doesn't want arakun to suggest a check here.
    pub check_declined: bool,
    /// Its agents run in a sandbox (`agent::sandbox`).
    pub sandbox: bool,
    /// The branch new environments start from; None for origin's default branch.
    pub base: Option<String>,
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
    /// Only on servers: nothing of it is on this machine.
    pub fn is_remote(&self) -> bool {
        Remote::is_path(&self.path)
    }

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
        // A server's folders are no worktrees here: git does not list them.
        self.environments
            .extend(old.into_iter().filter(|e| e.remote.is_some()));
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

/// Logo colors for new projects, picked from the path so a project
/// keeps its look if it is removed and added again.
pub const LOGO_COLORS: [u32; 8] = {
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

fn path_hash(path: &Path) -> u64 {
    let mut h: u64 = 0xcbf29ce484222325;
    for b in path.as_os_str().as_encoded_bytes() {
        h = (h ^ *b as u64).wrapping_mul(0x100000001b3);
    }
    h
}

/// A new project's logo color and icon, from its path.
pub fn logo_for(path: &Path) -> (u32, u64) {
    let h = path_hash(path);
    (LOGO_COLORS[(h % 8) as usize], icon_from_seed(h))
}

fn splitmix(state: &mut u64) -> u64 {
    *state = state.wrapping_add(0x9e3779b97f4a7c15);
    let mut z = *state;
    z = (z ^ (z >> 30)).wrapping_mul(0xbf58476d1ce4e5b9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94d049bb133111eb);
    z ^ (z >> 31)
}

/// A mirrored 8×8 sprite, the same for the same seed. Pixels near the middle
/// are likelier lit, and drafts that are too sparse, too full or in pieces
/// are thrown away, so it reads as one shape and not as noise.
pub fn icon_from_seed(seed: u64) -> u64 {
    // How likely each column of the left half is lit, outer column first.
    const COLUMN: [u64; 4] = [30, 50, 62, 72];
    let mut state = seed;
    loop {
        let mut bits = 0u64;
        for y in 0..8 {
            let r = splitmix(&mut state);
            for (x, &column) in COLUMN.iter().enumerate() {
                let mut chance = column;
                if y == 0 || y == 7 {
                    chance -= 15;
                }
                if (r >> (x * 8)) & 0xff < chance * 256 / 100 {
                    bits |= 1 << (y * 8 + x);
                    bits |= 1 << (y * 8 + 7 - x);
                }
            }
        }
        if icon_reads_well(bits) {
            return bits;
        }
    }
}

fn icon_reads_well(bits: u64) -> bool {
    let lit = bits.count_ones();
    let rows = (0..8).filter(|y| (bits >> (y * 8)) & 0xff != 0).count();
    (20..=40).contains(&lit) && rows >= 6 && icon_in_one_piece(bits)
}

/// Every lit pixel reaches every other through lit neighbors, diagonals too.
fn icon_in_one_piece(bits: u64) -> bool {
    if bits == 0 {
        return true;
    }
    let mut seen = bits.isolate_lowest_one();
    let mut stack = vec![seen.trailing_zeros()];
    while let Some(i) = stack.pop() {
        let (x, y) = ((i % 8) as i32, (i / 8) as i32);
        for (dx, dy) in [
            (-1, -1),
            (0, -1),
            (1, -1),
            (-1, 0),
            (1, 0),
            (-1, 1),
            (0, 1),
            (1, 1),
        ] {
            let (nx, ny) = (x + dx, y + dy);
            if !(0..8).contains(&nx) || !(0..8).contains(&ny) {
                continue;
            }
            let bit = 1u64 << (ny * 8 + nx);
            if bits & bit != 0 && seen & bit == 0 {
                seen |= bit;
                stack.push((ny * 8 + nx) as u32);
            }
        }
    }
    seen == bits
}

/// `icon`, or a new one if another project already has it.
pub fn unique_icon(icon: u64, taken: impl Fn(u64) -> bool) -> u64 {
    let mut seed = icon;
    let mut icon = icon;
    while taken(icon) {
        seed = seed.wrapping_add(1);
        icon = icon_from_seed(seed);
    }
    icon
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn project_icons_are_mirrored_shapes_that_differ() {
        let mut seen = std::collections::HashSet::new();
        for n in 0..500 {
            let (_, icon) = logo_for(Path::new(&format!("/home/me/code/project-{n}")));
            assert!(icon_reads_well(icon));
            for y in 0..8 {
                let row = (icon >> (y * 8)) as u8;
                assert_eq!(row, row.reverse_bits(), "row {y} of {icon:#x} is mirrored");
            }
            seen.insert(icon);
        }
        assert!(seen.len() > 495, "only {} different icons", seen.len());
        assert_eq!(logo_for(Path::new("/a")), logo_for(Path::new("/a")));
    }

    #[test]
    fn a_taken_icon_is_drawn_again() {
        let first = icon_from_seed(1);
        let other = unique_icon(first, |i| i == first);
        assert_ne!(other, first);
        assert_eq!(unique_icon(first, |_| false), first);
    }

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

    #[test]
    fn a_name_picked_by_hand_is_not_renamed() {
        let mut env = Environment::new(
            "/tmp/arakun-placeholder-test".into(),
            Some("env/brave-otter".into()),
            false,
        );
        env.placeholder = Some("env/brave-otter".into());
        assert!(env.awaits_name());
        // `git branch -m` in a terminal, to a name that also looks random, then a refresh.
        env.set_branch(Some("env/bright-fox".into()));
        assert!(!env.awaits_name());
    }

    #[test]
    fn a_folder_outside_a_repository_has_no_branch() {
        let mut env = Environment::new(Remote::env_path("s1", "/srv/notes"), None, false);
        // A repository's checkout with no branch is on a detached HEAD.
        assert_eq!(env.branch.as_ref(), "detached HEAD");
        assert_eq!(env.checked_out(), None);
        // A plain folder has none at all, and its name labels it.
        env.set_no_branch();
        assert_eq!(env.branch.as_ref(), "");
        assert_eq!(env.name.as_ref(), "notes");
        assert_eq!(env.checked_out(), None);
        env.set_branch(Some("main".into()));
        assert_eq!(env.checked_out(), Some("main"));
    }

    #[test]
    fn an_environment_is_archived_once_every_session_is() {
        let mut env = Environment::new("/tmp/arakun-archive-test".into(), None, false);
        // A worktree waiting for its first session stays in the list.
        assert!(!env.archived());
        env.sessions
            .push(Session::new("a".into(), "A".into(), Agent::Claude));
        env.sessions
            .push(Session::new("b".into(), "B".into(), Agent::Claude));
        env.sessions[0].settled = true;
        assert!(!env.archived());
        env.sessions[1].settled = true;
        assert!(env.archived());
    }

    const HERE: &str = "/tmp/arakun-test";

    fn env_with(kinds: Vec<TabKind>) -> TabStrip {
        TabStrip {
            tabs: kinds
                .into_iter()
                .map(|k| Tab::of(Path::new(HERE), "t", k))
                .collect(),
            ..Default::default()
        }
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
        assert_eq!(env.split_other(Path::new(HERE)), Some(1));

        env.active_tab = 1;
        env.settle_split(Some(ids[0]));
        let split = env.split.unwrap();
        assert_eq!(split.other, ids[0]);
        assert!(
            !split.focus_first,
            "the focused pane moved to the second slot"
        );
        assert_eq!(env.split_other(Path::new(HERE)), Some(0));

        // Picking a third tab replaces the focused pane's content only.
        env.active_tab = 2;
        env.settle_split(Some(ids[1]));
        assert_eq!(env.split.unwrap().other, ids[0]);
        assert_eq!(env.split_other(Path::new(HERE)), Some(0));
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
        let no_companion = |_: &mut TabStrip| -> usize { unreachable!() };
        // Not split: b dropped on the right opens beside the active a.
        assert!(env.place_tab(Path::new(HERE), ids[1], false, no_companion));
        let left_right = |env: &TabStrip| {
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
        assert!(!env.place_tab(Path::new(HERE), ids[1], false, no_companion));
        assert!(env.place_tab(Path::new(HERE), ids[1], true, no_companion));
        assert_eq!(left_right(&env), (ids[1], ids[0]));
        // A third tab takes over the pane it lands on, either side.
        assert!(env.place_tab(Path::new(HERE), ids[2], false, no_companion));
        assert_eq!(left_right(&env), (ids[1], ids[2]));
        assert!(env.place_tab(Path::new(HERE), ids[0], true, no_companion));
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
        assert!(env.place_tab(Path::new(HERE), ids[0], false, |_| 1));
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
        assert_eq!(env.split_other(Path::new(HERE)), None);
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

    fn titles(env: &TabStrip) -> Vec<String> {
        env.tabs.iter().map(|t| name(&t.kind)).collect()
    }

    fn name(kind: &TabKind) -> String {
        match kind {
            TabKind::Browser { url } => url.to_string(),
            _ => String::new(),
        }
    }

    #[test]
    fn moving_a_tab_keeps_the_same_one_active() {
        let mut env = env_with(vec![browser("a"), browser("b"), browser("c")]);
        env.active_tab = 1;
        assert!(env.move_tab(0, 2));
        assert_eq!(titles(&env), ["b", "c", "a"]);
        assert_eq!(env.active_tab, 0);
        // Past the end lands last; the same place does nothing.
        assert!(env.move_tab(0, 9));
        assert_eq!(titles(&env), ["c", "a", "b"]);
        assert!(!env.move_tab(2, 2));
        assert!(!env.move_tab(5, 0));
    }

    #[test]
    fn closing_tabs_to_the_right_keeps_the_anchor_active() {
        let mut env = env_with(vec![browser("a"), browser("b"), browser("c"), browser("d")]);
        env.active_tab = 3;
        let other = env.tabs[2].id;
        env.split = Some(Split {
            other,
            dir: SplitDir::Right,
            ratio: 0.5,
            focus_first: true,
        });
        let closed = env.remove_tabs(1, |i| i > 1);
        let closed: Vec<(usize, String)> =
            closed.iter().map(|(i, t)| (*i, name(&t.kind))).collect();
        assert_eq!(closed, [(2, "c".into()), (3, "d".into())]);
        assert_eq!(titles(&env), ["a", "b"]);
        assert_eq!(env.active_tab, 1);
        assert!(env.split.is_none(), "the other pane's tab went");
    }

    #[test]
    fn closing_other_tabs_keeps_a_surviving_active_tab() {
        let mut env = env_with(vec![browser("a"), browser("b"), browser("c")]);
        env.active_tab = 2;
        env.remove_tabs(0, |i| i == 1);
        assert_eq!(titles(&env), ["a", "c"]);
        assert_eq!(env.active_tab, 1);
    }

    fn strip_of(envs: &[&str]) -> TabStrip {
        TabStrip {
            tabs: envs
                .iter()
                .map(|e| Tab::of(Path::new(e), *e, browser(e)))
                .collect(),
            ..Default::default()
        }
    }

    fn envs(strip: &TabStrip) -> Vec<String> {
        strip
            .tabs
            .iter()
            .map(|t| match &t.env {
                Some(env) => env.display().to_string(),
                None => "view".into(),
            })
            .collect()
    }

    #[test]
    fn only_the_active_tab_can_be_last_on_screen() {
        let a = Path::new("a");
        let mut strip = strip_of(&["a", "b"]);
        strip.tabs.push(Tab::new("Notes", TabKind::Notes));
        // a's only tab, on screen.
        assert!(strip.is_last_on_screen(0, a));
        // b's only tab, but another tab is on screen.
        assert!(!strip.is_last_on_screen(1, Path::new("b")));
        // A view on screen while a has no tab of its own.
        strip.tabs.remove(0);
        strip.active_tab = 1;
        assert!(strip.is_last_on_screen(1, a));
        // A second tab of a: either one can go.
        let mut strip = strip_of(&["a", "a", "b"]);
        assert!(!strip.is_last_on_screen(0, a));
        strip.active_tab = 1;
        assert!(!strip.is_last_on_screen(1, a));
    }

    #[test]
    fn a_new_tab_lands_after_the_tabs_of_its_environment() {
        let mut strip = strip_of(&["a", "a", "b"]);
        strip.active_tab = 2;
        let active = strip.tabs[2].id;
        assert_eq!(strip.add(Tab::new_session(Path::new("a"))), 2);
        assert_eq!(envs(&strip), ["a", "a", "a", "b"]);
        assert_eq!(strip.active().map(|t| t.id), Some(active));
        // The first tab of an environment, and the first view, go last.
        assert_eq!(strip.add(Tab::new_session(Path::new("c"))), 4);
        assert_eq!(strip.add(Tab::new("Notes", TabKind::Notes)), 5);
        assert_eq!(strip.add(Tab::new_session(Path::new("b"))), 4);
        assert_eq!(envs(&strip), ["a", "a", "a", "b", "b", "c", "view"]);
        assert_eq!(strip.active().map(|t| t.id), Some(active));
    }

    #[test]
    fn closing_the_active_tab_stays_in_its_environment_when_it_can() {
        let mut strip = strip_of(&["a", "a", "b", "b"]);
        // The last tab of `a`: its left neighbour, not the first of `b`.
        strip.active_tab = 1;
        strip.remove(1);
        assert_eq!(strip.active_tab, 0);
        // The only tab of `a`: the right neighbour takes over.
        strip.remove(0);
        assert_eq!((strip.active_tab, envs(&strip)[0].as_str()), (0, "b"));
        // A tab before the active one: the same tab stays active.
        strip.active_tab = 1;
        let active = strip.tabs[1].id;
        strip.remove(0);
        assert_eq!(strip.active().map(|t| t.id), Some(active));
        assert!(strip.remove(5).is_none());
    }

    #[test]
    fn an_environment_comes_back_on_the_tab_it_showed_last() {
        let mut strip = strip_of(&["a", "a", "b"]);
        assert_eq!(strip.front_of(Path::new("a")), Some(0));
        strip.front.insert("a".into(), strip.tabs[1].id);
        assert_eq!(strip.front_of(Path::new("a")), Some(1));
        // That tab closed: the first one left of the environment.
        strip.remove(1);
        assert_eq!(strip.front_of(Path::new("a")), Some(0));
        assert_eq!(strip.front_of(Path::new("c")), None);
    }

    #[test]
    fn the_tabs_of_an_environment_that_is_gone_close() {
        let mut strip = strip_of(&["a", "b", "a", "b"]);
        strip.active_tab = 3;
        let active = strip.tabs[3].id;
        let gone = strip.remove_where(|t| t.is_in(Path::new("a")));
        assert_eq!(gone.len(), 2);
        assert_eq!(envs(&strip), ["b", "b"]);
        assert_eq!(strip.active().map(|t| t.id), Some(active));
    }

    #[test]
    fn a_split_shows_only_with_its_own_environment() {
        let mut strip = strip_of(&["a", "a", "b"]);
        strip.tabs.push(Tab::new("Notes", TabKind::Notes));
        strip.split = Some(Split {
            other: strip.tabs[1].id,
            dir: SplitDir::Right,
            ratio: 0.5,
            focus_first: true,
        });
        assert_eq!(strip.split_other(Path::new("a")), Some(1));
        // On another environment it waits, and shows again on the way back.
        strip.active_tab = 2;
        assert_eq!(strip.split_other(Path::new("b")), None);
        assert!(strip.split.is_some());
        // A view fits beside any environment.
        strip.split.as_mut().unwrap().other = strip.tabs[3].id;
        assert_eq!(strip.split_other(Path::new("b")), Some(3));
    }

    #[test]
    fn an_environment_gets_its_split_back() {
        let mut strip = strip_of(&["a", "a", "b"]);
        strip.tabs.push(Tab::new("Notes", TabKind::Notes));
        let ids: Vec<u64> = strip.tabs.iter().map(|t| t.id).collect();
        let (a, b) = (Path::new("a"), Path::new("b"));
        strip.bring_split(a, None);
        strip.split = Some(Split {
            other: ids[3],
            dir: SplitDir::Right,
            ratio: 0.5,
            focus_first: true,
        });
        // On `b` the split of `a` is away, also when its other pane is a view.
        strip.active_tab = 2;
        strip.bring_split(b, Some(ids[0]));
        assert!(strip.split.is_none());
        assert_eq!(strip.split_other(b), None);
        // Back on the tab that had the focus: as it was.
        strip.active_tab = 0;
        strip.bring_split(a, Some(ids[2]));
        assert_eq!(strip.split_other(a), Some(3));
        assert!(strip.split.unwrap().focus_first);
        // Back on the other pane's tab: the panes trade roles.
        strip.active_tab = 2;
        strip.bring_split(b, Some(ids[0]));
        strip.active_tab = 3;
        strip.bring_split(a, Some(ids[2]));
        let split = strip.split.unwrap();
        assert_eq!((split.other, split.focus_first), (ids[0], false));
    }

    #[test]
    fn closed_tabs_are_capped() {
        let mut env = env_with(vec![browser("a")]);
        for i in 0..40 {
            env.remember_closed(i, TabKind::Notes, None);
        }
        assert_eq!(env.closed_tabs.len(), 25);
        assert_eq!(env.closed_tabs[0].index, 15, "the oldest went");
    }
}
