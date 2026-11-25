//! What survives a restart, in `~/.local/share/arakun/state.json`. Worktrees are
//! remembered too so the first frame shows the real list before git answers.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::agent::AgentKind;
use crate::agent::{SessionMeta, Setting};
use crate::model::{
    Automation, Environment, Project, Session, Snippet, Split, SplitDir, Status, Tab, TabKind,
    TabStrip,
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
    #[serde(default = "yes")]
    pub arcade: bool,
    /// `theme::DiffPalette` id.
    #[serde(default)]
    pub diff_palette: String,
    #[serde(default)]
    pub glass: bool,
    /// `theme::Contrast` id; empty follows the system.
    #[serde(default)]
    pub contrast: String,
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
    /// `ui::motion::Reduce` id; empty follows the system.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub reduce_motion: String,
    /// Closing the window hides it instead of quitting (`background`, Linux).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub background: bool,
    /// Where the window was when the app last saved, so it opens there again.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub window: Option<WindowState>,
    /// The reopened tabs in the order the strip showed them, by `tab_key`.
    /// Each environment keeps its own tabs too; this says how they mix.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tab_order: Vec<String>,
    /// How sessions and tabs show (`tab_layout`). Missing in files from
    /// before it; see `TabLayout::for_state_without_it`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tabs: Option<crate::tab_layout::TabLayout>,
    /// There was no state file: this is a new install.
    #[serde(skip)]
    pub fresh: bool,
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

pub const DOCK_HEIGHT: f32 = 260.;
/// Default widths of the side cards; the state file carries the dragged ones.
pub const RAIL_W: f32 = 200.;
pub const PANEL_W: f32 = 260.;

/// Window layout that is not tied to a project.
#[derive(Clone)]
pub struct Layout {
    pub dock_open: bool,
    pub dock_height: f32,
    pub rail_open: bool,
    pub panel_open: bool,
    pub rail_w: f32,
    pub panel_w: f32,
    pub arcade: bool,
    pub diff_palette: crate::theme::DiffPalette,
    pub glass: bool,
    pub contrast: crate::theme::Contrast,
    pub grays: crate::theme::Grays,
    pub type_scale: crate::ui::TypeScale,
    /// Whether the rail's usage card shows Claude, Droid, OpenCode and Devin.
    pub rail_usage: [bool; 4],
    pub ui_zoom: u32,
    pub window: Option<WindowState>,
}

#[derive(Serialize, Deserialize)]
pub struct ProjectState {
    pub path: PathBuf,
    pub logo: u32,
    /// The drawing older versions picked by number. Still written, as 0, so
    /// they can read this file; `icon` replaced it.
    #[serde(default)]
    pub art: u8,
    /// The 8×8 icon; missing in files from before it, which get one drawn
    /// from the path.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icon: Option<u64>,
    /// Shell command run in a terminal when a new environment is created.
    #[serde(default)]
    pub setup: Option<String>,
    /// Shell command run after agent turns that edit files.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub check: Option<String>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub check_declined: bool,
    /// Its agents run in a sandbox.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub sandbox: bool,
    /// The branch new environments start from; None for origin's default branch.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base: Option<String>,
    #[serde(default)]
    pub notes: Vec<String>,
    #[serde(default)]
    pub monos: Vec<Snippet>,
    /// Old files hold only name and body; the rest defaults to a manual command.
    #[serde(default)]
    pub automations: Vec<Automation>,
    /// The sidebar tree's order of its sessions, by id (`Project::session_order`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub session_order: Vec<String>,
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
    /// Devin settings for the next session, as (key, value).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub devin_settings: Vec<(String, String)>,
    /// Cursor settings for the next session, as (key, value).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub cursor_settings: Vec<(String, String)>,
    /// Antigravity settings for the next session, as (key, value).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub antigravity_settings: Vec<(String, String)>,
    /// Workers AI settings for the next session, as (key, value).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub workers_ai_settings: Vec<(String, String)>,
    #[serde(default)]
    pub sessions: Vec<SessionState>,
    /// Ids of the sessions open as tabs, in order.
    #[serde(default)]
    pub open: Vec<String>,
    /// Those of them pinned to the title bar. Missing in files from before
    /// pins: every open session counts as pinned, so none disappears.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pinned: Option<Vec<String>>,
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
    /// Unix seconds it was last on screen; 0 in files from before it was kept.
    #[serde(default, skip_serializing_if = "is_zero_u64")]
    pub used: u64,
}

fn is_zero_u64(n: &u64) -> bool {
    *n == 0
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
    /// The other pane's environment, when it is not this one: a split can show
    /// the tabs of two environments, and is kept with its focused pane's.
    /// `tab` then counts among that environment's tabs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub env: Option<PathBuf>,
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
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub settled: bool,
    /// It finished a turn the user has not looked at yet.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub unread: bool,
}

fn is_session(t: &Tab) -> bool {
    matches!(t.kind, TabKind::Session { .. })
}

fn is_saved_browser(t: &Tab) -> bool {
    matches!(&t.kind, TabKind::Browser { url } if !matches!(url.as_ref(), "" | "about:blank"))
}

/// The tabs of one environment, in the order the strip shows them. The file
/// keeps them per environment, as it did when every environment had its own tabs.
fn tabs_of<'a>(strip: &'a TabStrip, e: &Environment) -> Vec<&'a Tab> {
    tabs_in(strip, &e.path)
}

fn tabs_in<'a>(strip: &'a TabStrip, env: &Path) -> Vec<&'a Tab> {
    strip.tabs.iter().filter(|t| t.is_in(env)).collect()
}

/// Position of the environment's front tab among its session tabs (0 when it
/// is another tab). With a split put aside, the tab of its focused pane counts:
/// the split is saved against it.
fn active_session_tab(strip: &TabStrip, e: &Environment) -> usize {
    let tabs = tabs_of(strip, e);
    let parked = strip
        .parked
        .get(&e.path)
        .filter(|_| strip.split_env.as_deref() != Some(&e.path));
    let front = match parked {
        Some((_, focus)) => Some(*focus),
        None => strip.front_of(&e.path).map(|i| strip.tabs[i].id),
    };
    match tabs.iter().position(|t| Some(t.id) == front) {
        Some(i) if is_session(tabs[i]) => tabs[..i].iter().filter(|t| is_session(t)).count(),
        _ => 0,
    }
}

