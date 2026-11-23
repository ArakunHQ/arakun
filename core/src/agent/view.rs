//! The transcript of a [`Thread`] in a chat layout: user bubbles on the right,
//! the agent's work folded behind a "Worked for 12s" line once a turn settles,
//! and the reply as markdown below.
//!
//! The list is virtualized per turn. Streaming only remeasures the live turn;
//! a settled transcript does no work until something changes.

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::{Duration, Instant};

use gpui::prelude::FluentBuilder as _;
use gpui::{
    Anchor, AnyElement, App, AppContext as _, ClipboardItem, Context, Div, ElementId, Entity,
    EventEmitter, FocusHandle, Focusable, FollowMode, FontWeight, Hsla, InteractiveElement as _,
    IntoElement, KeyDownEvent, ListAlignment, ListState, MouseButton, MouseDownEvent,
    MouseMoveEvent, MouseUpEvent, ParentElement as _, Pixels, Point, Render, ScrollHandle,
    ScrollWheelEvent, SharedString, StatefulInteractiveElement as _, Styled as _, StyledImage as _,
    StyledText, Subscription, Task, TextRun, Transformation, Window, anchored, deferred, div, font,
    linear_color_stop, linear_gradient, list, point, px, radians,
};

use super::diff::{self, LineKind};
use super::markdown::{self, Block, Doc, Fade, MdStyle};
use super::search::{self, Find, Spot};
use super::selection::{SelText, TextMap};
use super::shell_diff::ShellChange;
use super::thread::{
    AskUserRequest, CheckAnswer, Entry, NoticeMark, NoticePlace, PermissionRequest, PlanEntry,
    PlanStatus, RewindPlan, Thread, ThreadEvent, ThreadStatus, ToolCall, ToolContent, ToolStatus,
};
use super::{AgentKind, launch};
use crate::highlight::LineSpans;
use crate::input::{InputEvent, TextInput};
use crate::mascot::{Mascot, Mood};
use crate::theme;
use crate::ui::{hover_icon, icon};

mod anim;
mod cards;
mod diffs;
mod find_bar;
mod footer;
mod note_box;
#[cfg(any(target_os = "linux", target_os = "macos"))]
mod ssh;
mod tool_rows;

// Free helpers the split-out parts of the transcript share.
use anim::*;
use cards::*;
use diffs::*;
use tool_rows::*;

const COLUMN_W: f32 = 896.;
/// The raccoon perched on the composer: 48 px and up use the detailed parts.
const MASCOT_W: f32 = 64.;
const PREVIEW_LINES: usize = 120;
const OUTPUT_LINES: usize = 40;
/// Steps after which a live phase shows in a window pinned to its newest step.
const LIVE_WINDOW_STEPS: usize = 10;
const LIVE_WINDOW_H: f32 = 280.;
/// How long streamed-in words take to fade in.
const FADE: Duration = Duration::from_millis(360);

fn red() -> Hsla {
    theme::error().into()
}

/// What waits on the user: a permission, a question.
fn amber() -> Hsla {
    theme::needs_you()
}

fn fmt_elapsed(ms: u64) -> String {
    let secs = ((ms + 500) / 1000).max(1);
    if secs < 60 {
        format!("{secs}s")
    } else if secs.is_multiple_of(60) {
        format!("{}m", secs / 60)
    } else {
        format!("{}m {}s", secs / 60, secs % 60)
    }
}

/// What a working turn waits on while its calls run: the subagents it
/// started, or the command it ran. None while it writes or thinks.
fn waiting_on(content: &[Entry], cwd: &Path) -> Option<(String, Option<String>)> {
    let running: Vec<&ToolCall> = content
        .iter()
        .filter_map(|e| match e {
            Entry::Tool(t)
                if t.parent.is_none()
                    && matches!(t.status, ToolStatus::Pending | ToolStatus::InProgress) =>
            {
                Some(t)
            }
            _ => None,
        })
        .collect();
    let agents = running.iter().filter(|t| t.is_subagent()).count();
    if agents > 0 {
        return Some((
            match agents {
                1 => "waiting on an agent".to_string(),
                n => format!("waiting on {n} agents"),
            },
            None,
        ));
    }
    let cmds: Vec<&&ToolCall> = running.iter().filter(|t| t.kind == "execute").collect();
    match cmds.as_slice() {
        [] => None,
        [t] => Some(match tool_rows::wait_limit(t) {
            Some(tool_rows::Wait::Sleep(_)) => ("waiting on a timer".to_string(), None),
            _ => {
                let label = tool_rows::tool_label(t, cwd, true, false);
                let cmd = label.target.lines().next().unwrap_or("").trim();
                (
                    "waiting on".to_string(),
                    Some(crate::redact::text(cmd).into_owned()),
                )
            }
        }),
        many => Some((format!("waiting on {} commands", many.len()), None)),
    }
}

fn fmt_clock(unix: u64) -> String {
    match crate::schedule::local(unix) {
        Some(tm) => format!("{:02}:{:02}", tm.hour, tm.minute),
        None => String::new(),
    }
}

fn rel_path(path: &Path, cwd: &Path) -> String {
    path.strip_prefix(cwd)
        .ok()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(path)
        .display()
        .to_string()
}

/// Drops parsed messages from `from` on whose entry is no longer a message. A
/// shrink resets the view, but a sync can replace entries in place without one.
/// Entries before `from` did not change, so a streamed chunk visits only the tail.
fn keep_docs_of<T>(docs: &mut HashMap<usize, T>, entries: &[Entry], from: usize) {
    for (ix, entry) in entries.iter().enumerate().skip(from) {
        if !matches!(entry, Entry::Agent { .. } | Entry::Thought { .. }) {
            docs.remove(&ix);
        }
    }
}

/// Expanded turns with more than this many phases split into one `gpui::list`
/// row per phase so long threads virtualize instead of laying out thousands of
/// items in a single list item.
const VIRTUALIZE_PHASES: usize = 16;

#[derive(Clone, Copy, PartialEq, Eq)]
enum TurnSlice {
    Whole,
    Head,
    Tail,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ListRow {
    Turn(usize),
    TurnHead(usize),
    TurnPhase { turn: usize, phase: usize },
    TurnTail(usize),
}

impl ListRow {
    fn turn(self) -> usize {
        match self {
            Self::Turn(t)
            | Self::TurnHead(t)
            | Self::TurnPhase { turn: t, .. }
            | Self::TurnTail(t) => t,
        }
    }
}

struct LongTurn {
    range: Range<usize>,
    live: bool,
    body_start: usize,
    content_len: usize,
    phases: Vec<Phase>,
}

fn is_live_turn(turn: usize, turns: &[Range<usize>], t: &Thread) -> bool {
    let start = turns.get(turn).map(|r| r.start);
    (start.is_some() && t.turn_entry() == start)
        || (t.is_busy() && turn + 1 == turns.len() && t.turn_entry().is_none())
}

fn build_rows(
    turns: &[Range<usize>],
    t: &Thread,
    folds: &HashSet<usize>,
) -> (Vec<ListRow>, HashMap<usize, LongTurn>) {
    build_rows_from(turns, t.entries(), |ti| is_live_turn(ti, turns, t), folds)
}

fn build_rows_from(
    turns: &[Range<usize>],
    all_entries: &[Entry],
    is_live: impl Fn(usize) -> bool,
    folds: &HashSet<usize>,
) -> (Vec<ListRow>, HashMap<usize, LongTurn>) {
    let mut rows = Vec::with_capacity(turns.len());
    let mut long_turns = HashMap::new();
    for (ti, range) in turns.iter().enumerate() {
        let live = is_live(ti);
        let show_work = live || folds.contains(&range.start);
        if show_work {
            let entries = &all_entries[range.clone()];
            let parts = turn_parts(entries, live);
            let body = &entries[parts.body_start..];
            let content = &body[..body.len() - parts.trailing_notices];
            if !parts.work.is_empty() {
                let phases = phases_of(content, &parts.work);
                if phases.len() > VIRTUALIZE_PHASES {
                    let count = phases.len();
                    rows.push(ListRow::TurnHead(ti));
                    for phase in 0..count {
                        rows.push(ListRow::TurnPhase { turn: ti, phase });
                    }
                    rows.push(ListRow::TurnTail(ti));
                    long_turns.insert(
                        ti,
                        LongTurn {
                            range: range.clone(),
                            live,
                            body_start: parts.body_start,
                            content_len: content.len(),
                            phases,
                        },
                    );
                    continue;
                }
            }
        }
        rows.push(ListRow::Turn(ti));
    }
    (rows, long_turns)
}

fn turns_of(entries: &[Entry]) -> Vec<Range<usize>> {
    let mut turns = Vec::new();
    let mut start = 0;
    for (i, e) in entries.iter().enumerate() {
        if matches!(e, Entry::User { .. }) && i > start {
            turns.push(start..i);
            start = i;
        }
    }
    if start < entries.len() {
        turns.push(start..entries.len());
    }
    turns
}

/// Answers being put together for a pending AskUser questionnaire.
struct AskState {
    tool_call_id: String,
    /// Picked option indices, per question.
    picked: Vec<Vec<usize>>,
    /// A free-text answer per question.
    inputs: Vec<Entity<TextInput>>,
    _subs: Vec<Subscription>,
}

/// A selection in one markdown surface, in its [`TextMap`] numbering.
struct Selection {
    surface: usize,
    anchor: usize,
    head: usize,
    dragging: bool,
    moved: bool,
}

impl Selection {
    fn range(&self) -> Range<usize> {
        self.anchor.min(self.head)..self.anchor.max(self.head)
    }
}

/// Errors listed under a notice; the agent gets them all.
const NOTICE_PLACES: usize = 5;

/// The selectable surface of the spec-mode plan card (entries use their index).
const PLAN_SURFACE: usize = usize::MAX;

pub enum ThreadViewEvent {
    /// Inline code naming a file that exists was clicked, with the line it
    /// pointed at (`src/a.rs:42`).
    OpenFile(PathBuf, Option<usize>),
    /// Selected text was added to the chat with a note (maybe empty).
    AddNote {
        surface: usize,
        range: Range<usize>,
        quote: String,
        note: String,
    },
    /// An Agent call's subagent was asked for, by its id; `None` asks for the list.
    OpenSubagent(Option<String>),
    /// "Tell us what happened" under an agent's error: the report sent for it,
    /// if any, and the agent.
    TellUs {
        error_event: Option<String>,
        agent: String,
    },
}

/// Ctrl+F over the conversation: prompts, replies, and thoughts.
struct Search {
    input: Entity<TextInput>,
    query: String,
    /// Every match as (entry, index among that entry's matches), in reading order.
    hits: Vec<(usize, usize)>,
    current: Option<usize>,
    /// The transcript changed since `hits` was counted.
    stale: bool,
    /// Where focus was before the search opened.
    restore: Option<FocusHandle>,
    /// What showing the current match opened, closed again when it moves on.
    revealed: Option<Revealed>,
    _sub: Subscription,
}

/// What [`ThreadView::reveal_entry`] opened: only what was closed before.
struct Revealed {
    turn: usize,
    fold: Option<usize>,
    /// A phase group and how it was set by hand before.
    phase: Option<(usize, Option<bool>)>,
    row: Option<usize>,
}

/// The note box opened by "Add to message" on a selection.
struct NoteBox {
    surface: usize,
    range: Range<usize>,
    quote: String,
    input: Entity<TextInput>,
    at: Point<Pixels>,
    _sub: Subscription,
}

impl EventEmitter<ThreadViewEvent> for ThreadView {}

pub struct ThreadView {
    thread: Entity<Thread>,
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    ssh: ssh::State,
    can_tell_us: bool,
    ask: Option<AskState>,
    /// The parsed spec-mode plan on screen, keyed by its text.
    plan_md: RefCell<Option<(String, Rc<Vec<Block>>)>>,
    list: ListState,
    turns: Vec<Range<usize>>,
    rows: Vec<ListRow>,
    long_turns: HashMap<usize, LongTurn>,
    markdown: HashMap<usize, Rc<Doc>>,
    /// Text pieces of every markdown surface rendered, for selection.
    maps: RefCell<HashMap<usize, Rc<TextMap>>>,
    /// Per markdown surface: its text length each time it grew, and when.
    arrivals: HashMap<usize, Vec<(usize, Instant)>>,
    /// A surface is mid-fade, so another frame is wanted.
    fading: bool,
    selection: Option<Selection>,
    /// Where the selection's toolbar shows (the mouse-up point), once a drag ends.
    menu_at: Option<Point<Pixels>>,
    note: Option<NoteBox>,
    /// Annotated ranges per surface, from notes waiting in the composer.
    marks: HashMap<usize, Vec<Range<usize>>>,
    /// The Spacing setting the list's turns were measured at: turn heights
    /// follow it, and the list re-measures only on a width change.
    spacing: f32,
    search: Option<Search>,
    /// Where the current match was laid out, to scroll it into view.
    spot: Spot,
    /// Frames waited for the current match to be laid out.
    settle_tries: u8,
    /// Phase groups opened or closed by hand, by their first entry.
    phase_open: HashMap<usize, bool>,
    /// The scroll of the live phase's window, by its first entry.
    live_scroll: Option<(usize, ScrollHandle)>,
    open: HashSet<usize>,
    folds: HashSet<usize>,
    /// Turns whose changes are shown under their "Worked for" line, by first entry.
    changes_open: HashSet<usize>,
    focus: FocusHandle,
    restore_focus: Option<FocusHandle>,
    following: bool,
    /// The window is in front. Loops hold still behind other windows, so
    /// they draw no frames there.
    active: bool,
    /// Entries and folds still on their way in.
    entrances: Arrivals,
    /// Redraws the live turn's elapsed time while the agent works.
    tick: Option<Task<()>>,
    /// True once `render` has painted the last notification. While false,
    /// streaming updates coalesce instead of re-measuring and notifying a
    /// hidden view (or re-running multiple times within one frame).
    rendered: bool,
    pending_reset: bool,
    pending_from: Option<usize>,
    pending_status: bool,
    /// The raccoon, shown by the shell perched on the composer under this session.
    mascot: Entity<Mascot>,
    _subs: Vec<Subscription>,
}

impl ThreadView {
    /// Whether a failed session offers "Tell us what happened". The shell turns
    /// it off when feedback cannot be sent.
    pub fn set_can_tell_us(&mut self, on: bool) {
        self.can_tell_us = on;
    }

