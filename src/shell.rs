//! The main window: project rail, workspace panel, title bar with environment
//! tabs, main pane, terminal dock and status bar.

use std::cell::{Cell, RefCell};
use std::ops::Range;
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use gpui::AnimationExt as _;
use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, App, AppContext as _, Bounds, ClipboardItem, Context, ElementInputHandler, Entity,
    EntityInputHandler, FocusHandle, Focusable, FontWeight, HighlightStyle, Hsla,
    InteractiveElement as _, IntoElement, KeyDownEvent, MouseButton, MouseDownEvent,
    MouseMoveEvent, ParentElement as _, Pixels, Render, ScrollHandle, SharedString,
    StatefulInteractiveElement as _, Styled as _, StyledText, Subscription, Task, TextLayout,
    UTF16Selection, UnderlineStyle, UniformListScrollHandle, Window, WindowControlArea, canvas,
    div, fill, img, point, px, relative, size, uniform_list,
};

mod active;
pub(crate) mod agent_cli;
mod arrange;
mod automations;
mod beta_card;
mod branches;
mod browsers;
mod checks;
mod cleanup;
mod cloudflare;
mod commands;
mod composer;
mod dock;
mod documents;
mod drag;
mod extensions;
mod git_actions;
mod github;
pub use github::open_terminal_link;
mod doop;
mod headless;
mod home;
mod inbox;
mod island;
mod limits;
mod linear;
mod linear_env;
mod linear_events;
mod linear_follow;
mod linear_outbox;
mod linear_run;
mod linear_servers;
mod linear_sync;
mod menus;
mod naming;
mod new_dialog;
mod notes;
mod onboarding;
pub(crate) mod pages;
mod palette;
mod panel;
mod panes;
mod phone;
mod pinned;
mod posthog;
mod project_icon;
mod rail;
mod rail_blocks;
mod rail_pick;
mod running;
mod runs;
mod servers;
mod session_actions;
mod session_tree;
mod sessions;
mod settle;
mod sidebar_builder;
#[cfg(feature = "coderlabs")]
mod site;
mod subagents;
mod subnav;
mod survey_card;
mod switching;
mod tab_views;
#[cfg(feature = "coderlabs")]
mod tasks;
#[cfg(feature = "team")]
mod team;
mod titlebar;
mod updates;

// Free helpers the split-out parts of the window share.
use documents::*;
use panel::*;
use rail::*;
use titlebar::*;

use crate::account::Account;
use crate::account_view::{self, AccountDialog, AccountDialogEvent};
use crate::agent::AgentKind;
use crate::attach_picker::{AttachEvent, AttachPicker};
use crate::ci::{self, Ci};
#[cfg(feature = "coderlabs")]
use crate::coderspace::CoderSpace;
use crate::customize_view::{CustomizeEvent, CustomizeTab, CustomizeView};
use crate::dialogs::{CreateEnvDialog, DeleteEnvDialog, DialogEvent};
use crate::explorer;
use crate::git;
use crate::github::GitHub;
use crate::highlight;
use crate::input::{InputEvent, TextInput};
use crate::model::{
    self, Document, Environment, Project, Status, Tab, TabKind, TermTab, pretty_path,
};
use crate::picker::{FolderPicker, PickerEvent};
use crate::settings_view::{SettingsDialog, SettingsEvent};
#[cfg(feature = "coderlabs")]
use crate::site_view::SiteView;
use crate::state;
#[cfg(feature = "coderlabs")]
use crate::tasks_view::TasksView;
#[cfg(feature = "team")]
use crate::team_view::{TeamEvent, TeamView};
use crate::terminal::{Terminal, TerminalEvent, TerminalView};
use crate::theme;
use crate::ui::{self, hover_icon, icon, icon_button};

gpui::actions!(
    arakun,
    [
        ToggleDock,
        NextEnvironment,
        PrevEnvironment,
        NewSession,
        AddProject,
        NewEnvironment,
        NewEnvironmentWithOptions,
        CloseTab,
        NewTerminal,
        NewBrowserTab,
        QuickSwitch,
        ToggleSidebar,
        TogglePanel,
        ToggleSubagents,
        OpenSettings,
        ToggleRedacted,
        GoBackTab,
        GoForwardTab,
        NextTab,
        PrevTab,
        SelectTab1,
        SelectTab2,
        SelectTab3,
        SelectTab4,
        SelectTab5,
        SelectTab6,
        SelectTab7,
        SelectTab8,
        SelectLastTab,
        SplitRight,
        SplitDown,
        FocusOtherPane,
        SwapPanes,
        ReopenClosedTab,
        MoveTabLeft,
        MoveTabRight,
        TogglePin,
        FindInChat,
        ZoomIn,
        ZoomOut,
        ZoomReset,
        CustomizeSidebar,
        OpenCustomize,
        SendFeedback
    ]
);

#[derive(Clone, Copy, PartialEq, Eq)]
enum PanelTab {
    Sessions,
    Explorer,
    Changes,
    /// The session's pull request and subagents, or the transcript of one.
    Session,
}

enum Modal {
    Picker(Entity<FolderPicker>),
    Attach(Entity<AttachPicker>),
    CreateEnv(Entity<CreateEnvDialog>),
    DeleteEnv(Entity<DeleteEnvDialog>),
    Command(Entity<crate::dialogs::CommandDialog>),
    Account(Entity<AccountDialog>),
    /// The rail's User row: the GitHub sign-in and Teams.
    Profile(Entity<crate::profile_dialog::ProfileDialog>),
    Commit(Entity<crate::git_dialogs::CommitDialog>),
    GitConfirm(Entity<crate::git_dialogs::ConfirmDialog>),
    PullRequest(Entity<crate::git_dialogs::PullRequestDialog>),
    Palette(Entity<crate::palette::Palette>),
    Settings(Entity<SettingsDialog>),
    Automations(Entity<crate::automations_view::AutomationsDialog>),
    /// Rename, remove or import sessions (`session_actions`).
    Session(gpui::AnyView),
    /// The first-launch question about crash reports and usage counts.
    Telemetry(Entity<crate::telemetry_dialog::TelemetryDialog>),
    /// Sign in, for Teams.
    TeamsSignIn(Entity<crate::arakun_teams::sign_in::SignInDialog>),
    /// A session or a project, from the + on the rail's Projects heading.
    New(Entity<new_dialog::NewDialog>),
    /// Connecting arakun-pilot, or changing its settings.
    Pilot(Entity<crate::github_view::connect::PilotDialog>),
    /// A PostHog survey, drawn natively (`survey_dialog`).
    Survey(Entity<crate::survey_dialog::SurveyDialog>),
}

#[derive(Clone, Copy)]
enum MenuTarget {
    Project(usize),
    Env(usize, usize),
    /// The composer's agent chip: the agents beside the model, reasoning and
    /// fast mode of the one hovered.
    ModelMenu,
    /// The composer's permissions chip: Claude's mode, Droid's mode and autonomy.
    PermissionMenu,
    /// Which providers the rail's usage card shows.
    RailUsage,
    /// Sessions across all projects that want attention, and past notices.
    Inbox,
    /// A session of the visible environment, by index, from its row or tab.
    Session(usize),
    /// A file of the Changes panel, by index into the visible environment's list.
    Change(usize),
    /// The active project's automations, from the rail.
    Automations,
    /// A tab of the visible environment, by index.
    Tab(usize),
    /// Which project a CoderSpace task should be worked on in (`Shell::task_start`).
    #[cfg(feature = "coderlabs")]
    TaskProject,
    /// An item of the rail by its segment and place, or the segment itself.
    RailBlock(usize, Option<usize>),
    /// A segment of the rail, from its title.
    RailSection(usize),
    /// Every project, to switch to, from the rail's open project.
    ProjectSwitch,
    /// A session of the sidebar's tree: project, environment, session.
    TreeSession(usize, usize, usize),
    /// Several rows of the projects list picked at once (`rail_pick`).
    Picked,
    /// Several sessions of the tree picked at once (`tree_pick`).
    TreePicked,
    /// The order of the tree's sessions and what it lists, from the button
    /// in its heading.
    TreeSort,
    /// The pinned tabs that don't fit the title bar.
    MoreTabs,
    /// The tree of sessions, from the title bar while the sidebar is hidden.
    Sessions,
    /// The rail's Help: the feedback forms and the docs.
    Help,
    /// The local servers that run, from the status bar or the rail.
    Running,
}

/// An environment made with one click, shown while its worktree is made.
struct PendingEnv {
    /// The project's repository.
    repo: std::path::PathBuf,
    name: String,
}

struct ContextMenu {
    target: MenuTarget,
    at: gpui::Point<Pixels>,
}

/// A row of a composer or rail menu, as both the mouse and the keys reach it.
#[derive(Clone, Debug, PartialEq)]
enum MenuKey {
    /// A setting's choice for an agent: (agent, setting, choice id, can be
    /// picked). Another agent's model switches to it; its other settings are
    /// only remembered while a session runs, and switch on the New session tab.
    Choice(AgentKind, crate::agent::Setting, String, bool),
    /// The reasoning row of the model menu; ← and → step through its levels.
    Effort(AgentKind),
    /// Fast mode, with whether it is on now.
    Fast(AgentKind, bool),
    /// Back to the agent's default model, reasoning and fast mode.
    Reset(AgentKind),
    /// Switches the composer to the agent, with the settings it remembers.
    Agent(AgentKind),
    /// Shows or hides a provider in the rail's usage card.
    Rail(AgentKind),
    OpenUsage,
    /// Puts the tree's sessions in this order.
    TreeSort(crate::tab_layout::Sort),
    /// Whether each project heads its own sessions in the tree.
    TreeGroup,
    /// Whether the tree lists archived sessions too.
    TreeArchived,
}

/// Opens a view, once what it needs is there (`Shell::pick_project_for`).
type OpenView = fn(&mut Shell, &mut Window, &mut Context<Shell>);

pub struct Shell {
    projects: Vec<Project>,
    /// The one row of tabs, for every project and environment.
    strip: model::TabStrip,
    /// The active tab brought another environment on screen: `render` gives
    /// it its terminal and starts its sessions, which need the window.
    env_followed: bool,
    /// The latest `refresh_project` of each project root. An older one that ends
    /// later would put back what it read before, such as a branch renamed since.
    refresh_gen: std::collections::HashMap<std::path::PathBuf, u64>,
    /// A line for the next prompt to an environment's sessions after arakun renamed
    /// its branch: they started on the old name.
    rename_notes: std::collections::HashMap<std::path::PathBuf, String>,
    /// Where a server's first prompt put back in an environment's composer was
    /// written: what it mentions is still looked for there when it is sent again.
    mention_sources: std::collections::HashMap<std::path::PathBuf, std::path::PathBuf>,
    /// Sessions whose transcript is being read off the main thread, to show them.
    restoring: std::collections::HashSet<SharedString>,
    /// Reads in `restoring` an `ensure_live` beat: when one ends, its older
    /// transcript is dropped instead of put back over the newer session.
    restoring_stale: std::collections::HashSet<SharedString>,
    /// Transcripts read while the pointer rests on their row (`switching`).
    ahead: std::collections::HashMap<SharedString, switching::Ahead>,
    /// The thread the pane drew last, to fade in the next (`switching`).
    shown: RefCell<switching::Shown>,
    active_project: usize,
    dock_open: bool,
    dock_height: f32,
    /// Set while the dock's top edge is being dragged.
    dock_drag: bool,
    /// Set while a side card's gap handle is being dragged.
    side_drag: Option<SideDrag>,
    env_menu_open: bool,
    /// Environments made with one click whose worktree is not there yet.
    pending_envs: Vec<PendingEnv>,
    /// Environments made for a named branch, still being made (`linear_env`).
    linear_envs: linear_env::InFlight<linear_env::Waiter>,
    /// Where the environment menu was opened from below (the composer); `None`
    /// anchors it under the panel header.
    env_menu_at: Option<gpui::Point<Pixels>>,
    tab_scroll: ScrollHandle,
    /// The tab last scrolled into view in the title bar, so a new active tab is too.
    tab_revealed: Cell<Option<u64>>,
    /// A check is queued for keyboard focus that went nowhere (see `render`).
    refocus_queued: bool,
    modal: Option<(Modal, Subscription)>,
    /// The popover survey card and what its clock knows (`shell::survey_card`).
    survey: survey_card::CardState,
    /// Refreshes the open palette when GitHub brings in more.
    palette_watch: Option<Subscription>,
    /// Each project's `owner/name` on GitHub (None when it has none), by
    /// directory. Read in the background: finding it runs `git`, and the
    /// palette asks for it on every GitHub update.
    repo_slugs: std::collections::HashMap<std::path::PathBuf, Option<String>>,
    /// The background read of `repo_slugs`; a new one replaces it.
    repo_slugs_read: Option<Task<()>>,
    menu: Option<ContextMenu>,
    /// Polls `git status` of the visible environment while the window is active.
    poll: Option<Task<()>>,
    /// When the last background `git fetch` ended, so the network is asked rarely.
    last_fetch: Option<Instant>,
    /// A background `git fetch` is running; one at a time.
    fetching: bool,
    /// The polled git reads running now, so a slow one is not started again.
    git_reads: git_actions::GitReads,
    /// Diff tabs whose file lost its changes; closed from `render` (needs a window).
    stale_diffs: Vec<(std::path::PathBuf, String)>,
    /// The ASCII face over the start and welcome screens (redraws the shell
    /// while its intro fades the screen in) and the small raccoon atop the
    /// rail, which steps aside while a raccoon shows in the main view.
    home_face: Entity<crate::ascii_face::AsciiFace>,
    _home_face: Subscription,
    rail_mascot: Entity<crate::mascot::Mascot>,
    focus: FocusHandle,
    /// The composer: each environment's draft, its caret, history and completion.
    composer: Entity<composer::Composer>,
    /// The composer's focus, kept here to focus it without reading the entity.
    composer_focus: FocusHandle,
    /// The composer's events, and its focus: focusing it claims its tab,
    /// leaving it lets the tab go.
    _composer: [Subscription; 3],
    _activation: Subscription,
    /// Where the window is, saved with the state so it reopens there.
    window_state: Option<state::WindowState>,
    _bounds: Subscription,
    _quit: [Subscription; 2],
    /// `arakun restart` asked to quit: save for the next arakun and launch it.
    restarting: bool,
    /// The `update-policy` feature flag's dialog (`shell::updates`), over
    /// everything else, dialogs included.
    update_dialog: Option<(Entity<crate::update_dialog::UpdateDialog>, Subscription)>,
    /// Reports agents sent with `arakun feedback`, per environment.
    feedback_limits: crate::surveys::agent::RateLimit,
    /// The pages last marked visible, so tab switches can tell when to update CEF.
    shown_browsers: Vec<gpui::EntityId>,
    /// Answers `arakun <command>` from the control socket.
    _control: Option<Task<()>>,
    /// Runs scheduled automations while the window is open.
    _scheduler: Option<Task<()>>,
    /// Merges seen and settled (`settle`).
    settle: settle::Watch,
    cleanup: cleanup::Watch,
    /// Set while the divider between split panes is being dragged.
    split_drag: bool,
    /// Bounds of the split panes as last painted, for dragging the divider.
    split_bounds: Rc<Cell<Bounds<Pixels>>>,
    browsers_dirty: bool,
    account: Entity<Account>,
    _account: Subscription,
    /// The Teams sign-in, and whether the server lets this account in.
    teams: Entity<crate::arakun_teams::Teams>,
    teams_view: Entity<crate::arakun_teams::view::TeamsView>,
    _teams: [Subscription; 2],
    /// The project rail on the left: open, width, picks and the session tree.
    rail: rail::Rail,
    /// The Workspace panel on the right: open, width, tab and Customize.
    panel: panel::Panel,
    /// What the rail shows, in the user's order.
    sidebar: crate::sidebar::Sidebar,
    /// The sidebar builder tab's selection, fields and undo.
    builder: sidebar_builder::Builder,
    /// The widths the rail and workspace panel are drawn at this frame: the
    /// dragged ones, narrowed when the window is too small to leave the work
    /// in the middle `MAIN_MIN_W` (see `fit_sides`).
    side_fit: Cell<[f32; 2]>,
    /// The workspace panel, open but left out this frame because the window
    /// is too narrow for it (see `crowd_out`).
    side_crowded: Cell<bool>,
    /// The Session tab's subagent state: the one open, its redraw, dismissal.
    subagents: subagents::Subagents,
    /// The session on screen when its branch's pull request was last looked
    /// at, and whether it had one: a read that finds one counts as connecting
    /// it, a switch to a session that already has one does not.
    branch_pr_seen: Option<(SharedString, bool)>,
    arcade: bool,
    /// What the person pinned from the palette (`pins.json`).
    pins: crate::palette::pins::Pins,
    /// Automations made from a sentence in the palette (`rules.json`).
    rules: crate::palette::rules::Rules,
    session_query: Entity<TextInput>,
    session_filter: Option<AgentKind>,
    /// The sessions list shows the ones settled after a merge too.
    show_settled: bool,
    _session_query: Subscription,
    /// Input of the Notes tab; Enter appends a note to the active project.
    notes_input: Entity<TextInput>,
    _notes_input: Subscription,
    #[cfg(feature = "coderlabs")]
    coderspace: Entity<CoderSpace>,
    #[cfg(feature = "coderlabs")]
    tasks_view: Entity<TasksView>,
    /// A task waiting for its project to be picked, and the agent to work on it.
    #[cfg(feature = "coderlabs")]
    task_start: Option<(String, AgentKind)>,
    #[cfg(feature = "coderlabs")]
    _tasks: [Subscription; 3],
    ci: Entity<Ci>,
    /// The views of the one-page tabs, with their subscriptions.
    views: tab_views::TabViews,
    #[cfg(feature = "coderlabs")]
    site_view: Entity<SiteView>,
    #[cfg(feature = "coderlabs")]
    _site_view: Subscription,
    #[cfg(feature = "team")]
    team_view: Entity<TeamView>,
    #[cfg(feature = "team")]
    _team_view: Subscription,
    linear_sync: linear_sync::LinearSync,
    /// Sessions started from Linear, and the reports Linear has not taken yet.
    linear_runs: crate::linear::runs::Runs,
    linear_outbox: linear_outbox::Outbox,
    /// Deliveries being worked on, by id, with their agent session: a second
    /// frame waits for the first, and a stop for the work to land.
    linear_in_flight: std::collections::HashMap<String, String>,
    /// Stops that came while other work for their agent session was in flight.
    linear_held_stops: Vec<crate::phone::linear::Delivery>,
    /// Deliveries that came before the Arakun account was loaded.
    linear_kept: Vec<crate::phone::linear::Delivery>,
    /// New issues being read from Linear now.
    linear_reads: usize,
    /// The timer that tells Linear a quiet session still works is on.
    linear_watching: bool,
    github: Entity<GitHub>,
    pilot: Entity<crate::github::pilot::Pilot>,
    _github: [Subscription; 4],
    /// The phone's server and what each phone watches, while the phone is on.
    phone: Option<phone::PhoneLink>,
    /// The phone's keys on their way from the keyring.
    phone_keys: phone::PhoneKeys,
    /// The account's signed-in devices, for the Phone tab's relay section.
    signed_in: phone::SignedIn,
    beta_card: beta_card::BetaCard,
    /// The account `relay_owner_changed` last settled the relay's devices for.
    relay_owner_seen: Option<crate::phone::AccountNow>,
    /// The connections to the machines running `arakun server`, once the
    /// Computers tab was opened.
    servers: Option<servers::ServersLink>,
    #[cfg(feature = "coderlabs")]
    canvas_view: Entity<crate::canvas_view::CanvasView>,
    /// (project root, branch) → the fix session whose push CI is judging.
    fix_loops: std::collections::HashMap<(std::path::PathBuf, String), runs::FixLoop>,
    /// The project check's state per session id.
    checks: std::collections::HashMap<String, checks::CheckLoop>,
    review_draft: Option<notes::ReviewDraft>,
    /// The session card being dragged.
    lifted: Option<SharedString>,
    /// The tab being dragged onto a pane, while a drag is on.
    lifted_tab: Option<u64>,
    /// The gap in the tab bar a dragged tab would drop into: before the tab
    /// at this index, or after the last one.
    tab_drop: Option<usize>,
    /// The tab whose side marked `tab_drop`.
    tab_drop_over: Option<Bounds<Pixels>>,
    /// The session on screen pinned itself, or something was pinned: after
    /// that, nothing pins by itself in this window.
    pins_seeded: bool,
    /// The local servers found by the last look (`shell::running`).
    running: Vec<crate::running::Server>,
    /// A look is under way.
    running_scanning: bool,
    /// Look once more when the one under way ends.
    running_again: bool,
    running_checked: Option<Instant>,
    /// The pinned tabs drawn under "N more" in the last frame.
    bar_more: RefCell<Vec<u64>>,
    /// Where the pinned tabs' part of the title bar was in the last frame.
    bar_area: Rc<Cell<Bounds<Pixels>>>,
    slide: Option<drag::SessionSlide>,
    drop_menu: Option<drag::DropMenu>,
    /// Dropped sessions still being read for each environment's composer.
    dropping: std::collections::HashMap<std::path::PathBuf, usize>,
    /// The agent whose models the model menu shows; `None` is the composer's.
    menu_agent: Option<AgentKind>,
    /// The row the arrow keys are on, in `menu_keys` order.
    menu_cursor: Option<usize>,
    /// The reasoning level under the pointer, by its id: the meter shows it
    /// lit as a preview, and its name, until the pointer leaves.
    effort_hover: Option<String>,
    /// The model list's scroll, kept on the row the keys are on.
    menu_scroll: ScrollHandle,
    /// The alert level (`limits::level`) each plan window last notified at.
    limit_alerts: std::collections::HashMap<String, u8>,
    /// The first limit check only notes the levels; later ones notify.
    limits_seen: bool,
    _runs: [Subscription; 2],
    /// The Dynamic Island, while it is on and the compositor has layer-shell.
    island: Option<island::IslandLink>,
    #[cfg(feature = "team")]
    team: Option<team::TeamLink>,
    /// Every notice shown, for the Inbox. A cell: notices go out from `&self`.
    notices: RefCell<crate::notices::History>,
    /// The Inbox was open at the last frame: when it closes, what it showed counts as seen.
    inbox_open: bool,
    /// The view shown in the welcome's place: one that needs no project,
    /// opened before any was added.
    alone: Option<home::Alone>,
    /// A sign-in's terminal, in the welcome's place: no project was added.
    terminal_alone: Option<dock::TerminalAlone>,
    /// `History::arrived` when the Inbox last drew: what it showed.
    inbox_shown: Cell<u64>,
    /// "Clear" was clicked once; the second click empties the history.
    clear_armed: bool,
    /// The setup, while it shows in the window's place (`shell::onboarding`).
    onboarding: Option<onboarding::Onboarding>,
    /// A few keys on the start screen, right after someone new finishes the
    /// setup, until they close them.
    setup_tips: bool,
    /// Someone asked the setup to pair the phone before they had a project:
    /// the Phone tab opens once the first one is added.
    phone_after_project: bool,
}

