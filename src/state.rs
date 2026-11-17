//! What survives a restart, in `~/.local/share/arakun/state.json`. Worktrees are
//! remembered too so the first frame shows the real list before git answers.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::agent::{SessionMeta, Setting};
use crate::model::{
    Agent, Automation, Environment, Project, Session, Snippet, Split, SplitDir, Status, Tab,
    TabKind,
};

#[derive(Serialize, Deserialize, Default)]
pub struct State {
    #[serde(default)]
    pub projects: Vec<ProjectState>,
    #[serde(default)]
    pub active_project: usize,
    #[serde(default = "yes")]
    pub dock_open: bool,
    #[serde(default = "dock_height")]
    pub dock_height: f32,
    #[serde(default = "yes")]
    pub rail_open: bool,
    #[serde(default = "yes")]
    pub panel_open: bool,
    #[serde(default = "rail_w")]
    pub rail_w: f32,
    #[serde(default = "panel_w")]
    pub panel_w: f32,
    /// The Subagents panel, beside the chat.
    #[serde(default)]
    pub subagents_open: bool,
    #[serde(default = "subagents_w")]
    pub subagents_w: f32,
    #[serde(default = "yes")]
    pub arcade: bool,
    /// `theme::DiffPalette` id.
    #[serde(default)]
    pub diff_palette: String,
    #[serde(default)]
    pub glass: bool,
    /// `theme::Grays` id; empty is the default ramp.
    #[serde(default)]
    pub grays: String,
    /// `ui::TypeScale` id.
    #[serde(default)]
    pub type_scale: String,
    /// Agents (`claude`, `droid`) the rail's usage card leaves out.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub rail_hidden: Vec<String>,
    /// The agent picked last (`claude`, `droid`): new environments start with it.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub last_agent: String,
    /// Whole-UI zoom in percent (`ui_zoom`).
    #[serde(default = "ui_zoom")]
    pub ui_zoom: u32,
    /// The Dynamic Island was turned off in Settings (`island`).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub island_off: bool,
    /// Where the window was when the app last saved, so it opens there again.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub window: Option<WindowState>,
}

/// The window's place and size in screen points, as `gpui::WindowBounds`.
/// For a maximized or full screen window, the bounds are where it goes back to.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct WindowState {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub maximized: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub fullscreen: bool,
    /// The display it was on (`PlatformDisplay::uuid`). macOS gives the bounds
    /// relative to that display, so they mean nothing on another one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display: Option<String>,
}

impl WindowState {
    /// The window as it is now, after `prev`. gpui on X11 reports neither full
    /// screen nor the bounds to go back to, and macOS reports a zoomed window as
    /// windowed, so the flags are read too. In either mode the bounds stay the
    /// last ones of the plain window, which are the ones to go back to, as long
    /// as it stays on the same display: bounds on one display are no place on
    /// another, so after a move they are what gpui reports.
    pub fn next(
        prev: Option<&WindowState>,
        bounds: gpui::WindowBounds,
        maximized: bool,
        fullscreen: bool,
        display: Option<String>,
    ) -> Self {
        let mut now = Self::from_bounds(bounds);
        now.fullscreen |= fullscreen;
        now.maximized = (now.maximized || maximized) && !now.fullscreen;
        if (now.fullscreen || now.maximized)
            && let Some(prev) = prev
            && prev.display == display
        {
            (now.x, now.y, now.w, now.h) = (prev.x, prev.y, prev.w, prev.h);
        }
        now.display = display;
        now
    }

    pub fn from_bounds(bounds: gpui::WindowBounds) -> Self {
        let (b, maximized, fullscreen) = match bounds {
            gpui::WindowBounds::Windowed(b) => (b, false, false),
            gpui::WindowBounds::Maximized(b) => (b, true, false),
            gpui::WindowBounds::Fullscreen(b) => (b, false, true),
        };
        WindowState {
            x: b.origin.x.into(),
            y: b.origin.y.into(),
            w: b.size.width.into(),
            h: b.size.height.into(),
            maximized,
            fullscreen,
            display: None,
        }
    }

    pub fn to_bounds(&self) -> gpui::WindowBounds {
        self.bounds_at(self.rect())
    }

    /// This window's mode, with `b` as its bounds.
    pub fn bounds_at(&self, b: gpui::Bounds<gpui::Pixels>) -> gpui::WindowBounds {
        if self.fullscreen {
            gpui::WindowBounds::Fullscreen(b)
        } else if self.maximized {
            gpui::WindowBounds::Maximized(b)
        } else {
            gpui::WindowBounds::Windowed(b)
        }
    }

    pub fn rect(&self) -> gpui::Bounds<gpui::Pixels> {
        gpui::Bounds::new(
            gpui::point(gpui::px(self.x), gpui::px(self.y)),
            gpui::size(gpui::px(self.w), gpui::px(self.h)),
        )
    }

    /// This window with its mode, centred in `visible`, a display's visible
    /// area. It keeps its size where that fits and shrinks where it does not:
    /// a window taller than the area would put its title bar off the screen.
    pub fn centred_in(&self, visible: gpui::Bounds<gpui::Pixels>) -> gpui::WindowBounds {
        let size = self.rect().size.min(&visible.size);
        self.bounds_at(gpui::Bounds::centered_at(visible.center(), size))
    }
}