/// Position of a tab among its environment's tabs that reopen after a restart,
/// in their reopened order.
fn reopened_index(strip: &TabStrip, env: &Path, id: u64) -> Option<usize> {
    let tabs = tabs_in(strip, env);
    let tab = tabs.iter().position(|t| t.id == id)?;
    let before = &tabs[..tab];
    if is_session(tabs[tab]) {
        Some(before.iter().filter(|t| is_session(t)).count())
    } else if is_saved_browser(tabs[tab]) {
        let sessions = tabs.iter().filter(|t| is_session(t)).count();
        Some(sessions + before.iter().filter(|t| is_saved_browser(t)).count())
    } else {
        None
    }
}

/// What names a tab that reopens after a restart, across the whole strip: a
/// session by its id, a browser tab by its environment and its place among
/// that environment's saved pages. `None` for a tab that does not reopen.
fn tab_key(strip: &TabStrip, i: usize) -> Option<String> {
    let tab = &strip.tabs[i];
    match &tab.kind {
        TabKind::Session { id, .. } => Some(format!("session:{id}")),
        _ if is_saved_browser(tab) => {
            let env = tab.env.as_deref()?;
            let n = strip.tabs[..i]
                .iter()
                .filter(|t| t.is_in(env) && is_saved_browser(t))
                .count();
            Some(format!("browser:{n}:{}", env.display()))
        }
        _ => None,
    }
}

fn tab_order(strip: &TabStrip) -> Vec<String> {
    (0..strip.tabs.len())
        .filter_map(|i| tab_key(strip, i))
        .collect()
}

/// Puts the reopened tabs back in the order `order` saved. Tabs it does not
/// name keep their place among each other, after the ones it does.
fn apply_tab_order(strip: &mut TabStrip, order: &[String]) {
    if order.is_empty() {
        return;
    }
    let rank: Vec<usize> = (0..strip.tabs.len())
        .map(|i| {
            tab_key(strip, i)
                .and_then(|k| order.iter().position(|o| *o == k))
                .unwrap_or(usize::MAX)
        })
        .collect();
    let mut tabs: Vec<(usize, Tab)> = rank.into_iter().zip(strip.tabs.drain(..)).collect();
    tabs.sort_by_key(|(r, _)| *r);
    strip.tabs = tabs.into_iter().map(|(_, t)| t).collect();
}

/// The split, kept with the environment of its focused pane.
fn split_state(strip: &TabStrip, e: &Environment) -> Option<SplitState> {
    let split = if strip.split_env.as_deref() == Some(&e.path) {
        strip.split_other()?;
        strip.split?
    } else {
        // Not on screen: its split waits with the strip.
        strip.parked.get(&e.path)?.0
    };
    let other_env = strip.tabs[strip.position(split.other)?].env.as_deref()?;
    Some(SplitState {
        tab: reopened_index(strip, other_env, split.other)?,
        env: (other_env != e.path).then(|| other_env.to_path_buf()),
        down: split.dir == SplitDir::Down,
        ratio: split.ratio,
        focus_first: split.focus_first,
    })
}

pub fn transcript_path(id: &str) -> PathBuf {
    data_dir().join("sessions").join(format!("{id}.json"))
}

/// Where a session is written out as markdown for another agent to read: when
/// it is linked into a draft, or when another agent continues it.
pub fn linked_path(id: &str) -> PathBuf {
    data_dir().join("linked").join(format!("{id}.md"))
}

/// The files a session leaves in the data dir.
pub fn session_files(id: &str) -> [PathBuf; 2] {
    [transcript_path(id), linked_path(id)]
}

/// How long the files of a session its state stopped listing are kept,
/// counted from the first startup that found it gone. Removing the session
/// deletes them at once; the sweep is for the few that missed it, such as a
/// worktree removed outside Arakun.
const ORPHAN_AGE: u64 = 7 * 24 * 60 * 60;

/// The sessions a state was seen listing, kept next to it: None while it
/// still lists one, else when (Unix seconds) a startup first found it gone.
type Seen = std::collections::BTreeMap<String, Option<u64>>;

/// Where the sessions a state was seen listing are kept: next to it, named
/// after its whole file name, so `work.json` and `work.backup` don't share.
fn seen_path() -> PathBuf {
    let state = file();
    let name = state.file_name().unwrap_or_default().to_string_lossy();
    state.with_file_name(format!("{name}.seen"))
}

/// Deletes the transcripts and linked notes of sessions this state listed
/// before and has not listed for `ORPHAN_AGE`, such as those of an
/// environment whose worktree was removed outside Arakun.
///
/// Only sessions this state was seen listing count. Another Arakun with its
/// own state (`ARAKUN_STATE`), of any version, keeps its sessions in the same
/// folder, and a file whose owner was never seen is never deleted. `listed`
/// is what the state held when Arakun loaded it. Skipped when the state or
/// the seen list can't be read, since every file would look unlisted.
pub fn sweep_orphans(listed: std::collections::HashSet<String>) {
    let parses = std::fs::read(file())
        .ok()
        .is_some_and(|b| serde_json::from_slice::<State>(&b).is_ok());
    if !parses {
        return;
    }
    let seen_path = seen_path();
    let seen: Seen = match std::fs::read(&seen_path) {
        Ok(bytes) => match serde_json::from_slice(&bytes) {
            Ok(seen) => seen,
            Err(_) => return,
        },
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Seen::new(),
        Err(_) => return,
    };
    let dirs = [
        (data_dir().join("sessions"), "json"),
        (data_dir().join("linked"), "md"),
    ];
    let seen = sweep(&dirs, seen, &listed, crate::model::unix_now());
    if let Err(e) = crate::durable::save_json(&seen_path, &seen) {
        eprintln!("arakun: could not save the sessions seen listed: {e}");
    }
}