struct InboxItem {
    project: usize,
    env: usize,
    id: SharedString,
    status: Status,
    title: SharedString,
    /// The Linear issue it was started for.
    linear: Option<SharedString>,
    place: SharedString,
    updated: u64,
}

const DOCK_MIN_H: f32 = 120.;
/// What the dock leaves above it for the work, however tall it was dragged.
const DOCK_ROOM_ABOVE: f32 = 160.;

/// Whether to ask after a crash. Not on first launch, before the answer to the
/// consent question, and not when nothing can be sent: the notice would lead
/// to a dead end.
fn crash_notice_wanted(should_ask: bool, blocked: bool) -> bool {
    !should_ask && !blocked
}

/// The tallest the dock can be in a window `viewport_h` tall: its bottom edge
/// is one gap above the window's.
fn dock_max_h(viewport_h: f32) -> f32 {
    (viewport_h - gap() - DOCK_ROOM_ABOVE).max(DOCK_MIN_H)
}
/// How narrow and wide the side cards can be dragged.
const RAIL_W_RANGE: std::ops::Range<f32> = 160. ..320.;
const PANEL_W_RANGE: std::ops::Range<f32> = 200. ..480.;
/// How wide the panel gets while it shows a subagent's transcript, which
/// reads badly at the panel's usual width.
const SUBAGENT_TRANSCRIPT_W: f32 = 360.;
/// How narrow the work between the side cards may get before they give way.
const MAIN_MIN_W: f32 = 420.;
/// Below this, even at their narrowest the side cards leave too little for
/// the work, and the workspace panel is left out. A 1366x768 laptop at 125%
/// scale, or a window snapped to half a screen, is that narrow.
const MAIN_FLOOR_W: f32 = 320.;

/// Whether the open workspace panel is left out so the cards' narrowest
/// widths fit in `room`. The rail always stays: it holds the projects and the
/// way back to everything else. With `keep_panel` the panel stays too:
/// Customize sits there, and a change made in it (a wider gap, a bigger text
/// size) must not take away the controls that undo it. Each card is its
/// minimum width plus its handle; a closed panel is `None`.
fn crowd_out(room: f32, rail: f32, panel: Option<f32>, keep_panel: bool) -> bool {
    !keep_panel && panel.is_some_and(|p| rail + p > room)
}

/// Side card widths that leave `room` for the cards together. Each one gives
/// up width in proportion to how far it is above its own minimum, and none
/// goes below it: on a window too small even for that, the middle shrinks.
/// `cards` holds (dragged width, minimum); a hidden card is (0, 0).
fn fit_sides<const N: usize>(room: f32, cards: [(f32, f32); N]) -> [f32; N] {
    let want: f32 = cards.iter().map(|c| c.0).sum();
    let slack: f32 = cards.iter().map(|c| (c.0 - c.1).max(0.)).sum();
    if want <= room || slack <= 0. {
        return cards.map(|c| c.0);
    }
    let keep = 1. - ((want - room) / slack).min(1.);
    cards.map(|(w, min)| (min + (w - min).max(0.) * keep).min(w).round())
}
/// Air around and between the window's floating cards, set in Settings ›
/// Appearance. The window's edges, the room above the dock and the resize
/// handles between side cards all use this one value, so they stay equal.
/// It is its own setting, so Spacing does not scale it.
fn gap() -> f32 {
    crate::ui::window_gap()
}

/// How wide a side handle can be grabbed, whatever the window gap: with no
/// gap there would be nothing to grab between the cards.
const SIDE_HANDLE_MIN_W: f32 = 6.;

/// `gap()` as a length, for the root's padding and the side handles.
fn gap_px() -> gpui::Pixels {
    crate::ui::window_gap_px()
}

/// The dock's terminal tabs are all one width.
const DOCK_TAB_W: f32 = 224.;
/// Where a dock tab's 20 px close button sits, centered on the tab.
const DOCK_TAB_CLOSE_TOP: f32 = (ui::DOCK_TAB_H - 20.) / 2.;

/// Which side card its gap handle is resizing.
#[derive(Clone, Copy, PartialEq)]
enum SideDrag {
    Rail,
    Panel,
}
const POLL: Duration = Duration::from_secs(3);
const POLL_IDLE: Duration = Duration::from_secs(15);
/// A `git fetch` of the visible environment at most this often while the window is
/// active, so the pull count notices commits pushed elsewhere.
const FETCH_EVERY: Duration = Duration::from_secs(120);

