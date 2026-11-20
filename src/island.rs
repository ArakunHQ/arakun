//! The Dynamic Island: a black shape that grows out of the top bezel, with the
//! raccoon as its face: a Wayland layer on Linux, a panel over the menu bar on
//! macOS (`island/mac.rs`), where it grows out of the notch or the screen's own
//! Dynamic Island when there is one (`island/edge.rs`). It shows what the agents of
//! every project do: asleep when nothing ran for a while, a dot per busy
//! session, a card when one finishes, needs an answer or fails, and the list
//! of them on hover. It runs in a process of its own (`island_host.rs`), which
//! the app (`shell/island.rs`) and headless agents feed; `examples/island_demo.rs`
//! drives the same view with made-up agents.

// The island needs a Wayland layer shell or macOS: elsewhere it builds but never runs.
#![cfg_attr(
    not(any(target_os = "linux", target_os = "macos")),
    allow(dead_code, unused_imports)
)]

// Named by path: `examples/island_demo.rs` includes this file from another folder.
#[path = "island/edge.rs"]
mod edge;
#[cfg(target_os = "macos")]
#[path = "island/mac.rs"]
mod mac;

pub use edge::Edge;

use std::collections::VecDeque;
use std::f32::consts::{FRAC_PI_2, PI, TAU};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, LazyLock};
use std::time::{Duration, Instant};

use gpui::{
    AnimationExt as _, App, Bounds, Context, DispatchPhase, Entity, EventEmitter, FontWeight, Hsla,
    IntoElement, MouseButton, MouseExitEvent, MouseMoveEvent, ParentElement as _, PathBuilder,
    PathStyle, Pixels, Point, Render, SharedString, StrokeOptions, Styled as _, TextRun,
    TruncateFrom, Window, canvas, div, hsla, point, prelude::*, px, rgb, size,
};
use lyon_tessellation::{LineCap, LineJoin};

use serde::{Deserialize, Serialize};

use crate::mascot::{Mascot, Mood};
use crate::theme;

/// The layer stays this big; only the shape inside moves, and the input
/// region follows it so clicks around it reach the windows below. On macOS
/// there is room for a camera cutout too: in the middle of a compact island,
/// and over an open one.
const WIN_W: f32 = if cfg!(target_os = "macos") {
    640.
} else {
    560.
};
/// Tall enough for the call card with the tallest alert card under it.
const WIN_H: f32 = if cfg!(target_os = "macos") {
    380.
} else {
    300.
};
/// The concave fillets where the shape melts into the top edge.
const SHOULDER: f32 = 10.;
const SMALL_W: f32 = 30.;
/// The small raccoon's inset from the island's left edge, and the gap to
/// whatever shows right of it.
const FACE_X: f32 = 12.;
const FACE_GAP: f32 = 10.;
/// The room the raccoon takes beside a camera cutout, and the least the
/// island shows on each side of one.
const EAR: f32 = FACE_X + SMALL_W + FACE_X;
/// Where a compact island's content starts, past the raccoon.
const CONTENT_X: f32 = FACE_X + SMALL_W + FACE_GAP;
/// A compact island's inner padding on the right.
const PAD_R: f32 = 16.;
/// The working state's dots, and the "+n" after them for the rest.
const DOT: f32 = 7.;
const DOT_GAP: f32 = 6.;
const MORE_SIZE: f32 = 11.;
/// A Geist Mono glyph's advance, in ems.
const MONO_ADVANCE: f32 = 0.6;
/// Audio bars: each bar's width, the gap between them, their box's height,
/// and how many the recording wave has.
const BAR_W: f32 = 3.;
const BAR_GAP: f32 = 2.;
const BARS_H: f32 = 22.;
const REC_BARS: usize = 9;
const COMPACT_H: f32 = 32.;
const BIG_W: f32 = 46.;
const ROW_H: f32 = 34.;
/// A row's gap to the island's sides and, for the last, its bottom.
const ROW_INSET: f32 = 6.;
const HEAD_H: f32 = 40.;
/// The most rows the list shows, and dots the working state does.
pub const MAX_ROWS: usize = 6;
const MAX_DOTS: usize = 6;
/// Nothing busy for this long and the raccoon falls asleep.
const SLEEP_AFTER: Duration = Duration::from_secs(120);
/// An alert left alone goes away after this long, plus time to read it.
const ALERT_FOR: Duration = Duration::from_secs(12);
/// A notice with nothing to open (no buttons) goes sooner.
const NOTICE_FOR: Duration = Duration::from_secs(5);
/// However long the text, no card stays longer than this on its own.
const ALERT_MAX: Duration = Duration::from_secs(25);
/// The card hangs this far under the island once it let go.
const CARD_GAP: f32 = 10.;
const CARD_W: f32 = 440.;
/// A short notice without buttons: a one-line pill.
const PILL_W: f32 = 330.;
const PILL_H: f32 = 54.;
const BADGE: f32 = 26.;
/// How long a new card ignores the pointer, which may be resting where it drops.
const CARD_SETTLE: Duration = Duration::from_millis(400);
/// A card's buttons' height.
const BUTTON_H: f32 = 28.;
/// A notice's picture, where the badge goes.
const AVATAR: f32 = 32.;
const TITLE_SIZE: f32 = 14.5;
const TITLE_LH: f32 = 19.;
const TITLE_LINES: usize = 2;
const BODY_SIZE: f32 = 12.5;
const BODY_LH: f32 = 17.;
const BODY_LINES: usize = 4;
/// The fillets' radius where the card hangs from the island, at full join.
const NECK_R: f32 = 12.;
/// How long a card takes to melt back in; the next one drips after.
const MELT_FOR: f32 = 0.9;
/// How long a confirmation card is up before a click on "Approve" counts.
const ARM_AFTER: Duration = Duration::from_millis(1200);
/// How long the island stays open after the pointer leaves it. It should
/// close as the pointer goes; this much is left for SUPER P's overlay,
/// which takes the pointer the moment it opens, to freeze the screen first.
const LEAVE_GRACE: Duration = Duration::from_millis(150);
/// How often macOS is asked where the pointer is: while it is near the
/// island, and while it is not.
#[cfg(target_os = "macos")]
const POINTER_NEAR: Duration = Duration::from_millis(16);
#[cfg(target_os = "macos")]
const POINTER_FAR: Duration = Duration::from_millis(60);
/// How close to the island the pointer is "near".
#[cfg(target_os = "macos")]
const POINTER_MARGIN: f32 = 80.;
/// How often the island checks it is still on the right screen.
#[cfg(target_os = "macos")]
const SCREENS_EVERY: Duration = Duration::from_secs(2);

static ENABLED: AtomicBool = AtomicBool::new(true);

/// Whether the island shows (Settings › General, `island_off` in the state file).
pub fn enabled() -> bool {
    ENABLED.load(Ordering::Relaxed)
}

pub fn set_enabled(on: bool) {
    ENABLED.store(on, Ordering::Relaxed);
}

/// The island needs a layer-shell window (Wayland; on X11 gpui would open a
/// normal one) or macOS, where it is a panel over the menu bar.
#[cfg(not(windows))]
pub fn supported(cx: &App) -> bool {
    cfg!(target_os = "macos") || cx.compositor_name() == "Wayland"
}

/// Call from `open_window`'s build closure, before `Island::new`: on macOS it
/// makes the new window the island's (`mac::adopt`) and reads the top edge of
/// its screen, which the island then takes its shape from.
pub fn adopt(window: &mut Window) {
    #[cfg(target_os = "macos")]
    {
        use raw_window_handle::{HasWindowHandle as _, RawWindowHandle};
        match window.window_handle().map(|h| h.as_raw()) {
            Ok(RawWindowHandle::AppKit(handle)) => {
                mac::adopt(handle.ns_view.as_ptr(), (WIN_W, WIN_H));
            }
            _ => eprintln!("arakun: island: no AppKit window to place"),
        }
    }
    #[cfg(not(target_os = "macos"))]
    let _ = window;
}

/// The top edge of the screen the island opened on.
fn screen_edge() -> Edge {
    #[cfg(target_os = "macos")]
    return mac::edge();
    #[cfg(not(target_os = "macos"))]
    Edge::from_env().unwrap_or_default()
}

/// The island's process is no app of its own: out of the Dock (macOS).
pub fn hide_from_dock() {
    #[cfg(target_os = "macos")]
    mac::hide_from_dock();
}

/// Brings the app with this process id to the front, where the island can
/// (macOS; on Linux the compositor is asked, see `island_host`).
pub fn activate_app(pid: u32) {
    #[cfg(target_os = "macos")]
    mac::activate(pid);
    #[cfg(not(target_os = "macos"))]
    let _ = pid;
}

/// On Windows the island is a topmost popup that clicks pass around (`top_center`).
#[cfg(windows)]
pub fn supported(_: &App) -> bool {
    true
}

/// The screen the island lives on: the widest landscape one, so a monitor
/// turned on its side isn't picked. `None` leaves it to the compositor. On
/// macOS `adopt` places the window itself.
#[cfg(target_os = "linux")]
pub fn landscape_display(cx: &gpui::App) -> Option<gpui::DisplayId> {
    let screens: Vec<_> = cx
        .displays()
        .iter()
        .map(|d| {
            (
                d.id(),
                d.bounds().origin.x.as_f32(),
                d.bounds().origin.y.as_f32(),
                d.bounds().size.width.as_f32(),
                d.bounds().size.height.as_f32(),
            )
        })
        .collect();
    // gpui reports a monitor turned on its side at its unrotated size; Hyprland knows the rotation.
    if let Some((x, y)) = hyprland_landscape()
        && let Some(&(id, ..)) = screens.iter().find(|s| s.1 == x && s.2 == y)
    {
        return Some(id);
    }
    pick_landscape(screens.iter().map(|&(id, _, _, w, h)| (id, w, h)))
}

#[cfg(not(target_os = "linux"))]
pub fn landscape_display(_cx: &gpui::App) -> Option<gpui::DisplayId> {
    None
}

/// Where the widest landscape monitor sits, by `hyprctl monitors -j`.
#[cfg(target_os = "linux")]
fn hyprland_landscape() -> Option<(f32, f32)> {
    std::env::var_os("HYPRLAND_INSTANCE_SIGNATURE")?;
    let out = std::process::Command::new("hyprctl")
        .args(["monitors", "-j"])
        .output()
        .ok()?;
    let monitors: Vec<serde_json::Value> = serde_json::from_slice(&out.stdout).ok()?;
    hyprland_pick(&monitors)
}

#[cfg(any(target_os = "linux", test))]
fn hyprland_pick(monitors: &[serde_json::Value]) -> Option<(f32, f32)> {
    monitors
        .iter()
        .filter_map(|m| {
            let (w, h) = (m["width"].as_f64()?, m["height"].as_f64()?);
            let (w, h) = if m["transform"].as_i64().unwrap_or(0) % 2 == 1 {
                (h, w)
            } else {
                (w, h)
            };
            let (x, y) = (m["x"].as_f64()? as f32, m["y"].as_f64()? as f32);
            (w > h).then_some((w, x, y))
        })
        .max_by(|a, b| a.0.total_cmp(&b.0))
        .map(|(_, x, y)| (x, y))
}

#[cfg(any(target_os = "linux", test))]
fn pick_landscape<I: Copy>(screens: impl Iterator<Item = (I, f32, f32)>) -> Option<I> {
    screens
        .filter(|&(_, w, h)| w > h)
        .max_by(|a, b| a.1.total_cmp(&b.1))
        .map(|(id, _, _)| id)
}

/// The island's window: on top of everything, at the top edge, out of the
/// window list. `keys` lets a click give it the keyboard (the demo's keys);
/// without, it never takes focus.
pub fn window_options(keys: bool) -> gpui::WindowOptions {
    use gpui::{WindowBackgroundAppearance, WindowBounds, WindowOptions};
    WindowOptions {
        titlebar: None,
        window_bounds: Some(WindowBounds::Windowed(Bounds {
            origin: point(px(0.), px(0.)),
            size: size(px(WIN_W), px(WIN_H)),
        })),
        app_id: Some("arakun-island".to_string()),
        focus: false,
        is_movable: false,
        is_resizable: false,
        window_background: WindowBackgroundAppearance::Transparent,
        // It never has the focus, and gpui would hold an unfocused window to 30 fps.
        inactive_frame_interval: None,
        kind: window_kind(keys),
        ..Default::default()
    }
}

#[cfg(target_os = "linux")]
fn window_kind(keys: bool) -> gpui::WindowKind {
    use gpui::layer_shell::*;
    gpui::WindowKind::LayerShell(LayerShellOptions {
        namespace: "arakun-island".to_string(),
        layer: Layer::Overlay,
        anchor: Anchor::TOP,
        exclusive_zone: None,
        margin: Some((px(0.), px(0.), px(0.), px(0.))),
        keyboard_interactivity: if keys {
            KeyboardInteractivity::OnDemand
        } else {
            KeyboardInteractivity::None
        },
        ..Default::default()
    })
}

/// Windows has no layer shell to anchor the island to the top edge: it is a
/// topmost window placed at the top center of the display's work area, so a
/// taskbar moved to the top stays visible. `None` without displays.
#[cfg(windows)]
pub fn top_center(cx: &App, display: Option<gpui::DisplayId>) -> Option<Bounds<Pixels>> {
    let screen = display
        .and_then(|id| cx.find_display(id))
        .or_else(|| cx.primary_display())?;
    let area = screen.visible_bounds();
    Some(Bounds {
        origin: point(area.center().x - px(WIN_W / 2.), area.origin.y),
        size: size(px(WIN_W), px(WIN_H)),
    })
}

/// macOS: a panel above the menu bar that doesn't activate the app; `adopt`
/// makes the rest of it. On Windows a topmost popup, placed by `top_center`.
/// Elsewhere `supported` says no and the island never opens, so this just has
/// to build.
#[cfg(not(target_os = "linux"))]
fn window_kind(_keys: bool) -> gpui::WindowKind {
    gpui::WindowKind::PopUp
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    Sleeping,
    /// Awake, nothing busy: a turn ended less than `SLEEP_AFTER` ago.
    Idle,
    Working,
    /// Speaking and Listening are for the voice to come; only `arakun island` shows them.
    Speaking,
    Listening,
    Alert,
}

impl Phase {
    pub fn from_name(s: &str) -> Option<Phase> {
        Some(match s {
            "sleeping" => Phase::Sleeping,
            "idle" => Phase::Idle,
            "working" => Phase::Working,
            "speaking" => Phase::Speaking,
            "listening" => Phase::Listening,
            "alert" => Phase::Alert,
            _ => return None,
        })
    }
}

