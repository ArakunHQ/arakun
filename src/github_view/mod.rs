//! The GitHub tab, the Bandeja: one line that says whether anything needs
//! the user, then every pull request pilot follows, grouped by where it
//! stands (from pilot's `GET /board`), with what was merged today folded at
//! the bottom. Without a board it shows the review queue (the inbox and
//! searches, so what was read on github.com still shows) the same way.
//!
//! A row opens the sheet on the right: where the pull request stands, who has
//! it, pilot's question with its own answers, and the tabs (what pilot did,
//! the conversation, checks, files) over the composer. Writing to GitHub
//! always takes a second step, and an agent's review waits as a draft until
//! the user posts it. The island opens the same sheet over any window.

mod bandeja;
mod diff;
mod item;
mod pilot;
mod queue;
mod sheet;
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
use crate::github::pilot::Pilot;
use crate::github::pr::{self, Draft, FileDiff, ItemKind, ItemRef, MergeMethod, Verdict};
use crate::github::queue::{Details, Entry};
use crate::github::{GitHub, Problem};
use crate::input::{InputEvent, TextInput};
use crate::theme;
use crate::ui::{self, Type, Typography as _};

/// Under this width the sheet covers the whole tab.
const NARROW: f32 = 900.;
/// The gap the sheet keeps from the window's edges.
const GAP: f32 = 12.;

/// How the tab lays itself out at a size.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Layout {
    /// The sheet covers the list instead of sitting beside it.
    narrow: bool,
    /// The sheet's width.
    sheet_w: f32,
}

fn layout(w: f32) -> Layout {
    if w < NARROW {
        // The gutters `render` draws follow the Spacing setting.
        let gap = f32::from(crate::ui::sp(GAP));
        return Layout {
            narrow: true,
            sheet_w: (w - 2. * gap).max(0.),
        };
    }
    Layout {
        narrow: false,
        sheet_w: (w * 0.42).clamp(560., 680.),
    }
}