/// Deletes the files in `dirs` (`<id>.<ext>`) of the sessions `seen` holds
/// that `listed` has not held for `ORPHAN_AGE`, and returns the seen list for
/// next time. A session whose files can't be checked or deleted, or whose
/// headless folder may still be there, stays for the next startup to retry.
fn sweep(
    dirs: &[(PathBuf, &str)],
    mut seen: Seen,
    listed: &std::collections::HashSet<String>,
    now: u64,
) -> Seen {
    for id in listed {
        seen.insert(id.clone(), None);
    }
    seen.retain(|id, gone| {
        if listed.contains(id) {
            return true;
        }
        let since = *gone.get_or_insert(now);
        if now.saturating_sub(since) <= ORPHAN_AGE {
            return true;
        }
        // A headless session's own folder keeps it, listed or not.
        if !matches!(crate::headless::dir(id).try_exists(), Ok(false)) {
            return true;
        }
        let mut kept = false;
        for (dir, ext) in dirs {
            match std::fs::remove_file(dir.join(format!("{id}.{ext}"))) {
                Err(e) if e.kind() != std::io::ErrorKind::NotFound => kept = true,
                _ => {}
            }
        }
        kept
    });
    seen
}

/// Sessions `arakun restart` cut off mid-turn, for the next arakun to reopen.
pub fn resume_path() -> PathBuf {
    file().with_extension("resume.json")
}

/// Next to the state, so a second Arakun with its own state (`ARAKUN_STATE`)
/// has its own first launch too.
pub fn onboarding_path() -> PathBuf {
    file().with_extension("onboarding.json")
}

/// Held by the one Arakun that loads and saves this state; next to it, so a
/// second Arakun with its own state (`ARAKUN_STATE`) runs beside it.
pub fn lock_path() -> PathBuf {
    lock_for(&file())
}

/// `state.json.lock`: the whole name, so `a.json` and `a.alt` get two locks.
fn lock_for(state: &Path) -> PathBuf {
    let mut name = state.as_os_str().to_owned();
    name.push(".lock");
    PathBuf::from(name)
}

/// Whether Arakun saved its state here before: not a first launch.
pub fn exists() -> bool {
    file().exists()
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
    let path = file();
    let fresh = !path.exists();
    State {
        fresh,
        ..load_from(&path)
    }
}

/// A file this build can't read (say, from a newer Arakun that knows more
/// agents) is set aside, not saved over on quit.
fn load_from(path: &Path) -> State {
    crate::stored::load(path).unwrap_or(State {
        dock_open: true,
        dock_height: DOCK_HEIGHT,
        rail_open: true,
        panel_open: true,
        rail_w: RAIL_W,
        panel_w: PANEL_W,
        arcade: true,
        ..Default::default()
    })
}

/// The newest state snapshot not yet on disk. Saves are written off the UI
/// thread, on whichever thread is free; each takes the newest snapshot when
/// its turn comes, so an older one can never be renamed over a newer one.
static SAVES: Saves = Saves::new();

/// Queues `bytes` (on the UI thread, where the state changes) in place of
/// any older snapshot not written yet.
pub fn queue(bytes: Vec<u8>) {
    SAVES.queue(bytes);
}

/// Writes the newest queued snapshot, if a save before this one has not.
/// A state.json that could not be read on launch is set aside first, or
/// kept and the save fails.
pub fn write_queued() -> std::io::Result<()> {
    SAVES.write(|bytes| crate::stored::write(&file(), bytes))
}

struct Saves {
    pending: std::sync::Mutex<Option<Vec<u8>>>,
    writing: std::sync::Mutex<()>,
}

impl Saves {
    const fn new() -> Self {
        Saves {
            pending: std::sync::Mutex::new(None),
            writing: std::sync::Mutex::new(()),
        }
    }

    fn pending(&self) -> std::sync::MutexGuard<'_, Option<Vec<u8>>> {
        self.pending
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn queue(&self, bytes: Vec<u8>) {
        *self.pending() = Some(bytes);
    }

    fn write(&self, write: impl FnOnce(&[u8]) -> std::io::Result<()>) -> std::io::Result<()> {
        // One write at a time: the snapshot taken here is the newest, and
        // a newer one queued meanwhile waits and is written after it.
        let _one_at_a_time = self
            .writing
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let Some(bytes) = self.pending().take() else {
            return Ok(());
        };
        let written = write(&bytes);
        if written.is_err() {
            // Kept for the next save to try, unless a newer one came.
            self.pending().get_or_insert(bytes);
        }
        written
    }
}

/// [`snapshot_with`] for projects that have no tabs open.
#[cfg(test)]
pub fn snapshot(projects: &[Project], active_project: usize, layout: Layout) -> Vec<u8> {
    snapshot_with(projects, &TabStrip::default(), active_project, layout)
}