impl Shell {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let activation = cx.observe_window_activation(window, |this, window, cx| {
            this.sync_poll(window, cx);
            let active = window.is_window_active();
            if active {
                this.refresh_visible(cx);
                // Back at the window: what it shows was seen, so a notice
                // waiting for the person to leave would come too late.
                this.phone_seen_on_screen();
                // The system's settings may have changed while Arakun was behind.
                crate::ui::contrast::follow_system(cx);
                crate::ui::motion::follow_system(cx);
            }
            #[cfg(feature = "coderlabs")]
            this.coderspace
                .update(cx, |cs, cx| cs.set_window_active(active, cx));
            this.ci
                .update(cx, |ci, cx| ci.set_window_active(active, cx));
            this.github
                .update(cx, |g, cx| g.set_window_active(active, cx));
            this.pilot
                .update(cx, |p, cx| p.set_window_active(active, cx));
            this.sync_teams_shown(window, cx);
            this.sync_polled_shown(window, cx);
        });
        let focus = cx.focus_handle();
        let (projects, active_project, layout, strip, drafts) = state::restore(state::load());
        let composer = cx.new(|cx| composer::Composer::new(drafts, cx));
        let composer_focus = composer.read(cx).focus.clone();
        let composer_subs = [
            cx.subscribe_in(&composer, window, |this, _, ev, window, cx| {
                this.on_composer_event(ev, window, cx)
            }),
            cx.on_focus(&composer_focus, window, |this, window, cx| {
                this.claim_composer_tab(window, cx)
            }),
            cx.on_blur(&composer_focus, window, |this, window, cx| {
                this.spy_from_composer(window, cx)
            }),
        ];
        crate::agent::sandbox::set_projects(projects.iter().flat_map(|p| {
            std::iter::once(&p.path)
                .chain(p.environments.iter().map(|e| &e.path))
                .map(|path| crate::agent::sandbox::ProjectFolder {
                    path: path.clone(),
                    sandbox: p.sandbox,
                    pass: p.pass_env.clone(),
                    secrets: p.secrets.clone(),
                })
        }));
        theme::set_diff_palette(layout.diff_palette);
        theme::set_glass(layout.glass);
        theme::set_contrast(layout.contrast);
        theme::set_grays(layout.grays);
        crate::palettes::apply_saved();
        account_view::set_rail_shows(AgentKind::Claude, layout.rail_usage[0]);
        account_view::set_rail_shows(AgentKind::Droid, layout.rail_usage[1]);
        account_view::set_rail_shows(AgentKind::OpenCode, layout.rail_usage[2]);
        account_view::set_rail_shows(AgentKind::Devin, layout.rail_usage[3]);
        crate::ui::set_type_scale(layout.type_scale);
        if layout.ui_zoom != crate::ui_zoom::pct() {
            crate::ui_zoom::set_pct(layout.ui_zoom);
            window.bounds_changed(cx);
        }
        if theme::glass() {
            window.set_background_appearance(theme::window_background());
        }
        let account = cx.new(Account::new);
        let account_sub = cx.observe(&account, |this, _, cx| {
            this.check_limit_alerts(cx);
            this.phone_usage_changed(cx);
        });
        let teams = cx.new(crate::arakun_teams::Teams::new);
        let teams_view = cx.new(|cx| crate::arakun_teams::view::TeamsView::new(teams.clone(), cx));
        let teams_subs = [
            cx.observe_in(&teams, window, |this, _, window, cx| {
                this.teams_changed(cx);
                this.account_changed(window, cx);
                this.linear_account_changed(window, cx);
            }),
            cx.subscribe_in(
                &teams_view,
                window,
                |this, _, ev: &crate::arakun_teams::view::TeamsViewEvent, window, cx| match ev {
                    crate::arakun_teams::view::TeamsViewEvent::OpenProfile => {
                        this.open_profile(None, window, cx);
                    }
                },
            ),
        ];
        let session_query = cx.new(|cx| TextInput::new("Search sessions...", cx));
        let session_query_sub =
            cx.subscribe(&session_query, |_, input, ev: &InputEvent, cx| match ev {
                InputEvent::Changed => cx.notify(),
                InputEvent::Cancel => input.update(cx, |i, cx| i.set_text("", cx)),
                _ => {}
            });
        let rail = rail::Rail::new(&layout, cx);
        let panel = panel::Panel::new(&layout);
        let notes_input = cx.new(|cx| TextInput::new("Add a note and press Enter...", cx));
        let notes_input_sub = cx.subscribe(&notes_input, |this, input, ev: &InputEvent, cx| {
            if let InputEvent::Submit = ev {
                let text = input.read(cx).text().trim().to_string();
                // A server's own project is not saved, so its notes would be lost.
                if !text.is_empty() && this.has_projects() && !this.project().is_remote() {
                    this.projects[this.active_project].notes.push(text);
                    this.save(cx);
                }
                input.update(cx, |i, cx| i.set_text("", cx));
                cx.notify();
            }
        });
        #[cfg(feature = "coderlabs")]
        let coderspace = cx.new(CoderSpace::new);
        #[cfg(feature = "coderlabs")]
        let tasks_view = cx.new(|cx| TasksView::new(coderspace.clone(), window, cx));
        #[cfg(feature = "coderlabs")]
        let tasks_subs = [
            cx.observe(&coderspace, |_, _, cx| cx.notify()),
            cx.subscribe_in(&tasks_view, window, |this, _, ev, window, cx| {
                this.on_tasks_event(ev, window, cx)
            }),
            cx.subscribe_in(&coderspace, window, |this, _, ev, window, cx| {
                this.on_coderspace_event(ev, window, cx)
            }),
        ];
        let ci = cx.new(Ci::new);
        let runs_subs = [
            cx.observe(&ci, |this, _, cx| {
                this.branch_pr_read(cx);
                this.linear_links_from_ci(cx);
                cx.notify()
            }),
            cx.subscribe_in(&ci, window, |this, _, ev, window, cx| {
                this.on_ci_event(ev, window, cx)
            }),
        ];
        #[cfg(feature = "coderlabs")]
        let canvas_view = cx.new(crate::canvas_view::CanvasView::new);
        #[cfg(feature = "coderlabs")]
        let site_view = cx.new(SiteView::new);
        #[cfg(feature = "coderlabs")]
        let site_sub = cx.subscribe_in(&site_view, window, |this, _, ev, window, cx| {
            this.on_site_event(ev, window, cx)
        });
        #[cfg(feature = "team")]
        let team_view = cx.new(TeamView::new);
        #[cfg(feature = "team")]
        let team_sub = cx.subscribe_in(
            &team_view,
            window,
            |this, _, ev: &TeamEvent, window, cx| match ev {
                TeamEvent::Open(url) => this.open_url(url.clone(), window, cx),
                TeamEvent::Reload => this.sync_team(window, cx),
            },
        );
        let sidebar = crate::sidebar::Sidebar::load();
        let views = crate::sidebar::Extensions::load(&sidebar);
        let github = cx.new(GitHub::new);
        let pilot = cx.new(crate::github::pilot::Pilot::new);
        let tab_views = tab_views::TabViews::new(&ci, &github, &pilot, views, window, cx);
        let github_subs = [
            cx.observe(&pilot, |_, _, cx| cx.notify()),
            cx.subscribe_in(&pilot, window, |this, _, ev, window, cx| {
                this.on_pilot_event(ev, window, cx)
            }),
            // The rail badge counts what the model holds.
            cx.observe(&github, |_, _, cx| cx.notify()),
            cx.subscribe_in(&github, window, |this, _, ev, window, cx| {
                this.on_github_event(ev, window, cx)
            }),
        ];
        if projects.is_empty() {
            window.focus(&focus, cx);
        } else {
            window.focus(&composer_focus, cx);
        }
        crate::mascot::set_animated(layout.arcade);
        crate::ui::motion::apply(cx);
        let home_face = cx.new(crate::ascii_face::AsciiFace::new);
        let ui_alpha = Cell::new(1.);
        let home_face_sub = cx.observe(&home_face, move |_, face, cx| {
            let a = face.read(cx).ui_alpha();
            if a != ui_alpha.replace(a) {
                cx.notify();
            }
        });
        let mut this = Self {
            projects,
            strip,
            env_followed: false,
            refresh_gen: Default::default(),
            rename_notes: Default::default(),
            mention_sources: Default::default(),
            restoring: Default::default(),
            restoring_stale: Default::default(),
            ahead: Default::default(),
            shown: Default::default(),
            active_project,
            dock_open: layout.dock_open,
            dock_height: layout.dock_height,
            dock_drag: false,
            side_drag: None,
            env_menu_open: false,
            pending_envs: Vec::new(),
            linear_envs: Default::default(),
            env_menu_at: None,
            tab_scroll: ScrollHandle::new(),
            tab_revealed: Cell::new(None),
            refocus_queued: false,
            modal: None,
            survey: survey_card::CardState::new(Instant::now()),
            palette_watch: None,
            repo_slugs: Default::default(),
            repo_slugs_read: None,
            menu: None,
            poll: None,
            last_fetch: None,
            fetching: false,
            git_reads: Default::default(),
            stale_diffs: Vec::new(),
            home_face,
            _home_face: home_face_sub,
            rail_mascot: cx.new(|cx| crate::mascot::Mascot::new(24., cx)),
            focus,
            composer,
            composer_focus,
            _composer: composer_subs,
            _activation: activation,
            window_state: layout.window,
            _bounds: cx
                .observe_window_bounds(window, |this, window, cx| this.track_window(window, cx)),
            shown_browsers: Vec::new(),
            _control: None,
            _scheduler: None,
            settle: settle::Watch::default(),
            cleanup: cleanup::Watch::default(),
            split_drag: false,
            split_bounds: Rc::default(),
            browsers_dirty: false,
            account,
            _account: account_sub,
            teams,
            teams_view,
            _teams: teams_subs,
            rail,
            panel,
            sidebar,
            builder: Default::default(),
            side_fit: Cell::new([layout.rail_w, layout.panel_w]),
            side_crowded: Cell::new(false),
            subagents: Default::default(),
            branch_pr_seen: None,
            arcade: layout.arcade,
            pins: crate::palette::pins::Pins::load(),
            rules: crate::palette::rules::Rules::load(),
            session_query,
            session_filter: None,
            show_settled: false,
            _session_query: session_query_sub,
            notes_input,
            _notes_input: notes_input_sub,
            #[cfg(feature = "coderlabs")]
            coderspace,
            #[cfg(feature = "coderlabs")]
            tasks_view,
            #[cfg(feature = "coderlabs")]
            task_start: None,
            #[cfg(feature = "coderlabs")]
            _tasks: tasks_subs,
            ci,
            views: tab_views,
            #[cfg(feature = "coderlabs")]
            site_view,
            #[cfg(feature = "coderlabs")]
            _site_view: site_sub,
            #[cfg(feature = "team")]
            team_view,
            #[cfg(feature = "team")]
            _team_view: team_sub,
            linear_sync: Default::default(),
            linear_runs: crate::linear::runs::Runs::load(),
            linear_outbox: Default::default(),
            linear_in_flight: Default::default(),
            linear_held_stops: Vec::new(),
            linear_kept: Vec::new(),
            linear_reads: 0,
            linear_watching: false,
            github,
            pilot,
            _github: github_subs,
            phone: None,
            phone_keys: Default::default(),
            signed_in: Default::default(),
            beta_card: beta_card::BetaCard::new(cx),
            relay_owner_seen: None,
            servers: None,
            #[cfg(feature = "coderlabs")]
            canvas_view,
            fix_loops: std::collections::HashMap::new(),
            checks: std::collections::HashMap::new(),
            review_draft: None,
            lifted: None,
            lifted_tab: None,
            tab_drop: None,
            tab_drop_over: None,
            pins_seeded: false,
            running: Vec::new(),
            running_scanning: false,
            running_again: false,
            running_checked: None,
            bar_more: RefCell::default(),
            bar_area: Rc::default(),
            slide: None,
            drop_menu: None,
            dropping: std::collections::HashMap::new(),
            menu_agent: None,
            menu_cursor: None,
            effort_hover: None,
            menu_scroll: ScrollHandle::new(),
            limit_alerts: std::collections::HashMap::new(),
            limits_seen: false,
            island: None,
            #[cfg(feature = "team")]
            team: None,
            notices: RefCell::new(crate::notices::load()),
            inbox_open: false,
            onboarding: None,
            setup_tips: false,
            phone_after_project: false,
            alone: None,
            terminal_alone: None,
            inbox_shown: Cell::new(0),
            clear_armed: false,
            restarting: false,
            update_dialog: None,
            feedback_limits: Default::default(),
            _runs: runs_subs,
            _quit: [
                cx.on_app_quit(|this, cx| {
                    // A draft typed in the last moments may not be saved yet.
                    // Replaces any save still queued, so none can land over it.
                    state::queue(this.state_bytes(cx));
                    if let Err(e) = state::write_queued() {
                        eprintln!("arakun: could not save state: {e}");
                    }
                    this.save_transcripts(cx);
                    // Notices are saved off the UI thread: let the last one land.
                    crate::notices::flush();
                    // Settings switched, or a sidebar, pin, to-do or phone
                    // change made, in the last moments.
                    crate::durable::flush();
                    // Up to 2 s for usage events still queued.
                    crate::telemetry::quit();
                    crate::control::cleanup();
                    // Before the restart below, so it starts the new version.
                    match crate::update::apply_staged() {
                        Ok(Some(tag)) => {
                            eprintln!("arakun: installed Arakun {tag}");
                            #[cfg(windows)]
                            crate::island_host::link::restart_after_update();
                        }
                        Ok(None) => {}
                        Err(e) => eprintln!("arakun: {e}"),
                    }
                    if this.restarting
                        && let Err(e) = crate::platform::relaunch()
                    {
                        eprintln!("arakun: could not start again: {e}");
                    }
                    async {}
                }),
                cx.on_release(|this, cx| this.save_transcripts(cx)),
            ],
        };
        for pi in 0..this.projects.len() {
            this.refresh_project(pi, cx);
        }
        this.sync_ci_roots(cx);
        this.push_linear_host(cx);
        this.sync_poll(window, cx);
        // Before any session is made live: none of them runs yet.
        this.linear_recover(window, cx);
        this.ensure_terminal(window, cx);
        this.ensure_active_live(window, cx);
        this.resume_after_restart(window, cx);
        // Taken from the state as loaded: a refresh can save one without a
        // worktree removed outside Arakun before the sweep reads it.
        let listed = this
            .projects
            .iter()
            .flat_map(|p| &p.environments)
            .flat_map(|e| &e.sessions)
            .flat_map(sessions::session_file_ids)
            .collect();
        let images = servers::images_dir();
        cx.background_spawn(crate::guard::task(async move {
            state::make_chat_files_private();
            state::sweep_orphans(listed);
            crate::cleanup::sweep_leftovers(&images);
        }))
        .detach();
        this.warm_droid_catalog(cx);
        this.listen_for_commands(window, cx);
        // The system close (the title bar's X, Alt+F4, Super+W) goes through the
        // same hide-or-quit as Arakun's own close button.
        window.on_window_should_close(cx, crate::background::should_close);
        this.watch_headless(window, cx);
        this.start_scheduler(window, cx);
        this.start_rule_ticker(window, cx);
        this.restore_phone(window, cx);
        this.restore_servers(window, cx);
        this.start_updates(window, cx);
        this.report_unreadable(cx);
        this.watch_secret_store(cx);
        this
    }

    fn has_projects(&self) -> bool {
        !self.projects.is_empty()
    }

    fn project(&self) -> &Project {
        &self.projects[self.active_project]
    }

    fn env(&self) -> &Environment {
        let p = self.project();
        &p.environments[p.active_env]
    }

    /// `env()` for code that can run after the last project is removed, such
    /// as work queued for the next frame or a focus change.
    fn try_env(&self) -> Option<&Environment> {
        self.projects
            .get(self.active_project)
            .and_then(|p| p.environments.get(p.active_env))
    }

    fn env_mut(&mut self) -> &mut Environment {
        let p = &mut self.projects[self.active_project];
        &mut p.environments[p.active_env]
    }

    /// The environment a tab works in; a view's is the one on screen.
    fn tab_env(&self, tab: &Tab) -> &Environment {
        tab.env
            .as_deref()
            .and_then(|p| self.find_env(p))
            .unwrap_or_else(|| self.env())
    }

    /// The other pane's tab, while the split shows.
    fn split_other(&self) -> Option<usize> {
        self.strip.split_other()
    }

    /// Makes the active tab one of the environment on screen: the tab it showed
    /// last, or a New session tab when it has none. `keep_view` leaves a view
    /// (GitHub, Usage…) in front, since it belongs to no environment.
    fn show_env_tab(&mut self, keep_view: bool) {
        let path = self.env().path.clone();
        let fits = self.strip.active().is_some_and(|t| match &t.env {
            Some(env) => *env == path,
            None => keep_view,
        });
        if fits {
            return;
        }
        // A blank tab that is all the environment left behind has no use:
        // going through environments must not leave one for each.
        let strip = &mut self.strip;
        // Unless it is half of that environment's split, which waits for it.
        if let Some(left) = strip
            .active()
            .filter(|t| matches!(t.kind, TabKind::NewSession))
            && strip.tabs.iter().filter(|t| t.env == left.env).count() == 1
            && !(strip.split.is_some() && strip.split_env == left.env)
        {
            strip.remove(strip.active_tab);
        }
        let index = match strip.front_of(&path) {
            Some(i) => i,
            None => strip.add(Tab::new_session(&path)),
        };
        strip.active_tab = index;
    }

    /// Brings the strip in line with the projects: the tabs of environments that
    /// are gone close, and the active tab is one of the environment on screen.
    fn settle_strip(&mut self, cx: &mut Context<Self>) {
        let projects = &self.projects;
        let lost = |env: &Option<std::path::PathBuf>| {
            env.as_deref().is_some_and(|path| {
                !projects
                    .iter()
                    .flat_map(|p| &p.environments)
                    .any(|e| e.path == path)
            })
        };
        let active = self.strip.active().map(|t| t.id);
        let gone = self.strip.remove_where(|t| lost(&t.env));
        self.strip.closed_tabs.retain(|c| !lost(&c.env));
        self.strip.parked.retain(|env, _| !lost(&Some(env.clone())));
        if lost(&self.strip.split_env) {
            self.strip.split = None;
            self.strip.split_env = None;
        }
        for tab in &gone {
            if let Some(b) = &tab.browser {
                b.view.update(cx, |v, cx| v.close(cx));
            }
        }
        if !self.has_projects() {
            // The views go with the last project, as the welcome takes the window.
            self.strip = Default::default();
            return;
        }
        let before = self.strip.active().map(|t| t.id);
        if active.is_some_and(|id| self.strip.position(id).is_some()) {
            self.show_env_tab(true);
        } else {
            // The active tab went: the environment now on screen shows the tab
            // it showed last, not whichever tab took the old place in the row.
            let path = self.env().path.clone();
            self.strip.active_tab = match self.strip.front_of(&path) {
                Some(i) => i,
                None => self.strip.add(Tab::new_session(&path)),
            };
        }
        if !gone.is_empty() || self.strip.active().map(|t| t.id) != before {
            self.note_active_tab(cx);
            self.env_followed = true;
            cx.notify();
        }
    }

    /// The split of the environment on screen comes with it; `prev` is the tab
    /// that was active before.
    fn bring_split(&mut self, prev: Option<u64>) {
        if self.has_projects() {
            let here = self.env().path.clone();
            self.strip.bring_split(&here, prev);
        }
    }

    /// The environment of the active tab comes on screen with it.
    fn follow_tab(&mut self, cx: &mut Context<Self>) {
        if !self.has_projects() {
            return;
        }
        let Some(tab) = self.strip.active() else {
            return;
        };
        let Some(path) = tab.env.clone() else {
            return;
        };
        self.strip.front.insert(path.clone(), tab.id);
        if self.env().path == path {
            return;
        }
        let Some((pi, ei)) = self
            .projects
            .iter()
            .enumerate()
            .find_map(|(pi, p)| Some((pi, p.env_index(&path)?)))
        else {
            return;
        };
        self.active_project = pi;
        self.projects[pi].active_env = ei;
        self.env_menu_open = false;
        let env = self.env().path.clone();
        self.composer.update(cx, |c, _| c.show_env(&env));
        self.sync_teams_project(cx);
        self.refresh_visible(cx);
        if self.panel.tab == PanelTab::Explorer {
            self.ensure_explorer(cx);
        }
        self.env_followed = true;
    }

    // -----------------------------------------------------------------------
    // Persistence and git refresh

    fn save(&self, cx: &mut Context<Self>) {
        // Its environments too: an environment elsewhere stays the project's
        // even if its Git metadata says otherwise.
        crate::agent::sandbox::set_projects(self.projects.iter().flat_map(|p| {
            std::iter::once(&p.path)
                .chain(p.environments.iter().map(|e| &e.path))
                .map(|path| crate::agent::sandbox::ProjectFolder {
                    path: path.clone(),
                    sandbox: p.sandbox,
                    pass: p.pass_env.clone(),
                    secrets: p.secrets.clone(),
                })
        }));
        let envs: std::collections::HashSet<&std::path::Path> = self
            .projects
            .iter()
            .flat_map(|p| &p.environments)
            .map(|e| e.path.as_path())
            .collect();
        self.composer
            .update(cx, |c, _| c.retain_drafts(|env| envs.contains(env)));
        state::queue(self.state_bytes(cx));
        cx.background_spawn(crate::guard::task(async move {
            if let Err(e) = state::write_queued() {
                eprintln!("arakun: could not save state: {e}");
            }
        }))
        .detach();
        // Whatever changed the saved state may show in the phones' inbox.
        if self.phone.is_some() {
            cx.spawn(async move |this, cx| {
                let _ = this.update(cx, |this, cx| this.phone_inbox_changed(cx));
            })
            .detach();
        }
    }

    /// Says which saved files could not be read since the last call. Without
    /// it the user only sees their projects, prompts or memories gone.
    fn report_unreadable(&self, cx: &mut Context<Self>) {
        for r in crate::stored::take_reports() {
            let name = r
                .path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| r.path.display().to_string());
            let title = format!("Could not read {name}");
            let body = match &r.kept {
                // The copy's name first: a long body is cut short on the island.
                Some(kept) => format!(
                    "Kept as {}, in {}. Arakun went on without it.",
                    kept.file_name()
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or_default(),
                    kept.parent().unwrap_or(std::path::Path::new("")).display()
                ),
                None => format!(
                    "It could not be moved aside either, so Arakun won't save over {}.",
                    r.path.display()
                ),
            };
            self.notify(crate::island::AlertKind::Warning, &title, &body, None, cx);
        }
    }

    /// Says once, ever, that secrets went to a private file because there is
    /// no keyring. They are kept on background threads, so this looks every
    /// few seconds, and stops once it has said it.
    fn watch_secret_store(&self, cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(Duration::from_secs(5)).await;
                if this.upgrade().is_none() {
                    break;
                }
                let Some(body) = crate::secret_store::take_notice() else {
                    continue;
                };
                let _ = this.update(cx, |this, cx| {
                    let title = "Keys are kept in a private file";
                    this.notify(crate::island::AlertKind::Warning, title, body, None, cx);
                });
                break;
            }
        })
        .detach();
    }

    /// Notes where the window is, for the next save.
    fn track_window(&mut self, window: &Window, cx: &App) {
        let display = window
            .display(cx)
            .and_then(|d| d.uuid().ok())
            .map(|u| u.to_string());
        self.window_state = Some(state::WindowState::next(
            self.window_state.as_ref(),
            window.window_bounds(),
            window.is_maximized(),
            window.is_fullscreen(),
            display,
        ));
    }

    fn state_bytes(&self, cx: &App) -> Vec<u8> {
        let layout = state::Layout {
            dock_open: self.dock_open,
            dock_height: self.dock_height,
            rail_open: self.rail.open,
            panel_open: self.panel.open,
            rail_w: self.rail.width,
            panel_w: self.panel.width,
            arcade: self.arcade,
            diff_palette: theme::diff_palette(),
            glass: theme::glass_chosen(),
            contrast: theme::contrast_setting(),
            grays: theme::grays(),
            type_scale: crate::ui::type_scale(),
            rail_usage: [
                account_view::rail_shows(AgentKind::Claude),
                account_view::rail_shows(AgentKind::Droid),
                account_view::rail_shows(AgentKind::OpenCode),
                account_view::rail_shows(AgentKind::Devin),
            ],
            ui_zoom: crate::ui_zoom::pct(),
            window: self.window_state.clone(),
        };
        let composer = self.composer.read(cx);
        state::snapshot_with(
            &self.projects,
            &self.strip,
            self.active_project,
            layout,
            &|env| composer.saved(env),
        )
    }

    /// Re-reads a project's worktrees, then the status of every environment.
    fn refresh_project(&mut self, pi: usize, cx: &mut Context<Self>) {
        let Some(root) = self.projects.get(pi).map(|p| p.path.clone()) else {
            return;
        };
        // Only servers have it: there is no folder here to ask git about.
        if crate::model::Remote::is_path(&root) {
            return;
        }
        let gen_ = self.refresh_gen.entry(root.clone()).or_default();
        *gen_ += 1;
        let gen_ = *gen_;
        cx.spawn(async move |this, cx| {
            let r = root.clone();
            let trees = cx
                .background_spawn(crate::guard::task(async move { git::worktrees(&r) }))
                .await;
            let paths = this
                .update(cx, |this, cx| {
                    if this.refresh_gen.get(&root) != Some(&gen_) {
                        return None;
                    }
                    let pi = this.projects.iter().position(|p| p.path == root)?;
                    if let Ok(trees) = trees {
                        this.projects[pi].sync_worktrees(trees);
                        this.settle_strip(cx);
                        this.save(cx);
                        cx.notify();
                    }
                    Some(
                        this.projects[pi]
                            .environments
                            .iter()
                            .map(|e| e.path.clone())
                            .collect::<Vec<_>>(),
                    )
                })
                .ok()
                .flatten()
                .unwrap_or_default();
            for path in paths {
                let _ = this.update(cx, |this, cx| this.refresh_status(&path, cx));
            }
        })
        .detach();
    }

    fn refresh_status(&mut self, env_path: &std::path::Path, cx: &mut Context<Self>) {
        if !self.git_reads.start(env_path, git_actions::GitRead::Status) {
            return;
        }
        let path = env_path.to_path_buf();
        // A server's folder: the server reads its changes there.
        let at = self.git_at(env_path);
        cx.spawn(async move |this, cx| {
            let (changes, sync) = cx
                .background_spawn(crate::guard::task(async move { at.status() }))
                .await;
            let _ = this.update(cx, |this, cx| {
                let timed_out = changes.as_ref().err().or(sync.as_ref().err());
                let timed_out = timed_out.filter(|e| git::timed_out(e));
                let recovered = this.git_reads.status_ended(&path, timed_out.is_some());
                if let Some(env) = this.find_env_mut(&path) {
                    // A server that could not be asked says so in the panel,
                    // which would otherwise show no changes, or old ones.
                    if env.remote.is_some() {
                        let error = changes.as_ref().err().map(|e| e.to_string().into());
                        if env.status_error != error {
                            env.status_error = error;
                            cx.notify();
                        }
                    }
                    if let Some(e) = timed_out {
                        env.git_error = Some(
                            format!(
                                "{e}. Arakun looks again in {} minutes.",
                                git_actions::STATUS_PAUSE.as_secs() / 60
                            )
                            .into(),
                        );
                        cx.notify();
                    } else if recovered && env.git_error.is_some() {
                        env.git_error = None;
                        cx.notify();
                    }
                }
                if this.git_reads.end(&path, git_actions::GitRead::Status) {
                    this.refresh_status(&path, cx);
                }
                if let Ok(sync) = sync
                    && let Some(env) = this.find_env_mut(&path)
                    && env.sync != sync
                {
                    env.sync = sync;
                    cx.notify();
                }
                this.maybe_lookup_pr(&path, cx);
                let Ok(changes) = changes else { return };
                let mut reloads: Vec<TabKind> = Vec::new();
                let mut stale: Vec<String> = Vec::new();
                let mut dirs: Option<Vec<std::path::PathBuf>> = None;
                let mut found = false;
                for project in &mut this.projects {
                    if let Some(env) = project.environments.iter_mut().find(|e| e.path == path) {
                        found = true;
                        if env.changes != changes {
                            // Open diff tabs track their file: reload when its entry
                            // changed, close when the file has no changes left.
                            let old = std::mem::replace(&mut env.changes, changes);
                            for tab in this.strip.tabs.iter().filter(|t| t.is_in(&env.path)) {
                                let TabKind::Diff { path: file, .. } = &tab.kind else {
                                    continue;
                                };
                                let was = old.iter().find(|c| &c.path == file);
                                let now = env.changes.iter().find(|c| &c.path == file);
                                if was == now {
                                    continue;
                                }
                                match now {
                                    Some(c) => reloads.push(TabKind::Diff {
                                        path: file.clone(),
                                        untracked: c.kind == git::ChangeKind::Untracked,
                                    }),
                                    None => stale.push(file.clone()),
                                }
                            }
                            // New or removed files show up in the tree too.
                            if env.explorer.children.contains_key(&env.explorer.root) {
                                dirs = Some(env.explorer.loaded_dirs());
                            }
                            cx.notify();
                        }
                        break;
                    }
                }
                if !found {
                    return;
                }
                for kind in reloads {
                    this.reload_doc(path.clone(), kind, cx);
                }
                this.stale_diffs
                    .extend(stale.into_iter().map(|f| (path.clone(), f)));
                if let Some(dirs) = dirs {
                    this.reload_dirs(path.clone(), dirs, cx);
                }
            });
        })
        .detach();
    }

    /// Status (and open folders) of what is on screen right now.
    fn refresh_visible(&mut self, cx: &mut Context<Self>) {
        if !self.has_projects() {
            return;
        }
        let path = self.env().path.clone();
        self.refresh_status(&path, cx);
        if self.panel.tab == PanelTab::Explorer {
            let dirs = self.env().explorer.loaded_dirs();
            self.reload_dirs(path, dirs, cx);
        }
    }

    fn sync_poll(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !window.is_window_active() || !self.has_projects() {
            self.poll = None;
            return;
        }
        if self.poll.is_some() {
            return;
        }
        self.maybe_fetch(cx);
        self.refresh_running(false, cx);
        self.poll = Some(cx.spawn(async move |this, cx| {
            loop {
                let wait = this
                    .update(cx, |this, _| {
                        let busy =
                            this.has_projects()
                                && this.env().sessions.iter().any(|s| {
                                    matches!(s.status, Status::Working | Status::NeedsInput)
                                });
                        if busy { POLL } else { POLL_IDLE }
                    })
                    .unwrap_or(POLL_IDLE);
                cx.background_executor().timer(wait).await;
                let Some(path) = this
                    .update(cx, |this, _| {
                        this.has_projects().then(|| this.env().path.clone())
                    })
                    .ok()
                else {
                    break;
                };
                if let Some(path) = path {
                    let _ = this.update(cx, |this, cx| {
                        this.refresh_status(&path, cx);
                        this.maybe_fetch(cx);
                        this.refresh_home(cx);
                        this.refresh_running(false, cx);
                    });
                }
            }
        }));
    }

    /// Fetches the visible environment's remotes in the background when the last
    /// fetch is old, then refreshes the sync counts. Errors are ignored: a laptop
    /// offline is normal, and the next tick tries again.
    fn maybe_fetch(&mut self, cx: &mut Context<Self>) {
        if !self.has_projects() || self.env().sync.remote.is_none() {
            return;
        }
        if self.fetching || self.last_fetch.is_some_and(|t| t.elapsed() < FETCH_EVERY) {
            return;
        }
        self.fetching = true;
        let path = self.env().path.clone();
        let at = self.git_at(&path);
        cx.spawn(async move |this, cx| {
            let fetched = cx
                .background_spawn(crate::guard::task(async move { at.fetch() }))
                .await;
            let _ = this.update(cx, |this, cx| {
                this.fetching = false;
                // Counted from the end, so a slow fetch is not followed by another at once.
                this.last_fetch = Some(Instant::now());
                if fetched.is_ok() {
                    this.refresh_status(&path, cx);
                }
            });
        })
        .detach();
    }

    fn find_env_mut(&mut self, path: &std::path::Path) -> Option<&mut Environment> {
        self.projects
            .iter_mut()
            .flat_map(|p| p.environments.iter_mut())
            .find(|e| e.path == path)
    }

    // -----------------------------------------------------------------------
    // Projects, environments and dialogs

    fn open_picker(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.open_picker_then(None, window, cx);
    }

    /// The Add-project picker, opened because `what` (a view's name) needs a
    /// project: it says so, and `then` opens `what` once one is added.
    pub(super) fn pick_project_for(
        &mut self,
        what: &str,
        then: OpenView,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let why = format!("{what} works in a project. Pick a git repository to open it in.");
        self.open_picker_then(Some((why, then)), window, cx);
    }

    fn open_picker_then(
        &mut self,
        why: Option<(String, OpenView)>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // Ctrl+O reaches here over an open dialog; replacing one that is still
        // saving would lose its result.
        if self.modal_busy(cx) {
            return;
        }
        self.menu = None;
        self.env_menu_open = false;
        let existing = self.projects.iter().map(|p| p.path.clone()).collect();
        let (why, then) = why.unzip();
        let servers = self.clone_servers();
        let picker = cx.new(|cx| {
            let picker = FolderPicker::new(existing, window, cx).servers(servers, cx);
            match why {
                Some(why) => picker.because(why),
                None => picker,
            }
        });
        let sub = cx.subscribe_in(
            &picker,
            window,
            move |this, _, ev: &PickerEvent, window, cx| match ev {
                PickerEvent::Picked(path) => {
                    let path = path.clone();
                    this.close_modal(window, cx);
                    this.add_project(path, window, cx);
                    if let Some(then) = then
                        && this.has_projects()
                    {
                        then(this, window, cx);
                    }
                }
                PickerEvent::CloneOnServer { server, url } => {
                    let (server, url) = (server.clone(), url.clone());
                    this.close_modal(window, cx);
                    this.servers_clone_from_picker(&server, &url, cx);
                }
                PickerEvent::Dismissed => this.close_modal(window, cx),
            },
        );
        self.modal = Some((Modal::Picker(picker), sub));
        cx.notify();
    }

    fn add_project(
        &mut self,
        path: std::path::PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let known = self.projects.iter().position(|p| p.path == path);
        let index = match known {
            Some(i) => i,
            None => {
                let mut project = state::new_project(&path);
                project.icon = model::unique_icon(project.icon, |icon| {
                    self.projects.iter().any(|p| p.icon == icon)
                });
                self.projects.push(project);
                crate::telemetry::capture(crate::telemetry::events::PROJECT_ADDED, &[]);
                self.projects.len() - 1
            }
        };
        // The one on screen until now was in use until now. Without this a
        // switch right after a long sleep leaves it with its old date, and
        // the cleanup could take what was just open.
        self.mark_visible_used();
        self.avoid_env_being_removed(index);
        self.active_project = index;
        self.sync_teams_project(cx);
        self.show_env_tab(false);
        self.note_active_tab(cx);
        self.refresh_project(index, cx);
        self.sync_ci_roots(cx);
        self.push_linear_host(cx);
        // A mapping broken when the project went can be sent again.
        if known.is_none() {
            self.sync_linear(window, cx);
        }
        self.save(cx);
        self.focus_composer(window, cx);
        self.ensure_terminal(window, cx);
        self.sync_poll(window, cx);
        // The setup goes on to its next step, or ends when the project
        // came some other way.
        self.onboarding_project_added(window, cx);
        cx.notify();
    }

    fn remove_project(&mut self, pi: usize, window: &mut Window, cx: &mut Context<Self>) {
        self.menu = None;
        if pi >= self.projects.len() {
            return;
        }
        let project = self.projects.remove(pi);
        let pids: Vec<String> = project
            .environments
            .iter()
            .flat_map(Self::phone_ids_of)
            .collect();
        self.forget_phone_sessions(&pids);
        let sessions = project
            .environments
            .into_iter()
            .flat_map(|e| e.sessions)
            .collect();
        self.forget_sessions(sessions, window, cx);
        if self.active_project >= pi && self.active_project > 0 {
            self.active_project -= 1;
        }
        self.active_project = self
            .active_project
            .min(self.projects.len().saturating_sub(1));
        self.sync_teams_project(cx);
        self.settle_strip(cx);
        self.unfloat_github(cx);
        // Its Computers tab went with it; a no-op while one is left elsewhere.
        self.servers_tab_closed(cx);
        self.sync_ci_roots(cx);
        self.push_linear_host(cx);
        // Its Linear targets must stop coming here.
        self.sync_linear(window, cx);
        self.save(cx);
        self.sync_poll(window, cx);
        cx.notify();
    }

    fn open_create_env(&mut self, pi: usize, window: &mut Window, cx: &mut Context<Self>) {
        self.menu = None;
        self.env_menu_open = false;
        let Some(project) = self.projects.get(pi) else {
            return;
        };
        // This computer, unless only servers have the project, and each
        // connected server with it; the open environment's machine first picked.
        let mut machines = Vec::new();
        if !project.is_remote() {
            machines.push(crate::dialogs::Machine {
                server: None,
                label: "This computer".into(),
                folder: None,
            });
        }
        // The open environment speaks for its server: the new one starts with
        // its agent. Any of a server's folders for the others.
        let open_env = project.environments.get(project.active_env);
        let envs = open_env.into_iter().chain(project.environments.iter());
        for e in envs {
            let Some(r) = &e.remote else { continue };
            if self.server_env_retired(&e.path) {
                continue;
            }
            let known = machines
                .iter()
                .any(|m| m.server.as_ref().is_some_and(|(s, _)| *s == r.server));
            if !known && self.server_reach(&r.server) == Some(crate::servers_view::Reach::Connected)
            {
                machines.push(crate::dialogs::Machine {
                    server: Some((r.server.clone(), r.place.clone())),
                    label: r.machine.clone(),
                    folder: self.server_repo_folder(&r.server, &r.place),
                });
            }
        }
        if machines.is_empty() {
            self.notify(
                crate::island::AlertKind::Warning,
                "No server is connected",
                "This project is only on servers, and none of them is connected right now.",
                None,
                cx,
            );
            return;
        }
        let open = project
            .environments
            .get(project.active_env)
            .and_then(|e| e.remote.as_ref())
            .map(|r| r.server.clone());
        let open_pick = machines
            .iter()
            .position(|m| m.server.as_ref().map(|(s, _)| s) == open.as_ref())
            .unwrap_or(0);
        // Spread: the machine it would go to without asking, the open one
        // first on a tie.
        // The others keep their order, as in `spread_target`.
        let mut order: Vec<usize> = (0..machines.len()).collect();
        order.remove(open_pick);
        order.insert(0, open_pick);
        let ids: Vec<Option<&str>> = order
            .iter()
            .map(|&i| machines[i].server.as_ref().map(|(s, _)| s.as_str()))
            .collect();
        let pick = self
            .servers_spread_pick(&ids)
            .map_or(open_pick, |i| order[i]);
        let (repo, name) = (project.path.clone(), project.name.clone());
        let (setup, start) = (project.setup.clone(), project.base.clone());
        let dialog = cx.new(|cx| {
            CreateEnvDialog::new(repo, name, setup, start, window, cx).with_machines(machines, pick)
        });
        let sub = cx.subscribe_in(
            &dialog,
            window,
            move |this, dialog, ev: &DialogEvent, window, cx| match ev {
                DialogEvent::Created {
                    path,
                    branch,
                    auto_name,
                } => {
                    let (path, branch, auto_name) = (path.clone(), branch.clone(), *auto_name);
                    this.close_modal(window, cx);
                    this.environment_created(pi, path, branch, auto_name, window, cx);
                }
                DialogEvent::CreateOnServer {
                    server,
                    from,
                    branch,
                    auto_name,
                } => {
                    let (server, from) = (server.clone(), from.clone());
                    let (branch, auto_name) = (branch.clone(), *auto_name);
                    // The dialog stays until the server answers.
                    let asked = Some(dialog.entity_id());
                    this.servers_new_env_on(&server, &from, branch, auto_name, asked, None, cx);
                }
                _ => this.close_modal(window, cx),
            },
        );
        self.modal = Some((Modal::CreateEnv(dialog), sub));
        cx.notify();
    }

    /// The machine a new environment of project `pi` goes to while they are
    /// spread: `Some(None)` for this computer, `Some(Some(env))` for the
    /// server of that environment. `None` when they are not spread or no
    /// machine can take it.
    fn spread_target(&self, pi: usize) -> Option<Option<usize>> {
        let project = self.projects.get(pi)?;
        let open = project
            .environments
            .get(project.active_env)
            .and_then(|e| e.remote.as_ref())
            .map(|r| r.server.as_str());
        let mut machines: Vec<(Option<&str>, Option<usize>)> = Vec::new();
        if !project.is_remote() {
            machines.push((None, None));
        }
        // The open environment first, so its server makes the new one from it.
        let envs = std::iter::once(project.active_env).chain(0..project.environments.len());
        for i in envs {
            let Some(e) = project.environments.get(i) else {
                continue;
            };
            let Some(r) = &e.remote else { continue };
            let known = machines.iter().any(|(s, _)| *s == Some(r.server.as_str()));
            if known
                || self.server_env_retired(&e.path)
                || self.server_reach(&r.server) != Some(crate::servers_view::Reach::Connected)
            {
                continue;
            }
            machines.push((Some(r.server.as_str()), Some(i)));
        }
        // The open environment's machine wins a tie.
        if let Some(at) = machines.iter().position(|(s, _)| *s == open) {
            let first = machines.remove(at);
            machines.insert(0, first);
        }
        let ids: Vec<Option<&str>> = machines.iter().map(|(s, _)| *s).collect();
        let i = self.servers_spread_pick(&ids)?;
        Some(machines[i].1)
    }

    /// A new environment without asking: a random branch from the current
    /// commit, as the dialog would fill in. Its row shows at once and the
    /// worktree follows; the agent names the branch after the first prompt.
    fn quick_create_env(&mut self, pi: usize, window: &mut Window, cx: &mut Context<Self>) {
        self.quick_create_env_then(pi, None, window, cx)
    }

    /// [`Self::quick_create_env`], then `prompt` starts the worktree's session:
    /// a first prompt sent where a session already is.
    fn quick_create_env_then(
        &mut self,
        pi: usize,
        prompt: Option<sessions::QueuedPrompt>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.menu = None;
        self.env_menu_open = false;
        let Some(project) = self.projects.get(pi) else {
            return;
        };
        // Spread across machines, one made without a prompt goes where
        // `spread_target` says. A prompt stays on the machine it was written
        // on: what it mentions and the settings picked with it are there.
        let spread = prompt
            .as_ref()
            .map_or_else(|| self.spread_target(pi), |_| None);
        if let Some(Some(from)) = spread {
            self.servers_new_env(pi, from, None, cx);
            return;
        }
        // In a server's environment, or a project only servers have: the
        // new one is made on that server.
        let on_server = spread.is_none()
            && (project
                .environments
                .get(project.active_env)
                .is_some_and(|e| e.remote.is_some())
                || project.is_remote());
        if on_server {
            // A queued prompt goes only to the server it was written on,
            // never to another that has this project too.
            let written_on = prompt.as_ref().map(|p| {
                self.find_env(p.from())
                    .and_then(|e| e.remote.as_ref())
                    .map(|r| r.server.clone())
            });
            let from = server_source(project, |p| self.server_env_retired(p)).filter(|&i| {
                written_on.as_ref().is_none_or(|server| {
                    server.is_some()
                        && project.environments[i].remote.as_ref().map(|r| &r.server)
                            == server.as_ref()
                })
            });
            let Some(from) = from else {
                if let Some(prompt) = prompt {
                    self.unqueue(prompt, cx);
                }
                self.notify(
                    crate::island::AlertKind::Warning,
                    "Could not make the environment",
                    "The server no longer offers this folder, nor another of this project.",
                    None,
                    cx,
                );
                return;
            };
            self.servers_new_env(pi, from, prompt, cx);
            return;
        }
        let (repo, project_name) = (project.path.clone(), project.name.clone());
        let start = project.base.clone();
        let pending: Vec<String> = self
            .pending_envs
            .iter()
            .filter(|p| p.repo == repo)
            .map(|p| p.name.clone())
            .collect();
        let name = crate::env_name::random(|n| {
            pending.iter().any(|p| p == n) || crate::dialogs::folder_taken(&project_name, n)
        });
        self.pending_envs.push(PendingEnv {
            repo: repo.clone(),
            name: name.clone(),
        });
        cx.notify();
        let (key, shown) = (repo.clone(), name.clone());
        cx.spawn_in(window, async move |this, cx| {
            let made = cx
                .background_spawn(crate::guard::task(async move {
                    // A branch can already have the random name; git would
                    // check it out instead. Pick another then, and once more
                    // if git refuses the first.
                    let local = git::branches(&repo).map(|b| b.local).unwrap_or_default();
                    let taken = |n: &str, also: &str| {
                        n == also
                            || local.iter().any(|b| b == n)
                            || crate::dialogs::folder_taken(&project_name, n)
                    };
                    let mut name = name;
                    if local.contains(&name) {
                        name = crate::env_name::random(|n| taken(n, ""));
                    }
                    // Before the first commit there is nothing to branch from:
                    // None, and the session starts in the main checkout.
                    if !git::has_commits(&repo)? {
                        return Ok(None);
                    }
                    let base = git::new_env_base(&repo, start.as_deref());
                    let mut result = Err(anyhow::anyhow!("not tried"));
                    for _ in 0..2 {
                        let path = crate::model::worktree_path(&project_name, &name);
                        result = git::add_worktree(&repo, &path, &name, base.as_deref(), true)
                            .map(|()| Some((path, name.clone())));
                        if result.is_ok() {
                            break;
                        }
                        name = crate::env_name::random(|n| taken(n, &name));
                    }
                    result
                }))
                .await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.pending_envs
                    .retain(|p| !(p.repo == key && p.name == shown));
                match made {
                    // The project can have moved in the list since.
                    Ok(Some((path, branch))) => {
                        if let Some(pi) = this.projects.iter().position(|p| p.path == key) {
                            this.environment_created(pi, path.clone(), branch, true, window, cx);
                            if let Some(prompt) = prompt {
                                this.start_queued(&path, prompt, window, cx);
                            }
                        }
                    }
                    Ok(None) => this.use_checkout(&key, prompt, window, cx),
                    Err(e) => {
                        if let Some(prompt) = prompt {
                            this.unqueue(prompt, cx);
                        }
                        this.notify(
                            crate::island::AlertKind::Failure,
                            "Could not create the environment",
                            &e.to_string(),
                            None,
                            cx,
                        )
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// A repository without commits has no worktrees: a new session goes to
    /// the main checkout of the project at `repo`, with `prompt` when one waits.
    fn use_checkout(
        &mut self,
        repo: &std::path::Path,
        prompt: Option<sessions::QueuedPrompt>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some((pi, ei)) = self.projects.iter().enumerate().find_map(|(pi, p)| {
            (p.path == repo)
                .then(|| p.environments.iter().position(|e| e.is_checkout))
                .flatten()
                .map(|ei| (pi, ei))
        }) else {
            return;
        };
        let checkout = self.projects[pi].environments[ei].path.clone();
        match prompt {
            Some(prompt) => self.start_queued(&checkout, prompt, window, cx),
            None => {
                self.select_env(pi, ei, window, cx);
                self.show_new_session_tab(window, cx);
            }
        }
    }

    /// Shows the new worktree right away and lets `git worktree list` fill in details.
    fn environment_created(
        &mut self,
        pi: usize,
        path: std::path::PathBuf,
        branch: String,
        auto_name: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(project) = self.projects.get_mut(pi) else {
            return;
        };
        crate::telemetry::capture(
            crate::telemetry::events::ENVIRONMENT_CREATED,
            &[("auto_name", if auto_name { "true" } else { "false" })],
        );
        if project.env_index(&path).is_none() {
            project
                .environments
                .push(Environment::new(path.clone(), Some(branch.clone()), false));
        }
        let ei = project.env_index(&path).unwrap_or(0);
        if auto_name {
            // The folder keeps the random name; the label follows the branch once renamed.
            let env = &mut project.environments[ei];
            env.placeholder = Some(branch);
            env.follow_branch();
        }
        self.select_env(pi, ei, window, cx);
        // It opens on its New session tab, ready for the first prompt.
        self.focus_composer(window, cx);
        self.refresh_project(pi, cx);
        // The project's setup script runs in a terminal of the new environment.
        let setup = self.projects.get(pi).and_then(|p| p.setup.clone());
        if let Some(script) = setup {
            let env_path = self.env().path.clone();
            self.run_in_terminal(&env_path, &script, true, window, cx);
        }
    }

    /// The dialog for the project's setup script (`check` false) or check, from its context menu.
    /// `suggested` fills the field instead of the current command.
    fn open_project_command(
        &mut self,
        pi: usize,
        check: bool,
        suggested: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.menu = None;
        let Some(project) = self.projects.get(pi) else {
            return;
        };
        let current = suggested.or_else(|| {
            if check {
                project.check.clone()
            } else {
                project.setup.clone()
            }
        });
        let dialog = cx.new(|cx| {
            if check {
                crate::dialogs::CommandDialog::check(current.as_deref(), window, cx)
            } else {
                crate::dialogs::CommandDialog::setup(current.as_deref(), window, cx)
            }
        });
        let sub = cx.subscribe_in(
            &dialog,
            window,
            move |this, _, ev: &DialogEvent, window, cx| {
                if let DialogEvent::Command(command) = ev {
                    if let Some(project) = this.projects.get_mut(pi) {
                        if check {
                            project.check = command.clone();
                        } else {
                            project.setup = command.clone();
                        }
                    }
                    this.save(cx);
                }
                this.close_modal(window, cx);
            },
        );
        self.modal = Some((Modal::Command(dialog), sub));
        cx.notify();
    }

    /// Shows the visible environment's `kind` tab, opening it when missing. A view
    /// opened from the rail is a preview: the next view takes its place unless
    /// this one was used (a click inside it, or a double click on its title).
    pub(super) fn show_view_tab(
        &mut self,
        title: &'static str,
        kind: TabKind,
        is: impl Fn(&TabKind) -> bool,
    ) {
        let strip = &mut self.strip;
        let index = match strip.tabs.iter().position(|t| is(&t.kind)) {
            Some(i) => i,
            None => {
                crate::telemetry::capture(
                    crate::telemetry::events::VIEW_OPENED,
                    &[("view", title)],
                );
                let mut tab = Tab::new(title, kind);
                tab.preview = true;
                let active = strip.active_tab;
                if strip.tabs.get(active).is_some_and(|t| t.preview) {
                    strip.tabs[active] = tab;
                    active
                } else {
                    strip.add(tab)
                }
            }
        };
        strip.active_tab = index;
    }

    /// Keeps the visible preview tab: it was used.
    pub(super) fn pin_preview(&mut self, cx: &mut Context<Self>) {
        if !self.has_projects() {
            return;
        }
        let active = self.strip.active_tab;
        if let Some(tab) = self.strip.tabs.get_mut(active).filter(|t| t.preview) {
            tab.preview = false;
            cx.notify();
        }
    }

    /// Opens (or focuses) the Notes tab of the visible environment.
    fn open_notes_tab(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.has_projects() {
            self.pick_project_for("The Notes tab", Self::open_notes_tab, window, cx);
            return;
        }
        self.show_view_tab("Notes", TabKind::Notes, |k| matches!(k, TabKind::Notes));
        self.note_active_tab(cx);
        let focus = self.notes_input.focus_handle(cx);
        window.focus(&focus, cx);
        cx.notify();
    }

    /// Opens (or focuses) the Memory tab of the visible environment.
    fn open_memory_tab(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.has_projects() {
            self.pick_project_for("The Memory tab", Self::open_memory_tab, window, cx);
            return;
        }
        self.show_view_tab("Memory", TabKind::Memory, |k| matches!(k, TabKind::Memory));
        self.note_active_tab(cx);
        let dir = self.env().path.clone();
        self.views.memory.update(cx, |v, cx| {
            v.set_dir(&dir, cx);
            v.focus(window, cx);
        });
        cx.notify();
    }

    /// Opens (or focuses) the Usage tab of the visible environment. Plans
    /// belong to no project: with none, it shows in the welcome's place.
    fn open_usage_tab(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.has_projects() {
            self.show_view_tab("Usage", TabKind::Usage, |k| matches!(k, TabKind::Usage));
            self.note_active_tab(cx);
        } else {
            self.show_alone(Some(home::Alone::Usage), cx);
        }
        self.sync_usage_view(cx);
        self.views.usage.update(cx, |v, cx| v.focus(window, cx));
        cx.notify();
    }

    /// Whether the Teams tab is on screen, in either pane of a split, in a
    /// window in front.
    fn teams_shown(&self, window: &Window) -> bool {
        let is_teams = |i: usize| {
            matches!(
                self.strip.tabs.get(i).map(|t| &t.kind),
                Some(TabKind::Teams)
            )
        };
        // With no project the tab shows in the welcome's place.
        let on_screen = if self.has_projects() {
            is_teams(self.strip.active_tab) || self.split_other().is_some_and(is_teams)
        } else {
            self.alone == Some(home::Alone::Teams)
        };
        window.is_window_active() && on_screen
    }

    /// Whether a tab `is` picks is on screen, in either pane of a split, in a
    /// window in front.
    fn tab_shown(&self, window: &Window, is: impl Fn(&TabKind) -> bool) -> bool {
        let at = |i: usize| self.strip.tabs.get(i).is_some_and(|t| is(&t.kind));
        window.is_window_active()
            && self.has_projects()
            && (at(self.strip.active_tab) || self.split_other().is_some_and(at))
    }

    fn cloudflare_shown(&self, window: &Window) -> bool {
        self.tab_shown(window, |k| matches!(k, TabKind::Cloudflare))
    }

    fn memory_shown(&self, window: &Window) -> bool {
        self.tab_shown(window, |k| matches!(k, TabKind::Memory))
    }

    fn usage_shown(&self, window: &Window) -> bool {
        if self.has_projects() {
            self.tab_shown(window, |k| matches!(k, TabKind::Usage))
        } else {
            // With no project the tab shows in the welcome's place.
            window.is_window_active()
                && self.terminal_alone.is_none()
                && self.alone == Some(home::Alone::Usage)
        }
    }

    /// Tells the tabs that poll files whether they are on screen: they only
    /// poll then.
    fn sync_polled_shown(&mut self, window: &Window, cx: &mut Context<Self>) {
        let cloudflare = self.cloudflare_shown(window);
        let memory = self.memory_shown(window);
        let usage = self.usage_shown(window);
        self.views
            .cloudflare
            .update(cx, |v, _| v.set_shown(cloudflare));
        self.views
            .memory
            .update(cx, |v, cx| v.set_shown(memory, cx));
        self.views.usage.update(cx, |v, cx| v.set_shown(usage, cx));
    }

    /// Tells the Teams tab whether it is on screen: only then does a message
    /// that arrives in its open channel count as read.
    fn sync_teams_shown(&mut self, window: &Window, cx: &mut Context<Self>) {
        let shown = self.teams_shown(window);
        self.teams_view.update(cx, |v, cx| v.set_shown(shown, cx));
    }

    /// Opens (or focuses) the Teams tab. Teams belongs to the account, not to
    /// a project: with none, it shows in the welcome's place.
    pub(super) fn open_teams_tab(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.has_projects() {
            self.show_view_tab("Team", TabKind::Teams, |k| matches!(k, TabKind::Teams));
            self.note_active_tab(cx);
        } else {
            self.show_alone(Some(home::Alone::Teams), cx);
        }
        self.teams_view.update(cx, |v, cx| v.focus(window, cx));
        cx.notify();
    }

    /// The sign-in dialog: GitHub's code, then whether it worked. It starts
    /// the sign-in when none is under way.
    pub(super) fn open_teams_sign_in(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.open_sign_in(false, window, cx);
    }

    /// The sign-in dialog for what needs the account rather than Teams (the
    /// relay): it closes by itself once signed in, so it never offers Team.
    pub(super) fn open_account_sign_in(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.open_sign_in(true, window, cx);
    }

    fn open_sign_in(&mut self, close_when_in: bool, window: &mut Window, cx: &mut Context<Self>) {
        use crate::arakun_teams::sign_in::{SignInDialog, SignInEvent};
        self.menu = None;
        self.env_menu_open = false;
        let teams = self.teams.clone();
        let dialog = cx.new(|cx| SignInDialog::new(teams, close_when_in, window, cx));
        let sub = cx.subscribe_in(
            &dialog,
            window,
            |this, _, ev: &SignInEvent, window, cx| match ev {
                SignInEvent::Dismissed => this.close_modal(window, cx),
                SignInEvent::OpenTeam => {
                    this.close_modal(window, cx);
                    this.open_teams_tab(window, cx);
                }
            },
        );
        self.modal = Some((Modal::TeamsSignIn(dialog), sub));
        cx.notify();
    }

    /// The rail shows Team once the server lets the account in. A layout
    /// saved before Team existed gets it once, under Search.
    fn teams_changed(&mut self, cx: &mut Context<Self>) {
        use crate::sidebar::View;
        if self.teams.read(cx).enabled() && self.sidebar.is_new(View::Teams) {
            // Not through `change_sidebar`: the user did not make this change,
            // so Undo in Customize must not get a step for it.
            self.sidebar.place_after_search(View::Teams);
            self.sidebar.known.push(View::Teams);
            self.sidebar.save();
        }
        // The User row shows the account's GitHub picture.
        let login = self.teams.read(cx).me().map(|m| m.login.clone());
        if let Some(login) = login {
            self.teams.update(cx, |t, cx| t.want_pictures([login], cx));
        }
        cx.notify();
    }

    /// Opens (or focuses) the Team tab: what the team hub holds.
    #[cfg(feature = "team")]
    pub(super) fn open_team_tab(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.has_projects() {
            self.open_picker(window, cx);
            return;
        }
        self.show_view_tab("Equipo", TabKind::Team, |k| matches!(k, TabKind::Team));
        self.note_active_tab(cx);
        self.team_view.update(cx, |v, cx| v.focus(window, cx));
        cx.notify();
    }

    /// Opens (or focuses) the Design system tab of the visible environment.
    fn open_gallery_tab(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.has_projects() {
            self.pick_project_for("The Design system tab", Self::open_gallery_tab, window, cx);
            return;
        }
        self.show_view_tab("Design system", TabKind::Gallery, |k| {
            matches!(k, TabKind::Gallery)
        });
        self.note_active_tab(cx);
        self.views.gallery.update(cx, |v, cx| v.focus(window, cx));
        cx.notify();
    }

    /// Opens (or focuses) the Isometric tab of the visible environment.
    fn open_iso_tab(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.has_projects() {
            self.pick_project_for("The Isometric tab", Self::open_iso_tab, window, cx);
            return;
        }
        self.show_view_tab("Isometric", TabKind::Isometric, |k| {
            matches!(k, TabKind::Isometric)
        });
        self.note_active_tab(cx);
        let root = self.env().path.clone();
        self.views.iso.update(cx, |v, cx| {
            v.set_root(root, cx);
            v.focus(window, cx);
        });
        cx.notify();
    }

    /// Opens (or focuses) the Canvas tab of the visible environment, reading `source`
    /// (a canvas JSON, a folder or a Doop URL) when given.
    #[cfg(feature = "coderlabs")]
    pub(super) fn open_canvas_tab(
        &mut self,
        source: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.has_projects() {
            self.open_picker(window, cx);
            return;
        }
        self.show_view_tab("Canvas", TabKind::Canvas, |k| matches!(k, TabKind::Canvas));
        self.note_active_tab(cx);
        self.canvas_view.update(cx, |v, cx| {
            if let Some(source) = source {
                v.open(source, cx);
            }
            v.focus(window, cx);
        });
        cx.notify();
    }

    /// Gives the Usage tab the session titles and project names it shows.
    fn sync_usage_view(&self, cx: &mut Context<Self>) {
        let mut titles = std::collections::HashMap::new();
        let mut places = Vec::new();
        for p in &self.projects {
            let name = p.path.file_name().map_or_else(
                || p.path.display().to_string(),
                |n| n.to_string_lossy().into_owned(),
            );
            for e in &p.environments {
                places.push((e.path.clone(), name.clone()));
                for s in &e.sessions {
                    let id = s
                        .live
                        .as_ref()
                        .and_then(|l| l.thread.read(cx).session_id().map(str::to_string));
                    if let Some(id) =
                        id.or_else(|| s.meta.as_ref().and_then(|m| m.session_id.clone()))
                    {
                        titles.insert(id, s.title.to_string());
                    }
                }
            }
            places.push((p.path.clone(), name));
        }
        self.views.usage.update(cx, |v, cx| {
            v.set_account(self.account.clone(), cx);
            v.set_context(titles, places, cx);
        });
    }

    fn open_delete_env(
        &mut self,
        pi: usize,
        ei: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.menu = None;
        let Some(project) = self.projects.get(pi) else {
            return;
        };
        let Some(env) = project.environments.get(ei) else {
            return;
        };
        if env.is_checkout {
            return;
        }
        // Its folder is on a server, which deletes it.
        if env.remote.is_some() {
            return self.servers_delete_env(pi, ei, window, cx);
        }
        let repo = project.path.clone();
        let path = env.path.clone();
        let branch = env.checked_out().map(str::to_string);
        let dirty = env.changes.len();
        // Taken now: a refresh that sees the worktree gone can drop the
        // environment, sessions and all, before `Deleted` arrives.
        let own_ids: Vec<String> = env.sessions.iter().map(|s| s.id.to_string()).collect();
        let session_ids: Vec<String> = env
            .sessions
            .iter()
            .flat_map(sessions::session_file_ids)
            .collect();
        let dialog = cx.new(|cx| DeleteEnvDialog::new(repo, path, branch, dirty, window, cx));
        let sub = cx.subscribe_in(
            &dialog,
            window,
            move |this, _, ev: &DialogEvent, window, cx| match ev {
                DialogEvent::Deleted(path) => {
                    let path = path.clone();
                    this.close_modal(window, cx);
                    if this.find_env(&path).is_none() {
                        // A refresh dropped the environment first, and a
                        // headless scan may have put its sessions in another
                        // one meanwhile: they go too.
                        for id in &own_ids {
                            while let Some(i) = this.strip.session_tab(id) {
                                this.discard_tab(i, window, cx);
                            }
                        }
                        let mut moved = Vec::new();
                        for env in this.projects.iter_mut().flat_map(|p| &mut p.environments) {
                            let (gone, kept) = std::mem::take(&mut env.sessions)
                                .into_iter()
                                .partition(|s| own_ids.iter().any(|id| *id == *s.id));
                            env.sessions = kept;
                            moved.extend::<Vec<_>>(gone);
                        }
                        // A worktree is local: phones call its sessions by id.
                        this.forget_phone_sessions(&own_ids);
                        this.forget_sessions(moved, window, cx);
                        sessions::discard_session_files(own_ids.clone(), session_ids.clone(), cx);
                    }
                    this.forget_envs(&[path], window, cx);
                }
                _ => this.close_modal(window, cx),
            },
        );
        self.modal = Some((Modal::DeleteEnv(dialog), sub));
        cx.notify();
    }

    /// Takes deleted worktrees off their projects' lists.
    fn forget_envs(
        &mut self,
        paths: &[std::path::PathBuf],
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if paths.is_empty() {
            return;
        }
        // Their sessions stop, and their transcripts go from disk.
        let pids: Vec<String> = self
            .projects
            .iter()
            .flat_map(|p| &p.environments)
            .filter(|e| paths.contains(&e.path))
            .flat_map(Self::phone_ids_of)
            .collect();
        self.forget_phone_sessions(&pids);
        let sessions = self
            .projects
            .iter_mut()
            .flat_map(|p| &mut p.environments)
            .filter(|e| paths.contains(&e.path))
            .flat_map(|e| std::mem::take(&mut e.sessions))
            .collect();
        self.forget_sessions(sessions, window, cx);
        let before = self.projects.len();
        let touched = drop_envs(&mut self.projects, &mut self.active_project, paths);
        if self.projects.len() != before {
            self.sync_ci_roots(cx);
            self.push_linear_host(cx);
            // The targets of a project that went must stop coming here.
            self.sync_linear(window, cx);
            self.sync_poll(window, cx);
        }
        self.rail.pick.retain(|p| !paths.contains(p));
        // Their tabs, and the pages of their browser tabs, go with them.
        self.settle_strip(cx);
        self.unfloat_github(cx);
        self.servers_tab_closed(cx);
        self.save(cx);
        for pi in touched {
            self.refresh_project(pi, cx);
        }
        self.ensure_terminal(window, cx);
        cx.notify();
    }

    /// The Profile dialog, with the name the rail's User row shows when
    /// Customize set one.
    pub(super) fn open_profile(
        &mut self,
        label: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        use crate::profile_dialog::{ProfileDialog, ProfileEvent};
        if self.modal_busy(cx) {
            return;
        }
        self.menu = None;
        self.env_menu_open = false;
        let teams = self.teams.clone();
        let git = rail_blocks::git_identity().clone();
        let dialog = cx.new(|cx| ProfileDialog::new(teams, label, git, window, cx));
        let sub = cx.subscribe_in(
            &dialog,
            window,
            |this, _, ev: &ProfileEvent, window, cx| match ev {
                ProfileEvent::Dismissed => this.close_modal(window, cx),
                ProfileEvent::SignIn => {
                    this.close_modal(window, cx);
                    this.open_teams_sign_in(window, cx);
                }
                ProfileEvent::OpenTeam => {
                    this.close_modal(window, cx);
                    this.open_teams_tab(window, cx);
                }
            },
        );
        self.modal = Some((Modal::Profile(dialog), sub));
        cx.notify();
    }

    fn open_account(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.menu = None;
        self.env_menu_open = false;
        let account = self.account.clone();
        let dialog = cx.new(|cx| AccountDialog::new(account, window, cx));
        let sub = cx.subscribe_in(
            &dialog,
            window,
            |this, _, ev: &AccountDialogEvent, window, cx| match ev {
                AccountDialogEvent::OpenUrl(url) => {
                    let url = url.clone();
                    this.close_modal(window, cx);
                    this.open_url(url, window, cx);
                }
                AccountDialogEvent::Dismissed => this.close_modal(window, cx),
            },
        );
        self.modal = Some((Modal::Account(dialog), sub));
        cx.notify();
    }

    /// A pull request or issue in the GitHub tab when `gh` can read it, else
    /// the page as `open_page` opens it.
    fn open_url(&mut self, url: String, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(item) = self.github_item(&url, cx) {
            self.open_github_tab(window, cx);
            self.views.github.update(cx, |v, cx| v.reveal(&item, cx));
        } else {
            self.open_page(url, window, cx);
        }
    }

    /// In a arakun browser tab when there is an environment to hold it and the user
    /// keeps links in Arakun, else the desktop browser.
    fn open_page(&mut self, url: String, window: &mut Window, cx: &mut Context<Self>) {
        if self.has_projects() && crate::browser::links_in_arakun() {
            self.open_browser_tab(url, window, cx);
        } else {
            cx.open_url(&url);
        }
    }

    /// After a run that ended without a clean exit, asks what happened. Not on
    /// the first launch, which has nothing to ask about yet.
    pub fn ask_after_crash(&mut self, cx: &mut Context<Self>) {
        let Some(event) = crate::crash::last_run_ended_badly() else {
            return;
        };
        if !crash_notice_wanted(crate::telemetry::should_ask(), crate::telemetry::blocked()) {
            return;
        }
        let open = island::Open::Feedback {
            error_event: event,
            agent: None,
            category: None,
        };
        self.notify(
            crate::island::AlertKind::Warning,
            "Arakun quit unexpectedly. Tell us what happened?",
            "",
            Some(open),
            cx,
        );
    }

    /// Asks once, on first launch, whether to send crash reports and usage
    /// counts; nothing is sent before the answer.
    pub fn ask_telemetry(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        // The setup asks it as one of its steps, and again here when it
        // closes without an answer.
        if self.modal.is_some() || self.onboarding.is_some() || !crate::telemetry::should_ask() {
            return;
        }
        use crate::telemetry_dialog::{DETAILS_URL, TelemetryDialog, TelemetryEvent};
        let dialog = cx.new(|cx| TelemetryDialog::new(window, cx));
        let sub = cx.subscribe_in(
            &dialog,
            window,
            |this, _, ev: &TelemetryEvent, window, cx| match ev {
                TelemetryEvent::Answered(yes) => {
                    let choice = if *yes {
                        crate::telemetry::Consent::ALL
                    } else {
                        crate::telemetry::Consent::NONE
                    };
                    // Followed for this run either way; unsaved, it asks again next launch.
                    if let Err(e) = crate::telemetry::set_consent(choice) {
                        eprintln!("arakun: {e}");
                    }
                    this.close_modal(window, cx);
                }
                // The desktop browser: the dialog stays open over the app.
                TelemetryEvent::OpenDetails => cx.open_url(DETAILS_URL),
                TelemetryEvent::Dismissed => this.close_modal(window, cx),
            },
        );
        self.modal = Some((Modal::Telemetry(dialog), sub));
        cx.notify();
    }

    /// Shows a survey over the app. `extra` fills response properties the
    /// survey leaves empty, such as the error a survey is about.
    pub fn open_survey(
        &mut self,
        survey: crate::surveys::model::Survey,
        src: crate::telemetry::Source,
        extra: Vec<(String, serde_json::Value)>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        use crate::survey_dialog::{SurveyDialog, SurveyEvent};
        use crate::surveys::{ids, send, store};
        match survey_card::decide(self.modal.is_some() || self.onboarding.is_some(), src) {
            survey_card::Ask::Open => {}
            survey_card::Ask::Drop => return,
            // One request at a time: a newer one replaces an older.
            survey_card::Ask::Defer => {
                self.survey.pending = Some(survey_card::PendingAsk { survey, src, extra });
                return;
            }
        }
        // The built-in forms can be sent any number of times; others once.
        let builtin = ids::is_builtin(&survey.id);
        let dialog = cx.new(|cx| SurveyDialog::new(survey, src, extra, window, cx));
        let sub = cx.subscribe_in(
            &dialog,
            window,
            move |this, dialog, ev: &SurveyEvent, window, cx| match ev {
                SurveyEvent::Sent => {
                    if !builtin {
                        store::mark(&dialog.read(cx).survey().id, store::Seen::Answered);
                    }
                }
                SurveyEvent::Dismissed => {
                    let d = dialog.read(cx);
                    if !d.is_sent() && src == crate::telemetry::Source::Popover {
                        store::mark(&d.survey().id, store::Seen::Dismissed);
                        send::dismissed(d.survey());
                    }
                    this.close_modal(window, cx);
                }
                // The dialog shows why and keeps the answers.
                SurveyEvent::Failed(why) => eprintln!("arakun: feedback not sent: {why}"),
                // Behind the dialog, which stays open to finish the survey.
                SurveyEvent::OpenUrl(url) => {
                    if this.has_projects() {
                        this.open_browser_tab(url.clone(), window, cx);
                    } else {
                        cx.open_url(url);
                    }
                }
            },
        );
        self.modal = Some((Modal::Survey(dialog), sub));
        cx.notify();
    }

    fn send_feedback(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.send_feedback_as(None, window, cx);
    }

    /// The feedback form, with its kind (one of `ids::KINDS`) picked when given.
    fn send_feedback_as(
        &mut self,
        kind: Option<&'static str>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        use crate::surveys::{ids, store};
        let survey = store::builtin(&ids::USER);
        self.open_survey(
            survey,
            crate::telemetry::Source::User,
            Vec::new(),
            window,
            cx,
        );
        // Only when this form opened: a busy modal defers it, and then it
        // opens later without the kind, which is still a working form.
        if let (Some(kind), Some((Modal::Survey(dialog), _))) = (kind, &self.modal) {
            dialog.update(cx, |d, cx| {
                d.preset_kind(kind);
                cx.notify();
            });
        }
    }

    fn open_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        // Replacing a dialog that is still saving would lose its result.
        if self.modal_busy(cx) {
            return;
        }
        // A second dialog would read the palettes before the open one's
        // pending save lands, then save over that edit.
        if matches!(self.modal, Some((Modal::Settings(_), _))) {
            return;
        }
        self.menu = None;
        self.env_menu_open = false;
        let arcade = self.arcade;
        let droid = self.account.read(cx).connected();
        let teams = self.teams.clone();
        #[cfg(feature = "coderlabs")]
        let coderspace = {
            let cs = self.coderspace.read(cx);
            cs.connected().then(|| {
                if cs.my_name.is_empty() {
                    "Connected".to_string()
                } else {
                    cs.my_name.clone()
                }
            })
        };
        let views_on = {
            let ext = self.views.extensions.read(cx);
            crate::sidebar::View::EXTENSIONS
                .iter()
                .copied()
                .filter(|v| ext.shows(*v))
                .collect()
        };
        let project = self.has_projects().then(|| self.project()).map(|p| {
            crate::settings_view::ProjectSettings {
                path: p.path.clone(),
                name: p.name.clone(),
                setup: p.setup.clone(),
                check: p.check.clone(),
                base: p.base.clone(),
                sandbox: p.sandbox,
                pass_env: p.pass_env.join(" "),
                remote: p.is_remote(),
            }
        });
        let look = crate::customize_view::Summary::of(
            &match &self.panel.customize {
                Some((view, _)) => view.read(cx).saved().clone(),
                None => crate::palettes::load(),
            },
            &self.sidebar,
        );
        let dialog = cx.new(|cx| {
            SettingsDialog::new(
                arcade,
                droid,
                teams,
                look,
                #[cfg(feature = "coderlabs")]
                coderspace,
                views_on,
                project,
                window,
                cx,
            )
        });
        let rail_w = self.rail.width;
        dialog.update(cx, |d, _| d.rail_w = rail_w);
        let sub = cx.subscribe_in(
            &dialog,
            window,
            |this, _, ev: &SettingsEvent, window, cx| match ev {
                SettingsEvent::Arcade(on) => {
                    this.arcade = *on;
                    crate::mascot::set_animated(*on);
                    this.save(cx);
                    cx.refresh_windows();
                }
                SettingsEvent::RailUsage(agent, on) => {
                    account_view::set_rail_shows(*agent, *on);
                    this.save(cx);
                    cx.notify();
                }
                SettingsEvent::ReduceMotion(r) => {
                    crate::ui::motion::set_choice(*r, cx);
                    this.save(cx);
                }
                #[cfg(target_os = "linux")]
                SettingsEvent::Background(on) => {
                    crate::background::set_enabled(*on);
                    this.save(cx);
                }
                SettingsEvent::Island(on) => {
                    crate::island::set_enabled(*on);
                    this.sync_island(window, cx);
                    this.save(cx);
                }
                SettingsEvent::Zoom(pct) => this.zoom_to(*pct, window, cx),
                SettingsEvent::TabLayout(layout, preset) => {
                    this.apply_tab_layout(*layout, *preset, cx)
                }
                SettingsEvent::SidebarWidth(w) => {
                    this.rail.width = w.clamp(RAIL_W_RANGE.start, RAIL_W_RANGE.end);
                    this.save(cx);
                    cx.notify();
                }
                SettingsEvent::SidebarView(view, on) => {
                    this.views
                        .extensions
                        .update(cx, |v, cx| v.set_view(*view, *on, cx));
                }
                SettingsEvent::ProjectCommand {
                    project,
                    check,
                    command,
                } => {
                    if let Some(p) = this.projects.iter_mut().find(|p| p.path == *project) {
                        if *check {
                            p.check = command.clone();
                            // A check set by hand is one the user wants.
                            if command.is_some() {
                                p.check_declined = false;
                            }
                        } else {
                            p.setup = command.clone();
                        }
                        this.save(cx);
                    }
                }
                SettingsEvent::ProjectSandbox { project, on } => {
                    if let Some(p) = this.projects.iter_mut().find(|p| p.path == *project) {
                        p.sandbox = *on;
                        this.save(cx);
                    }
                }
                SettingsEvent::ProjectPassEnv { project, names } => {
                    if let Some(p) = this.projects.iter_mut().find(|p| p.path == *project) {
                        p.pass_env = names.clone();
                        this.save(cx);
                    }
                }
                SettingsEvent::ProjectBase { project, base } => {
                    if let Some(p) = this.projects.iter_mut().find(|p| p.path == *project) {
                        p.base = base.clone();
                        this.save(cx);
                    }
                }
                SettingsEvent::RestartToUpdate => {
                    this.restarting = true;
                    cx.quit();
                }
                SettingsEvent::OpenAccount => {
                    this.close_modal(window, cx);
                    this.open_account(window, cx);
                }
                SettingsEvent::SendFeedback => {
                    // Their own choice goes first, not a notice's earlier request.
                    this.survey.pending = None;
                    this.close_modal(window, cx);
                    this.send_feedback(window, cx);
                }
                SettingsEvent::OpenExtensions => {
                    this.close_modal(window, cx);
                    this.open_extensions_tab(window, cx);
                }
                #[cfg(feature = "coderlabs")]
                SettingsEvent::OpenTasks => {
                    this.close_modal(window, cx);
                    this.open_tasks_tab(window, cx);
                }
                SettingsEvent::OpenProfile => {
                    this.close_modal(window, cx);
                    this.open_profile(None, window, cx);
                }
                SettingsEvent::OpenUsage => {
                    this.close_modal(window, cx);
                    this.open_usage_tab(window, cx);
                }
                SettingsEvent::OpenCustomize => {
                    this.close_modal(window, cx);
                    this.open_customize(CustomizeTab::Colors, window, cx);
                }
                SettingsEvent::ClaudeSignIn => {
                    this.close_modal(window, cx);
                    this.sign_in(AgentKind::Claude, window, cx);
                }
                SettingsEvent::ClaudeSignOut => {
                    this.close_modal(window, cx);
                    this.sign_out(AgentKind::Claude, window, cx);
                }
                SettingsEvent::CodexSignIn => {
                    this.close_modal(window, cx);
                    this.sign_in(AgentKind::Codex, window, cx);
                }
                SettingsEvent::CodexSignOut => {
                    this.close_modal(window, cx);
                    this.sign_out(AgentKind::Codex, window, cx);
                }
                SettingsEvent::DevinSignIn => {
                    this.close_modal(window, cx);
                    this.sign_in(AgentKind::Devin, window, cx);
                }
                SettingsEvent::DevinSignOut => {
                    this.close_modal(window, cx);
                    this.sign_out(AgentKind::Devin, window, cx);
                }
                SettingsEvent::CursorSignIn => {
                    this.close_modal(window, cx);
                    this.sign_in(AgentKind::Cursor, window, cx);
                }
                SettingsEvent::CursorSignOut => {
                    this.close_modal(window, cx);
                    this.sign_out(AgentKind::Cursor, window, cx);
                }
                SettingsEvent::AntigravitySignIn => {
                    this.close_modal(window, cx);
                    this.sign_in(crate::agent::AgentKind::Antigravity, window, cx);
                }
                SettingsEvent::OpenCodeSignIn => {
                    this.close_modal(window, cx);
                    this.sign_in(AgentKind::OpenCode, window, cx);
                }
                SettingsEvent::OpenCodeGoSignIn => {
                    this.close_modal(window, cx);
                    let cmd = crate::agent::opencode_go_login_command();
                    let kind = AgentKind::OpenCode;
                    this.run_account_command("Sign in to OpenCode Go", kind, cmd, window, cx);
                }
                SettingsEvent::OpenCodeSignOut => {
                    this.close_modal(window, cx);
                    this.sign_out(AgentKind::OpenCode, window, cx);
                }
                #[cfg(feature = "coderlabs")]
                SettingsEvent::DisconnectCoderSpace => {
                    this.coderspace.update(cx, |cs, cx| cs.disconnect(cx));
                    cx.notify();
                }
                SettingsEvent::Dismissed => this.close_modal(window, cx),
            },
        );
        self.modal = Some((Modal::Settings(dialog), sub));
        cx.notify();
    }

    fn toggle_panel(&mut self, cx: &mut Context<Self>) {
        // Customize holds the panel's place: the toggle brings the workspace
        // panel back there rather than hiding it unseen.
        if self.panel.customize.is_some() {
            self.close_customize(cx);
            if !self.panel.open {
                self.panel.open = true;
                self.save(cx);
            }
            cx.notify();
            return;
        }
        self.panel.open = !self.panel.open;
        self.save(cx);
        cx.notify();
    }

    /// Opens the Customize panel in the workspace panel's place, on `tab`, or
    /// moves the open one to it. It shows without a project too, so how the
    /// app looks is always changed from the same place.
    pub(super) fn open_customize(
        &mut self,
        tab: CustomizeTab,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if matches!(self.modal, Some((Modal::Settings(_), _))) {
            self.close_modal(window, cx);
        }
        self.menu = None;
        if let Some((view, _)) = &self.panel.customize {
            view.update(cx, |v, cx| v.set_tab(tab, cx));
            self.panel.customize_tab = tab;
        } else {
            let view = cx.new(|cx| CustomizeView::new(tab, window, cx));
            let sub =
                cx.subscribe_in(
                    &view,
                    window,
                    |this, _, ev: &CustomizeEvent, window, cx| match ev {
                        CustomizeEvent::Close => this.close_customize(cx),
                    CustomizeEvent::Tab(tab) => {
                        this.panel.customize_tab = *tab;
                        cx.notify();
                    }
                        CustomizeEvent::OpenGallery => this.open_gallery_tab(window, cx),
                        CustomizeEvent::Grays(g) => {
                            theme::set_grays(*g);
                            this.save(cx);
                            cx.refresh_windows();
                        }
                        CustomizeEvent::TypeScale(t) => {
                            crate::ui::set_type_scale(*t);
                            this.save(cx);
                            cx.refresh_windows();
                        }
                        CustomizeEvent::DiffPalette(p) => {
                            theme::set_diff_palette(*p);
                            this.save(cx);
                            cx.refresh_windows();
                        }
                        CustomizeEvent::Glass(on) => {
                            theme::set_glass(*on);
                            window.set_background_appearance(theme::window_background());
                            this.save(cx);
                            cx.refresh_windows();
                        }
                        CustomizeEvent::Contrast(c) => {
                            theme::set_contrast(*c);
                            // `apply` can't update this window while the
                            // callback holds it, so set it here.
                            window.set_background_appearance(theme::window_background());
                            this.save(cx);
                            crate::ui::contrast::apply(cx);
                        }
                        CustomizeEvent::SidebarStyle(style) => {
                            let style = *style;
                            this.change_sidebar(cx, |s| s.style = style);
                        }
                        CustomizeEvent::SidebarPreset(p) => {
                            this.apply_sidebar_preset(*p, cx);
                        }
                        CustomizeEvent::RailWidth(w) => {
                            this.rail.width = *w;
                            this.save(cx);
                            cx.notify();
                        }
                        CustomizeEvent::ResetSidebar => {
                            this.change_sidebar(cx, |s| *s = crate::sidebar::Sidebar::default());
                            this.clear_builder_selection();
                            this.rail.width = crate::state::RAIL_W;
                            this.save(cx);
                        }
                        CustomizeEvent::UndoSidebar(redo) => this.undo_sidebar(*redo, cx),
                        CustomizeEvent::CopyLook { look, sidebar } => {
                            let mut look = look.clone();
                            if *sidebar {
                                look.sidebar = serde_json::to_value(&this.sidebar).ok();
                            }
                            cx.write_to_clipboard(gpui::ClipboardItem::new_string(look.encode()));
                        }
                        CustomizeEvent::ImportLayout(layout) => {
                            match crate::sidebar::Sidebar::parse(layout.to_string().as_bytes()) {
                                Some(mut s) => {
                                    // A command pin runs on a click and shows only its
                                    // title, so one from a pasted layout is left out.
                                    let dropped = s.drop_commands();
                                    this.change_sidebar(cx, |cur| *cur = s);
                                    this.clear_builder_selection();
                                    this.builder.flash = Some(if dropped == 0 {
                                        "Sidebar layout imported".into()
                                    } else {
                                        format!(
                                            "Sidebar layout imported without its {dropped} command{}; add commands with Command…",
                                            if dropped == 1 { "" } else { "s" }
                                        )
                                        .into()
                                    });
                                }
                                None => {
                                    this.builder.flash = Some("That sidebar layout could not be read".into())
                                }
                            }
                            cx.notify();
                        }
                    },
                );
            let shell = cx.entity().downgrade();
            view.update(cx, |v, _| {
                v.set_arrange(std::rc::Rc::new(move |window, cx| {
                    shell
                        .upgrade()
                        .map(|s| s.update(cx, |s, cx| s.render_arrange_panel(window, cx)))
                }))
            });
            self.panel.customize_tab = tab;
            self.panel.customize = Some((view, sub));
        }
        cx.notify();
    }

    /// Closes the Customize panel; its last edit is saved as it goes.
    fn close_customize(&mut self, cx: &mut Context<Self>) {
        self.panel.customize = None;
        // The views the Add chips marked new have been seen, except Teams
        // while the server keeps the account out: it stays new until it shows.
        let hidden: &[crate::sidebar::View] = if self.teams.read(cx).enabled() {
            &[]
        } else {
            &[crate::sidebar::View::Teams]
        };
        if self.sidebar.mark_known(hidden) {
            self.sidebar.save();
        }
        cx.notify();
    }

    /// Shows or hides the sidebar alone: Ctrl+B, its buttons and the palette
    /// all run this, so one name always does one thing.
    fn toggle_rail(&mut self, cx: &mut Context<Self>) {
        if self.sidebar.style.collapse == crate::sidebar::Collapse::Never {
            // Otherwise the key looks broken.
            self.flash(
                "The sidebar always shows: Customize sidebar → When hidden.",
                cx,
            );
            return;
        }
        self.rail.open = !self.rail.open;
        self.save(cx);
        cx.notify();
    }

    /// Turns redacted mode on or off. Every window draws again at once, so
    /// nothing private stays on screen for a frame longer than it must.
    pub(super) fn toggle_redacted(&mut self, cx: &mut Context<Self>) {
        crate::redact::set(!crate::redact::on());
        cx.refresh_windows();
    }

    /// The rail holds "Add project", so it stays while there is no project.
    /// Narrowed to icons it still shows, and some rails never close.
    fn rail_shown(&self) -> bool {
        use crate::sidebar::Collapse;
        self.rail.open
            || self.arranging()
            || !self.has_projects()
            || matches!(
                self.sidebar.style.collapse,
                Collapse::Icons | Collapse::Never
            )
    }

    /// Where the title bar starts, from the window's left edge: past the rail
    /// when it shows on the left.
    fn titlebar_x(&self) -> f32 {
        if self.rail_shown() && self.sidebar.style.side == crate::sidebar::Side::Left {
            gap() + self.rail_width() + gap()
        } else {
            gap()
        }
    }

    /// How wide the rail is drawn: its icons, or its dragged width as it fits.
    fn rail_width(&self) -> f32 {
        if self.rail_narrow() {
            rail_blocks::RAIL_ICONS_W
        } else {
            self.side_fit.get()[0]
        }
    }

    fn panel_width(&self) -> f32 {
        self.side_fit.get()[1]
    }

    /// Narrows the side cards for this frame when the window can't hold them
    /// at their dragged widths and still leave `MAIN_MIN_W` for the work.
    /// The dragged widths stay, so widening the window brings them back.
    fn fit_side_cards(&self, window: &Window) {
        let rail = self.rail_shown();
        let viewport_w = window.viewport_size().width.as_f32();
        let rail_card = match (rail, self.rail_narrow()) {
            (false, _) => (0., 0.),
            (true, true) => (rail_blocks::RAIL_ICONS_W, rail_blocks::RAIL_ICONS_W),
            (true, false) => (self.rail.width, RAIL_W_RANGE.start),
        };
        // Each card's narrowest, with the handle beside it.
        let crowded = crowd_out(
            viewport_w - 2. * gap() - MAIN_FLOOR_W,
            if rail { rail_card.1 + gap() } else { 0. },
            self.panel_wanted().then(|| PANEL_W_RANGE.start + gap()),
            self.panel.customize.is_some(),
        );
        self.side_crowded.set(crowded);
        let panel = self.panel_shown();
        let shown = [rail, panel].iter().filter(|s| **s).count() as f32;
        // The window's padding at both sides, and one handle beside each card.
        let room = viewport_w - (2. + shown) * gap() - MAIN_MIN_W;
        let [r, p] = fit_sides(
            room,
            [
                rail_card,
                if panel {
                    (self.panel_preferred_w(), PANEL_W_RANGE.start)
                } else {
                    (0., 0.)
                },
            ],
        );
        self.side_fit.set([
            if rail { r } else { self.rail.width },
            if panel { p } else { self.panel.width },
        ]);
    }

    /// The workspace panel is open and the window has room for it this frame.
    fn panel_shown(&self) -> bool {
        self.panel_wanted() && !self.side_crowded.get()
    }

    /// Something should sit in the workspace panel's place: the panel of a
    /// project, or Customize, which also shows without one.
    fn panel_wanted(&self) -> bool {
        self.panel.customize.is_some() || (self.has_projects() && self.panel.open)
    }

    /// The panel's dragged width, or wider while Customize is in it (its
    /// palettes sit three to a row) or while it shows a subagent's transcript.
    fn panel_preferred_w(&self) -> f32 {
        if self.panel.customize.is_some() {
            self.panel.width.max(crate::customize_view::CUSTOMIZE_W)
        } else if self.panel.tab == PanelTab::Session && self.subagents.view.is_some() {
            self.panel.width.max(SUBAGENT_TRANSCRIPT_W)
        } else {
            self.panel.width
        }
    }

    /// The gap between two cards doubles as the resize handle for the side card.
    fn side_handle(&self, side: SideDrag, cx: &mut Context<Self>) -> impl IntoElement {
        let w = gap();
        let grab = w.max(SIDE_HANDLE_MIN_W);
        div().flex_none().w(gap_px()).h_full().relative().child(
            // Drawn over the cards, so a narrow or zero gap can still be
            // grabbed across the cards' edges.
            gpui::deferred(
                div()
                    .id(("side-handle", side as usize))
                    .absolute()
                    .top_0()
                    .bottom_0()
                    .left(px((w - grab) / 2.))
                    .w(px(grab))
                    .cursor_col_resize()
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _, _, cx| {
                            this.side_drag = Some(side);
                            cx.stop_propagation();
                            cx.notify();
                        }),
                    ),
            ),
        )
    }

    fn show_changes(&mut self, cx: &mut Context<Self>) {
        self.set_panel_tab(PanelTab::Changes, cx);
        if !self.panel.open {
            self.panel.open = true;
            self.save(cx);
        }
        self.refresh_visible(cx);
        cx.notify();
    }

    fn open_switcher(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.open_palette(window, cx);
    }

    fn open_session_at(
        &mut self,
        pi: usize,
        ei: usize,
        id: SharedString,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let exists = self
            .projects
            .get(pi)
            .and_then(|p| p.environments.get(ei))
            .is_some_and(|e| e.session_index(&id).is_some());
        if !exists {
            return;
        }
        self.select_env(pi, ei, window, cx);
        self.open_session(id, window, cx);
    }

    /// The open dialog runs work that must not lose its result.
    fn modal_busy(&self, cx: &App) -> bool {
        match &self.modal {
            Some((Modal::GitConfirm(d), _)) => d.read(cx).busy(),
            Some((Modal::DeleteEnv(d), _)) => d.read(cx).busy(),
            // A click outside while it checks would lose the save's error.
            Some((Modal::Pilot(d), _)) => d.read(cx).busy(),
            _ => false,
        }
    }

    fn close_modal(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        // The backdrop closes the sign-in dialog without its own dismiss:
        // give up a code still waiting on GitHub here too.
        if matches!(self.modal, Some((Modal::TeamsSignIn(_), _))) {
            self.teams.update(cx, |t, cx| {
                if !t.signed_in() {
                    t.cancel(cx);
                }
            });
        }
        if let Some((Modal::Attach(picker), _)) = self.modal.take() {
            picker.update(cx, |picker, cx| picker.release(cx));
        }
        if let Some(focus) = self.onboarding_focus() {
            window.focus(focus, cx);
        } else if self.has_projects() {
            self.focus_composer(window, cx);
        } else {
            window.focus(&self.focus, cx);
        }
        self.open_pending_survey(window, cx);
        cx.notify();
    }

    /// A project about to show would show the environment it was left on.
    /// When the cleanup is removing that one right now (`shell::cleanup`),
    /// the project shows its main checkout instead.
    fn avoid_env_being_removed(&mut self, project: usize) {
        let Some(p) = self.projects.get(project) else {
            return;
        };
        let going = p
            .environments
            .get(p.active_env)
            .is_some_and(|e| self.being_cleaned_up(&e.path));
        if going {
            let main = p.environments.iter().position(|e| e.is_checkout);
            self.projects[project].active_env = main.unwrap_or(0);
        }
    }

    fn select_project(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        // The one on screen until now was in use until now. Without this a
        // switch right after a long sleep leaves it with its old date, and
        // the cleanup could take what was just open.
        self.mark_visible_used();
        self.avoid_env_being_removed(index);
        self.active_project = index;
        self.env_menu_open = false;
        self.after_switch(window, cx);
    }

    /// A paired review belongs to one environment, so every path that
    /// changes the one on screen tells Teams, which ends a grant made in
    /// another one. Without this it would last until the Teams tab draws.
    fn sync_teams_project(&mut self, cx: &mut Context<Self>) {
        let dir = self.has_projects().then(|| self.env().path.clone());
        self.teams_view.update(cx, |v, cx| match dir {
            Some(dir) => v.set_project(dir, cx),
            None => v.clear_project(cx),
        });
    }

    fn after_switch(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.sync_teams_project(cx);
        let env = self.env().path.clone();
        self.composer.update(cx, |c, _| c.show_env(&env));
        // The strip goes to the tab this environment showed last.
        self.show_env_tab(false);
        self.note_active_tab(cx);
        self.mark_visible_used();
        // The project or environment switched to may show its GitHub tab.
        self.unfloat_github(cx);
        self.refresh_visible(cx);
        self.ensure_active_live(window, cx);
        self.ensure_terminal(window, cx);
        if self.panel.tab == PanelTab::Explorer {
            self.ensure_explorer(cx);
        }
        self.save(cx);
        cx.notify();
    }

    fn select_env(
        &mut self,
        project: usize,
        env: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // Its folder is going away this moment (`shell::cleanup`).
        if self.projects[project]
            .environments
            .get(env)
            .is_some_and(|e| self.being_cleaned_up(&e.path))
        {
            return;
        }
        // The one on screen until now was in use until now. Without this a
        // switch right after a long sleep leaves it with its old date, and
        // the cleanup could take what was just open.
        self.mark_visible_used();
        self.active_project = project;
        self.projects[project].active_env = env;
        self.env_menu_open = false;
        self.after_switch(window, cx);
    }

    fn cycle_env(&mut self, delta: isize, window: &mut Window, cx: &mut Context<Self>) {
        if !self.has_projects() {
            return;
        }
        let p = &self.projects[self.active_project];
        let n = p.environments.len() as isize;
        let next = (p.active_env as isize + delta).rem_euclid(n) as usize;
        self.select_env(self.active_project, next, window, cx);
    }

    fn select_tab(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        self.activate_tab(index, window, cx);
        self.note_active_tab(cx);
    }

    fn activate_tab(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        let prev = self.strip.active().map(|t| t.id);
        self.strip.active_tab = index;
        // Back and forward come here without `note_active_tab`.
        self.follow_tab(cx);
        self.bring_split(prev);
        self.strip.settle_split(prev);
        self.mark_on_screen_read(cx);
        self.unfloat_github(cx);
        self.ensure_active_live(window, cx);
        self.save(cx);
        cx.notify();
    }

    /// Remembers the active tab for the back/forward buttons.
    fn note_active_tab(&mut self, cx: &mut Context<Self>) {
        // The GitHub tab came to the front: a sheet floating over another
        // tab stays open in it instead of hiding its list.
        self.unfloat_github(cx);
        // Every path that changes the active tab ends here, so the environment
        // follows it and the split settles here too.
        self.follow_tab(cx);
        self.bring_split(self.strip.tab_back.last().copied());
        let strip = &mut self.strip;
        strip.settle_split(strip.tab_back.last().copied());
        self.mark_on_screen_read(cx);
        let strip = &mut self.strip;
        let Some(id) = strip.active().map(|t| t.id) else {
            return;
        };
        if strip.tab_back.last() == Some(&id) {
            return;
        }
        strip.tab_back.push(id);
        let excess = strip.tab_back.len().saturating_sub(64);
        strip.tab_back.drain(..excess);
        strip.tab_fwd.clear();
    }

    /// Back/forward through the tabs, skipping tabs that were closed since.
    fn tab_history(&mut self, back: bool, window: &mut Window, cx: &mut Context<Self>) {
        if !self.has_projects() {
            return;
        }
        let target = {
            let env = &mut self.strip;
            let mut target = None;
            if back {
                // Forward returns to the tab Back started from, not to a
                // closed one skipped on the way.
                let from = env.tab_back.last().copied().unwrap_or_default();
                while env.tab_back.len() > 1 {
                    env.tab_back.pop();
                    let prev = env.tab_back.last().copied().unwrap_or_default();
                    if let Some(i) = env.tabs.iter().position(|t| t.id == prev) {
                        env.tab_fwd.push(from);
                        target = Some(i);
                        break;
                    }
                }
            } else {
                while let Some(id) = env.tab_fwd.pop() {
                    if let Some(i) = env.tabs.iter().position(|t| t.id == id) {
                        env.tab_back.push(id);
                        target = Some(i);
                        break;
                    }
                }
            }
            target
        };
        if let Some(index) = target {
            self.activate_tab(index, window, cx);
        }
    }

    fn can_go_back(&self) -> bool {
        let env = &self.strip;
        env.tab_back.len() > 1
            && env.tab_back[..env.tab_back.len() - 1]
                .iter()
                .any(|id| env.tabs.iter().any(|t| t.id == *id))
    }

    fn can_go_forward(&self) -> bool {
        let env = &self.strip;
        env.tab_fwd
            .iter()
            .any(|id| env.tabs.iter().any(|t| t.id == *id))
    }

    /// Closes a tab the user asked to close; Reopen closed tab can bring it back.
    fn close_tab(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        if self.can_close_tab(index) {
            self.remove_tab(index, true, window, cx);
        } else if crate::tab_layout::current().pinned_bar()
            && let Some(tab) = self.strip.tabs.get(index).filter(|t| t.pinned)
        {
            // The last tab of the environment on screen stays open, but
            // closing it still takes it off the bar.
            let id = tab.id;
            self.unpin_tab(id, cx);
        }
    }

    /// Whether the user may close the tab at `index`. Not the active tab when
    /// the environment on screen has no other: `remove_tab` would only put a
    /// blank New session tab in its place, which the user can open themselves.
    fn can_close_tab(&self, index: usize) -> bool {
        if !self.has_projects() || index >= self.strip.tabs.len() {
            return false;
        }
        // In a split, the other pane's tab is left to show.
        !self.strip.is_last_on_screen(index, &self.env().path)
            || (index == self.strip.active_tab && self.split_other().is_some())
    }

    /// Closes a tab that went away by itself (its session or diff is gone, the
    /// page closed itself), so Reopen closed tab skips it.
    fn discard_tab(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        self.remove_tab(index, false, window, cx);
    }

    fn remove_tab(
        &mut self,
        index: usize,
        remember: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.has_projects() {
            return;
        }
        let mut closed_session = None;
        let mut closed_servers = false;
        let here = self.env().path.clone();
        let was_active = index == self.strip.active_tab;
        if let Some(tab) = self.strip.remove(index) {
            // Closing the tab on screen keeps its environment on screen: a
            // closed view, or the last tab of the environment, must not land
            // on a neighbour that works elsewhere and bring that one in.
            let strays = tab.env.is_none()
                || !self
                    .strip
                    .active()
                    .is_some_and(|t| t.env.as_deref().is_none_or(|e| e == here));
            if was_active && strays {
                self.strip.active_tab = match self
                    .strip
                    .front_of(&here)
                    .or_else(|| self.strip.unsplit_to_other(&here))
                {
                    Some(i) => i,
                    None => self.strip.add(Tab::new_session(&here)),
                };
            }
            if let Some(kind) = reopenable(&tab, cx).filter(|_| remember) {
                self.strip.remember_closed(index, kind, tab.env.clone());
            }
            closed_servers = matches!(tab.kind, TabKind::Servers);
            if let Some(b) = &tab.browser {
                b.view.update(cx, |v, cx| v.close(cx));
            }
            if let (TabKind::Session { id, .. }, Some(env)) = (tab.kind, tab.env) {
                closed_session = Some((env, id));
            }
        }
        if let Some((path, id)) = closed_session {
            self.retire_if_hidden(&path, &id, cx);
        }
        if closed_servers {
            self.servers_tab_closed(cx);
        }
        self.note_active_tab(cx);
        self.ensure_active_live(window, cx);
        self.save(cx);
        cx.notify();
    }

    /// One thread per worktree: a new session gets a worktree of its own,
    /// unless the open one is a worktree with no thread yet. The main
    /// checkout takes none.
    fn new_session(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.has_projects() {
            return;
        }
        if !self.env().sessions.is_empty() || self.env().is_checkout {
            self.draft_new_session(self.active_project, window, cx);
            return;
        }
        self.show_new_session_tab(window, cx);
    }

    /// A new session in project `pi`: the New session tab of its main
    /// checkout, ready to type in at once. The first prompt sent there makes
    /// the worktree, so nobody waits on git before writing. On a server the
    /// worktree is made first, as a prompt queued for it would be lost, and
    /// so when environments are spread and a server's turn is next.
    pub(super) fn draft_new_session(
        &mut self,
        pi: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.menu = None;
        self.env_menu_open = false;
        let Some(project) = self.projects.get(pi) else {
            return;
        };
        let on_server = project
            .environments
            .get(project.active_env)
            .is_some_and(|e| e.remote.is_some())
            || project.is_remote();
        let checkout = project
            .environments
            .iter()
            .position(|e| e.is_checkout && e.remote.is_none());
        // Spread across machines, the balancer says where it goes, not the
        // open environment: a prompt written in the draft would stay on this
        // computer, so a server's pick is made at once there.
        let here = match self.spread_target(pi) {
            Some(Some(_)) => false,
            Some(None) => true,
            None => !on_server,
        };
        match checkout {
            Some(ei) if here => {
                self.select_env(pi, ei, window, cx);
                self.show_new_session_tab(window, cx);
            }
            _ => self.quick_create_env(pi, window, cx),
        }
    }

    /// The open environment's New session tab, made when it has none.
    fn show_new_session_tab(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let path = self.env().path.clone();
        let blank = self
            .strip
            .find_in(&path, |t| matches!(t.kind, TabKind::NewSession));
        self.strip.active_tab = match blank {
            Some(i) => i,
            None => self.strip.add(Tab::new_session(&path)),
        };
        self.note_active_tab(cx);
        self.focus_composer(window, cx);
        cx.notify();
    }
}

/// What Reopen closed tab brings back for `tab`: a browser tab at the page it
/// was on. `None` for tabs not worth reopening, such as a blank New session tab.
fn reopenable(tab: &Tab, cx: &App) -> Option<TabKind> {
    match &tab.kind {
        TabKind::NewSession => None,
        TabKind::Browser { url } => {
            let url = tab
                .browser
                .as_ref()
                .map(|b| b.view.read(cx).url())
                .unwrap_or_else(|| url.clone());
            let blank = matches!(url.as_ref(), "" | "about:blank");
            (!blank).then_some(TabKind::Browser { url })
        }
        kind => Some(kind.clone()),
    }
}

impl Focusable for Shell {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Shell {
    /// Whether a pane in view already shows a raccoon: the start screen's
    /// ASCII face in the focused pane, a browser tab's new-tab page, or the
    /// raccoon on the composer of a live session. Same conditions as
    /// `render_pane_body`, so a session that is still connecting leaves the
    /// rail's raccoon in place.
    fn main_shows_raccoon(&self, cx: &App) -> bool {
        // With no project, the welcome screen shows the ASCII face.
        if !self.has_projects() {
            return true;
        }
        let tab = self.strip.active();
        let home = matches!(tab.map(|t| &t.kind), None | Some(TabKind::NewSession));
        let browser_home = tab
            .and_then(|t| t.browser.as_ref())
            .is_some_and(|b| b.view.read(cx).shows_raccoon());
        home || browser_home || self.composer_chat().is_some()
    }

    /// Whether the rail shows its raccoon. It steps aside while the main view
    /// shows one, so only one raccoon is in view, but comes back when a session
    /// out of view needs input: its mood is the only cue across sessions.
    fn rail_shows_raccoon(&self, inbox: &[InboxItem], cx: &App) -> bool {
        if !self.main_shows_raccoon(cx) {
            return true;
        }
        // No project, so no session to wait on input.
        if !self.has_projects() {
            return false;
        }
        let in_view = match self.strip.tabs.get(self.composer_tab()).map(|t| &t.kind) {
            Some(TabKind::Session { id, .. }) => Some(id),
            _ => None,
        };
        inbox
            .iter()
            .any(|i| i.status == Status::NeedsInput && Some(&i.id) != in_view)
    }

    fn render_main(&self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        match self.split_other() {
            Some(other) => self.render_split(other, window, cx),
            None => self.render_pane(self.strip.active_tab, true, window, cx),
        }
    }

    /// One tab's content. The composer goes under the session it writes to
    /// (`composer_tab`).
    fn render_pane(
        &self,
        index: usize,
        focused: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let body = self.render_pane_body(index, focused, window, cx);
        if !self.strip.tabs.get(index).is_some_and(|t| t.preview) {
            return body;
        }
        // A click anywhere inside a preview tab means it's in use: keep it.
        div()
            .size_full()
            .capture_any_mouse_down(cx.listener(|this, _, _, cx| this.pin_preview(cx)))
            .child(body)
            .into_any_element()
    }

    fn render_pane_body(
        &self,
        index: usize,
        focused: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let env = self.env();
        let tab = self.strip.tabs.get(index);
        match tab.map(|t| t.kind.clone()) {
            Some(TabKind::Session { id, .. }) => {
                // The other pane's session may work in another environment.
                let env = tab.map_or(env, |t| self.tab_env(t));
                let live = env
                    .session_index(&id)
                    .and_then(|i| env.sessions[i].live.as_ref());
                let transcript: AnyElement = match live {
                    // Cached: GPUI reuses the transcript's layout and paint from the last
                    // frame until it notifies, instead of rebuilding a long session for
                    // every frame something else draws (a page beside it renders at 165 Hz).
                    Some(live) => gpui::AnyView::from(live.view.clone())
                        .cached(gpui::StyleRefinement::default().size_full())
                        .into_any_element(),
                    None => switching::render_waiting(&id),
                };
                // One pane only: in a split, moving the focus between the
                // two must not fade either.
                let alone = self.split_other().is_none();
                if alone {
                    self.note_shown(&id, live.is_some());
                }
                div()
                    .flex()
                    .flex_col()
                    .size_full()
                    .child(
                        div()
                            .relative()
                            .flex_1()
                            .min_h_0()
                            .child(transcript)
                            .when(alone, |d| d.children(self.render_fade())),
                    )
                    .when(index == self.composer_tab(), |d| {
                        d.child(
                            div()
                                .relative()
                                .mx_auto()
                                .w_full()
                                .max_w(px(896.))
                                .p(crate::ui::sp(6.))
                                .pt_0()
                                // Before the composer, so the bar hides its chin. It moves
                                // itself on top of the card above the composer when one shows.
                                .when_some(live, |d, live| {
                                    let mascot = live.view.read(cx).mascot();
                                    let top = mascot.read(cx).ledge();
                                    d.child(
                                        div().absolute().top(px(-top)).right(px(40.)).child(mascot),
                                    )
                                })
                                .child(self.render_composer(false, cx)),
                        )
                    })
                    .into_any_element()
            }
            Some(TabKind::Browser { .. }) => match tab.and_then(|t| t.browser.as_ref()) {
                Some(b) => b.view.clone().into_any_element(),
                None => panel_empty("Opening page…").into_any_element(),
            },
            Some(TabKind::File { .. }) | Some(TabKind::Diff { .. }) => {
                self.render_document(&self.strip.tabs[index], window, cx)
            }
            Some(TabKind::Notes) => self.render_notes(cx).into_any_element(),
            #[cfg(feature = "coderlabs")]
            Some(TabKind::Tasks) => {
                // Session statuses change all the time; reading them here keeps the drawer current.
                self.sync_task_sessions(cx);
                self.tasks_view.clone().into_any_element()
            }
            Some(TabKind::Runs) => {
                self.sync_runs_view(cx);
                self.views.runs.clone().into_any_element()
            }
            Some(TabKind::Memory) => {
                let dir = env.path.clone();
                self.views.memory.update(cx, |v, cx| v.set_dir(&dir, cx));
                self.views.memory.clone().into_any_element()
            }
            Some(TabKind::Cloudflare) => {
                self.with_subnav(index, self.views.cloudflare.clone().into_any_element(), cx)
            }
            #[cfg(feature = "coderlabs")]
            Some(TabKind::Site) => self.site_view.clone().into_any_element(),
            #[cfg(feature = "team")]
            Some(TabKind::Team) => self.team_view.clone().into_any_element(),
            Some(TabKind::PostHog) => {
                // Follows the visible project, also when it changes under an open tab.
                let root = self.project().path.clone();
                self.views
                    .posthog
                    .update(cx, |v, cx| v.set_project(&root, cx));
                self.views.posthog.clone().into_any_element()
            }
            Some(TabKind::Linear) => self.views.linear.clone().into_any_element(),
            Some(TabKind::Doop) => self.views.doop.clone().into_any_element(),
            Some(TabKind::Branches) => {
                // Sessions start and stop all the time; the rows follow them.
                self.sync_branches_view(cx);
                self.views.branches.clone().into_any_element()
            }
            Some(TabKind::GitHub) => {
                let corners = self.pane_card_corners(index);
                self.views
                    .github
                    .update(cx, |v, cx| v.set_card_corners(corners, cx));
                self.views.github.clone().into_any_element()
            }
            Some(TabKind::Extensions) => {
                // Follows the visible environment, also when it changes under an open tab.
                let dir = env.path.clone();
                self.views.extensions.update(cx, |v, cx| {
                    v.set_dir(dir, cx);
                    v.refresh_if_stale(cx);
                });
                self.with_subnav(index, self.views.extensions.clone().into_any_element(), cx)
            }
            Some(TabKind::Phone) => self.views.phone.clone().into_any_element(),
            Some(TabKind::Servers) => {
                // This computer's sessions start and stop all the time; its row follows them.
                self.sync_this_computer(cx);
                self.views.servers.clone().into_any_element()
            }
            Some(TabKind::Teams) => {
                // @bot reads the visible environment's code.
                let dir = env.path.clone();
                self.teams_view.update(cx, |v, cx| v.set_project(dir, cx));
                self.teams_view.clone().into_any_element()
            }
            Some(TabKind::Gallery) => self.views.gallery.clone().into_any_element(),
            Some(TabKind::Isometric) => {
                // Follows the visible environment, also for a tab restored at
                // launch that was never opened from the rail.
                let root = self.env().path.clone();
                self.views.iso.update(cx, |v, cx| v.set_root(root, cx));
                self.views.iso.clone().into_any_element()
            }
            #[cfg(feature = "coderlabs")]
            Some(TabKind::Canvas) => self.canvas_view.clone().into_any_element(),
            Some(TabKind::Usage) => {
                self.sync_usage_view(cx);
                self.views.usage.clone().into_any_element()
            }
            _ if focused => self.render_home(cx).into_any_element(),
            _ => div()
                .flex()
                .flex_col()
                .size_full()
                .items_center()
                .justify_center()
                .gap(crate::ui::sp(10.))
                .text_size(px(ui::TEXT_MD))
                .text_color(theme::text_subtle())
                .child(env.agent.icon(22.))
                .child("New session")
                .child(
                    div()
                        .text_size(px(ui::TEXT_SM))
                        .text_color(theme::text_faint())
                        .child("Click to write the first prompt here"),
                )
                .into_any_element(),
        }
    }

    /// Tab and Shift+Tab move between controls. This listens last, so a view
    /// that uses Tab itself (a field completing, the new-session dialog)
    /// already took it. Terminals, pages and the composer type it instead:
    /// not all of them stop the key, so they are left out by name.
    fn tab_key(&mut self, ev: &gpui::KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let k = &ev.keystroke;
        let m = k.modifiers;
        if k.key != "tab" || m.control || m.alt || m.platform || m.function {
            return;
        }
        if self.composer_focus.is_focused(window) || types_tab(&window.context_stack()) {
            return;
        }
        let step = |window: &mut Window, cx: &mut Context<Self>| {
            if m.shift {
                window.focus_prev(cx);
            } else {
                window.focus_next(cx);
            }
        };
        step(window, cx);
        // gpui's tab order spans the window, so an open dialog would let Tab
        // reach the controls behind it. Step on to the next stop inside one;
        // back at the first stop means no dialog is open. The update dialog
        // covers any other dialog, so while it is open only its stops count.
        let update = self.update_dialog.as_ref().map(|(d, _)| d.focus_handle(cx));
        let inside = |window: &Window, cx: &App| match &update {
            Some(update) => update.contains_focused(window, cx),
            None => in_modal(&window.context_stack()),
        };
        let first = window.focused(cx);
        for _ in 0..MAX_TAB_STOPS {
            if inside(window, cx) {
                break;
            }
            step(window, cx);
            let now = window.focused(cx);
            if now.is_none() || now == first {
                break;
            }
        }
        if !inside(window, cx)
            && let Some(first) = first
        {
            window.focus(&first, cx);
        }
        cx.stop_propagation();
    }
}

/// How many tab stops Tab passes at most looking for one inside a dialog.
const MAX_TAB_STOPS: usize = 1024;

/// Whether focus is inside a dialog (`ui::modal_panel`).
fn in_modal(contexts: &[gpui::KeyContext]) -> bool {
    contexts.iter().any(|c| c.contains(ui::MODAL_CONTEXT))
}

/// Whether focus is in something that types a Tab or moves with it: a
/// terminal, a web page or the palette.
fn types_tab(contexts: &[gpui::KeyContext]) -> bool {
    contexts.iter().any(|c| {
        ["Terminal", "Browser", "Palette"]
            .iter()
            .any(|name| c.contains(name))
    })
}

impl Render for Shell {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.sync_inbox_seen();
        // X11 can enter or leave full screen without moving the window, which
        // tells no bounds observer; the frame drawn for it notices. So does the
        // first frame of a window that has never been saved.
        if self.window_state.as_ref().is_none_or(|w| {
            w.fullscreen != window.is_fullscreen()
                || w.maximized != (window.is_maximized() && !window.is_fullscreen())
        }) {
            self.track_window(window, cx);
        }
        // When what had the keyboard leaves the screen (the composer when a page opens
        // over it), keys reach no element and every shortcut stops until a click.
        if !self.refocus_queued && self.has_projects() && !self.focus.contains_focused(window, cx) {
            self.refocus_queued = true;
            cx.on_next_frame(window, |this, window, cx| {
                this.refocus_queued = false;
                if !this.focus.contains_focused(window, cx) {
                    this.focus_active_content(window, cx);
                }
            });
        }
        let has_projects = self.has_projects();
        self.fit_side_cards(window);
        if has_projects {
            // With a project a view in the welcome's place is a tab again;
            // without one later, the welcome comes back first. The server is
            // told nobody looks at its tab now. After this frame: telling it
            // updates the view, which cannot happen while this one draws.
            if self.alone.take() == Some(home::Alone::Servers) {
                cx.defer_in(window, |this, _, cx| this.servers_tab_closed(cx));
            }
            // A sign-in started before the first project goes on in its dock.
            if self.terminal_alone.is_some() {
                cx.defer_in(window, |this, window, cx| {
                    this.adopt_terminal_alone(window, cx)
                });
            }
        }
        let rail_right = self.sidebar.style.side == crate::sidebar::Side::Right;
        if has_projects && std::mem::take(&mut self.env_followed) {
            cx.defer_in(window, |this, window, cx| {
                this.ensure_active_live(window, cx);
                this.ensure_terminal(window, cx);
            });
        }
        if has_projects {
            if self.browsers_out_of_date() {
                cx.defer_in(window, |this, window, cx| {
                    this.ensure_active_browser(false, window, cx)
                });
            }
        } else if !self.shown_browsers.is_empty() {
            cx.defer_in(window, |this, _, cx| this.sync_browsers(cx));
        }
        // Many paths change the tab in front; this follows all of them.
        if self.teams_view.read(cx).is_shown() != self.teams_shown(window) {
            cx.defer_in(window, |this, window, cx| this.sync_teams_shown(window, cx));
        }
        if self.views.cloudflare.read(cx).is_shown() != self.cloudflare_shown(window)
            || self.views.memory.read(cx).is_shown() != self.memory_shown(window)
            || self.views.usage.read(cx).is_shown() != self.usage_shown(window)
        {
            cx.defer_in(window, |this, window, cx| {
                this.sync_polled_shown(window, cx)
            });
        }
        // With pinned tabs, a session tab that shows nowhere closes, and a
        // lone session pins itself; whatever changed the tabs, this follows.
        if self.pins_unsettled() {
            cx.defer_in(window, |this, _, cx| this.settle_pins(cx));
        }
        if !self.stale_diffs.is_empty() {
            let stale = std::mem::take(&mut self.stale_diffs);
            cx.defer_in(window, |this, window, cx| {
                for (env_path, file) in stale {
                    this.close_diff_tab(&env_path, &file, window, cx);
                }
            });
        }
        let onboarding = self.render_onboarding(window, cx);
        // The setup takes the window's whole place: no rail, panels or dock.
        let chrome = onboarding.is_none();
        let body: AnyElement = if let Some(onboarding) = onboarding {
            onboarding
        } else if has_projects {
            // The tabs sit on the window, above the sheet: only the work itself
            // is a surface. The title bar moves the window itself, so a press
            // anywhere here must not reach the root's handler as well.
            div()
                .map(crate::ui::stop_window_move)
                .flex()
                .flex_col()
                .flex_1()
                .min_w_0()
                .h_full()
                .child(self.render_titlebar(window, cx))
                .child(
                    ui::card()
                        .id("main-card")
                        .relative()
                        .flex_1()
                        .min_h_0()
                        .bg(theme::sheet())
                        .child(self.render_main(window, cx))
                        .when_some(self.render_tab_drop_zones(cx), |d, zones| d.child(zones))
                        .when_some(self.render_image_drop_zone(cx), |d, zone| d.child(zone)),
                )
                .into_any_element()
        } else if let Some(alone) = &self.terminal_alone {
            self.render_terminal_alone(alone, window, cx)
                .into_any_element()
        } else if let Some(alone) = self.alone {
            self.render_view_alone(alone, window, cx).into_any_element()
        } else {
            self.render_welcome(window, cx).into_any_element()
        };

        let update_dialog = self.update_dialog.as_ref().map(|(d, _)| d.clone());
        let palette_open = matches!(self.modal, Some((Modal::Palette(_), _)));
        // Dialogs sit in the middle, like the system's own. Lists that filter
        // as you type stay near the top, so they don't jump as their height changes.
        let center_modal = !matches!(
            self.modal,
            Some((
                Modal::Palette(_) | Modal::Picker(_) | Modal::Attach(_) | Modal::New(_),
                _
            ))
        );
        let modal: Option<(AnyElement, u64)> = self.modal.as_ref().map(|(m, _)| match m {
            Modal::Picker(e) => (e.clone().into_any_element(), e.entity_id().as_u64()),
            Modal::Attach(e) => (e.clone().into_any_element(), e.entity_id().as_u64()),
            Modal::CreateEnv(e) => (e.clone().into_any_element(), e.entity_id().as_u64()),
            Modal::DeleteEnv(e) => (e.clone().into_any_element(), e.entity_id().as_u64()),
            Modal::Command(e) => (e.clone().into_any_element(), e.entity_id().as_u64()),
            Modal::Account(e) => (e.clone().into_any_element(), e.entity_id().as_u64()),
            Modal::Profile(e) => (e.clone().into_any_element(), e.entity_id().as_u64()),
            Modal::Commit(e) => (e.clone().into_any_element(), e.entity_id().as_u64()),
            Modal::GitConfirm(e) => (e.clone().into_any_element(), e.entity_id().as_u64()),
            Modal::PullRequest(e) => (e.clone().into_any_element(), e.entity_id().as_u64()),
            Modal::Palette(e) => (e.clone().into_any_element(), e.entity_id().as_u64()),
            Modal::Settings(e) => (e.clone().into_any_element(), e.entity_id().as_u64()),
            Modal::Automations(e) => (e.clone().into_any_element(), e.entity_id().as_u64()),
            Modal::Session(v) => (v.clone().into_any_element(), v.entity_id().as_u64()),
            Modal::Telemetry(e) => (e.clone().into_any_element(), e.entity_id().as_u64()),
            Modal::TeamsSignIn(e) => (e.clone().into_any_element(), e.entity_id().as_u64()),
            Modal::Pilot(e) => (e.clone().into_any_element(), e.entity_id().as_u64()),
            Modal::New(e) => (e.clone().into_any_element(), e.entity_id().as_u64()),
            Modal::Survey(e) => (e.clone().into_any_element(), e.entity_id().as_u64()),
        });
        let viewport_h = window.viewport_size().height.as_f32();
        let floating_sheet = self.render_floating_sheet(cx);
        let context_menu = self.menu.as_ref().map(|m| {
            let above = match m.target {
                MenuTarget::ModelMenu | MenuTarget::PermissionMenu | MenuTarget::RailUsage => true,
                // Help can be anywhere in the rail; low down it opens upward.
                // So can Running, which the status bar opens too.
                MenuTarget::Help | MenuTarget::Running => m.at.y.as_f32() > viewport_h / 2.,
                _ => false,
            };
            // The room between the anchor and the window edge the menu opens toward.
            let room = if above {
                m.at.y.as_f32() - 24.
            } else {
                viewport_h - m.at.y.as_f32() - 8.
            };
            let menu = div()
                .flex()
                .flex_col()
                .max_h(px(room.max(120.)))
                .child(self.render_context_menu(m, cx));
            // Keyed by where it opened, so a menu opened at another spot
            // arrives again; it slides out of its anchor.
            let key = ((m.at.x.as_f32() as usize) << 16) ^ m.at.y.as_f32() as usize;
            let from = if above {
                ui::motion::From::Below
            } else {
                ui::motion::From::Above
            };
            let menu = ui::motion::enter(menu, ("menu-in", key), from, 4., ui::motion::FAST);
            (m.at, above, menu.into_any_element())
        });

        div()
            .id("shell")
            .key_context("Shell")
            .track_focus(&self.focus)
            .when(self.dock_drag, |d| {
                d.cursor_row_resize()
                    .on_mouse_move(cx.listener(|this, ev: &gpui::MouseMoveEvent, window, cx| {
                        // The dock card's bottom edge is one gap above the window's edge.
                        let viewport_h = window.viewport_size().height.as_f32();
                        let bottom = viewport_h - gap();
                        this.dock_height = (bottom - ev.position.y.as_f32())
                            .clamp(DOCK_MIN_H, dock_max_h(viewport_h))
                            .round();
                        cx.notify();
                    }))
                    .on_mouse_up(
                        MouseButton::Left,
                        cx.listener(|this, _, _, cx| {
                            this.dock_drag = false;
                            this.save(cx);
                            cx.notify();
                        }),
                    )
            })
            .when(self.side_drag.is_some(), |d| {
                d.cursor_col_resize()
                    .on_mouse_move(cx.listener(|this, ev: &gpui::MouseMoveEvent, window, cx| {
                        let window_w = window.viewport_size().width.as_f32();
                        match this.side_drag {
                            Some(SideDrag::Rail) => {
                                let x = ev.position.x.as_f32();
                                let w = if this.sidebar.style.side == crate::sidebar::Side::Right {
                                    window_w - gap() - x
                                } else {
                                    x - gap()
                                };
                                this.rail.width = w.clamp(RAIL_W_RANGE.start, RAIL_W_RANGE.end)
                            }
                            Some(SideDrag::Panel) => {
                                this.panel.width = ((window_w - gap()) - ev.position.x.as_f32())
                                    .clamp(PANEL_W_RANGE.start, PANEL_W_RANGE.end)
                            }
                            None => {}
                        }
                        cx.notify();
                    }))
                    .on_mouse_up(
                        MouseButton::Left,
                        cx.listener(|this, _, _, cx| {
                            this.side_drag = None;
                            this.save(cx);
                            cx.notify();
                        }),
                    )
            })
            // Clicks that reach the root landed on a gap between cards: drag the window.
            .on_mouse_down(MouseButton::Left, |ev, window, _| {
                crate::ui::drag_window(ev, window)
            })
            .on_action(cx.listener(|this, _: &ToggleDock, window, cx| this.toggle_dock(window, cx)))
            .on_action(cx.listener(|this, _: &CustomizeSidebar, window, cx| {
                this.open_customize(CustomizeTab::Sidebar, window, cx)
            }))
            .on_action(cx.listener(|this, _: &OpenCustomize, window, cx| {
                this.open_customize(CustomizeTab::Colors, window, cx)
            }))
            .on_action(cx.listener(|this, _: &ZoomIn, window, cx| {
                this.zoom_to(crate::ui_zoom::next(crate::ui_zoom::pct(), 1), window, cx)
            }))
            .on_action(cx.listener(|this, _: &ZoomOut, window, cx| {
                this.zoom_to(crate::ui_zoom::next(crate::ui_zoom::pct(), -1), window, cx)
            }))
            .on_action(cx.listener(|this, _: &ZoomReset, window, cx| this.zoom_to(100, window, cx)))
            .on_action(cx.listener(|this, _: &FindInChat, window, cx| {
                if let Some(view) = this.active_thread_view() {
                    view.update(cx, |v, cx| v.open_search(window, cx));
                }
            }))
            .on_action(cx.listener(
                |this, link: &crate::agent::markdown::OpenLink, window, cx| {
                    this.open_url(link.url.clone(), window, cx)
                },
            ))
            .on_action(
                cx.listener(|this, _: &NewTerminal, window, cx| this.new_terminal(window, cx)),
            )
            .on_action(cx.listener(|this, _: &NewBrowserTab, window, cx| {
                this.open_browser_tab(String::new(), window, cx)
            }))
            .on_action(cx.listener(|this, _: &AddProject, window, cx| this.open_picker(window, cx)))
            .on_action(
                cx.listener(|this, _: &QuickSwitch, window, cx| this.open_switcher(window, cx)),
            )
            .on_action(
                cx.listener(|this, _: &crate::palette::Pinned1, w, cx| this.open_pinned(0, w, cx)),
            )
            .on_action(
                cx.listener(|this, _: &crate::palette::Pinned2, w, cx| this.open_pinned(1, w, cx)),
            )
            .on_action(
                cx.listener(|this, _: &crate::palette::Pinned3, w, cx| this.open_pinned(2, w, cx)),
            )
            .on_action(
                cx.listener(|this, _: &crate::palette::Pinned4, w, cx| this.open_pinned(3, w, cx)),
            )
            .on_action(
                cx.listener(|this, _: &crate::palette::Pinned5, w, cx| this.open_pinned(4, w, cx)),
            )
            .on_action(
                cx.listener(|this, _: &crate::palette::Pinned6, w, cx| this.open_pinned(5, w, cx)),
            )
            .on_action(
                cx.listener(|this, _: &crate::palette::Pinned7, w, cx| this.open_pinned(6, w, cx)),
            )
            .on_action(
                cx.listener(|this, _: &crate::palette::Pinned8, w, cx| this.open_pinned(7, w, cx)),
            )
            .on_action(
                cx.listener(|this, _: &crate::palette::Pinned9, w, cx| this.open_pinned(8, w, cx)),
            )
            .on_action(cx.listener(|this, _: &ToggleSidebar, _, cx| this.toggle_rail(cx)))
            .on_action(cx.listener(|this, _: &TogglePanel, _, cx| this.toggle_panel(cx)))
            .on_action(cx.listener(|this, _: &ToggleSubagents, _, cx| this.toggle_subagents(cx)))
            .on_action(
                cx.listener(|this, _: &OpenSettings, window, cx| this.open_settings(window, cx)),
            )
            .on_action(
                cx.listener(|this, _: &SendFeedback, window, cx| this.send_feedback(window, cx)),
            )
            .on_action(cx.listener(|this, _: &ToggleRedacted, _, cx| this.toggle_redacted(cx)))
            .on_action(cx.listener(|this, _: &NewEnvironment, window, cx| {
                if this.has_projects() {
                    this.quick_create_env(this.active_project, window, cx)
                }
            }))
            .on_action(
                cx.listener(|this, _: &NewEnvironmentWithOptions, window, cx| {
                    if this.has_projects() {
                        this.open_create_env(this.active_project, window, cx)
                    }
                }),
            )
            .on_action(cx.listener(|this, _: &CloseTab, window, cx| {
                if this.has_projects() {
                    let i = this.strip.active_tab;
                    this.close_tab(i, window, cx)
                }
            }))
            .on_action(
                cx.listener(|this, _: &NextEnvironment, window, cx| this.cycle_env(1, window, cx)),
            )
            .on_action(
                cx.listener(|this, _: &PrevEnvironment, window, cx| this.cycle_env(-1, window, cx)),
            )
            .on_action(cx.listener(|this, _: &NewSession, window, cx| this.new_session(window, cx)))
            .on_action(
                cx.listener(|this, _: &GoBackTab, window, cx| this.tab_history(true, window, cx)),
            )
            .on_action(
                cx.listener(|this, _: &GoForwardTab, window, cx| {
                    this.tab_history(false, window, cx)
                }),
            )
            .map(|d| self.pane_handlers(d, cx))
            .on_key_down(cx.listener(Self::tab_key))
            .capture_key_down(|ev: &gpui::KeyDownEvent, window, cx| {
                if ev.keystroke.key == "escape" && crate::agent::markdown::close_preview(window) {
                    cx.stop_propagation();
                }
            })
            .capture_key_down(cx.listener(|this, ev: &gpui::KeyDownEvent, window, cx| {
                let k = &ev.keystroke;
                // A survey never pops up while someone is typing.
                this.survey.last_typed = Instant::now();
                // The update dialog's Enter and Space press its own buttons.
                if this.menu.is_some()
                    && this.update_dialog.is_none()
                    && !k.modifiers.control
                    && !k.modifiers.alt
                    && !k.modifiers.platform
                    && this.menu_key(&k.key, window, cx)
                {
                    cx.stop_propagation();
                    return;
                }
                if ev.keystroke.key == "escape" {
                    if this.cancel_session_drag(window, cx) {
                        cx.stop_propagation();
                    } else if this.drop_menu.take().is_some()
                        || this.menu.take().is_some()
                        || this.drop_pick()
                    {
                        cx.stop_propagation();
                        cx.notify();
                    }
                }
            }))
            .relative()
            .flex()
            .flex_col()
            .size_full()
            .bg(theme::window_bg())
            .p(crate::ui::window_gap_px())
            .gap(crate::ui::window_gap_px())
            .text_color(theme::content())
            .font_family(theme::FONT_SANS)
            .child(
                div()
                    .flex()
                    .flex_1()
                    .min_h_0()
                    .when(chrome && self.rail_shown() && !rail_right, |d| {
                        d.child(self.render_rail(window, cx))
                            .child(self.side_handle(SideDrag::Rail, cx))
                    })
                    .child(body)
                    .when(chrome && self.panel_shown(), |d| {
                        d.child(self.side_handle(SideDrag::Panel, cx))
                            .child(self.render_panel(cx))
                    })
                    .when(chrome && self.rail_shown() && rail_right, |d| {
                        d.child(self.side_handle(SideDrag::Rail, cx))
                            .child(self.render_rail(window, cx))
                    }),
            )
            .when(chrome && has_projects, |d| {
                d.child(self.render_dock(viewport_h, window, cx))
            })
            .when_some(self.render_session_drop_menu(cx), |d, m| d.child(m))
            .when_some(context_menu, |d, (at, above, menu)| {
                d.child(
                    div()
                        .id("context-menu-backdrop")
                        .absolute()
                        .top_0()
                        .left_0()
                        .size_full()
                        .occlude()
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(|this, _, _, cx| {
                                this.menu = None;
                                cx.notify();
                            }),
                        )
                        .on_mouse_down(
                            MouseButton::Right,
                            cx.listener(|this, _, _, cx| {
                                this.menu = None;
                                cx.notify();
                            }),
                        ),
                )
                .child(gpui::deferred(
                    gpui::anchored()
                        .when(above, |a| a.anchor(gpui::Anchor::BottomLeft))
                        // Composer chips open their menu just above the chip row, not over it.
                        .position(if above {
                            point(at.x - px(12.), at.y - px(16.))
                        } else {
                            at
                        })
                        .snap_to_window_with_margin(px(8.))
                        .child(menu),
                ))
            })
            .when_some(floating_sheet, |d, sheet| d.child(sheet))
            .when_some(modal, |d, (modal, key)| {
                d.child(
                    div()
                        .id("modal-backdrop")
                        .absolute()
                        .top_0()
                        .left_0()
                        .size_full()
                        .flex()
                        .flex_col()
                        .items_center()
                        .map(|d| {
                            if center_modal {
                                d.pt(crate::ui::sp(12.))
                            } else {
                                d.pt(relative(0.10))
                            }
                        })
                        .pb(crate::ui::sp(12.))
                        .px(crate::ui::sp(12.))
                        .occlude()
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(|this, _, window, cx| {
                                // A running deletion still has to report what
                                // went, which its event does.
                                if !this.modal_busy(cx) {
                                    this.close_modal(window, cx)
                                }
                            }),
                        )
                        .child(
                            // Keyed by the dialog, so one opened over another
                            // arrives too. A flex column like the backdrop, so a
                            // tall dialog still fits the window.
                            div()
                                .flex()
                                .flex_col()
                                .items_center()
                                .when(center_modal, |d| d.justify_center())
                                .flex_1()
                                .min_h_0()
                                .w_full()
                                .relative()
                                .child(modal)
                                .with_animation(
                                    ("modal-in", key as usize),
                                    gpui::Animation::new(ui::motion::BASE),
                                    |d, t| {
                                        let (opacity, offset) =
                                            ui::motion::enter_at(t, ui::motion::From::Below, 10.);
                                        d.opacity(opacity).top(px(offset))
                                    },
                                ),
                        )
                        // The palette dims the app further: gpui has no backdrop blur.
                        .with_animation(
                            "modal-backdrop-in",
                            gpui::Animation::new(ui::motion::BASE),
                            move |d, t| {
                                let dim = if palette_open { 0.62 } else { 0.4 };
                                d.bg(gpui::black().opacity(dim * ui::motion::ease_out(t)))
                            },
                        ),
                )
            })
            .when_some(update_dialog, |d, dialog| {
                d.child(
                    div()
                        .id("update-backdrop")
                        .absolute()
                        .top_0()
                        .left_0()
                        .size_full()
                        .flex()
                        .flex_col()
                        .items_center()
                        .pt(relative(0.10))
                        .px(crate::ui::sp(12.))
                        .occlude()
                        .child(ui::motion::enter(
                            div()
                                .flex()
                                .flex_col()
                                .items_center()
                                .w_full()
                                .child(dialog),
                            "update-dialog-in",
                            ui::motion::From::Below,
                            10.,
                            ui::motion::BASE,
                        ))
                        .with_animation(
                            "update-backdrop-in",
                            gpui::Animation::new(ui::motion::BASE),
                            |d, t| d.bg(gpui::black().opacity(0.4 * ui::motion::ease_out(t))),
                        ),
                )
            })
            .when(self.env_menu_open, |d| {
                d.child(
                    div()
                        .id("env-menu-backdrop")
                        .absolute()
                        .top_0()
                        .left_0()
                        .size_full()
                        .occlude()
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.env_menu_open = false;
                            cx.notify();
                        })),
                )
                .child(self.render_env_menu(viewport_h, cx))
            })
            .when_some(self.render_survey_card(cx), |d, card| d.child(card))
            .when_some(crate::agent::markdown::preview(), |d, image| {
                d.child(crate::agent::markdown::render_preview(image, window, cx))
            })
    }
}

