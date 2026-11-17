//! The GitHub tab: a review work queue and the pull request or issue picked in
//! it. On the left, what waits on the user (from the inbox and from searches,
//! so what was read on github.com still shows) and the week's review pulse; on
//! the right, the item: its state, what blocks it, its timeline, files, checks
//! and commits, and the actions. Writing to GitHub always takes a second step,
//! and an agent's review waits as a draft until the user posts it.

mod diff;
mod item;
mod pulse;
mod queue;
mod timeline;

use std::cell::{Cell, RefCell};
use std::collections::{BTreeSet, HashMap, HashSet};
use std::rc::Rc;

use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, AnyView, App, AppContext as _, Bounds, Context, Entity, EventEmitter, FocusHandle,
    Focusable, InteractiveElement as _, IntoElement, KeyDownEvent, ListAlignment, ListState,
    MouseButton, ParentElement as _, Pixels, Point, Render, ScrollHandle, StyleRefinement,
    Styled as _, Subscription, WeakEntity, Window, canvas, div, px,
};

use crate::agent::markdown::{self, Block, MdStyle};
use crate::github::pr::{self, Draft, FileDiff, ItemKind, ItemRef, MergeMethod, Verdict};
use crate::github::queue::{Details, Entry, Group};
use crate::github::{GitHub, Problem};
use crate::input::{InputEvent, TextInput};
use crate::theme;
use crate::ui::{self, Type, Typography as _};

/// Under this width the queue and the item take turns instead of sharing.
const NARROW: f32 = 900.;
/// From this width the left column is at its full width, with the pulse.
const WIDE: f32 = 1300.;
/// The item card needs this much to keep its properties in a column beside
/// the tabs; under it they wrap above them and the timeline gets the width.
const SIDE_PROPS: f32 = 1050.;
/// Under this height the pulse folds into a strip even when wide.
const TALL: f32 = 860.;
const GAP: f32 = 12.;

/// How the tab lays itself out at a size.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Layout {
    /// The queue and the item take turns.
    narrow: bool,
    /// The left column's width (the whole width when narrow).
    left_w: f32,
    /// The pulse as a full card; else a strip that opens on a click.
    pulse_full: bool,
    /// The item's properties in a column beside the tabs.
    side_props: bool,
    /// The item card's width.
    item_w: f32,
}

fn layout(w: f32, h: f32) -> Layout {
    // The gutters `render` draws follow the Spacing setting.
    let gap = f32::from(crate::ui::sp(GAP));
    if w < NARROW {
        return Layout {
            narrow: true,
            left_w: w,
            pulse_full: false,
            side_props: w - 2. * gap >= SIDE_PROPS,
            item_w: w - 2. * gap,
        };
    }
    let wide = w >= WIDE;
    let left_w = if wide {
        (w * 0.3).clamp(400., 440.)
    } else {
        (w * 0.34).clamp(340., 400.)
    };
    let item_w = w - left_w - 3. * gap;
    Layout {
        narrow: false,
        left_w,
        pulse_full: wide && h >= TALL,
        side_props: item_w >= SIDE_PROPS,
        item_w,
    }
}

pub enum GitHubViewEvent {
    /// Show a GitHub page.
    Open(String),
    /// Start an agent session.
    Ask { title: String, prompt: String },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Tab {
    Timeline,
    Files,
    Checks,
    Commits,
}

/// A write to GitHub waiting for its second click.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Confirm {
    Approve,
    RequestChanges,
    Merge(MergeMethod),
    Close,
    /// Post the agent's draft review (`confirm_draft`).
    PostDraft,
}

enum DiffState {
    Loading,
    Failed(String),
    Ready { files: Rc<Vec<FileDiff>>, cut: bool },
}

/// A pull request's diff, for the head commit and base it was read at.
struct Diff {
    /// While loading, what the details showed; once read, what GitHub said
    /// the diff is of (`pr::fetch_diff_at`).
    tip: pr::Tip,
    /// Which load this is: an older load that ends late is dropped.
    load: u64,
    state: DiffState,
}

/// Whether a diff read at `read_at` still shows the pull request whose
/// details say `head` and `base`: a push or a retarget makes it stale.
/// Unknown details (not read yet) keep what there is.
fn diff_is_current(read_at: &pr::Tip, head: &str, base: &str) -> bool {
    head.is_empty() || (read_at.head == head && read_at.base == base)
}

/// Keeps `typed` as the unsent comment of `from` (an empty one is dropped)
/// and takes out the one kept for `to`: what the input shows next.
fn swap_comment(
    drafts: &mut HashMap<String, String>,
    from: Option<&ItemRef>,
    to: Option<&ItemRef>,
    typed: String,
) -> String {
    if let Some(from) = from {
        let key = crate::github::queue::key_of(from);
        if typed.trim().is_empty() {
            drafts.remove(&key);
        } else {
            drafts.insert(key, typed);
        }
    }
    to.and_then(|to| drafts.remove(&crate::github::queue::key_of(to)))
        .unwrap_or_default()
}

/// What the last action did, shown in the card.
struct Note {
    ok: bool,
    /// Not a failure to fix: this Arakun may not write to GitHub.
    quiet: bool,
    text: String,
}

impl Note {
    fn ok(text: impl Into<String>) -> Self {
        Note {
            ok: true,
            quiet: false,
            text: text.into(),
        }
    }

    fn failed(text: impl Into<String>) -> Self {
        Note {
            ok: false,
            quiet: false,
            text: text.into(),
        }
    }
}

pub struct GitHubView {
    github: Entity<GitHub>,
    /// The entry picked in the queue, kept while the queue drops it (it was
    /// read, or merged).
    picked: Option<Entry>,
    /// The narrow layout shows the item instead of the queue.
    narrow_open: bool,
    tab: Tab,
    done_open: bool,
    /// Where the header menu opened, while it is open.
    menu_at: Option<Point<Pixels>>,
    /// Where the focus menu (which owners and repositories show) is open.
    focus_at: Option<Point<Pixels>>,
    watch_input: Entity<TextInput>,
    watch_note: Option<&'static str>,
    /// The threads "Mark all read" would mark when it was first clicked.
    /// It asks once more before marking, since GitHub cannot undo it; a list
    /// that changed in between asks again.
    confirm_all: Option<BTreeSet<String>>,
    comment: Entity<TextInput>,
    /// The item the comment input's text was typed for. Text is kept per
    /// item, so a comment written for one is never sent to the next.
    comment_for: Option<ItemRef>,
    /// Unsent comments of the items not shown, by `queue::key_of`.
    comment_drafts: HashMap<String, String>,
    /// The text of a review being confirmed, or of the draft being edited.
    body_input: Entity<TextInput>,
    confirm: Option<Confirm>,
    /// The pull request's head commit when the write was asked: what the user
    /// saw. The write holds to it, so new commits make it stop.
    confirm_head: String,
    /// The draft as it was when its post was asked: an agent may write a new
    /// one before the second click, and that one wasn't read.
    confirm_draft: Option<Draft>,
    editing_draft: bool,
    busy: Option<&'static str>,
    note: Option<Note>,
    draft: Option<Draft>,
    diffs: HashMap<String, Diff>,
    /// Counts diff loads, to tell a late one from the latest.
    diff_loads: u64,
    /// Files of the open diff the user folded.
    folded: HashSet<usize>,
    /// Files of the open diff shown in full past the first lines.
    unfolded_lines: HashSet<usize>,
    /// Repository → the merge methods it allows; None while it is asked.
    merge_methods: HashMap<String, Option<Vec<MergeMethod>>>,
    /// Markdown already parsed, by its text.
    md: RefCell<HashMap<String, Rc<Vec<Block>>>>,
    /// Drafts parsed as written, HTML and all, by their text.
    md_raw: RefCell<HashMap<String, Rc<Vec<Block>>>>,
    /// The start of long bodies, parsed for their fold, by the whole text.
    md_start: RefCell<HashMap<String, Rc<Vec<Block>>>>,
    /// The size this view got last frame: (width, height).
    size: Rc<Cell<(f32, f32)>>,
    /// The folded pulse strip is open.
    pulse_open: bool,
    /// All the item's properties show in a short window.
    head_open: bool,
    /// Timeline bodies shown in full past their first lines, by key.
    expanded: HashSet<String>,
    queue_scroll: ScrollHandle,
    body_scroll: ScrollHandle,
    /// The timeline of the picked item, and the list that shows it: only the
    /// rows on screen are built and laid out, so scrolling a long one stays
    /// cheap.
    timeline: Option<Rc<timeline::Timeline>>,
    timeline_list: ListState,
    /// The Files tab's list, the same way.
    files_rows: Option<diff::FilesRows>,
    files_list: ListState,
    /// The queue, the item card and the card's scrolling body, each a view of
    /// its own (see `Pane`).
    queue_pane: Entity<Pane>,
    card_pane: Entity<Pane>,
    body_pane: Entity<Pane>,
    focus: FocusHandle,
    _subs: Vec<Subscription>,
}