    pub fn new(thread: Entity<Thread>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let turns = turns_of(thread.read(cx).entries());
        let seen = thread.read(cx).entries().len();
        let folds = HashSet::new();
        let (rows, long_turns) = build_rows(&turns, thread.read(cx), &folds);
        let list = ListState::new(rows.len() + 1, ListAlignment::Top, px(800.));
        list.set_follow_mode(FollowMode::Tail);
        let weak = cx.weak_entity();
        list.set_scroll_handler(move |ev, _, cx| {
            let following = ev.is_following_tail;
            let _ = weak.update(cx, |this, cx| {
                if this.following != following {
                    this.following = following;
                    cx.notify();
                }
            });
        });
        let subs = vec![
            cx.subscribe_in(&thread, window, |this, source, ev, window, cx| {
                #[cfg(any(target_os = "linux", target_os = "macos"))]
                this.ssh
                    .sync(source.read(cx), matches!(ev, ThreadEvent::Reset));
                this.thread_event(source, ev, window, cx);
            }),
            cx.observe_window_activation(window, |this, window, cx| {
                this.active = window.is_window_active();
                this.sync_tick(window, cx);
                cx.notify();
            }),
        ];
        let mut this = ThreadView {
            thread,
            #[cfg(any(target_os = "linux", target_os = "macos"))]
            ssh: ssh::State::default(),
            can_tell_us: true,
            ask: None,
            plan_md: RefCell::new(None),
            list,
            turns,
            rows,
            long_turns,
            markdown: HashMap::new(),
            maps: RefCell::new(HashMap::new()),
            arrivals: HashMap::new(),
            fading: false,
            selection: None,
            menu_at: None,
            note: None,
            marks: HashMap::new(),
            spacing: crate::ui::space_scale(),
            search: None,
            spot: Rc::new(Cell::new(None)),
            settle_tries: 0,
            phase_open: HashMap::new(),
            live_scroll: None,
            open: HashSet::new(),
            folds,
            changes_open: HashSet::new(),
            focus: cx.focus_handle(),
            restore_focus: None,
            following: true,
            active: window.is_window_active(),
            entrances: Arrivals::new(seen),
            tick: None,
            rendered: true,
            pending_reset: false,
            pending_from: None,
            pending_status: false,
            mascot: cx.new(|cx| Mascot::new(MASCOT_W, cx).dancer().shy()),
            _subs: subs,
        };
        this.sync_mascot(cx);
        this.sync_tick(window, cx);
        this.sync_ask(window, cx);
        this
    }

    pub fn thread(&self) -> &Entity<Thread> {
        &self.thread
    }

    /// Highlights the quotes of notes that are waiting to be sent.
    pub fn set_marks(&mut self, marks: Vec<(usize, Range<usize>)>, cx: &mut Context<Self>) {
        let mut by_surface: HashMap<usize, Vec<Range<usize>>> = HashMap::new();
        for (surface, range) in marks {
            by_surface.entry(surface).or_default().push(range);
        }
        if by_surface != self.marks {
            self.marks = by_surface;
            self.list.remeasure();
            cx.notify();
        }
    }

    fn clear_selection(&mut self) -> bool {
        self.menu_at = None;
        self.selection.take().is_some()
    }

    /// Scrolls to the newest content and follows it again.
    pub fn jump_to_bottom(&mut self, cx: &mut Context<Self>) {
        self.list.scroll_to_end();
        self.list.set_follow_mode(FollowMode::Tail);
        self.following = true;
        cx.notify();
    }

    fn is_live(&self, turn: usize, t: &Thread) -> bool {
        let start = self.turns.get(turn).map(|r| r.start);
        (start.is_some() && t.turn_entry() == start)
            || (t.is_busy() && turn + 1 == self.turns.len() && t.turn_entry().is_none())
    }

    /// A prompt's text, with the search matches in it painted.
    fn user_text(&self, ix: usize, shown: String) -> AnyElement {
        let shown = crate::redact::same_bytes(&shown).unwrap_or(shown);
        let Some(find) = self.find_for(ix) else {
            return shown.into_any_element();
        };
        let hits = search::find_all(&shown, find.query);
        if hits.is_empty() {
            return shown.into_any_element();
        }
        let text = SharedString::from(shown);
        let styled = StyledText::new(text.clone());
        let layout = styled.layout().clone();
        SelText::new(
            styled.into_any_element(),
            text,
            layout,
            Rc::new(Cell::new(None)),
            Vec::new(),
            None,
            theme::mix(0.08),
            theme::mix(0.5),
        )
        .hits(hits, find.current, find.spot)
        .into_any_element()
    }

    fn thread_event(
        &mut self,
        _: &Entity<Thread>,
        ev: &ThreadEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(s) = &mut self.search
            && matches!(ev, ThreadEvent::EntriesChanged { .. } | ThreadEvent::Reset)
        {
            s.stale = true;
        }
        // Coalesce high-frequency updates while a notification is still pending
        // paint (multiple chunks in one frame, or a background tab not on screen).
        if !self.rendered {
            match ev {
                ThreadEvent::EntriesChanged { from } => {
                    if !self.pending_reset {
                        self.pending_from =
                            Some(self.pending_from.map_or(*from, |prev| prev.min(*from)));
                    }
                    return;
                }
                ThreadEvent::Reset => {
                    self.pending_reset = true;
                    self.pending_from = None;
                    return;
                }
                ThreadEvent::StatusChanged
                | ThreadEvent::Changed
                | ThreadEvent::SubagentsChanged { .. } => {
                    self.pending_status = true;
                    if !self.is_busy_for_tick(cx) {
                        self.tick = None;
                    }
                    return;
                }
                _ => {}
            }
        }
        match ev {
            ThreadEvent::EntriesChanged { from } => self.apply_entries_changed(*from, cx),
            ThreadEvent::Reset => self.apply_reset(cx),
            ThreadEvent::StatusChanged => self.apply_status_changed(window, cx),
            ThreadEvent::NeedsPermission => {
                self.remeasure_footer();
                self.sync_ask(window, cx);
                if self.thread.read(cx).pending_permission().is_some()
                    && !self.focus.is_focused(window)
                {
                    self.restore_focus = window.focused(cx);
                    window.focus(&self.focus, cx);
                }
            }
            ThreadEvent::Changed => {
                self.remeasure_footer();
                self.sync_ask(window, cx)
            }
            ThreadEvent::RewindChanged { entry } => self.remeasure_entry(*entry, cx),
            // A background subagent keeps the timers ticking after the turn.
            ThreadEvent::SubagentsChanged { .. } => self.sync_tick(window, cx),
            _ => {}
        }
        self.sync_mascot(cx);
        self.rendered = false;
        cx.notify();
    }

    fn apply_entries_changed(&mut self, from: usize, cx: &mut Context<Self>) {
        let t = self.thread.read(cx);
        let (len, at_work) = (t.entries().len(), t.is_busy());
        self.entrances.entries_now(len, at_work, Instant::now());
        keep_docs_of(&mut self.markdown, self.thread.read(cx).entries(), from);
        self.sync_turns(from, cx);
    }

    fn apply_reset(&mut self, cx: &mut Context<Self>) {
        self.markdown.clear();
        self.arrivals.clear();
        self.maps.borrow_mut().clear();
        self.selection = None;
        self.menu_at = None;
        self.note = None;
        self.phase_open.clear();
        self.open.clear();
        self.folds.clear();
        self.changes_open.clear();
        let t = self.thread.read(cx);
        self.entrances.reset(t.entries().len());
        self.turns = turns_of(t.entries());
        let (rows, long_turns) = build_rows(&self.turns, t, &self.folds);
        self.rows = rows;
        self.long_turns = long_turns;
        self.list.reset(self.rows.len() + 1);
        self.list.set_follow_mode(FollowMode::Tail);
    }

    fn apply_status_changed(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.sync_rows(None, cx);
        self.remeasure_footer();
        if let Some(last) = self.turns.len().checked_sub(1) {
            self.remeasure_turn_rows(last);
        }
        self.sync_tick(window, cx);
        self.sync_ask(window, cx);
    }

    fn flush_pending(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let reset = std::mem::take(&mut self.pending_reset);
        let from = self.pending_from.take();
        let status = std::mem::take(&mut self.pending_status);
        if reset {
            self.apply_reset(cx);
        } else if let Some(from) = from {
            self.apply_entries_changed(from, cx);
        }
        if status {
            self.apply_status_changed(window, cx);
        }
        if reset || from.is_some() || status {
            self.sync_mascot(cx);
        }
        if self.tick.is_none() && self.is_busy_for_tick(cx) {
            self.sync_tick(window, cx);
        }
    }

    fn is_busy_for_tick(&self, cx: &App) -> bool {
        let t = self.thread.read(cx);
        matches!(
            t.status(),
            ThreadStatus::Working | ThreadStatus::NeedsInput | ThreadStatus::Connecting
        ) || t.running_subagents() > 0
    }

    pub fn mascot(&self) -> Entity<Mascot> {
        self.mascot.clone()
    }

    fn sync_mascot(&self, cx: &mut Context<Self>) {
        let t = self.thread.read(cx);
        let mood = match t.status() {
            // Starting up is not a turn: no "done" hop once it connects.
            ThreadStatus::Connecting | ThreadStatus::Idle => Mood::Idle,
            ThreadStatus::NeedsInput => Mood::Waiting,
            ThreadStatus::Error(_) => Mood::Error,
            ThreadStatus::Working => match t.entries().last() {
                Some(Entry::Tool(_) | Entry::Agent { .. } | Entry::Plan { .. }) => Mood::Working,
                _ => Mood::Thinking,
            },
        };
        // It sits on top of the card above the composer, never over it.
        let perch = self.queue_height(cx);
        self.mascot.update(cx, |m, _| {
            m.set_mood(mood);
            m.set_perch(perch);
        });
    }