// ---------------------------------------------------------------------------
// Pieces

fn plural(n: usize, word: &str) -> String {
    if n == 1 {
        format!("1 {word}")
    } else {
        format!("{n} {word}s")
    }
}

impl Shell {
    /// The Notes tab: the active project's scratch list.
    fn render_notes(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let notes = &self.project().notes;
        let remote = self.project().is_remote();
        let mut list = div()
            .id("notes-list")
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .p(crate::ui::sp(16.))
            .gap(crate::ui::sp(6.));
        if remote {
            list = list.child(ui::empty_state(
                "note01",
                "No notes here",
                "This project is only on your servers, and Arakun does not save it. Add notes in a project on this machine.",
                None,
            ));
        } else if notes.is_empty() {
            list = list.child(ui::empty_state(
                "note01",
                "No notes yet",
                "Ideas, links, things to check later. Type one above and press Enter.",
                None,
            ));
        }
        for (ni, note) in notes.iter().enumerate() {
            let group: SharedString = format!("note-{ni}").into();
            list = list.child(
                div()
                    .group(group.clone())
                    .flex()
                    .gap(crate::ui::sp(8.))
                    .px(crate::ui::sp(10.))
                    .py(crate::ui::sp(8.))
                    .rounded(crate::ui::rad(8.))
                    .bg(theme::fill_raised())
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_size(px(ui::TEXT_MD))
                            .line_height(px(19.))
                            .child(note.clone()),
                    )
                    .child(
                        div()
                            .id(("note-del", ni))
                            .flex_none()
                            .invisible()
                            .group_hover(group, |s| s.visible())
                            .cursor_pointer()
                            .child(icon("cancel01", 12.).text_color(theme::text_subtle()))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                let project = &mut this.projects[this.active_project];
                                if ni < project.notes.len() {
                                    project.notes.remove(ni);
                                    this.save(cx);
                                }
                                cx.notify();
                            })),
                    ),
            );
        }
        div()
            .flex()
            .flex_col()
            .size_full()
            .child(
                div()
                    .flex()
                    .flex_none()
                    .items_center()
                    .gap(crate::ui::sp(8.))
                    .h(px(36.))
                    .px(crate::ui::sp(14.))
                    .border_b_1()
                    .border_color(theme::stroke())
                    .text_size(px(ui::TEXT_SM))
                    .text_color(theme::text_subtle())
                    .child(icon("file01", 14.).text_color(theme::text_subtle()))
                    .child("Notes")
                    .child(div().flex_1())
                    .child(
                        div()
                            .text_color(theme::text_faint())
                            .child(format!("{}", self.project().name)),
                    ),
            )
            .when(!remote, |d| {
                d.child(
                    div()
                        .flex_none()
                        .p(crate::ui::sp(10.))
                        .border_b_1()
                        .border_color(theme::stroke())
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .h(px(34.))
                                .px(crate::ui::sp(10.))
                                .rounded(crate::ui::rad(8.))
                                .border_1()
                                .border_color(theme::border())
                                .bg(theme::background())
                                .text_size(px(ui::TEXT_MD))
                                .child(self.notes_input.clone()),
                        ),
                )
            })
            .child(list)
    }
}

