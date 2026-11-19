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
mod automations;
mod branches;
mod browsers;
mod checks;
mod cloudflare;
mod commands;
mod composer;
mod dock;
mod documents;
mod drag;
mod extensions;
mod git_actions;
mod github;
mod headless;
mod home;
mod inbox;
mod island;
mod limits;
mod menus;
mod naming;
mod notes;
mod onboarding;
pub(crate) mod pages;
mod palette;
mod panel;
mod panes;
mod paste;
mod phone;
mod posthog;
mod rail;
mod rail_blocks;
mod rail_pick;
mod runs;
mod servers;
mod session_actions;
mod sessions;
mod settle;
mod sidebar_builder;
#[cfg(feature = "coderlabs")]
mod site;
mod subagents;
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
use crate::attach_picker::{AttachEvent, AttachPicker};
use crate::branches_view::BranchesView;
use crate::ci::{self, Ci};
use crate::cloudflare_view::CloudflareView;
#[cfg(feature = "coderlabs")]
use crate::coderspace::CoderSpace;
use crate::dialogs::{CreateEnvDialog, DeleteEnvDialog, DialogEvent};
use crate::explorer;
use crate::extensions_view::ExtensionsView;
use crate::gallery_view::GalleryView;
use crate::git;
use crate::github::GitHub;
use crate::github_view::GitHubView;
use crate::highlight;
use crate::input::{InputEvent, TextInput};
use crate::memory_view::{MemoryEvent, MemoryView};
use crate::model::{
    self, Agent, Document, Environment, Project, Status, Tab, TabKind, TermTab, pretty_path,
};
use crate::picker::{FolderPicker, PickerEvent};
use crate::posthog_view::PostHogView;
use crate::runs_view::RunsView;
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
use crate::usage_view::UsageView;
use composer::Completion;

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
        ToggleDance,
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
        FindInChat,
        ZoomIn,
        ZoomOut,
        ZoomReset,
        CustomizeSidebar
    ]
);

#[derive(Clone, Copy, PartialEq, Eq)]
enum PanelTab {
    Sessions,
    Explorer,
    Changes,
}