    fn sync_turns(&mut self, from: usize, cx: &mut Context<Self>) {
        self.turns = turns_of(self.thread.read(cx).entries());
        self.sync_rows(Some(from), cx);
    }

    pub(super) fn sync_rows(&mut self, remeasure_from: Option<usize>, cx: &App) {
        let (new_rows, new_long) = build_rows(&self.turns, self.thread.read(cx), &self.folds);
        let old_len = self.rows.len();
        let new_len = new_rows.len();
        let prefix = self
            .rows
            .iter()
            .zip(new_rows.iter())
            .take_while(|(a, b)| a == b)
            .count();
        let max_suffix = old_len.min(new_len) - prefix;
        let suffix = self.rows[prefix..]
            .iter()
            .rev()
            .zip(new_rows[prefix..].iter().rev())
            .take(max_suffix)
            .take_while(|(a, b)| a == b)
            .count();
        let spliced = prefix + suffix < old_len || prefix + suffix < new_len;
        let splice_end = new_len - suffix;
        if spliced {
            self.list
                .splice(prefix..old_len - suffix, splice_end - prefix);
        }
        self.rows = new_rows;
        self.long_turns = new_long;
        if let Some(entry) = remeasure_from
            && let Some(row_ix) = self.row_for_entry(entry, cx)
        {
            self.list.remeasure_items(row_ix..self.rows.len());
        }
    }

    pub(super) fn row_start_entry(&self, row_ix: usize) -> Option<usize> {
        let row = *self.rows.get(row_ix)?;
        match row {
            ListRow::Turn(t) | ListRow::TurnHead(t) => self.turns.get(t).map(|r| r.start),
            ListRow::TurnPhase { turn, phase } => {
                let lt = self.long_turns.get(&turn)?;
                let p = lt.phases.get(phase)?;
                let rel = p.head.or_else(|| p.steps.first().copied()).unwrap_or(0);
                Some(lt.range.start + lt.body_start + rel)
            }
            ListRow::TurnTail(t) => self.long_turns.get(&t).map_or_else(
                || self.turns.get(t).map(|r| r.start),
                |lt| {
                    let after_phases = lt
                        .phases
                        .last()
                        .and_then(|p| p.steps.last().copied().or(p.head))
                        .map_or(0, |i| i + 1);
                    Some(lt.range.start + lt.body_start + after_phases)
                },
            ),
        }
    }

    pub(super) fn row_for_entry(&self, entry: usize, cx: &App) -> Option<usize> {
        let turn = self.turns.iter().position(|t| t.contains(&entry))?;
        if let Some(lt) = self.long_turns.get(&turn) {
            let base = lt.range.start + lt.body_start;
            if entry < base {
                return self.rows.iter().position(|r| *r == ListRow::TurnHead(turn));
            }
            let mut rel = entry - base;
            if rel < lt.content_len {
                // Nested subagent tool calls render inside their parent Agent phase.
                let entries = self.thread.read(cx).entries();
                if let Some(content) = entries.get(base..base + lt.content_len)
                    && let Some(Entry::Tool(tc)) = content.get(rel)
                    && let Some(parent_id) = tc.parent.as_deref()
                    && let Some(parent_rel) = content
                        .iter()
                        .position(|e| matches!(e, Entry::Tool(p) if p.id == parent_id))
                {
                    rel = parent_rel;
                }
                if let Some(pi) = lt
                    .phases
                    .iter()
                    .position(|p| p.head == Some(rel) || p.steps.contains(&rel))
                {
                    return self
                        .rows
                        .iter()
                        .position(|r| *r == ListRow::TurnPhase { turn, phase: pi });
                }
            }
            return self.rows.iter().position(|r| *r == ListRow::TurnTail(turn));
        }
        self.rows.iter().position(|r| *r == ListRow::Turn(turn))
    }

    pub(super) fn remeasure_turn_rows(&mut self, turn: usize) {
        let Some(first) = self.rows.iter().position(|r| r.turn() == turn) else {
            return;
        };
        let last = self
            .rows
            .iter()
            .rposition(|r| r.turn() == turn)
            .unwrap_or(first);
        self.list.remeasure_items(first..last + 1);
    }

    pub(super) fn remeasure_entry(&mut self, entry: usize, cx: &App) {
        if let Some(row_ix) = self.row_for_entry(entry, cx) {
            self.list.remeasure_items(row_ix..row_ix + 1);
        }
    }