/// A part of the tab drawn as a view of its own, from the tab's state. A
/// scroll redraws only the view it scrolls in (and the views around it), so
/// with the queue and the item card as panes GPUI reuses last frame's layout
/// of the part that didn't move, instead of building the whole tab again on
/// every step of the wheel.
pub(super) struct Pane {
    view: WeakEntity<GitHubView>,
    draw: fn(&mut GitHubView, &mut Context<GitHubView>) -> AnyElement,
    _follow: Subscription,
}

impl Pane {
    fn new(
        view: &Entity<GitHubView>,
        draw: fn(&mut GitHubView, &mut Context<GitHubView>) -> AnyElement,
        cx: &mut App,
    ) -> Entity<Self> {
        cx.new(|cx| Self {
            view: view.downgrade(),
            draw,
            // The pane shows the tab's state: whatever changes the tab changes it.
            _follow: cx.observe(view, |_, _, cx| cx.notify()),
        })
    }

    /// The pane in the place `style` lays out; it fills that place.
    fn cached(pane: &Entity<Self>, style: StyleRefinement) -> AnyElement {
        AnyView::from(pane.clone()).cached(style).into_any_element()
    }
}

impl Render for Pane {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.view
            .update(cx, self.draw)
            .unwrap_or_else(|_| div().into_any_element())
    }
}

impl EventEmitter<GitHubViewEvent> for GitHubView {}

/// Newlines in a draft, as one-line input shows them.
const NEWLINE: &str = "⏎";

impl GitHubView {
    pub fn new(github: Entity<GitHub>, cx: &mut Context<Self>) -> Self {
        let watch_input = cx.new(|cx| TextInput::new("organization or owner/repo", cx));
        let comment = cx.new(|cx| TextInput::new("Comment, or type / for actions", cx));
        let body_input = cx.new(|cx| TextInput::new("Say why (optional)", cx));
        let subs = vec![
            cx.observe(&github, |this, _, cx| this.on_github_changed(cx)),
            cx.subscribe(&watch_input, |this, _, ev: &InputEvent, cx| match ev {
                InputEvent::Submit => this.add_watch(cx),
                InputEvent::Changed => {
                    this.watch_note = None;
                    cx.notify();
                }
                _ => {}
            }),
            cx.subscribe(&comment, |this, _, ev: &InputEvent, cx| match ev {
                InputEvent::SubmitAlt => this.send_comment(cx),
                InputEvent::Submit => this.submit_command(cx),
                InputEvent::Cancel => {
                    this.comment.update(cx, |i, cx| i.set_text("", cx));
                }
                _ => {}
            }),
        ];
        let me = cx.entity();
        let queue_pane = Pane::new(
            &me,
            |this, cx| {
                // The queue card grows to fill the column, as it did inline.
                div()
                    .flex()
                    .flex_col()
                    .size_full()
                    .child(this.render_queue(cx))
                    .into_any_element()
            },
            cx,
        );
        let card_pane = Pane::new(
            &me,
            |this, cx| {
                let lay = this.layout();
                div()
                    .flex()
                    .size_full()
                    .child(this.render_item(lay, cx))
                    .into_any_element()
            },
            cx,
        );
        let body_pane = Pane::new(&me, |this, cx| this.render_body(cx), cx);
        let mut this = Self {
            github,
            picked: None,
            narrow_open: false,
            tab: Tab::Timeline,
            done_open: false,
            menu_at: None,
            focus_at: None,
            watch_input,
            watch_note: None,
            confirm_all: None,
            comment,
            comment_for: None,
            comment_drafts: HashMap::new(),
            body_input,
            confirm: None,
            confirm_head: String::new(),
            confirm_draft: None,
            editing_draft: false,
            busy: None,
            note: None,
            draft: None,
            diffs: HashMap::new(),
            diff_loads: 0,
            folded: HashSet::new(),
            unfolded_lines: HashSet::new(),
            merge_methods: HashMap::new(),
            md: RefCell::new(HashMap::new()),
            md_raw: RefCell::new(HashMap::new()),
            md_start: RefCell::new(HashMap::new()),
            size: Rc::new(Cell::new((1200., 900.))),
            pulse_open: false,
            head_open: false,
            expanded: HashSet::new(),
            queue_scroll: ScrollHandle::new(),
            body_scroll: ScrollHandle::new(),
            timeline: None,
            timeline_list: ListState::new(0, ListAlignment::Top, px(600.)),
            files_rows: None,
            files_list: ListState::new(0, ListAlignment::Top, px(600.)),
            focus: cx.focus_handle(),
            queue_pane,
            card_pane,
            body_pane,
            _subs: subs,
        };
        this.resubscribe_body(cx);
        this
    }

    pub fn focus(&self, window: &mut Window, cx: &mut App) {
        window.focus(&self.focus, cx);
    }

    fn layout(&self) -> Layout {
        let (w, h) = self.size.get();
        layout(w, h)
    }

    fn narrow(&self) -> bool {
        self.layout().narrow
    }

    /// A person's avatar: their picture once downloaded, else initials.
    fn face(&self, login: &str, size: f32, cx: &App) -> gpui::Div {
        ui::avatar_image(login, self.github.read(cx).avatar(login), size)
    }

    /// After the focus changes: an item that left the queue isn't shown any
    /// more, the first one of the new queue is.
    fn after_focus_change(&mut self, cx: &mut Context<Self>) {
        let gone = self.picked.as_ref().is_some_and(|p| {
            !self
                .github
                .read(cx)
                .queue()
                .rows
                .iter()
                .any(|(e, _)| e.key() == p.key())
        });
        if gone {
            self.picked = None;
            self.narrow_open = false;
            self.switch_comment(cx);
        }
        self.on_github_changed(cx);
    }