pub fn snapshot_with(
    projects: &[Project],
    strip: &TabStrip,
    active_project: usize,
    layout: Layout,
) -> Vec<u8> {
    // Headless sessions are listed from their own folder, not saved here;
    // their project stays once it holds sessions the app took over.
    // A server's projects and folders come from the server each time.
    let saved = |p: &&Project| {
        !p.is_remote()
            && (p.path != crate::headless::root()
                || p.environments
                    .iter()
                    .flat_map(|e| &e.sessions)
                    .any(|s| !crate::headless::shown(&s.id)))
    };
    let kept = projects.iter().filter(saved).count();
    // The index is into the saved list, so skipped projects before the active
    // one must not count. Local projects added later sit behind remote ones.
    let active_saved = projects
        .iter()
        .take(active_project)
        .filter(saved)
        .count()
        .min(kept.saturating_sub(1));
    let state = State {
        projects: projects
            .iter()
            .filter(saved)
            .map(|p| ProjectState {
                path: p.path.clone(),
                logo: p.logo,
                art: 0,
                icon: Some(p.icon),
                setup: p.setup.clone(),
                check: p.check.clone(),
                check_declined: p.check_declined,
                sandbox: p.sandbox,
                base: p.base.clone(),
                notes: p.notes.clone(),
                monos: p.monos.clone(),
                automations: p.automations.clone(),
                session_order: session_order(p),
                #[cfg(feature = "coderlabs")]
                coderspace: p.coderspace.clone(),
                active_env: p
                    .environments
                    .get(p.active_env)
                    .filter(|e| e.remote.is_none())
                    .map(|e| e.path.clone()),
                envs: p
                    .environments
                    .iter()
                    .filter(|e| e.remote.is_none())
                    .map(|e| EnvState {
                        path: e.path.clone(),
                        branch: Some(e.branch.to_string()),
                        is_main: e.is_checkout,
                        placeholder: e.placeholder.clone(),
                        follow_branch: e.follow_branch,
                        used: e.used,
                        agent: Some(e.agent.id().into()),
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
                        devin_settings: e
                            .devin_settings
                            .iter()
                            .map(|(s, v)| (s.key().to_string(), v.clone()))
                            .collect(),
                        cursor_settings: e
                            .cursor_settings
                            .iter()
                            .map(|(s, v)| (s.key().to_string(), v.clone()))
                            .collect(),
                        antigravity_settings: e
                            .antigravity_settings
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
                                agent: s.agent.id().into(),
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
                                settled: s.settled,
                                unread: s.unread,
                            })
                            .collect(),
                        open: tabs_of(strip, e)
                            .into_iter()
                            .filter_map(|t| match &t.kind {
                                TabKind::Session { id, .. } => Some(id.to_string()),
                                _ => None,
                            })
                            .collect(),
                        pinned: Some(
                            tabs_of(strip, e)
                                .into_iter()
                                .filter(|t| t.pinned)
                                .filter_map(|t| t.session_id().map(|id| id.to_string()))
                                .collect(),
                        ),
                        active_tab: active_session_tab(strip, e),
                        browsers: tabs_of(strip, e)
                            .into_iter()
                            .filter_map(|t| match &t.kind {
                                // Blank tabs are not worth reopening.
                                TabKind::Browser { url } if is_saved_browser(t) => {
                                    Some(url.to_string())
                                }
                                _ => None,
                            })
                            .collect(),
                        split: split_state(strip, e),
                        draft: e.draft.to_string(),
                        caret: e.draft.head(),
                        images: e.draft_images.clone(),
                    })
                    .collect(),
            })
            .collect(),
        active_project: active_saved,
        dock_open: layout.dock_open,
        dock_height: layout.dock_height,
        rail_open: layout.rail_open,
        panel_open: layout.panel_open,
        rail_w: layout.rail_w,
        panel_w: layout.panel_w,
        arcade: layout.arcade,
        diff_palette: layout.diff_palette.id().to_string(),
        glass: layout.glass,
        contrast: layout.contrast.id().to_string(),
        grays: layout.grays.id().to_string(),
        type_scale: layout.type_scale.id().to_string(),
        rail_hidden: [
            ("claude", layout.rail_usage[0]),
            ("droid", layout.rail_usage[1]),
            ("opencode", layout.rail_usage[2]),
            ("devin", layout.rail_usage[3]),
        ]
        .into_iter()
        .filter(|(_, shown)| !shown)
        .map(|(k, _)| k.to_string())
        .collect(),
        ui_zoom: layout.ui_zoom,
        last_agent: crate::model::default_agent().id().to_string(),
        island_off: !crate::island::enabled(),
        reduce_motion: crate::ui::motion::choice().id().to_string(),
        background: crate::background::enabled(),
        window: layout.window,
        tab_order: tab_order(strip),
        tabs: Some(crate::tab_layout::current()),
        fresh: false,
    };
    serde_json::to_vec_pretty(&state).unwrap_or_default()
}

pub fn restore(state: State) -> (Vec<Project>, usize, Layout, TabStrip) {
    let mut strip = TabStrip::default();
    // The split each environment was saved with, and the tab of its focused
    // pane; the one on screen gets its own back, the others wait. The other
    // pane's tab is found once every environment's tabs are in.
    let mut saved_splits: Vec<(PathBuf, SplitState, u64)> = Vec::new();
    let last_agent = state.last_agent.clone();
    crate::island::set_enabled(!state.island_off);
    crate::background::set_enabled(state.background);
    // A new install starts on the sidebar tree, which needs a wider sidebar.
    let fresh = state.fresh && state.tabs.is_none();
    crate::tab_layout::set(
        state
            .tabs
            .unwrap_or_else(|| crate::tab_layout::TabLayout::for_state_without_it(!state.fresh)),
    );
    crate::ui::motion::restore(crate::ui::motion::Reduce::from_id(&state.reduce_motion));
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
                    // Unknown in an older file: counted from now, so nothing
                    // looks old the first time the cleanup runs.
                    if e.used > 0 {
                        env.used = e.used;
                    }
                    env.set_branch(e.branch);
                    if let Some(agent) = e.agent {
                        env.agent = AgentKind::from_id(&agent);
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
                    env.devin_settings = e
                        .devin_settings
                        .into_iter()
                        .filter_map(|(k, v)| Setting::from_key(&k).map(|s| (s, v)))
                        .collect();
                    env.cursor_settings = e
                        .cursor_settings
                        .into_iter()
                        .filter_map(|(k, v)| Setting::from_key(&k).map(|s| (s, v)))
                        .collect();
                    env.antigravity_settings = e
                        .antigravity_settings
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
                                AgentKind::from_id(&s.agent),
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
                            session.settled = s.settled;
                            session.unread = s.unread;
                            session.status = Status::Idle;
                            session
                        })
                        .collect();
                    let mut tabs: Vec<Tab> = e
                        .open
                        .iter()
                        .filter_map(|id| {
                            let s = &env.sessions[env.session_index(id)?];
                            let mut tab = Tab::of(
                                &env.path,
                                s.title.clone(),
                                TabKind::Session {
                                    agent: s.agent,
                                    id: s.id.clone(),
                                },
                            );
                            tab.pinned = e.pinned.as_ref().is_none_or(|p| p.contains(id));
                            Some(tab)
                        })
                        .collect();
                    let sessions_open = tabs.len();
                    tabs.extend(e.browsers.into_iter().map(|url| {
                        let title = if url.is_empty() {
                            "New tab".to_string()
                        } else {
                            url.clone()
                        };
                        Tab::of(&env.path, title, TabKind::Browser { url: url.into() })
                    }));
                    let front = if sessions_open > 0 {
                        e.active_tab.min(sessions_open - 1)
                    } else {
                        0
                    };
                    if let Some(tab) = tabs.get(front) {
                        strip.front.insert(env.path.clone(), tab.id);
                    }
                    if let Some(s) = e.split
                        && let Some(focus) = tabs.get(front)
                    {
                        saved_splits.push((env.path.clone(), s, focus.id));
                    }
                    strip.tabs.extend(tabs);
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
                icon: p.icon.unwrap_or_else(|| crate::model::logo_for(&p.path).1),
                path: p.path,
                logo: crate::model::current_logo(p.logo),
                environments,
                active_env,
                setup: p.setup,
                check: p.check,
                check_declined: p.check_declined,
                sandbox: p.sandbox,
                base: p.base,
                notes: p.notes,
                monos: p.monos,
                automations: p.automations,
                session_order: p.session_order.into_iter().map(Into::into).collect(),
                #[cfg(feature = "coderlabs")]
                coderspace: p.coderspace,
            }
        })
        .collect();
    // Before `apply_tab_order`: each environment's tabs are still in their
    // reopened order, which `SplitState::tab` counts in.
    let splits: Vec<(PathBuf, Split, u64)> = saved_splits
        .into_iter()
        .filter_map(|(path, s, focus)| {
            let other_env = s.env.as_deref().unwrap_or(&path);
            let other = tabs_in(&strip, other_env).get(s.tab)?.id;
            (other != focus).then(|| {
                let split = Split {
                    other,
                    dir: if s.down {
                        SplitDir::Down
                    } else {
                        SplitDir::Right
                    },
                    ratio: s.ratio.clamp(Split::MIN_RATIO, Split::MAX_RATIO),
                    focus_first: s.focus_first,
                };
                (path, split, focus)
            })
        })
        .collect();
    apply_tab_order(&mut strip, &state.tab_order);
    let active = state.active_project.min(projects.len().saturating_sub(1));
    // The strip opens on the tab of the environment that was on screen.
    if let Some(shown) = projects
        .get(active)
        .and_then(|p| p.environments.get(p.active_env))
    {
        strip.active_tab = strip
            .front_of(&shown.path)
            .unwrap_or_else(|| strip.add(Tab::new_session(&shown.path)));
        for (path, split, focus) in splits {
            if path == shown.path {
                strip.split = Some(split);
            } else {
                strip.parked.insert(path, (split, focus));
            }
        }
        strip.split_env = Some(shown.path.clone());
        strip.seed_tab_history();
    }
    let layout = Layout {
        dock_open: state.dock_open,
        dock_height: state.dock_height,
        rail_open: state.rail_open,
        panel_open: state.panel_open,
        rail_w: if fresh {
            state.rail_w.max(crate::tab_layout::SIDEBAR_W)
        } else {
            state.rail_w
        },
        panel_w: state.panel_w,
        arcade: state.arcade,
        diff_palette: crate::theme::DiffPalette::from_id(&state.diff_palette),
        glass: state.glass,
        contrast: crate::theme::Contrast::from_id(&state.contrast),
        grays: crate::theme::Grays::from_id(&state.grays),
        type_scale: crate::ui::TypeScale::from_id(&state.type_scale),
        rail_usage: ["claude", "droid", "opencode", "devin"]
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
        (false, _) => crate::model::set_default_agent(AgentKind::from_id(&last_agent)),
        (true, Some(agent)) => crate::model::set_default_agent(agent),
        (true, None) => {}
    }
    (projects, active, layout, strip)
}