    fn sync_tick(&mut self, window: &Window, cx: &mut Context<Self>) {
        // A background subagent's timer runs on after the turn ends.
        let busy = self.is_busy_for_tick(cx);
        if !(busy && window.is_window_active()) {
            self.tick = None;
            return;
        }
        if self.tick.is_some() {
            return;
        }
        self.tick = Some(cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor()
                    .timer(Duration::from_millis(250))
                    .await;
                let keep = this
                    .update(cx, |this, cx| {
                        // Hidden views never clear `rendered`; stop ticking until rendered again.
                        if !this.rendered {
                            this.tick = None;
                            return false;
                        }
                        this.rendered = false;
                        cx.notify();
                        true
                    })
                    .unwrap_or(false);
                if !keep {
                    break;
                }
            }
        }));
    }

    fn markdown_for(&mut self, ix: usize, text: &str) -> Rc<Doc> {
        let doc = self
            .markdown
            .entry(ix)
            .or_insert_with(|| Rc::new(Doc::parse(text)));
        if !doc.is_current(text) {
            Rc::make_mut(doc).update(text);
        }
        doc.clone()
    }

    /// A selectable markdown surface for entry `ix`.
    fn surface(&mut self, ix: usize, text: &str, st: MdStyle, cx: &Context<Self>) -> AnyElement {
        let doc = self.markdown_for(ix, text);
        let fade = self.fade_for(ix, text.len());
        self.surface_faded(ix, &doc.blocks, st, fade.as_ref(), cx)
    }

    /// The fade for text that grew to `len`: each recent growth a span whose
    /// opacity rises with its age. Text seen for the first time does not fade.
    fn fade_for(&mut self, ix: usize, len: usize) -> Option<Fade> {
        let now = Instant::now();
        let settled = now.checked_sub(FADE).unwrap_or(now);
        let rec = self
            .arrivals
            .entry(ix)
            .or_insert_with(|| vec![(len, settled)]);
        let last = rec.last().map_or(0, |r| r.0);
        if len > last {
            rec.push((len, now));
        } else if len < last {
            *rec = vec![(len, settled)];
        }
        while rec.len() > 1 && rec[1].1.elapsed() >= FADE {
            rec.remove(0);
        }
        let spans: Vec<(usize, usize, f32)> = rec
            .windows(2)
            .filter_map(|w| {
                let age = w[1].1.elapsed().as_secs_f32() / FADE.as_secs_f32();
                (age < 1.).then(|| {
                    let eased = 1. - (1. - age).powi(2);
                    (len - w[0].0, len - w[1].0, 0.12 + 0.88 * eased)
                })
            })
            .collect();
        if spans.is_empty() {
            return None;
        }
        self.fading = true;
        Some(Fade(spans))
    }

    fn surface_of(
        &self,
        ix: usize,
        blocks: &[Block],
        st: MdStyle,
        cx: &Context<Self>,
    ) -> AnyElement {
        self.surface_faded(ix, blocks, st, None, cx)
    }

    fn surface_faded(
        &self,
        ix: usize,
        blocks: &[Block],
        st: MdStyle,
        fade: Option<&Fade>,
        cx: &Context<Self>,
    ) -> AnyElement {
        let map = self.maps.borrow_mut().entry(ix).or_default().clone();
        let selection = self
            .selection
            .as_ref()
            .filter(|s| s.surface == ix)
            .map(Selection::range);
        div()
            .id(("surface", ix))
            .min_w_0()
            .cursor_text()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, ev: &MouseDownEvent, window, cx| {
                    this.select_start(ix, ev, window, cx)
                }),
            )
            .child(markdown::render(
                ("md", ix),
                blocks,
                st,
                &map,
                selection,
                self.marks.get(&ix).map_or(&[][..], Vec::as_slice),
                Some(self.thread.read(cx).cwd()),
                // A server's files are not here: only their copies are shown.
                Some(self.thread.read(cx))
                    .filter(|t| t.is_remote())
                    .map(|t| t.fetched_images()),
                fade,
                self.find_for(ix).filter(|_| ix != PLAN_SURFACE),
            ))
            .into_any_element()
    }

    fn select_start(
        &mut self,
        surface: usize,
        ev: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(map) = self.maps.borrow().get(&surface).cloned() else {
            return;
        };
        let Some(at) = map.hit(ev.position) else {
            return;
        };
        let range = match ev.click_count {
            2 => map.word_at(at),
            n if n >= 3 => map.piece_at(at),
            _ => at..at,
        };
        self.menu_at = None;
        self.selection = Some(Selection {
            surface,
            anchor: range.start,
            head: range.end,
            dragging: ev.click_count == 1,
            moved: ev.click_count > 1,
        });
        window.focus(&self.focus, cx);
        cx.stop_propagation();
        cx.notify();
    }

    fn select_move(&mut self, ev: &MouseMoveEvent, cx: &mut Context<Self>) {
        let Some(sel) = self.selection.as_mut().filter(|s| s.dragging) else {
            return;
        };
        if ev.pressed_button != Some(MouseButton::Left) {
            sel.dragging = false;
            return;
        }
        let map = self.maps.borrow().get(&sel.surface).cloned();
        if let Some(at) = map.and_then(|m| m.hit(ev.position))
            && at != sel.head
        {
            sel.head = at;
            sel.moved = true;
            cx.notify();
        }
    }

    fn select_end(&mut self, ev: &MouseUpEvent, cx: &mut Context<Self>) {
        let Some(sel) = self.selection.as_mut() else {
            return;
        };
        let was_click = sel.dragging && !sel.moved;
        sel.dragging = false;
        if !was_click {
            if !sel.range().is_empty() && self.menu_at.is_none() && self.note.is_none() {
                self.menu_at = Some(ev.position);
                cx.notify();
            }
            return;
        }
        let file = self
            .maps
            .borrow()
            .get(&sel.surface)
            .and_then(|m| m.file_at(sel.head));
        if let Some(file) = file
            && let Some(path) = self.resolve_file(&file, cx)
        {
            cx.emit(ThreadViewEvent::OpenFile(path, markdown::file_line(&file)));
        }
    }

    /// What a notice points at (a failing check's errors), each opening its file at the line.
    fn notice_places(
        &self,
        ix: usize,
        places: &[NoticePlace],
        cx: &Context<Self>,
    ) -> Option<AnyElement> {
        if places.is_empty() {
            return None;
        }
        let mut col = div()
            .flex()
            .flex_col()
            .pl(crate::ui::sp(14.))
            .pt(crate::ui::sp(2.))
            .text_size(px(crate::ui::TEXT_SM));
        for (k, place) in places.iter().take(NOTICE_PLACES).enumerate() {
            let target = self.resolve_file(&place.path, cx);
            let row = div()
                .id(("notice-place", ix * 32 + k))
                .flex()
                .gap(crate::ui::sp(8.))
                .min_w_0()
                .px(crate::ui::sp(6.))
                .py(crate::ui::sp(1.))
                .rounded(crate::ui::rad(4.))
                .child(
                    div()
                        .flex_none()
                        .font_family(theme::FONT_MONO)
                        .text_color(if target.is_some() {
                            markdown::link_color()
                        } else {
                            theme::mix(0.5)
                        })
                        .child(format!("{}:{}", place.path, place.line)),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .text_color(theme::text_subtle())
                        .child(place.message.clone()),
                );
            col = col.child(match target {
                Some(path) => {
                    let line = place.line as usize;
                    row.cursor_pointer()
                        .hover(|s| s.bg(theme::fill_selected()))
                        .on_click(cx.listener(move |_, _, _, cx| {
                            cx.emit(ThreadViewEvent::OpenFile(path.clone(), Some(line)))
                        }))
                }
                None => row,
            });
        }
        if places.len() > NOTICE_PLACES {
            let more = places.len() - NOTICE_PLACES;
            col = col.child(
                div()
                    .px(crate::ui::sp(6.))
                    .text_color(theme::text_faint())
                    .child(format!("and {more} more")),
            );
        }
        Some(col.into_any_element())
    }

    /// A file reference from the transcript as an existing file, relative to the session.
    fn resolve_file(&self, name: &str, cx: &App) -> Option<PathBuf> {
        let name = markdown::file_path(name);
        let path = if let Some(rest) = name.strip_prefix("~/") {
            std::env::home_dir()?.join(rest)
        } else {
            self.thread.read(cx).cwd().join(name)
        };
        path.is_file().then_some(path)
    }

    fn selected_text(&self) -> Option<String> {
        let sel = self.selection.as_ref()?;
        let range = sel.range();
        if range.is_empty() {
            return None;
        }
        let text = self.maps.borrow().get(&sel.surface)?.text_in(range);
        (!text.is_empty()).then_some(text)
    }

    fn toggle_open(&mut self, entry: usize, cx: &mut Context<Self>) {
        let open = !self.open.remove(&entry);
        if open {
            self.open.insert(entry);
        }
        self.opened(Fold::Row, entry, open);
        self.remeasure_entry(entry, cx);
        cx.notify();
    }

    fn toggle_fold(&mut self, turn_start: usize, cx: &mut Context<Self>) {
        let open = !self.folds.remove(&turn_start);
        if open {
            self.folds.insert(turn_start);
        }
        self.opened(Fold::Work, turn_start, open);
        self.sync_rows(Some(turn_start), cx);
        cx.notify();
    }

    fn remeasure_last(&mut self) {
        if let Some(last) = self.turns.len().checked_sub(1) {
            self.remeasure_turn_rows(last);
        }
        self.remeasure_footer();
    }

    /// The footer shows what the agent waits on when no turn does.
    fn remeasure_footer(&mut self) {
        let n = self.rows.len();
        self.list.remeasure_items(n..n + 1);
    }

    /// Keeps the answer state in step with the questionnaire the thread is waiting on.
    fn sync_ask(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let pending = self
            .thread
            .read(cx)
            .pending_question()
            .map(|q| (q.tool_call_id.clone(), q.questions.len()));
        match pending {
            None => {
                if self.ask.take().is_some() {
                    self.remeasure_last();
                }
            }
            Some((id, n)) if self.ask.as_ref().is_none_or(|a| a.tool_call_id != id) => {
                let inputs: Vec<Entity<TextInput>> = (0..n)
                    .map(|_| cx.new(|cx| TextInput::new("Or type an answer…", cx)))
                    .collect();
                let subs = inputs
                    .iter()
                    .map(|input| {
                        cx.subscribe_in(input, window, |this, _, ev: &InputEvent, _, cx| match ev {
                            InputEvent::Submit => this.submit_answers(cx),
                            InputEvent::Changed => cx.notify(),
                            _ => {}
                        })
                    })
                    .collect();
                self.ask = Some(AskState {
                    tool_call_id: id,
                    picked: vec![Vec::new(); n],
                    inputs,
                    _subs: subs,
                });
                self.remeasure_last();
            }
            Some(_) => {}
        }
    }

    /// One answer per question, or None while any is still blank.
    fn answers(&self, req: &AskUserRequest, cx: &App) -> Option<Vec<String>> {
        let ask = self.ask.as_ref()?;
        req.questions
            .iter()
            .enumerate()
            .map(|(i, q)| {
                let mut parts: Vec<String> = ask.picked[i]
                    .iter()
                    .filter_map(|&o| q.options.get(o).cloned())
                    .collect();
                let typed = ask.inputs[i].read(cx).text().trim().to_string();
                if !typed.is_empty() {
                    parts.push(typed);
                }
                (!parts.is_empty()).then(|| parts.join(", "))
            })
            .collect()
    }

    fn submit_answers(&mut self, cx: &mut Context<Self>) {
        let Some(req) = self.thread.read(cx).pending_question().cloned() else {
            return;
        };
        let Some(answers) = self.answers(&req, cx) else {
            return;
        };
        // A server's question keeps its answers until the server takes them.
        if !self.thread.read(cx).is_remote() {
            self.ask = None;
        }
        self.thread
            .update(cx, |t, cx| t.answer_question(answers, cx));
        self.remeasure_last();
        cx.notify();
    }

    fn toggle_pick(&mut self, question: usize, option: usize, multi: bool, cx: &mut Context<Self>) {
        let Some(picked) = self.ask.as_mut().and_then(|a| a.picked.get_mut(question)) else {
            return;
        };
        if let Some(pos) = picked.iter().position(|&o| o == option) {
            picked.remove(pos);
        } else if multi {
            picked.push(option);
        } else {
            *picked = vec![option];
        }
        cx.notify();
    }

    fn plan_blocks(&self, text: &str) -> Rc<Vec<Block>> {
        let mut cache = self.plan_md.borrow_mut();
        if let Some((cached, blocks)) = cache.as_ref()
            && cached == text
        {
            return blocks.clone();
        }
        let blocks = Rc::new(markdown::parse(text));
        *cache = Some((text.to_string(), blocks.clone()));
        blocks
    }

    fn after_permission(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.thread.read(cx).pending_permission().is_none()
            && let Some(handle) = self.restore_focus.take()
        {
            window.focus(&handle, cx);
        }
    }

    fn on_key(&mut self, ev: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let ks = &ev.keystroke;
        if ks.key == "c"
            && (ks.modifiers.control || ks.modifiers.platform)
            && !ks.modifiers.alt
            && let Some(text) = self.selected_text()
        {
            cx.write_to_clipboard(ClipboardItem::new_string(text));
            cx.stop_propagation();
            return;
        }
        let Some(options) = self
            .thread
            .read(cx)
            .pending_permission()
            .map(|p| p.options.clone())
        else {
            if ks.key == "escape" && self.clear_selection() {
                cx.stop_propagation();
                cx.notify();
            } else if ks.key == "escape" && self.search.is_some() {
                cx.stop_propagation();
                self.close_search(window, cx);
            }
            return;
        };
        let key = ev.keystroke.key.as_str();
        match key {
            "enter" => self.thread.update(cx, |t, cx| t.allow_permission(cx)),
            "escape" => self.thread.update(cx, |t, cx| t.reject_permission(cx)),
            k if k.len() == 1 && k.as_bytes()[0].is_ascii_digit() => {
                let n = (k.as_bytes()[0] - b'0') as usize;
                let Some(opt) = n.checked_sub(1).and_then(|i| options.get(i)) else {
                    return;
                };
                let id = opt.id.clone();
                self.thread
                    .update(cx, |t, cx| t.respond_permission(&id, cx));
            }
            _ => return,
        }
        cx.stop_propagation();
        self.after_permission(window, cx);
    }

    // ---- rendering -------------------------------------------------------

    fn render_item(
        &mut self,
        ix: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let cx: &Context<Self> = cx;
        let body = if let Some(&row) = self.rows.get(ix) {
            match row {
                ListRow::Turn(ti) => self.render_turn(ti, window, cx),
                ListRow::TurnHead(ti) => self.render_turn_head(ti, window, cx),
                ListRow::TurnPhase { turn, phase } => {
                    self.render_turn_phase(turn, phase, window, cx)
                }
                ListRow::TurnTail(ti) => self.render_turn_tail(ti, window, cx),
            }
        } else {
            let footer = self.render_footer(cx);
            match self.unshown_asks(window, cx) {
                Some(asks) => div()
                    .flex()
                    .flex_col()
                    .child(asks)
                    .child(footer)
                    .into_any_element(),
                None => footer,
            }
        };
        // Fades and entrances: on the motion clock, not at the display's rate,
        // since text streams in for as long as the agent writes.
        if std::mem::take(&mut self.fading) {
            crate::ui::motion::redraw_on_tick(window);
        }
        div()
            .id(("turn", ix))
            .w_full()
            .flex()
            .justify_center()
            .child(
                div()
                    .w_full()
                    .max_w(px(COLUMN_W))
                    .px(crate::ui::sp(6.))
                    .when(ix == 0, |d| d.pt(crate::ui::sp(20.)))
                    .child(body),
            )
            .into_any_element()
    }

    /// What the agent waits on when no turn shows it. A server sends its
    /// questions apart from the transcript, which may not load at all.
    fn unshown_asks(&self, window: &Window, cx: &Context<Self>) -> Option<AnyElement> {
        let t = self.thread.read(cx);
        if (0..self.turns.len()).any(|ix| self.is_live(ix, t)) {
            return None;
        }
        let permission = t.pending_permission().filter(|p| {
            !t.entries()
                .iter()
                .any(|e| matches!(e, Entry::Tool(tc) if tc.id == p.tool_call_id))
        });
        let question = t.pending_question();
        if permission.is_none() && question.is_none() {
            return None;
        }
        let cwd = t.cwd().to_path_buf();
        Some(
            div()
                .flex()
                .flex_col()
                .gap(crate::ui::sp(8.))
                .px(crate::ui::sp(16.))
                .children(permission.map(|p| self.permission_card(p, None, &cwd, window, cx)))
                .children(question.map(|q| self.question_card(q, cx)))
                .into_any_element(),
        )
    }

    fn render_turn_head(&mut self, ix: usize, window: &Window, cx: &Context<Self>) -> AnyElement {
        self.render_turn_slice(ix, TurnSlice::Head, window, cx)
    }

    fn render_turn_tail(&mut self, ix: usize, window: &Window, cx: &Context<Self>) -> AnyElement {
        self.render_turn_slice(ix, TurnSlice::Tail, window, cx)
    }

    fn render_turn_phase(
        &mut self,
        turn: usize,
        phase_ix: usize,
        window: &Window,
        cx: &Context<Self>,
    ) -> AnyElement {
        let Some((range, live, body_start, content_len, phase, is_last)) =
            self.long_turns.get(&turn).and_then(|lt| {
                lt.phases.get(phase_ix).cloned().map(|p| {
                    (
                        lt.range.clone(),
                        lt.live,
                        lt.body_start,
                        lt.content_len,
                        p,
                        phase_ix + 1 == lt.phases.len(),
                    )
                })
            })
        else {
            return div().into_any_element();
        };
        let thread = self.thread.clone();
        let t = thread.read(cx);
        let cwd = t.cwd().to_path_buf();
        let permission = t.pending_permission().cloned();
        let base = range.start + body_start;
        let Some(content) = t.entries().get(base..base + content_len) else {
            return div().into_any_element();
        };
        div()
            .flex()
            .flex_col()
            .min_w_0()
            .px(crate::ui::sp(16.))
            .child(self.render_phase(
                &phase,
                content,
                base,
                &cwd,
                live,
                live && is_last,
                permission.as_ref(),
                window,
                cx,
            ))
            .into_any_element()
    }

    fn render_turn(&mut self, ix: usize, window: &Window, cx: &Context<Self>) -> AnyElement {
        self.render_turn_slice(ix, TurnSlice::Whole, window, cx)
    }

    fn render_turn_slice(
        &mut self,
        ix: usize,
        slice: TurnSlice,
        window: &Window,
        cx: &Context<Self>,
    ) -> AnyElement {
        let range = self.turns[ix].clone();
        let thread = self.thread.clone();
        let t = thread.read(cx);
        let kind = t.kind();
        let cwd = t.cwd().to_path_buf();
        let entries = &t.entries()[range.clone()];
        let live = self.is_live(ix, t);
        let permission = t.pending_permission().cloned();
        let question = t.pending_question().cloned();
        let status = t.status();
        let cancelling = t.is_cancelling();
        let elapsed_live = t.turn_elapsed();

        let mut col = div()
            .group(SharedString::from(format!("turn-{ix}")))
            .flex()
            .flex_col()
            .when(slice != TurnSlice::Head, |d| d.pb(crate::ui::sp(8.)));

        let TurnParts {
            body_start,
            trailing_notices,
            reply,
            plan,
            work,
        } = turn_parts(entries, live);
        let mut elapsed_ms = None;
        if let Some(Entry::User {
            text,
            at,
            elapsed_ms: e,
            ..
        }) = entries.first()
        {
            elapsed_ms = *e;
            if slice != TurnSlice::Tail {
                let rewind = t.can_rewind(range.start) && t.rewind_plan().is_none();
                let bubble = self.user_bubble(range.start, text, *at, rewind, cx);
                col = col.child(self.arrive(range.start, bubble, cx));
                if let Some(plan) = t.rewind_plan().filter(|p| p.entry == range.start) {
                    col = col.child(self.rewind_card(plan, &cwd, cx));
                }
            }
        }

        let body = &entries[body_start..];
        let content = &body[..body.len() - trailing_notices];
        let base = range.start + body_start;

        if slice != TurnSlice::Tail {
            if live {
                let waiting_on = waiting_on(content, &cwd);
                col = col.child(self.live_header(
                    kind,
                    &status,
                    cancelling,
                    question.is_some(),
                    elapsed_live,
                    waiting_on,
                    window,
                ));
            }

            let show_work = live || self.folds.contains(&range.start);
            if !live && !work.is_empty() {
                let label = match elapsed_ms {
                    Some(ms) => format!("Worked for {}", fmt_elapsed(ms)),
                    None => "Worked".to_string(),
                };
                let turn_start = range.start;
                let group: SharedString = format!("fold-{turn_start}").into();
                let shell = match entries.first() {
                    Some(Entry::User { shell_changes, .. }) => shell_changes.as_slice(),
                    _ => &[],
                };
                let edits = turn_edits(shell, content);
                let changes_shown =
                    !edits.files.is_empty() && self.changes_open.contains(&turn_start);
                let changes_chip = (!edits.files.is_empty()).then(|| {
                    let n = edits.files.len();
                    div()
                        .id(("changes", turn_start))
                        .flex()
                        .flex_none()
                        .items_center()
                        .gap(crate::ui::sp(6.))
                        .h(px(24.))
                        .px(crate::ui::sp(6.))
                        .rounded(crate::ui::rad(8.))
                        .cursor_pointer()
                        .when(changes_shown, |d| d.bg(theme::fill_selected()))
                        .hover(|s| s.bg(theme::fill_selected()))
                        .child(icon("file-diff", 13.).text_color(theme::text_subtle()))
                        .child(
                            div()
                                .text_size(px(crate::ui::TEXT_SM))
                                .text_color(theme::text_subtle())
                                .child(if n == 1 {
                                    "1 file".to_string()
                                } else {
                                    format!("{n} files")
                                }),
                        )
                        .child(diff_counts(edits.added, edits.removed))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            cx.stop_propagation();
                            this.toggle_changes(turn_start, cx);
                        }))
                });
                col = col.child(
                    div()
                        .id(("fold", turn_start))
                        .group(group.clone())
                        .flex()
                        .items_center()
                        .gap(crate::ui::sp(6.))
                        .px(crate::ui::sp(16.))
                        .py(crate::ui::sp(4.))
                        .cursor_pointer()
                        .child(
                            div()
                                .relative()
                                .size(px(14.))
                                .flex_none()
                                .when(!show_work, |d| {
                                    d.child(
                                        div()
                                            .absolute()
                                            .size_full()
                                            .group_hover(group.clone(), |s| s.opacity(0.))
                                            .child(kind.icon(14.)),
                                    )
                                })
                                .child(
                                    div()
                                        .absolute()
                                        .size_full()
                                        .when(!show_work, |d| {
                                            d.opacity(0.)
                                                .group_hover(group.clone(), |s| s.opacity(1.))
                                        })
                                        .child(chevron(
                                            ("fold-chevron", turn_start),
                                            show_work,
                                            icon("arrow-right01", 14.)
                                                .text_color(theme::text_subtle()),
                                        )),
                                ),
                        )
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .truncate()
                                .text_size(px(crate::ui::TEXT_LG))
                                .text_color(theme::text_subtle())
                                .group_hover(group, |s| s.text_color(theme::text_muted()))
                                .child(label),
                        )
                        .when_some(changes_chip, |d, chip| d.child(chip))
                        .on_click(
                            cx.listener(move |this, _, _, cx| this.toggle_fold(turn_start, cx)),
                        ),
                );
                if changes_shown {
                    let changes = self.turn_changes(&edits, turn_start, base, &cwd, cx);
                    col = col.child(self.unfold(Fold::Changes, turn_start, changes, cx));
                }
            }

            if live && content.is_empty() && status == ThreadStatus::Working && !cancelling {
                col = col.child(
                    div()
                        .px(crate::ui::sp(16.))
                        .py(crate::ui::sp(4.))
                        .text_size(px(crate::ui::TEXT_LG))
                        .child(shimmer(
                            self.active,
                            "Thinking…",
                            theme::text_faint(),
                            theme::text_muted(),
                        )),
                );
            }

            if live && let Some(retry) = t.retry_status() {
                col = col.child(
                    div()
                        .flex()
                        .items_center()
                        .gap(crate::ui::sp(8.))
                        .mx(px(16.))
                        .my(px(4.))
                        .px(crate::ui::sp(10.))
                        .py(crate::ui::sp(6.))
                        .rounded(crate::ui::rad(12.))
                        .bg(theme::fill_raised())
                        .text_size(px(crate::ui::TEXT_MD))
                        .text_color(theme::text_muted())
                        .child(spinner(self.active, 12., theme::text_subtle()))
                        .child(div().min_w_0().truncate().child(retry.to_string())),
                );
            }

            if slice == TurnSlice::Head {
                return col.into_any_element();
            }

            if show_work && !work.is_empty() {
                let phases = phases_of(content, &work);
                let last = phases.len() - 1;
                let mut trail = div()
                    .flex()
                    .flex_col()
                    .min_w_0()
                    .px(crate::ui::sp(16.))
                    .py(crate::ui::sp(2.));
                for (pi, phase) in phases.iter().enumerate() {
                    trail = trail.child(self.render_phase(
                        phase,
                        content,
                        base,
                        &cwd,
                        live,
                        live && pi == last,
                        permission.as_ref(),
                        window,
                        cx,
                    ));
                }
                let trail = self.unfold(Fold::Work, range.start, trail.into_any_element(), cx);
                col = col.child(trail);
            }
        }

        if let Some(p) = plan
            && let Entry::Plan { entries: plan } = &content[p]
        {
            col = col.child(
                div()
                    .px(crate::ui::sp(16.))
                    .py(crate::ui::sp(4.))
                    .child(plan_card(plan)),
            );
        }

        for (k, e) in content[reply.clone()].iter().enumerate() {
            let entry_ix = base + reply.start + k;
            if let Entry::Agent { text } = e {
                let reply = div()
                    .min_w_0()
                    .px(crate::ui::sp(16.))
                    .pt(crate::ui::sp(12.))
                    .pb(crate::ui::sp(4.))
                    .child(self.surface(entry_ix, text, MdStyle::reply(), cx))
                    .into_any_element();
                col = col.child(self.arrive(entry_ix, reply, cx));
            }
        }

        if !live && let Some(Entry::User { at, tokens, .. }) = entries.first() {
            let reply = content[reply.clone()]
                .iter()
                .filter_map(|e| match e {
                    Entry::Agent { text } => Some(text.trim()),
                    _ => None,
                })
                .filter(|t| !t.is_empty())
                .collect::<Vec<_>>()
                .join("\n\n");
            if !reply.is_empty() {
                let elapsed = work.is_empty().then_some(elapsed_ms).flatten();
                let latest = ix + 1 == self.turns.len();
                col = col.child(turn_footer(ix, reply, *at, *tokens, elapsed, latest));
            }
        }

        // A permission for a tool this turn never announced still needs a card.
        if let Some(p) = &permission
            && live
            && !entries
                .iter()
                .any(|e| matches!(e, Entry::Tool(tc) if tc.id == p.tool_call_id))
        {
            col = col.child(
                div()
                    .px(crate::ui::sp(16.))
                    .child(self.permission_card(p, None, &cwd, window, cx)),
            );
        }

        if live && let Some(q) = &question {
            col = col.child(
                div()
                    .px(crate::ui::sp(16.))
                    .child(self.question_card(q, cx)),
            );
        }

        let notices_from = body.len() - trailing_notices;
        for (k, e) in body[notices_from..].iter().enumerate() {
            match e {
                Entry::Notice { text, error, .. }
                    if !*error && legacy_compaction(text).is_some() =>
                {
                    col = col.child(compaction_divider(
                        legacy_compaction(text).unwrap_or(0),
                        None,
                        None,
                    ));
                }
                Entry::Notice {
                    text,
                    error,
                    mark,
                    places,
                } => {
                    let ix = base + notices_from + k;
                    col = col.child(notice_row(text, *error, *mark));
                    if let Some(places) = self.notice_places(ix, places, cx) {
                        col = col.child(
                            div()
                                .px(crate::ui::sp(16.))
                                .pb(crate::ui::sp(4.))
                                .child(places),
                        );
                    }
                }
                Entry::Compacted {
                    removed,
                    before,
                    after,
                } => {
                    col = col.child(compaction_divider(*removed, *before, *after));
                }
                _ => {}
            }
        }
        col.into_any_element()
    }

    fn user_bubble(
        &self,
        ix: usize,
        full: &str,
        at: u64,
        rewind: bool,
        cx: &Context<Self>,
    ) -> AnyElement {
        let group: SharedString = format!("user-{ix}").into();
        let (typed, pastes) = super::attach::split_pastes(full);
        let (typed, images) = super::attach::split_images(&typed, self.thread.read(cx).cwd());
        let single_image = images.len() == 1;
        let text = typed.as_str();
        let expanded = self.open.contains(&ix);
        let long = text.lines().count() > 8 || text.len() > 700;
        let shown: String = if long && !expanded {
            let mut cut: String = text.lines().take(6).collect::<Vec<_>>().join("\n");
            if let Some((i, _)) = cut.char_indices().nth(560) {
                cut.truncate(i);
            }
            cut.push('…');
            cut
        } else {
            text.to_string()
        };
        let single = !text.contains('\n')
            && text.chars().count() < 70
            && pastes.is_empty()
            && images.is_empty();
        let copy = full.to_string();
        div()
            .id(("user", ix))
            .group(group.clone())
            .flex()
            .flex_col()
            .items_end()
            .pl(crate::ui::sp(56.))
            .pr(crate::ui::sp(16.))
            .pt(crate::ui::sp(6.))
            .pb(crate::ui::sp(20.))
            .child(
                div()
                    .relative()
                    .flex()
                    .flex_col()
                    .items_end()
                    .max_w_full()
                    .child(
                        div()
                            .max_w(px(576.))
                            .px(crate::ui::sp(12.))
                            .py(crate::ui::sp(8.))
                            .rounded(crate::ui::rad(if single { 18. } else { 12. }))
                            .bg(theme::raised())
                            .text_size(px(crate::ui::TEXT_LG))
                            .line_height(px(20.))
                            .text_color(theme::content())
                            .when(!text.is_empty(), |d| d.child(self.user_text(ix, shown)))
                            .when(!images.is_empty(), |d| {
                                d.child(
                                    div()
                                        .flex()
                                        .flex_wrap()
                                        .justify_end()
                                        .gap(crate::ui::sp(6.))
                                        .when(!text.is_empty(), |d| d.mt(px(8.)))
                                        .children(images.into_iter().enumerate().map(
                                            |(n, path)| {
                                                let open = path.clone();
                                                let (max_w, max_h) = if single_image {
                                                    (400., 220.)
                                                } else {
                                                    (200., 120.)
                                                };
                                                div()
                                                    .id(("user-image", ix * 64 + n))
                                                    .map(|d| {
                                                        match thumb_size(&path, max_w, max_h) {
                                                            Some((w, h)) => d.w(px(w)).h(px(h)),
                                                            // Unknown size: the old box, the image fit inside.
                                                            None => d
                                                                .h(px(max_h))
                                                                .max_w(px(max_w))
                                                                .min_w(px(48.)),
                                                        }
                                                    })
                                                    .rounded(crate::ui::rad(12.))
                                                    .overflow_hidden()
                                                    .border_1()
                                                    .border_color(theme::border_strong())
                                                    .bg(theme::fill_hover())
                                                    .cursor_pointer()
                                                    .child(
                                                        gpui::img(path)
                                                            .object_fit(gpui::ObjectFit::Contain)
                                                            .size_full()
                                                            .with_fallback(|| {
                                                                div()
                                                                .p(crate::ui::sp(8.))
                                                                .text_size(px(crate::ui::TEXT_SM))
                                                                .text_color(theme::text_subtle())
                                                                .child("Image not found")
                                                                .into_any_element()
                                                            }),
                                                    )
                                                    .on_click(cx.listener(move |_, _, _, cx| {
                                                        cx.emit(ThreadViewEvent::OpenFile(
                                                            open.clone(),
                                                            None,
                                                        ))
                                                    }))
                                            },
                                        )),
                                )
                            })
                            .when(!pastes.is_empty(), |d| {
                                d.child(
                                    div()
                                        .flex()
                                        .flex_wrap()
                                        .justify_end()
                                        .gap(crate::ui::sp(6.))
                                        .when(!text.is_empty(), |d| d.mt(px(6.)))
                                        .children(pastes.into_iter().enumerate().map(|(n, p)| {
                                            let copied = markdown::was_copied(&p);
                                            let lines = p.lines().count();
                                            div()
                                                .id(("user-paste", ix * 64 + n))
                                                .flex()
                                                .items_center()
                                                .gap(crate::ui::sp(6.))
                                                .h(px(24.))
                                                .px(crate::ui::sp(8.))
                                                .rounded(crate::ui::rad(8.))
                                                .bg(theme::fill_selected())
                                                .text_size(px(crate::ui::TEXT_SM))
                                                .cursor_pointer()
                                                .hover(|s| s.bg(theme::mix(0.14)))
                                                .child(
                                                    icon(
                                                        if copied { "tick02" } else { "file01" },
                                                        12.,
                                                    )
                                                    .text_color(theme::text_subtle()),
                                                )
                                                .child(
                                                    div()
                                                        .text_color(theme::text_muted())
                                                        .child("Pasted text"),
                                                )
                                                .child(
                                                    div().text_color(theme::text_subtle()).child(
                                                        if copied {
                                                            "copied".to_string()
                                                        } else {
                                                            format!("{lines} lines")
                                                        },
                                                    ),
                                                )
                                                .on_click(move |_, window, cx| {
                                                    markdown::copy_with_feedback(&p, window, cx)
                                                })
                                        })),
                                )
                            })
                            .when(long, |d| {
                                d.child(
                                    div()
                                        .id(("user-more", ix))
                                        .mt(px(4.))
                                        .px(crate::ui::sp(4.))
                                        .py(crate::ui::sp(2.))
                                        .rounded(crate::ui::rad(4.))
                                        .text_size(px(crate::ui::TEXT_SM))
                                        .text_color(theme::text_subtle())
                                        .cursor_pointer()
                                        .hover(|s| {
                                            s.bg(theme::fill_selected())
                                                .text_color(theme::content())
                                        })
                                        .child(if expanded { "Show less" } else { "Show more" })
                                        .on_click(cx.listener(move |this, _, _, cx| {
                                            this.toggle_open(ix, cx)
                                        })),
                                )
                            }),
                    )
                    .child(
                        div()
                            .absolute()
                            .top_full()
                            .right_0()
                            .flex()
                            .items_center()
                            .gap(crate::ui::sp(4.))
                            .px(crate::ui::sp(4.))
                            .pt(crate::ui::sp(2.))
                            .opacity(0.)
                            .group_hover(group, |s| s.opacity(1.))
                            .child(markdown::copy_button_sized(
                                ("user-copy", ix),
                                copy,
                                20.,
                                12.,
                            ))
                            .when(rewind, |d| {
                                d.child(
                                    div()
                                        .id(("user-rewind", ix))
                                        .size(px(20.))
                                        .flex()
                                        .items_center()
                                        .justify_center()
                                        .rounded(crate::ui::rad(4.))
                                        .cursor_pointer()
                                        .hover(|s| s.bg(theme::fill_selected()))
                                        .child(icon("undo", 12.).text_color(theme::text_subtle()))
                                        .on_click(cx.listener(move |this, _, _, cx| {
                                            this.thread.update(cx, |t, cx| t.begin_rewind(ix, cx))
                                        })),
                                )
                            })
                            .child(
                                div()
                                    .text_size(px(crate::ui::TEXT_SM))
                                    .text_color(theme::text_faint())
                                    .child(fmt_clock(at)),
                            ),
                    ),
            )
            .into_any_element()
    }

    fn live_header(
        &self,
        kind: AgentKind,
        status: &ThreadStatus,
        cancelling: bool,
        question: bool,
        elapsed: Option<Duration>,
        waiting_on: Option<(String, Option<String>)>,
        window: &Window,
    ) -> AnyElement {
        let label = if cancelling {
            "Stopping…".to_string()
        } else if *status == ThreadStatus::NeedsInput && question {
            "Waiting for your answer".to_string()
        } else if *status == ThreadStatus::NeedsInput {
            "Waiting for your approval".to_string()
        } else {
            match elapsed {
                Some(d) if d.as_millis() >= 1000 => {
                    format!("Working for {}", fmt_elapsed(d.as_millis() as u64))
                }
                _ => "Working…".to_string(),
            }
        };
        let waiting = *status == ThreadStatus::NeedsInput;
        // Nothing beats in a background window, so nothing draws there.
        let state = if cancelling || !window.is_window_active() {
            Beacon::Still
        } else if waiting {
            Beacon::Waiting
        } else {
            Beacon::Working
        };
        let color = if waiting {
            amber()
        } else if cancelling {
            theme::text_faint()
        } else {
            theme::working()
        };
        div()
            .flex()
            .items_center()
            .gap(crate::ui::sp(8.))
            .px(crate::ui::sp(16.))
            .py(crate::ui::sp(4.))
            .text_size(px(crate::ui::TEXT_LG))
            .text_color(theme::text_subtle())
            .child(beacon(state, color))
            .child(div().flex_none().child(kind.label()))
            .child(div().text_color(theme::text_faint()).child("·"))
            .child(
                div()
                    .flex_none()
                    .when(waiting, |d| d.text_color(amber()))
                    .child(label),
            )
            .when_some(
                waiting_on.filter(|_| *status == ThreadStatus::Working && !cancelling),
                |d, (what, cmd)| {
                    d.child(div().flex_none().text_color(theme::text_faint()).child("·"))
                        .child(
                            div()
                                .flex_none()
                                .text_color(theme::text_faint())
                                .child(what),
                        )
                        .when_some(cmd, |d, cmd| {
                            d.child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .truncate()
                                    .font_family(theme::FONT_MONO)
                                    .text_size(px(crate::ui::TEXT_MD))
                                    .text_color(theme::text_subtle())
                                    .child(cmd),
                            )
                        })
                },
            )
            .into_any_element()
    }

    fn toggle_phase(&mut self, key: usize, open: bool, cx: &mut Context<Self>) {
        self.phase_open.insert(key, !open);
        self.opened(Fold::Phase, key, !open);
        self.remeasure_entry(key, cx);
        cx.notify();
    }

    /// One phase of the work trail: the prose that opened it, then a summary
    /// line that folds the steps under a curved rail.
    #[allow(clippy::too_many_arguments)]
    fn render_phase(
        &mut self,
        phase: &Phase,
        content: &[Entry],
        base: usize,
        cwd: &Path,
        live: bool,
        active: bool,
        permission: Option<&PermissionRequest>,
        window: &Window,
        cx: &Context<Self>,
    ) -> AnyElement {
        let mut col = div().flex().flex_col().min_w_0();
        if let Some(h) = phase.head
            && let Entry::Agent { text } = &content[h]
        {
            let head = div()
                .min_w_0()
                .pt(crate::ui::sp(8.))
                .pb(crate::ui::sp(4.))
                .child(self.surface(base + h, text, MdStyle::reply(), cx))
                .into_any_element();
            col = col.child(self.arrive(base + h, head, cx));
        }
        if phase.steps.is_empty() {
            return col.into_any_element();
        }
        if phase.head.is_none() && phase.steps.len() == 1 {
            let i = phase.steps[0];
            return col
                .child(self.step(i, content, base, cwd, live, permission, window, cx))
                .into_any_element();
        }
        // A subagent's call waiting for an answer opens its Agent call's phase.
        let asking_parent = permission.and_then(|p| {
            content.iter().find_map(|e| match e {
                Entry::Tool(t) if t.id == p.tool_call_id => t.parent.clone(),
                _ => None,
            })
        });
        let asking = permission.is_some_and(|p| {
            phase.steps.iter().any(|&i| {
                matches!(&content[i], Entry::Tool(t)
                    if t.id == p.tool_call_id || asking_parent.as_deref() == Some(t.id.as_str()))
            })
        });
        let key = base + phase.head.unwrap_or(phase.steps[0]);
        let open = self
            .phase_open
            .get(&key)
            .copied()
            .unwrap_or(active || asking);
        let steps: Vec<&Entry> = phase.steps.iter().map(|&i| &content[i]).collect();
        let (glyph, summary) = phase_summary(&steps);
        let group: SharedString = format!("phase-{key}").into();
        col = col.child(
            div()
                .id(("phase", key))
                .group(group.clone())
                .flex()
                .items_center()
                .gap(crate::ui::sp(6.))
                .py(crate::ui::sp(4.))
                .min_w_0()
                .cursor_pointer()
                .child(icon(glyph, 14.).text_color(theme::text_subtle()))
                .child(
                    div()
                        .min_w_0()
                        .truncate()
                        .text_size(px(crate::ui::TEXT_LG))
                        .text_color(theme::text_subtle())
                        .group_hover(group.clone(), |s| s.text_color(theme::text_muted()))
                        .child(summary),
                )
                .child(chevron(
                    ("phase-chevron", key),
                    open,
                    icon("arrow-right01", 12.)
                        .text_color(theme::text_faint())
                        .group_hover(group, |s| s.text_color(theme::text_muted())),
                ))
                .on_click(cx.listener(move |this, _, _, cx| this.toggle_phase(key, open, cx))),
        );
        if !open {
            return col.into_any_element();
        }
        let n = phase.steps.len();
        let mut rail = div().flex().flex_col().flex_none().min_w_0();
        for (k, &i) in phase.steps.iter().enumerate() {
            let row = self.step(i, content, base, cwd, live, permission, window, cx);
            rail = rail.child(rail_step(k + 1 == n, row));
        }
        let rail = self.unfold(Fold::Phase, key, rail.into_any_element(), cx);
        // A long live phase scrolls inside a window that follows its newest
        // step until scrolled up, and again once back at the bottom.
        if active && !asking && n >= LIVE_WINDOW_STEPS {
            let scroll = match &self.live_scroll {
                Some((k, h)) if *k == key => h.clone(),
                _ => {
                    let h = ScrollHandle::new();
                    self.live_scroll = Some((key, h.clone()));
                    h
                }
            };
            let at_bottom = scroll.offset().y <= -scroll.max_offset().y + px(2.);
            if at_bottom {
                scroll.scroll_to_bottom();
            }
            let hidden_above = scroll.offset().y < -px(1.);
            let wheel = scroll.clone();
            col.child(
                div()
                    .relative()
                    .min_w_0()
                    // The window takes the wheel while it can move that way;
                    // at either end the conversation scrolls instead.
                    .on_scroll_wheel(move |ev: &ScrollWheelEvent, window, cx| {
                        let dy = ev.delta.pixel_delta(window.line_height()).y;
                        let before = wheel.offset().y - dy;
                        let max = wheel.max_offset().y;
                        if (dy > px(0.) && before < px(0.)) || (dy < px(0.) && before > -max) {
                            cx.stop_propagation();
                        }
                    })
                    .child(
                        div()
                            .id(("phase-window", key))
                            .max_h(px(LIVE_WINDOW_H))
                            .overflow_y_scroll()
                            .track_scroll(&scroll)
                            .child(rail),
                    )
                    .when(hidden_above, |d| {
                        d.child(div().absolute().top_0().left_0().right_0().h(px(32.)).bg(
                            linear_gradient(
                                180.,
                                linear_color_stop(theme::background(), 0.),
                                linear_color_stop(theme::background().opacity(0.), 1.),
                            ),
                        ))
                    }),
            )
            .into_any_element()
        } else {
            col.child(rail).into_any_element()
        }
    }
}