    /// Keeps the picked entry fresh, picks the first row when nothing is
    /// picked yet, and reads the draft again: an agent may have left one.
    fn on_github_changed(&mut self, cx: &mut Context<Self>) {
        let fresh = {
            let q = self.github.read(cx).queue();
            match &self.picked {
                Some(p) => q
                    .rows
                    .iter()
                    .find(|(e, _)| e.key() == p.key())
                    .map(|(e, _)| e.clone()),
                None => q
                    .rows
                    .iter()
                    .find(|(_, p)| p.group != Group::Done)
                    .map(|(e, _)| e.clone()),
            }
        };
        match (fresh, self.picked.is_some()) {
            (Some(e), true) => self.picked = Some(e),
            (Some(e), false) if !self.narrow() => self.select(e, false, cx),
            _ => {}
        }
        // A push moves the head: the files shown must move with it.
        if self.tab == Tab::Files
            && let Some(item) = self.item()
        {
            self.load_diff(item, cx);
        }
        self.reload_draft();
        cx.notify();
    }

    fn reload_draft(&mut self) {
        if self.editing_draft {
            return;
        }
        self.draft = self
            .item()
            .filter(|i| i.kind == ItemKind::Pull)
            .and_then(|i| pr::load_draft(&i));
    }

    /// The pull request or issue picked, if the pick is one.
    fn item(&self) -> Option<ItemRef> {
        self.picked.as_ref().and_then(|e| e.item.clone())
    }

    /// Shows a pull request or issue, as the palette opens one.
    pub fn reveal(&mut self, item: &ItemRef, cx: &mut Context<Self>) {
        let key = crate::github::queue::key_of(item);
        let found = self
            .github
            .read(cx)
            .queue()
            .rows
            .iter()
            .find(|(e, _)| e.key() == key)
            .map(|(e, _)| e.clone());
        let entry = found.unwrap_or_else(|| Entry {
            item: Some(item.clone()),
            title: item.label(),
            author: String::new(),
            updated_at: String::new(),
            closed_at: String::new(),
            state: String::new(),
            why: Vec::new(),
            notification: None,
        });
        self.select(entry, true, cx);
    }

    /// Picks an entry; `open` shows it in the narrow layout too (a click,
    /// not the first pick made for the user).
    fn select(&mut self, entry: Entry, open: bool, cx: &mut Context<Self>) {
        let same = self.picked.as_ref().is_some_and(|p| p.key() == entry.key());
        self.picked = Some(entry);
        if open {
            self.narrow_open = true;
        }
        if !same {
            self.tab = Tab::Timeline;
            self.confirm = None;
            self.editing_draft = false;
            self.note = None;
            self.folded.clear();
            self.unfolded_lines.clear();
            self.expanded.clear();
            self.body_scroll = ScrollHandle::new();
        }
        if let Some(item) = self.item() {
            let known = self.github.read(cx).details(&item).is_some();
            if !known || !same {
                self.github
                    .update(cx, |g, cx| g.fetch_item(item.clone(), cx));
            }
            if item.kind == ItemKind::Pull && !self.merge_methods.contains_key(&item.repo) {
                self.load_merge_methods(item.repo.clone(), cx);
            }
        }
        self.switch_comment(cx);
        self.reload_draft();
        cx.notify();
    }

    /// Puts away the comment typed for the item shown before and brings
    /// back the one of the item shown now.
    fn switch_comment(&mut self, cx: &mut Context<Self>) {
        let now = self.item();
        if now == self.comment_for {
            return;
        }
        let typed = self.comment.read(cx).text().to_string();
        let next = swap_comment(
            &mut self.comment_drafts,
            self.comment_for.as_ref(),
            now.as_ref(),
            typed,
        );
        self.comment.update(cx, |i, cx| i.set_text(next, cx));
        self.comment_for = now;
    }

    fn load_merge_methods(&mut self, repo: String, cx: &mut Context<Self>) {
        self.merge_methods.insert(repo.clone(), None);
        cx.spawn(async move |this, cx| {
            let r = repo.clone();
            let read = cx
                .background_spawn(async move { pr::allowed_merge_methods(&r) })
                .await;
            let _ = this.update(cx, |this, cx| {
                // Unknown counts as squash allowed: GitHub refuses a wrong one with its own words.
                this.merge_methods.insert(
                    repo,
                    Some(read.unwrap_or_else(|_| vec![MergeMethod::Squash])),
                );
                cx.notify();
            });
        })
        .detach();
    }