/// Where a saved window opens.
#[derive(Debug, PartialEq)]
pub enum Place {
    /// At its saved bounds, on this display.
    At(gpui::DisplayId),
    /// With its mode, centred on this display; see `WindowState::centred_in`.
    Centred(gpui::DisplayId),
}

impl WindowState {
    /// Where the window opens, given each connected display's id, uuid and
    /// bounds, or `None` to open at the default size. Bounds count as shown
    /// when 100×40 points of them, enough to grab the title bar, are on the
    /// display. On the saved display but not shown there, the window is
    /// centred on it: Wayland tells no window where it is, so the saved place
    /// can be anywhere.
    pub fn place(
        &self,
        displays: &[(gpui::DisplayId, Option<String>, gpui::Bounds<gpui::Pixels>)],
    ) -> Option<Place> {
        let rect = self.rect();
        let shows = |b: &gpui::Bounds<gpui::Pixels>| {
            let shown = b.intersect(&rect);
            shown.size.width >= gpui::px(100.) && shown.size.height >= gpui::px(40.)
        };
        match &self.display {
            Some(uuid) => {
                let (id, _, b) = displays.iter().find(|(_, u, _)| u.as_ref() == Some(uuid))?;
                Some(if shows(b) {
                    Place::At(*id)
                } else {
                    Place::Centred(*id)
                })
            }
            // Saved without its display: the one holding its centre, else any
            // it shows on. Windows opens on the primary display without one.
            None => {
                let centre = rect.center();
                displays
                    .iter()
                    .find(|(_, _, b)| b.contains(&centre) && shows(b))
                    .or_else(|| displays.iter().find(|(_, _, b)| shows(b)))
                    .map(|(id, _, _)| Place::At(*id))
            }
        }
    }
}

/// The saved window, read without the rest of the state, which the shell
/// loads later. Only the `window` field is kept.
pub fn saved_window() -> Option<WindowState> {
    #[derive(Deserialize)]
    struct Only {
        window: Option<WindowState>,
    }
    let bytes = std::fs::read(file()).ok()?;
    serde_json::from_slice::<Only>(&bytes).ok()?.window
}

fn ui_zoom() -> u32 {
    100
}

fn yes() -> bool {
    true
}

fn dock_height() -> f32 {
    DOCK_HEIGHT
}

fn rail_w() -> f32 {
    RAIL_W
}

fn panel_w() -> f32 {
    PANEL_W
}

fn subagents_w() -> f32 {
    SUBAGENTS_W
}

pub const DOCK_HEIGHT: f32 = 260.;
/// Default widths of the side cards; the state file carries the dragged ones.
pub const RAIL_W: f32 = 200.;
pub const PANEL_W: f32 = 260.;
pub const SUBAGENTS_W: f32 = 360.;

/// Window layout that is not tied to a project.
#[derive(Clone)]
pub struct Layout {
    pub dock_open: bool,
    pub dock_height: f32,
    pub rail_open: bool,
    pub panel_open: bool,
    pub rail_w: f32,
    pub panel_w: f32,
    pub subagents_open: bool,
    pub subagents_w: f32,
    pub arcade: bool,
    pub diff_palette: crate::theme::DiffPalette,
    pub glass: bool,
    pub grays: crate::theme::Grays,
    pub type_scale: crate::ui::TypeScale,
    /// Whether the rail's usage card shows Claude, Droid and OpenCode.
    pub rail_usage: [bool; 3],
    pub ui_zoom: u32,
    pub window: Option<WindowState>,
}

#[derive(Serialize, Deserialize)]
pub struct ProjectState {
    pub path: PathBuf,
    pub logo: u32,
    pub art: u8,
    /// Shell command run in a terminal when a new environment is created.
    #[serde(default)]
    pub setup: Option<String>,
    /// Shell command run after agent turns that edit files.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub check: Option<String>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub check_declined: bool,
    #[serde(default)]
    pub notes: Vec<String>,
    #[serde(default)]
    pub monos: Vec<Snippet>,
    /// Old files hold only name and body; the rest defaults to a manual command.
    #[serde(default)]
    pub automations: Vec<Automation>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg(feature = "coderlabs")]
    pub coderspace: Option<String>,
    #[serde(default)]
    pub active_env: Option<PathBuf>,
    #[serde(default)]
    pub envs: Vec<EnvState>,
}

#[derive(Serialize, Deserialize)]
pub struct EnvState {
    pub path: PathBuf,
    #[serde(default)]
    pub branch: Option<String>,
    #[serde(default)]
    pub is_main: bool,
    /// The random name the first prompt replaces, while the branch still has it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub placeholder: Option<String>,
    /// The rail shows the branch, not the folder.
    #[serde(default, skip_serializing_if = "is_false")]
    pub follow_branch: bool,
    #[serde(default)]
    pub agent: Option<String>,
    /// Native Droid settings picked on the New session tab, as (key, value).
    #[serde(default)]
    pub settings: Vec<(String, String)>,
    /// Claude Code settings for the next session, as (key, value).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub claude_settings: Vec<(String, String)>,
    /// OpenCode settings for the next session, as (key, value).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub opencode_settings: Vec<(String, String)>,
    /// Codex settings for the next session, as (key, value).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub codex_settings: Vec<(String, String)>,
    /// Workers AI settings for the next session, as (key, value).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub workers_ai_settings: Vec<(String, String)>,
    #[serde(default)]
    pub sessions: Vec<SessionState>,
    /// Ids of the sessions open as tabs, in order.
    #[serde(default)]
    pub open: Vec<String>,
    #[serde(default)]
    pub active_tab: usize,
    /// URLs of the browser tabs, which reopen after the session tabs.
    #[serde(default)]
    pub browsers: Vec<String>,
    #[serde(default)]
    pub split: Option<SplitState>,
    /// What was being written in the composer, and where its caret was.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub draft: String,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub caret: usize,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub images: Vec<PathBuf>,
}