/// A composer chip: flat text that lights up on hover, with an optional dim
/// second part ("Opus 5.5 High") and a caret when it opens a menu.
fn chip(
    id: &'static str,
    leading: Option<AnyElement>,
    label: String,
    dim: Option<String>,
    enabled: bool,
) -> gpui::Stateful<gpui::Div> {
    div()
        .id(id)
        .flex()
        .min_w_0()
        .max_w(px(260.))
        .items_center()
        .gap(crate::ui::sp(5.))
        .h(px(ui::CONTROL_H))
        .px(crate::ui::sp(7.))
        .rounded(crate::ui::rad(ui::RADIUS))
        .text_size(px(ui::TEXT_SM))
        .text_color(theme::text_muted())
        .when(enabled, |d| {
            d.cursor_pointer()
                .hover(|s| s.bg(theme::selection_hover()).text_color(theme::content()))
        })
        .when_some(leading, |d, l| {
            d.child(div().flex_none().flex().items_center().child(l))
        })
        .child(div().flex_none().child(label))
        .when_some(dim, |d, dim| {
            d.child(
                div()
                    .min_w_0()
                    .truncate()
                    .text_color(theme::text_subtle())
                    .child(dim),
            )
        })
        .when(enabled, |d| {
            d.child(
                icon("arrow-down01", 11.)
                    .flex_none()
                    .text_color(theme::text_faint()),
            )
        })
}