impl ThreadView {
    /// A step of the trail; an Agent call carries its subagent's calls beneath
    /// it while it runs, while one of them waits for an answer, or when opened.
    #[allow(clippy::too_many_arguments)]
    fn step(
        &mut self,
        i: usize,
        content: &[Entry],
        base: usize,
        cwd: &Path,
        live: bool,
        permission: Option<&PermissionRequest>,
        window: &Window,
        cx: &Context<Self>,
    ) -> AnyElement {
        let row = self.activity_row(base + i, &content[i], cwd, live, permission, window, cx);
        let Entry::Tool(agent) = &content[i] else {
            return row;
        };
        let calls = subagent_calls(content, &agent.id);
        if calls.is_empty() {
            return row;
        }
        let running = live && matches!(agent.status, ToolStatus::Pending | ToolStatus::InProgress);
        let asking = permission.is_some_and(|p| {
            calls
                .iter()
                .any(|&c| matches!(&content[c], Entry::Tool(t) if t.id == p.tool_call_id))
        });
        if !(running || asking || self.open.contains(&(base + i))) {
            return row;
        }
        let n = calls.len();
        let mut rail = div().flex().flex_col().min_w_0().pl(crate::ui::sp(4.));
        for (k, &c) in calls.iter().enumerate() {
            let child = self.activity_row(base + c, &content[c], cwd, live, permission, window, cx);
            rail = rail.child(rail_step(k + 1 == n, child));
        }
        let rail = self.unfold(Fold::Row, base + i, rail.into_any_element(), cx);
        div()
            .flex()
            .flex_col()
            .min_w_0()
            .child(row)
            .child(rail)
            .into_any_element()
    }
}