fn is_zero(n: &usize) -> bool {
    *n == 0
}

fn is_false(b: &bool) -> bool {
    !*b
}

#[derive(Serialize, Deserialize)]
pub struct SplitState {
    /// The other pane's tab, by position among the reopened tabs (sessions, then browsers).
    pub tab: usize,
    #[serde(default)]
    pub down: bool,
    pub ratio: f32,
    #[serde(default)]
    pub focus_first: bool,
}

#[derive(Serialize, Deserialize)]
pub struct SessionState {
    pub id: String,
    pub title: String,
    pub agent: String,
    pub updated: u64,
    #[serde(default)]
    pub added: u32,
    #[serde(default)]
    pub removed: u32,
    #[serde(default)]
    pub meta: Option<SessionMeta>,
    /// The CoderSpace task it was started from.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg(feature = "coderlabs")]
    pub task: Option<String>,
    /// The GitHub Actions run it was started to fix.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ci_run: Option<u64>,
    /// Started by `arakun agent new`: the model it asked for.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cli_model: Option<String>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub auto_allow: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub continues: Vec<crate::model::Earlier>,
}

fn is_session(t: &Tab) -> bool {
    matches!(t.kind, TabKind::Session { .. })
}

/// Position of the active tab among the session tabs (0 when another tab is active).
fn active_session_tab(e: &Environment) -> usize {
    match e.tabs.get(e.active_tab) {
        Some(t) if is_session(t) => e.tabs[..e.active_tab]
            .iter()
            .filter(|t| is_session(t))
            .count(),
        _ => 0,
    }
}

fn is_saved_browser(t: &Tab) -> bool {
    matches!(&t.kind, TabKind::Browser { url } if !matches!(url.as_ref(), "" | "about:blank"))
}

/// Position of a tab among the tabs that reopen after a restart, in their reopened order.
fn reopened_index(e: &Environment, id: u64) -> Option<usize> {
    let tab = e.tabs.iter().position(|t| t.id == id)?;
    let before = &e.tabs[..tab];
    if is_session(&e.tabs[tab]) {
        Some(before.iter().filter(|t| is_session(t)).count())
    } else if is_saved_browser(&e.tabs[tab]) {
        let sessions = e.tabs.iter().filter(|t| is_session(t)).count();
        Some(sessions + before.iter().filter(|t| is_saved_browser(t)).count())
    } else {
        None
    }
}

fn split_state(e: &Environment) -> Option<SplitState> {
    let split = e.split.as_ref()?;
    e.split_other()?;
    Some(SplitState {
        tab: reopened_index(e, split.other)?,
        down: split.dir == SplitDir::Down,
        ratio: split.ratio,
        focus_first: split.focus_first,
    })
}

/// Where a session's transcript is saved.
pub fn transcript_path(id: &str) -> PathBuf {
    data_dir().join("sessions").join(format!("{id}.json"))
}

/// Sessions `arakun restart` cut off mid-turn, for the next arakun to reopen.
pub fn resume_path() -> PathBuf {
    file().with_extension("resume.json")
}

pub fn data_dir() -> PathBuf {
    crate::platform::app_dir()
}

fn file() -> PathBuf {
    std::env::var_os("ARAKUN_STATE")
        .map(PathBuf::from)
        .unwrap_or_else(|| data_dir().join("state.json"))
}

pub fn load() -> State {
    std::fs::read(file())
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or(State {
            dock_open: true,
            dock_height: DOCK_HEIGHT,
            rail_open: true,
            panel_open: true,
            rail_w: RAIL_W,
            panel_w: PANEL_W,
            subagents_w: SUBAGENTS_W,
            arcade: true,
            ..Default::default()
        })
}

pub fn write(bytes: &[u8]) -> std::io::Result<()> {
    crate::durable::write(&file(), bytes)
}