enum Modal {
    Picker(Entity<FolderPicker>),
    Attach(Entity<AttachPicker>),
    CreateEnv(Entity<CreateEnvDialog>),
    DeleteEnv(Entity<DeleteEnvDialog>),
    Command(Entity<crate::dialogs::CommandDialog>),
    Account(Entity<AccountDialog>),
    Commit(Entity<crate::git_dialogs::CommitDialog>),
    GitConfirm(Entity<crate::git_dialogs::ConfirmDialog>),
    Palette(Entity<crate::palette::Palette>),
    Settings(Entity<SettingsDialog>),
    Automations(Entity<crate::automations_view::AutomationsDialog>),
    /// Rename, remove or import sessions (`session_actions`).
    Session(gpui::AnyView),
    /// The first-launch question about crash reports and usage counts.
    Telemetry(Entity<crate::telemetry_dialog::TelemetryDialog>),
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
    Choice(Agent, crate::agent::Setting, String, bool),
    /// The reasoning row of the model menu; ← and → step through its levels.
    Effort(Agent),
    /// Fast mode, with whether it is on now.
    Fast(Agent, bool),
    /// Back to the agent's default model, reasoning and fast mode.
    Reset(Agent),
    /// Switches the composer to the agent, with the settings it remembers.
    Agent(Agent),
    /// Shows or hides a provider in the rail's usage card.
    Rail(crate::agent::AgentKind),
    OpenUsage,
}

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
    active_project: usize,
    panel_tab: PanelTab,
    dock_open: bool,
    dock_height: f32,
    /// Set while the dock's top edge is being dragged.
    dock_drag: bool,
    /// Set while a side card's gap handle is being dragged.
    side_drag: Option<SideDrag>,
    env_menu_open: bool,
    /// Environments made with one click whose worktree is not there yet.
    pending_envs: Vec<PendingEnv>,
    /// Where the environment menu was opened from below (the composer); `None`
    /// anchors it under the panel header.
    env_menu_at: Option<gpui::Point<Pixels>>,
    /// The composer's text as last painted, for clicks, Up/Down and the IME.
    composer_layout: Rc<RefCell<Option<TextLayout>>>,
    composer_scroll: ScrollHandle,
    tab_scroll: ScrollHandle,
    /// The tab last scrolled into view in the title bar, so a new active tab is too.
    tab_revealed: Cell<Option<u64>>,
    /// A check is queued for keyboard focus that went nowhere (see `render`).
    refocus_queued: bool,
    /// Scroll the composer to its caret on the next paint; set by edits and
    /// caret moves, not by the wheel.
    composer_reveal: Rc<Cell<bool>>,
    /// A mouse selection is being dragged in the composer.
    composer_selecting: bool,
    /// Where Up/Down aim, so the caret keeps its column across shorter lines.
    composer_goal_x: Option<Pixels>,
    prompt_history: crate::history::History,
    /// The history entry Up/Down brought into the composer.
    history_at: Option<usize>,
    /// Saves the state shortly after the draft stops changing.
    draft_save: Option<Task<()>>,
    /// The composer's caret, shared with the canvas that paints it.
    caret: Rc<RefCell<crate::caret::Caret>>,
    _caret_focus: Subscription,
    _composer_blur: Subscription,
    modal: Option<(Modal, Subscription)>,
    /// Refreshes the open palette when GitHub brings in more.
    palette_watch: Option<Subscription>,
    /// Each project's `owner/name` on GitHub (None when it has none), by
    /// directory. Read in the background: finding it runs `git`, and the
    /// palette asks for it on every GitHub update.
    repo_slugs: std::collections::HashMap<std::path::PathBuf, Option<String>>,
    /// The background read of `repo_slugs`; a new one replaces it.
    repo_slugs_read: Option<Task<()>>,
    menu: Option<ContextMenu>,
    /// Rows of the sidebar picked with Ctrl or Shift+click (`rail_pick`), by path.
    rail_pick: Vec<std::path::PathBuf>,
    /// The row Shift+click picks from.
    rail_pick_anchor: Option<std::path::PathBuf>,
    /// Polls `git status` of the visible environment while the window is active.
    poll: Option<Task<()>>,
    /// The last background `git fetch`, so the network is asked rarely.
    last_fetch: Option<Instant>,
    /// Diff tabs whose file lost its changes; closed from `render` (needs a window).
    stale_diffs: Vec<(std::path::PathBuf, String)>,
    /// Open `@`/`/` completion of the composer.
    completion: Option<Completion>,
    /// `git ls-files` of an environment, cached briefly for `@` completion.
    file_index: Option<(std::path::PathBuf, Instant, Arc<Vec<String>>)>,
    file_index_loading: bool,
    /// The ASCII face over the start and welcome screens (redraws the shell
    /// while its intro fades the screen in) and the small raccoon atop the
    /// rail, which steps aside while a raccoon shows in the main view.
    home_face: Entity<crate::ascii_face::AsciiFace>,
    _home_face: Subscription,
    rail_mascot: Entity<crate::mascot::Mascot>,
    explorer_scroll: UniformListScrollHandle,
    focus: FocusHandle,
    composer_focus: FocusHandle,
    _activation: Subscription,
    /// Where the window is, saved with the state so it reopens there.
    window_state: Option<state::WindowState>,
    _bounds: Subscription,
    _quit: [Subscription; 2],
    /// `arakun restart` asked to quit: save for the next arakun and launch it.
    restarting: bool,
    /// The pages last marked visible, so tab switches can tell when to update CEF.
    shown_browsers: Vec<gpui::EntityId>,
    /// Answers `arakun <command>` from the control socket.
    _control: Option<Task<()>>,
    /// Runs scheduled automations while the window is open.
    _scheduler: Option<Task<()>>,
    /// Merges seen and settled (`settle`).
    settle: settle::Watch,
    /// Set while the divider between split panes is being dragged.
    split_drag: bool,
    /// Bounds of the split panes as last painted, for dragging the divider.
    split_bounds: Rc<Cell<Bounds<Pixels>>>,
    browsers_dirty: bool,
    account: Entity<Account>,
    _account: Subscription,
    rail_open: bool,
    panel_open: bool,
    /// What the rail shows, in the user's order.
    sidebar: crate::sidebar::Sidebar,
    /// The sidebar builder tab's selection, fields and undo.
    builder: sidebar_builder::Builder,
    /// Dragged widths of the side cards; state::RAIL_W / PANEL_W until dragged.
    rail_w: f32,
    panel_w: f32,
    /// The widths the rail, workspace panel and subagents column are drawn at
    /// this frame: the dragged ones, narrowed when the window is too small to
    /// leave the work in the middle `MAIN_MIN_W` (see `fit_sides`).
    side_fit: Cell<[f32; 3]>,
    /// The workspace panel and the subagents column, open but left out this
    /// frame because the window is too narrow for them (see `crowd_out`).
    side_crowded: Cell<[bool; 2]>,
    /// Which of the two the user asked for last: on a narrow window it stays
    /// and the other gives way.
    side_prefer: SideCard,
    /// The Subagents panel, between the chat and the Workspace panel.
    subagents_open: bool,
    subagents_w: f32,
    /// The user closed it, so a new subagent doesn't open it again until restart.
    subagents_dismissed: bool,
    /// The subagent whose transcript the Subagents panel shows.
    subagent_view: Option<subagents::SubagentView>,
    /// Redraws the Subagents panel each second while a subagent in it runs.
    subagent_tick: Option<Task<()>>,
    arcade: bool,
    /// What the person pinned from the palette (`pins.json`).
    pins: crate::palette::pins::Pins,
    /// Automations made from a sentence in the palette (`rules.json`).
    rules: crate::palette::rules::Rules,
    session_query: Entity<TextInput>,
    session_filter: Option<Agent>,
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
    task_start: Option<(String, Agent)>,
    #[cfg(feature = "coderlabs")]
    _tasks: [Subscription; 3],
    ci: Entity<Ci>,
    runs_view: Entity<RunsView>,
    memory_view: Entity<MemoryView>,
    _memory_view: Subscription,
    cloudflare_view: Entity<CloudflareView>,
    _cloudflare_view: Subscription,
    #[cfg(feature = "coderlabs")]
    site_view: Entity<SiteView>,
    #[cfg(feature = "coderlabs")]
    _site_view: Subscription,
    #[cfg(feature = "team")]
    team_view: Entity<TeamView>,
    #[cfg(feature = "team")]
    _team_view: Subscription,
    posthog_view: Entity<PostHogView>,
    _posthog_view: Subscription,
    branches_view: Entity<BranchesView>,
    github: Entity<GitHub>,
    github_view: Entity<GitHubView>,
    pilot: Entity<crate::github::pilot::Pilot>,
    _github: [Subscription; 5],
    extensions_view: Entity<ExtensionsView>,
    _extensions_view: Subscription,
    _branches_view: Subscription,
    /// The phone's server and what each phone watches, while the phone is on.
    phone: Option<phone::PhoneLink>,
    phone_view: Entity<crate::phone_view::PhoneView>,
    _phone_view: Subscription,
    /// The connections to the machines running `arakun server`, once the
    /// Servers tab was opened.
    servers: Option<servers::ServersLink>,
    servers_view: Entity<crate::servers_view::ServersView>,
    _servers_view: Subscription,
    usage_view: Entity<UsageView>,
    gallery_view: Entity<GalleryView>,
    iso_view: Entity<crate::iso_view::IsoView>,
    #[cfg(feature = "coderlabs")]
    canvas_view: Entity<crate::canvas_view::CanvasView>,
    _gallery_view: Subscription,
    _usage_view: Subscription,
    /// (project root, branch) → the fix session whose push CI is judging.
    fix_loops: std::collections::HashMap<(std::path::PathBuf, String), runs::FixLoop>,
    /// The project check's state per session id.
    checks: std::collections::HashMap<String, checks::CheckLoop>,
    /// Large pastes waiting in each environment's composer, sent with the draft.
    pastes: std::collections::HashMap<std::path::PathBuf, Vec<String>>,
    /// Annotations and review comments waiting in each environment's composer.
    notes: std::collections::HashMap<std::path::PathBuf, Vec<crate::agent::notes::ChatNote>>,
    review_draft: Option<notes::ReviewDraft>,
    /// The session card being dragged.
    lifted: Option<SharedString>,
    /// The tab being dragged onto a pane, while a drag is on.
    lifted_tab: Option<u64>,
    slide: Option<drag::SessionSlide>,
    drop_menu: Option<drag::DropMenu>,
    /// Other sessions' conversations waiting in each environment's composer.
    attached: std::collections::HashMap<std::path::PathBuf, Vec<drag::AttachedContext>>,
    saved_prompts: Vec<crate::prompts::SavedPrompt>,
    /// A one-line note under the composer (a prompt was saved, ...), until the next key.
    composer_flash: Option<SharedString>,
    /// The agent whose models the model menu shows; `None` is the composer's.
    menu_agent: Option<Agent>,
    /// The row the arrow keys are on, in `menu_keys` order.
    menu_cursor: Option<usize>,
    /// The model list's scroll, kept on the row the keys are on.
    menu_scroll: ScrollHandle,
    /// The alert level (`limits::level`) each plan window last notified at.
    limit_alerts: std::collections::HashMap<String, u8>,
    /// The first limit check only notes the levels; later ones notify.
    limits_seen: bool,
    _runs: [Subscription; 3],
    /// The Dynamic Island, while it is on and the compositor has layer-shell.
    island: Option<island::IslandLink>,
    #[cfg(feature = "team")]
    team: Option<team::TeamLink>,
    /// Every notice shown, for the Inbox. A cell: notices go out from `&self`.
    notices: RefCell<crate::notices::History>,
    /// The Inbox was open at the last frame: when it closes, what it showed counts as seen.
    inbox_open: bool,
    /// The GitHub tab shown with no project added: the queue reads GitHub,
    /// not a project, so it doesn't need one. It takes the welcome's place.
    github_alone: bool,
    /// The Servers tab shows in the welcome's place: no project was added.
    servers_alone: bool,
    /// `History::arrived` when the Inbox last drew: what it showed.
    inbox_shown: Cell<u64>,
    /// "Clear" was clicked once; the second click empties the history.
    clear_armed: bool,
    /// The setup, while it shows in the window's place (`shell::onboarding`).
    onboarding: Option<onboarding::Onboarding>,
}