/// The server environment of `project` a new one is made from: the open one,
/// else another on its server, else any. A folder its server no longer
/// offers (`retired`) makes nothing.
fn server_source(
    project: &crate::model::Project,
    retired: impl Fn(&std::path::Path) -> bool,
) -> Option<usize> {
    let envs = &project.environments;
    let usable = |e: &crate::model::Environment| e.remote.is_some() && !retired(&e.path);
    let open = envs.get(project.active_env).and_then(|e| e.remote.as_ref());
    if envs.get(project.active_env).is_some_and(usable) {
        return Some(project.active_env);
    }
    open.and_then(|o| {
        envs.iter()
            .position(|e| usable(e) && e.remote.as_ref().is_some_and(|r| r.server == o.server))
    })
    .or_else(|| envs.iter().position(usable))
}

/// Takes `paths` off every project's list and returns the projects that kept
/// rows, by their new index. A project left with no rows goes: it was opened
/// on a deleted worktree, and `env()` needs a row in every project.
fn drop_envs(
    projects: &mut Vec<Project>,
    active_project: &mut usize,
    paths: &[std::path::PathBuf],
) -> Vec<usize> {
    let mut touched = Vec::new();
    let mut emptied = Vec::new();
    for (pi, project) in projects.iter_mut().enumerate() {
        let open = project
            .environments
            .get(project.active_env)
            .map(|e| e.path.clone());
        let before = project.environments.len();
        project.environments.retain(|e| !paths.contains(&e.path));
        if project.environments.len() == before {
            continue;
        }
        if project.environments.is_empty() {
            emptied.push(pi);
            continue;
        }
        // The open row stays open; when it went, the one at its place.
        project.active_env = open
            .and_then(|o| project.environments.iter().position(|e| e.path == o))
            .unwrap_or(project.active_env)
            .min(project.environments.len() - 1);
        touched.push(pi);
    }
    for &pi in emptied.iter().rev() {
        projects.remove(pi);
        if *active_project >= pi && *active_project > 0 {
            *active_project -= 1;
        }
        for t in &mut touched {
            if *t > pi {
                *t -= 1;
            }
        }
    }
    *active_project = (*active_project).min(projects.len().saturating_sub(1));
    touched
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_crash_notice_when_nothing_can_be_sent() {
        assert!(crash_notice_wanted(false, false));
        assert!(!crash_notice_wanted(true, false));
        assert!(!crash_notice_wanted(false, true));
    }

    /// The dock's sizes before corners and spacing became settings. At the
    /// default scales it must look exactly as it did.
    #[test]
    fn dock_keeps_its_default_sizes() {
        assert_eq!(ui::DOCK_BAR_H, 32.);
        assert_eq!(
            (DOCK_TAB_W, ui::DOCK_TAB_H, DOCK_TAB_CLOSE_TOP),
            (224., 24., 2.)
        );
        // Chips and icon buttons as tall as the tabs, framed evenly.
        assert_eq!(ui::ICON_BUTTON, ui::DOCK_TAB_H);
        assert_eq!(ui::DOCK_INSET, 4.);
        // Tab padding, gap and corner; chip padding and corner; body padding.
        for n in [8., 28., 6., 2., 12., 10.] {
            assert_eq!(ui::sp(n), px(n));
        }
        for r in [4., ui::RADIUS] {
            assert_eq!(ui::rad(r), px(r));
        }
        assert_eq!(ui::RADIUS, 8.);
    }

    /// The window's edges and the room between its cards are one value.
    #[test]
    fn window_gap_is_one_value() {
        assert_eq!(gap_px(), px(8.));
        assert_eq!(gap_px(), px(crate::ui::window_gap()));
    }

    #[test]
    fn side_cards_give_way_on_a_small_window() {
        // Room enough: the dragged widths stay.
        assert_eq!(fit_sides(500., [(200., 160.), (260., 200.)]), [200., 260.]);
        // 40 short of 100 slack: each keeps 60% of what it has above its minimum.
        assert_eq!(fit_sides(420., [(200., 160.), (260., 200.)]), [184., 236.]);
        // Never below a minimum, and a hidden card stays hidden.
        assert_eq!(
            fit_sides(100., [(200., 160.), (0., 0.), (300., 260.)]),
            [160., 0., 260.]
        );
        // A card already narrower than its minimum (icons) keeps its width.
        assert_eq!(fit_sides(0., [(52., 52.)]), [52.]);
    }

    #[test]
    fn a_narrow_window_leaves_out_the_panel() {
        let (rail, panel) = (168., Some(208.));
        // 1366x768 at 125%: both fit at their narrowest.
        let room = 1093. - 16. - MAIN_FLOOR_W;
        assert!(!crowd_out(room, rail, panel, false));
        // Too narrow: the panel goes, and the rail stays.
        assert!(crowd_out(300., rail, panel, false));
        // A closed panel is never left out.
        assert!(!crowd_out(300., rail, None, false));
        // Customize in the panel's place stays, even with no room at all.
        assert!(!crowd_out(100., rail, panel, true));
    }

    #[test]
    fn a_server_environment_is_made_from_a_folder_still_offered() {
        use crate::model::{Environment, Remote};
        let env = |server: &str, place: &str| {
            let mut e = Environment::new(Remote::env_path(server, place), None, false);
            e.remote = Some(Remote {
                server: server.into(),
                machine: server.into(),
                place: place.into(),
            });
            e
        };
        let mut project = crate::state::new_project(std::path::Path::new("remote:app"));
        project.environments = vec![
            env("s2", "/srv/b"),
            env("s1", "/srv/old"),
            env("s1", "/srv/app"),
        ];
        project.active_env = 1;
        let old = Remote::env_path("s1", "/srv/old");
        // The open folder, while it is offered.
        assert_eq!(server_source(&project, |_| false), Some(1));
        // Retired: another of its server's, not another server's.
        assert_eq!(server_source(&project, |p| p == old), Some(2));
        // None left there: any server's.
        assert_eq!(
            server_source(&project, |p| p != project.environments[0].path),
            Some(0)
        );
        assert_eq!(server_source(&project, |_| true), None);
    }

    #[test]
    fn hunk_lines() {
        assert_eq!(hunk_start("@@ -10,4 +12,6 @@ fn main() {"), Some(12));
        assert_eq!(hunk_start("@@ -1 +1 @@"), Some(1));
        assert_eq!(hunk_start("+@@ not a hunk"), None);
    }

    #[test]
    fn deleting_a_worktree_drops_a_project_opened_on_it() {
        use std::path::PathBuf;
        let repo = PathBuf::from("/r/repo");
        let tree = PathBuf::from("/r/tree");
        let other = PathBuf::from("/r/other");
        let mut parent = crate::state::new_project(&repo);
        parent
            .environments
            .push(Environment::new(tree.clone(), None, false));
        parent.active_env = 1;
        // The worktree opened as its own project before its parent listed it.
        let mut projects = vec![
            crate::state::new_project(&tree),
            parent,
            crate::state::new_project(&other),
        ];
        let mut active = 2;
        let touched = drop_envs(&mut projects, &mut active, &[tree]);
        assert_eq!(projects.len(), 2);
        assert!(projects.iter().all(|p| !p.environments.is_empty()));
        assert_eq!(projects[0].path, repo);
        assert_eq!(projects[0].active_env, 0);
        assert_eq!(touched, vec![0]);
        // The open project stays open at its new index.
        assert_eq!(active, 1);
        assert_eq!(projects[active].path, other);
    }

    #[test]
    fn tab_stays_in_what_types_it() {
        let ctx = |s: &str| gpui::KeyContext::parse(s).unwrap();
        assert!(!types_tab(&[ctx("Shell")]));
        assert!(!types_tab(&[ctx("Shell"), ctx("ThreadView")]));
        assert!(types_tab(&[ctx("Shell"), ctx("Terminal")]));
        assert!(types_tab(&[ctx("Shell"), ctx("Browser")]));
        assert!(types_tab(&[ctx("Shell"), ctx("Palette"), ctx("TextInput")]));
        assert!(in_modal(&[ctx("Shell"), ctx(ui::MODAL_CONTEXT)]));
        assert!(!in_modal(&[ctx("Shell"), ctx("ThreadView")]));
    }

    /// `shell` sits at the top: it shows every other module, and none of them
    /// reaches back into it. A path back makes a cycle, which keeps any of
    /// them from moving into a crate of its own (#614). Put what they share
    /// in that module, `ui` or a small module of its own instead.
    #[test]
    fn nothing_outside_the_shell_depends_on_it() {
        fn walk(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
            for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
                let p = entry.path();
                if p.is_dir() {
                    walk(&p, out);
                } else if p.extension().is_some_and(|x| x == "rs") {
                    out.push(p);
                }
            }
        }
        let src = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut files = Vec::new();
        walk(&src, &mut files);
        assert!(
            files.len() > 50,
            "found only {} files in {}",
            files.len(),
            src.display()
        );
        let mut found = Vec::new();
        for file in files {
            let rel = file.strip_prefix(&src).unwrap();
            if rel == std::path::Path::new("shell.rs") || rel.starts_with("shell") {
                continue;
            }
            let text = std::fs::read_to_string(&file).unwrap();
            for (n, line) in text.lines().enumerate() {
                if line.contains("crate::shell") {
                    found.push(format!("{}:{}: {}", rel.display(), n + 1, line.trim()));
                }
            }
        }
        assert!(
            found.is_empty(),
            "these reach into `shell`:\n{}",
            found.join("\n")
        );
    }
}