pub fn snapshot(projects: &[Project], active_project: usize, layout: Layout) -> Vec<u8> {
    let state = State {
        projects: projects
            .iter()
            // Headless sessions are listed from their own folder, not saved here;
            // their project stays once it holds sessions the app took over.
            .filter(|p| {
                p.path != crate::headless::root()
                    || p.environments
                        .iter()
                        .flat_map(|e| &e.sessions)
                        .any(|s| !crate::headless::shown(&s.id))
            })
            .map(|p| ProjectState {
                path: p.path.clone(),
                logo: p.logo,
                art: p.art,
                setup: p.setup.clone(),
                check: p.check.clone(),
                check_declined: p.check_declined,
                notes: p.notes.clone(),
                monos: p.monos.clone(),
                automations: p.automations.clone(),
                #[cfg(feature = "coderlabs")]
                coderspace: p.coderspace.clone(),
                active_env: p.environments.get(p.active_env).map(|e| e.path.clone()),
                envs: p
                    .environments
                    .iter()
                    .map(|e| EnvState {
                        path: e.path.clone(),
                        branch: Some(e.branch.to_string()),
                        is_main: e.is_checkout,
                        placeholder: e.placeholder.clone(),
                        follow_branch: e.follow_branch,
                        agent: Some(e.agent.key().into()),
                        settings: e
                            .draft_settings
                            .iter()
                            .map(|(s, v)| (s.key().to_string(), v.clone()))
                            .collect(),
                        claude_settings: e
                            .claude_settings
                            .iter()
                            .map(|(s, v)| (s.key().to_string(), v.clone()))
                            .collect(),
                        opencode_settings: e
                            .opencode_settings
                            .iter()
                            .map(|(s, v)| (s.key().to_string(), v.clone()))
                            .collect(),
                        codex_settings: e
                            .codex_settings
                            .iter()
                            .map(|(s, v)| (s.key().to_string(), v.clone()))
                            .collect(),
                        workers_ai_settings: e
                            .workers_ai_settings
                            .iter()
                            .map(|(s, v)| (s.key().to_string(), v.clone()))
                            .collect(),
                        sessions: e
                            .sessions
                            .iter()
                            .filter(|s| !crate::headless::shown(&s.id))
                            .map(|s| SessionState {
                                id: s.id.to_string(),
                                title: s.title.to_string(),
                                agent: s.agent.key().into(),
                                updated: s.updated,
                                added: s.added,
                                removed: s.removed,
                                meta: s.meta.clone(),
                                #[cfg(feature = "coderlabs")]
                                task: s.task.clone(),
                                ci_run: s.ci_run,
                                cli_model: s.cli_model.clone(),
                                auto_allow: s.auto_allow,
                                continues: s.continues.clone(),
                            })
                            .collect(),
                        open: e
                            .tabs
                            .iter()
                            .filter_map(|t| match &t.kind {
                                TabKind::Session { id, .. } => Some(id.to_string()),
                                _ => None,
                            })
                            .collect(),
                        active_tab: active_session_tab(e),
                        browsers: e
                            .tabs
                            .iter()
                            .filter_map(|t| match &t.kind {
                                // Blank tabs are not worth reopening.
                                TabKind::Browser { url } if is_saved_browser(t) => {
                                    Some(url.to_string())
                                }
                                _ => None,
                            })
                            .collect(),
                        split: split_state(e),
                        draft: e.draft.to_string(),
                        caret: e.draft.head(),
                        images: e.draft_images.clone(),
                    })
                    .collect(),
            })
            .collect(),
        active_project,
        dock_open: layout.dock_open,
        dock_height: layout.dock_height,
        rail_open: layout.rail_open,
        panel_open: layout.panel_open,
        rail_w: layout.rail_w,
        panel_w: layout.panel_w,
        subagents_open: layout.subagents_open,
        subagents_w: layout.subagents_w,
        arcade: layout.arcade,
        diff_palette: layout.diff_palette.id().to_string(),
        glass: layout.glass,
        grays: layout.grays.id().to_string(),
        type_scale: layout.type_scale.id().to_string(),
        rail_hidden: [
            ("claude", layout.rail_usage[0]),
            ("droid", layout.rail_usage[1]),
            ("opencode", layout.rail_usage[2]),
        ]
        .into_iter()
        .filter(|(_, shown)| !shown)
        .map(|(k, _)| k.to_string())
        .collect(),
        ui_zoom: layout.ui_zoom,
        last_agent: crate::model::default_agent().key().to_string(),
        island_off: !crate::island::enabled(),
        window: layout.window,
    };
    serde_json::to_vec_pretty(&state).unwrap_or_default()
}