struct InboxItem {
    project: usize,
    env: usize,
    id: SharedString,
    status: Status,
    title: SharedString,
    place: SharedString,
    updated: u64,
}

const DOCK_MIN_H: f32 = 120.;
/// What the dock leaves above it for the work, however tall it was dragged.
const DOCK_ROOM_ABOVE: f32 = 160.;

/// The tallest the dock can be in a window `viewport_h` tall: its bottom edge
/// is one gap above the window's.
fn dock_max_h(viewport_h: f32) -> f32 {
    (viewport_h - gap() - DOCK_ROOM_ABOVE).max(DOCK_MIN_H)
}
/// How narrow and wide the side cards can be dragged.
const RAIL_W_RANGE: std::ops::Range<f32> = 160. ..320.;
const PANEL_W_RANGE: std::ops::Range<f32> = 200. ..480.;
const SUBAGENTS_W_RANGE: std::ops::Range<f32> = 260. ..640.;
/// How narrow the work between the side cards may get before they give way.
const MAIN_MIN_W: f32 = 420.;
/// Below this, even at their narrowest the side cards leave too little for
/// the work, and the workspace panel or subagents column is left out. A
/// 1366x768 laptop at 125% scale, or a window snapped to half a screen, is
/// that narrow.
const MAIN_FLOOR_W: f32 = 320.;

/// The workspace panel or the subagents column.
#[derive(Clone, Copy, PartialEq, Debug)]
enum SideCard {
    Panel,
    Subagents,
}