/// Steps in the trail between two stretches of prose.
#[derive(Clone)]
struct Phase {
    /// The prose that opened the phase.
    head: Option<usize>,
    steps: Vec<usize>,
}

/// The entries of a turn shown as its reply: the agent messages it ends with.
/// A settled turn that ends in tool calls instead (Droid ticks its todo list
/// after writing the summary) keeps its last agent messages as the reply, so
/// the summary doesn't fold away with the work.
fn reply_span(content: &[Entry], settled: bool) -> Range<usize> {
    let run_start = |end: usize| {
        content[..end]
            .iter()
            .rposition(|e| !matches!(e, Entry::Agent { .. }))
            .map_or(0, |i| i + 1)
    };
    let tail = run_start(content.len());
    let tail_has_text = content[tail..]
        .iter()
        .any(|e| matches!(e, Entry::Agent { text } if !text.trim().is_empty()));
    if !settled || tail_has_text {
        return tail..content.len();
    }
    match content
        .iter()
        .rposition(|e| matches!(e, Entry::Agent { text } if !text.trim().is_empty()))
    {
        Some(last) => run_start(last)..last + 1,
        None => content.len()..content.len(),
    }
}

/// How a turn's entries split on screen.
struct TurnParts {
    /// 1 when the turn opens with the user's prompt.
    body_start: usize,
    /// Notices and compactions drawn after everything else.
    trailing_notices: usize,
    /// The rest, the content, is indexed from `body_start`: its reply,
    reply: Range<usize>,
    /// the plan kept above the reply once settled,
    plan: Option<usize>,
    /// and the work folded behind "Worked for".
    work: Vec<usize>,
}