pub fn new_project(path: &Path) -> Project {
    let (logo, icon) = crate::model::logo_for(path);
    Project {
        name: crate::model::dir_name(path).into(),
        path: path.to_path_buf(),
        logo,
        icon,
        environments: vec![Environment::new(path.to_path_buf(), None, true)],
        active_env: 0,
        setup: None,
        check: None,
        check_declined: false,
        sandbox: false,
        base: None,
        notes: Vec::new(),
        monos: Vec::new(),
        automations: Vec::new(),
        session_order: Vec::new(),
        #[cfg(feature = "coderlabs")]
        coderspace: None,
    }
}

/// The project's order of its sessions as saved: only sessions it still
/// has, so the list doesn't grow with every session ever deleted.
fn session_order(p: &Project) -> Vec<String> {
    let has = |id: &gpui::SharedString| {
        p.environments
            .iter()
            .any(|e| e.sessions.iter().any(|s| &s.id == id))
    };
    p.session_order
        .iter()
        .filter(|id| has(id))
        .map(|id| id.to_string())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(
        disk: &std::sync::Mutex<Vec<Vec<u8>>>,
        fail: bool,
    ) -> impl FnOnce(&[u8]) -> std::io::Result<()> + '_ {
        move |bytes| {
            if fail {
                return Err(std::io::Error::other("disk full"));
            }
            disk.lock().unwrap().push(bytes.to_vec());
            Ok(())
        }
    }

    #[test]
    fn only_the_newest_queued_snapshot_is_written() {
        let saves = Saves::new();
        let disk = std::sync::Mutex::new(Vec::new());
        saves.queue(b"older".to_vec());
        saves.queue(b"newer".to_vec());
        // Two save tasks ran; the first takes the newest, the second finds
        // nothing left.
        saves.write(record(&disk, false)).unwrap();
        saves.write(record(&disk, false)).unwrap();
        assert_eq!(*disk.lock().unwrap(), [b"newer".to_vec()]);
    }

    #[test]
    fn a_failed_save_is_kept_for_the_next_one() {
        let saves = Saves::new();
        let disk = std::sync::Mutex::new(Vec::new());
        saves.queue(b"state".to_vec());
        assert!(saves.write(record(&disk, true)).is_err());
        saves.write(record(&disk, false)).unwrap();
        assert_eq!(*disk.lock().unwrap(), [b"state".to_vec()]);
        // A newer snapshot queued after a failure replaces the failed one.
        saves.queue(b"old".to_vec());
        assert!(saves.write(record(&disk, true)).is_err());
        saves.queue(b"new".to_vec());
        saves.write(record(&disk, false)).unwrap();
        assert_eq!(disk.lock().unwrap().last().unwrap(), b"new");
    }

    #[test]
    fn saves_from_many_threads_end_with_the_newest() {
        let saves = Saves::new();
        let disk = std::sync::Mutex::new(Vec::new());
        std::thread::scope(|scope| {
            for n in 0..32u8 {
                saves.queue(vec![n]);
                let (saves, disk) = (&saves, &disk);
                scope.spawn(move || saves.write(record(disk, false)).unwrap());
            }
        });
        let disk = disk.into_inner().unwrap();
        assert_eq!(disk.last().unwrap(), &[31]);
        assert!(disk.windows(2).all(|w| w[0] < w[1]), "{disk:?}");
    }

    #[test]
    fn a_settled_session_stays_settled() {
        let old: SessionState =
            serde_json::from_str(r#"{"id":"s1","title":"T","agent":"droid","updated":1}"#).unwrap();
        assert!(!old.settled, "older state files have nothing settled");
        assert!(serde_json::to_value(&old).unwrap().get("settled").is_none());
        let json = r#"{"id":"s1","title":"T","agent":"droid","updated":1,"settled":true}"#;
        let s: SessionState = serde_json::from_str(json).unwrap();
        assert!(s.settled);
        assert_eq!(serde_json::to_value(&s).unwrap()["settled"], true);
    }

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
    fn only_sessions_unlisted_for_a_week_are_swept() {
        let dir = std::env::temp_dir().join(format!("arakun-sweep-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let (sessions, linked) = (dir.join("sessions"), dir.join("linked"));
        for d in [&sessions, &linked] {
            std::fs::create_dir_all(d).unwrap();
        }
        for name in [
            "listed.json",
            "removed.json",
            "recent.json",
            "headless-run.json",
            "theirs.json",
        ] {
            std::fs::write(sessions.join(name), "{}").unwrap();
        }
        std::fs::write(linked.join("removed.md"), "").unwrap();
        let headless = dir.join("headless");
        std::fs::create_dir_all(headless.join("headless-run")).unwrap();
        crate::headless::TEST_ROOT.with(|r| *r.borrow_mut() = Some(headless));

        let now = 100 * ORPHAN_AGE;
        let long_ago = Some(now - ORPHAN_AGE - 1);
        let seen = Seen::from([
            ("listed".into(), None),
            ("removed".into(), long_ago),
            ("recent".into(), Some(now - 60)),
            ("headless-run".into(), long_ago),
            // Listed at the last startup: gone from now on, kept a week.
            ("dropped".into(), None),
        ]);
        let listed = ["listed", "new"].map(String::from).into();
        let dirs = [(sessions.clone(), "json"), (linked.clone(), "md")];
        let kept = sweep(&dirs, seen, &listed, now);
        crate::headless::TEST_ROOT.with(|r| *r.borrow_mut() = None);

        assert_eq!(
            kept,
            Seen::from([
                ("listed".into(), None),
                ("new".into(), None),
                ("recent".into(), Some(now - 60)),
                ("headless-run".into(), long_ago),
                ("dropped".into(), Some(now)),
            ])
        );
        let mut left: Vec<_> = [&sessions, &linked]
            .iter()
            .flat_map(|d| std::fs::read_dir(d).unwrap())
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        left.sort();
        // Another state's session, never seen here, stays whatever its age.
        assert_eq!(
            left,
            [
                "headless-run.json",
                "listed.json",
                "recent.json",
                "theirs.json"
            ]
        );
        let _ = std::fs::remove_dir_all(&dir);
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
            arcade: false,
            diff_palette: crate::theme::DiffPalette::Default,
            glass: false,
            contrast: crate::theme::Contrast::System,
            grays: crate::theme::Grays::Crisp,
            type_scale: crate::ui::TypeScale::Compact,
            rail_usage: [true, true, true, true],
            ui_zoom: 100,
            window: None,
        };
        let (_, _, layout, _) = restore(serde_json::from_slice(&snapshot(&[], 0, layout)).unwrap());
        assert!(!layout.rail_open && !layout.panel_open);
    }

    #[test]
    fn active_project_counts_only_saved_projects() {
        let local = |n: &str| new_project(Path::new(&format!("/tmp/arakun-active-{n}")));
        let remote = |k: &str| new_project(&crate::model::Remote::project_path(k));
        // A local project added after the servers' ones sits behind them.
        let projects = [
            local("l0"),
            remote("r1"),
            remote("r2"),
            local("l3"),
            local("l4"),
        ];
        let layout = Layout {
            dock_open: false,
            dock_height: DOCK_HEIGHT,
            rail_open: true,
            panel_open: true,
            rail_w: RAIL_W,
            panel_w: PANEL_W,
            arcade: false,
            diff_palette: crate::theme::DiffPalette::Default,
            glass: false,
            contrast: crate::theme::Contrast::System,
            grays: crate::theme::Grays::Crisp,
            type_scale: crate::ui::TypeScale::Compact,
            rail_usage: [true, true, true, true],
            ui_zoom: 100,
            window: None,
        };
        let state: State = serde_json::from_slice(&snapshot(&projects, 3, layout)).unwrap();
        assert_eq!(state.projects.len(), 3);
        assert_eq!(
            state.projects[state.active_project].path,
            Path::new("/tmp/arakun-active-l3")
        );
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
            arcade: false,
            diff_palette: crate::theme::DiffPalette::Default,
            glass: false,
            contrast: crate::theme::Contrast::System,
            grays: crate::theme::Grays::Soft,
            type_scale: crate::ui::TypeScale::Comfortable,
            rail_usage: [true, false, true, false],
            ui_zoom: 100,
            window: None,
        };
        let (_, _, layout, _) = restore(serde_json::from_slice(&snapshot(&[], 0, layout)).unwrap());
        assert_eq!(layout.rail_usage, [true, false, true, false]);
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
            arcade: false,
            diff_palette: crate::theme::DiffPalette::Default,
            glass: false,
            contrast: crate::theme::Contrast::System,
            grays: crate::theme::Grays::Crisp,
            type_scale: crate::ui::TypeScale::Compact,
            rail_usage: [true, true, true, true],
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
            arcade: false,
            diff_palette: crate::theme::DiffPalette::Default,
            glass: false,
            contrast: crate::theme::Contrast::System,
            grays: crate::theme::Grays::Crisp,
            type_scale: crate::ui::TypeScale::Compact,
            rail_usage: [true, true, true, true],
            ui_zoom: 100,
            window: None,
        };
        let bytes = snapshot(std::slice::from_ref(&project), 0, layout);
        let (mut projects, _, _, _) = restore(serde_json::from_slice(&bytes).unwrap());
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
            arcade: false,
            diff_palette: crate::theme::DiffPalette::Default,
            glass: false,
            contrast: crate::theme::Contrast::System,
            grays: crate::theme::Grays::Crisp,
            type_scale: crate::ui::TypeScale::Compact,
            rail_usage: [true, true, true, true],
            ui_zoom: 100,
            window: None,
        };
        let bytes = snapshot(std::slice::from_ref(&project), 0, layout);
        let (projects, _, _, _) = restore(serde_json::from_slice(&bytes).unwrap());
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
    fn tab_order_survives_a_restart() {
        let mut project = new_project(Path::new("/tmp/arakun-order-test"));
        let a = PathBuf::from("/tmp/arakun-order-test");
        let b = PathBuf::from("/tmp/arakun-order-test-2");
        project.environments[0].sessions = vec![
            Session::new("s1".into(), "One".into(), AgentKind::Droid),
            Session::new("s2".into(), "Two".into(), AgentKind::Droid),
        ];
        let mut other = Environment::new(b.clone(), None, false);
        other.sessions = vec![Session::new("s3".into(), "Three".into(), AgentKind::Droid)];
        project.environments.push(other);
        let session = |env: &Path, id: &str| {
            Tab::of(
                env,
                id.to_string(),
                TabKind::Session {
                    agent: AgentKind::Droid,
                    id: id.to_string().into(),
                },
            )
        };
        let page = Tab::of(
            &a,
            "Docs",
            TabKind::Browser {
                url: "https://example.com".into(),
            },
        );
        // Mixed across environments, a page between two sessions: the
        // per-environment lists alone would put them back as s1 s2 docs s3.
        let mut strip = TabStrip {
            tabs: vec![
                session(&a, "s2"),
                session(&b, "s3"),
                page,
                session(&a, "s1"),
            ],
            ..Default::default()
        };
        strip.tabs[0].pinned = true;
        let layout = Layout {
            dock_open: false,
            dock_height: DOCK_HEIGHT,
            rail_open: true,
            panel_open: true,
            rail_w: RAIL_W,
            panel_w: PANEL_W,
            arcade: false,
            diff_palette: crate::theme::DiffPalette::Default,
            glass: false,
            contrast: crate::theme::Contrast::System,
            grays: crate::theme::Grays::Crisp,
            type_scale: crate::ui::TypeScale::Compact,
            rail_usage: [true, true, true, true],
            ui_zoom: 100,
            window: None,
        };
        let bytes = snapshot_with(std::slice::from_ref(&project), &strip, 0, layout);
        let (_, _, _, strip) = restore(serde_json::from_slice(&bytes).unwrap());
        let titles: Vec<&str> = strip.tabs.iter().map(|t| t.title.as_ref()).collect();
        assert_eq!(titles, ["Two", "Three", "https://example.com", "One"]);
        let pinned: Vec<bool> = strip.tabs.iter().map(|t| t.pinned).collect();
        assert_eq!(pinned, [true, false, false, false]);

        // A file from before the order was kept: each environment's tabs in turn.
        let mut old: State = serde_json::from_slice(&bytes).unwrap();
        old.tab_order.clear();
        for e in &mut old.projects[0].envs {
            e.pinned = None;
        }
        let (_, _, _, strip) = restore(old);
        let titles: Vec<&str> = strip.tabs.iter().map(|t| t.title.as_ref()).collect();
        assert_eq!(titles, ["Two", "One", "https://example.com", "Three"]);
        // From before pins, every open session stays in the bar.
        let pinned: Vec<bool> = strip.tabs.iter().map(|t| t.pinned).collect();
        assert_eq!(pinned, [true, true, false, true]);
    }

    #[test]
    fn the_trees_manual_order_survives_a_restart_without_deleted_sessions() {
        let mut project = new_project(Path::new("/tmp/arakun-session-order-test"));
        project.environments[0].sessions = vec![
            Session::new("s1".into(), "One".into(), AgentKind::Droid),
            Session::new("s2".into(), "Two".into(), AgentKind::Droid),
        ];
        // "gone" was deleted after it was dragged.
        project.session_order = vec!["s2".into(), "gone".into(), "s1".into()];
        let layout = Layout {
            dock_open: false,
            dock_height: DOCK_HEIGHT,
            rail_open: true,
            panel_open: true,
            rail_w: RAIL_W,
            panel_w: PANEL_W,
            arcade: false,
            diff_palette: crate::theme::DiffPalette::Default,
            glass: false,
            contrast: crate::theme::Contrast::System,
            grays: crate::theme::Grays::Crisp,
            type_scale: crate::ui::TypeScale::Compact,
            rail_usage: [true, true, true, true],
            ui_zoom: 100,
            window: None,
        };
        let bytes = snapshot(std::slice::from_ref(&project), 0, layout.clone());
        let (projects, _, _, _) = restore(serde_json::from_slice(&bytes).unwrap());
        let order: Vec<&str> = projects[0]
            .session_order
            .iter()
            .map(|id| id.as_ref())
            .collect();
        assert_eq!(order, ["s2", "s1"]);
        // A project never dragged writes nothing, so older builds read the
        // file as before; and a file without the field reads as no order.
        project.session_order.clear();
        let bytes = snapshot(std::slice::from_ref(&project), 0, layout);
        assert!(
            !String::from_utf8(bytes.clone())
                .unwrap()
                .contains("session_order")
        );
        let (projects, _, _, _) = restore(serde_json::from_slice(&bytes).unwrap());
        assert!(projects[0].session_order.is_empty());
    }

    #[test]
    fn split_survives_a_restart() {
        let mut project = new_project(Path::new("/tmp/arakun-split-test"));
        let env = &mut project.environments[0];
        env.sessions = vec![Session::new("s1".into(), "One".into(), AgentKind::Droid)];
        let here = env.path.clone();
        let mut strip = TabStrip {
            tabs: vec![
                Tab::of(
                    &here,
                    "One",
                    TabKind::Session {
                        agent: AgentKind::Droid,
                        id: "s1".into(),
                    },
                ),
                Tab::of(&here, "New tab", TabKind::Browser { url: "".into() }),
                Tab::of(
                    &here,
                    "Docs",
                    TabKind::Browser {
                        url: "https://example.com".into(),
                    },
                ),
            ],
            ..Default::default()
        };
        strip.split = Some(Split {
            other: strip.tabs[2].id,
            dir: SplitDir::Down,
            ratio: 0.35,
            focus_first: true,
        });
        strip.split_env = Some(here.clone());

        let layout = Layout {
            dock_open: false,
            dock_height: DOCK_HEIGHT,
            rail_open: true,
            panel_open: true,
            rail_w: RAIL_W,
            panel_w: PANEL_W,
            arcade: false,
            diff_palette: crate::theme::DiffPalette::Default,
            glass: false,
            contrast: crate::theme::Contrast::System,
            grays: crate::theme::Grays::Crisp,
            type_scale: crate::ui::TypeScale::Compact,
            rail_usage: [true, true, true, true],
            ui_zoom: 100,
            window: None,
        };
        let bytes = snapshot_with(std::slice::from_ref(&project), &strip, 0, layout.clone());
        let (_, _, _, strip) = restore(serde_json::from_slice(&bytes).unwrap());
        // The blank browser tab is not reopened, so the docs tab lands at index 1.
        assert_eq!(strip.tabs.len(), 2);
        let split = strip.split.expect("split restored");
        assert_eq!(strip.split_other(), Some(1));
        assert_eq!(split.dir, SplitDir::Down);
        assert!(split.focus_first);
        assert!((split.ratio - 0.35).abs() < 1e-6);

        // Saved while another environment is on screen, it waits for its own.
        let mut strip = strip;
        let elsewhere = PathBuf::from("/tmp/arakun-split-elsewhere");
        project
            .environments
            .push(Environment::new(elsewhere.clone(), None, false));
        project.active_env = 1;
        let (focus, other) = (strip.tabs[0].id, strip.tabs[1].id);
        strip.bring_split(&elsewhere, Some(focus));
        assert!(strip.split.is_none());
        // The environment's front tab moved on since; the split keeps its own.
        strip.front.insert(here.clone(), other);
        let bytes = snapshot_with(std::slice::from_ref(&project), &strip, 0, layout);
        let (_, _, _, mut strip) = restore(serde_json::from_slice(&bytes).unwrap());
        assert!(strip.split.is_none(), "it is not the one on screen");
        strip.active_tab = strip.front_of(&here).unwrap();
        strip.bring_split(&here, None);
        assert_eq!((strip.active_tab, strip.split_other()), (0, Some(1)));
    }

    #[test]
    fn a_split_of_two_environments_survives_a_restart() {
        let mut project = new_project(Path::new("/tmp/arakun-split-two-test"));
        let a = project.environments[0].path.clone();
        let b = PathBuf::from("/tmp/arakun-split-two-test-2");
        project.environments[0].sessions = vec![
            Session::new("s1".into(), "One".into(), AgentKind::Droid),
            Session::new("s2".into(), "Two".into(), AgentKind::Droid),
        ];
        let mut other = Environment::new(b.clone(), None, false);
        other.sessions = vec![Session::new("s3".into(), "Three".into(), AgentKind::Droid)];
        project.environments.push(other);
        let session = |env: &Path, id: &str| {
            Tab::of(
                env,
                id.to_string(),
                TabKind::Session {
                    agent: AgentKind::Droid,
                    id: id.to_string().into(),
                },
            )
        };
        let mut strip = TabStrip {
            tabs: vec![session(&a, "s1"), session(&a, "s2"), session(&b, "s3")],
            ..Default::default()
        };
        let ids: Vec<u64> = strip.tabs.iter().map(|t| t.id).collect();
        strip.bring_split(&a, None);
        // B's tab dropped on the second pane, beside A's: B comes on screen.
        assert!(strip.place_tab(ids[2], false, |_| unreachable!()));
        strip.front.insert(a.clone(), ids[0]);
        strip.front.insert(b.clone(), ids[2]);
        project.active_env = 1;
        let layout = Layout {
            dock_open: false,
            dock_height: DOCK_HEIGHT,
            rail_open: true,
            panel_open: true,
            rail_w: RAIL_W,
            panel_w: PANEL_W,
            arcade: false,
            diff_palette: crate::theme::DiffPalette::Default,
            glass: false,
            contrast: crate::theme::Contrast::System,
            grays: crate::theme::Grays::Crisp,
            type_scale: crate::ui::TypeScale::Compact,
            rail_usage: [true, true, true, true],
            ui_zoom: 100,
            window: None,
        };
        let title = |strip: &TabStrip, i: usize| strip.tabs[i].title.to_string();
        let bytes = snapshot_with(std::slice::from_ref(&project), &strip, 0, layout.clone());
        let (_, _, _, mut strip) = restore(serde_json::from_slice(&bytes).unwrap());
        let split = strip.split.expect("split restored");
        let other = strip.split_other().expect("both panes show");
        assert_eq!(
            (title(&strip, strip.active_tab), title(&strip, other)),
            ("Three".into(), "One".into())
        );
        assert!(!split.focus_first, "B's tab is still on the right");
        assert_eq!(strip.split_env.as_deref(), Some(b.as_path()));

        // The focus goes to A's pane: the split stays, now A's.
        let prev = strip.active().map(|t| t.id);
        strip.active_tab = other;
        strip.bring_split(&a, prev);
        strip.settle_split(prev);
        project.active_env = 0;
        let bytes = snapshot_with(std::slice::from_ref(&project), &strip, 0, layout);
        let (_, _, _, strip) = restore(serde_json::from_slice(&bytes).unwrap());
        let split = strip.split.expect("split restored");
        let other = strip.split_other().expect("both panes show");
        assert_eq!(
            (title(&strip, strip.active_tab), title(&strip, other)),
            ("One".into(), "Three".into())
        );
        assert!(split.focus_first, "A's tab is still on the left");
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

    #[test]
    fn each_state_file_has_its_own_lock() {
        assert_eq!(
            lock_for(Path::new("/w/state.json")),
            PathBuf::from("/w/state.json.lock")
        );
        assert_ne!(
            lock_for(Path::new("/w/state.json")),
            lock_for(Path::new("/w/state.alt"))
        );
    }

    #[test]
    fn a_state_from_a_newer_arakun_is_kept_not_saved_over() {
        let dir = crate::stored::tests::scratch("state-newer");
        let path = dir.join("state.json");
        // A session whose agent this build doesn't know.
        let newer = r#"{"projects":[{"path":"/p","logo":0,"art":0,"envs":[{"path":"/p",
            "sessions":[{"id":"s1","title":"T","agent":"future","updated":1,
            "meta":{"agent":"future","session_id":null,"title":"T","cwd":"/p",
            "created":1,"updated":1}}]}]}]}"#;
        std::fs::write(&path, newer).unwrap();
        let state = load_from(&path);
        assert!(state.projects.is_empty());
        assert!(state.dock_open, "the defaults of a first launch");
        // The queued save on quit writes a new file; the old one is still there.
        let saves = Saves::new();
        saves.queue(b"{}".to_vec());
        saves
            .write(|bytes| crate::stored::write(&path, bytes))
            .unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"{}");
        assert_eq!(crate::stored::tests::kept(&path).as_deref(), Some(newer));
        let _ = std::fs::remove_dir_all(dir);
    }
}