pub enum GitHubViewEvent {
    /// Show a GitHub page.
    Open(String),
    /// Start an agent session.
    Ask { title: String, prompt: String },
    /// The sheet shown over another window closed.
    Closed,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Tab {
    /// "Pilot activity": the board's timeline.
    Pilot,
    Timeline,
    Files,
    Checks,
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

/// What Enter or Ctrl+Enter in the comment input does with its text.
#[derive(Debug, PartialEq, Eq)]
enum CommentSend {
    /// Post it as a comment on the item shown.
    Post,
    /// A `/` action: run it.
    Command,
    /// Nothing to send: no item, or no text.
    NoItem,
    Empty,
    /// Another write is running; the text stays in the input.
    Busy(&'static str),
    /// This very text is the comment being posted.
    Sending,
    /// The text was typed for this other item: it stays that one's draft.
    Elsewhere(ItemRef),
}

/// Decides what sending `text`, typed for `typed_for`, does while `shown`
/// is the item on screen and `busy` names a write still running.
/// `posting` is the last comment sent and its item: while a comment write
/// runs, the text left in the input is often that one, not a new comment.
/// `typed_for` may lag behind `shown` (a queue refresh swaps the entry for
/// a fresh one): the same pull request or issue by `queue::key_of` counts
/// as the one shown, and so does no item at all, since nothing else would
/// keep that text.
fn comment_send(
    text: &str,
    shown: Option<&ItemRef>,
    typed_for: Option<&ItemRef>,
    busy: Option<&'static str>,
    posting: Option<(&ItemRef, &str)>,
) -> CommentSend {
    let Some(shown) = shown else {
        return CommentSend::NoItem;
    };
    let text = text.trim();
    if text.is_empty() {
        return CommentSend::Empty;
    }
    if let Some(other) = typed_for
        && crate::github::queue::key_of(other) != crate::github::queue::key_of(shown)
    {
        return CommentSend::Elsewhere(other.clone());
    }
    if text.starts_with('/') {
        return CommentSend::Command;
    }
    match busy {
        Some("comment")
            if posting.is_some_and(|(on, sent)| {
                same_item(Some(on), Some(shown)) && sent.trim() == text
            }) =>
        {
            CommentSend::Sending
        }
        Some(what) => CommentSend::Busy(what),
        None => CommentSend::Post,
    }
}

/// Whether two references name the same item. A queue refresh may read it
/// again under another repo case or kind.
fn same_item(a: Option<&ItemRef>, b: Option<&ItemRef>) -> bool {
    a.map(crate::github::queue::key_of) == b.map(crate::github::queue::key_of)
}

/// The note for a comment held back while the write `what` runs.
fn busy_note(what: &str) -> String {
    let what = match what {
        "comment" => "the last comment",
        "post-draft" => "the draft review",
        "request-changes" => "the change request",
        "approve" => "the approval",
        "merge" => "the merge",
        "close" => "the close",
        other => other,
    };
    format!("Still sending {what}: this comment wasn't sent yet. Press Enter again once it's done.")
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

    /// Nothing went, and why: not a failure to fix.
    fn quiet(text: impl Into<String>) -> Self {
        Note {
            ok: false,
            quiet: true,
            text: text.into(),
        }
    }
}

pub struct GitHubView {
    github: Entity<GitHub>,
    /// The entry picked in the queue, kept while the queue drops it (it was
    /// read, or merged).
    picked: Option<Entry>,
    /// The sheet shows the picked item.
    sheet_open: bool,
    /// The sheet shows over another window (opened from the island): the
    /// tab's list isn't drawn, and closing it hands the window back.
    floating: bool,
    tab: Tab,
    /// "Done today" is unfolded.
    done_open: bool,
    /// "More on GitHub" (the queue's rows pilot doesn't follow) is unfolded.
    more_open: bool,
    /// Folded groups the user asked to show past their first rows.
    fold_full: HashSet<&'static str>,
    /// Decisions answered from the sheet, until the board is done with them.
    answers: sheet::Answers,
    /// A task for pilot on its way from the composer.
    task_sending: bool,
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
    /// The comment last sent and its item, to tell a second Enter on it
    /// from a new comment. Read only while the "comment" write runs.
    posting: Option<(ItemRef, String)>,
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
    pilot: Entity<Pilot>,
    /// Cards whose points are open, by item id.
    pilot_open: HashSet<String>,
    /// The card picked, or shown from the island.
    pilot_focus: Option<String>,
    /// The confirm strip came from this card.
    pilot_act: Option<pilot::PilotAct>,
    /// Why a card's action didn't go, by item id.
    pilot_notes: HashMap<String, String>,
    /// The card whose "Other answer…" line is open.
    pilot_other: Option<String>,
    pilot_input: Entity<TextInput>,
    /// Cards whose answer is on its way.
    pilot_sending: HashSet<String>,
    /// "Sent…" after an answer went, until its timer clears it.
    pilot_sent: Option<(String, gpui::Task<()>)>,
    _pilot_sub: Subscription,
    _subs: Vec<Subscription>,
    /// Apart from `_subs`: it is replaced with each new body input.
    _body_sub: Option<Subscription>,
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
    pub fn new(github: Entity<GitHub>, pilot: Entity<Pilot>, cx: &mut Context<Self>) -> Self {
        let watch_input = cx.new(|cx| TextInput::new("organization or owner/repo", cx));
        let comment = cx.new(|cx| TextInput::new("Comment (Enter sends), or / for actions", cx));
        let body_input = cx.new(|cx| TextInput::new("Say why (optional)", cx));
        let pilot_input = cx.new(|cx| TextInput::new("Your answer for the agent", cx));
        let subs = vec![
            cx.subscribe(&pilot_input, |this, _, ev: &InputEvent, cx| match ev {
                InputEvent::Submit => this.pilot_send_other(cx),
                InputEvent::Cancel => {
                    this.pilot_other = None;
                    cx.notify();
                }
                _ => {}
            }),
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
                InputEvent::Submit | InputEvent::SubmitAlt => this.send_comment(cx),
                // Esc clears the box; on an empty one it closes the sheet.
                InputEvent::Cancel => {
                    if this.comment.read(cx).text().is_empty() {
                        this.close_sheet(cx);
                    } else {
                        this.comment.update(cx, |i, cx| i.set_text("", cx));
                    }
                }
                _ => {}
            }),
        ];
        let me = cx.entity();
        let queue_pane = Pane::new(&me, |this, cx| this.render_bandeja(cx), cx);
        let card_pane = Pane::new(&me, |this, cx| this.render_sheet(cx), cx);
        let body_pane = Pane::new(&me, |this, cx| this.render_body(cx), cx);
        let pilot_sub = cx.observe(&pilot, |this, pilot, cx| {
            let (people, shown, open) = {
                let p = pilot.read(cx);
                (p.people(), p.shown_ids(), p.open_decisions())
            };
            this.pilot_open.retain(|id| shown.contains(id));
            // The board's questions aren't always shown cards (an answer
            // hides its card): a failed answer's note stays while asked.
            this.pilot_notes
                .retain(|id, _| shown.contains(id) || open.contains(id));
            if let Some(b) = pilot.read(cx).board() {
                this.answers.settle(b);
            }
            this.settle_tab(cx);
            // The sheet answers the board's question with the same line.
            let asked = this
                .board_pr(cx)
                .and_then(|b| b.open_decision().map(|d| d.item.clone()));
            if this
                .pilot_other
                .as_ref()
                .is_some_and(|id| !shown.contains(id) && asked.as_ref() != Some(id))
            {
                this.pilot_other = None;
            }
            this.github.update(cx, |g, cx| g.want_people(people, cx));
            cx.notify();
        });
        let mut this = Self {
            github,
            picked: None,
            sheet_open: false,
            floating: false,
            tab: Tab::Timeline,
            done_open: false,
            more_open: false,
            fold_full: HashSet::new(),
            answers: sheet::Answers::default(),
            task_sending: false,
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
            posting: None,
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
            pilot,
            pilot_open: HashSet::new(),
            pilot_focus: None,
            pilot_act: None,
            pilot_notes: HashMap::new(),
            pilot_other: None,
            pilot_input,
            pilot_sending: HashSet::new(),
            pilot_sent: None,
            _pilot_sub: pilot_sub,
            _subs: subs,
            _body_sub: None,
        };
        this.resubscribe_body(cx);
        this
    }

    pub fn focus(&self, window: &mut Window, cx: &mut App) {
        window.focus(&self.focus, cx);
    }

    fn layout(&self) -> Layout {
        layout(self.size.get().0)
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
        // An item pilot follows stays: the board shows it whatever the focus.
        let on_board = self.board_pr(cx).is_some();
        if gone && !on_board && !self.sheet_open {
            self.picked = None;
            self.switch_comment(cx);
        }
        self.on_github_changed(cx);
    }

    /// Keeps the picked entry fresh and reads the draft again: an agent may
    /// have left one. Nothing opens on its own: the sheet waits for a click.
    fn on_github_changed(&mut self, cx: &mut Context<Self>) {
        let fresh = self.picked.as_ref().and_then(|p| {
            self.github
                .read(cx)
                .queue()
                .rows
                .iter()
                .find(|(e, _)| e.key() == p.key())
                .map(|(e, _)| e.clone())
        });
        if let Some(e) = fresh {
            self.picked = Some(e);
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

    /// Shows a pull request or issue in the sheet, as the palette opens one.
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

    /// Picks an entry; `open` shows it in the sheet (a click, not J/K on
    /// the list).
    fn select(&mut self, entry: Entry, open: bool, cx: &mut Context<Self>) {
        let same = self.picked.as_ref().is_some_and(|p| p.key() == entry.key());
        self.picked = Some(entry);
        if open {
            self.sheet_open = true;
        }
        if !same {
            // What pilot did comes first when pilot follows it.
            self.tab = if self.board_pr(cx).is_some() {
                Tab::Pilot
            } else {
                Tab::Timeline
            };
            self.confirm = None;
            self.pilot_act = None;
            self.editing_draft = false;
            self.note = None;
            self.folded.clear();
            self.unfolded_lines.clear();
            self.expanded.clear();
            self.body_scroll = ScrollHandle::new();
        } else {
            self.settle_tab(cx);
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

    /// "Pilot activity" is listed only while the board has the pull request:
    /// once it goes (a 404, other settings), the conversation shows instead.
    fn settle_tab(&mut self, cx: &App) {
        self.tab = listed_tab(self.tab, self.board_pr(cx).is_some());
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

    fn step(&mut self, by: isize, cx: &mut Context<Self>) {
        let rows = self.walkable(cx);
        if rows.is_empty() {
            return;
        }
        let at = self
            .picked
            .as_ref()
            .and_then(|p| rows.iter().position(|r| r.key() == p.key()));
        let next = match at {
            Some(i) => (i as isize + by).clamp(0, rows.len() as isize - 1) as usize,
            None => 0,
        };
        let open = opens_on_step(self.sheet_open);
        if let Some(entry) = self.row_entry(&rows[next], cx) {
            self.select(entry, open, cx);
        }
    }

    fn refresh(&mut self, cx: &mut Context<Self>) {
        self.github.update(cx, |g, cx| g.refresh(cx));
        self.pilot.update(cx, |p, cx| p.refresh(cx));
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

    /// Enter or Ctrl+Enter in the comment input: posts the comment, runs a
    /// `/` action, or says in the card why nothing was sent.
    fn send_comment(&mut self, cx: &mut Context<Self>) {
        let text = self.comment.read(cx).text().to_string();
        let shown = self.item();
        let hold = |this: &mut Self, text: String, cx: &mut Context<Self>| {
            this.note = Some(Note {
                ok: false,
                quiet: true,
                text,
            });
            cx.notify();
        };
        let posting = self.posting.as_ref().map(|(on, t)| (on, t.as_str()));
        match comment_send(
            &text,
            shown.as_ref(),
            self.comment_for.as_ref(),
            self.busy,
            posting,
        ) {
            CommentSend::Post => {}
            CommentSend::Command => {
                self.comment_for = shown;
                self.submit_command(cx);
                return;
            }
            CommentSend::NoItem => {
                return hold(
                    self,
                    "Pick a pull request or issue to comment on.".into(),
                    cx,
                );
            }
            CommentSend::Empty => {
                return hold(self, "Write a comment first.".into(), cx);
            }
            CommentSend::Busy(what) => return hold(self, busy_note(what), cx),
            CommentSend::Sending => return hold(self, "Sending this comment…".into(), cx),
            // Text typed for another item is never sent to this one.
            CommentSend::Elsewhere(other) => {
                self.switch_comment(cx);
                let text = format!(
                    "Not sent: that text was written for {}. It's kept there as a draft.",
                    other.label()
                );
                return hold(self, text, cx);
            }
        }
        // The same item, maybe read again: the text is for the one shown.
        self.comment_for = shown.clone();
        self.posting = shown.map(|on| (on, text.clone()));
        let body = text.clone();
        let on = self.item();
        self.write(
            "comment",
            "Comment posted.".into(),
            move |item| pr::comment(item, &body).map(|_| ()),
            // The input stays usable while gh works: keep anything typed since,
            // or meant for another item.
            move |this, cx| {
                if same_item(this.item().as_ref(), on.as_ref())
                    && this.comment.read(cx).text() == text
                {
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

    /// A `/` action typed in the comment input.
    fn submit_command(&mut self, cx: &mut Context<Self>) {
        let text = self.comment.read(cx).text().trim().to_string();
        let Some(cmd) = text.strip_prefix('/') else {
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
                    text: format!("Not sent: “{text}” is not an action. Actions here: {list}"),
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
        // A card sets its own after this.
        self.pilot_act = None;
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
        self._body_sub = Some(sub);
    }

    fn run_confirm(&mut self, c: Confirm, cx: &mut Context<Self>) {
        // A card's review comes in with its line breaks as ⏎.
        let body = self.body_input.read(cx).text().replace(NEWLINE, "\n");
        if let Some(act) = self.pilot_for(c) {
            self.run_pilot_confirm(c, act, body, cx);
            return;
        }
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
            "enter" if self.picked.is_some() && !self.sheet_open => {
                self.sheet_open = true;
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
            "escape" if self.sheet_open || self.floating => self.close_sheet(cx),
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

    /// The pull request picked, when it is the user's own, still open, and in
    /// the repository arakun-pilot works on: (number, title). The palette
    /// offers it as where a task for the cloud agent goes.
    pub fn own_open_pull(&self, cx: &App) -> Option<(u64, String)> {
        let item = self.item().filter(|i| {
            i.kind == ItemKind::Pull && i.repo.eq_ignore_ascii_case(crate::github::pilot::REPO)
        })?;
        if !self.is_mine(cx) {
            return None;
        }
        let (title, state) = match self.github.read(cx).details(&item) {
            Some(Details::Pull(p)) => (p.title.clone(), p.state.clone()),
            _ => {
                let e = self.picked.as_ref()?;
                (e.title.clone(), e.state.clone())
            }
        };
        still_open(&state).then_some((item.number, title))
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
    // The open fence, and how many block quotes it opened in.
    let mut fence: Option<(char, usize, usize)> = None;
    let mut in_comment = false;
    for line in text.lines() {
        if let Some((ch, len, quotes)) = fence {
            // Only the quotes the fence opened in are markup: a `>` past
            // them is code.
            let (inner, found) = unquote(line, quotes);
            // A fence in a block quote ends with the quote.
            if found < quotes {
                fence = None;
            } else {
                let t = inner.trim_start();
                // Four spaces before it make the marker code, not a close.
                if inner.len() - t.len() <= 3
                    && t.len() - t.trim_start_matches(ch).len() >= len
                    && t.trim_start_matches(ch).trim().is_empty()
                {
                    fence = None;
                }
                lines.push((line.to_string(), true));
                continue;
            }
        }
        let (inner, quotes) = unquote(line, usize::MAX);
        if !in_comment && let Some((ch, len)) = fence_open(inner) {
            fence = Some((ch, len, quotes));
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
    let mut out = Vec::with_capacity(lines.len());
    let mut i = 0;
    while i < lines.len() {
        let breaks = |(line, code): &(String, bool)| *code || line.trim().is_empty();
        if breaks(&lines[i]) {
            out.push(lines[i].0.clone());
            i += 1;
            continue;
        }
        // A paragraph: a code span in it can run over lines.
        let end = lines[i..]
            .iter()
            .position(breaks)
            .map_or(lines.len(), |n| i + n);
        let para: Vec<&str> = lines[i..end].iter().map(|(l, _)| l.as_str()).collect();
        // Blocks next to each other each leave a blank line: one is enough,
        // and the paragraph's own ends have one already.
        let blank = |l: &String| l.trim_matches([' ', '>']).is_empty();
        let mut md = html_paragraph(&para);
        md.dedup_by(|b, a| blank(a) && a == b);
        let gone = md.iter().position(|l| !blank(l)).unwrap_or(md.len());
        md.drain(..gone);
        while md.last().is_some_and(blank) {
            md.pop();
        }
        out.extend(md);
        i = end;
    }
    out.join("\n")
}

/// A line inside block quotes without up to `max` of their `>` markers, and
/// how many it took off: a fence opens and closes there as anywhere else.
fn unquote(line: &str, max: usize) -> (&str, usize) {
    let mut t = line;
    let mut n = 0;
    while n < max {
        match t.trim_start_matches(' ').strip_prefix('>') {
            Some(r) => t = r.strip_prefix(' ').unwrap_or(r),
            None => break,
        }
        n += 1;
    }
    (t, n)
}

/// A paragraph with its HTML as markdown, read as one text so a link or
/// bold that runs over lines keeps its meaning. When a tag took a line
/// break with it (a `<summary>` over lines), the lines no longer match
/// those in, so each line is read on its own instead.
fn html_paragraph(para: &[&str]) -> Vec<String> {
    let spans = multiline_spans(para);
    if para.len() > 1 && para.iter().any(|l| l.contains('<')) {
        // Read in one go, a bold or a link around a code span over lines
        // stays open over it; html_inline keeps code spans as they are.
        // It also knows which list an `<li>` on a later line is in.
        let md = html_inline(&para.join("\n"), false, 0);
        let pieces: Vec<&str> = md.split('\n').collect();
        if pieces.len() == para.len() {
            return para
                .iter()
                .zip(pieces)
                .flat_map(|(line, md)| md_lines(line, md))
                .collect();
        }
    }
    para.iter()
        .zip(spans)
        .flat_map(|(line, spans)| {
            if line.contains('<') {
                md_lines(line, &html_line(line, &spans))
            } else {
                vec![line.to_string()]
            }
        })
        .collect()
}

/// The parts of each line of a paragraph that are in a code span running
/// over more than one line, as byte ranges: a code span can hold a line
/// break, and HTML in it is text.
fn multiline_spans(lines: &[&str]) -> Vec<Vec<(usize, usize)>> {
    let joined = lines.join("\n");
    let mut spans = Vec::new();
    let mut at = 0;
    while let Some(i) = joined[at..].find('`') {
        let start = at + i;
        let end = start + code_span_len(&joined[start..]);
        if joined[start..end].contains('\n') {
            spans.push((start, end));
        }
        at = end;
    }
    // The spans are in order and don't overlap: skip those already passed.
    let mut next = 0;
    let mut start = 0;
    lines
        .iter()
        .map(|line| {
            let end = start + line.len();
            while spans.get(next).is_some_and(|&(_, b)| b <= start) {
                next += 1;
            }
            let keep = spans[next..]
                .iter()
                .take_while(|&&(a, _)| a < end)
                .map(|&(a, b)| (a.max(start) - start, b.min(end) - start))
                .collect();
            start = end + 1;
            keep
        })
        .collect()
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
            let end = code_span_len(rest);
            out.push_str(&rest[..end]);
            rest = &rest[end..];
        } else {
            out.push('<');
            rest = &rest[1..];
        }
    }
}

/// How much of `rest`, which starts with backticks, is a code span: up to
/// the next run of exactly as many backticks, or only the run when none
/// closes it (then the backticks are text).
fn code_span_len(rest: &str) -> usize {
    let ticks = rest.len() - rest.trim_start_matches('`').len();
    let run = &rest[..ticks];
    let after = &rest[ticks..];
    let mut from = 0;
    loop {
        match after[from..].find(run) {
            Some(i) => {
                let i = from + i;
                let len = after[i..].len() - after[i..].trim_start_matches('`').len();
                if len == ticks {
                    return ticks + i + ticks;
                }
                from = i + len;
            }
            None => return ticks,
        }
    }
}

/// Tags GitHub renders that only shape or decorate (CodeRabbit writes its
/// reviews in them): they go, and what they hold stays.
const LAYOUT_TAGS: &[&str] = &[
    "abbr",
    "blockquote",
    "br",
    "dd",
    "details",
    "div",
    "dl",
    "dt",
    "h1",
    "h2",
    "h3",
    "h4",
    "h5",
    "h6",
    "hr",
    "ins",
    "li",
    "ol",
    "p",
    "picture",
    "source",
    "span",
    "sub",
    "sup",
    "table",
    "tbody",
    "td",
    "th",
    "thead",
    "tr",
    "u",
    "ul",
];

/// A known HTML tag at the start of `rest`: its lowercase name, whether it
/// closes, its attributes and its length. Anything else (`Vec<T>`, an
/// autolink `<https://…>`) is not one.
fn html_tag(rest: &str) -> Option<(String, bool, &str, usize)> {
    let body = rest.strip_prefix('<')?;
    let (closing, body) = match body.strip_prefix('/') {
        Some(b) => (true, b),
        None => (false, body),
    };
    let name_len = body
        .find(|c: char| !c.is_ascii_alphanumeric())
        .unwrap_or(body.len());
    let name = body[..name_len].to_ascii_lowercase();
    let known = LAYOUT_TAGS.contains(&name.as_str())
        || matches!(
            name.as_str(),
            "a" | "b" | "strong" | "em" | "i" | "code" | "kbd" | "del" | "s" | "img" | "summary"
        );
    let after = &body[name_len..];
    if !known || !(after.starts_with(['>', ' ', '/', '\t'])) {
        return None;
    }
    // A `>` in a quoted value (`title="a > b"`) does not end the tag.
    let mut quote = None;
    let end = after.char_indices().find_map(|(i, c)| {
        match (quote, c) {
            (None, '"' | '\'') => quote = Some(c),
            (Some(q), c) if c == q => quote = None,
            (None, '>') => return Some(i),
            _ => {}
        }
        None
    })?;
    let len = 1 + usize::from(closing) + name_len + end + 1;
    Some((name, closing, after[..end].trim_end_matches('/'), len))
}

/// The value of `attr` in a tag's attributes, quoted with `"` or `'` or
/// not at all. `alt` is not `data-alt`.
fn tag_attr<'a>(attrs: &'a str, attr: &str) -> Option<&'a str> {
    let mut rest = attrs;
    loop {
        rest = rest.trim_start_matches(|c: char| c.is_whitespace() || c == '/');
        if rest.is_empty() {
            return None;
        }
        // At least one byte, so a stray `=` can't stop the scan.
        let name_len = rest
            .find(|c: char| c.is_whitespace() || c == '=')
            .unwrap_or(rest.len())
            .max(1);
        let name = &rest[..name_len];
        rest = rest[name_len..].trim_start();
        let value = rest.strip_prefix('=').map(|v| {
            let v = v.trim_start();
            let (value, after) = match v.chars().next() {
                Some(q @ ('"' | '\'')) => {
                    let inner = &v[1..];
                    let end = inner.find(q).unwrap_or(inner.len());
                    (&inner[..end], &inner[(end + 1).min(inner.len())..])
                }
                _ => v.split_at(v.find(char::is_whitespace).unwrap_or(v.len())),
            };
            rest = after;
            value
        });
        if name.eq_ignore_ascii_case(attr) {
            return value;
        }
    }
}

/// Where `close` (a lowercase closing tag) starts in `rest`, in any case.
/// With `spans`, one in a code span or in another tag's attributes is text
/// and doesn't count; inside `<code>` a backtick is text, so there it does.
fn find_close(rest: &str, close: &str, spans: bool) -> Option<usize> {
    let bytes = rest.as_bytes();
    // `<code>` and `<kbd>` closing tags not found in what is left: once one
    // is missing, it is missing later too, so each is looked for once.
    let mut missing: Vec<&str> = Vec::new();
    let mut at = 0;
    while at + close.len() <= bytes.len() {
        if bytes[at..at + close.len()].eq_ignore_ascii_case(close.as_bytes()) {
            return Some(at);
        }
        if spans && bytes[at] == b'`' {
            at += code_span_len(&rest[at..]);
        } else if let Some((name, closing, _, len)) = (spans && bytes[at] == b'<')
            .then(|| html_tag(&rest[at..]))
            .flatten()
        {
            at += len;
            let inner = match (name.as_str(), closing) {
                ("code", false) => "</code>",
                ("kbd", false) => "</kbd>",
                _ => continue,
            };
            if missing.contains(&inner) {
                continue;
            }
            // The whole element: its backticks are text, so they can't
            // start a code span.
            match find_close(&rest[at..], inner, false) {
                Some(end) => at += end + inner.len(),
                None => missing.push(inner),
            }
        } else {
            at += 1;
        }
    }
    None
}

/// An `<img>` someone put in the text (a screenshot) as a markdown image.
/// An icon is not one: it has no source, or a size of 32 pixels or less
/// (CodeRabbit's are 12 to 32).
fn content_image(attrs: &str) -> Option<String> {
    let src = tag_attr(attrs, "src")
        .map(str::trim)
        .filter(|s| !s.is_empty())?;
    let small = ["width", "height"].iter().any(|a| {
        tag_attr(attrs, a)
            .and_then(|v| v.trim().parse::<u32>().ok())
            .is_some_and(|n| n <= 32)
    });
    if small {
        return None;
    }
    let alt = tag_attr(attrs, "alt").unwrap_or("").trim();
    let alt = alt.replace('[', "\\[").replace(']', "\\]");
    Some(format!("![{alt}]({})", link_dest(src)))
}

/// `url` as a markdown link destination: in `<>`, so a `)` or a space in it
/// doesn't end the link early.
fn link_dest(url: &str) -> String {
    let mut out = String::with_capacity(url.len() + 2);
    out.push('<');
    for c in url.chars() {
        match c {
            '<' | '>' | '\\' => {
                out.push('\\');
                out.push(c);
            }
            // A line break can't be in one.
            '\n' | '\r' => {}
            c => out.push(c),
        }
    }
    out.push('>');
    out
}

/// The attributes of the first `<img>` in a picture.
fn picture_img(mut rest: &str) -> Option<&str> {
    while let Some(at) = rest.find('<') {
        rest = &rest[at..];
        match html_tag(rest) {
            Some((name, false, attrs, _)) if name == "img" => return Some(attrs),
            Some((.., len)) => rest = &rest[len..],
            None => rest = &rest[1..],
        }
    }
    None
}

/// The line breaks in `html`, each with the indent and `>` markers of the
/// line it starts: a picture over lines in a block quote or a list leaves
/// its lines in it.
fn picture_breaks(html: &str) -> String {
    html.split('\n')
        .skip(1)
        .map(|line| {
            let text = line.trim_start_matches([' ', '\t', '>']);
            format!("\n{}", &line[..line.len() - text.len()])
        })
        .collect()
}

/// HTML's escapes for code, which a markdown code span would show as typed.
fn decode_entities(text: &str) -> String {
    text.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&nbsp;", " ")
        .replace("&amp;", "&")
}

/// `text` as a markdown code span, fenced by more backticks than it holds
/// in a row.
fn code_span(text: &str) -> String {
    if text.trim().is_empty() {
        return String::new();
    }
    let longest = text.split(|c| c != '`').map(str::len).max().unwrap_or(0);
    let ticks = "`".repeat(longest + 1);
    let pad = if text.starts_with('`') || text.ends_with('`') {
        " "
    } else {
        ""
    };
    format!("{ticks}{pad}{text}{pad}{ticks}")
}

/// One line of a body with its HTML as markdown: bold, emphasis, code and
/// links keep their meaning, icons (CodeRabbit's pictures) go, screenshots
/// become images, and layout tags go with what they hold left in place. A
/// summary is bold; inside it only its text counts. Code spans stay as they
/// are, and so do the `keep` ranges (parts of code spans over lines).
/// Block tags leave the marks that [`md_lines`] writes out.
fn html_line(line: &str, keep: &[(usize, usize)]) -> String {
    let mut md = String::with_capacity(line.len());
    let mut at = 0;
    for &(from, to) in keep {
        md.push_str(&html_inline(&line[at..from], false, 0));
        md.push_str(&line[from..to]);
        at = to;
    }
    md.push_str(&html_inline(&line[at..], false, 0));
    md
}

// Marks html_inline leaves until the lines are split: line breaks of their
// own would shift the lines of a paragraph.

/// Stands for `<hr>`.
const RULE: char = '\0';
/// Stands for a blank line, where a block (`<p>`, `<blockquote>`, a list,
/// a heading) starts or ends.
const BREAK: char = '\u{1}';
/// Stands for a line break before a list item, when text came before it.
const LINE: char = '\u{2}';
/// One space of a nested list item's indent: the spaces a new line starts
/// with are dropped, these are not.
const NEST: char = '\u{3}';

/// The markdown lines `md` stands for, made from `line`. A tag taken from
/// the start leaves no indent the line didn't have: indented text reads as
/// code. A line that held only tags is gone, not a blank line: a blank line
/// would end a list or a table around it. So is one left with only the `>`
/// of its block quotes. Blank lines come only from the marks of block tags.
fn md_lines(line: &str, md: &str) -> Vec<String> {
    let bare = |s: &str| s.trim_matches([' ', '\t', '>']).is_empty();
    let md = if line.starts_with(char::is_whitespace) {
        md
    } else {
        md.trim_start_matches([' ', '\t'])
    };
    // A table row can't break over lines: its blocks run on, a space apart.
    let row = unquote(line, usize::MAX).0.trim_start().starts_with('|');
    if row || !md.contains([RULE, BREAK, LINE]) {
        let md = md.replace([RULE, BREAK, LINE, NEST], " ");
        let gone = md.trim().is_empty() || (bare(&md) && !bare(line));
        return if gone { Vec::new() } else { vec![md] };
    }
    // Each new line stays in the block quotes and the list item `line` is in.
    let (inner, _) = unquote(line, usize::MAX);
    let quote = &line[..line.len() - inner.len()];
    let lead = quote.len() + list_lead(inner);
    let cont = format!("{quote}{}", " ".repeat(lead - quote.len()));
    let blank = quote.trim_end();
    let mut out: Vec<String> = Vec::new();
    let mut cur = String::new();
    // Whether `cur` is a new line that has no text yet.
    let mut fresh = false;
    // Only the quote markers or the list marker the line starts with: a
    // block there starts the item, not a line after it.
    let only_lead = |cur: &str| {
        lead > quote.len()
            && line
                .get(..lead)
                .is_some_and(|l| cur.trim_end() == l.trim_end())
    };
    let end_line = |cur: &mut String, out: &mut Vec<String>| {
        if !bare(cur) {
            out.push(cur.trim_end().replace(NEST, " "));
        }
        cur.clear();
    };
    let blank_line = |out: &mut Vec<String>| {
        if out.last().is_none_or(|l| l != blank) {
            out.push(blank.to_string());
        }
    };
    for piece in md.split_inclusive([RULE, BREAK, LINE]) {
        let (text, mark) = match piece.chars().last() {
            Some(c @ (RULE | BREAK | LINE)) => (&piece[..piece.len() - 1], Some(c)),
            _ => (piece, None),
        };
        if !fresh {
            cur.push_str(text);
        } else if !text.trim().is_empty() {
            cur = format!("{cont}{}", text.trim_start_matches([' ', '\t']));
            fresh = false;
        }
        let Some(mark) = mark else { continue };
        if mark != RULE && only_lead(&cur) {
            continue;
        }
        end_line(&mut cur, &mut out);
        if mark != LINE {
            blank_line(&mut out);
        }
        if mark == RULE {
            out.push(format!("{cont}---"));
            blank_line(&mut out);
        }
        fresh = true;
    }
    end_line(&mut cur, &mut out);
    out
}

/// How wide the list marker `line` starts with is, with the spaces before
/// and after it: the indent of the lines that go on in its item.
fn list_lead(line: &str) -> usize {
    let t = line.trim_start_matches(' ');
    let digits = t.len() - t.trim_start_matches(|c: char| c.is_ascii_digit()).len();
    let marker = match t.as_bytes().get(digits) {
        Some(b'-' | b'*' | b'+') if digits == 0 => 1,
        Some(b'.' | b')') if (1..=9).contains(&digits) => digits + 1,
        _ => return 0,
    };
    let after = &t[marker..];
    let space = after.len() - after.trim_start_matches(' ').len();
    if space == 0 && !after.is_empty() {
        return 0;
    }
    line.len() - t.len() + marker + space.clamp(1, 4)
}

/// The heading level of a tag name: 2 for `h2`.
fn heading_level(name: &str) -> Option<usize> {
    let level = name.strip_prefix('h')?.parse().ok()?;
    (1..=6).contains(&level).then_some(level)
}

const HEADING_ENDS: [&str; 6] = ["</h1>", "</h2>", "</h3>", "</h4>", "</h5>", "</h6>"];

/// How deep a summary or link is read inside another. Deeper ones lose
/// their tags only: each level reads the rest of the line again.
const MAX_DEPTH: u8 = 4;

fn html_inline(line: &str, plain: bool, depth: u8) -> String {
    // A layout tag between words keeps them apart.
    fn gap(out: &mut String) {
        if !out.is_empty() && !out.ends_with(' ') {
            out.push(' ');
        }
    }
    let mut out = String::with_capacity(line.len());
    // Where each open bold, emphasis or strike starts in `out`, by its marker.
    let mut open: Vec<(&'static str, usize)> = Vec::new();
    // Closing tags not found in what is left of the line.
    let mut missing: Vec<&'static str> = Vec::new();
    // The open lists, inside out: an ordered one's next number, and how
    // wide its last item's marker was (a nested item is indented by it).
    let mut lists: Vec<(Option<u32>, usize)> = Vec::new();
    let mut rest = line;
    while let Some(at) = rest.find(['<', '`']) {
        out.push_str(&rest[..at]);
        rest = &rest[at..];
        if rest.starts_with('`') {
            let end = code_span_len(rest);
            out.push_str(&rest[..end]);
            rest = &rest[end..];
            continue;
        }
        let Some((name, closing, attrs, len)) = html_tag(rest) else {
            out.push('<');
            rest = &rest[1..];
            continue;
        };
        let tag = &rest[..len];
        rest = &rest[len..];
        let mut close_in = |close: &'static str| {
            // Once a closing tag is missing, it is missing from every later
            // part of the line too: no need to look again.
            if missing.contains(&close) {
                return None;
            }
            let spans = !matches!(close, "</code>" | "</kbd>");
            let end = find_close(rest, close, spans);
            if end.is_none() {
                missing.push(close);
            }
            end
        };
        match (name.as_str(), closing) {
            ("summary", false) => match close_in("</summary>").filter(|_| depth < MAX_DEPTH) {
                Some(end) => {
                    let text = html_inline(&rest[..end], true, depth + 1);
                    let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
                    if !text.is_empty() {
                        out.push_str(&format!("**{text}**"));
                    }
                    rest = &rest[end + "</summary>".len()..];
                }
                None => gap(&mut out),
            },
            ("picture", false) => match close_in("</picture>") {
                Some(end) => {
                    let img = picture_img(&rest[..end]);
                    // Its line breaks stay after it, so the lines of a
                    // paragraph stay where they were, and in their quote.
                    let breaks = format!("{}{}", picture_breaks(tag), picture_breaks(&rest[..end]));
                    rest = &rest[end + "</picture>".len()..];
                    if let Some(image) = img.and_then(content_image) {
                        out.push_str(&image);
                    } else {
                        // An icon: its words before a count ("Passed 4"),
                        // else nothing.
                        let alt = img.and_then(|a| tag_attr(a, "alt"));
                        match alt.filter(|a| !a.trim().is_empty() && counts(rest)) {
                            Some(alt) => out.push_str(alt.trim()),
                            None => rest = after_icon(&out, rest),
                        }
                    }
                    out.push_str(&breaks);
                }
                // Over lines: its `<img>` is read on its own.
                None => rest = after_icon(&out, rest),
            },
            ("img", _) => match content_image(attrs) {
                Some(image) => out.push_str(&image),
                None => {
                    match tag_attr(attrs, "alt").filter(|a| !a.trim().is_empty() && counts(rest)) {
                        Some(alt) => out.push_str(alt.trim()),
                        None => rest = after_icon(&out, rest),
                    }
                }
            },
            ("source", _) => rest = after_icon(&out, rest),
            // A bare `<a>` is more likely a generic (`Result<A>`) than a link.
            ("a", false) if attrs.trim().is_empty() => out.push_str(tag),
            ("a", false) => match close_in("</a>").filter(|_| depth < MAX_DEPTH) {
                Some(end) => {
                    let inner = html_inline(&rest[..end], plain, depth + 1);
                    let text = inner.trim();
                    // Line breaks around the text stay outside the link, so
                    // the lines of a paragraph stay where they were.
                    let breaks = |s: &str| "\n".repeat(s.matches('\n').count());
                    out.push_str(&breaks(&inner[..inner.len() - inner.trim_start().len()]));
                    match tag_attr(attrs, "href") {
                        Some(href) if !text.is_empty() && !plain => {
                            out.push_str(&format!("[{text}]({})", link_dest(href)))
                        }
                        _ => out.push_str(text),
                    }
                    out.push_str(&breaks(&inner[inner.trim_end().len()..]));
                    rest = &rest[end + "</a>".len()..];
                }
                None => gap(&mut out),
            },
            ("br", _) => out.push(' '),
            _ if plain => {}
            (tag @ ("b" | "strong" | "em" | "i" | "del" | "s"), closing) => {
                let marker = match tag {
                    "b" | "strong" => "**",
                    "em" | "i" => "*",
                    _ => "~~",
                };
                if !closing {
                    open.push((marker, out.len()));
                } else if let Some(at) = open.iter().rposition(|(m, _)| *m == marker) {
                    let from = open[at].1;
                    // Tags opened inside it and still open are dropped: the
                    // text they started in moves.
                    open.truncate(at);
                    close_marker(&mut out, marker, from);
                }
            }
            ("code" | "kbd", false) => {
                let close = if name == "code" { "</code>" } else { "</kbd>" };
                match close_in(close) {
                    Some(end) => {
                        out.push_str(&code_span(&decode_entities(&rest[..end])));
                        rest = &rest[end + close.len()..];
                    }
                    None => gap(&mut out),
                }
            }
            ("code" | "kbd", true) => {}
            // `x<sup>2</sup>` is one word.
            ("sup" | "sub", _) => {}
            // Blank lines around it: after a line of text, `---` would make
            // that line a heading.
            ("hr", _) => out.push(RULE),
            // Blocks only where a line can start: not in a link or a
            // summary. In a list item they stay in the item.
            ("ul" | "ol", false) if depth == 0 => {
                if lists.is_empty() {
                    out.push(BREAK);
                }
                let start = tag_attr(attrs, "start").and_then(|s| s.trim().parse().ok());
                lists.push(((name == "ol").then(|| start.unwrap_or(1)), 0));
            }
            ("ul" | "ol", true) if depth == 0 => {
                lists.pop();
                if lists.is_empty() {
                    out.push(BREAK);
                }
            }
            ("li", false) if depth == 0 => {
                let outer = lists.len().saturating_sub(1);
                let indent: usize = lists[..outer].iter().map(|(_, w)| w).sum();
                let marker = match lists.last_mut() {
                    Some((Some(n), w)) => {
                        let m = format!("{n}. ");
                        *n = n.saturating_add(1);
                        *w = m.len();
                        m
                    }
                    Some((None, w)) => {
                        *w = 2;
                        "- ".into()
                    }
                    None => "- ".into(),
                };
                out.push(LINE);
                out.extend(std::iter::repeat_n(NEST, indent));
                out.push_str(&marker);
            }
            // The next item or the line's end keeps the words apart.
            ("li", true) if depth == 0 && (rest.is_empty() || rest.starts_with(['<', '\n'])) => {}
            ("p" | "blockquote", _) if depth == 0 && lists.is_empty() => out.push(BREAK),
            (h, false) if depth == 0 && lists.is_empty() && heading_level(h).is_some() => {
                let level = heading_level(h).unwrap_or(1);
                out.push(BREAK);
                if let Some(end) = close_in(HEADING_ENDS[level - 1]) {
                    // One line: its line breaks go after it, so the lines
                    // of a paragraph stay where they were.
                    let inner = html_inline(&rest[..end], false, depth + 1);
                    let text = inner.split_whitespace().collect::<Vec<_>>().join(" ");
                    if !text.is_empty() {
                        out.push_str(&format!("{} {text}", "#".repeat(level)));
                        out.push(BREAK);
                    }
                    out.extend(std::iter::repeat_n('\n', inner.matches('\n').count()));
                    rest = &rest[end + HEADING_ENDS[level - 1].len()..];
                }
            }
            (h, true) if depth == 0 && lists.is_empty() && heading_level(h).is_some() => {
                out.push(BREAK)
            }
            _ => gap(&mut out),
        }
    }
    out.push_str(rest);
    out
}

/// Whether the text after an icon is a number, past spaces and tags: the
/// icon says what it counts.
fn counts(mut rest: &str) -> bool {
    loop {
        rest = rest.trim_start();
        match html_tag(rest) {
            Some((.., len)) => rest = &rest[len..],
            None => return rest.starts_with(|c: char| c.is_ascii_digit()),
        }
    }
}

/// What follows an icon that was taken out: without the space that kept it
/// from the text, when nothing or a marker comes before (`**<icon> Info**`
/// would leave `** Info**`, which is not bold).
fn after_icon<'a>(out: &str, rest: &'a str) -> &'a str {
    if out.is_empty() || out.ends_with(['*', '~', '_', ' ', '(', '[']) {
        rest.trim_start()
    } else {
        rest
    }
}

/// Wraps what `out` holds from `from` in `marker`, with the spaces at its
/// ends moved outside: `** Moderate**` is not bold in markdown, and an
/// icon taken out of a bold text leaves just that space. Nothing is
/// wrapped when nothing is left.
fn close_marker(out: &mut String, marker: &str, from: usize) {
    let inner = out.split_off(from);
    let text = inner.trim();
    if text.is_empty() {
        out.push_str(&inner);
        return;
    }
    let lead = &inner[..inner.len() - inner.trim_start().len()];
    let trail = &inner[inner.trim_end().len()..];
    out.push_str(&format!("{lead}{marker}{text}{marker}{trail}"));
}

/// Whether J/K (↓/↑) open the item they move to: on the list they only
/// move the pick, and Enter opens it; with the sheet open they go to the
/// next one, still open.
fn opens_on_step(sheet_open: bool) -> bool {
    sheet_open
}

/// The tab to show: Pilot only while the board has the pull request, else
/// the conversation, which every pull request lists.
fn listed_tab(tab: Tab, on_board: bool) -> Tab {
    match tab {
        Tab::Pilot if !on_board => Tab::Timeline,
        t => t,
    }
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

/// Whether a pull request's state (`OPEN` from gh, `open` from a search) is
/// open. Unknown (a notification only) counts as open: GitHub refuses a task
/// on a closed one anyway.
fn still_open(state: &str) -> bool {
    state.is_empty() || state.eq_ignore_ascii_case("open")
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
        let root = div()
            .id("github-view")
            .track_focus(&self.focus)
            .on_key_down(cx.listener(Self::on_key))
            .relative()
            .flex()
            .size_full()
            .overflow_hidden()
            .font_family(theme::FONT_SANS)
            .ty(Type::Body)
            .text_color(theme::text())
            .child(measure);
        if crate::github::GitHub::disabled() && !self.floating {
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
        // Over another window only the sheet shows; in the tab, the list.
        let root = root.when(!self.floating, |d| {
            d.child(Pane::cached(
                &self.queue_pane,
                StyleRefinement::default().size_full(),
            ))
        });
        let shows_sheet = (self.sheet_open || self.floating) && self.picked.is_some();
        let root = root.when(shows_sheet, |d| {
            d.child(
                // The rest dims: a click on it closes the sheet.
                div()
                    .id("gh-sheet-dim")
                    .absolute()
                    .top_0()
                    .left_0()
                    .size_full()
                    .bg(gpui::black().opacity(0.45))
                    .occlude()
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, _, _, cx| this.close_sheet(cx)),
                    ),
            )
            .child(
                div()
                    .id("gh-sheet")
                    .absolute()
                    .top(crate::ui::sp(GAP))
                    .bottom(crate::ui::sp(GAP))
                    .right(crate::ui::sp(GAP))
                    .w(px(lay.sheet_w))
                    .flex()
                    .flex_col()
                    .rounded(crate::ui::rad(ui::RADIUS_XL))
                    .bg(theme::sheet())
                    .shadow_lg()
                    .overflow_hidden()
                    .occlude()
                    .child(Pane::cached(
                        &self.card_pane,
                        StyleRefinement::default().size_full(),
                    )),
            )
        });
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
    fn only_an_open_pull_takes_a_task() {
        assert!(still_open("OPEN") && still_open("open") && still_open(""));
        assert!(!still_open("MERGED") && !still_open("closed"));
    }

    #[test]
    fn the_layout_follows_the_width() {
        // The sheet takes a share of the window, within bounds.
        assert_eq!(layout(1900.).sheet_w, 680.);
        assert_eq!(layout(1000.).sheet_w, 560.);
        let l = layout(1400.);
        assert!(!l.narrow && (l.sheet_w - 588.).abs() < 0.5);
        // Narrow: the sheet covers the list.
        assert!(layout(800.).narrow);
        assert!(!layout(900.).narrow);
    }

    #[test]
    fn a_quote_marker_in_a_fence_is_code() {
        // Not in a quote: `> ```` is code, not the closing fence.
        let body = "```text\n> ```\n<b>x</b>\n```";
        assert_eq!(without_html(body), body);
        // In one quote: only its marker is markup.
        let body = "> ```\n> > ```\n> <b>x</b>\n> ```";
        assert_eq!(without_html(body), body);
        // The quote ends, and the fence with it.
        assert_eq!(without_html("> ```\n> a\n<b>x</b>"), "> ```\n> a\n**x**");
    }

    #[test]
    fn a_link_keeps_its_whole_destination() {
        let md =
            without_html("<a href=\"https://example.com/a)b c\">docs</a> <img src=\"s>(.png\">");
        let dests: Vec<String> = pulldown_cmark::Parser::new(&md)
            .filter_map(|e| match e {
                pulldown_cmark::Event::Start(
                    pulldown_cmark::Tag::Link { dest_url, .. }
                    | pulldown_cmark::Tag::Image { dest_url, .. },
                ) => Some(dest_url.to_string()),
                _ => None,
            })
            .collect();
        assert_eq!(dests, ["https://example.com/a)b c", "s>(.png"], "{md}");
    }

    #[test]
    fn many_code_spans_over_lines_stay_fast() {
        let body = "`a\nb`\n".repeat(10_000);
        let start = std::time::Instant::now();
        assert_eq!(without_html(&body), body.trim_end());
        assert!(start.elapsed() < std::time::Duration::from_secs(2));
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
    fn coderabbits_html_reads_as_markdown() {
        // A review summary as CodeRabbit writes it (shortened).
        let body = concat!(
            "<details>\n",
            "<summary><strong>Merge Risk:</strong> <strong><picture><img src=\"https://x/risk.svg\" width=\"16\" height=\"16\" alt=\"\"></picture> Moderate</strong> · up to <code>8b9cb</code></summary>\n",
            "<dl>\n<dd>\n<br>\n\n",
            "<strong><sup><picture><img src=\"https://x/failed.svg\" width=\"14\" alt=\"Failed\"></picture></sup> Failed checks: 1 warning</strong>\n\n",
            "<h6>\n**<picture><img src=\"https://x/info.svg\" width=\"12\"></picture> Review info**\n</h6>\n",
            "<a href=\"https://app.coderabbit.ai/x\"><picture><source srcset=\"d.svg\"><img src=\"l.svg\" alt=\"Explain Change →\" height=\"32\"></picture></a>\n",
            "See <a href=\"https://docs.coderabbit.ai\">the docs</a> and <b>this</b>.\n",
            "<summary><strong>Pre-merge checks: <sup><picture><img alt=\"Passed\"></picture></sup> 4 · <sup><picture><img alt=\"Failed\"></picture></sup> 1</strong></summary>\n",
            "</dd>\n</dl>\n</details>",
        );
        assert_eq!(
            without_html(body),
            concat!(
                "**Merge Risk: Moderate · up to 8b9cb**\n\n",
                "**Failed checks: 1 warning**\n\n",
                // A heading, though only bold in it.
                "###### **Review info**\n\n",
                "See [the docs](<https://docs.coderabbit.ai>) and **this**.\n",
                // An icon that counts keeps its words.
                "**Pre-merge checks: Passed 4 · Failed 1**",
            )
        );
        // Not tags: generics, autolinks, comparisons, and code spans stay.
        for kept in [
            "Vec<String> and Option<T>",
            "<https://example.com>",
            "a < b and c > d",
            "Use `<details>` here",
        ] {
            assert_eq!(without_html(kept), kept);
        }
        // A fenced block keeps its HTML.
        let fenced = "```html\n<dd><b>x</b></dd>\n```";
        assert_eq!(without_html(fenced), fenced);
    }

    #[test]
    fn pasted_screenshots_stay_as_images() {
        // GitHub's own paste format.
        let pasted = "Before:\n\n<img width=\"1200\" height=\"700\" alt=\"Image\" src=\"https://github.com/user-attachments/assets/abc\" />\n\nAfter:";
        assert_eq!(
            without_html(pasted),
            "Before:\n\n![Image](<https://github.com/user-attachments/assets/abc>)\n\nAfter:"
        );
        // A table of before and after shots, and a picture over lines.
        assert_eq!(
            without_html(
                "<table><tr><td><img src='a.png'></td><td><img src=b.png></td></tr></table>"
            ),
            "![](<a.png>) ![](<b.png>) "
        );
        let picture = "<picture>\n<source media=\"(prefers-color-scheme: dark)\" srcset=\"d.png\">\n<img alt=\"Shot\" src=\"l.png\">\n</picture>";
        assert_eq!(without_html(picture), "![Shot](<l.png>)");
        // In a picture on one line, a screenshot still shows.
        assert_eq!(
            without_html("<picture><img src=\"l.png\" width=\"600\"></picture>"),
            "![](<l.png>)"
        );
    }

    #[test]
    fn html_edge_cases_read_as_markdown() {
        // `<hr>` after text: not a heading underline.
        assert_eq!(
            without_html("Some text.\n<hr>\nMore."),
            "Some text.\n\n---\n\nMore."
        );
        assert_eq!(without_html("foo<hr>bar"), "foo\n\n---\n\nbar");
        // Superscripts and subscripts stay in their word.
        assert_eq!(
            without_html("x<sup>2</sup> and H<sub>2</sub>O"),
            "x2 and H2O"
        );
        // A bare `<A>` is a generic, not a link.
        assert_eq!(without_html("Result<A> on success"), "Result<A> on success");
        // Attributes: quoted `>`, single quotes, no quotes, and `data-alt`.
        assert_eq!(without_html("<span title=\"a > b\">word</span>"), "word ");
        assert_eq!(
            without_html("<a href='https://e.com'>docs</a> <a href=https://f.com>more</a>"),
            "[docs](<https://e.com>) [more](<https://f.com>)"
        );
        assert_eq!(
            without_html("<picture><img data-alt=\"No\" alt=\"Yes\" width=\"16\"></picture> 3"),
            "Yes 3"
        );
        // Code: entities decoded, backticks inside kept.
        assert_eq!(without_html("<code>Vec&lt;T&gt;</code>"), "`Vec<T>`");
        assert_eq!(without_html("<code>a`b</code>"), "``a`b``");
        assert_eq!(without_html("<kbd>`</kbd>"), "`` ` ``");
        // Crossed tags don't split a character.
        assert_eq!(without_html("<b>😀<i>x</b></i>"), "**😀x**");
        // Unclosed links and summaries don't nest without end.
        let many = "<a href=\"x\">".repeat(10_000);
        assert_eq!(without_html(&many), "");
        let nested = format!("{}t{}", "<a href=\"x\">".repeat(100), "</a>".repeat(100));
        assert!(without_html(&nested).contains('t'));
    }

    #[test]
    fn html_in_code_over_lines_stays() {
        // A code span over lines keeps its HTML.
        let span = "`before\n<b>x</b>\nafter`";
        assert_eq!(without_html(span), span);
        // So does a fenced block in a quote, and the quote's end closes it.
        let quoted = "> ```html\n> <b>x</b>\n> ```\n<b>y</b>";
        assert_eq!(without_html(quoted), "> ```html\n> <b>x</b>\n> ```\n**y**");
        assert_eq!(
            without_html("> ```\n> <b>x</b>\n<b>y</b>"),
            "> ```\n> <b>x</b>\n**y**"
        );
    }

    #[test]
    fn a_fence_indented_four_spaces_does_not_close() {
        let body = "```html\n    ```\n<b>x</b>\n```";
        assert_eq!(without_html(body), body);
    }

    #[test]
    fn a_closing_tag_in_a_code_span_is_text() {
        assert_eq!(
            without_html("<a href=\"x\">see `a\n</a>` end</a>"),
            "[see `a\n</a>` end](<x>)"
        );
        assert_eq!(
            without_html("<summary>`</summary>` x</summary>"),
            "**`</summary>` x**"
        );
        // In `<code>` a backtick is text.
        assert_eq!(without_html("<code>`</code> `b`"), "`` ` `` `b`");
        // Nor does it start a code span that hides the closing tag.
        assert_eq!(
            without_html("<a href=\"x\"><code>`</code></a> <code>`</code>"),
            "[`` ` ``](<x>) `` ` ``"
        );
    }

    #[test]
    fn code_in_an_attribute_is_text() {
        let md =
            without_html("<a href=\"x\"><span title=\"<code>\">docs</span></a><code>tail</code>");
        assert_eq!(md, "[docs](<x>)`tail`");
    }

    #[test]
    fn many_unclosed_code_tags_in_a_link_stay_fast() {
        let body = format!("<a href=\"x\">see {}</a>", "<code>".repeat(10_000));
        let start = std::time::Instant::now();
        let md = without_html(&body);
        assert!(md.starts_with("[see](<x>)"), "{md}");
        assert!(start.elapsed() < std::time::Duration::from_secs(2));
    }

    #[test]
    fn tags_over_lines_keep_their_meaning() {
        assert_eq!(
            without_html("<a href=\"https://example.com\">\ndocs\n</a>"),
            "[docs](<https://example.com>)"
        );
        assert_eq!(
            without_html("<strong>first\nsecond</strong>"),
            "**first\nsecond**"
        );
        assert_eq!(
            without_html("- a <a href=\"x\">two\n  lines</a>\n- b<hr>c"),
            "- a [two\n  lines](<x>)\n- b\n\n  ---\n\n  c"
        );
        // A picture over lines keeps its lines, so a link over lines next
        // to it keeps its destination.
        assert_eq!(
            without_html("<picture>\n<img src=\"l.png\" width=\"600\">\n</picture> <b>x</b>"),
            "![](<l.png>)\n**x**"
        );
        assert_eq!(
            without_html(
                "<a href=\"x\">first\nsecond</a>\n<picture>\n<img src=\"shot.png\">\n</picture>"
            ),
            "[first\nsecond](<x>)\n![](<shot.png>)"
        );
        assert_eq!(
            without_html(
                "<strong><picture>\n<img src=\"i.svg\" width=\"16\">\n</picture> Risk</strong>"
            ),
            "**Risk**"
        );
        // In a block quote or a list, the lines stay in it.
        assert_eq!(
            without_html("> <picture>\n> <img src=\"s.png\">\n> </picture> # caption"),
            "> ![](<s.png>)\n>  # caption"
        );
        assert_eq!(
            without_html("- a <b>x</b>\n  <picture>\n  <img src=\"s.png\">\n  </picture> # c"),
            "- a **x**\n  ![](<s.png>)\n   # c"
        );
        // HTML code with a code span over lines in it stays one span.
        assert_eq!(without_html("<code>x `a\nb` y</code>"), "``x `a\nb` y``");
        // A bold around a code span over lines stays bold.
        assert_eq!(
            without_html("<strong>first `a\nb` last</strong>"),
            "**first `a\nb` last**"
        );
        assert_eq!(
            without_html("<a href=\"x\">see `a\n<b>b</b>`</a>"),
            "[see `a\n<b>b</b>`](<x>)"
        );
    }

    #[test]
    fn html_lists_headings_and_blocks_read_as_markdown() {
        // Release notes as Dependabot writes them (shortened).
        let body = concat!(
            "Bumps x11rb from 0.13.2 to 0.14.0.\n",
            "<details>\n",
            "<summary>Changelog</summary>\n",
            "<p><em>Sourced from <a href=\"https://c\">x11rb's changelog</a>.</em></p>\n",
            "<blockquote>\n",
            "<h1>Version 0.14.0</h1>\n",
            "<p>Fixes:</p>\n",
            "<ul>\n",
            "<li>Fix <code>CUT_BUFFER0</code> names. The code previously\n",
            "used <code>CUT_BUFFE_R0</code>.</li>\n",
            "<li>Add <code>Foo::bar</code></li>\n",
            "</ul>\n",
            "</blockquote>\n",
            "</details>",
        );
        assert_eq!(
            without_html(body),
            concat!(
                "Bumps x11rb from 0.13.2 to 0.14.0.\n",
                "**Changelog**\n\n",
                "*Sourced from [x11rb's changelog](<https://c>).*\n\n",
                "# Version 0.14.0\n\n",
                "Fixes:\n\n",
                "- Fix `CUT_BUFFER0` names. The code previously\n",
                "used `CUT_BUFFE_R0`.\n",
                "- Add `Foo::bar`",
            )
        );
        // Blocks, not one paragraph.
        use pulldown_cmark::{Event, Parser, Tag};
        let md = without_html(body);
        let starts: Vec<&str> = Parser::new(&md)
            .filter_map(|e| match e {
                Event::Start(Tag::Paragraph) => Some("p"),
                Event::Start(Tag::Heading { .. }) => Some("h"),
                Event::Start(Tag::Item) => Some("li"),
                _ => None,
            })
            .collect();
        assert_eq!(starts, ["p", "p", "h", "p", "li", "li"], "{md}");
        // On one line, nested, and numbered from `start`.
        assert_eq!(
            without_html("<ul><li>a<ul><li>b</li></ul></li><li>c</li></ul>after"),
            "- a\n  - b\n- c\n\nafter"
        );
        assert_eq!(
            without_html("<ol start=\"3\"><li>x</li><li>y</li></ol>"),
            "3. x\n4. y"
        );
        let last = format!("<ol start=\"{}\"><li>x</li><li>y</li></ol>", u32::MAX);
        assert!(without_html(&last).ends_with(". y"));
        assert_eq!(
            without_html("<h2>Title <code>x</code></h2>Text"),
            "## Title `x`\n\nText"
        );
        // A block in a list item stays in it, and so do the lines after.
        assert_eq!(without_html("<ul><li><p>a</p><p>b</p></li></ul>"), "- a b");
        assert_eq!(without_html("- <p>a</p>b"), "- a\n\n  b");
        assert_eq!(without_html("> <p>a</p><p>b</p>"), "> a\n>\n> b");
        // In a link or a summary a block is only a gap.
        assert_eq!(
            without_html("<a href=\"x\"><h3>docs</h3></a>"),
            "[docs](<x>)"
        );
        // A table row stays one line.
        assert_eq!(without_html("| <p>a</p> | b |"), "|  a  | b |");
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
    fn the_pilot_tab_goes_with_the_board() {
        assert_eq!(listed_tab(Tab::Pilot, true), Tab::Pilot);
        assert_eq!(listed_tab(Tab::Pilot, false), Tab::Timeline);
        assert_eq!(listed_tab(Tab::Files, false), Tab::Files);
    }

    #[test]
    fn keys_in_the_list_move_without_opening() {
        assert!(!opens_on_step(false));
        assert!(opens_on_step(true));
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

    #[test]
    fn a_comment_goes_out_or_says_why() {
        let item = |repo: &str, number, kind| ItemRef {
            repo: repo.into(),
            number,
            kind,
        };
        let a = item("o/r", 1, ItemKind::Pull);
        let b = item("o/r", 2, ItemKind::Pull);
        let send = |text, typed_for, busy| comment_send(text, Some(&a), typed_for, busy, None);
        assert_eq!(send("hi", Some(&a), None), CommentSend::Post);
        // A queue refresh may leave the same pull request read again, under
        // another case or kind: still the one shown.
        let again = item("O/R", 1, ItemKind::Issue);
        assert_eq!(send("hi", Some(&again), None), CommentSend::Post);
        // Typed before anything was picked: nothing else would keep it.
        assert_eq!(send("hi", None, None), CommentSend::Post);
        assert_eq!(send(" /merge", Some(&a), None), CommentSend::Command);
        assert_eq!(send("  ", Some(&a), None), CommentSend::Empty);
        assert_eq!(
            send("hi", Some(&a), Some("merge")),
            CommentSend::Busy("merge")
        );
        assert_eq!(
            send("hi", Some(&b), None),
            CommentSend::Elsewhere(b.clone())
        );
        assert_eq!(
            comment_send("hi", None, Some(&a), None, None),
            CommentSend::NoItem
        );
        // A second Enter on the comment being posted: it is the one sent.
        let sending = |text, on| comment_send(text, Some(&a), Some(&a), Some("comment"), on);
        assert_eq!(sending("hi ", Some((&again, "hi"))), CommentSend::Sending);
        assert_eq!(
            sending("other", Some((&a, "hi"))),
            CommentSend::Busy("comment")
        );
        assert_eq!(
            sending("hi", Some((&b, "hi"))),
            CommentSend::Busy("comment")
        );
        // Another write: the same text was posted earlier, not now.
        assert_eq!(
            comment_send("hi", Some(&a), Some(&a), Some("merge"), Some((&a, "hi"))),
            CommentSend::Busy("merge")
        );
        // Read again while the comment was sent: the input still clears.
        assert!(same_item(Some(&a), Some(&again)));
        assert!(!same_item(Some(&a), Some(&b)));
        assert!(!same_item(Some(&a), None));
        assert!(same_item(None, None));
        assert!(busy_note("comment").contains("the last comment"));
        assert!(busy_note("merge").contains("wasn't sent"));
    }
}