fn turn_parts(entries: &[Entry], live: bool) -> TurnParts {
    let body_start = usize::from(matches!(entries.first(), Some(Entry::User { .. })));
    let body = &entries[body_start..];
    let trailing_notices = body
        .iter()
        .rev()
        .take_while(|e| matches!(e, Entry::Notice { .. } | Entry::Compacted { .. }))
        .count();
    let content = &body[..body.len() - trailing_notices];
    let reply = reply_span(content, !live);
    // While the agent works the latest plan sits in the trail; once settled it
    // stays visible above the reply rather than folding away with the work.
    let plan = (!live)
        .then(|| {
            (0..content.len())
                .rev()
                .filter(|i| !reply.contains(i))
                .find(|&i| matches!(content[i], Entry::Plan { .. }))
        })
        .flatten();
    let work = (0..content.len())
        .filter(|&i| !reply.contains(&i) && Some(i) != plan && !nested(content, i))
        .collect();
    TurnParts {
        body_start,
        trailing_notices,
        reply,
        plan,
        work,
    }
}

/// A turn split into phases: prose starts one, thoughts and tool calls join the current one.
fn phases_of(content: &[Entry], work: &[usize]) -> Vec<Phase> {
    let mut phases: Vec<Phase> = Vec::new();
    for &i in work {
        match &content[i] {
            Entry::Agent { .. } => phases.push(Phase {
                head: Some(i),
                steps: Vec::new(),
            }),
            _ => match phases.last_mut() {
                Some(p) => p.steps.push(i),
                None => phases.push(Phase {
                    head: None,
                    steps: vec![i],
                }),
            },
        }
    }
    phases
}

/// The calls a subagent made, nested under its Agent call rather than in the trail.
fn subagent_calls(content: &[Entry], parent: &str) -> Vec<usize> {
    (0..content.len())
        .filter(|&i| matches!(&content[i], Entry::Tool(t) if t.parent.as_deref() == Some(parent)))
        .collect()
}

/// A call whose Agent call is in the same turn: it shows under that one.
fn nested(content: &[Entry], i: usize) -> bool {
    let Entry::Tool(t) = &content[i] else {
        return false;
    };
    t.parent.as_deref().is_some_and(|p| {
        content
            .iter()
            .any(|e| matches!(e, Entry::Tool(o) if o.id == p))
    })
}

/// The icon of a phase's main kind of work and a line of what it did:
/// "Edited 2 files · Ran a command".
fn phase_summary(steps: &[&Entry]) -> (&'static str, String) {
    let mut files: HashSet<String> = HashSet::new();
    let (mut edits, mut runs, mut reads, mut searches, mut agents, mut other, mut plans) =
        (0, 0, 0, 0, 0, 0, 0);
    for e in steps {
        match e {
            Entry::Tool(t) if t.is_subagent() => agents += 1,
            Entry::Tool(t) => match t.kind.as_str() {
                "edit" | "delete" | "move" => {
                    edits += 1;
                    let path = t.locations.first().cloned().or_else(|| {
                        t.content.iter().find_map(|c| match c {
                            ToolContent::Diff { path, .. } => Some(path.clone()),
                            _ => None,
                        })
                    });
                    files.insert(path.map_or_else(|| t.title.clone(), |p| p.display().to_string()));
                }
                "execute" => runs += 1,
                "read" => reads += 1,
                "search" | "fetch" => searches += 1,
                "think" => {}
                _ => other += 1,
            },
            Entry::Plan { .. } => plans += 1,
            _ => {}
        }
    }
    let plural = |n: usize, one: &str, many: &str| {
        if n == 1 {
            one.to_string()
        } else {
            many.replace('#', &n.to_string())
        }
    };
    let mut parts = Vec::new();
    if edits > 0 {
        parts.push(plural(
            files.len().max(1),
            "Edited a file",
            "Edited # files",
        ));
    }
    if reads > 0 && searches == 0 {
        parts.push(plural(reads, "Read a file", "Read # files"));
    } else if reads + searches > 0 {
        parts.push("Explored the project".to_string());
    }
    if runs > 0 {
        parts.push(plural(runs, "Ran a command", "Ran # commands"));
    }
    if agents > 0 {
        parts.push(plural(agents, "Ran an agent", "Ran # agents"));
    }
    if other > 0 {
        parts.push(plural(other, "Used a tool", "Used # tools"));
    }
    if plans > 0 {
        parts.push("Updated the plan".to_string());
    }
    if parts.is_empty() {
        parts.push("Thought".to_string());
    }
    // Ties go to the kind listed first.
    let kinds = [
        (edits, "pencil-edit01"),
        (runs, "computer-terminal01"),
        (reads + searches, "search01"),
        (agents, "bot"),
        (other, "wrench01"),
    ];
    let glyph = kinds
        .iter()
        .fold(
            (0, "ai-idea"),
            |best, &(n, g)| if n > best.0 { (n, g) } else { best },
        )
        .1;
    (glyph, parts.join(" · "))
}

/// A step under a phase: a spine down the left that curves into the row.
fn rail_step(last: bool, row: AnyElement) -> AnyElement {
    crate::ui::tree_item(last, 15., 0., row)
}

/// Under a settled reply: copy it, and when it was sent and what it used.
/// Shown on hover, and always for the latest turn.
fn turn_footer(
    ix: usize,
    reply: String,
    at: u64,
    tokens: Option<u64>,
    elapsed_ms: Option<u64>,
    latest: bool,
) -> AnyElement {
    let mut meta = vec![fmt_clock(at)];
    if let Some(ms) = elapsed_ms {
        meta.push(fmt_elapsed(ms));
    }
    if let Some(n) = tokens {
        meta.push(format!("{} tokens", super::thread::fmt_count(n)));
    }
    let meta: Vec<String> = meta.into_iter().filter(|m| !m.is_empty()).collect();
    div()
        .flex()
        .items_center()
        .gap(crate::ui::sp(6.))
        .h(px(28.))
        .px(crate::ui::sp(10.))
        .when(!latest, |d| {
            d.opacity(0.)
                .group_hover(SharedString::from(format!("turn-{ix}")), |s| s.opacity(1.))
        })
        .child(markdown::copy_button(("reply-copy", ix), reply))
        .child(
            div()
                .text_size(px(crate::ui::TEXT_SM))
                .text_color(theme::text_faint())
                .child(meta.join(" · ")),
        )
        .into_any_element()
}

/// How many messages an older "Compacted the conversation (n messages
/// summarized)." notice speaks of, for transcripts from before the marker.
fn legacy_compaction(text: &str) -> Option<u64> {
    let rest = text.strip_prefix("Compacted the conversation (")?;
    rest.split_whitespace().next()?.parse().ok()
}

/// A rule across the transcript where the conversation was summarized: what
/// is above it is no longer in the agent's context word for word.
fn compaction_divider(removed: u64, before: Option<u64>, after: Option<u64>) -> AnyElement {
    let mut label = "Session compacted".to_string();
    if removed > 0 {
        label.push_str(&format!(" · {removed} messages summarized"));
    }
    match (before, after) {
        (Some(b), Some(a)) => label.push_str(&format!(" · context {b}% → {a}%")),
        (Some(b), None) => label.push_str(&format!(" · context was {b}%")),
        (None, Some(a)) => label.push_str(&format!(" · context now {a}%")),
        (None, None) => {}
    }
    let rule = || div().flex_1().h(px(1.)).bg(theme::fill_selected());
    div()
        .flex()
        .items_center()
        .gap(crate::ui::sp(10.))
        .px(crate::ui::sp(16.))
        .py(crate::ui::sp(12.))
        .child(rule())
        .child(
            div()
                .flex()
                .flex_none()
                .items_center()
                .gap(crate::ui::sp(6.))
                .text_size(px(crate::ui::TEXT_SM))
                .text_color(theme::text_subtle())
                .child(icon("archive02", 13.).text_color(theme::text_subtle()))
                .child(label),
        )
        .child(rule())
        .into_any_element()
}