/// What the shape shows; hovering turns it into the list unless a card is out.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum View {
    Compact(Phase),
    /// Agents at work: the raccoon, this many dots and a "+n" this many
    /// characters long (0 for none).
    Busy(usize, usize),
    /// Recording at rest or at work: the raccoon and the green bars.
    Recording,
    /// With this many rows.
    List(usize),
    /// A voice call: its phase and live captions.
    Call,
}

impl View {
    /// The view's size on `edge`. A camera cutout takes the middle of a
    /// compact island, so its two halves move apart; an open one starts under it.
    fn size(self, edge: Edge) -> (f32, f32) {
        let (w, h) = self.base();
        let (cut_w, cut_h) = (edge.cut_w(), edge.cut_h());
        let (w, h) = if cut_w <= 0. {
            (w, h)
        } else if self.open() {
            (w.max(cut_w + 2. * EAR), cut_h + h)
        } else {
            (cut_w + 2. * self.ear(), h.max(cut_h))
        };
        (w.min(WIN_W - 2. * SHOULDER), h)
    }

    /// A list or a call: content in rows, which goes under a cutout whole.
    fn open(self) -> bool {
        matches!(self, View::List(_) | View::Call)
    }

    /// How wide a compact island is on each side of a cutout: the raccoon is
    /// on its left, what the view shows on its right, and both sides match.
    fn ear(self) -> f32 {
        let (w, _) = self.base();
        match self {
            View::Compact(Phase::Sleeping | Phase::Idle) => EAR,
            // A label after the raccoon and a wave at the far end.
            View::Compact(Phase::Speaking | Phase::Listening | Phase::Alert) => w / 2.,
            _ => (w - CONTENT_X + FACE_GAP).max(EAR),
        }
    }