    /// Reads the diff unless the one there is for the current head (or on its way).
    fn load_diff(&mut self, item: ItemRef, cx: &mut Context<Self>) {
        let key = crate::github::queue::key_of(&item);
        let tip = match self.github.read(cx).details(&item) {
            Some(Details::Pull(p)) => pr::Tip {
                head: p.head_ref_oid.clone(),
                base: p.base_ref_name.clone(),
            },
            _ => pr::Tip::default(),
        };
        if self
            .diffs
            .get(&key)
            .is_some_and(|d| diff_is_current(&d.tip, &tip.head, &tip.base))
        {
            return;
        }
        self.diff_loads += 1;
        let load = self.diff_loads;
        self.diffs.insert(
            key.clone(),
            Diff {
                tip: tip.clone(),
                load,
                state: DiffState::Loading,
            },
        );
        cx.spawn(async move |this, cx| {
            let it = item.clone();
            let read = cx
                .background_spawn(async move {
                    pr::fetch_diff_at(&it).map(|(d, at)| (pr::split_diff(&d.text), d.cut, at))
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                // A newer load (a push, a refresh) replaced this one.
                let Some(d) = this.diffs.get_mut(&key).filter(|d| d.load == load) else {
                    return;
                };
                let mut behind = false;
                match read {
                    Ok((files, cut, at)) => {
                        // Kept under what it was read at, so the files never
                        // show beside another head or base.
                        behind = !tip.head.is_empty() && at != tip;
                        d.tip = at;
                        d.state = DiffState::Ready {
                            files: Rc::new(files),
                            cut,
                        };
                    }
                    Err(p) => d.state = DiffState::Failed(p.text()),
                }
                // The details are older than the diff: read them again so
                // the two meet.
                if behind {
                    this.github.update(cx, |g, cx| g.fetch_item(item, cx));
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn set_tab(&mut self, tab: Tab, cx: &mut Context<Self>) {
        self.tab = tab;
        self.body_scroll = ScrollHandle::new();
        self.timeline_list.scroll_to(gpui::ListOffset::default());
        self.files_list.scroll_to(gpui::ListOffset::default());
        if tab == Tab::Files
            && let Some(item) = self.item()
        {
            self.load_diff(item, cx);
        }
        cx.notify();
    }

    /// The rows J and K walk: every group, the done ones only when shown.
    fn walkable(&self, cx: &App) -> Vec<Entry> {
        let q = self.github.read(cx).queue();
        q.rows
            .iter()
            .filter(|(_, p)| p.group != Group::Done || self.done_open)
            .map(|(e, _)| e.clone())
            .collect()
    }

    fn step(&mut self, by: isize, cx: &mut Context<Self>) {
        let rows = self.walkable(cx);
        if rows.is_empty() {
            return;
        }
        let at = self
            .picked
            .as_ref()
            .and_then(|p| rows.iter().position(|e| e.key() == p.key()));
        let next = match at {
            Some(i) => (i as isize + by).clamp(0, rows.len() as isize - 1) as usize,
            None => 0,
        };
        let open = opens_on_step(self.narrow(), self.narrow_open);
        self.select(rows[next].clone(), open, cx);
    }

    fn refresh(&mut self, cx: &mut Context<Self>) {
        self.github.update(cx, |g, cx| {
            g.refresh(cx);
            g.refresh_pulse(true, cx);
        });
        if let Some(item) = self.item() {
            self.diffs.remove(&crate::github::queue::key_of(&item));
            self.github
                .update(cx, |g, cx| g.fetch_item(item.clone(), cx));
            if self.tab == Tab::Files {
                self.load_diff(item, cx);
            }
        }
    }

    fn add_watch(&mut self, cx: &mut Context<Self>) {
        let text = self.watch_input.read(cx).text().to_string();
        if text.trim().is_empty() {
            return;
        }
        if crate::github::watch_entry(&text).is_none() {
            self.watch_note = Some("Type an organization (arakunhq) or a repository (owner/repo).");
        } else {
            // Already on the list counts as added: the box clears either way.
            self.github.update(cx, |g, cx| g.add_watch(&text, cx));
            self.watch_input.update(cx, |i, cx| i.set_text("", cx));
            self.watch_note = None;
        }
        cx.notify();
    }

    fn open_url(&mut self, url: String, cx: &mut Context<Self>) {
        cx.emit(GitHubViewEvent::Open(url));
    }

    /// The picked entry's page on GitHub, and its notification marked read.
    fn open_on_github(&mut self, cx: &mut Context<Self>) {
        let Some(e) = self.picked.clone() else { return };
        let url = match (&e.item, &e.notification) {
            (Some(item), _) => item.url(),
            (None, Some(n)) => n.web_url(),
            _ => return,
        };
        self.mark_read(cx);
        self.open_url(url, cx);
    }

    fn mark_read(&mut self, cx: &mut Context<Self>) {
        if let Some(n) = self
            .picked
            .as_ref()
            .and_then(|e| e.notification.as_ref())
            .filter(|n| n.unread)
        {
            let id = n.id.clone();
            self.github.update(cx, |g, cx| g.mark_read(&id, cx));
        }
    }

    fn title(&self, cx: &App) -> String {
        let Some(e) = &self.picked else {
            return String::new();
        };
        match e
            .item
            .as_ref()
            .and_then(|i| self.github.read(cx).details(i))
        {
            Some(Details::Pull(p)) => p.title.clone(),
            Some(Details::Issue(i)) => i.title.clone(),
            None => e.title.clone(),
        }
    }

    // -----------------------------------------------------------------------
    // Agents

    fn ask(&mut self, title: String, prompt: String, cx: &mut Context<Self>) {
        cx.emit(GitHubViewEvent::Ask { title, prompt });
    }

    fn review_with_agent(&mut self, cx: &mut Context<Self>) {
        let Some(item) = self.item().filter(|i| i.kind == ItemKind::Pull) else {
            return;
        };
        let title = self.title(cx);
        self.ask(
            format!("Review: {title}"),
            pr::review_prompt(&item, &title),
            cx,
        );
    }

    fn catch_up(&mut self, cx: &mut Context<Self>) {
        let Some(item) = self.item() else { return };
        let title = self.title(cx);
        let prompt = match self.github.read(cx).details(&item) {
            Some(Details::Pull(p)) => {
                let me = self.github.read(cx).me().unwrap_or_default().to_string();
                let since = pr::my_review(p, &me)
                    .and_then(|r| crate::ci::parse_time(&r.submitted_at))
                    .map(|t| {
                        format!(
                            "{} ago",
                            crate::github::pulse::span(crate::model::unix_now().saturating_sub(t))
                        )
                    })
                    .unwrap_or_default();
                pr::catch_up_prompt(&item, &title, &since, &pr::new_since_my_review(p, &me))
            }
            // An issue has no reviews: what it says so far is the catch-up.
            _ => pr::summarize_prompt(&item, &title),
        };
        self.ask(format!("Catch up: {title}"), prompt, cx);
    }

    fn summarize(&mut self, cx: &mut Context<Self>) {
        let Some(item) = self.item() else { return };
        let title = self.title(cx);
        self.ask(
            format!("Summary: {title}"),
            pr::summarize_prompt(&item, &title),
            cx,
        );
    }

    // -----------------------------------------------------------------------
    // Writes

    /// Runs one write in the background. After it: the item read again and
    /// its notification marked read, or the failure in the card.
    fn write(
        &mut self,
        what: &'static str,
        done: String,
        job: impl FnOnce(&ItemRef) -> Result<(), Problem> + Send + 'static,
        after: impl FnOnce(&mut Self, &mut Context<Self>) + 'static,
        cx: &mut Context<Self>,
    ) {
        let Some(item) = self.item() else { return };
        if self.busy.is_some() {
            return;
        }
        self.busy = Some(what);
        self.note = None;
        // The pick may change while gh works; the thread to mark read is this one's.
        let thread = self
            .picked
            .as_ref()
            .and_then(|e| e.notification.as_ref())
            .filter(|n| n.unread)
            .map(|n| n.id.clone());
        cx.notify();
        cx.spawn(async move |this, cx| {
            let it = item.clone();
            let result = cx.background_spawn(async move { job(&it) }).await;
            let _ = this.update(cx, |this, cx| {
                this.busy = None;
                // Another item may be picked now: its confirmation stays, and
                // the result says which item it is about.
                let here = this.item().as_ref() == Some(&item);
                match result {
                    Ok(()) => {
                        this.note = Some(Note::ok(about(here, &item, done)));
                        if let Some(id) = &thread {
                            this.github.update(cx, |g, cx| g.mark_read(id, cx));
                        }
                        // The diff stays: it changes only with the head,
                        // which this read brings and `load_diff` follows.
                        this.github
                            .update(cx, |g, cx| g.fetch_item(item.clone(), cx));
                        after(this, cx);
                    }
                    Err(p) if pr::refused_read_only(&p) => {
                        if here {
                            this.confirm = None;
                        }
                        this.note = Some(Note {
                            ok: false,
                            quiet: true,
                            text: about(
                                here,
                                &item,
                                "Read-only in the test Arakun: nothing was sent.".into(),
                            ),
                        });
                    }
                    Err(p) => {
                        this.note = Some(Note::failed(about(here, &item, p.text())));
                        // It may have failed because the pull request moved
                        // (a push before a merge): show what it is now.
                        this.github
                            .update(cx, |g, cx| g.fetch_item(item.clone(), cx));
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn send_comment(&mut self, cx: &mut Context<Self>) {
        // Text typed for another item is never sent to this one.
        if self.comment_for != self.item() {
            self.switch_comment(cx);
            return;
        }
        let text = self.comment.read(cx).text().to_string();
        if text.trim().is_empty() || self.item().is_none() {
            return;
        }
        if text.trim_start().starts_with('/') {
            self.submit_command(cx);
            return;
        }
        let body = text.clone();
        let on = self.item();
        self.write(
            "comment",
            "Comment posted.".into(),
            move |item| pr::comment(item, &body).map(|_| ()),
            // The input stays usable while gh works: keep anything typed since,
            // or meant for another item.
            move |this, cx| {
                if this.item() == on && this.comment.read(cx).text() == text {
                    this.comment.update(cx, |i, cx| i.set_text("", cx))
                } else if let Some(on) = &on {
                    // Put away when another item was picked: it was sent.
                    let key = crate::github::queue::key_of(on);
                    if this.comment_drafts.get(&key) == Some(&text) {
                        this.comment_drafts.remove(&key);
                    }
                }
            },
            cx,
        );
    }

    /// Enter in the command bar: a `/` action, or a reminder of how to send.
    fn submit_command(&mut self, cx: &mut Context<Self>) {
        let text = self.comment.read(cx).text().trim().to_string();
        let Some(cmd) = text.strip_prefix('/') else {
            if !text.is_empty() {
                self.note = Some(Note {
                    ok: false,
                    quiet: true,
                    text: crate::platform::shortcut_label("Ctrl+Enter sends the comment."),
                });
                cx.notify();
            }
            return;
        };
        let pull = self.item().is_some_and(|i| i.kind == ItemKind::Pull);
        let clear = |this: &mut Self, cx: &mut Context<Self>| {
            this.comment.update(cx, |i, cx| i.set_text("", cx))
        };
        match cmd.split_whitespace().next().unwrap_or_default() {
            "approve" if pull => self.ask_confirm(Confirm::Approve, cx),
            "request-changes" | "changes" if pull => self.ask_confirm(Confirm::RequestChanges, cx),
            "merge" if pull => self.ask_merge(cx),
            "close" if !pull => self.ask_confirm(Confirm::Close, cx),
            "review" if pull => self.review_with_agent(cx),
            "catchup" | "catch-up" => self.catch_up(cx),
            "summarize" | "summary" => self.summarize(cx),
            "open" => self.open_on_github(cx),
            _ => {
                let list = if pull {
                    "/approve, /request-changes, /merge, /review, /catchup, /summarize, /open"
                } else {
                    "/close, /catchup, /summarize, /open"
                };
                self.note = Some(Note {
                    ok: false,
                    quiet: true,
                    text: format!("Actions here: {list}"),
                });
                cx.notify();
                return;
            }
        }
        clear(self, cx);
    }

    fn ask_confirm(&mut self, c: Confirm, cx: &mut Context<Self>) {
        if self.item().is_none() {
            return;
        }
        self.confirm = Some(c);
        self.confirm_head = self.head(cx);
        self.confirm_draft = self.draft.clone();
        self.editing_draft = false;
        self.note = None;
        let placeholder = match c {
            Confirm::RequestChanges => "What should change (required)",
            _ => "Say why (optional)",
        };
        self.body_input = cx.new(|cx| TextInput::new(placeholder, cx));
        self.resubscribe_body(cx);
        cx.notify();
    }

    /// The open pull request's head commit as last read; empty when unknown.
    fn head(&self, cx: &App) -> String {
        match self
            .item()
            .and_then(|i| self.github.read(cx).details(&i).cloned())
        {
            Some(Details::Pull(p)) => p.head_ref_oid.clone(),
            _ => String::new(),
        }
    }

    fn ask_merge(&mut self, cx: &mut Context<Self>) {
        let method = self.merge_method();
        if let Some(m) = method {
            self.ask_confirm(Confirm::Merge(m), cx);
        }
    }

    /// The method the merge button uses: squash when allowed, else the first
    /// the repository allows. None when it allows none to this user.
    fn merge_method(&self) -> Option<MergeMethod> {
        let item = self.item()?;
        match self.merge_methods.get(&item.repo) {
            Some(Some(list)) if list.contains(&MergeMethod::Squash) => Some(MergeMethod::Squash),
            Some(Some(list)) => list.first().copied(),
            _ => Some(MergeMethod::Squash),
        }
    }

    /// A new body input needs its own subscription; the old one goes with it.
    fn resubscribe_body(&mut self, cx: &mut Context<Self>) {
        let sub = cx.subscribe(&self.body_input, |this, _, ev: &InputEvent, cx| match ev {
            InputEvent::Submit | InputEvent::SubmitAlt => {
                if this.editing_draft {
                    this.save_draft_edit(cx);
                } else if let Some(c) = this.confirm {
                    this.run_confirm(c, cx);
                }
            }
            InputEvent::Cancel => {
                this.confirm = None;
                this.editing_draft = false;
                cx.notify();
            }
            _ => {}
        });
        // The first three are fixed; the fourth is the body input's.
        self._subs.truncate(3);
        self._subs.push(sub);
    }

    fn run_confirm(&mut self, c: Confirm, cx: &mut Context<Self>) {
        let body = self.body_input.read(cx).text().to_string();
        let base = match self
            .item()
            .and_then(|i| self.github.read(cx).details(&i).cloned())
        {
            Some(Details::Pull(p)) => p.base_ref_name.clone(),
            _ => String::new(),
        };
        // The confirmation goes only if its item is still the one picked.
        let on = self.item();
        let clear = move |this: &mut Self, _: &mut Context<Self>| {
            if this.item() == on {
                this.confirm = None;
            }
        };
        match c {
            Confirm::Approve => {
                let head = self.confirm_head.clone();
                // Same as a merge: commits that arrived between the two
                // clicks weren't seen, so they aren't approved.
                if head != self.head(cx) {
                    self.confirm = None;
                    self.note = Some(Note::failed(
                        "New commits arrived after you asked to approve. Look at them, then approve again.",
                    ));
                    cx.notify();
                    return;
                }
                self.write(
                    "approve",
                    "Approved.".into(),
                    move |i| pr::review(i, Verdict::Approve, &body, &head),
                    clear,
                    cx,
                )
            }
            Confirm::RequestChanges => {
                if body.trim().is_empty() {
                    self.note = Some(Note::failed("Say what should change first."));
                    cx.notify();
                    return;
                }
                self.write(
                    "request-changes",
                    "Changes requested.".into(),
                    move |i| pr::review(i, Verdict::RequestChanges, &body, ""),
                    clear,
                    cx,
                )
            }
            Confirm::Merge(m) => {
                let head = self.confirm_head.clone();
                // A read between the two clicks brought new commits: the
                // user hasn't seen them, so this merge would fail anyway.
                if head != self.head(cx) {
                    self.confirm = None;
                    self.note = Some(Note::failed(
                        "New commits arrived after you asked to merge. Look at them, then merge again.",
                    ));
                    cx.notify();
                    return;
                }
                let done = format!("{} into {base}.", merged_label(m));
                self.write("merge", done, move |i| pr::merge(i, m, &head), clear, cx)
            }
            Confirm::Close => self.write("close", "Closed.".into(), pr::close, clear, cx),
            Confirm::PostDraft => self.post_draft(cx),
        }
    }

    /// How the draft would go out now; None without a draft or a pull request.
    fn draft_posts_as(&self, cx: &App) -> Option<Verdict> {
        let draft = self.draft.as_ref()?;
        match self.github.read(cx).details(&draft.item())? {
            Details::Pull(p) => Some(pr::draft_posts_as(draft, p)),
            Details::Issue(_) => None,
        }
    }

    /// The second click on "Post as …".
    fn post_draft(&mut self, cx: &mut Context<Self>) {
        let Some(draft) = self.draft.clone() else {
            return;
        };
        if self.confirm_draft.as_ref() != Some(&draft) {
            self.confirm = None;
            self.note = Some(Note::failed(
                "The draft changed after you asked to post it. Read it again first.",
            ));
            cx.notify();
            return;
        }
        let Some(verdict) = self.draft_posts_as(cx) else {
            return;
        };
        let done = match verdict {
            Verdict::Approve => "Approval posted.",
            Verdict::RequestChanges => "Change request posted.",
            Verdict::Comment => "Review posted.",
        };
        let posted = draft.clone();
        let on = self.item();
        self.write(
            "post-draft",
            done.into(),
            move |i| pr::post_draft(i, &posted, verdict),
            move |this, _| {
                if this.item() == on {
                    this.confirm = None;
                }
                // An agent (or an edit) may have saved a new draft while this
                // one was sent: that one wasn't posted, so it stays.
                let _ = pr::delete_draft_if(&draft);
                if this.draft.as_ref() == Some(&draft) {
                    this.draft = None;
                }
            },
            cx,
        );
    }

    fn edit_draft(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(draft) = &self.draft else { return };
        let text = draft.body.trim_end().replace('\n', NEWLINE);
        self.body_input = cx.new(|cx| TextInput::new("The review", cx));
        self.body_input.update(cx, |i, cx| i.set_text(text, cx));
        self.resubscribe_body(cx);
        self.editing_draft = true;
        self.confirm = None;
        window.focus(&self.body_input.focus_handle(cx), cx);
        cx.notify();
    }

    fn save_draft_edit(&mut self, cx: &mut Context<Self>) {
        let body = self.body_input.read(cx).text().replace(NEWLINE, "\n");
        if let Some(d) = &mut self.draft {
            d.body = body;
            if let Err(e) = pr::save_draft(d) {
                // Stay in the editor: the next read would replace the text.
                self.note = Some(Note::failed(format!("Could not save the draft: {e}")));
                cx.notify();
                return;
            }
        }
        self.editing_draft = false;
        cx.notify();
    }

    fn discard_draft(&mut self, cx: &mut Context<Self>) {
        if self.confirm == Some(Confirm::PostDraft) {
            self.confirm = None;
        }
        if let Some(d) = self.draft.take()
            && let Err(e) = pr::delete_draft(&d.item())
        {
            self.note = Some(Note::failed(format!("Could not delete the draft: {e}")));
        }
        self.editing_draft = false;
        cx.notify();
    }

    // -----------------------------------------------------------------------
    // Keys

    fn on_key(&mut self, ev: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let k = &ev.keystroke;
        // Keys typed in an input are text, not shortcuts.
        if k.modifiers.control
            || k.modifiers.alt
            || k.modifiers.platform
            || !self.focus.is_focused(window)
        {
            return;
        }
        let pull = self.item().is_some_and(|i| i.kind == ItemKind::Pull);
        let mine = self.is_mine(cx);
        match k.key.as_str() {
            "j" | "down" => self.step(1, cx),
            "k" | "up" => self.step(-1, cx),
            "a" if pull && !mine => self.ask_confirm(Confirm::Approve, cx),
            "c" if self.item().is_some() => window.focus(&self.comment.focus_handle(cx), cx),
            "r" if pull => self.review_with_agent(cx),
            "u" if self.item().is_some() => self.catch_up(cx),
            "s" if self.item().is_some() => self.summarize(cx),
            "enter" if self.picked.is_some() && self.narrow() && !self.narrow_open => {
                self.narrow_open = true;
                cx.notify();
            }
            "o" if self.picked.is_some() => self.open_on_github(cx),
            "escape"
                if self.confirm.is_some()
                    || self.editing_draft
                    || self.menu_at.is_some()
                    || self.focus_at.is_some() =>
            {
                self.confirm = None;
                self.editing_draft = false;
                self.menu_at = None;
                self.focus_at = None;
                cx.notify();
            }
            "escape" if self.narrow() && self.narrow_open => {
                self.narrow_open = false;
                cx.notify();
            }
            _ => return,
        }
        cx.stop_propagation();
    }

    /// Whether the open pull request is the user's own.
    fn is_mine(&self, cx: &App) -> bool {
        let g = self.github.read(cx);
        let me = g.me().unwrap_or_default();
        match self.item().and_then(|i| g.details(&i)) {
            Some(Details::Pull(p)) => !me.is_empty() && p.author.login.eq_ignore_ascii_case(me),
            _ => self
                .picked
                .as_ref()
                .is_some_and(|e| !me.is_empty() && e.author.eq_ignore_ascii_case(me)),
        }
    }

    /// Markdown parsed once per text.
    fn markdown(
        &self,
        id: impl Into<gpui::ElementId>,
        text: &str,
        st: MdStyle,
    ) -> gpui::AnyElement {
        let blocks = parsed(&self.md, text, |t| markdown::parse(&without_html(t)));
        render_markdown(id, &blocks, st)
    }

    /// A draft as it will be posted: its HTML, comments too, stays as text,
    /// so nothing in it is hidden from the one who posts it.
    fn markdown_as_written(
        &self,
        id: impl Into<gpui::ElementId>,
        text: &str,
        st: MdStyle,
    ) -> gpui::AnyElement {
        let blocks = parsed(&self.md_raw, text, markdown::parse);
        render_markdown(id, &blocks, st)
    }

    /// Only the start of a long body, for its fold: what the fold hides
    /// would still be laid out on every frame the body shows.
    fn markdown_start(
        &self,
        id: impl Into<gpui::ElementId>,
        text: &str,
        st: MdStyle,
    ) -> gpui::AnyElement {
        let blocks = parsed(&self.md_start, text, |t| {
            markdown::parse(timeline::fold_start(&without_html(t)))
        });
        render_markdown(id, &blocks, st)
    }
}

/// `text` as markdown, parsed by `parse` on its first use.
fn parsed(
    cache: &RefCell<HashMap<String, Rc<Vec<Block>>>>,
    text: &str,
    parse: impl FnOnce(&str) -> Vec<Block>,
) -> Rc<Vec<Block>> {
    let mut md = cache.borrow_mut();
    if let Some(blocks) = md.get(text) {
        return blocks.clone();
    }
    // Bodies of items looked at long ago: parsing again is cheap next to keeping them all.
    if md.len() > 400 {
        md.clear();
    }
    let blocks = Rc::new(parse(text));
    md.insert(text.to_string(), blocks.clone());
    blocks
}

fn render_markdown(
    id: impl Into<gpui::ElementId>,
    blocks: &[Block],
    st: MdStyle,
) -> gpui::AnyElement {
    markdown::render(
        id,
        blocks,
        st,
        &crate::agent::TextMap::default(),
        None,
        &[],
        None,
        None,
        None,
    )
}

/// GitHub bodies with the HTML the markdown renderer would show as text:
/// comments dropped, `<details>` opened, a `<summary>` as a bold line.
/// Code blocks and code spans are left alone: `<!--` there is text to show.
fn without_html(text: &str) -> String {
    // Each line out, and whether it is inside a fenced code block.
    let mut lines: Vec<(String, bool)> = Vec::new();
    let mut fence: Option<(char, usize)> = None;
    let mut in_comment = false;
    for line in text.lines() {
        if let Some((ch, len)) = fence {
            let t = line.trim_start();
            if t.len() - t.trim_start_matches(ch).len() >= len
                && t.trim_start_matches(ch).trim().is_empty()
            {
                fence = None;
            }
            lines.push((line.to_string(), true));
            continue;
        }
        if !in_comment && let Some(open) = fence_open(line) {
            fence = Some(open);
            lines.push((line.to_string(), true));
            continue;
        }
        // A comment that started on an earlier line joins what follows it
        // to that line, as if the comment had never been there.
        let joins = in_comment;
        let kept = strip_comments(line, &mut in_comment);
        match lines.last_mut() {
            Some((last, false)) if joins => last.push_str(&kept),
            _ if joins && kept.is_empty() => {}
            _ => lines.push((kept, false)),
        }
    }
    lines
        .into_iter()
        .filter_map(|(line, code)| {
            if code {
                return Some(line);
            }
            let t = line.trim();
            let lower = t.to_ascii_lowercase();
            if matches!(
                lower.as_str(),
                "<details>" | "</details>" | "<details open>" | "<br>" | "<br/>" | "<p>" | "</p>"
            ) {
                return None;
            }
            if let Some(inner) = lower
                .strip_prefix("<summary>")
                .and_then(|s| s.strip_suffix("</summary>"))
            {
                let inner = &t[9..9 + inner.len()];
                return Some(format!("**{}**", inner.trim()));
            }
            Some(line)
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// The fence character and length if `line` opens a fenced code block.
fn fence_open(line: &str) -> Option<(char, usize)> {
    let t = line.trim_start();
    if line.len() - t.len() > 3 {
        return None;
    }
    let ch = t.chars().next().filter(|c| matches!(c, '`' | '~'))?;
    let len = t.len() - t.trim_start_matches(ch).len();
    // A backtick fence's info string can't hold a backtick.
    (len >= 3 && !(ch == '`' && t[len..].contains('`'))).then_some((ch, len))
}

/// One line without its HTML comments, keeping code spans as they are.
/// `in_comment` carries a comment that is still open over to the next line.
fn strip_comments(line: &str, in_comment: &mut bool) -> String {
    let mut out = String::with_capacity(line.len());
    let mut rest = line;
    loop {
        if *in_comment {
            match rest.find("-->") {
                Some(end) => {
                    rest = &rest[end + 3..];
                    *in_comment = false;
                }
                None => return out,
            }
        }
        let next = rest.find(['<', '`']);
        let Some(at) = next else {
            out.push_str(rest);
            return out;
        };
        out.push_str(&rest[..at]);
        rest = &rest[at..];
        if rest.starts_with("<!--") {
            rest = &rest[4..];
            *in_comment = true;
        } else if rest.starts_with('`') {
            let ticks = rest.len() - rest.trim_start_matches('`').len();
            let run = &rest[..ticks];
            // The span ends at the next run of exactly as many backticks.
            let after = &rest[ticks..];
            let mut from = 0;
            let close = loop {
                match after[from..].find(run) {
                    Some(i) => {
                        let i = from + i;
                        let len = after[i..].len() - after[i..].trim_start_matches('`').len();
                        if len == ticks {
                            break Some(i);
                        }
                        from = i + len;
                    }
                    None => break None,
                }
            };
            let end = close.map_or(ticks, |i| ticks + i + ticks);
            out.push_str(&rest[..end]);
            rest = &rest[end..];
        } else {
            out.push('<');
            rest = &rest[1..];
        }
    }
}

/// Whether J/K (↓/↑) open the item they move to. The narrow layout shows
/// the list or one item: in the list they only move the pick, and Enter
/// opens it; in an item they go to the next one, still open.
fn opens_on_step(narrow: bool, narrow_open: bool) -> bool {
    !narrow || narrow_open
}

/// The result of a write, naming its item when another one is picked now.
fn about(here: bool, item: &ItemRef, text: String) -> String {
    if here {
        text
    } else {
        format!("{}: {text}", item.label())
    }
}

/// The style of bodies in the timeline: reading text, a little smaller than a reply.
fn body_style() -> MdStyle {
    MdStyle {
        size: ui::TEXT_LG,
        line_height: 22.,
        color: theme::text_muted(),
        strong: theme::text(),
        heading: theme::text(),
        gap: 8.,
    }
}

fn merged_label(m: MergeMethod) -> &'static str {
    match m {
        MergeMethod::Squash => "Squashed and merged",
        MergeMethod::Merge => "Merged",
        MergeMethod::Rebase => "Rebased and merged",
    }
}

/// The question and the button of the confirmation before a draft goes out.
/// An old draft says it is old, and an old approval only goes out as a comment.
fn draft_question(draft: &Draft, stale: bool, posts_as: Verdict) -> (String, String) {
    let who = if draft.author.is_empty() {
        "the agent"
    } else {
        draft.author.as_str()
    };
    let sha = pr::short_sha(&draft.head_sha);
    let word = timeline::verdict_word(posts_as);
    if draft.verdict == Verdict::Approve && posts_as != Verdict::Approve {
        let when = if sha.is_empty() {
            "doesn't say which commit it approves".to_string()
        } else {
            format!("approves {sha}, and new commits came since")
        };
        return (
            format!("{who}'s draft {when}. Post it as a comment instead?"),
            "Post as comment".into(),
        );
    }
    if stale {
        return (
            format!(
                "{who}'s draft is for {sha}, and new commits came since. Post it as a {word} anyway?"
            ),
            "Post anyway".into(),
        );
    }
    let what = match posts_as {
        Verdict::Approve => format!("Approve {sha} with {who}'s draft?"),
        _ => format!("Post {who}'s draft as a {word}?"),
    };
    (what, format!("Post as {word}"))
}

fn merge_label(m: MergeMethod) -> &'static str {
    match m {
        MergeMethod::Squash => "Squash and merge",
        MergeMethod::Merge => "Merge",
        MergeMethod::Rebase => "Rebase and merge",
    }
}

/// A short time since `t` (unix seconds): `1h 10m`.
fn ago(t: u64) -> String {
    if t == 0 {
        return String::new();
    }
    crate::github::pulse::span(crate::model::unix_now().saturating_sub(t))
}

fn ago_iso(at: &str) -> String {
    ago(crate::ci::parse_time(at).unwrap_or(0))
}

/// A line in mono at a role color.
fn mono(content: impl Into<gpui::SharedString>, color: gpui::Hsla) -> gpui::Div {
    div().ty(Type::Mono).text_color(color).child(content.into())
}

impl Focusable for GitHubView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for GitHubView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // The width this view got last frame decides the layout of this one.
        let (cell, id) = (self.size.clone(), cx.entity_id());
        // The panes lay themselves out from the size too.
        let panes = [&self.queue_pane, &self.card_pane, &self.body_pane].map(|p| p.entity_id());
        let measure = canvas(
            |_, _, _| {},
            move |bounds: Bounds<Pixels>, _, _: &mut Window, cx: &mut App| {
                let now = (bounds.size.width.as_f32(), bounds.size.height.as_f32());
                let was = cell.get();
                if (now.0 - was.0).abs() > 0.5 || (now.1 - was.1).abs() > 0.5 {
                    cell.set(now);
                    cx.notify(id);
                    for pane in panes {
                        cx.notify(pane);
                    }
                }
            },
        )
        .absolute()
        .size_full();

        let lay = self.layout();
        let narrow = lay.narrow;
        let root = div()
            .id("github-view")
            .track_focus(&self.focus)
            .on_key_down(cx.listener(Self::on_key))
            .relative()
            .flex()
            .size_full()
            .gap(crate::ui::sp(GAP))
            .p(crate::ui::sp(GAP))
            .overflow_hidden()
            .bg(theme::background())
            .font_family(theme::FONT_SANS)
            .ty(Type::Body)
            .text_color(theme::text())
            .child(measure);
        if crate::github::GitHub::disabled() {
            return root.child(
                ui::empty_state(
                    "github",
                    "GitHub is turned off",
                    "ARAKUN_GITHUB=off is set.",
                    None,
                )
                .flex_1()
                .justify_center(),
            );
        }
        let left = div()
            .flex()
            .flex_col()
            .flex_none()
            .min_h_0()
            .gap(crate::ui::sp(GAP))
            .when(narrow, |d| d.flex_1().w_full())
            .when(!narrow, |d| d.w(px(lay.left_w)))
            .child(Pane::cached(
                &self.queue_pane,
                StyleRefinement::default().flex_1().min_h_0(),
            ))
            .child(if lay.pulse_full {
                self.render_pulse(cx)
            } else {
                self.render_pulse_strip(cx)
            });
        let card = || {
            Pane::cached(
                &self.card_pane,
                StyleRefinement::default().flex_1().min_w_0(),
            )
        };
        let root = if narrow {
            if self.narrow_open && self.picked.is_some() {
                root.child(card())
            } else {
                root.child(left)
            }
        } else {
            root.child(left).child(card())
        };
        let root = root.when_some(self.focus_at, |d, at| {
            d.child(
                div()
                    .id("gh-focus-backdrop")
                    .absolute()
                    .top_0()
                    .left_0()
                    .size_full()
                    .occlude()
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, _, _, cx| {
                            this.focus_at = None;
                            cx.notify();
                        }),
                    ),
            )
            .child(gpui::deferred(
                gpui::anchored()
                    .position(at)
                    .snap_to_window_with_margin(px(8.))
                    .child(self.render_focus_menu(cx)),
            ))
        });
        root.when_some(self.menu_at, |d, at| {
            d.child(
                div()
                    .id("gh-menu-backdrop")
                    .absolute()
                    .top_0()
                    .left_0()
                    .size_full()
                    .occlude()
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, _, _, cx| {
                            this.menu_at = None;
                            this.confirm_all = None;
                            cx.notify();
                        }),
                    ),
            )
            .child(gpui::deferred(
                gpui::anchored()
                    .position(at)
                    .snap_to_window_with_margin(px(8.))
                    .child(self.render_menu(cx)),
            ))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_diff_follows_the_head() {
        let at = |head: &str| pr::Tip {
            head: head.into(),
            base: "main".into(),
        };
        assert!(diff_is_current(&at("aaa"), "aaa", "main"));
        // Retargeted with no new commit: the files compare to another base.
        assert!(!diff_is_current(&at("aaa"), "aaa", "release"));
        // A push moved the head: the old files must not show.
        assert!(!diff_is_current(&at("aaa"), "bbb", "main"));
        assert!(!diff_is_current(&at(""), "bbb", "main"));
        // Details not read yet: keep what there is.
        assert!(diff_is_current(&at("aaa"), "", ""));
    }

    #[test]
    fn an_old_draft_says_so_before_it_goes_out() {
        let draft = Draft {
            repo: "o/r".into(),
            number: 7,
            verdict: Verdict::Approve,
            body: "LGTM".into(),
            author: "Claude".into(),
            head_sha: "0123456789abcdef".into(),
            created: 1,
        };
        let (q, verb) = draft_question(&draft, false, Verdict::Approve);
        assert_eq!(q, "Approve 0123456 with Claude's draft?");
        assert_eq!(verb, "Post as approval");
        let (q, verb) = draft_question(&draft, true, Verdict::Comment);
        assert!(
            q.contains("approves 0123456, and new commits came since"),
            "{q}"
        );
        assert_eq!(verb, "Post as comment");
        let changes = Draft {
            verdict: Verdict::RequestChanges,
            ..draft.clone()
        };
        let (q, verb) = draft_question(&changes, true, Verdict::RequestChanges);
        assert!(
            q.contains("is for 0123456") && q.ends_with("anyway?"),
            "{q}"
        );
        assert_eq!(verb, "Post anyway");
        let (q, _) = draft_question(&changes, false, Verdict::RequestChanges);
        assert_eq!(q, "Post Claude's draft as a change request?");
    }

    #[test]
    fn the_layout_follows_the_width() {
        // Both sidebars open on a 1920 window: wide, properties above the tabs.
        let l = layout(1400., 1000.);
        assert!(!l.narrow && l.pulse_full && !l.side_props);
        assert!((l.left_w - 420.).abs() < 0.5);
        // Full width: room for the properties column.
        let l = layout(1900., 1000.);
        assert!(l.side_props && l.left_w == 440.);
        // Medium: a narrower queue, the pulse folded.
        let l = layout(1100., 1000.);
        assert!(!l.narrow && !l.pulse_full && !l.side_props);
        assert!(l.left_w >= 340. && l.left_w <= 400.);
        // Wide but short: the pulse folds too.
        assert!(!layout(1500., 700.).pulse_full);
        // Narrow: one at a time.
        assert!(layout(800., 900.).narrow);
        assert!(!layout(900., 900.).narrow);
    }

    #[test]
    fn html_in_bodies_reads_as_markdown() {
        let body = "Intro <!-- hidden -->text\n<details>\n<summary>Why it matters</summary>\n\nInside.\n</details>";
        assert_eq!(
            without_html(body),
            "Intro text\n**Why it matters**\n\nInside."
        );
        assert_eq!(without_html("plain"), "plain");
        assert_eq!(without_html("a <!-- never closed"), "a ");
        assert_eq!(without_html("a <!-- two\nlines -->b\nc"), "a b\nc");
    }

    #[test]
    fn html_comments_in_code_stay() {
        let fenced = "```html\n<!-- kept -->\n<details>\n```\nafter <!-- gone -->";
        assert_eq!(
            without_html(fenced),
            "```html\n<!-- kept -->\n<details>\n```\nafter "
        );
        let tilde = "~~~~\n<!-- kept\n~~~\n~~~~\nx";
        assert_eq!(without_html(tilde), tilde);
        assert_eq!(
            without_html("Write `<!-- x -->` <!-- gone -->here"),
            "Write `<!-- x -->` here"
        );
        assert_eq!(without_html("``a ` <!-- b``<!-- c -->d"), "``a ` <!-- b``d");
        // A lone backtick is text, and a comment after it still goes.
        assert_eq!(without_html("it` <!-- c -->x"), "it` x");
    }

    #[test]
    fn keys_in_the_narrow_list_move_without_opening() {
        assert!(!opens_on_step(true, false));
        assert!(opens_on_step(true, true));
        // Wide: the card is always there; it stays open if the window narrows.
        assert!(opens_on_step(false, false));
    }

    #[test]
    fn a_result_for_another_item_names_it() {
        let item = ItemRef {
            repo: "o/r".into(),
            number: 7,
            kind: ItemKind::Pull,
        };
        assert_eq!(about(true, &item, "Approved.".into()), "Approved.");
        assert_eq!(about(false, &item, "Approved.".into()), "o/r#7: Approved.");
    }

    #[test]
    fn a_comment_stays_with_its_item() {
        let item = |number| ItemRef {
            repo: "o/r".into(),
            number,
            kind: ItemKind::Pull,
        };
        let (a, b) = (item(1), item(2));
        let mut drafts = HashMap::new();
        // Written for A, then B is picked: B starts empty.
        assert_eq!(
            swap_comment(&mut drafts, Some(&a), Some(&b), "for A".into()),
            ""
        );
        // Back to A: its text comes back, and B's empty one isn't kept.
        assert_eq!(
            swap_comment(&mut drafts, Some(&b), Some(&a), " ".into()),
            "for A"
        );
        assert!(drafts.is_empty());
        // Nothing picked any more: A's text is kept for later.
        assert_eq!(swap_comment(&mut drafts, Some(&a), None, "x".into()), "");
        assert_eq!(drafts.len(), 1);
    }
}