fn notice_row(text: &str, error: bool, mark: Option<NoticeMark>) -> AnyElement {
    let glyph = match (error, mark) {
        (true, _) => icon("alert-circle", 14.)
            .text_color(red().opacity(0.85))
            .into_any_element(),
        (false, Some(NoticeMark::Busy)) => {
            crate::ui::spinner(14., theme::mix(0.45)).into_any_element()
        }
        (false, Some(NoticeMark::Passed)) => icon("checkmark-circle02", 14.)
            .text_color(theme::success())
            .into_any_element(),
        (false, None) => icon("pause", 14.)
            .text_color(theme::text_faint())
            .into_any_element(),
    };
    div()
        .flex()
        .items_center()
        .gap(crate::ui::sp(6.))
        .px(crate::ui::sp(16.))
        .py(crate::ui::sp(6.))
        .text_size(px(crate::ui::TEXT_MD))
        .text_color(if error {
            red().opacity(0.85)
        } else {
            theme::mix(0.45)
        })
        .child(glyph)
        .child(
            div()
                .flex_1()
                .min_w_0()
                .child(crate::redact::text(text).into_owned()),
        )
        .into_any_element()
}

thread_local! {
    /// Syntax colors of the diff cards on screen, by their content.
    static DIFF_SPANS: RefCell<HashMap<u64, Option<Rc<Vec<LineSpans>>>>> = RefCell::new(HashMap::new());
}

impl Focusable for ThreadView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for ThreadView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.rendered = true;
        self.flush_pending(window, cx);
        let spacing = crate::ui::space_scale();
        if spacing != self.spacing {
            self.spacing = spacing;
            self.list.remeasure();
        }
        self.refresh_search(cx);
        let search_bar = self.render_search_bar(cx);
        let scrolled_away = !self.following && self.list.is_scrolled_to_end() == Some(false);
        let queue = self.render_queue(cx);
        let menu = self.render_selection_menu(cx);
        let note = self.render_note_box(cx);
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        let (ssh_bar, ssh_body) = self.render_ssh(cx);
        #[cfg(not(any(target_os = "linux", target_os = "macos")))]
        let (ssh_bar, ssh_body): (Option<AnyElement>, Option<AnyElement>) = (None, None);
        div()
            .id("thread-view")
            .key_context("ThreadView")
            .track_focus(&self.focus)
            .on_key_down(cx.listener(|this, ev, window, cx| {
                // Keys in SSH fields must never answer a transcript permission.
                #[cfg(any(target_os = "linux", target_os = "macos"))]
                if this.ssh.shown() {
                    return;
                }
                this.on_key(ev, window, cx);
            }))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, window, cx| {
                    if this.note.is_some() {
                        this.finish_note(false, window, cx);
                    }
                    if this.clear_selection() {
                        cx.notify();
                    }
                }),
            )
            .on_mouse_move(cx.listener(|this, ev, _, cx| this.select_move(ev, cx)))
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, ev, _, cx| this.select_end(ev, cx)),
            )
            .relative()
            .flex()
            .flex_col()
            .size_full()
            .min_h_0()
            .font_family(theme::FONT_SANS)
            .text_color(theme::content())
            .children(ssh_bar)
            .child(ssh_body.unwrap_or_else(|| {
                div()
                    .relative()
                    .flex_1()
                    .min_h_0()
                    .child(list(self.list.clone(), cx.processor(Self::render_item)).size_full())
                    .when_some(search_bar, |d, bar| d.child(bar))
                    .when(scrolled_away, |d| {
                        d.child(
                            div()
                                .absolute()
                                .bottom(px(8.))
                                .left_0()
                                .right_0()
                                .flex()
                                .justify_center()
                                .child(
                                    div()
                                        .id("jump-to-bottom")
                                        .size(px(24.))
                                        .flex()
                                        .items_center()
                                        .justify_center()
                                        .rounded(crate::ui::rad(8.))
                                        .border_1()
                                        .border_color(theme::border_strong())
                                        .bg(theme::fill_selected())
                                        .shadow_md()
                                        .cursor_pointer()
                                        .hover(|s| s.bg(theme::fill_hover()))
                                        .child(
                                            icon("arrow-down01", 16.).text_color(theme::content()),
                                        )
                                        .on_click(
                                            cx.listener(|this, _, _, cx| this.jump_to_bottom(cx)),
                                        ),
                                ),
                        )
                    })
                    .into_any_element()
            }))
            .when_some(queue, |d, q| d.child(q))
            .when_some(menu, |d, m| d.child(m))
            .when_some(note, |d, n| d.child(n))
    }
}

/// The box an image sent in a message shows in: its own size, scaled down to
/// fit `max_w` by `max_h` and never up, so a small image is not stretched
/// across a tall box. None when the file's size can't be read.
fn thumb_size(path: &Path, max_w: f32, max_h: f32) -> Option<(f32, f32)> {
    thread_local! {
        static SIZES: RefCell<HashMap<PathBuf, Option<(f32, f32)>>> = RefCell::new(HashMap::new());
    }
    let natural = SIZES.with(|m| {
        *m.borrow_mut().entry(path.to_path_buf()).or_insert_with(|| {
            imagesize::size(path)
                .ok()
                .map(|s| (s.width as f32, s.height as f32))
        })
    })?;
    Some(fit(natural, max_w, max_h))
}

/// `natural` scaled down to fit `max_w` by `max_h`, keeping its shape.
fn fit((w, h): (f32, f32), max_w: f32, max_h: f32) -> (f32, f32) {
    if w <= 0. || h <= 0. {
        return (max_w.min(48.), max_h.min(48.));
    }
    let scale = (max_w / w).min(max_h / h).min(1.);
    ((w * scale).round().max(1.), (h * scale).round().max(1.))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_sent_image_keeps_its_shape_and_is_never_enlarged() {
        // A small strip stays its own size instead of filling a tall box.
        assert_eq!(fit((73., 18.), 400., 220.), (73., 18.));
        // A big screenshot shrinks to the box, keeping its shape.
        assert_eq!(fit((1254., 1254.), 400., 220.), (220., 220.));
        assert_eq!(fit((1600., 400.), 400., 220.), (400., 100.));
        // A broken header doesn't give a zero-sized box.
        assert_eq!(fit((0., 0.), 400., 220.), (48., 48.));
    }

    fn agent(text: &str) -> Entry {
        Entry::Agent { text: text.into() }
    }

    fn tool(title: &str) -> Entry {
        serde_json::from_value(serde_json::json!({
            "type": "tool", "id": title, "title": title, "kind": "other", "status": "completed",
        }))
        .unwrap()
    }

    #[test]
    fn docs_of_removed_or_replaced_entries_are_dropped() {
        let entries = [
            agent("kept"),
            tool("Read"),
            Entry::Thought {
                text: "kept too".into(),
            },
        ];
        let docs = || -> HashMap<usize, ()> { [(0, ()), (1, ()), (2, ())].into() };
        let left = |mut docs: HashMap<usize, ()>| {
            let mut left: Vec<usize> = docs.drain().map(|(ix, _)| ix).collect();
            left.sort();
            left
        };

        let mut all = docs();
        keep_docs_of(&mut all, &entries, 0);
        assert_eq!(left(all), [0, 2]);

        let mut tail = docs();
        keep_docs_of(&mut tail, &entries, 2);
        assert_eq!(left(tail), [0, 1, 2]);

        let mut past_the_end = docs();
        keep_docs_of(&mut past_the_end, &entries, 9);
        assert_eq!(left(past_the_end), [0, 1, 2]);
    }

    #[test]
    fn reply_is_the_agent_messages_a_turn_ends_with() {
        let content = [
            tool("Read"),
            agent("looking"),
            tool("Grep"),
            agent("summary"),
            agent("more"),
        ];
        assert_eq!(reply_span(&content, true), 3..5);
        assert_eq!(reply_span(&content, false), 3..5);
    }

    #[test]
    fn settled_reply_survives_tool_calls_after_it() {
        let content = [
            tool("Read"),
            agent("summary"),
            agent("more"),
            tool("TodoWrite"),
            agent("  "),
        ];
        assert_eq!(reply_span(&content, false), 4..5);
        assert_eq!(reply_span(&content, true), 1..3);
        let content = &content[..4];
        assert_eq!(reply_span(content, true), 1..3);
        assert_eq!(reply_span(content, false), 4..4);
    }

    #[test]
    fn long_expanded_turn_splits_into_per_phase_list_rows() {
        let mut entries = vec![Entry::User {
            text: "do 1000 steps".into(),
            at: 0,
            elapsed_ms: None,
            tokens: None,
            message_id: None,
            shell_changes: Vec::new(),
        }];
        for i in 0..1000 {
            entries.push(agent(&format!("step {i}")));
            entries.push(tool("Read"));
        }
        entries.push(agent("LONGTHREAD-END"));
        let turns = turns_of(&entries);
        assert_eq!(turns.len(), 1);

        // Folded finished turn stays a single list row.
        let empty = HashSet::new();
        let (rows, long) = build_rows_from(&turns, &entries, |_| false, &empty);
        assert_eq!(rows, vec![ListRow::Turn(0)]);
        assert!(long.is_empty());

        // Live or unfolded 1000-phase turn splits into Head + 1000 Phases + Tail.
        let (rows, long) = build_rows_from(&turns, &entries, |_| true, &empty);
        assert_eq!(rows.len(), 1002);
        assert_eq!(rows[0], ListRow::TurnHead(0));
        assert_eq!(rows[1], ListRow::TurnPhase { turn: 0, phase: 0 });
        assert_eq!(
            rows[1000],
            ListRow::TurnPhase {
                turn: 0,
                phase: 999
            }
        );
        assert_eq!(rows[1001], ListRow::TurnTail(0));
        assert_eq!(long[&0].phases.len(), 1000);
    }

    #[test]
    fn turn_without_agent_text_has_no_reply() {
        let content = [tool("Read"), tool("Grep")];
        assert_eq!(reply_span(&content, true), 2..2);
        assert_eq!(reply_span(&[], true), 0..0);
    }

    fn edit(path: &str, old: Option<&str>, new: &str, added: u32, status: &str) -> Entry {
        serde_json::from_value(serde_json::json!({
            "type": "tool", "id": path, "title": "Edit", "kind": "edit", "status": status,
            "content": [{"type": "diff", "path": path, "old_text": old, "new_text": new}],
            "added": added,
        }))
        .unwrap()
    }

    #[test]
    fn turn_edits_group_finished_diffs_by_file() {
        let content = [
            edit("a.rs", Some("x"), "x\ny", 1, "completed"),
            tool("Read"),
            edit("b.rs", None, "new", 1, "completed"),
            edit("a.rs", Some("y"), "y\nz", 1, "completed"),
            edit("c.rs", None, "nope", 9, "failed"),
        ];
        let shell = [ShellChange {
            path: "/w/gen.rs".into(),
            old: Some("a".into()),
            new: "b".into(),
            added: 1,
            removed: 1,
        }];
        let edits = turn_edits(&shell, &content);
        let files: Vec<(&Path, usize)> = edits.files.iter().map(|(p, d)| (*p, d.len())).collect();
        assert_eq!(
            files,
            [
                (Path::new("a.rs"), 2),
                (Path::new("b.rs"), 1),
                (Path::new("/w/gen.rs"), 1)
            ]
        );
        assert_eq!(edits.files[0].1[1].at, DiffAt::Tool(3, 0));
        assert_eq!(edits.files[2].1[0].at, DiffAt::Shell(0));
        assert_eq!((edits.added, edits.removed), (4, 1));
    }

    #[test]
    fn changed_line_finds_the_snippet_in_the_file() {
        let file = "a\nb\nfn main() {\n    hola();\n}\n";
        assert_eq!(
            changed_line(file, Some("fn main() {\n}"), "fn main() {\n    hola();\n}"),
            Some(4)
        );
        assert_eq!(changed_line(file, None, "a\nb"), Some(1));
        assert_eq!(changed_line(file, Some("x"), "gone"), None);
    }
}