    /// The size on a plain edge.
    fn base(self) -> (f32, f32) {
        match self {
            // At rest only the raccoon shows, so the island stays clear of the
            // window's tabs under it.
            View::Compact(Phase::Sleeping | Phase::Idle) => (FACE_X + SMALL_W + FACE_X, 30.),
            // Just wide enough for the raccoon and its dots.
            View::Busy(dots, more) => (CONTENT_X + dots_w(dots, more) + PAD_R, COMPACT_H),
            View::Recording => (CONTENT_X + bars_w(REC_BARS) + PAD_R, COMPACT_H),
            View::Compact(Phase::Working) => (150., COMPACT_H),
            View::Compact(_) => (330., 36.),
            View::List(n) => (480., HEAD_H + n as f32 * ROW_H + ROW_INSET),
            View::Call => (440., 104.),
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    Working,
    Waiting,
    Error,
    /// Finished a turn a short while ago.
    Done,
}

impl Status {
    fn color(self) -> Hsla {
        match self {
            Status::Working => rgb(theme::SUCCESS).into(),
            Status::Waiting => rgb(theme::ATTENTION).into(),
            Status::Error => rgb(theme::ERROR).into(),
            Status::Done => hsla(0., 0., 1., 0.3),
        }
    }

    /// Busy, or waiting on the user: it keeps the island awake.
    pub fn active(self) -> bool {
        self != Status::Done
    }

    /// The list's order: what needs the user first.
    fn rank(self) -> u8 {
        match self {
            Status::Waiting => 0,
            Status::Error => 1,
            Status::Working => 2,
            Status::Done => 3,
        }
    }
}

/// One agent session, as the list shows it.
#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
pub struct Row {
    /// The session id; what "View" and a click on the row open.
    pub key: SharedString,
    pub status: Status,
    pub model: SharedString,
    pub doing: SharedString,
    pub elapsed: SharedString,
    pub cost: SharedString,
    /// Unix seconds of its last activity.
    pub updated: u64,
}

/// What the island shows of the agents, all projects at once.
#[derive(Clone, Default, PartialEq, Debug, Serialize, Deserialize)]
pub struct Feed {
    pub rows: Vec<Row>,
    /// What the sessions active today cost, e.g. "$2.87".
    pub today: Option<SharedString>,
    /// The session the user is looking at in a focused window: no alerts for it.
    pub viewing: Option<SharedString>,
    /// Whether the app reduces motion, by its setting or the system's. None
    /// with no app connected (or an older one): the island follows the system.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reduce_motion: Option<bool>,
}

/// Most relevant first: needs input, errors, working, then recently finished,
/// newest first within each.
pub fn sort_rows(rows: &mut [Row]) {
    rows.sort_by(|a, b| {
        a.status
            .rank()
            .cmp(&b.status.rank())
            .then(b.updated.cmp(&a.updated))
    });
}

/// How wide `dots` dots and a "+n" `more` characters long lay out.
fn dots_w(dots: usize, more: usize) -> f32 {
    let label = more as f32 * MORE_SIZE * MONO_ADVANCE;
    let items = dots + usize::from(more > 0);
    dots as f32 * DOT + label + items.saturating_sub(1) as f32 * DOT_GAP
}

/// How wide `n` audio bars lay out.
fn bars_w(n: usize) -> f32 {
    n as f32 * BAR_W + n.saturating_sub(1) as f32 * BAR_GAP
}

/// The working state's view for these rows.
fn busy(rows: &[Row]) -> View {
    let (dots, more) = dots(rows);
    let label = if more > 0 {
        format!("+{more}").len()
    } else {
        0
    };
    View::Busy(dots.len(), label)
}

/// Whether `view`'s content, drawn from `rows` now, still has the size the
/// view was made for. The dots and the list read the live feed, so a view kept
/// from before the feed changed would draw the new dots or rows in the old
/// room and spill out of it. Recording swaps the dots for its bars, which
/// need the recording view's width.
fn drawn_as(view: View, rows: &[Row], recording: bool) -> bool {
    let bars_fit = || view.base().0 >= View::Recording.base().0;
    match view {
        View::Busy(..) => busy(rows) == view && (!recording || bars_fit()),
        View::Compact(_) if recording => bars_fit(),
        View::List(n) => rows.len().min(MAX_ROWS) == n,
        _ => true,
    }
}

/// The working state's dots, at most `MAX_DOTS`, and how many more there are.
pub fn dots(rows: &[Row]) -> (Vec<Status>, usize) {
    let active: Vec<Status> = rows
        .iter()
        .map(|r| r.status)
        .filter(|s| s.active())
        .collect();
    let more = active.len().saturating_sub(MAX_DOTS);
    (active.into_iter().take(MAX_DOTS).collect(), more)
}

/// The phase the agents call for; `quiet` is how long since one was last busy.
pub fn phase_for(rows: &[Row], alert: bool, quiet: Option<Duration>) -> Phase {
    if alert {
        Phase::Alert
    } else if rows.iter().any(|r| r.status.active()) {
        Phase::Working
    } else if quiet.is_some_and(|q| q < SLEEP_AFTER) {
        Phase::Idle
    } else {
        Phase::Sleeping
    }
}

/// "0:58", "12:41", "1:02:07".
pub fn fmt_elapsed(d: Duration) -> String {
    let s = d.as_secs();
    if s >= 3600 {
        format!("{}:{:02}:{:02}", s / 3600, s / 60 % 60, s % 60)
    } else {
        format!("{}:{:02}", s / 60, s % 60)
    }
}

/// One line of text for a row: markdown markers off, spaces collapsed, cut at 120 chars.
pub fn one_line(text: &str) -> String {
    let line = text
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("");
    let line = line
        .trim_start_matches(['#', '>', '-', '*', ' '])
        .replace(['`', '*'], "");
    let line = line.split_whitespace().collect::<Vec<_>>().join(" ");
    match line.char_indices().nth(120) {
        Some((i, _)) => format!("{}…", &line[..i]),
        None => line,
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AlertKind {
    Finished,
    Input,
    Error,
    /// A headless agent's tool call waiting for "Approve" or "Deny"; it
    /// stays until answered.
    Confirm,
    /// Not about a session (a deploy, CI, a limit, a task…): a badge in the
    /// raccoon's place, and "View" only when `key` names something to open.
    Success,
    Warning,
    Failure,
    Info,
}

impl AlertKind {
    /// About an agent session, with the raccoon on the card.
    pub fn session(self) -> bool {
        matches!(
            self,
            AlertKind::Finished | AlertKind::Input | AlertKind::Error | AlertKind::Confirm
        )
    }

    /// The badge a notice shows in the raccoon's place.
    fn badge(self) -> Hsla {
        match self {
            AlertKind::Success => rgb(theme::SUCCESS).into(),
            AlertKind::Warning => rgb(theme::ATTENTION).into(),
            AlertKind::Failure => rgb(theme::ERROR).into(),
            _ => hsla(0., 0., 1., 0.7),
        }
    }
}

#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
pub struct Alert {
    /// The session it is about; empty for one sent by `arakun island alert`.
    pub key: SharedString,
    pub kind: AlertKind,
    pub title: SharedString,
    pub subtitle: SharedString,
    /// The request a `Confirm` answers.
    #[serde(default)]
    pub req: SharedString,
    /// A picture for a notice's badge (a sender's avatar): a file, empty for none.
    #[serde(default)]
    pub image: SharedString,
    /// The service a notice comes from (`github`): its logo marks the picture's
    /// corner, or takes the badge's place when there is no picture.
    #[serde(default)]
    pub brand: SharedString,
}

impl Alert {
    /// "Later" and "View" (or "Deny" and "Approve"): a session's card always,
    /// a notice only when it names something to open.
    pub fn buttons(&self) -> bool {
        self.kind.session() || !self.key.is_empty()
    }
}

/// How long a card stays when left alone: its kind's time plus about a second
/// for every 15 words past the first 12, up to `ALERT_MAX`. A confirmation
/// waits for its answer.
pub fn alert_for(alert: &Alert) -> Option<Duration> {
    if alert.kind == AlertKind::Confirm {
        return None;
    }
    let base = if alert.buttons() {
        ALERT_FOR
    } else {
        NOTICE_FOR
    };
    let words = alert.title.split_whitespace().count() + alert.subtitle.split_whitespace().count();
    let reading = Duration::from_secs_f32(words.saturating_sub(12) as f32 / 15.);
    Some((base + reading).min(ALERT_MAX))
}

/// Alerts waiting their turn; the front one shows, for `alert_for` while not hovered.
#[derive(Default)]
pub struct Alerts {
    queue: VecDeque<(u64, Alert)>,
    next: u64,
    since: Option<Instant>,
}

impl Alerts {
    /// A newer alert about the same session replaces the one waiting.
    pub fn push(&mut self, alert: Alert, now: Instant) {
        self.next += 1;
        let seq = self.next;
        match self
            .queue
            .iter()
            .position(|(_, a)| !a.key.is_empty() && a.key == alert.key)
        {
            Some(0) => {
                self.queue[0] = (seq, alert);
                self.since = Some(now);
            }
            Some(i) => self.queue[i] = (seq, alert),
            None => self.queue.push_back((seq, alert)),
        }
        self.since.get_or_insert(now);
    }

    /// The alert showing, and a number that changes whenever another one does.
    pub fn front(&self) -> Option<(u64, &Alert)> {
        self.queue.front().map(|(seq, a)| (*seq, a))
    }

    pub fn dismiss(&mut self, now: Instant) {
        self.queue.pop_front();
        self.since = (!self.queue.is_empty()).then_some(now);
    }

    pub fn retain(&mut self, now: Instant, keep: impl Fn(&Alert) -> bool) {
        let front = self.front().map(|(seq, _)| seq);
        self.queue.retain(|(_, a)| keep(a));
        if self.front().map(|(seq, _)| seq) != front {
            self.since = (!self.queue.is_empty()).then_some(now);
        }
    }

    /// Times the front alert out; hovering holds it. Returns whether it went.
    pub fn tick(&mut self, now: Instant, hovered: bool) -> bool {
        let Some((_, front)) = self.front() else {
            return false;
        };
        let stay = alert_for(front);
        if stay.is_none() || hovered {
            self.since = Some(now);
            return false;
        }
        match (self.since, stay) {
            (Some(at), Some(stay)) if now.duration_since(at) >= stay => {
                self.dismiss(now);
                true
            }
            _ => false,
        }
    }
}

pub enum IslandEvent {
    /// "View": show this session in the main window.
    Open(SharedString),
    /// A row of the list was clicked.
    OpenRow(SharedString),
    /// "Later" put the alert about this session away.
    Later(SharedString),
    /// An alert went away, by "Later" or its time.
    Dismissed,
    /// "Approve" or "Deny" on a `Confirm` card.
    Answer {
        key: SharedString,
        req: SharedString,
        approve: bool,
    },
    /// A click on the shape, off its buttons and rows.
    Pressed,
    /// A click on the small raccoon: starts or ends a call.
    Face,
}

/// How loud the microphone and the voice are right now, 0 to 1: a call
/// writes them from its audio threads, the waveform reads them each frame.
#[derive(Default)]
pub struct Levels {
    mic: AtomicU32,
    out: AtomicU32,
}

impl Levels {
    #[cfg(feature = "coderlabs")]
    pub fn set_mic(&self, v: f32) {
        self.mic.store(v.to_bits(), Ordering::Relaxed);
    }

    #[cfg(feature = "coderlabs")]
    pub fn set_out(&self, v: f32) {
        self.out.store(v.to_bits(), Ordering::Relaxed);
    }

    fn get(&self, listening: bool) -> f32 {
        f32::from_bits(if listening { &self.mic } else { &self.out }.load(Ordering::Relaxed))
    }
}

/// A call on the island: listening or speaking, what each side last said.
struct CallView {
    phase: Phase,
    user: SharedString,
    assistant: SharedString,
    levels: Arc<Levels>,
    /// The level the bars show, eased toward the live one.
    shown: f32,
}

/// A critically-underdamped spring (stiffness 380, damping 26): a hint of
/// overshoot, settled in about 0.4 s.
#[derive(Clone, Copy)]
struct Spring {
    x: f32,
    v: f32,
}

impl Spring {
    fn step(&mut self, target: f32, dt: f32) {
        const K: f32 = 380.;
        const C: f32 = 26.;
        let n = (dt / (1. / 240.)).ceil().max(1.);
        let h = dt / n;
        for _ in 0..n as usize {
            self.v += (K * (target - self.x) - C * self.v) * h;
            self.x += self.v * h;
        }
    }

    fn settled(&self, target: f32) -> bool {
        (self.x - target).abs() < 0.05 && self.v.abs() < 0.5
    }
}

/// Whether the pointer is really on a card. A card can drop under a pointer
/// that rests on the window below (its tabs), and that must not hold the card
/// or catch the next click: it only counts once the pointer moves on the card
/// after the card settled. Any change of card or island starts it over, since
/// gpui sends no leave for a card that is gone.
#[derive(Default)]
struct CardHover {
    /// The card (by its seq) the pointer is on.
    hot: Option<u64>,
    /// Where the pointer was on that card when it settled.
    anchor: Option<(u64, Point<Pixels>)>,
}

impl CardHover {
    /// The pointer moved to `at` on card `seq`; returns whether that made it hot.
    fn moved(&mut self, seq: u64, at: Point<Pixels>, settled: bool) -> bool {
        if self.hot == Some(seq) {
            return false;
        }
        match self.anchor {
            // Moves before the card settled don't count: the anchor follows them.
            Some((s, from)) if s == seq && settled => {
                if (at.x - from.x).abs() + (at.y - from.y).abs() > px(4.) {
                    self.hot = Some(seq);
                    return true;
                }
            }
            _ => self.anchor = Some((seq, at)),
        }
        false
    }

    fn is_hot(&self, seq: u64) -> bool {
        self.hot == Some(seq)
    }

    /// Starts over; returns whether a card was hot.
    fn reset(&mut self) -> bool {
        self.anchor = None;
        self.hot.take().is_some()
    }
}

/// An alert's card: a shape of its own that drips out of the island, hangs
/// under it and melts back in when the alert goes.
struct Card {
    seq: u64,
    alert: Alert,
    born: Instant,
    /// When it started to melt back in.
    gone: Option<Instant>,
    /// Measured on the first frame it is drawn.
    layout: Option<CardLayout>,
}

/// The card's text as measured, and the size around it.
#[derive(Clone)]
struct CardLayout {
    title: SharedString,
    body: SharedString,
    body_lines: usize,
    pill: bool,
    w: f32,
    h: f32,
}

impl CardLayout {
    /// Where the text column starts: past the raccoon, or past the badge or picture.
    fn text_x(alert: &Alert) -> f32 {
        if alert.kind.session() {
            18. + BIG_W + 12.
        } else {
            14. + Self::face(alert) + 12.
        }
    }

    /// The badge's or picture's side, on a notice.
    fn face(alert: &Alert) -> f32 {
        if alert.image.is_empty() && alert.brand.is_empty() {
            BADGE
        } else {
            AVATAR
        }
    }

    fn measure(alert: &Alert, window: &Window, cx: &App) -> CardLayout {
        let flat = |s: &str| SharedString::from(s.split_whitespace().collect::<Vec<_>>().join(" "));
        let (title, body) = (flat(&alert.title), flat(&alert.subtitle));
        let session = alert.kind.session();
        // Only a confirmation keeps its buttons on the card; the others show
        // theirs over it on hover, so they take no room.
        let buttons = alert.kind == AlertKind::Confirm;
        let bold = gpui::Font {
            weight: FontWeight::SEMIBOLD,
            ..gpui::font(theme::FONT_SANS)
        };
        let plain = gpui::font(theme::FONT_SANS);
        let tx = Self::text_x(alert);
        if !buttons && !session {
            let room = PILL_W - tx - 18.;
            let wide = |text: &SharedString, font: &gpui::Font, size: f32| {
                let run = TextRun {
                    len: text.len(),
                    font: font.clone(),
                    color: gpui::white(),
                    background_color: None,
                    underline: None,
                    strikethrough: None,
                };
                window
                    .text_system()
                    .shape_line(text.clone(), px(size), &[run], None)
                    .width
                    .as_f32()
            };
            if wide(&title, &bold, TITLE_SIZE) <= room && wide(&body, &plain, BODY_SIZE) <= room {
                return CardLayout {
                    title,
                    body,
                    body_lines: 1,
                    pill: true,
                    w: PILL_W,
                    h: PILL_H,
                };
            }
        }
        let room = CARD_W - tx - 18.;
        let title_lines = lines(&title, bold, TITLE_SIZE, room, TITLE_LINES, window, cx);
        let body_lines = lines(&body, plain, BODY_SIZE, room, BODY_LINES, window, cx);
        let text_h = title_lines as f32 * TITLE_LH
            + if body_lines > 0 {
                3. + body_lines as f32 * BODY_LH
            } else {
                0.
            };
        let face = if session {
            BIG_W * 166. / 212.
        } else {
            Self::face(alert)
        };
        let foot = if buttons { 12. + BUTTON_H + 14. } else { 14. };
        CardLayout {
            title,
            body,
            body_lines,
            pill: false,
            w: CARD_W,
            h: 14. + face.max(text_h) + foot,
        }
    }
}

/// How many lines `text` takes wrapped to `width`, at most `max`: the same
/// truncation and wrapping a div with `line_clamp(max).text_ellipsis()` does.
fn lines(
    text: &SharedString,
    font: gpui::Font,
    size: f32,
    width: f32,
    max: usize,
    window: &Window,
    cx: &App,
) -> usize {
    if text.is_empty() {
        return 0;
    }
    let runs = [TextRun {
        len: text.len(),
        font: font.clone(),
        color: gpui::white(),
        background_color: None,
        underline: None,
        strikethrough: None,
    }];
    let mut wrapper = cx.text_system().line_wrapper(font, px(size));
    let (text, runs) =
        wrapper.truncate_wrapped_line(text.clone(), px(width), max, "…", &runs, TruncateFrom::End);
    match window
        .text_system()
        .shape_text(text, px(size), &runs, Some(px(width)), Some(max))
    {
        Ok(shaped) => shaped
            .iter()
            .map(|l| l.wrap_boundaries().len() + 1)
            .sum::<usize>()
            .min(max),
        Err(_) => max,
    }
}

/// Where the card is at a moment, in the window's pixels.
#[derive(Clone, Copy, Debug)]
struct CardGeom {
    w: f32,
    h: f32,
    top: f32,
    /// How strongly it joins the island, 0 to 1: the fillets of the neck.
    join: f32,
    /// How far the facing edges bow toward each other after the neck snaps.
    bulge: f32,
    /// The content's opacity.
    alpha: f32,
}

/// A spring from 0 to 1 let go at `t` = 0 (mass 1, stiffness `k`, damping `c`).
fn spring(t: f32, k: f32, c: f32) -> f32 {
    if t <= 0. {
        return 0.;
    }
    let w0 = k.sqrt();
    let z = c / (2. * w0);
    let wd = w0 * (1. - z * z).sqrt();
    1. - (-z * w0 * t).exp() * ((wd * t).cos() + z * w0 / wd * (wd * t).sin())
}

fn lerp(a: f32, b: f32, t: f32) -> f32 {
    a + (b - a) * t
}

/// The card `t` s into dripping out of an island `iw` by `ih`: its width,
/// height and top. It starts small and hidden inside, then drops and grows.
fn drip(w: f32, h: f32, iw: f32, ih: f32, t: f32) -> (f32, f32, f32) {
    let (fall, grow) = (spring(t, 130., 12.), spring(t - 0.07, 190., 19.));
    let (w0, h0) = (iw.min(w) * 0.55, 6.);
    (
        lerp(w0, w, grow),
        lerp(h0, h, grow),
        lerp(ih - h0 - 4., ih + CARD_GAP, fall),
    )
}

/// When the neck snaps: the gap first reaches most of `CARD_GAP` (whatever the sizes).
fn snap_at() -> f32 {
    static AT: LazyLock<f32> = LazyLock::new(|| {
        (0..1500)
            .map(|ms| ms as f32 / 1000.)
            .find(|&t| drip(1., 1., 1., 0., t).2 >= 0.7 * CARD_GAP)
            .unwrap_or(0.2)
    });
    *AT
}

/// The card `t` s after it started to drip out and, once going, `melt` s after
/// it started back in. The neck between it and the island grows in, snaps
/// with a wobble as the card drops, forms again as it rises and lets go
/// before it is fully in.
fn card_geom(w: f32, h: f32, iw: f32, ih: f32, t: f32, melt: Option<f32>) -> CardGeom {
    let (mut cw, mut ch, mut top) = drip(w, h, iw, ih, t);
    let s = t - snap_at();
    let mut join = if s < 0. {
        smoothstep(0., 0.12, t)
    } else {
        1. - smoothstep(0., 0.09, s)
    };
    let mut bulge = if s < 0. {
        0.
    } else {
        3.2 * (-s / 0.11).exp() * (TAU * 6.5 * s).cos()
    };
    let mut alpha = smoothstep(0.02, 0.2, s);
    if let Some(m) = melt {
        let (rise, shrink) = (spring(m, 260., 20.), spring(m - 0.08, 300., 24.));
        cw = lerp(cw, iw * 0.6, shrink);
        ch = lerp(ch, 4., shrink);
        top = lerp(top, ih - 14., rise);
        // Joined again as the gap closes, and let go while the card finishes
        // going in: nothing may hang under the island when it is removed.
        join = join.max(smoothstep(14., 3., top - ih)) * (1. - smoothstep(0.6, 0.95, shrink));
        bulge = 0.;
        alpha *= 1. - smoothstep(0., 0.12, m);
    }
    CardGeom {
        w: cw,
        h: ch,
        top,
        join,
        bulge,
        alpha,
    }
}

/// The island's little bounce `melt` s after a card started back in.
fn bounce(melt: f32) -> f32 {
    let s = melt - 0.24;
    if s <= 0. {
        0.
    } else {
        2.6 * (-s / 0.13).exp() * (TAU * 5. * s).sin()
    }
}

/// The card's corner radius: a pill's for a short card, 26 for a tall one.
fn card_radius(full_h: f32, w: f32, h: f32) -> f32 {
    (if full_h > 60. { 26. } else { full_h / 2. })
        .min(h / 2.)
        .min(w / 2.)
}

/// A rounded rectangle centered on the window's middle (x = 0 here).
#[derive(Clone, Copy, Debug)]
struct Rounded {
    hw: f32,
    top: f32,
    bottom: f32,
    r: f32,
}

impl Rounded {
    /// Signed distance from (x, y), negative inside.
    fn sd(&self, x: f32, y: f32) -> f32 {
        let hh = (self.bottom - self.top) / 2.;
        let qx = x.abs() - self.hw + self.r;
        let qy = (y - self.top - hh).abs() - hh + self.r;
        qx.max(0.).hypot(qy.max(0.)) + qx.max(qy).min(0.) - self.r
    }

    /// The outline's nearest point to (x, y), which is outside.
    fn nearest(&self, x: f32, y: f32) -> (f32, f32) {
        let ix = x.clamp(-(self.hw - self.r), self.hw - self.r);
        let iy = y.clamp(
            self.top + self.r,
            (self.bottom - self.r).max(self.top + self.r),
        );
        let (dx, dy) = (x - ix, y - iy);
        let d = dx.hypot(dy).max(1e-4);
        (ix + dx / d * self.r, iy + dy / d * self.r)
    }

    /// Narrowed toward its middle, `s` of its straight edges left.
    fn narrowed(self, s: f32) -> Rounded {
        Rounded {
            hw: self.r + (self.hw - self.r) * s,
            ..self
        }
    }
}

/// The right side of the neck: a ball of radius `r` wedged between the island
/// and the card, as near the middle as it gets. Its arc between the two
/// points it touches is the concave fillet. `None` when the ball passes
/// between them: nothing joins them.
fn wedge(island: Rounded, card: Rounded, r: f32) -> Option<[(f32, f32); 3]> {
    if r < 0.5 {
        return None;
    }
    // The ball's center stays r off the island: along its bottom, round its
    // corner and up its side, until it is r off the card too.
    let flat = island.hw - island.r;
    let round = island.r + r;
    let quarter = round * FRAC_PI_2;
    let end = flat + quarter + (island.bottom - island.r - SHOULDER).max(0.);
    let at = |s: f32| {
        if s <= flat {
            (s, island.bottom + r)
        } else if s <= flat + quarter {
            let a = (s - flat) / round;
            (
                flat + round * a.sin(),
                island.bottom - island.r + round * a.cos(),
            )
        } else {
            (
                island.hw + r,
                island.bottom - island.r - (s - flat - quarter),
            )
        }
    };
    let off = |s: f32| {
        let (x, y) = at(s);
        card.sd(x, y) - r
    };
    if off(0.) >= 0. {
        return None;
    }
    let (mut lo, mut hi) = (0., 0.);
    while off(hi) < 0. {
        if hi >= end {
            return None;
        }
        lo = hi;
        hi = (hi + 1.).min(end);
    }
    for _ in 0..12 {
        let mid = (lo + hi) / 2.;
        if off(mid) < 0. { lo = mid } else { hi = mid }
    }
    let c = at(hi);
    Some([c, island.nearest(c.0, c.1), card.nearest(c.0, c.1)])
}

pub struct Island {
    start: Instant,
    last: Option<Instant>,
    /// Motion is reduced: working dots hold still. Read at each render.
    still: bool,
    /// When the last frames were painted, for `arakun island fps`.
    frames: std::collections::VecDeque<Instant>,
    phase: Phase,
    /// A phase held by `arakun island` or the demo instead of the agents' own.
    held: Option<Phase>,
    feed: Feed,
    alerts: Alerts,
    /// What the island shows while a card hangs under it: the agents' phase or the call's.
    base: Phase,
    card: Option<Card>,
    /// When the last card started back in, for the island's bounce.
    melted: Option<Instant>,
    /// The confirmation card on screen, since when, and whether the pointer has
    /// been off the island since: "Approve" counts only for a pointer that came to it.
    armed: Option<(u64, Instant, bool)>,
    /// When an agent was last busy.
    busy_at: Option<Instant>,
    hovered: bool,
    /// Whether the pointer is really on the card: its buttons show only then,
    /// the way a phone shows a notification's actions on a swipe.
    card_hover: CardHover,
    /// Bumped on every pointer change, so a delayed leave knows if it still holds.
    leaving: u64,
    /// Off after a click, so the next state shows under the pointer.
    peek: bool,
    pinned: bool,
    view: View,
    prev: Option<(View, Instant)>,
    from: f32,
    progress: f32,
    w: Spring,
    h: Spring,
    region: Option<Vec<[i32; 4]>>,
    /// The top edge of the screen the island is on.
    edge: Edge,
    /// Whether the pointer is on the island, as `track_pointer` last saw it,
    /// and when it last looked at the screens.
    #[cfg(target_os = "macos")]
    pointer_on: bool,
    #[cfg(target_os = "macos")]
    screens_at: Instant,
    call: Option<CallView>,
    /// A meeting being recorded: its levels and the one the green wave shows.
    recording: Option<(Arc<Levels>, f32)>,
    small: Entity<Mascot>,
    big: Entity<Mascot>,
}

impl EventEmitter<IslandEvent> for Island {}

impl Island {
    pub fn new(cx: &mut Context<Self>) -> Self {
        // Times alerts out and lets the raccoon doze off; redraws only on a change.
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor()
                    .timer(Duration::from_millis(500))
                    .await;
                let alive = this.update(cx, |island, cx| {
                    // A card waits while it is really hovered; one that just
                    // sits under a resting pointer times out as usual.
                    let hold = island.hovered && (island.card.is_none() || island.card_is_hot());
                    if island.alerts.tick(Instant::now(), hold) {
                        cx.emit(IslandEvent::Dismissed);
                    }
                    island.refresh(cx);
                });
                if alive.is_err() {
                    break;
                }
            }
        })
        .detach();
        // macOS says nothing while a window ignores the pointer: ask where it is.
        #[cfg(target_os = "macos")]
        cx.spawn(async move |this, cx| {
            while let Ok(near) = this.update(cx, |island, cx| island.track_pointer(cx)) {
                let wait = if near { POINTER_NEAR } else { POINTER_FAR };
                cx.background_executor().timer(wait).await;
            }
        })
        .detach();
        let edge = screen_edge();
        let view = View::Compact(Phase::Sleeping);
        let (w, h) = view.size(edge);
        let mut island = Island {
            still: false,
            start: Instant::now(),
            frames: Default::default(),
            last: None,
            phase: Phase::Sleeping,
            held: None,
            feed: Feed::default(),
            alerts: Alerts::default(),
            base: Phase::Sleeping,
            card: None,
            melted: None,
            armed: None,
            busy_at: None,
            hovered: false,
            card_hover: CardHover::default(),
            leaving: 0,
            peek: true,
            pinned: false,
            view,
            prev: None,
            from: w * 0.4,
            progress: 0.,
            w: Spring { x: w * 0.6, v: 0. },
            h: Spring { x: h, v: 0. },
            region: None,
            edge,
            #[cfg(target_os = "macos")]
            pointer_on: false,
            #[cfg(target_os = "macos")]
            screens_at: Instant::now(),
            call: None,
            recording: None,
            small: cx.new(|cx| Mascot::new(SMALL_W, cx).awake().color(gpui::white())),
            big: cx.new(|cx| Mascot::new(BIG_W, cx).awake().color(gpui::white())),
        };
        island.go(Phase::Sleeping, Phase::Sleeping, cx);
        island
    }

    pub fn phase(&self) -> Phase {
        self.phase
    }

    /// The pointer, which a window that ignores it hears nothing of: lets the
    /// clicks through unless it is on the island, and tells the island when it
    /// comes and goes. Returns whether it is near, to look again soon. Now and
    /// then it also follows the screen (a lid closed, a monitor plugged in).
    #[cfg(target_os = "macos")]
    fn track_pointer(&mut self, cx: &mut Context<Self>) -> bool {
        if self.screens_at.elapsed() >= SCREENS_EVERY {
            self.screens_at = Instant::now();
            if let Some(edge) = mac::follow_screen()
                && edge != self.edge
            {
                self.edge = edge;
                cx.notify();
            }
        }
        let Some((x, y)) = mac::pointer() else {
            return false;
        };
        let on = !passthrough()
            && self.region.iter().flatten().any(|r| {
                let (x, y) = (x - r[0] as f32, y - r[1] as f32);
                x >= 0. && y >= 0. && x < r[2] as f32 && y < r[3] as f32
            });
        if on != self.pointer_on {
            self.pointer_on = on;
            mac::click_through(!on);
            self.hover(on, cx);
        }
        // Near is a margin around the island itself, so the fast poll runs
        // only while the pointer is about to reach it.
        self.region.iter().flatten().any(|r| {
            let (x, y) = (x - r[0] as f32, y - r[1] as f32);
            x >= -POINTER_MARGIN
                && y >= -POINTER_MARGIN
                && x < r[2] as f32 + POINTER_MARGIN
                && y < r[3] as f32 + POINTER_MARGIN
        })
    }

    /// New data from the shell; redraws only when something shown changed.
    pub fn set_feed(&mut self, feed: Feed, cx: &mut Context<Self>) {
        let now = Instant::now();
        if feed.rows.iter().any(|r| r.status.active()) {
            self.busy_at = Some(now);
        }
        // Gone stale: the user is on it, or the question was answered.
        let rows = &feed.rows;
        self.alerts.retain(now, |a| {
            a.kind == AlertKind::Confirm
                || feed.viewing.as_ref() != Some(&a.key)
                    && (a.kind != AlertKind::Input
                        || rows
                            .iter()
                            .any(|r| r.key == a.key && r.status == Status::Waiting))
        });
        if feed != self.feed {
            self.feed = feed;
            cx.notify();
        }
        self.refresh(cx);
    }

    pub fn alert(&mut self, alert: Alert, cx: &mut Context<Self>) {
        self.alerts.push(alert, Instant::now());
        self.busy_at = Some(Instant::now());
        self.refresh(cx);
    }

    /// Drops the alerts no longer true: an answered request, a gone app's sessions.
    pub fn retain_alerts(&mut self, keep: impl Fn(&Alert) -> bool, cx: &mut Context<Self>) {
        self.alerts.retain(Instant::now(), keep);
        self.refresh(cx);
    }

    /// Nothing ran here: the raccoon may sleep right away.
    pub fn forget_busy(&mut self, cx: &mut Context<Self>) {
        self.busy_at = None;
        self.refresh(cx);
    }

    /// Shows `phase` whatever the agents do, until `None`; a held alert needs one queued.
    pub fn hold(&mut self, phase: Option<Phase>, cx: &mut Context<Self>) {
        self.held = phase;
        self.peek = false;
        self.refresh(cx);
    }

    pub fn pin(&mut self, on: bool, cx: &mut Context<Self>) {
        self.pinned = on;
        self.refresh(cx);
        cx.notify();
    }

    pub fn pinned(&self) -> bool {
        self.pinned
    }

    /// A call starts (with the levels its waveform follows) or ends.
    #[cfg(feature = "coderlabs")]
    pub fn set_call(&mut self, levels: Option<Arc<Levels>>, cx: &mut Context<Self>) {
        self.call = levels.map(|levels| CallView {
            phase: Phase::Listening,
            user: SharedString::default(),
            assistant: SharedString::default(),
            levels,
            shown: 0.,
        });
        self.peek = false;
        self.refresh(cx);
        cx.notify();
    }

    /// Recording a meeting: the closed island shows a green wave instead of its dot.
    #[cfg(feature = "coderlabs")]
    pub fn set_recording(&mut self, levels: Option<Arc<Levels>>, cx: &mut Context<Self>) {
        self.recording = levels.map(|l| (l, 0.));
        cx.notify();
    }

    #[cfg(feature = "coderlabs")]
    pub fn call_phase(&mut self, phase: Phase, cx: &mut Context<Self>) {
        if let Some(c) = &mut self.call
            && c.phase != phase
        {
            c.phase = phase;
            self.refresh(cx);
        }
    }

    #[cfg(feature = "coderlabs")]
    pub fn captions(&mut self, user: &str, assistant: &str, cx: &mut Context<Self>) {
        if let Some(c) = &mut self.call {
            c.user = user.to_string().into();
            c.assistant = assistant.to_string().into();
            cx.notify();
        }
    }

    /// Follows the agents, the held phase and the alert queue.
    fn refresh(&mut self, cx: &mut Context<Self>) {
        let alert = self.alerts.front().is_some();
        let quiet = self.busy_at.map(|t| t.elapsed());
        let rows = &self.feed.rows;
        let phase = match self.held {
            Some(Phase::Alert) if !alert => phase_for(rows, false, quiet),
            Some(p) => p,
            // In a call the voice reads the cards out; hovering shows the one waiting.
            None => match &self.call {
                Some(c) if !alert || !self.hovered => c.phase,
                _ => phase_for(rows, alert, quiet),
            },
        };
        // Under a card the island keeps showing the agents, or the call.
        let base = match &self.call {
            _ if phase != Phase::Alert => phase,
            Some(c) => c.phase,
            None => phase_for(rows, false, quiet),
        };
        if (phase, base) != (self.phase, self.base) {
            self.go(phase, base, cx);
        } else {
            // Waiting turned working and so on; a mood it already has is a no-op.
            let mood = self.small_mood();
            self.small.update(cx, |m, _| m.set_mood(mood));
        }
        self.sync_card(cx);
    }

    /// Starts the front alert's card dripping out, or melts the one out back
    /// in once its alert went. The next drips only after the last is back in.
    fn sync_card(&mut self, cx: &mut Context<Self>) {
        let now = Instant::now();
        let want = self
            .alerts
            .front()
            .filter(|_| self.phase == Phase::Alert && !self.pinned)
            .map(|(seq, a)| (seq, a.clone()));
        match &mut self.card {
            Some(card) => {
                if card.gone.is_none() && want.as_ref().map(|w| w.0) != Some(card.seq) {
                    card.gone = Some(now);
                    self.melted = Some(now);
                    self.card_hover.reset();
                    cx.notify();
                }
            }
            None => {
                if let Some((seq, alert)) = want {
                    let mood = match alert.kind {
                        AlertKind::Input | AlertKind::Confirm => Mood::Waiting,
                        AlertKind::Error => Mood::Error,
                        _ => Mood::Done,
                    };
                    // Played again from the start for each card.
                    self.big.update(cx, |m, _| {
                        m.set_mood(Mood::Idle);
                        m.set_mood(mood);
                    });
                    self.card = Some(Card {
                        seq,
                        alert,
                        born: now,
                        gone: None,
                        layout: None,
                    });
                    self.card_hover.reset();
                    cx.notify();
                }
            }
        }
    }

    fn small_mood(&self) -> Mood {
        let has = |s: Status| self.feed.rows.iter().any(|r| r.status == s);
        match self.base {
            Phase::Sleeping => Mood::Sleeping,
            Phase::Idle | Phase::Alert => Mood::Idle,
            Phase::Working if self.call.is_some() => Mood::Thinking,
            Phase::Working
                if has(Status::Working) || !(has(Status::Waiting) || has(Status::Error)) =>
            {
                Mood::Working
            }
            Phase::Working if has(Status::Waiting) => Mood::Waiting,
            Phase::Working => Mood::Error,
            Phase::Speaking => Mood::Thinking,
            Phase::Listening => Mood::Waiting,
        }
    }

    fn go(&mut self, phase: Phase, base: Phase, cx: &mut Context<Self>) {
        self.phase = phase;
        self.base = base;
        let mood = self.small_mood();
        self.small.update(cx, |m, _| m.set_mood(mood));
        cx.notify();
    }

    fn dismiss(&mut self, cx: &mut Context<Self>) {
        self.alerts.dismiss(Instant::now());
        self.peek = false;
        if self.held == Some(Phase::Alert) {
            self.held = None;
        }
        cx.emit(IslandEvent::Dismissed);
        self.refresh(cx);
    }

    /// Whether the pointer is really on the card out, which still shows.
    fn card_is_hot(&self) -> bool {
        self.card
            .as_ref()
            .is_some_and(|c| c.gone.is_none() && self.card_hover.is_hot(c.seq))
    }

    /// Whether `seq` is the card out and its alert still the one in front:
    /// a melting card's buttons must not answer the next alert.
    fn showing(&self, seq: u64) -> bool {
        self.card
            .as_ref()
            .is_some_and(|c| c.seq == seq && c.gone.is_none())
            && self.alerts.front().is_some_and(|(n, _)| n == seq)
    }

    /// The card's "View": its session in front, and the card away.
    pub fn see(&mut self, cx: &mut Context<Self>) {
        if let Some((_, a)) = self
            .alerts
            .front()
            .filter(|(_, a)| a.kind != AlertKind::Confirm)
        {
            let key = a.key.clone();
            self.open(key, false, cx);
            self.dismiss(cx);
        }
    }

    /// The card's "Later".
    pub fn later(&mut self, cx: &mut Context<Self>) {
        if let Some((_, a)) = self
            .alerts
            .front()
            .filter(|(_, a)| a.kind != AlertKind::Confirm)
        {
            cx.emit(IslandEvent::Later(a.key.clone()));
            self.dismiss(cx);
        }
    }

    /// A confirmation's "Approve" or "Deny".
    pub fn answer(&mut self, approve: bool, cx: &mut Context<Self>) {
        if approve && !self.approvable(Instant::now()) {
            return;
        }
        if let Some((_, a)) = self
            .alerts
            .front()
            .filter(|(_, a)| a.kind == AlertKind::Confirm)
        {
            cx.emit(IslandEvent::Answer {
                key: a.key.clone(),
                req: a.req.clone(),
                approve,
            });
            self.dismiss(cx);
        }
    }

    /// A click on "Approve" is the user's only when the card has been up a
    /// moment and the pointer came to it after it appeared, not one already there.
    fn approvable(&self, now: Instant) -> bool {
        may_approve(self.armed, self.confirm_front(), now)
    }

    /// The confirmation card out, once it is.
    fn confirm_front(&self) -> Option<u64> {
        self.card
            .as_ref()
            .filter(|c| c.alert.kind == AlertKind::Confirm && self.showing(c.seq))
            .map(|c| c.seq)
    }

    /// Notes when a confirmation card shows up, and whether the pointer was on the island then.
    fn arm(&mut self, now: Instant) {
        self.armed = rearm(self.armed, self.confirm_front(), self.hovered, now);
    }

    fn open(&mut self, key: SharedString, row: bool, cx: &mut Context<Self>) {
        self.peek = false;
        cx.emit(if row {
            IslandEvent::OpenRow(key)
        } else {
            IslandEvent::Open(key)
        });
        cx.notify();
    }

    fn list_rows(&self) -> usize {
        self.feed.rows.len().min(MAX_ROWS)
    }

    fn target(&self) -> View {
        let n = self.list_rows();
        match self.base {
            // Working, in a call, is the assistant thinking.
            Phase::Speaking | Phase::Listening | Phase::Working
                if self.call.is_some() && !self.pinned =>
            {
                View::Call
            }
            // The list would grow over a card: it waits until the card is back in.
            _ if self.card.is_none() && ((self.hovered && self.peek) || self.pinned) && n > 0 => {
                View::List(n)
            }
            // The bars take the dots' place and need more room than the raccoon
            // alone; a call or a voice phase keeps its own wave.
            Phase::Sleeping | Phase::Idle | Phase::Working if self.recording.is_some() => {
                View::Recording
            }
            Phase::Working => busy(&self.feed.rows),
            p => View::Compact(p),
        }
    }

    /// Moves the shape toward the current view; returns whether it still moves.
    fn step(&mut self, dt: f32) -> bool {
        let target = self.target();
        let (tw, th) = target.size(self.edge);
        if target != self.view {
            self.prev = Some((self.view, Instant::now()));
            self.view = target;
            self.from = ((self.w.x - tw).abs() + (self.h.x - th).abs()).max(1.);
            self.progress = 0.;
        }
        self.w.step(tw, dt);
        self.h.step(th, dt);
        let left = (self.w.x - tw).abs() + (self.h.x - th).abs();
        // Never back down: the overshoot must not dim the content again.
        self.progress = self.progress.max(1. - left / self.from);
        !(self.w.settled(tw) && self.h.settled(th))
    }

    /// How far the new content has faded in, in step with the shape.
    fn content_alpha(&self) -> f32 {
        smoothstep(0.4, 0.92, self.progress)
    }

    fn prev_alpha(&self) -> Option<(View, f32)> {
        let (v, at) = self.prev?;
        let a = 1. - at.elapsed().as_secs_f32() / 0.12;
        (a > 0. && drawn_as(v, &self.feed.rows, self.recording.is_some())).then_some((v, a))
    }

    /// The pointer came or went. Leaving waits `LEAVE_GRACE` first: a screenshot
    /// tool's overlay takes the pointer away the moment it opens, and the
    /// expanded island should still be there when the screen freezes.
    fn hover(&mut self, on: bool, cx: &mut Context<Self>) {
        if !on && let Some(a) = &mut self.armed {
            a.2 = true;
        }
        self.leaving += 1;
        if on || !self.hovered {
            self.set_hover(on, cx);
            return;
        }
        let ticket = self.leaving;
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(LEAVE_GRACE).await;
            let _ = this.update(cx, |this, cx| {
                if this.leaving == ticket {
                    this.set_hover(false, cx);
                }
            });
        })
        .detach();
    }

    fn set_hover(&mut self, on: bool, cx: &mut Context<Self>) {
        if !on {
            // Leaving the window sends no leave to the card under the pointer.
            self.card_hover.reset();
        }
        if self.hovered != on {
            self.hovered = on;
            self.peek = true;
            if self.call.is_some() {
                self.refresh(cx);
            }
            cx.notify();
        }
    }

    /// A working agent's dot breathing, `phase` radians behind the others.
    /// Steady when motion is reduced.
    fn pulse(&self, t: f32, phase: f32) -> f32 {
        if self.still {
            1.
        } else {
            0.75 + 0.25 * (t * TAU / 1.6 + phase).sin()
        }
    }

    fn compact(&self, phase: Phase, t: f32) -> gpui::AnyElement {
        let right = match phase {
            Phase::Sleeping | Phase::Idle => div().into_any_element(),
            Phase::Alert => div()
                .size(px(5.))
                .rounded_full()
                .bg(hsla(0., 0., 1., 0.22))
                .into_any_element(),
            Phase::Working => {
                let (dots, more) = dots(&self.feed.rows);
                div()
                    .flex()
                    .items_center()
                    .gap(px(DOT_GAP))
                    .children(dots.into_iter().enumerate().map(|(i, s)| {
                        let pulse = match s {
                            Status::Working => self.pulse(t, i as f32),
                            _ => 1.,
                        };
                        div()
                            .size(px(DOT))
                            .rounded_full()
                            .bg(s.color().opacity(pulse))
                    }))
                    .when(more > 0, |d| {
                        d.child(
                            div()
                                .font_family(theme::FONT_MONO)
                                .text_size(px(MORE_SIZE))
                                .text_color(hsla(0., 0., 1., 0.5))
                                .child(format!("+{more}")),
                        )
                    })
                    .into_any_element()
            }
            Phase::Speaking => wave(t, false, None),
            Phase::Listening => wave(t, true, None),
        };
        // Recording shows over whatever the agents do; a call keeps its own wave.
        let right = match &self.recording {
            Some((_, shown)) if !matches!(phase, Phase::Speaking | Phase::Listening) => {
                bars(t, REC_GREEN, 1., Some(*shown), REC_BARS)
            }
            _ => right,
        };
        let label = match phase {
            Phase::Speaking => Some(("Arakun", hsla(0., 0., 1., 0.55))),
            Phase::Listening => Some(("Listening", theme::accent().opacity(0.7))),
            _ => None,
        };
        div()
            .size_full()
            .flex()
            .items_center()
            .justify_between()
            .pl(px(CONTENT_X))
            .pr(px(PAD_R))
            .child(div().when_some(label, |d, (text, color)| {
                d.text_size(px(12.5))
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(color)
                    .child(text)
            }))
            .child(right)
            .into_any_element()
    }

    /// What the card says: the raccoon or a badge, the title (up to two lines)
    /// and the text (up to four), and its buttons. Laid out at the card's full
    /// width, from its top down to its bottom as it is now (still growing or
    /// shrinking), so the buttons stay on it.
    fn card_content(
        &self,
        card: &Card,
        layout: &CardLayout,
        top: f32,
        h: f32,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let a = &card.alert;
        let session = a.kind.session();
        let confirm = a.kind == AlertKind::Confirm;
        let seq = card.seq;
        // The action is a white pill with an arrow, the way out a quiet word
        // beside it: the eye lands on what the card is for.
        let primary = |label: &'static str| {
            div()
                .id(label)
                .h(px(BUTTON_H))
                .pl(px(13.))
                .pr(px(10.))
                .flex()
                .items_center()
                .gap(px(5.))
                .rounded_full()
                .bg(gpui::white())
                .hover(|s| s.bg(hsla(0., 0., 0.86, 1.)))
                .text_size(px(12.))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(gpui::black())
                .child(label)
                .child(
                    gpui::svg()
                        .path("icons/arrow-right01.svg")
                        .size(px(13.))
                        .text_color(gpui::black()),
                )
        };
        let quiet = |label: &'static str| {
            div()
                .id(label)
                .h(px(BUTTON_H))
                .px(px(11.))
                .flex()
                .items_center()
                .rounded_full()
                .hover(|s| s.bg(hsla(0., 0., 1., 0.1)))
                .text_size(px(12.))
                .font_weight(FontWeight::MEDIUM)
                .text_color(hsla(0., 0., 1., 0.62))
                .child(label)
        };
        let face = if session {
            div()
                .flex_none()
                .w(px(BIG_W))
                .child(self.big.clone())
                .into_any_element()
        } else if !a.image.is_empty() {
            avatar(a).into_any_element()
        } else if !a.brand.is_empty() {
            brand_face(&a.brand).into_any_element()
        } else {
            badge(a.kind).into_any_element()
        };
        let room = layout.w - CardLayout::text_x(a) - 18.;
        let title = div()
            .w(px(room))
            .text_size(px(TITLE_SIZE))
            .line_height(px(TITLE_LH))
            .font_weight(FontWeight::SEMIBOLD)
            .text_color(gpui::white())
            .line_clamp(TITLE_LINES)
            .text_ellipsis()
            .child(layout.title.clone());
        let body = div()
            .w(px(room))
            .text_size(px(BODY_SIZE))
            .line_height(px(BODY_LH))
            .text_color(hsla(0., 0., 1., 0.66))
            .line_clamp(BODY_LINES)
            .text_ellipsis()
            .child(layout.body.clone());
        let text = div()
            .flex()
            .flex_col()
            .when(!layout.pill && layout.body_lines > 0, |d| d.gap(px(3.)))
            .child(title)
            .when(layout.body_lines > 0, |d| d.child(body));
        // Under the text, not in the far corner, so the card has no empty
        // stretch at the bottom left.
        let indent = CardLayout::text_x(a) - if session { 18. } else { 14. };
        let hot = self.card_hover.is_hot(seq);
        let buttons = (a.buttons() && (confirm || hot)).then(|| {
            div()
                .flex()
                .items_center()
                .when(confirm, |d| d.pl(px(indent)))
                .gap(px(4.))
                .children(card_buttons(confirm).map(|(label, action)| {
                    let pill = if action { primary(label) } else { quiet(label) };
                    pill.on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _, _, cx| {
                            cx.stop_propagation();
                            if this.showing(seq) {
                                match (confirm, action) {
                                    (true, approve) => this.answer(approve, cx),
                                    (false, true) => this.see(cx),
                                    (false, false) => this.later(cx),
                                }
                            }
                        }),
                    )
                }))
        });
        div()
            .absolute()
            .left(px(WIN_W / 2. - layout.w / 2.))
            .top(px(top))
            .w(px(layout.w))
            .h(px(h))
            .flex()
            .flex_col()
            .when(layout.pill, |d| d.justify_center())
            .when(!layout.pill, |d| {
                d.justify_between().pt(px(14.)).pb(px(14.))
            })
            .pl(px(if session { 18. } else { 14. }))
            .pr(px(18.))
            .id(("card", seq as usize))
            .on_hover(cx.listener(move |this, on: &bool, _, cx| {
                if !*on && this.card_hover.reset() {
                    cx.notify();
                }
            }))
            // Hot only after the pointer moves on the card, and not in the
            // card's first moments, when it may have just dropped under it.
            .on_mouse_move(cx.listener(move |this, ev: &gpui::MouseMoveEvent, _, cx| {
                let settled = this.card.as_ref().is_some_and(|c| {
                    c.seq == seq && c.gone.is_none() && c.born.elapsed() > CARD_SETTLE
                });
                if this.card_hover.moved(seq, ev.position, settled) {
                    cx.notify();
                }
            }))
            // A click anywhere on a notice opens what it is about, like "View".
            .when(a.buttons() && !confirm, |d| {
                d.cursor_pointer().on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, _, _, cx| {
                        cx.stop_propagation();
                        // Only a pointer that came to the card opens it; a
                        // click meant for the window below does nothing.
                        if this.showing(seq) && this.card_hover.is_hot(seq) {
                            this.see(cx)
                        }
                    }),
                )
            })
            .child(
                div()
                    .flex()
                    .gap(px(12.))
                    // A notice's picture sits centered on its text; the raccoon
                    // of a session card stays at the top.
                    .when(layout.pill || !session, |d| d.items_center())
                    .child(face)
                    .child(text),
            )
            .map(|d| match buttons {
                Some(row) if confirm => d.child(row),
                // Over the end of the text, which fades into the card's black.
                Some(row) => d.child(
                    div()
                        .absolute()
                        .top_0()
                        .right_0()
                        .h_full()
                        .flex()
                        .items_center()
                        .pl(px(44.))
                        .pr(px(14.))
                        .rounded_r(px(layout.h.min(28.)))
                        .bg(gpui::linear_gradient(
                            90.,
                            gpui::linear_color_stop(gpui::black().opacity(0.), 0.),
                            gpui::linear_color_stop(gpui::black(), 0.3),
                        ))
                        // The buttons slide in from the right edge as they fade
                        // in, like a phone notification's actions on a swipe.
                        .child(
                            row.with_animation(
                                ("card-actions", seq as usize),
                                gpui::Animation::new(Duration::from_millis(280))
                                    .with_easing(gpui::ease_out_quint()),
                                |el, t| el.opacity(t).mr(px(-(1. - t) * 22.)),
                            ),
                        )
                        .with_animation(
                            ("card-actions-fade", seq as usize),
                            gpui::Animation::new(Duration::from_millis(180)),
                            |el, t| el.opacity(t),
                        ),
                ),
                None => d,
            })
            .into_any_element()
    }

    fn list(&self, t: f32, cx: &mut Context<Self>) -> gpui::AnyElement {
        let dim = hsla(0., 0., 1., 0.5);
        let n = self.feed.rows.len();
        // The last row sits ROW_INSET in from the island's bottom corners: its
        // own bottom corners follow them, concentric, instead of a fixed 10.
        let shown = n.min(MAX_ROWS);
        let (w, h) = View::List(shown).size(self.edge);
        let last_radius = (bottom_radius(self.edge, w, h) - ROW_INSET).clamp(10., ROW_H / 2.);
        let head = div()
            .h(px(HEAD_H))
            .flex_none()
            .flex()
            .items_center()
            .justify_between()
            .pl(px(14. + SMALL_W + 10.))
            .pr(px(18.))
            .child(
                div()
                    .text_size(px(13.))
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(gpui::white())
                    .child(if n == 1 {
                        "1 agent".to_string()
                    } else {
                        format!("{n} agents")
                    }),
            )
            .when_some(self.feed.today.clone(), |d, today| {
                d.child(
                    div()
                        .font_family(theme::FONT_MONO)
                        .text_size(px(11.5))
                        .text_color(dim)
                        .child(format!("{today} today")),
                )
            });
        let rows = self
            .feed
            .rows
            .iter()
            .take(MAX_ROWS)
            .enumerate()
            .map(|(i, a)| {
                let pulse = match a.status {
                    Status::Working => self.pulse(t, 0.),
                    _ => 1.,
                };
                let key = a.key.clone();
                div()
                    .id(i)
                    .h(px(ROW_H))
                    .mx(px(ROW_INSET))
                    .px(px(12.))
                    .flex()
                    .items_center()
                    .gap(px(10.))
                    .rounded(px(10.))
                    .when(i + 1 == shown, |d| d.rounded_b(px(last_radius)))
                    .hover(|s| s.bg(hsla(0., 0., 1., 0.06)))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _, _, cx| {
                            cx.stop_propagation();
                            this.open(key.clone(), true, cx);
                        }),
                    )
                    .child(
                        div()
                            .flex_none()
                            .size(px(7.))
                            .rounded_full()
                            .bg(a.status.color().opacity(pulse)),
                    )
                    .child(
                        div()
                            .flex_none()
                            .min_w(px(64.))
                            .max_w(px(130.))
                            .truncate()
                            .text_size(px(12.5))
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(gpui::white())
                            .child(a.model.clone()),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_size(px(12.))
                            .text_color(dim)
                            .truncate()
                            .child(a.doing.clone()),
                    )
                    .child(
                        div()
                            .flex_none()
                            .font_family(theme::FONT_MONO)
                            .text_size(px(11.))
                            .text_color(hsla(0., 0., 1., 0.4))
                            .child(a.elapsed.clone()),
                    )
                    .child(
                        div()
                            .flex_none()
                            .w(px(40.))
                            .flex()
                            .justify_end()
                            .font_family(theme::FONT_MONO)
                            .text_size(px(11.))
                            .text_color(hsla(0., 0., 1., 0.75))
                            .child(a.cost.clone()),
                    )
            });
        div()
            .size_full()
            .flex()
            .flex_col()
            .font_family(theme::FONT_SANS)
            .child(head)
            .children(rows)
            .into_any_element()
    }

    /// The call: who has the turn and its waveform, the user's last words
    /// (their end, as they come) and the sentence the assistant is saying.
    fn call_card(&self, t: f32) -> gpui::AnyElement {
        let Some(c) = &self.call else {
            return div().into_any_element();
        };
        let listening = c.phase == Phase::Listening;
        let (label, color) = match c.phase {
            Phase::Listening => ("Listening", theme::accent().opacity(0.8)),
            Phase::Working => ("Thinking", hsla(0., 0., 1., 0.45)),
            _ => ("Arakun", hsla(0., 0., 1., 0.6)),
        };
        let head = div()
            .h(px(HEAD_H))
            .flex_none()
            .flex()
            .items_center()
            .justify_between()
            .pl(px(12. + SMALL_W + 10.))
            .pr(px(16.))
            .child(
                div()
                    .text_size(px(12.5))
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(color)
                    .child(label),
            )
            .child(wave(t, listening, Some(c.shown)));
        div()
            .size_full()
            .flex()
            .flex_col()
            .child(head)
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(3.))
                    .px(px(18.))
                    .min_w_0()
                    .child(
                        div()
                            .h(px(17.))
                            .text_size(px(12.))
                            .text_color(hsla(0., 0., 1., 0.5))
                            .whitespace_nowrap()
                            .overflow_hidden()
                            .text_ellipsis_start()
                            .child(c.user.clone()),
                    )
                    .child(
                        div()
                            .text_size(px(13.))
                            .line_height(px(17.))
                            .text_color(gpui::white())
                            .line_clamp(2)
                            .child(c.assistant.clone()),
                    ),
            )
            .into_any_element()
    }

    fn content(&self, view: View, t: f32, cx: &mut Context<Self>) -> gpui::AnyElement {
        match view {
            View::Compact(p) => self.compact(p, t),
            View::Busy(..) => self.compact(Phase::Working, t),
            // Any phase but a voice one shows the bars while recording.
            View::Recording => self.compact(Phase::Idle, t),
            View::List(_) => self.list(t, cx),
            View::Call => self.call_card(t),
        }
    }
}