pub fn restore(state: State) -> (Vec<Project>, usize, Layout) {
    let last_agent = state.last_agent.clone();
    crate::island::set_enabled(!state.island_off);
    let projects: Vec<Project> = state
        .projects
        .into_iter()
        .map(|p| {
            let mut environments: Vec<Environment> = p
                .envs
                .into_iter()
                .map(|e| {
                    let mut env = Environment::new(e.path, None, e.is_main);
                    env.placeholder = e.placeholder;
                    env.follow_branch = e.follow_branch;
                    env.set_branch(e.branch);
                    if let Some(agent) = e.agent {
                        env.agent = Agent::from_key(&agent);
                    }
                    env.draft_settings = e
                        .settings
                        .into_iter()
                        .filter_map(|(k, v)| Setting::from_key(&k).map(|s| (s, v)))
                        .collect();
                    env.claude_settings = e
                        .claude_settings
                        .into_iter()
                        .filter_map(|(k, v)| Setting::from_key(&k).map(|s| (s, v)))
                        .collect();
                    env.opencode_settings = e
                        .opencode_settings
                        .into_iter()
                        .filter_map(|(k, v)| Setting::from_key(&k).map(|s| (s, v)))
                        .collect();
                    env.codex_settings = e
                        .codex_settings
                        .into_iter()
                        .filter_map(|(k, v)| Setting::from_key(&k).map(|s| (s, v)))
                        .collect();
                    env.workers_ai_settings = e
                        .workers_ai_settings
                        .into_iter()
                        .filter_map(|(k, v)| Setting::from_key(&k).map(|s| (s, v)))
                        .collect();
                    env.draft = crate::draft::Draft::restore(e.draft, e.caret);
                    env.draft_images = e.images.into_iter().filter(|p| p.is_file()).collect();
                    env.sessions = e
                        .sessions
                        .into_iter()
                        .map(|s| {
                            let mut session = Session::new(
                                s.id.into(),
                                s.title.into(),
                                Agent::from_key(&s.agent),
                            );
                            session.updated = s.updated;
                            session.added = s.added;
                            session.removed = s.removed;
                            session.meta = s.meta;
                            #[cfg(feature = "coderlabs")]
                            {
                                session.task = s.task;
                            }
                            session.ci_run = s.ci_run;
                            session.cli_model = s.cli_model;
                            session.auto_allow = s.auto_allow;
                            session.continues = s.continues;
                            session.status = Status::Idle;
                            session
                        })
                        .collect();
                    let tabs: Vec<Tab> = e
                        .open
                        .iter()
                        .filter_map(|id| {
                            let s = &env.sessions[env.session_index(id)?];
                            Some(Tab::new(
                                s.title.clone(),
                                TabKind::Session {
                                    agent: s.agent,
                                    id: s.id.clone(),
                                },
                            ))
                        })
                        .collect();
                    let mut tabs = tabs;
                    let sessions_open = tabs.len();
                    tabs.extend(e.browsers.into_iter().map(|url| {
                        let title = if url.is_empty() {
                            "New tab".to_string()
                        } else {
                            url.clone()
                        };
                        Tab::new(title, TabKind::Browser { url: url.into() })
                    }));
                    if !tabs.is_empty() {
                        env.active_tab = if sessions_open > 0 {
                            e.active_tab.min(sessions_open - 1)
                        } else {
                            0
                        };
                        env.tabs = tabs;
                    }
                    env.split = e.split.and_then(|s| {
                        let other = env.tabs.get(s.tab).filter(|_| s.tab != env.active_tab)?;
                        Some(Split {
                            other: other.id,
                            dir: if s.down {
                                SplitDir::Down
                            } else {
                                SplitDir::Right
                            },
                            ratio: s.ratio.clamp(Split::MIN_RATIO, Split::MAX_RATIO),
                            focus_first: s.focus_first,
                        })
                    });
                    env
                })
                .collect();
            if environments.is_empty() {
                environments.push(Environment::new(p.path.clone(), None, true));
            }
            let active_env = p
                .active_env
                .and_then(|a| environments.iter().position(|e| e.path == a))
                .unwrap_or(0);
            Project {
                name: crate::model::dir_name(&p.path).into(),
                path: p.path,
                logo: crate::model::current_logo(p.logo),
                art: p.art,
                environments,
                active_env,
                setup: p.setup,
                check: p.check,
                check_declined: p.check_declined,
                notes: p.notes,
                monos: p.monos,
                automations: p.automations,
                #[cfg(feature = "coderlabs")]
                coderspace: p.coderspace,
            }
        })
        .collect();
    let active = state.active_project.min(projects.len().saturating_sub(1));
    let layout = Layout {
        dock_open: state.dock_open,
        dock_height: state.dock_height,
        rail_open: state.rail_open,
        panel_open: state.panel_open,
        rail_w: state.rail_w,
        panel_w: state.panel_w,
        subagents_open: state.subagents_open,
        subagents_w: state.subagents_w,
        arcade: state.arcade,
        diff_palette: crate::theme::DiffPalette::from_id(&state.diff_palette),
        glass: state.glass,
        grays: crate::theme::Grays::from_id(&state.grays),
        type_scale: crate::ui::TypeScale::from_id(&state.type_scale),
        rail_usage: ["claude", "droid", "opencode"]
            .map(|k| !state.rail_hidden.iter().any(|h| h == k)),
        // 0 only comes from `State::default()`, when there is no state file.
        ui_zoom: if state.ui_zoom == 0 {
            100
        } else {
            state.ui_zoom
        },
        window: state.window,
    };
    // State from before `last_agent` was kept: the agent of the latest session.
    let latest = projects
        .iter()
        .flat_map(|p| &p.environments)
        .flat_map(|e| &e.sessions)
        .max_by_key(|s| s.updated)
        .map(|s| s.agent);
    match (last_agent.is_empty(), latest) {
        (false, _) => crate::model::set_default_agent(Agent::from_key(&last_agent)),
        (true, Some(agent)) => crate::model::set_default_agent(agent),
        (true, None) => {}
    }
    (projects, active, layout)
}