/// Which of the open panel and subagents column to leave out so the cards'
/// narrowest widths fit in `room`. The one not preferred goes first. The rail
/// always stays: it holds the projects and the way back to everything else.
/// Each card is its minimum width plus its handle; a closed one is `None`.
fn crowd_out(
    room: f32,
    rail: f32,
    panel: Option<f32>,
    subagents: Option<f32>,
    prefer: SideCard,
) -> [bool; 2] {
    let mut out = [false; 2];
    let mut need = rail + panel.unwrap_or(0.) + subagents.unwrap_or(0.);
    let order = match prefer {
        SideCard::Panel => [(1, subagents), (0, panel)],
        SideCard::Subagents => [(0, panel), (1, subagents)],
    };
    for (i, w) in order {
        if need <= room {
            break;
        }
        if let Some(w) = w {
            out[i] = true;
            need -= w;
        }
    }
    out
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
    Subagents,
}
const POLL: Duration = Duration::from_secs(3);
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
            }
            #[cfg(feature = "coderlabs")]
            this.coderspace
                .update(cx, |cs, cx| cs.set_window_active(active, cx));
            this.ci
                .update(cx, |ci, cx| ci.set_window_active(active, cx));
            this.github
                .update(cx, |g, cx| g.set_window_active(active, cx));
        });
        let focus = cx.focus_handle();
        let composer_focus = cx.focus_handle();
        let caret_focus = cx.on_focus(&composer_focus, window, |this, window, cx| {
            this.claim_composer_tab(window, cx)
        });
        let composer_blur = cx.on_blur(&composer_focus, window, |this, window, cx| {
            this.spy_from_composer(window, cx)
        });
        let (projects, active_project, layout, strip) = state::restore(state::load());
        theme::set_diff_palette(layout.diff_palette);
        theme::set_glass(layout.glass);
        theme::set_grays(layout.grays);
        crate::palettes::apply_saved();
        account_view::set_rail_shows(crate::agent::AgentKind::Claude, layout.rail_usage[0]);
        account_view::set_rail_shows(crate::agent::AgentKind::Droid, layout.rail_usage[1]);
        account_view::set_rail_shows(crate::agent::AgentKind::OpenCode, layout.rail_usage[2]);
        account_view::set_rail_shows(crate::agent::AgentKind::Devin, layout.rail_usage[3]);
        crate::ui::set_type_scale(layout.type_scale);
        if layout.ui_zoom != crate::ui_zoom::pct() {
            crate::ui_zoom::set_pct(layout.ui_zoom);
            window.bounds_changed(cx);
        }
        if layout.glass {
            window.set_background_appearance(theme::window_background());
        }
        let account = cx.new(Account::new);
        let account_sub = cx.observe(&account, |this, _, cx| {
            this.check_limit_alerts(cx);
            this.phone_usage_changed(cx);
        });
        let session_query = cx.new(|cx| TextInput::new("Search conversations...", cx));
        let session_query_sub =
            cx.subscribe(&session_query, |_, input, ev: &InputEvent, cx| match ev {
                InputEvent::Changed => cx.notify(),
                InputEvent::Cancel => input.update(cx, |i, cx| i.set_text("", cx)),
                _ => {}
            });
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
        let runs_view = cx.new(|cx| RunsView::new(ci.clone(), cx));
        let runs_subs = [
            cx.observe(&ci, |_, _, cx| cx.notify()),
            cx.subscribe_in(&runs_view, window, |this, _, ev, window, cx| {
                this.on_runs_event(ev, window, cx)
            }),
            cx.subscribe_in(&ci, window, |this, _, ev, window, cx| {
                this.on_ci_event(ev, window, cx)
            }),
        ];
        let memory_view = cx.new(MemoryView::new);
        let memory_sub = cx.subscribe_in(
            &memory_view,
            window,
            |this, _, ev: &MemoryEvent, window, cx| match ev {
                MemoryEvent::Edit(path) => this.edit_in_terminal(path, None, window, cx),
            },
        );
        let usage_view = cx.new(UsageView::new);
        let gallery_view = cx.new(GalleryView::new);
        let iso_view = cx.new(crate::iso_view::IsoView::new);
        #[cfg(feature = "coderlabs")]
        let canvas_view = cx.new(crate::canvas_view::CanvasView::new);
        let gallery_sub = cx.subscribe(
            &gallery_view,
            |this, _, _: &crate::gallery_view::GalleryChanged, cx| {
                this.save(cx);
                cx.refresh_windows();
            },
        );
        let usage_sub = cx.subscribe_in(
            &usage_view,
            window,
            |this, _, ev: &crate::usage_view::UsageEvent, window, cx| match ev {
                crate::usage_view::UsageEvent::OpenAccount => this.open_account(window, cx),
                crate::usage_view::UsageEvent::ClaudeSignIn => {
                    if let Some(cmd) = crate::agent::login_command(crate::agent::AgentKind::Claude)
                    {
                        this.spawn_terminal_command(Some(cmd), true, window, cx);
                    }
                }
                crate::usage_view::UsageEvent::PlanUpdated => {
                    this.check_limit_alerts(cx);
                    this.phone_usage_changed(cx);
                }
            },
        );
        let cloudflare_view = cx.new(CloudflareView::new);
        let cloudflare_sub =
            cx.subscribe_in(&cloudflare_view, window, |this, _, ev, window, cx| {
                this.on_cloudflare_event(ev, window, cx)
            });
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
        let posthog_view = cx.new(PostHogView::new);
        let posthog_sub = cx.subscribe_in(&posthog_view, window, |this, _, ev, window, cx| {
            this.on_posthog_event(ev, window, cx)
        });
        let branches_view = cx.new(BranchesView::new);
        let sidebar = crate::sidebar::Sidebar::load();
        let views = crate::sidebar::Extensions::load(&sidebar);
        let extensions_view = cx.new(|cx| ExtensionsView::new(views, cx));
        let extensions_sub =
            cx.subscribe_in(&extensions_view, window, |this, _, ev, window, cx| {
                this.on_extensions_event(ev, window, cx)
            });
        let branches_sub = cx.subscribe_in(&branches_view, window, |this, _, ev, window, cx| {
            this.on_branches_event(ev, window, cx)
        });
        let github = cx.new(GitHub::new);
        let pilot = cx.new(crate::github::pilot::Pilot::new);
        let github_view = cx.new(|cx| GitHubView::new(github.clone(), pilot.clone(), cx));
        let github_subs = [
            cx.observe(&pilot, |_, _, cx| cx.notify()),
            cx.subscribe_in(&pilot, window, |this, _, ev, window, cx| {
                this.on_pilot_event(ev, window, cx)
            }),
            // The rail badge counts what the model holds.
            cx.observe(&github, |_, _, cx| cx.notify()),
            cx.subscribe_in(&github_view, window, |this, _, ev, window, cx| {
                this.on_github_view_event(ev, window, cx)
            }),
            cx.subscribe_in(&github, window, |this, _, ev, window, cx| {
                this.on_github_event(ev, window, cx)
            }),
        ];
        let phone_view = cx.new(crate::phone_view::PhoneView::new);
        let phone_sub = cx.subscribe_in(&phone_view, window, |this, _, ev, window, cx| {
            this.on_phone_event(ev, window, cx)
        });
        let servers_view = cx.new(crate::servers_view::ServersView::new);
        let servers_sub = cx.subscribe_in(&servers_view, window, |this, _, ev, window, cx| {
            this.on_servers_event(ev, window, cx)
        });
        if projects.is_empty() {
            window.focus(&focus, cx);
        } else {
            window.focus(&composer_focus, cx);
        }
        crate::mascot::set_animated(layout.arcade);
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
            active_project,
            panel_tab: PanelTab::Sessions,
            dock_open: layout.dock_open,
            dock_height: layout.dock_height,
            dock_drag: false,
            side_drag: None,
            env_menu_open: false,
            pending_envs: Vec::new(),
            env_menu_at: None,
            composer_layout: Rc::default(),
            composer_scroll: ScrollHandle::new(),
            tab_scroll: ScrollHandle::new(),
            tab_revealed: Cell::new(None),
            refocus_queued: false,
            composer_reveal: Rc::default(),
            composer_selecting: false,
            composer_goal_x: None,
            prompt_history: crate::history::History::load(),
            history_at: None,
            draft_save: None,
            caret: Default::default(),
            _caret_focus: caret_focus,
            _composer_blur: composer_blur,
            modal: None,
            palette_watch: None,
            repo_slugs: Default::default(),
            repo_slugs_read: None,
            menu: None,
            rail_pick: Vec::new(),
            rail_pick_anchor: None,
            poll: None,
            last_fetch: None,
            stale_diffs: Vec::new(),
            completion: None,
            file_index: None,
            file_index_loading: false,
            home_face,
            _home_face: home_face_sub,
            rail_mascot: cx.new(|cx| crate::mascot::Mascot::new(24., cx)),
            explorer_scroll: UniformListScrollHandle::new(),
            focus,
            composer_focus,
            _activation: activation,
            window_state: layout.window,
            _bounds: cx
                .observe_window_bounds(window, |this, window, cx| this.track_window(window, cx)),
            shown_browsers: Vec::new(),
            _control: None,
            _scheduler: None,
            settle: settle::Watch::default(),
            split_drag: false,
            split_bounds: Rc::default(),
            browsers_dirty: false,
            account,
            _account: account_sub,
            rail_open: layout.rail_open,
            panel_open: layout.panel_open,
            sidebar,
            builder: Default::default(),
            rail_w: layout.rail_w,
            panel_w: layout.panel_w,
            side_fit: Cell::new([layout.rail_w, layout.panel_w, layout.subagents_w]),
            side_crowded: Cell::new([false; 2]),
            side_prefer: SideCard::Panel,
            subagents_open: layout.subagents_open,
            subagents_w: layout
                .subagents_w
                .clamp(SUBAGENTS_W_RANGE.start, SUBAGENTS_W_RANGE.end),
            subagents_dismissed: false,
            subagent_view: None,
            subagent_tick: None,
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
            runs_view,
            memory_view,
            _memory_view: memory_sub,
            cloudflare_view,
            _cloudflare_view: cloudflare_sub,
            #[cfg(feature = "coderlabs")]
            site_view,
            #[cfg(feature = "coderlabs")]
            _site_view: site_sub,
            #[cfg(feature = "team")]
            team_view,
            #[cfg(feature = "team")]
            _team_view: team_sub,
            posthog_view,
            _posthog_view: posthog_sub,
            branches_view,
            _branches_view: branches_sub,
            github,
            github_view,
            pilot,
            _github: github_subs,
            extensions_view,
            _extensions_view: extensions_sub,
            phone: None,
            phone_view,
            _phone_view: phone_sub,
            servers: None,
            servers_view,
            _servers_view: servers_sub,
            _usage_view: usage_sub,
            usage_view,
            gallery_view,
            _gallery_view: gallery_sub,
            iso_view,
            #[cfg(feature = "coderlabs")]
            canvas_view,
            fix_loops: std::collections::HashMap::new(),
            checks: std::collections::HashMap::new(),
            pastes: std::collections::HashMap::new(),
            notes: std::collections::HashMap::new(),
            review_draft: None,
            lifted: None,
            lifted_tab: None,
            slide: None,
            drop_menu: None,
            attached: std::collections::HashMap::new(),
            saved_prompts: crate::prompts::load(),
            composer_flash: None,
            menu_agent: None,
            menu_cursor: None,
            menu_scroll: ScrollHandle::new(),
            limit_alerts: std::collections::HashMap::new(),
            limits_seen: false,
            island: None,
            #[cfg(feature = "team")]
            team: None,
            notices: RefCell::new(crate::notices::load()),
            inbox_open: false,
            github_alone: false,
            onboarding: None,
            servers_alone: false,
            inbox_shown: Cell::new(0),
            clear_armed: false,
            restarting: false,
            _runs: runs_subs,
            _quit: [
                cx.on_app_quit(|this, cx| {
                    // A draft typed in the last moments may not be saved yet.
                    // Replaces any save still queued, so none can land over it.
                    state::queue(this.state_bytes());
                    if let Err(e) = state::write_queued() {
                        eprintln!("arakun: could not save state: {e}");
                    }
                    this.save_transcripts(cx);
                    // Notices are saved off the UI thread: let the last one land.
                    crate::notices::flush();
                    // Settings switched in the last moments.
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
        this.sync_poll(window, cx);
        this.ensure_terminal(window, cx);
        this.ensure_active_live(window, cx);
        this.resume_after_restart(window, cx);
        this.warm_droid_catalog(cx);
        this.listen_for_commands(window, cx);
        // The compositor's close (Super+W, Alt+F4) hides too when the setting is on.
        window.on_window_should_close(cx, |window, _| crate::background::should_close(window));
        this.watch_headless(window, cx);
        this.start_scheduler(window, cx);
        this.start_rule_ticker(window, cx);
        this.restore_phone(window, cx);
        this.restore_servers(window, cx);
        this.start_updates(window, cx);
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
        self.strip.split_other(&self.env().path)
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
        self.completion = None;
        self.refresh_visible(cx);
        if self.panel_tab == PanelTab::Explorer {
            self.ensure_explorer(cx);
        }
        self.env_followed = true;
    }

    // -----------------------------------------------------------------------
    // Persistence and git refresh

    fn save(&self, cx: &mut Context<Self>) {
        state::queue(self.state_bytes());
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

    fn state_bytes(&self) -> Vec<u8> {
        let layout = state::Layout {
            dock_open: self.dock_open,
            dock_height: self.dock_height,
            rail_open: self.rail_open,
            panel_open: self.panel_open,
            rail_w: self.rail_w,
            panel_w: self.panel_w,
            subagents_open: self.subagents_open,
            subagents_w: self.subagents_w,
            arcade: self.arcade,
            diff_palette: theme::diff_palette(),
            glass: theme::glass(),
            grays: theme::grays(),
            type_scale: crate::ui::type_scale(),
            rail_usage: [
                account_view::rail_shows(crate::agent::AgentKind::Claude),
                account_view::rail_shows(crate::agent::AgentKind::Droid),
                account_view::rail_shows(crate::agent::AgentKind::OpenCode),
                account_view::rail_shows(crate::agent::AgentKind::Devin),
            ],
            ui_zoom: crate::ui_zoom::pct(),
            window: self.window_state.clone(),
        };
        state::snapshot_with(&self.projects, &self.strip, self.active_project, layout)
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
        // A server's folder: its changes are on that machine.
        if crate::model::Remote::is_path(env_path) {
            return;
        }
        let path = env_path.to_path_buf();
        cx.spawn(async move |this, cx| {
            let p = path.clone();
            let (changes, sync) = cx
                .background_spawn(crate::guard::task(async move {
                    (git::status(&p), git::sync(&p))
                }))
                .await;
            let _ = this.update(cx, |this, cx| {
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
        if self.panel_tab == PanelTab::Explorer {
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
        self.poll = Some(cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(POLL).await;
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
        if self.last_fetch.is_some_and(|t| t.elapsed() < FETCH_EVERY) {
            return;
        }
        self.last_fetch = Some(Instant::now());
        let path = self.env().path.clone();
        cx.spawn(async move |this, cx| {
            let p = path.clone();
            let fetched = cx
                .background_spawn(crate::guard::task(async move { git::fetch(&p) }))
                .await;
            if fetched.is_ok() {
                let _ = this.update(cx, |this, cx| this.refresh_status(&path, cx));
            }
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
        self.menu = None;
        self.env_menu_open = false;
        let existing = self.projects.iter().map(|p| p.path.clone()).collect();
        let picker = cx.new(|cx| FolderPicker::new(existing, window, cx));
        let sub = cx.subscribe_in(
            &picker,
            window,
            |this, _, ev: &PickerEvent, window, cx| match ev {
                PickerEvent::Picked(path) => {
                    let path = path.clone();
                    this.close_modal(window, cx);
                    this.add_project(path, window, cx);
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
        let index = match self.projects.iter().position(|p| p.path == path) {
            Some(i) => i,
            None => {
                self.projects.push(state::new_project(&path));
                crate::telemetry::capture(crate::telemetry::events::PROJECT_ADDED, &[]);
                self.projects.len() - 1
            }
        };
        self.active_project = index;
        self.show_env_tab(false);
        self.note_active_tab(cx);
        self.refresh_project(index, cx);
        self.sync_ci_roots(cx);
        self.save(cx);
        window.focus(&self.composer_focus, cx);
        self.ensure_terminal(window, cx);
        self.sync_poll(window, cx);
        // A project is what the setup was for, however it was added.
        self.finish_onboarding(window, cx);
        cx.notify();
    }

    fn remove_project(&mut self, pi: usize, window: &mut Window, cx: &mut Context<Self>) {
        self.menu = None;
        if pi >= self.projects.len() {
            return;
        }
        self.projects.remove(pi);
        if self.active_project >= pi && self.active_project > 0 {
            self.active_project -= 1;
        }
        self.active_project = self
            .active_project
            .min(self.projects.len().saturating_sub(1));
        self.settle_strip(cx);
        self.unfloat_github(cx);
        // Its Servers tab went with it; a no-op while one is left elsewhere.
        self.servers_tab_closed(cx);
        self.sync_ci_roots(cx);
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
        let pick = machines
            .iter()
            .position(|m| m.server.as_ref().map(|(s, _)| s) == open.as_ref())
            .unwrap_or(0);
        let (repo, name) = (project.path.clone(), project.name.clone());
        let setup = project.setup.clone();
        let dialog = cx.new(|cx| {
            CreateEnvDialog::new(repo, name, setup, window, cx).with_machines(machines, pick)
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
                    this.servers_new_env_on(&server, &from, branch, auto_name, asked, cx);
                }
                _ => this.close_modal(window, cx),
            },
        );
        self.modal = Some((Modal::CreateEnv(dialog), sub));
        cx.notify();
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
        // In a server's environment, or a project only servers have: the
        // new one is made on that server.
        let on_server = project
            .environments
            .get(project.active_env)
            .is_some_and(|e| e.remote.is_some())
            || project.is_remote();
        if on_server {
            let from = server_source(project, |p| self.server_env_retired(p));
            let Some(from) = from else {
                self.notify(
                    crate::island::AlertKind::Warning,
                    "Could not make the environment",
                    "The server no longer offers this folder, nor another of this project.",
                    None,
                    cx,
                );
                return;
            };
            self.servers_new_env(pi, from, cx);
            return;
        }
        let (repo, project_name) = (project.path.clone(), project.name.clone());
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
                    let mut result = Err(anyhow::anyhow!("not tried"));
                    for _ in 0..2 {
                        let path = crate::model::worktree_path(&project_name, &name);
                        result = git::add_worktree(&repo, &path, &name, None, true)
                            .map(|()| (path, name.clone()));
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
                    Ok((path, branch)) => {
                        if let Some(pi) = this.projects.iter().position(|p| p.path == key) {
                            this.environment_created(pi, path.clone(), branch, true, window, cx);
                            if let Some(prompt) = prompt {
                                this.start_queued(&path, prompt, window, cx);
                            }
                        }
                    }
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
        window.focus(&self.composer_focus, cx);
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
            self.open_picker(window, cx);
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
            self.open_picker(window, cx);
            return;
        }
        self.show_view_tab("Memory", TabKind::Memory, |k| matches!(k, TabKind::Memory));
        self.note_active_tab(cx);
        let dir = self.env().path.clone();
        self.memory_view.update(cx, |v, cx| {
            v.set_dir(&dir, cx);
            v.focus(window, cx);
        });
        cx.notify();
    }

    /// Opens (or focuses) the Usage tab of the visible environment.
    fn open_usage_tab(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.has_projects() {
            self.open_picker(window, cx);
            return;
        }
        self.show_view_tab("Usage", TabKind::Usage, |k| matches!(k, TabKind::Usage));
        self.note_active_tab(cx);
        self.sync_usage_view(cx);
        self.usage_view.update(cx, |v, cx| v.focus(window, cx));
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
            self.open_picker(window, cx);
            return;
        }
        self.show_view_tab("Design system", TabKind::Gallery, |k| {
            matches!(k, TabKind::Gallery)
        });
        self.note_active_tab(cx);
        self.gallery_view.update(cx, |v, cx| v.focus(window, cx));
        cx.notify();
    }

    /// Opens (or focuses) the Isometric tab of the visible environment.
    fn open_iso_tab(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.has_projects() {
            self.open_picker(window, cx);
            return;
        }
        self.show_view_tab("Isometric", TabKind::Isometric, |k| {
            matches!(k, TabKind::Isometric)
        });
        self.note_active_tab(cx);
        let root = self.env().path.clone();
        self.iso_view.update(cx, |v, cx| {
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
        self.usage_view.update(cx, |v, cx| {
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
        let branch = (env.branch.as_ref() != "detached HEAD").then(|| env.branch.to_string());
        let dirty = env.changes.len();
        let dialog = cx.new(|cx| DeleteEnvDialog::new(repo, path, branch, dirty, window, cx));
        let sub = cx.subscribe_in(
            &dialog,
            window,
            move |this, _, ev: &DialogEvent, window, cx| match ev {
                DialogEvent::Deleted(path) => {
                    let path = path.clone();
                    this.close_modal(window, cx);
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
        //PLACEHOLDER window: &mut Window, cx: &mut Context<Self>) {
        if paths.is_empty() {
            return;
        }
        let mut touched = Vec::new();
        for (pi, project) in self.projects.iter_mut().enumerate() {
            let open = project
                .environments
                .get(project.active_env)
                .map(|e| e.path.clone());
            let before = project.environments.len();
            project.environments.retain(|e| !paths.contains(&e.path));
            if project.environments.len() == before {
                continue;
            }
            // The open row stays open; when it went, the one at its place.
            project.active_env = open
                .and_then(|o| project.environments.iter().position(|e| e.path == o))
                .unwrap_or(project.active_env)
                .min(project.environments.len().saturating_sub(1));
            touched.push(pi);
        }
        self.rail_pick.retain(|p| !paths.contains(p));
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

    /// In a arakun browser tab when there is an environment to hold it and the user
    /// keeps links in Arakun, else the desktop browser.
    fn open_url(&mut self, url: String, window: &mut Window, cx: &mut Context<Self>) {
        if self.has_projects() && crate::browser::links_in_arakun() {
            self.open_browser_tab(url, window, cx);
        } else {
            cx.open_url(&url);
        }
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

    fn open_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        // A second dialog would read the palettes before the open one's
        // pending save lands, then save over that edit.
        if matches!(self.modal, Some((Modal::Settings(_), _))) {
            return;
        }
        self.menu = None;
        self.env_menu_open = false;
        let arcade = self.arcade;
        let droid = self.account.read(cx).connected();
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
        let dialog = cx.new(|cx| {
            SettingsDialog::new(
                arcade,
                droid,
                #[cfg(feature = "coderlabs")]
                coderspace,
                window,
                cx,
            )
        });
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
                SettingsEvent::DiffPalette(p) => {
                    theme::set_diff_palette(*p);
                    this.save(cx);
                    cx.refresh_windows();
                }
                SettingsEvent::RailUsage(agent, on) => {
                    account_view::set_rail_shows(*agent, *on);
                    this.save(cx);
                    cx.notify();
                }
                SettingsEvent::Grays(g) => {
                    theme::set_grays(*g);
                    this.save(cx);
                    cx.refresh_windows();
                }
                SettingsEvent::TypeScale(t) => {
                    crate::ui::set_type_scale(*t);
                    this.save(cx);
                    cx.refresh_windows();
                }
                SettingsEvent::Glass(on) => {
                    theme::set_glass(*on);
                    window.set_background_appearance(theme::window_background());
                    this.save(cx);
                    cx.refresh_windows();
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
                SettingsEvent::RestartToUpdate => {
                    this.restarting = true;
                    cx.quit();
                }
                SettingsEvent::OpenAccount => {
                    this.close_modal(window, cx);
                    this.open_account(window, cx);
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
                SettingsEvent::OpenUsage => {
                    this.close_modal(window, cx);
                    this.open_usage_tab(window, cx);
                }
                SettingsEvent::OpenGallery => {
                    this.close_modal(window, cx);
                    this.open_gallery_tab(window, cx);
                }
                SettingsEvent::ClaudeSignIn => {
                    this.close_modal(window, cx);
                    this.sign_in(crate::agent::AgentKind::Claude, window, cx);
                }
                SettingsEvent::ClaudeSignOut => {
                    this.close_modal(window, cx);
                    this.sign_out(crate::agent::AgentKind::Claude, window, cx);
                }
                SettingsEvent::CodexSignIn => {
                    this.close_modal(window, cx);
                    this.sign_in(crate::agent::AgentKind::Codex, window, cx);
                }
                SettingsEvent::CodexSignOut => {
                    this.close_modal(window, cx);
                    this.sign_out(crate::agent::AgentKind::Codex, window, cx);
                }
                SettingsEvent::DevinSignIn => {
                    this.close_modal(window, cx);
                    this.sign_in(crate::agent::AgentKind::Devin, window, cx);
                }
                SettingsEvent::DevinSignOut => {
                    this.close_modal(window, cx);
                    this.sign_out(crate::agent::AgentKind::Devin, window, cx);
                }
                SettingsEvent::OpenCodeSignIn => {
                    this.close_modal(window, cx);
                    this.sign_in(crate::agent::AgentKind::OpenCode, window, cx);
                }
                SettingsEvent::OpenCodeGoSignIn => {
                    this.close_modal(window, cx);
                    let cmd = crate::agent::opencode_go_login_command();
                    let kind = crate::agent::AgentKind::OpenCode;
                    this.run_account_command(
                        "Sign in to OpenCode Go",
                        "sign-in",
                        kind,
                        cmd,
                        window,
                        cx,
                    );
                }
                SettingsEvent::OpenCodeSignOut => {
                    this.close_modal(window, cx);
                    this.sign_out(crate::agent::AgentKind::OpenCode, window, cx);
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
        // Left out for room, a click brings it back in the subagents' place.
        if self.panel_open && self.side_crowded.get()[0] {
            self.side_prefer = SideCard::Panel;
            cx.notify();
            return;
        }
        self.panel_open = !self.panel_open;
        if self.panel_open {
            self.side_prefer = SideCard::Panel;
        }
        self.save(cx);
        cx.notify();
    }

    /// Shows or hides the sidebar alone: Ctrl+B, its buttons and the palette
    /// all run this, so one name always does one thing.
    fn toggle_rail(&mut self, cx: &mut Context<Self>) {
        if self.sidebar.style.collapse == crate::sidebar::Collapse::Never {
            // Otherwise the key looks broken.
            self.composer_flash =
                Some("The sidebar always shows: Customize sidebar → When hidden.".into());
            cx.notify();
            return;
        }
        self.rail_open = !self.rail_open;
        self.save(cx);
        cx.notify();
    }

    /// Turns redacted mode on or off. Every window draws again at once, so
    /// nothing private stays on screen for a frame longer than it must.
    pub(super) fn toggle_redacted(&mut self, cx: &mut Context<Self>) {
        if let Err(e) = crate::redact::set(!crate::redact::on()) {
            eprintln!("arakun: redacted mode will not last past a restart: {e}");
        }
        cx.refresh_windows();
    }

    /// The rail holds "Add project", so it stays while there is no project.
    /// Narrowed to icons it still shows, and some rails never close.
    fn rail_shown(&self) -> bool {
        use crate::sidebar::Collapse;
        self.rail_open
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

    fn subagents_width(&self) -> f32 {
        self.side_fit.get()[2]
    }

    /// Narrows the side cards for this frame when the window can't hold them
    /// at their dragged widths and still leave `MAIN_MIN_W` for the work.
    /// The dragged widths stay, so widening the window brings them back.
    fn fit_side_cards(&self, window: &Window) {
        let has_projects = self.has_projects();
        let rail = self.rail_shown();
        let viewport_w = window.viewport_size().width.as_f32();
        let rail_card = match (rail, self.rail_narrow()) {
            (false, _) => (0., 0.),
            (true, true) => (rail_blocks::RAIL_ICONS_W, rail_blocks::RAIL_ICONS_W),
            (true, false) => (self.rail_w, RAIL_W_RANGE.start),
        };
        // Each card's narrowest, with the handle beside it.
        let crowded = crowd_out(
            viewport_w - 2. * gap() - MAIN_FLOOR_W,
            if rail { rail_card.1 + gap() } else { 0. },
            (has_projects && self.panel_open).then(|| PANEL_W_RANGE.start + gap()),
            (has_projects && self.subagents_open).then(|| SUBAGENTS_W_RANGE.start + gap()),
            self.side_prefer,
        );
        self.side_crowded.set(crowded);
        let panel = self.panel_shown();
        let subagents = self.subagents_shown();
        let shown = [rail, panel, subagents].iter().filter(|s| **s).count() as f32;
        // The window's padding at both sides, and one handle beside each card.
        let room = viewport_w - (2. + shown) * gap() - MAIN_MIN_W;
        let [r, p, s] = fit_sides(
            room,
            [
                rail_card,
                if panel {
                    (self.panel_w, PANEL_W_RANGE.start)
                } else {
                    (0., 0.)
                },
                if subagents {
                    (self.subagents_w, SUBAGENTS_W_RANGE.start)
                } else {
                    (0., 0.)
                },
            ],
        );
        self.side_fit.set([
            if rail { r } else { self.rail_w },
            if panel { p } else { self.panel_w },
            if subagents { s } else { self.subagents_w },
        ]);
    }

    /// The workspace panel is open and the window has room for it this frame.
    fn panel_shown(&self) -> bool {
        self.has_projects() && self.panel_open && !self.side_crowded.get()[0]
    }

    /// The subagents column is open and the window has room for it this frame.
    pub(super) fn subagents_shown(&self) -> bool {
        self.has_projects() && self.subagents_open && !self.side_crowded.get()[1]
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
        self.panel_tab = PanelTab::Changes;
        self.side_prefer = SideCard::Panel;
        if !self.panel_open {
            self.panel_open = true;
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

    fn close_modal(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some((Modal::Attach(picker), _)) = self.modal.take() {
            picker.update(cx, |picker, cx| picker.release(cx));
        }
        if let Some(focus) = self.onboarding_focus() {
            window.focus(focus, cx);
        } else if self.has_projects() {
            window.focus(&self.composer_focus, cx);
        } else {
            window.focus(&self.focus, cx);
        }
        cx.notify();
    }

    fn select_project(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        self.active_project = index;
        self.env_menu_open = false;
        self.after_switch(window, cx);
    }

    fn after_switch(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.completion = None;
        // The strip goes to the tab this environment showed last.
        self.show_env_tab(false);
        self.note_active_tab(cx);
        self.refresh_visible(cx);
        self.ensure_active_live(window, cx);
        self.ensure_terminal(window, cx);
        if self.panel_tab == PanelTab::Explorer {
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
        }
    }

    /// Whether the user may close the tab at `index`. Not the active tab when
    /// the environment on screen has no other: `remove_tab` would only put a
    /// blank New session tab in its place, which the user can open themselves.
    fn can_close_tab(&self, index: usize) -> bool {
        if !self.has_projects() || index >= self.strip.tabs.len() {
            return false;
        }
        !self.strip.is_last_on_screen(index, &self.env().path)
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
                self.strip.active_tab = match self.strip.front_of(&here) {
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
    /// unless the open environment has no thread yet.
    fn new_session(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.has_projects() {
            return;
        }
        if !self.env().sessions.is_empty() {
            self.quick_create_env(self.active_project, window, cx);
            return;
        }
        let path = self.env().path.clone();
        let blank = self
            .strip
            .find_in(&path, |t| matches!(t.kind, TabKind::NewSession));
        self.strip.active_tab = match blank {
            Some(i) => i,
            None => self.strip.add(Tab::new_session(&path)),
        };
        self.note_active_tab(cx);
        window.focus(&self.composer_focus, cx);
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
                    None => panel_empty("Connecting…").into_any_element(),
                };
                div()
                    .flex()
                    .flex_col()
                    .size_full()
                    .child(div().flex_1().min_h_0().child(transcript))
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
                                .child(self.render_composer(false, window, cx)),
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
                self.runs_view.clone().into_any_element()
            }
            Some(TabKind::Memory) => {
                let dir = env.path.clone();
                self.memory_view.update(cx, |v, cx| v.set_dir(&dir, cx));
                self.memory_view.clone().into_any_element()
            }
            Some(TabKind::Cloudflare) => self.cloudflare_view.clone().into_any_element(),
            #[cfg(feature = "coderlabs")]
            Some(TabKind::Site) => self.site_view.clone().into_any_element(),
            #[cfg(feature = "team")]
            Some(TabKind::Team) => self.team_view.clone().into_any_element(),
            Some(TabKind::PostHog) => {
                // Follows the visible project, also when it changes under an open tab.
                let root = self.project().path.clone();
                self.posthog_view
                    .update(cx, |v, cx| v.set_project(&root, cx));
                self.posthog_view.clone().into_any_element()
            }
            Some(TabKind::Branches) => {
                // Sessions start and stop all the time; the rows follow them.
                self.sync_branches_view(cx);
                self.branches_view.clone().into_any_element()
            }
            Some(TabKind::GitHub) => self.github_view.clone().into_any_element(),
            Some(TabKind::Extensions) => {
                // Follows the visible environment, also when it changes under an open tab.
                let dir = env.path.clone();
                self.extensions_view.update(cx, |v, cx| {
                    v.set_dir(dir, cx);
                    v.refresh_if_stale(cx);
                });
                self.extensions_view.clone().into_any_element()
            }
            Some(TabKind::Phone) => self.phone_view.clone().into_any_element(),
            Some(TabKind::Servers) => self.servers_view.clone().into_any_element(),
            Some(TabKind::Gallery) => self.gallery_view.clone().into_any_element(),
            Some(TabKind::SidebarBuilder) => self.render_sidebar_builder(window, cx),
            Some(TabKind::Isometric) => {
                // Follows the visible environment, also for a tab restored at
                // launch that was never opened from the rail.
                let root = self.env().path.clone();
                self.iso_view.update(cx, |v, cx| v.set_root(root, cx));
                self.iso_view.clone().into_any_element()
            }
            #[cfg(feature = "coderlabs")]
            Some(TabKind::Canvas) => self.canvas_view.clone().into_any_element(),
            Some(TabKind::Usage) => {
                self.sync_usage_view(cx);
                self.usage_view.clone().into_any_element()
            }
            _ if focused => self.render_home(window, cx).into_any_element(),
            _ => div()
                .flex()
                .flex_col()
                .size_full()
                .items_center()
                .justify_center()
                .gap(crate::ui::sp(10.))
                .text_size(px(ui::TEXT_MD))
                .text_color(theme::text_subtle())
                .child(agent_icon(env.agent, 22.))
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
            // With a project the GitHub tab is a tab again; without one later,
            // the welcome comes back first. The Servers tab too.
            self.github_alone = false;
            // The Servers tab in the welcome's place goes with the welcome, and
            // the server is told nobody is looking. After this frame: telling
            // it updates the view, which cannot happen while this one draws.
            if std::mem::take(&mut self.servers_alone) {
                cx.defer_in(window, |this, _, cx| this.servers_tab_closed(cx));
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
        } else if self.servers_alone {
            self.render_servers_alone(window, cx).into_any_element()
        } else if self.github_alone {
            self.render_github_alone(window, cx).into_any_element()
        } else {
            self.render_welcome(window, cx).into_any_element()
        };

        let palette_open = matches!(self.modal, Some((Modal::Palette(_), _)));
        let modal: Option<(AnyElement, u64)> = self.modal.as_ref().map(|(m, _)| match m {
            Modal::Picker(e) => (e.clone().into_any_element(), e.entity_id().as_u64()),
            Modal::Attach(e) => (e.clone().into_any_element(), e.entity_id().as_u64()),
            Modal::CreateEnv(e) => (e.clone().into_any_element(), e.entity_id().as_u64()),
            Modal::DeleteEnv(e) => (e.clone().into_any_element(), e.entity_id().as_u64()),
            Modal::Command(e) => (e.clone().into_any_element(), e.entity_id().as_u64()),
            Modal::Account(e) => (e.clone().into_any_element(), e.entity_id().as_u64()),
            Modal::Commit(e) => (e.clone().into_any_element(), e.entity_id().as_u64()),
            Modal::GitConfirm(e) => (e.clone().into_any_element(), e.entity_id().as_u64()),
            Modal::Palette(e) => (e.clone().into_any_element(), e.entity_id().as_u64()),
            Modal::Settings(e) => (e.clone().into_any_element(), e.entity_id().as_u64()),
            Modal::Automations(e) => (e.clone().into_any_element(), e.entity_id().as_u64()),
            Modal::Session(v) => (v.clone().into_any_element(), v.entity_id().as_u64()),
            Modal::Telemetry(e) => (e.clone().into_any_element(), e.entity_id().as_u64()),
        });
        let viewport_h = window.viewport_size().height.as_f32();
        let floating_sheet = self.render_floating_sheet(cx);
        let context_menu = self.menu.as_ref().map(|m| {
            let above = matches!(
                m.target,
                MenuTarget::ModelMenu | MenuTarget::PermissionMenu | MenuTarget::RailUsage
            );
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
                                this.rail_w = w.clamp(RAIL_W_RANGE.start, RAIL_W_RANGE.end)
                            }
                            Some(SideDrag::Panel) => {
                                this.panel_w = ((window_w - gap()) - ev.position.x.as_f32())
                                    .clamp(PANEL_W_RANGE.start, PANEL_W_RANGE.end)
                            }
                            Some(SideDrag::Subagents) => {
                                // Its right edge is where the Workspace panel starts.
                                let rail = this.rail_shown()
                                    && this.sidebar.style.side == crate::sidebar::Side::Right;
                                let right = window_w
                                    - gap()
                                    - if this.panel_shown() {
                                        this.panel_width() + gap()
                                    } else {
                                        0.
                                    }
                                    - if rail { this.rail_width() + gap() } else { 0. };
                                this.subagents_w = (right - ev.position.x.as_f32())
                                    .clamp(SUBAGENTS_W_RANGE.start, SUBAGENTS_W_RANGE.end)
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
                this.open_sidebar_builder(window, cx)
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
            .on_action(cx.listener(|_, _: &ToggleDance, _, cx| {
                crate::mascot::toggle_dancing();
                cx.notify();
            }))
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
            .capture_key_down(|ev: &gpui::KeyDownEvent, window, cx| {
                if ev.keystroke.key == "escape" && crate::agent::markdown::close_preview(window) {
                    cx.stop_propagation();
                }
            })
            .capture_key_down(cx.listener(|this, ev: &gpui::KeyDownEvent, window, cx| {
                let k = &ev.keystroke;
                if this.menu.is_some()
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
                    .when(chrome && self.subagents_shown(), |d| {
                        d.child(self.side_handle(SideDrag::Subagents, cx))
                            .child(self.render_subagents(cx))
                    })
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
                d.child(self.render_dock(viewport_h, cx))
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
                        .pt(relative(0.10))
                        .pb(crate::ui::sp(12.))
                        .px(crate::ui::sp(12.))
                        .occlude()
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(|this, _, window, cx| this.close_modal(window, cx)),
                        )
                        .child(
                            // Keyed by the dialog, so one opened over another
                            // arrives too. A flex column like the backdrop, so a
                            // tall dialog still fits the window.
                            div()
                                .flex()
                                .flex_col()
                                .items_center()
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

pub(crate) fn agent_icon(agent: Agent, size: f32) -> AnyElement {
    match agent {
        Agent::Claude => img("providers/claude.svg")
            .size(px(size))
            .flex_none()
            .into_any_element(),
        Agent::Droid => ui::factory_logo(size).into_any_element(),
        Agent::OpenCode => ui::opencode_logo(size).into_any_element(),
        Agent::Codex | Agent::Devin | Agent::WorkersAi => agent.kind().icon(size),
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

#[cfg(test)]
mod tests {
    use super::*;

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
    fn a_narrow_window_leaves_out_a_side_card() {
        let (rail, panel, sub) = (168., Some(208.), Some(268.));
        // 1366x768 at 125%: all three fit at their narrowest.
        let room = 1093. - 16. - MAIN_FLOOR_W;
        assert_eq!(
            crowd_out(room, rail, panel, sub, SideCard::Panel),
            [false; 2]
        );
        // The smallest window: the one not asked for last goes.
        let room = 800. - 16. - MAIN_FLOOR_W;
        assert_eq!(
            crowd_out(room, rail, panel, sub, SideCard::Panel),
            [false, true]
        );
        assert_eq!(
            crowd_out(room, rail, panel, sub, SideCard::Subagents),
            [true, false]
        );
        // Too narrow for either: both go, and the rail stays.
        assert_eq!(
            crowd_out(100., rail, panel, sub, SideCard::Panel),
            [true, true]
        );
        // A closed card is never counted or left out.
        assert_eq!(
            crowd_out(room, rail, None, sub, SideCard::Panel),
            [false, false]
        );
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
}