/// Audio bars, each an envelope of a few slow sines so they roll like a voice.
/// With a live `level`, a call's: they rise with the voice and rest low in silence.
/// The recording wave's green.
const REC_GREEN: gpui::Hsla = gpui::Hsla {
    h: 142. / 360.,
    s: 0.7,
    l: 0.5,
    a: 1.,
};

fn wave(t: f32, listening: bool, level: Option<f32>) -> gpui::AnyElement {
    let color = if listening {
        theme::accent()
    } else {
        gpui::white()
    };
    let speed = if listening { 0.8 } else { 1.25 };
    bars(t, color, speed, level, 14)
}

/// `n` bars moving like a voice; with `level`, as loud as it.
fn bars(t: f32, color: gpui::Hsla, speed: f32, level: Option<f32>, n: usize) -> gpui::AnyElement {
    div()
        .h(px(BARS_H))
        .flex()
        .items_center()
        .gap(px(BAR_GAP))
        .children((0..n).map(|i| {
            let x = i as f32 / (n - 1) as f32;
            let s = t * speed;
            let v = 0.5
                + 0.22 * (s * 7.1 + x * 5.3).sin()
                + 0.18 * (s * 11.3 - x * 9.7).sin()
                + 0.1 * (s * 17.9 + x * 13.1).sin();
            // Taper toward both ends, and breathe as a whole.
            let env = (x * std::f32::consts::PI).sin().powf(0.45) * (0.8 + 0.2 * (s * 2.3).sin());
            // With a real level, silence is flat bars: no made-up motion when
            // nobody talks. Without one (a phase shown, not measured) it just moves.
            let gain = level.map_or(1.25, |l| if l < 0.03 { 0. } else { 1.6 * l });
            let h = 3. + 17. * (v * env * gain).clamp(0., 1.);
            div().w(px(BAR_W)).h(px(h)).rounded_full().bg(color)
        }))
        .into_any_element()
}