pub fn new_project(path: &Path) -> Project {
    let (logo, art) = crate::model::logo_for(path);
    Project {
        name: crate::model::dir_name(path).into(),
        path: path.to_path_buf(),
        logo,
        art,
        environments: vec![Environment::new(path.to_path_buf(), None, true)],
        active_env: 0,
        setup: None,
        check: None,
        check_declined: false,
        notes: Vec::new(),
        monos: Vec::new(),
        automations: Vec::new(),
        #[cfg(feature = "coderlabs")]
        coderspace: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn continued_sessions_keep_what_they_took_over() {
        let old: SessionState =
            serde_json::from_str(r#"{"id":"s1","title":"T","agent":"droid","updated":1}"#).unwrap();
        assert!(
            old.continues.is_empty(),
            "older state files have no earlier sessions"
        );
        let json = r#"{"id":"s2","title":"T","agent":"claude","updated":1,"continues":[{"id":"s1","agent_session":"d-1"},{"id":"s0"}]}"#;
        let s: SessionState = serde_json::from_str(json).unwrap();
        assert_eq!(
            s.continues,
            vec![
                crate::model::Earlier {
                    id: "s1".into(),
                    agent_session: Some("d-1".into())
                },
                crate::model::Earlier {
                    id: "s0".into(),
                    agent_session: None
                },
            ]
        );
        let saved = serde_json::to_value(&s).unwrap();
        assert_eq!(
            saved["continues"],
            serde_json::json!([{"id": "s1", "agent_session": "d-1"}, {"id": "s0"}])
        );
        assert!(
            serde_json::to_value(&old)
                .unwrap()
                .get("continues")
                .is_none()
        );
    }

    #[test]
    fn old_automations_load_as_manual_commands() {
        let json = r#"{"path":"/tmp/p","logo":1,"art":0,"automations":[{"name":"Tests","body":"cargo test"}]}"#;
        let p: ProjectState = serde_json::from_str(json).unwrap();
        let a = &p.automations[0];
        assert_eq!((a.name.as_str(), a.body.as_str()), ("Tests", "cargo test"));
        assert!(a.agent.is_none() && a.enabled && a.env.is_none());
        assert_eq!(a.schedule, crate::schedule::Schedule::Manual);
        assert_eq!(a.next_run(crate::model::unix_now()), None);
    }

    #[test]
    fn hidden_rail_survives_a_restart() {
        let state: State = serde_json::from_str("{}").unwrap();
        assert!(state.rail_open, "older state files show the rail");
        assert_eq!(
            state.rail_w, RAIL_W,
            "older state files get the default rail width"
        );
        let layout = Layout {
            dock_open: false,
            dock_height: DOCK_HEIGHT,
            rail_open: false,
            panel_open: false,
            rail_w: RAIL_W,
            panel_w: PANEL_W,
            subagents_open: false,
            subagents_w: SUBAGENTS_W,
            arcade: false,
            diff_palette: crate::theme::DiffPalette::Default,
            glass: false,
            grays: crate::theme::Grays::Crisp,
            type_scale: crate::ui::TypeScale::Compact,
            rail_usage: [true, true, true],
            ui_zoom: 100,
            window: None,
        };
        let (_, _, layout) = restore(serde_json::from_slice(&snapshot(&[], 0, layout)).unwrap());
        assert!(!layout.rail_open && !layout.panel_open);
    }

    #[test]
    fn rail_usage_choice_survives_a_restart() {
        let state: State = serde_json::from_str("{}").unwrap();
        assert!(
            state.rail_hidden.is_empty(),
            "older state files show both providers"
        );
        let layout = Layout {
            dock_open: false,
            dock_height: DOCK_HEIGHT,
            rail_open: true,
            panel_open: true,
            rail_w: RAIL_W,
            panel_w: PANEL_W,
            subagents_open: false,
            subagents_w: SUBAGENTS_W,
            arcade: false,
            diff_palette: crate::theme::DiffPalette::Default,
            glass: false,
            grays: crate::theme::Grays::Soft,
            type_scale: crate::ui::TypeScale::Comfortable,
            rail_usage: [true, false, true],
            ui_zoom: 100,
            window: None,
        };
        let (_, _, layout) = restore(serde_json::from_slice(&snapshot(&[], 0, layout)).unwrap());
        assert_eq!(layout.rail_usage, [true, false, true]);
        assert_eq!(
            (layout.grays, layout.type_scale),
            (crate::theme::Grays::Soft, crate::ui::TypeScale::Comfortable)
        );
    }

    #[test]
    #[cfg(feature = "coderlabs")]
    fn coderspace_link_round_trips() {
        let json = r#"{"path":"/tmp/p","logo":1,"art":0}"#;
        let p: ProjectState = serde_json::from_str(json).unwrap();
        assert_eq!(p.coderspace, None);
        assert!(!serde_json::to_string(&p).unwrap().contains("coderspace"));

        let mut project = new_project(Path::new("/tmp/arakun-cs-test"));
        project.coderspace = Some("rp7".into());
        let layout = Layout {
            dock_open: false,
            dock_height: DOCK_HEIGHT,
            rail_open: true,
            panel_open: true,
            rail_w: RAIL_W,
            panel_w: PANEL_W,
            subagents_open: false,
            subagents_w: SUBAGENTS_W,
            arcade: false,
            diff_palette: crate::theme::DiffPalette::Default,
            glass: false,
            grays: crate::theme::Grays::Crisp,
            type_scale: crate::ui::TypeScale::Compact,
            rail_usage: [true, true, true],
            ui_zoom: 100,
            window: None,
        };
        let bytes = snapshot(&[project], 0, layout);
        let state: State = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(state.projects[0].coderspace.as_deref(), Some("rp7"));
    }

    #[test]
    fn placeholder_survives_a_restart_but_not_a_rename() {
        let mut project = new_project(Path::new("/tmp/arakun-placeholder-test"));
        let mut env = Environment::new(
            "/tmp/arakun-placeholder-test-2".into(),
            Some("env/brave-otter".into()),
            false,
        );
        env.placeholder = Some("env/brave-otter".into());
        project.environments.push(env);
        let layout = Layout {
            dock_open: false,
            dock_height: DOCK_HEIGHT,
            rail_open: true,
            panel_open: true,
            rail_w: RAIL_W,
            panel_w: PANEL_W,
            subagents_open: false,
            subagents_w: SUBAGENTS_W,
            arcade: false,
            diff_palette: crate::theme::DiffPalette::Default,
            glass: false,
            grays: crate::theme::Grays::Crisp,
            type_scale: crate::ui::TypeScale::Compact,
            rail_usage: [true, true, true],
            ui_zoom: 100,
            window: None,
        };
        let bytes = snapshot(std::slice::from_ref(&project), 0, layout);
        let (mut projects, _, _) = restore(serde_json::from_slice(&bytes).unwrap());
        let env = &mut projects[0].environments[1];
        assert!(env.awaits_name());
        // Renamed by hand while arakun was closed: the refresh after the restart reads it.
        env.set_branch(Some("env/bright-fox".into()));
        assert!(!env.awaits_name());
    }

    #[test]
    fn draft_survives_a_restart() {
        let mut project = new_project(Path::new("/tmp/arakun-draft-test"));
        project.environments[0].draft.set("fix the tests\nplease");
        project.environments[0].draft.move_to(3, false);
        project.environments.push(Environment::new(
            "/tmp/arakun-draft-test-2".into(),
            None,
            false,
        ));
        let layout = Layout {
            dock_open: false,
            dock_height: DOCK_HEIGHT,
            rail_open: true,
            panel_open: true,
            rail_w: RAIL_W,
            panel_w: PANEL_W,
            subagents_open: false,
            subagents_w: SUBAGENTS_W,
            arcade: false,
            diff_palette: crate::theme::DiffPalette::Default,
            glass: false,
            grays: crate::theme::Grays::Crisp,
            type_scale: crate::ui::TypeScale::Compact,
            rail_usage: [true, true, true],
            ui_zoom: 100,
            window: None,
        };
        let bytes = snapshot(std::slice::from_ref(&project), 0, layout);
        let (projects, _, _) = restore(serde_json::from_slice(&bytes).unwrap());
        let draft = &projects[0].environments[0].draft;
        assert_eq!((&**draft, draft.head()), ("fix the tests\nplease", 3));
        assert!(projects[0].environments[1].draft.is_empty());
        let empty = String::from_utf8(bytes).unwrap();
        assert_eq!(
            empty.matches("\"draft\"").count(),
            1,
            "empty drafts are not written"
        );
    }

    #[test]
    fn split_survives_a_restart() {
        let mut project = new_project(Path::new("/tmp/arakun-split-test"));
        let env = &mut project.environments[0];
        env.sessions = vec![Session::new("s1".into(), "One".into(), Agent::Droid)];
        env.tabs = vec![
            Tab::new(
                "One",
                TabKind::Session {
                    agent: Agent::Droid,
                    id: "s1".into(),
                },
            ),
            Tab::new("New tab", TabKind::Browser { url: "".into() }),
            Tab::new(
                "Docs",
                TabKind::Browser {
                    url: "https://example.com".into(),
                },
            ),
        ];
        env.active_tab = 0;
        env.split = Some(Split {
            other: env.tabs[2].id,
            dir: SplitDir::Down,
            ratio: 0.35,
            focus_first: true,
        });

        let layout = Layout {
            dock_open: false,
            dock_height: DOCK_HEIGHT,
            rail_open: true,
            panel_open: true,
            rail_w: RAIL_W,
            panel_w: PANEL_W,
            subagents_open: false,
            subagents_w: SUBAGENTS_W,
            arcade: false,
            diff_palette: crate::theme::DiffPalette::Default,
            glass: false,
            grays: crate::theme::Grays::Crisp,
            type_scale: crate::ui::TypeScale::Compact,
            rail_usage: [true, true, true],
            ui_zoom: 100,
            window: None,
        };
        let bytes = snapshot(std::slice::from_ref(&project), 0, layout);
        let (projects, _, _) = restore(serde_json::from_slice(&bytes).unwrap());
        let env = &projects[0].environments[0];
        // The blank browser tab is not reopened, so the docs tab lands at index 1.
        assert_eq!(env.tabs.len(), 2);
        let split = env.split.expect("split restored");
        assert_eq!(env.split_other(), Some(1));
        assert_eq!(split.dir, SplitDir::Down);
        assert!(split.focus_first);
        assert!((split.ratio - 0.35).abs() < 1e-6);
    }

    #[test]
    fn the_window_comes_back_where_it_was() {
        let rect = gpui::Bounds::new(
            gpui::point(gpui::px(40.), gpui::px(60.)),
            gpui::size(gpui::px(1000.), gpui::px(700.)),
        );
        for bounds in [
            gpui::WindowBounds::Windowed(rect),
            gpui::WindowBounds::Maximized(rect),
            gpui::WindowBounds::Fullscreen(rect),
        ] {
            let saved = WindowState::from_bounds(bounds);
            let json = serde_json::to_string(&saved).unwrap();
            let back: WindowState = serde_json::from_str(&json).unwrap();
            assert_eq!(back.to_bounds(), bounds);
        }
        let plain = serde_json::to_string(&WindowState::from_bounds(gpui::WindowBounds::Windowed(
            rect,
        )))
        .unwrap();
        assert!(!plain.contains("maximized") && !plain.contains("fullscreen"));
        let old: State = serde_json::from_str("{}").unwrap();
        assert!(old.window.is_none(), "older state files open centred");
    }

    #[test]
    fn the_window_opens_on_the_display_it_was_saved_on() {
        let px = gpui::px;
        let rect = |x: f32, y: f32, w: f32, h: f32| {
            gpui::Bounds::new(gpui::point(px(x), px(y)), gpui::size(px(w), px(h)))
        };
        let (a, b) = (gpui::DisplayId::new(1), gpui::DisplayId::new(2));
        // Two 1920×1080 displays side by side, as Windows and X11 give them.
        let displays = [
            (a, Some("uuid-a".to_string()), rect(0., 0., 1920., 1080.)),
            (b, Some("uuid-b".to_string()), rect(1920., 0., 1920., 1080.)),
        ];
        let saved = |x: f32, display: Option<&str>| WindowState {
            x,
            y: 100.,
            w: 1000.,
            h: 640.,
            maximized: false,
            fullscreen: false,
            display: display.map(str::to_string),
        };
        assert_eq!(
            saved(2100., Some("uuid-b")).place(&displays),
            Some(Place::At(b))
        );
        // Shown nowhere on its display (Wayland's kept origin): centred there.
        assert_eq!(
            saved(0., Some("uuid-b")).place(&displays),
            Some(Place::Centred(b))
        );
        // Its display is gone: the default.
        assert_eq!(saved(2100., Some("uuid-c")).place(&displays), None);
        // No uuid: the display holding its centre, not the first one it touches.
        assert_eq!(saved(1500., None).place(&displays), Some(Place::At(b)));
        assert_eq!(saved(2100., None).place(&displays), Some(Place::At(b)));
        assert_eq!(saved(100., None).place(&displays), Some(Place::At(a)));
        assert_eq!(saved(9000., None).place(&displays), None);
    }

    #[test]
    fn a_centred_window_keeps_its_size_and_mode_where_it_fits() {
        let px = gpui::px;
        let rect = |x: f32, y: f32, w: f32, h: f32| {
            gpui::Bounds::new(gpui::point(px(x), px(y)), gpui::size(px(w), px(h)))
        };
        // A 1280×800 display right of another, under a 25 point menu bar.
        let visible = rect(1920., 25., 1280., 775.);
        let saved = |w: f32, h: f32, maximized: bool| WindowState {
            x: 0.,
            y: 0.,
            w,
            h,
            maximized,
            fullscreen: false,
            display: None,
        };
        assert_eq!(
            saved(1000., 640., true).centred_in(visible),
            gpui::WindowBounds::Maximized(rect(2060., 92.5, 1000., 640.))
        );
        // Wider and taller than the area: it fills it, title bar on screen.
        assert_eq!(
            saved(1600., 900., false).centred_in(visible),
            gpui::WindowBounds::Windowed(visible)
        );
        // Only taller: it keeps its width.
        assert_eq!(
            saved(1000., 900., false).centred_in(visible),
            gpui::WindowBounds::Windowed(rect(2060., 25., 1000., 775.))
        );
    }

    #[test]
    fn a_maximized_window_moved_to_another_display_takes_its_new_bounds() {
        let rect = |x: f32| {
            gpui::Bounds::new(
                gpui::point(gpui::px(x), gpui::px(60.)),
                gpui::size(gpui::px(1000.), gpui::px(700.)),
            )
        };
        let on = |d: &str| Some(d.to_string());
        // Maximized on display A, its plain bounds at x = 40.
        let on_a = WindowState::next(
            None,
            gpui::WindowBounds::Maximized(rect(40.)),
            true,
            false,
            on("a"),
        );
        // Windows moves it to display B at x = 1920 and reports its new place.
        let on_b = WindowState::next(
            Some(&on_a),
            gpui::WindowBounds::Maximized(rect(1960.)),
            true,
            false,
            on("b"),
        );
        assert_eq!(on_b.display, on("b"));
        assert_eq!(on_b.to_bounds(), gpui::WindowBounds::Maximized(rect(1960.)));
        // Still on B: those bounds stay.
        let still = WindowState::next(
            Some(&on_b),
            gpui::WindowBounds::Maximized(rect(1920.)),
            true,
            false,
            on("b"),
        );
        assert_eq!(still.rect(), rect(1960.));
    }

    #[test]
    fn a_maximized_or_full_screen_window_keeps_its_plain_bounds() {
        let plain = gpui::Bounds::new(
            gpui::point(gpui::px(40.), gpui::px(60.)),
            gpui::size(gpui::px(1000.), gpui::px(700.)),
        );
        let screen = gpui::Bounds::new(
            gpui::point(gpui::px(0.), gpui::px(0.)),
            gpui::size(gpui::px(1920.), gpui::px(1080.)),
        );
        let uuid = || Some("display-a".to_string());
        let windowed = WindowState::next(
            None,
            gpui::WindowBounds::Windowed(plain),
            false,
            false,
            uuid(),
        );
        assert_eq!(windowed.rect(), plain);
        assert_eq!(windowed.display, uuid());
        // X11: full screen only shows in the flag, with the screen as bounds.
        let full = WindowState::next(
            Some(&windowed),
            gpui::WindowBounds::Windowed(screen),
            false,
            true,
            uuid(),
        );
        assert!(full.fullscreen && !full.maximized);
        assert_eq!(full.to_bounds(), gpui::WindowBounds::Fullscreen(plain));
        // X11 maximized reports the maximized size; macOS zoom reports Windowed.
        for bounds in [
            gpui::WindowBounds::Maximized(screen),
            gpui::WindowBounds::Windowed(screen),
        ] {
            let max = WindowState::next(Some(&windowed), bounds, true, false, uuid());
            assert_eq!(max.to_bounds(), gpui::WindowBounds::Maximized(plain));
        }
        // Back to a plain window: its own bounds again.
        let back = WindowState::next(
            Some(&full),
            gpui::WindowBounds::Windowed(plain),
            false,
            false,
            uuid(),
        );
        assert_eq!(back.to_bounds(), gpui::WindowBounds::Windowed(plain));
    }
}