/// `ARAKUN_ISLAND_PASSTHROUGH=1`: a test island that clicks go through, so
/// the user's clicks on their own island don't land on it.
fn passthrough() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("ARAKUN_ISLAND_PASSTHROUGH").is_some_and(|v| v == "1"))
}

fn smoothstep(a: f32, b: f32, x: f32) -> f32 {
    let t = ((x - a) / (b - a)).clamp(0., 1.);
    t * t * (3. - 2. * t)
}

/// The radius a floating island's top corners have at most: at rest it is a
/// pill, its ends half its height.
const PILL_R: f32 = 20.;

fn top_radius(h: f32) -> f32 {
    (h / 2.).min(PILL_R)
}

/// The radius of the island's bottom corners at this size.
fn bottom_radius(edge: Edge, w: f32, h: f32) -> f32 {
    let r = (12. + (h - 32.) * 0.2).clamp(11., 30.);
    if edge.floating() {
        let top = top_radius(h);
        r.max(top).min(h - top).min(w / 2.)
    } else {
        r.min(h - SHOULDER.min(h / 2.)).min(w / 2.)
    }
}

/// The island, `b` being the window from the island's top down. `bow` pushes
/// the middle of its bottom edge down (up when negative).
fn shape(b: Bounds<Pixels>, edge: Edge, w: f32, h: f32, bow: f32, window: &mut Window) {
    if edge.floating() {
        pill(b, edge, w, h, bow, window);
    } else {
        hung(b, edge, w, h, bow, window);
    }
}

/// The island on a plain edge or a notch: concave shoulders flaring into the
/// top edge, straight sides, rounded bottom corners that grow with the height.
fn hung(b: Bounds<Pixels>, edge: Edge, w: f32, h: f32, bow: f32, window: &mut Window) {
    let o = b.origin;
    let p = |x: f32, y: f32| point(o.x + px(x), o.y + px(y));
    let (x0, x1) = (WIN_W / 2. - w / 2., WIN_W / 2. + w / 2.);
    let s = SHOULDER.min(h / 2.);
    let r = bottom_radius(edge, w, h);
    let arc = |pb: &mut PathBuilder, rad: f32, sweep: bool, to| {
        pb.arc_to(point(px(rad), px(rad)), px(0.), false, sweep, to)
    };

    let mut fill = PathBuilder::fill();
    fill.move_to(p(x0 - s, 0.));
    fill.line_to(p(x1 + s, 0.));
    arc(&mut fill, s, false, p(x1, s));
    fill.line_to(p(x1, h - r));
    arc(&mut fill, r, true, p(x1 - r, h));
    bowed(&mut fill, &p, x1 - r, x0 + r, h, bow);
    arc(&mut fill, r, true, p(x0, h - r));
    fill.line_to(p(x0, s));
    arc(&mut fill, s, false, p(x0 - s, 0.));
    fill.close();
    if let Ok(path) = fill.build() {
        window.paint_path(path, gpui::black());
    }

    // A hairline along the sides and bottom, just inside the black.
    let i = 0.5;
    let mut line = PathBuilder::stroke(px(1.));
    line.move_to(p(x0 + i, s + 2.));
    line.line_to(p(x0 + i, h - r));
    arc(&mut line, r - i, false, p(x0 + r, h - i));
    bowed(&mut line, &p, x0 + r, x1 - r, h - i, bow);
    arc(&mut line, r - i, false, p(x1 - i, h - r));
    line.line_to(p(x1 - i, s + 2.));
    if let Ok(path) = line.build() {
        window.paint_path(path, hsla(0., 0., 1., 0.06));
    }
}

/// The island around a screen's own Dynamic Island: it floats under the top
/// edge like that one, round all the way, and its hairline goes all around.
fn pill(b: Bounds<Pixels>, edge: Edge, w: f32, h: f32, bow: f32, window: &mut Window) {
    let o = b.origin;
    let p = |x: f32, y: f32| point(o.x + px(WIN_W / 2. + x), o.y + px(y));
    let arc = |pb: &mut PathBuilder, rad: f32, to| {
        pb.arc_to(point(px(rad), px(rad)), px(0.), false, true, to)
    };
    let (top, bottom) = (top_radius(h), bottom_radius(edge, w, h));
    let outline = |pb: &mut PathBuilder, i: f32| {
        let (x0, x1, y0, y1) = (-w / 2. + i, w / 2. - i, i, h - i);
        let (rt, rb) = ((top - i).max(0.), (bottom - i).max(0.));
        pb.move_to(p(x0 + rt, y0));
        pb.line_to(p(x1 - rt, y0));
        arc(pb, rt, p(x1, y0 + rt));
        pb.line_to(p(x1, y1 - rb));
        arc(pb, rb, p(x1 - rb, y1));
        bowed(pb, &p, x1 - rb, x0 + rb, y1, bow);
        arc(pb, rb, p(x0, y1 - rb));
        pb.line_to(p(x0, y0 + rt));
        arc(pb, rt, p(x0 + rt, y0));
        pb.close();
    };
    let mut fill = PathBuilder::fill();
    outline(&mut fill, 0.);
    if let Ok(path) = fill.build() {
        window.paint_path(path, gpui::black());
    }
    let mut line = PathBuilder::stroke(px(1.));
    outline(&mut line, 0.5);
    if let Ok(path) = line.build() {
        window.paint_path(path, hsla(0., 0., 1., 0.06));
    }
}

/// A horizontal edge from the current point at `xa` to `xb`, at height `y`,
/// its middle pushed down by `bow`: flat at both ends, deepest in the middle.
fn bowed(
    pb: &mut PathBuilder,
    p: &impl Fn(f32, f32) -> Point<Pixels>,
    xa: f32,
    xb: f32,
    y: f32,
    bow: f32,
) {
    if bow.abs() < 0.05 {
        pb.line_to(p(xb, y));
        return;
    }
    let (xm, q) = ((xa + xb) / 2., (xb - xa) / 4.);
    pb.cubic_bezier_to(p(xm, y + bow), p(xa + q, y), p(xm - q, y + bow));
    pb.cubic_bezier_to(p(xb, y), p(xm + q, y + bow), p(xb - q, y));
}

/// The card's black: a rounded rectangle whose top edge bows up by `bow`,
/// and its hairline at `edge` of the island's.
fn card_shape(b: Bounds<Pixels>, c: Rounded, bow: f32, edge: f32, window: &mut Window) {
    let o = b.origin;
    let p = |x: f32, y: f32| point(o.x + px(WIN_W / 2. + x), o.y + px(y));
    let arc = |pb: &mut PathBuilder, rad: f32, to| {
        pb.arc_to(point(px(rad), px(rad)), px(0.), false, true, to)
    };
    let outline = |pb: &mut PathBuilder, i: f32| {
        let (x0, x1, y0, y1, r) = (
            -c.hw + i,
            c.hw - i,
            c.top + i,
            c.bottom - i,
            (c.r - i).max(0.),
        );
        pb.move_to(p(x0 + r, y0));
        bowed(pb, &p, x0 + r, x1 - r, y0, -bow);
        arc(pb, r, p(x1, y0 + r));
        pb.line_to(p(x1, y1 - r));
        arc(pb, r, p(x1 - r, y1));
        pb.line_to(p(x0 + r, y1));
        arc(pb, r, p(x0, y1 - r));
        pb.line_to(p(x0, y0 + r));
        arc(pb, r, p(x0 + r, y0));
        pb.close();
    };
    let mut fill = PathBuilder::fill();
    outline(&mut fill, 0.);
    if let Ok(path) = fill.build() {
        window.paint_path(path, gpui::black());
    }
    if edge > 0.01 {
        let mut line = PathBuilder::stroke(px(1.));
        outline(&mut line, 0.5);
        if let Ok(path) = line.build() {
            window.paint_path(path, hsla(0., 0., 1., 0.06 * edge));
        }
    }
}

/// The neck between island and card: on each side the arc of a ball of
/// radius `r` wedged between them, joined through both shapes' insides
/// (`inset` deep, past their bowed edges). The same black as both, so no
/// seam shows; it is painted over their hairlines, which end where it starts.
fn neck(
    b: Bounds<Pixels>,
    island: Rounded,
    card: Rounded,
    r: f32,
    inset: f32,
    window: &mut Window,
) {
    // Joined weakly, only the middle holds: the neck narrows to a thread before it snaps.
    let gap = card.top - island.bottom;
    let hold = if gap <= 0. {
        1.
    } else {
        smoothstep(0., 1., 1. - gap / (2. * r))
    };
    let Some([c, i, k]) = wedge(island.narrowed(hold), card.narrowed(hold), r) else {
        return;
    };
    let o = b.origin;
    let p = |(x, y): (f32, f32)| point(o.x + px(WIN_W / 2. + x), o.y + px(y));
    let mirror = |(x, y): (f32, f32)| (-x, y);
    let inner = |(x, y): (f32, f32)| (x - (c.0 - x) / r * inset, y - (c.1 - y) / r * inset);
    let angle = |(x, y): (f32, f32)| (y - c.1).atan2(x - c.0);
    let large = (angle(i) - angle(k)).rem_euclid(TAU) > PI;
    let arc = |pb: &mut PathBuilder, to| pb.arc_to(point(px(r), px(r)), px(0.), large, false, to);

    let mut fill = PathBuilder::fill();
    fill.move_to(p(mirror(inner(i))));
    fill.line_to(p(inner(i)));
    fill.line_to(p(i));
    arc(&mut fill, p(k));
    fill.line_to(p(inner(k)));
    fill.line_to(p(mirror(inner(k))));
    fill.line_to(p(mirror(k)));
    arc(&mut fill, p(mirror(i)));
    fill.close();
    if let Ok(path) = fill.build() {
        window.paint_path(path, gpui::black());
    }
}

/// A sender's picture in the badge's place. A success, warning or failure
/// still shows as one: a ring in its color and a dot on the corner, so a
/// teammate's win and their bug don't look the same.
fn avatar(alert: &Alert) -> impl IntoElement {
    let kind = alert.kind;
    let tinted = matches!(
        kind,
        AlertKind::Success | AlertKind::Warning | AlertKind::Failure
    );
    // A notice from a service (GitHub) gets a white ring instead: the user
    // knows what it is, and the picture stays whole and centered.
    let branded = !tinted && !alert.brand.is_empty();
    let ringed = tinted || branded;
    let color = if branded { gpui::white() } else { kind.badge() };
    // A black gap between ring and picture, so the ring reads on any photo.
    let pic = gpui::img(std::path::PathBuf::from(alert.image.as_ref()))
        .size_full()
        .rounded_full()
        .when(ringed, |i| i.border_2().border_color(gpui::black()));
    let mark = canvas(
        |_, _, _| {},
        move |b, _, window, _| {
            let c = b.center();
            let p = |x: f32, y: f32| point(c.x + px(x), c.y + px(y));
            let opts = StrokeOptions::default()
                .with_line_width(1.8)
                .with_line_cap(LineCap::Round)
                .with_line_join(LineJoin::Round);
            let mut line = PathBuilder::fill().with_style(PathStyle::Stroke(opts));
            if kind == AlertKind::Success {
                line.move_to(p(-3., 0.3));
                line.line_to(p(-0.9, 2.4));
                line.line_to(p(3.2, -2.4));
            } else {
                line.move_to(p(0., -3.2));
                line.line_to(p(0., 0.6));
                line.move_to(p(0., 3.));
                line.line_to(p(0., 3.1));
            }
            if let Ok(path) = line.build() {
                window.paint_path(path, gpui::black());
            }
        },
    );
    div()
        .flex_none()
        .relative()
        .size(px(AVATAR))
        .rounded_full()
        .when(ringed, |d| d.border_2().border_color(color))
        .child(pic)
        .when(tinted, |d| {
            // Deferred: painted after the ring and the picture, always on top.
            d.child(
                gpui::deferred(
                    div()
                        .absolute()
                        .right(px(-5.))
                        .bottom(px(-5.))
                        .size(px(16.))
                        .rounded_full()
                        .bg(color)
                        .border_2()
                        .border_color(gpui::black())
                        .child(mark.size_full()),
                )
                .with_priority(1),
            )
        })
}

/// A card's buttons, left to right, and whether each is the action. The
/// action stays on the right, where "Approve" has always been: swapping it
/// with "Deny" would turn a habit into the wrong answer. On hover the row
/// sits at the card's right edge, so "View" lands under the thumb too.
fn card_buttons(confirm: bool) -> [(&'static str, bool); 2] {
    if confirm {
        [("Deny", false), ("Approve", true)]
    } else {
        [("Later", false), ("View", true)]
    }
}

/// The service's logo file under `icons/`, for a notice's `brand`.
fn brand_icon(brand: &str) -> Option<&'static str> {
    match brand {
        "github" => Some("icons/github.svg"),
        _ => None,
    }
}

/// A notice from a service with no picture of who sent it: the service's
/// logo, white on black like the island, so it reads at a glance.
fn brand_face(brand: &str) -> impl IntoElement {
    div()
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
        .size(px(AVATAR))
        .rounded_full()
        .bg(gpui::white())
        .when_some(brand_icon(brand), |d, path| {
            d.child(
                gpui::svg()
                    .path(path)
                    .size(px(20.))
                    .text_color(gpui::black()),
            )
        })
}

/// A notice's badge: a check for a success, "!" for a warning or a failure, "i" otherwise.
fn badge(kind: AlertKind) -> impl IntoElement {
    let color = kind.badge();
    div()
        .flex_none()
        .size(px(BADGE))
        .rounded_full()
        .bg(color.opacity(0.18))
        .child(
            canvas(
                |_, _, _| {},
                move |b, _, window, _| {
                    let c = b.center();
                    let p = |x: f32, y: f32| point(c.x + px(x), c.y + px(y));
                    let opts = StrokeOptions::default()
                        .with_line_width(2.2)
                        .with_line_cap(LineCap::Round)
                        .with_line_join(LineJoin::Round);
                    let mut line = PathBuilder::fill().with_style(PathStyle::Stroke(opts));
                    let dot = match kind {
                        AlertKind::Success => {
                            line.move_to(p(-5., 0.5));
                            line.line_to(p(-1.5, 4.));
                            line.line_to(p(5.5, -4.));
                            None
                        }
                        AlertKind::Warning | AlertKind::Failure => {
                            line.move_to(p(0., -5.5));
                            line.line_to(p(0., 1.5));
                            Some(5.)
                        }
                        _ => {
                            line.move_to(p(0., -0.5));
                            line.line_to(p(0., 5.5));
                            Some(-4.5)
                        }
                    };
                    if let Ok(path) = line.build() {
                        window.paint_path(path, color);
                    }
                    if let Some(y) = dot {
                        let r = px(1.4);
                        let mut d = PathBuilder::fill();
                        d.move_to(p(1.4, y));
                        d.arc_to(point(r, r), px(0.), false, true, p(-1.4, y));
                        d.arc_to(point(r, r), px(0.), false, true, p(1.4, y));
                        d.close();
                        if let Ok(path) = d.build() {
                            window.paint_path(path, color);
                        }
                    }
                },
            )
            .size_full(),
        )
}

fn may_approve(armed: Option<(u64, Instant, bool)>, front: Option<u64>, now: Instant) -> bool {
    matches!(armed, Some((n, at, came)) if Some(n) == front && came && now.duration_since(at) >= ARM_AFTER)
}

/// The arming of the card in front: kept while it is the same card, reset for a new one.
fn rearm(
    armed: Option<(u64, Instant, bool)>,
    front: Option<u64>,
    hovered: bool,
    now: Instant,
) -> Option<(u64, Instant, bool)> {
    match front {
        Some(n) if armed.map(|a| a.0) != Some(n) => Some((n, now, !hovered)),
        Some(_) => armed,
        None => None,
    }
}

impl Island {
    /// Frames painted in the last two seconds and the longest gap between them.
    pub fn fps_report(&self) -> String {
        let now = Instant::now();
        let recent: Vec<Instant> = self
            .frames
            .iter()
            .copied()
            .filter(|&t| now - t < Duration::from_secs(2))
            .collect();
        let gap = recent
            .windows(2)
            .map(|w| w[1] - w[0])
            .max()
            .unwrap_or_default();
        format!(
            "{} frames in 2 s, longest gap {} ms",
            recent.len(),
            gap.as_millis()
        )
    }
}

impl Render for Island {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.arm(Instant::now());
        let now = Instant::now();
        let dt = self
            .last
            .map_or(1. / 120., |l| (now - l).as_secs_f32().min(0.1));
        self.last = Some(now);
        self.frames.push_back(now);
        while self.frames.len() > 400 {
            self.frames.pop_front();
        }
        let moving = self.step(dt);
        if let Some(c) = &mut self.call {
            let live = c.levels.get(c.phase == Phase::Listening);
            // Up fast, down slow, like a meter.
            let k = if live > c.shown {
                1. - (-dt / 0.04).exp()
            } else {
                1. - (-dt / 0.18).exp()
            };
            c.shown += (live - c.shown) * k;
        }
        if let Some((levels, shown)) = &mut self.recording {
            // Either side talking moves it.
            let live = levels.get(true).max(levels.get(false));
            let k = if live > *shown {
                1. - (-dt / 0.04).exp()
            } else {
                1. - (-dt / 0.18).exp()
            };
            *shown += (live - *shown) * k;
        }
        let t = self.start.elapsed().as_secs_f32();
        let (w, h) = (self.w.x, self.h.x);
        let x0 = WIN_W / 2. - w / 2.;
        // What follows is laid out from the island's top, `oy` under the window's.
        let edge = self.edge;
        let (oy, cut_h) = (edge.top(), edge.cut_h());

        // The card: measured once, removed when back in (and the next one let out).
        if let Some(card) = &mut self.card
            && card.layout.is_none()
        {
            card.layout = Some(CardLayout::measure(&card.alert, window, cx));
        }
        if self
            .card
            .as_ref()
            .and_then(|c| c.gone)
            .is_some_and(|at| at.elapsed().as_secs_f32() >= MELT_FOR)
        {
            self.card = None;
            self.card_hover.reset();
            self.sync_card(cx);
        }
        let card = self.card.as_ref().and_then(|c| {
            let l = c.layout.as_ref()?;
            let g = card_geom(
                l.w,
                l.h,
                w,
                h,
                c.born.elapsed().as_secs_f32(),
                c.gone.map(|at| at.elapsed().as_secs_f32()),
            );
            Some((c, l, g))
        });
        let bounced = self
            .melted
            .map_or(0., |at| bounce(at.elapsed().as_secs_f32()));
        let ih = h + bounced;
        let island = Rounded {
            hw: w / 2.,
            top: -100.,
            bottom: ih,
            r: bottom_radius(edge, w, ih),
        };
        let shapes = card.map(|(_, l, g)| {
            let box_ = Rounded {
                hw: g.w / 2.,
                top: g.top,
                bottom: g.top + g.h,
                r: card_radius(l.h, g.w, g.h),
            };
            // The hairline shows only once the card hangs free.
            (box_, g, smoothstep(0., CARD_GAP, g.top - ih))
        });
        let card_box = card
            .filter(|(c, ..)| c.gone.is_none())
            .map(|(_, _, g)| Bounds {
                origin: point(px(WIN_W / 2. - g.w / 2.), px(oy + g.top)),
                size: size(px(g.w), px(g.h)),
            });

        // The gap over a floating island is its own too: the pointer thrown
        // at the top of the screen lands on it.
        let mut region = vec![[
            x0.floor() as i32,
            0,
            w.ceil() as i32 + 1,
            (oy + ih).ceil() as i32 + 1,
        ]];
        if let Some(b) = card_box {
            let (x, y) = (b.origin.x.as_f32(), b.origin.y.as_f32());
            region.push([
                x.floor() as i32,
                y.floor() as i32,
                b.size.width.as_f32().ceil() as i32 + 1,
                b.size.height.as_f32().ceil() as i32 + 1,
            ]);
        }
        if self.region.as_ref() != Some(&region) {
            let rects: Vec<_> = if passthrough() {
                vec![[0; 4]]
            } else {
                region.clone()
            }
            .into_iter()
            .map(|r| Bounds {
                origin: point(px(r[0] as f32), px(r[1] as f32)),
                size: size(px(r[2] as f32), px(r[3] as f32)),
            })
            .collect();
            window.set_input_region(Some(&rects));
            self.region = Some(region);
        }
        // Frames only while the shape moves or something pulses.
        self.still = cx.reduce_motion();
        let pulsing = !self.still && self.feed.rows.iter().any(|r| r.status == Status::Working);
        let animated = match self.view {
            View::Compact(Phase::Speaking | Phase::Listening) | View::Call => true,
            View::Compact(_) | View::Recording if self.recording.is_some() => true,
            View::Compact(Phase::Working) | View::Busy(..) | View::List(_) => pulsing,
            _ => false,
        };
        let card_moving = self
            .card
            .as_ref()
            .is_some_and(|c| c.gone.is_some() || c.born.elapsed().as_secs_f32() < 1.5)
            || self
                .melted
                .is_some_and(|at| at.elapsed().as_secs_f32() < 1.2);
        if moving || animated || card_moving || self.prev_alpha().is_some() {
            window.request_animation_frame();
        } else {
            // At rest: the next change starts its clock fresh instead of jumping by the idle time.
            self.last = None;
        }

        let weak = cx.weak_entity();
        let alpha = self.content_alpha();
        let layer = |view: View, a: f32, el: gpui::AnyElement| {
            let (tw, th) = view.size(edge);
            // An open view's rows start under the camera cutout.
            let under = if view.open() { cut_h } else { 0. };
            div()
                .absolute()
                .left(px(WIN_W / 2. - tw / 2.))
                .top(px(oy + under + (1. - a) * -4.))
                .w(px(tw))
                .h(px(th - under))
                .opacity(a)
                .child(el)
        };
        let mut layers = Vec::new();
        if let Some((v, a)) = self.prev_alpha() {
            layers.push(layer(v, a, self.content(v, t, cx)));
        } else if alpha > 0. {
            layers.push(layer(self.view, alpha, self.content(self.view, t, cx)));
        }
        // The card's text fades in once it let go, rising a little.
        let content = card.filter(|(.., g)| g.alpha > 0.01).map(|(c, l, g)| {
            let lift = (1. - g.alpha) * 4.;
            div()
                .opacity(g.alpha)
                .child(self.card_content(c, l, oy + g.top + lift, g.h - lift, cx))
        });

        let mh = SMALL_W * 166. / 212.;
        let face = div()
            .absolute()
            .left(px(x0 + FACE_X))
            // Beside the cutout while the island is compact; as it opens, down
            // to the head of what it shows under the cutout.
            .top(px(oy
                + (h - HEAD_H).clamp(0., cut_h)
                + ((h.min(HEAD_H) - mh) / 2.).max(0.)))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, _, cx| {
                    cx.stop_propagation();
                    this.peek = false;
                    cx.emit(IslandEvent::Face);
                }),
            )
            .child(self.small.clone());

        div()
            .size_full()
            .font_family(theme::FONT_SANS)
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|_, _, _, cx| cx.emit(IslandEvent::Pressed)),
            )
            .child(
                canvas(
                    |_, _, _| {},
                    move |b, _, window, _| {
                        let b = Bounds {
                            origin: point(b.origin.x, b.origin.y + px(oy)),
                            size: b.size,
                        };
                        let bow = shapes.map_or(0., |(_, g, _)| 0.6 * g.bulge);
                        shape(b, edge, w, ih, bow, window);
                        if let Some((c, g, edge)) = shapes {
                            card_shape(b, c, g.bulge, edge, window);
                            neck(b, island, c, NECK_R * g.join, 1.5 + g.bulge.abs(), window);
                        }
                        // Hover by hand: on leaving, gpui keeps the last pointer
                        // position and a growing shape would claim it again.
                        let inside = Bounds {
                            origin: point(px(x0), px(0.)),
                            size: size(px(w), px(oy + h)),
                        };
                        let on_move = weak.clone();
                        window.on_mouse_event(move |e: &MouseMoveEvent, phase, _, cx| {
                            if phase == DispatchPhase::Bubble {
                                let on = inside.contains(&e.position)
                                    || card_box.is_some_and(|c| c.contains(&e.position));
                                let _ = on_move.update(cx, |this, cx| this.hover(on, cx));
                            }
                        });
                        let on_exit = weak.clone();
                        window.on_mouse_event(move |_: &MouseExitEvent, _, _, cx| {
                            let _ = on_exit.update(cx, |this, cx| this.hover(false, cx));
                        });
                    },
                )
                .absolute()
                .size_full(),
            )
            .children(layers)
            .children(content)
            .child(face)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(key: &str, status: Status, updated: u64) -> Row {
        Row {
            key: key.to_string().into(),
            status,
            model: "GLM-5.3".into(),
            doing: SharedString::default(),
            elapsed: SharedString::default(),
            cost: SharedString::default(),
            updated,
        }
    }

    fn alert(key: &str, kind: AlertKind) -> Alert {
        Alert {
            key: key.to_string().into(),
            kind,
            title: "t".into(),
            subtitle: "s".into(),
            req: SharedString::default(),
            image: SharedString::default(),
            brand: SharedString::default(),
        }
    }

    #[test]
    fn phase_follows_the_agents() {
        let done = [row("a", Status::Done, 1)];
        assert_eq!(phase_for(&[], false, None), Phase::Sleeping);
        assert_eq!(
            phase_for(&done, false, Some(Duration::from_secs(30))),
            Phase::Idle
        );
        assert_eq!(
            phase_for(&done, false, Some(Duration::from_secs(600))),
            Phase::Sleeping
        );
        let busy = [row("a", Status::Done, 1), row("b", Status::Waiting, 1)];
        assert_eq!(phase_for(&busy, false, None), Phase::Working);
        assert_eq!(phase_for(&busy, true, None), Phase::Alert);
    }

    #[test]
    fn rows_put_what_needs_the_user_first() {
        let mut rows = vec![
            row("done", Status::Done, 9),
            row("old-work", Status::Working, 1),
            row("work", Status::Working, 5),
            row("err", Status::Error, 2),
            row("ask", Status::Waiting, 0),
        ];
        sort_rows(&mut rows);
        let keys: Vec<&str> = rows.iter().map(|r| r.key.as_ref()).collect();
        assert_eq!(keys, ["ask", "err", "work", "old-work", "done"]);
    }

    /// The content plus the island's inner padding must fit the island, so
    /// nothing spills past its edge in a state that shows dots or bars.
    #[test]
    fn dots_and_bars_fit_the_island() {
        // Content is centered up and down; this keeps it off the edges.
        const PAD_Y: f32 = 4.;
        let fits = |view: View, content_w: f32, content_h: f32| {
            let (w, h) = view.size(Edge::Flat);
            assert!(
                CONTENT_X + content_w + PAD_R <= w,
                "{view:?}: {content_w} wide needs {} but the island is {w}",
                CONTENT_X + content_w + PAD_R,
            );
            assert!(
                content_h + 2. * PAD_Y <= h,
                "{view:?}: {content_h} high needs {} but the island is {h}",
                content_h + 2. * PAD_Y,
            );
        };
        for active in 1..=40 {
            let rows: Vec<Row> = (0..active)
                .map(|i| row(&i.to_string(), Status::Working, 0))
                .collect();
            let (shown, more) = dots(&rows);
            let label = if more > 0 {
                format!("+{more}")
            } else {
                String::new()
            };
            let gaps = (shown.len() + usize::from(more > 0) - 1) as f32 * DOT_GAP;
            let w = shown.len() as f32 * DOT + label.len() as f32 * MORE_SIZE * MONO_ADVANCE + gaps;
            fits(busy(&rows), w, DOT.max(MORE_SIZE));
        }
        let w = REC_BARS as f32 * BAR_W + (REC_BARS - 1) as f32 * BAR_GAP;
        fits(View::Recording, w, BARS_H);
    }

    /// One agent becoming seven: the old view's layer would draw six dots
    /// and a "+1" in the room of one, so it doesn't fade out at all.
    #[test]
    fn an_old_view_fades_out_only_while_its_content_still_fits() {
        let working = |n: usize| -> Vec<Row> {
            (0..n)
                .map(|i| row(&i.to_string(), Status::Working, 0))
                .collect()
        };
        let one = busy(&working(1));
        let seven = working(7);
        assert_eq!(one, View::Busy(1, 0));
        assert!(drawn_as(one, &working(1), false));
        assert!(!drawn_as(one, &seven, false));
        assert!(drawn_as(busy(&seven), &seven, false));
        assert!(drawn_as(View::List(3), &working(3), false));
        assert!(!drawn_as(View::List(3), &seven, false));
        // Views whose content doesn't come from the rows always fit.
        assert!(drawn_as(View::Recording, &seven, true));
        assert!(drawn_as(View::Compact(Phase::Idle), &seven, false));
        // Recording starts while the island sleeps: the sleeping view would
        // draw the bars in the raccoon's room, so it doesn't fade out.
        let sleeping = View::Compact(Phase::Sleeping);
        assert!(drawn_as(sleeping, &[], false));
        assert!(!drawn_as(sleeping, &[], true));
        assert!(!drawn_as(one, &working(1), true));
        assert!(drawn_as(View::Compact(Phase::Working), &[], true));
    }

    #[test]
    fn a_card_is_hot_only_after_a_move_once_settled_and_until_it_changes() {
        let at = |x: f32, y: f32| point(px(x), px(y));
        let mut h = CardHover::default();
        // Moves while the card drops don't count, however far.
        assert!(!h.moved(1, at(0., 0.), false));
        assert!(!h.moved(1, at(50., 0.), false));
        // Once settled, a jitter isn't a move, a real one is.
        assert!(!h.moved(1, at(52., 0.), true));
        assert!(h.moved(1, at(60., 0.), true));
        assert!(h.is_hot(1) && !h.is_hot(2));
        // A new card under the same resting pointer starts cold.
        assert!(h.reset());
        assert!(!h.is_hot(1));
        assert!(!h.moved(2, at(60., 0.), true));
        assert!(!h.is_hot(2));
        assert!(h.moved(2, at(70., 0.), true));
        // A position seen on another card is no anchor for this one.
        h.reset();
        assert!(!h.moved(1, at(0., 0.), true));
        assert!(!h.moved(2, at(90., 0.), true));
        assert!(!h.is_hot(2));
        assert!(!h.reset());
    }

    #[test]
    fn dots_cap_at_six() {
        let mut rows: Vec<Row> = (0..8)
            .map(|i| row(&i.to_string(), Status::Working, 0))
            .collect();
        rows.push(row("x", Status::Done, 0));
        let (shown, more) = dots(&rows);
        assert_eq!((shown.len(), more), (6, 2));
        assert_eq!(dots(&rows[..3]).1, 0);
    }

    #[test]
    fn alerts_queue_replace_and_time_out() {
        let t0 = Instant::now();
        let mut q = Alerts::default();
        q.push(alert("a", AlertKind::Finished), t0);
        q.push(alert("b", AlertKind::Input), t0);
        // A newer alert of a waiting session takes its place in line.
        q.push(alert("b", AlertKind::Error), t0);
        assert_eq!(q.queue.len(), 2);
        assert_eq!(q.queue[1].1.kind, AlertKind::Error);
        assert_eq!(q.front().unwrap().1.key.as_ref(), "a");
        // Hovering holds it past its time.
        assert!(!q.tick(t0 + Duration::from_secs(20), true));
        assert!(!q.tick(t0 + Duration::from_secs(25), false));
        assert!(q.tick(t0 + Duration::from_secs(33), false));
        assert_eq!(q.front().unwrap().1.key.as_ref(), "b");
        // The next one gets its own full time.
        assert!(!q.tick(t0 + Duration::from_secs(40), false));
        q.dismiss(t0);
        assert!(q.front().is_none());
        assert!(!q.tick(t0 + Duration::from_secs(99), false));
    }

    #[test]
    fn retain_restarts_the_clock_for_a_new_front() {
        let t0 = Instant::now();
        let mut q = Alerts::default();
        q.push(alert("a", AlertKind::Input), t0);
        q.push(alert("b", AlertKind::Finished), t0);
        q.retain(t0 + Duration::from_secs(10), |a| a.key.as_ref() != "a");
        assert!(!q.tick(t0 + Duration::from_secs(15), false));
        assert!(q.tick(t0 + Duration::from_secs(22), false));
    }

    #[test]
    fn hyprland_tells_the_turned_monitor_apart() {
        let monitors: Vec<serde_json::Value> = serde_json::from_str(
            r#"[{"name":"DP-1","width":1920,"height":1080,"transform":1,"x":0,"y":0},
                {"name":"DP-2","width":1920,"height":1080,"transform":0,"x":1080,"y":420}]"#,
        )
        .unwrap();
        assert_eq!(hyprland_pick(&monitors), Some((1080., 420.)));
    }

    #[test]
    fn the_island_picks_the_landscape_screen() {
        assert_eq!(
            pick_landscape([(0, 1080., 1920.), (1, 1920., 1080.)].into_iter()),
            Some(1)
        );
        assert_eq!(pick_landscape([(0, 1080., 1920.)].into_iter()), None);
        assert_eq!(
            pick_landscape([(0, 1920., 1080.), (1, 2560., 1440.)].into_iter()),
            Some(1)
        );
    }

    #[test]
    fn approving_needs_a_pointer_that_came_and_a_moment() {
        let t0 = Instant::now();
        let late = t0 + Duration::from_secs(2);
        // A card that appears under the pointer: not approvable until it leaves and comes back.
        let a = rearm(None, Some(7), true, t0);
        assert!(!may_approve(a, Some(7), late));
        let a = a.map(|(n, at, _)| (n, at, true));
        assert!(may_approve(a, Some(7), late));
        // Too soon, even with the pointer coming in.
        let b = rearm(None, Some(8), false, t0);
        assert!(!may_approve(b, Some(8), t0 + Duration::from_millis(300)));
        assert!(may_approve(b, Some(8), late));
        // A new card re-arms; the old arming doesn't carry over.
        let c = rearm(b, Some(9), true, late);
        assert!(!may_approve(c, Some(9), late + Duration::from_secs(5)));
        assert_eq!(rearm(c, None, false, late), None);
    }

    #[test]
    fn approve_stays_right_of_reject() {
        assert_eq!(
            super::card_buttons(true),
            [("Deny", false), ("Approve", true)]
        );
        assert_eq!(
            super::card_buttons(false),
            [("Later", false), ("View", true)]
        );
    }

    #[test]
    fn a_confirmation_waits_for_its_answer() {
        let t0 = Instant::now();
        let mut q = Alerts::default();
        q.push(alert("h1", AlertKind::Confirm), t0);
        q.push(alert("a", AlertKind::Finished), t0);
        assert!(!q.tick(t0 + Duration::from_secs(3600), false));
        assert_eq!(q.front().unwrap().1.key.as_ref(), "h1");
        q.retain(t0, |a| a.kind != AlertKind::Confirm);
        assert!(q.tick(t0 + Duration::from_secs(13), false));
    }

    #[test]
    fn long_cards_stay_longer_to_be_read() {
        let mut a = alert("a", AlertKind::Finished);
        assert_eq!(alert_for(&a), Some(ALERT_FOR));
        // 42 words: 30 past the first 12, two more seconds.
        a.subtitle = vec!["palabra"; 41].join(" ").into();
        assert_eq!(alert_for(&a), Some(ALERT_FOR + Duration::from_secs(2)));
        a.subtitle = vec!["palabra"; 400].join(" ").into();
        assert_eq!(alert_for(&a), Some(ALERT_MAX));
        // A notice with nothing to open goes sooner; one with a target stays like a session's.
        assert_eq!(alert_for(&alert("", AlertKind::Success)), Some(NOTICE_FOR));
        assert_eq!(
            alert_for(&alert("open:runs", AlertKind::Failure)),
            Some(ALERT_FOR)
        );
        assert_eq!(alert_for(&alert("h1", AlertKind::Confirm)), None);
    }

    #[test]
    fn notices_get_buttons_only_with_something_to_open() {
        for kind in [
            AlertKind::Finished,
            AlertKind::Input,
            AlertKind::Error,
            AlertKind::Confirm,
        ] {
            assert!(alert("", kind).buttons());
        }
        for kind in [
            AlertKind::Success,
            AlertKind::Warning,
            AlertKind::Failure,
            AlertKind::Info,
        ] {
            assert!(!alert("", kind).buttons());
            assert!(alert("open:runs", kind).buttons());
        }
    }

    #[test]
    fn a_short_notice_times_out_sooner() {
        let t0 = Instant::now();
        let mut q = Alerts::default();
        q.push(alert("", AlertKind::Success), t0);
        assert!(!q.tick(t0 + Duration::from_secs(4), false));
        assert!(q.tick(t0 + Duration::from_secs(5), false));
    }

    #[test]
    fn the_card_drips_out_joined_and_hangs_free() {
        let (w, h, iw, ih) = (440., 150., 240., 32.);
        // Starts hidden inside the island, not yet joined.
        let g = card_geom(w, h, iw, ih, 0., None);
        assert!(g.top + g.h <= ih && g.join == 0.);
        // Joined while it drops, let go when it snaps, settled CARD_GAP under it.
        assert!(card_geom(w, h, iw, ih, snap_at() - 0.01, None).join > 0.9);
        let g = card_geom(w, h, iw, ih, snap_at() + 0.1, None);
        assert_eq!(g.join, 0.);
        let g = card_geom(w, h, iw, ih, 3., None);
        assert!((g.top - ih - CARD_GAP).abs() < 0.1 && (g.h - h).abs() < 0.1 && g.alpha == 1.);
    }

    #[test]
    fn nothing_hangs_under_the_island_once_the_card_is_in() {
        let (w, h, iw, ih) = (440., 175., 170., 30.);
        let g = card_geom(w, h, iw, ih, 5., Some(MELT_FOR));
        assert_eq!(g.join, 0.);
        assert!(g.top + g.h < ih, "the card is inside: {g:?}");
        assert!(g.w < iw);
        // The neck lets go while the card goes in: none at the end.
        let island = Rounded {
            hw: iw / 2.,
            top: -100.,
            bottom: ih,
            r: bottom_radius(Edge::Flat, iw, ih),
        };
        let card = Rounded {
            hw: g.w / 2.,
            top: g.top,
            bottom: g.top + g.h,
            r: card_radius(h, g.w, g.h),
        };
        assert!(wedge(island, card, NECK_R).is_none());
    }

    #[test]
    fn the_neck_joins_only_what_is_close() {
        let island = Rounded {
            hw: 120.,
            top: -100.,
            bottom: 32.,
            r: 12.,
        };
        let card = |top: f32| Rounded {
            hw: 220.,
            top,
            bottom: top + 150.,
            r: 26.,
        };
        // Wider apart than the ball: nothing joins them.
        assert!(wedge(island, card(32. + 2. * NECK_R + 1.), NECK_R).is_none());
        // Close: the ball sits beside the island, touching both.
        let [c, i, k] = wedge(island, card(38.), NECK_R).unwrap();
        assert!(c.0 > 100.);
        let dist = |a: (f32, f32)| (a.0 - c.0).hypot(a.1 - c.1);
        assert!((dist(i) - NECK_R).abs() < 0.1 && (dist(k) - NECK_R).abs() < 0.1);
        // A small card poking out of the bottom gets fillets on its sides.
        let drop = Rounded {
            hw: 60.,
            top: 20.,
            bottom: 60.,
            r: 20.,
        };
        let [c, _, k] = wedge(island, drop, 8.).unwrap();
        assert!((c.0 - 68.).abs() < 0.5 && (k.0 - 60.).abs() < 0.5);
    }

    #[test]
    fn elapsed_reads_like_a_clock() {
        assert_eq!(fmt_elapsed(Duration::from_secs(58)), "0:58");
        assert_eq!(fmt_elapsed(Duration::from_secs(761)), "12:41");
        assert_eq!(fmt_elapsed(Duration::from_secs(3727)), "1:02:07");
    }
}
