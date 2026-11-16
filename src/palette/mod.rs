//! Ctrl+K: one place to jump to anything, run any command, change a setting,
//! ask an agent or get a quick answer. The shell hands it what it knows
//! (`Seed`); the palette searches it with the project's files, symbols and
//! memory, ranks by match and by what the person opens (frecency), and tells
//! the shell what to do through `PaletteEvent`.
//!
//! Pure parts live beside it with their tests: `rank` (fuzzy match,
//! frecency), `query` (prefixes and chips), `clip` (clipboard), `pins`,
//! `rules` (automations from a sentence), `sources` (files, symbols, memory),
//! `actions` (what → offers), `answer` (quick answers).

pub mod actions;
pub mod answer;
pub mod clip;
pub mod item;
pub mod pins;
pub mod preview;
pub mod query;
pub mod rank;
pub mod rules;
pub mod sources;
pub mod todos;
mod view;

use std::collections::{HashMap, HashSet};
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use gpui::{
    App, AppContext as _, ClipboardItem, Context, Entity, EventEmitter, FocusHandle, Focusable,
    ScrollHandle, SharedString, Subscription, Task, Window,
};

use crate::input::{InputEvent, TextInput};
use crate::memory::Scope as MemScope;
use crate::ui::Tint;
pub use item::{Item, Kind, Lead, Preview, Tab, Target};
use query::{Chip, Lead as QueryLead, Scope};

gpui::actions!(
    palette,
    [
        MarkRow, TogglePin, PinUp, PinDown, Pinned1, Pinned2, Pinned3, Pinned4, Pinned5, Pinned6,
        Pinned7, Pinned8, Pinned9
    ]
);

/// The palette's keys, in its own context so they win over the shell's.
pub fn bind_keys(cx: &mut App) {
    use crate::platform::keystroke;
    use gpui::KeyBinding;
    let ctx = Some("Palette");
    cx.bind_keys([
        KeyBinding::new("space", MarkRow, ctx),
        KeyBinding::new(&keystroke("ctrl-p"), TogglePin, ctx),
        KeyBinding::new("alt-up", PinUp, ctx),
        KeyBinding::new("alt-down", PinDown, ctx),
        KeyBinding::new(&keystroke("ctrl-1"), Pinned1, ctx),
        KeyBinding::new(&keystroke("ctrl-2"), Pinned2, ctx),
        KeyBinding::new(&keystroke("ctrl-3"), Pinned3, ctx),
        KeyBinding::new(&keystroke("ctrl-4"), Pinned4, ctx),
        KeyBinding::new(&keystroke("ctrl-5"), Pinned5, ctx),
        KeyBinding::new(&keystroke("ctrl-6"), Pinned6, ctx),
        KeyBinding::new(&keystroke("ctrl-7"), Pinned7, ctx),
        KeyBinding::new(&keystroke("ctrl-8"), Pinned8, ctx),
        KeyBinding::new(&keystroke("ctrl-9"), Pinned9, ctx),
    ]);
}

/// A file next to the state file (or in the data dir), the way the other
/// stores find theirs, so a test instance keeps its own.
pub fn store_path(name: &str) -> PathBuf {
    match std::env::var_os("ARAKUN_STATE").map(PathBuf::from) {
        Some(state) => state.with_file_name(name),
        None => crate::state::data_dir().join(name),
    }
}

/// Numbered lines of a file and how many lines it has.
pub type Snippet = (Vec<(u32, String)>, usize);

/// What the shell knows when the palette opens.
#[derive(Default)]
pub struct Seed {
    pub items: Vec<Item>,
    /// The visible environment's directory: files, symbols and memory come from it.
    pub root: Option<PathBuf>,
    pub project: String,
    pub branch: String,
    /// `owner/name` of the visible project on GitHub.
    pub repo: Option<String>,
    /// (project name, `owner/name`) of every project on GitHub: where `#86`
    /// is looked for, and what `#arakun 86` narrows to.
    pub repos: Vec<(String, String)>,
    pub branches: Vec<String>,
    /// (id, title) of every session, for "when X finishes".
    pub sessions: Vec<(String, String)>,
    pub read_only: bool,
    /// The head commit GitHub last showed for each pull request read in full.
    /// A merge from here is pinned to the one the confirmation shows.
    pub heads: HashMap<crate::github::pr::ItemRef, String>,
    /// The agent new sessions start with, as it reads: "Opus 5.5".
    pub agent: String,
    /// The latest failed run per branch.
    pub failures: Vec<Failure>,
    pub clipboard: Option<String>,
    /// The local hour, for what the person opens at this time of day.
    pub hour: usize,
}

/// A failed CI run the palette can offer as context.
#[derive(Clone, Debug, Default)]
pub struct Failure {
    pub branch: String,
    /// `relay CI #4812 failed 1h ago`.
    pub what: String,
    /// The end of the failing step's log.
    pub log: Vec<String>,
    pub root: PathBuf,
    pub id: u64,
}

pub enum PaletteEvent {
    /// Do this. `keep` leaves the palette open (a setting turned in place).
    Run {
        target: Target,
        keep: bool,
    },
    /// Several marked rows at once.
    Bulk {
        op: Bulk,
        targets: Vec<Target>,
    },
    /// Tab on an answer: a session that starts from the question and the answer.
    KeepChatting {
        question: String,
        answer: String,
    },
    /// A confirmed automation.
    AddRule(rules::Draft),
    /// A confirmed merge from the palette, of the head commit the
    /// confirmation showed: a push since then is not merged unseen.
    Merge {
        item: crate::github::pr::ItemRef,
        head: String,
    },
    PinsChanged,
    /// `#86` with no such pull request listed: look it up in these repositories.
    FindPull {
        number: u64,
        repos: Vec<String>,
    },
    Dismissed,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Bulk {
    Open,
    Pin,
    Copy,
    Summarize,
}

/// A drill-down page: the palette shows what is inside one thing.
#[derive(Clone, Debug, PartialEq)]
pub enum Page {
    Project { index: usize, name: String },
    Person { login: String },
    Branches { project: String },
}

impl Page {
    fn crumbs(&self) -> Vec<(Option<&'static str>, String)> {
        match self {
            Page::Project { name, .. } => vec![(Some("folder01"), name.clone())],
            Page::Person { login } => vec![(Some("user"), login.clone())],
            Page::Branches { project } => vec![
                (None, project.clone()),
                (Some("git-branch"), "Branches".into()),
            ],
        }
    }

    fn placeholder(&self) -> String {
        match self {
            Page::Project { name, .. } => format!("filter {name}…"),
            Page::Person { login } => format!("filter {login}'s pull requests…"),
            Page::Branches { .. } => "filter branches…".into(),
        }
    }
}

/// A card made for what was typed.
#[derive(Clone, Debug, PartialEq)]
pub enum Card {
    Clip(clip::Clip),
    Ask,
    Remember(sources::MemoryHit),
    Answer,
    /// A question: Enter sends it for a quick answer. Nothing goes out before.
    OfferAnswer,
    /// A sentence the parser could not read as a rule: Enter has the model read it.
    OfferRule,
    Rule(rules::Draft),
    /// "to-do: …": Enter adds it.
    Todo(String),
    /// A merge to confirm: the pull request, its title and the head commit
    /// that Enter merges.
    Confirm(crate::github::pr::ItemRef, String, String),
}

/// The head commit a merge from the palette is pinned to: the one read when
/// it opened. None when it wasn't read, so nothing is merged.
fn head_to_merge(
    heads: &HashMap<crate::github::pr::ItemRef, String>,
    item: &crate::github::pr::ItemRef,
) -> Option<String> {
    heads.get(item).filter(|h| !h.is_empty()).cloned()
}

/// A commit as the confirmation names it: its first seven characters.
fn short_head(head: &str) -> &str {
    head.get(..7).unwrap_or(head)
}

/// A row as shown: its item, what matched, and the card it is if it is one.
#[derive(Clone, Debug)]
pub struct Entry {
    pub item: Item,
    pub ranges: Vec<Range<usize>>,
    pub card: Option<Card>,
}

/// What the list draws, in order.
#[derive(Clone, Debug)]
pub enum Row {
    Section {
        label: SharedString,
        count: Option<String>,
    },
    Pins,
    Entry(usize),
}

/// The open actions menu (→).
pub struct Menu {
    pub key: String,
    pub title: String,
    pub actions: Vec<actions::Action>,
    pub selected: usize,
}

pub struct Palette {
    /// (`owner/name`, number) already looked up for `#86`, so each is asked once.
    asked_pulls: HashSet<(String, u64)>,
    input: Entity<TextInput>,
    seed: Seed,
    files: Option<Arc<Vec<String>>>,
    symbols: Option<Arc<Vec<sources::Symbol>>>,
    indexing: bool,
    memory: Vec<(MemScope, crate::memory::Store)>,
    /// The words of each memory store, read once: searching memory runs on every keystroke.
    memory_docs: Vec<Vec<Vec<String>>>,
    /// Read off the UI thread: typing must never wait on the disk. The render
    /// path and `refilter` only look these up; a miss starts a read.
    line_counts: HashMap<String, usize>,
    counting: HashSet<String>,
    snippets: HashMap<(PathBuf, Option<u32>), Option<Snippet>>,
    reading: HashSet<(PathBuf, Option<u32>)>,
    frecency: rank::Frecency,
    pub pins: pins::Pins,
    todos: todos::Todos,
    clip: Option<clip::Clip>,
    scope: Option<Scope>,
    chips: Vec<Chip>,
    pages: Vec<Page>,
    tab: Tab,
    tabs: Vec<(Tab, usize)>,
    entries: Vec<Entry>,
    rows: Vec<Row>,
    selected: usize,
    /// Arrows moved the selection since the last keystroke: Space marks.
    nav: bool,
    marked: HashSet<String>,
    menu: Option<Menu>,
    answer: Option<answer::Answer>,
    answer_task: Option<Task<()>>,
    /// A rule the small model read from a sentence the parser could not: (sentence, rule).
    model_rule: Option<(String, rules::Draft)>,
    rule_task: Option<Task<()>>,
    /// The sentence the model is reading as a rule, while it reads.
    reading_rule: Option<String>,
    /// A merge waiting for Enter.
    confirm: Option<(crate::github::pr::ItemRef, String, String)>,
    /// A line under the list after something ran: "Copied", "Read-only…".
    note: Option<(String, bool)>,
    scroll: ScrollHandle,
    /// Re-entrancy guard: the input changes while leads are taken off it.
    taking: bool,
    _sub: Subscription,
}

impl EventEmitter<PaletteEvent> for Palette {}

impl Palette {
    pub fn new(seed: Seed, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let input = cx.new(|cx| {
            TextInput::new("Search, run a command or ask an agent…", cx)
                .caret_color(crate::theme::accent())
        });
        let sub = cx.subscribe_in(&input, window, |this: &mut Self, _, ev, window, cx| {
            this.on_input(*ev, window, cx)
        });
        window.focus(&input.focus_handle(cx), cx);
        let clip = seed.clipboard.as_deref().and_then(clip::classify);
        let mut this = Self {
            asked_pulls: HashSet::new(),
            input,
            seed,
            files: None,
            symbols: None,
            indexing: false,
            memory: Vec::new(),
            memory_docs: Vec::new(),
            line_counts: HashMap::new(),
            counting: HashSet::new(),
            snippets: HashMap::new(),
            reading: HashSet::new(),
            frecency: rank::Frecency::load(),
            pins: pins::Pins::load(),
            todos: {
                let mut t = todos::Todos::load();
                t.tidy(crate::memory::now());
                t
            },
            clip,
            scope: None,
            chips: Vec::new(),
            pages: Vec::new(),
            tab: Tab::All,
            tabs: Vec::new(),
            entries: Vec::new(),
            rows: Vec::new(),
            selected: 0,
            nav: false,
            marked: HashSet::new(),
            menu: None,
            answer: None,
            answer_task: None,
            model_rule: None,
            rule_task: None,
            reading_rule: None,
            confirm: None,
            note: None,
            scroll: ScrollHandle::new(),
            taking: false,
            _sub: sub,
        };
        this.load_files(cx);
        this.load_memory(cx);
        this.refilter(cx);
        this.selected = this.default_selection();
        this
    }

    /// Fresh rows from the shell (a setting changed, GitHub answered).
    pub fn set_items(&mut self, items: Vec<Item>, cx: &mut Context<Self>) {
        let key = self.selected_key();
        self.seed.items = items;
        self.refilter(cx);
        if let Some(i) = key.and_then(|k| self.entry_index(&k)) {
            self.selected = i;
        }
        cx.notify();
    }

    /// The projects' repositories, once the shell has read them.
    pub fn set_repos(
        &mut self,
        repo: Option<String>,
        repos: Vec<(String, String)>,
        items: Vec<Item>,
        cx: &mut Context<Self>,
    ) {
        self.seed.repo = repo;
        self.seed.repos = repos;
        self.set_items(items, cx);
    }

    /// Shown under the list: what happened after a write.
    pub fn show_note(&mut self, text: impl Into<String>, ok: bool, cx: &mut Context<Self>) {
        self.note = Some((text.into(), ok));
        cx.notify();
    }

    fn load_files(&mut self, cx: &mut Context<Self>) {
        let Some(root) = self.seed.root.clone() else {
            return;
        };
        cx.spawn(async move |this, cx| {
            let files = cx
                .background_spawn(async move { sources::list_files(&root) })
                .await;
            let _ = this.update(cx, |this, cx| {
                this.files = Some(Arc::new(files));
                this.refilter(cx);
                if !this.nav {
                    this.selected = this.default_selection();
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Memory stores can be large: read and index them off the UI thread.
    fn load_memory(&mut self, cx: &mut Context<Self>) {
        let Some(root) = self.seed.root.clone() else {
            return;
        };
        cx.spawn(async move |this, cx| {
            let (memory, docs) = cx
                .background_spawn(async move {
                    let memory = sources::memory_stores(&root);
                    let docs: Vec<_> = memory
                        .iter()
                        .map(|(_, s)| crate::memory::index(&s.items))
                        .collect();
                    (memory, docs)
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                this.memory = memory;
                this.memory_docs = docs;
                this.refilter_keep(cx);
                cx.notify();
            });
        })
        .detach();
    }

    fn index_symbols(&mut self, cx: &mut Context<Self>) {
        if self.indexing || self.symbols.is_some() {
            return;
        }
        let (Some(root), Some(files)) = (self.seed.root.clone(), self.files.clone()) else {
            return;
        };
        self.indexing = true;
        cx.spawn(async move |this, cx| {
            let symbols = cx
                .background_spawn(async move { sources::index_symbols(&root, &files) })
                .await;
            let _ = this.update(cx, |this, cx| {
                this.symbols = Some(Arc::new(symbols));
                this.indexing = false;
                this.refilter(cx);
                if !this.nav {
                    this.selected = this.default_selection();
                }
                cx.notify();
            });
        })
        .detach();
    }

    // ---- input ---------------------------------------------------------------------------

    fn on_input(&mut self, ev: InputEvent, window: &mut Window, cx: &mut Context<Self>) {
        match ev {
            InputEvent::Changed => {
                if self.taking {
                    return;
                }
                self.nav = false;
                self.note = None;
                self.take_leads(cx);
                self.menu = None;
                self.confirm = None;
                self.refilter(cx);
                self.selected = self.default_selection();
                self.scroll.scroll_to_item(0);
                self.drop_stale_requests(cx);
                cx.notify();
            }
            InputEvent::Up => self.step(-1, cx),
            InputEvent::Down => self.step(1, cx),
            InputEvent::Submit => self.submit(window, cx),
            InputEvent::SubmitAlt => self.ask_agent(cx),
            InputEvent::Cancel => self.escape(cx),
            InputEvent::Complete => self.tab_key(cx),
        }
    }

    /// Moves a typed scope character or a finished chip word off the input.
    fn take_leads(&mut self, cx: &mut Context<Self>) {
        let mut text = self.input.read(cx).text().to_string();
        let mut changed = false;
        if self.scope == Some(Scope::People)
            && self.pages.is_empty()
            && let Some((chip, rest)) = query::model_in_people(&text)
        {
            // `@opus ` typed key by key: the `@` set People before the word.
            self.scope = None;
            if !self.chips.contains(&chip) {
                self.chips.push(chip);
            }
            text = rest;
            changed = true;
        }
        while let Some((lead, rest)) = query::take(
            &text,
            self.scope.is_some() || !self.pages.is_empty(),
            &self.seed.branches,
        ) {
            match lead {
                QueryLead::Scope(s) => {
                    self.scope = Some(s);
                    self.tab = Tab::All;
                }
                QueryLead::Chip(c) => {
                    if !self.chips.contains(&c) {
                        self.chips.push(c);
                    }
                }
            }
            text = rest;
            changed = true;
        }
        if changed {
            self.taking = true;
            self.input.update(cx, |i, cx| i.set_text(text, cx));
            self.taking = false;
        }
    }

    fn query(&self, cx: &App) -> String {
        self.input.read(cx).text().trim().to_string()
    }

    fn step(&mut self, delta: isize, cx: &mut Context<Self>) {
        if let Some(menu) = &mut self.menu {
            let n = menu.actions.len() as isize;
            if n > 0 {
                menu.selected = ((menu.selected as isize + delta).rem_euclid(n)) as usize;
            }
            cx.notify();
            return;
        }
        let n = self.entries.len() as isize;
        if n == 0 {
            return;
        }
        self.nav = true;
        self.selected = ((self.selected as isize + delta).rem_euclid(n)) as usize;
        if let Some(row) = self
            .rows
            .iter()
            .position(|r| matches!(r, Row::Entry(i) if *i == self.selected))
        {
            // The section label above the first entry scrolls in with it.
            self.scroll
                .scroll_to_item(if self.selected == 0 { 0 } else { row });
        }
        cx.notify();
    }

    /// The row Enter takes with nothing moved: the first result, not a card,
    /// unless the card is the point (a rule or a merge to confirm, or an
    /// offer to ask the model, which sends nothing until that Enter).
    fn default_selection(&self) -> usize {
        if self.entries.first().is_some_and(|e| {
            matches!(
                e.card,
                Some(
                    Card::Rule(_)
                        | Card::Todo(_)
                        | Card::Confirm(..)
                        | Card::Clip(clip::Clip::Error(_))
                        | Card::OfferAnswer
                        | Card::OfferRule
                )
            )
        }) {
            return 0;
        }
        self.entries
            .iter()
            .position(|e| e.card.is_none())
            .unwrap_or(0)
    }

    fn selected_entry(&self) -> Option<&Entry> {
        self.entries.get(self.selected)
    }

    fn selected_key(&self) -> Option<String> {
        self.selected_entry().map(|e| e.item.key.clone())
    }

    fn entry_index(&self, key: &str) -> Option<usize> {
        self.entries.iter().position(|e| e.item.key == key)
    }

    fn submit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let _ = window;
        if let Some(menu) = self.menu.take() {
            if let Some(a) = menu.actions.get(menu.selected) {
                self.run_action(&menu.key, a.what.clone(), cx);
            }
            cx.notify();
            return;
        }
        if !self.marked.is_empty() {
            self.bulk(Bulk::Open, cx);
            return;
        }
        if let Some(i) = Some(self.selected) {
            self.pick(i, cx);
        }
    }

    /// Enter on the entry at `i`.
    pub fn pick(&mut self, i: usize, cx: &mut Context<Self>) {
        let Some(entry) = self.entries.get(i).cloned() else {
            return;
        };
        match &entry.card {
            Some(Card::Answer) => {
                // Enter on the answer itself: copy it.
                self.copy_answer(cx);
                return;
            }
            Some(Card::OfferAnswer) => {
                self.start_answer(cx);
                return;
            }
            // The same as Alt+Enter: the row says "Ask … about".
            Some(Card::Ask) => {
                self.ask_agent(cx);
                return;
            }
            Some(Card::OfferRule) => {
                self.start_rule(cx);
                return;
            }
            Some(Card::Rule(d)) => {
                cx.emit(PaletteEvent::AddRule(d.clone()));
                return;
            }
            Some(Card::Todo(text)) => {
                self.todos.add(text, crate::memory::now());
                self.todos.save();
                self.input.update(cx, |i, cx| i.set_text("", cx));
                self.show_note("Added to your to-dos", true, cx);
                return;
            }
            Some(Card::Confirm(item, _, head)) => {
                cx.emit(PaletteEvent::Merge {
                    item: item.clone(),
                    head: head.clone(),
                });
                self.confirm = None;
                self.refilter(cx);
                return;
            }
            _ => {}
        }
        match &entry.item.target {
            Target::Todo(id) => {
                let done = self.todos.toggle(*id, crate::memory::now());
                self.todos.save();
                self.refilter_keep(cx);
                self.show_note(if done { "Done" } else { "Back on your list" }, true, cx);
                return;
            }
            Target::Setting(_) => {
                cx.emit(PaletteEvent::Run {
                    target: entry.item.target.clone(),
                    keep: true,
                });
                return;
            }
            Target::Merge(item) => {
                self.ask_merge(item.clone(), entry.item.title.to_string(), cx);
                return;
            }
            Target::Copy(text) => {
                cx.write_to_clipboard(ClipboardItem::new_string(text.clone()));
                self.show_note("Copied", true, cx);
                return;
            }
            _ => {}
        }
        // A row with nowhere else to go (a person) opens its page.
        if entry.item.target == Target::Nothing
            && let Some(page) = self.page_of(&entry.item)
        {
            self.enter(page, cx);
            return;
        }
        if entry.item.target == Target::Nothing {
            return;
        }
        self.record(&entry.item.key, cx);
        cx.emit(PaletteEvent::Run {
            target: entry.item.target.clone(),
            keep: false,
        });
    }

    fn record(&mut self, key: &str, cx: &mut Context<Self>) {
        if key.is_empty() || key.starts_with("card:") {
            return;
        }
        self.frecency
            .record(key, crate::memory::now(), self.seed.hour);
        let f = self.frecency.clone();
        cx.background_spawn(async move { f.save() }).detach();
    }

    fn ask_merge(
        &mut self,
        item: crate::github::pr::ItemRef,
        title: String,
        cx: &mut Context<Self>,
    ) {
        if self.seed.read_only {
            self.show_note(
                format!(
                    "Did not merge #{}: GitHub is read-only in this Arakun, so nothing was sent.",
                    item.number
                ),
                false,
                cx,
            );
            return;
        }
        // The head is read now, so the confirmation can show what Enter
        // merges. Unread, nothing is offered to merge.
        let Some(head) = head_to_merge(&self.seed.heads, &item) else {
            self.show_note(
                format!(
                    "Did not merge #{}: its latest commit isn't loaded yet; open it in the GitHub tab.",
                    item.number
                ),
                false,
                cx,
            );
            return;
        };
        self.confirm = Some((item, title, head));
        self.menu = None;
        self.refilter(cx);
        self.selected = 0;
        cx.notify();
    }

    fn escape(&mut self, cx: &mut Context<Self>) {
        if self.menu.take().is_some() {
        } else if self.confirm.take().is_some() {
            self.refilter(cx);
        } else if !self.marked.is_empty() {
            self.marked.clear();
        } else if !self.pages.is_empty() && self.query(cx).is_empty() {
            self.back(cx);
        } else {
            cx.emit(PaletteEvent::Dismissed);
        }
        cx.notify();
    }

    /// Tab: keep chatting from an answer, else the next filter tab.
    fn tab_key(&mut self, cx: &mut Context<Self>) {
        if let Some(a) = &self.answer
            && !a.text.is_empty()
        {
            cx.emit(PaletteEvent::KeepChatting {
                question: a.question.clone(),
                answer: a.text.clone(),
            });
            return;
        }
        if self.tabs.len() > 1 {
            let at = self
                .tabs
                .iter()
                .position(|(t, _)| *t == self.tab)
                .unwrap_or(0);
            self.tab = self.tabs[(at + 1) % self.tabs.len()].0;
            self.refilter(cx);
            self.selected = self.default_selection();
            cx.notify();
        }
    }

    pub fn set_tab(&mut self, tab: Tab, cx: &mut Context<Self>) {
        self.tab = tab;
        self.refilter(cx);
        self.selected = self.default_selection();
        cx.notify();
    }

    /// → : into a page, or the actions of the row.
    fn right(&mut self, cx: &mut Context<Self>) -> bool {
        if !self.input.read(cx).caret_at_end() {
            return false;
        }
        if let Some(menu) = self.menu.take() {
            if let Some(a) = menu.actions.get(menu.selected) {
                self.run_action(&menu.key, a.what.clone(), cx);
            }
            cx.notify();
            return true;
        }
        let Some(entry) = self.selected_entry().cloned() else {
            return false;
        };
        if let Some(Card::Rule(_)) = entry.card {
            // "edit the rule": back to the words.
            return false;
        }
        if actions::drills(&entry.item)
            && let Some(page) = self.page_of(&entry.item)
        {
            self.enter(page, cx);
            return true;
        }
        self.open_menu(&entry.item, cx);
        true
    }

    pub fn open_menu(&mut self, item: &Item, cx: &mut Context<Self>) {
        let pinned = self.pins.has(&item.key);
        self.menu = Some(Menu {
            key: item.key.clone(),
            title: match &item.target {
                Target::Run { .. } => item.title.to_string(),
                _ => item.title.to_string(),
            },
            actions: actions::for_item(item, pinned, self.seed.read_only),
            selected: 0,
        });
        cx.notify();
    }

    fn left(&mut self, cx: &mut Context<Self>) -> bool {
        if self.menu.take().is_some() {
            cx.notify();
            return true;
        }
        if !self.pages.is_empty() && self.query(cx).is_empty() {
            self.back(cx);
            return true;
        }
        false
    }

    /// ⌫ on an empty input: up a page, else the last chip, else the scope.
    fn backspace_empty(&mut self, cx: &mut Context<Self>) -> bool {
        if !self.input.read(cx).text().is_empty() {
            return false;
        }
        if !self.pages.is_empty() {
            self.back(cx);
        } else if self.chips.pop().is_some() || self.scope.take().is_some() {
            self.refilter(cx);
        } else {
            return false;
        }
        cx.notify();
        true
    }

    fn run_action(&mut self, key: &str, what: actions::Do, cx: &mut Context<Self>) {
        match what {
            actions::Do::Pin => {
                if let Some(e) = self.entries.iter().find(|e| e.item.key == key).cloned() {
                    self.toggle_pin(&e.item, cx);
                }
            }
            actions::Do::Go(target) => match target {
                Target::Copy(text) => {
                    cx.write_to_clipboard(ClipboardItem::new_string(text));
                    self.show_note("Copied", true, cx);
                }
                Target::Merge(item) => {
                    let title = self
                        .entries
                        .iter()
                        .find(|e| e.item.key == key)
                        .map(|e| e.item.title.to_string())
                        .unwrap_or_default();
                    self.ask_merge(item, title, cx);
                }
                target => {
                    self.record(key, cx);
                    cx.emit(PaletteEvent::Run {
                        keep: matches!(target, Target::Setting(_)),
                        target,
                    })
                }
            },
        }
    }

    // ---- pages ---------------------------------------------------------------------------

    fn page_of(&self, item: &Item) -> Option<Page> {
        match (&item.kind, &item.target) {
            (Kind::Project, Target::Project(i)) => Some(Page::Project {
                index: *i,
                name: item.title.to_string(),
            }),
            (Kind::Person, _) => Some(Page::Person {
                login: item.key.trim_start_matches("person:").to_string(),
            }),
            (_, Target::Branches) => Some(Page::Branches {
                project: self.seed.project.clone(),
            }),
            _ => None,
        }
    }

    fn enter(&mut self, page: Page, cx: &mut Context<Self>) {
        self.pages.push(page);
        self.after_page(cx);
    }

    fn back(&mut self, cx: &mut Context<Self>) {
        self.pages.pop();
        self.after_page(cx);
    }

    fn after_page(&mut self, cx: &mut Context<Self>) {
        self.menu = None;
        self.marked.clear();
        self.tab = Tab::All;
        self.scope = None;
        self.taking = true;
        self.input.update(cx, |i, cx| i.set_text("", cx));
        self.taking = false;
        self.refilter(cx);
        self.selected = self.default_selection();
        cx.notify();
    }

    fn in_page(&self, page: &Page, item: &Item) -> bool {
        match page {
            Page::Project { index, .. } => matches!(
                item.target,
                Target::Env(p, _) | Target::Session(p, _, _) if p == *index
            ),
            Page::Person { login } => matches!(
                &item.preview,
                Preview::Pull { author, .. } if author.eq_ignore_ascii_case(login)
            ),
            Page::Branches { .. } => item.kind == Kind::Branch,
        }
    }

    // ---- pins and marks ------------------------------------------------------------------

    fn toggle_pin(&mut self, item: &Item, cx: &mut Context<Self>) {
        if item.kind == Kind::Card || item.key.is_empty() {
            return;
        }
        let pinned = self.pins.toggle(pins::Pin {
            kind: item.key.split(':').next().unwrap_or("").to_string(),
            id: item
                .key
                .split_once(':')
                .map(|(_, id)| id.to_string())
                .unwrap_or_default(),
            label: item.title.to_string(),
        });
        self.pins.save();
        self.show_note(
            if pinned {
                format!(
                    "Pinned “{}”: Ctrl+{} opens it",
                    item.title,
                    self.pins.items.len()
                )
            } else if self.pins.items.len() >= pins::MAX && !self.pins.has(&item.key) {
                "Nine pins at most: unpin one first".into()
            } else {
                format!("Unpinned “{}”", item.title)
            },
            true,
            cx,
        );
        cx.emit(PaletteEvent::PinsChanged);
        self.refilter(cx);
    }

    /// The row a pin opens, if it is still around.
    fn pinned_item(&self, n: usize) -> Option<Item> {
        let pin = self.pins.items.get(n)?;
        self.seed
            .items
            .iter()
            .find(|i| i.key == pin.key())
            .cloned()
            .or_else(|| {
                // Files pin by path; they are not in the seed.
                (pin.kind == "file").then(|| file_item(Path::new(&pin.id), pin.label.clone(), None))
            })
    }

    pub fn open_pinned(&mut self, n: usize, cx: &mut Context<Self>) {
        if let Some(item) = self.pinned_item(n) {
            self.record(&item.key, cx);
            cx.emit(PaletteEvent::Run {
                target: item.target,
                keep: false,
            });
        }
    }

    fn nudge_pin(&mut self, delta: isize, cx: &mut Context<Self>) {
        let Some(key) = self.selected_key() else {
            return;
        };
        if self.pins.nudge(&key, delta) {
            self.pins.save();
            cx.emit(PaletteEvent::PinsChanged);
            cx.notify();
        }
    }

    fn mark(&mut self, cx: &mut Context<Self>) -> bool {
        if !self.nav && self.marked.is_empty() {
            return false;
        }
        let Some(e) = self.selected_entry() else {
            return false;
        };
        if e.card.is_some() {
            return false;
        }
        let key = e.item.key.clone();
        if !self.marked.remove(&key) {
            self.marked.insert(key);
        }
        cx.notify();
        true
    }

    fn mark_all(&mut self, cx: &mut Context<Self>) -> bool {
        if self.marked.is_empty() {
            return false;
        }
        for e in &self.entries {
            if e.card.is_none() {
                self.marked.insert(e.item.key.clone());
            }
        }
        cx.notify();
        true
    }

    pub fn bulk(&mut self, op: Bulk, cx: &mut Context<Self>) {
        let picked: Vec<Item> = self
            .entries
            .iter()
            .filter(|e| self.marked.contains(&e.item.key))
            .map(|e| e.item.clone())
            .collect();
        match op {
            Bulk::Pin => {
                for item in &picked {
                    if !self.pins.has(&item.key) {
                        self.toggle_pin(item, cx);
                    }
                }
                self.marked.clear();
            }
            Bulk::Copy => {
                let text = picked
                    .iter()
                    .map(|i| match &i.target {
                        Target::Pull(r) => format!("{} {}", r.url(), i.title),
                        Target::File { path, .. } => path.display().to_string(),
                        _ => i.title.to_string(),
                    })
                    .collect::<Vec<_>>()
                    .join("\n");
                cx.write_to_clipboard(ClipboardItem::new_string(text));
                self.show_note(format!("Copied {}", picked.len()), true, cx);
                self.marked.clear();
            }
            Bulk::Open | Bulk::Summarize => {
                for i in &picked {
                    self.record(&i.key, cx);
                }
                cx.emit(PaletteEvent::Bulk {
                    op,
                    targets: picked.into_iter().map(|i| i.target).collect(),
                });
            }
        }
        cx.notify();
    }

    fn marked_kinds(&self) -> HashSet<Kind> {
        self.entries
            .iter()
            .filter(|e| self.marked.contains(&e.item.key))
            .map(|e| e.item.kind)
            .collect()
    }

    // ---- asking --------------------------------------------------------------------------

    fn model_chip(&self) -> Option<(String, String)> {
        self.chips.iter().find_map(|c| match c {
            Chip::Model { id, label } => Some((id.clone(), label.clone())),
            _ => None,
        })
    }

    fn branch_chip(&self) -> Option<String> {
        self.chips.iter().find_map(|c| match c {
            Chip::Branch(b) => Some(b.clone()),
            _ => None,
        })
    }

    /// The failed run on the branch the question is about, as context.
    fn failure(&self) -> Option<&Failure> {
        let branch = self
            .branch_chip()
            .unwrap_or_else(|| self.seed.branch.clone());
        self.seed.failures.iter().find(|f| f.branch == branch)
    }

    fn ask_target(&self, cx: &App) -> Target {
        let q = self.query(cx);
        let mut prompt = q.clone();
        if let Some(f) = self.failure()
            && self.branch_chip().is_some()
        {
            prompt = format!(
                "{q}\n\nContext: {}.\n```\n{}\n```",
                f.what,
                f.log.join("\n")
            );
        }
        Target::Ask {
            prompt,
            model: self.model_chip().map(|(id, _)| id),
            branch: self.branch_chip(),
        }
    }

    fn ask_agent(&mut self, cx: &mut Context<Self>) {
        if self.query(cx).is_empty() {
            return;
        }
        let target = self.ask_target(cx);
        cx.emit(PaletteEvent::Run {
            target,
            keep: false,
        });
    }

    /// Typing on stops a request made for the old text; nothing new starts
    /// until Enter on an offer, since a request sends the text, memory and
    /// CI logs to Anthropic.
    fn drop_stale_requests(&mut self, cx: &mut Context<Self>) {
        let q = self.query(cx);
        if self.answer.as_ref().is_some_and(|a| a.question != q) {
            // Dropping the task drops its job, which stops the process.
            self.answer = None;
            self.answer_task = None;
        }
        if self.reading_rule.as_ref().is_some_and(|s| *s != q) {
            self.reading_rule = None;
            self.rule_task = None;
        }
    }

    /// Whether an offer to ask the model fits here: not inside a scope or page.
    fn offers(&self) -> bool {
        self.scope.is_none() && self.pages.is_empty()
    }

    /// Enter on "read as an automation": the small model reads the sentence
    /// the parser did not get, and the answer is checked before it shows.
    fn start_rule(&mut self, cx: &mut Context<Self>) {
        let q = self.query(cx);
        if q.is_empty() || self.reading_rule.as_deref() == Some(q.as_str()) {
            return;
        }
        let repo = self.repo_for(rules::pr_number(&q));
        let sentence = q.clone();
        let sessions = self.seed.sessions.clone();
        let titles: Vec<String> = sessions.iter().map(|(_, t)| t.clone()).collect();
        self.reading_rule = Some(q);
        self.rule_task = Some(cx.spawn(async move |this, cx| {
            // Held here: when typing or closing drops this task, the process stops.
            let (rx, _job) = answer::start_rule(sentence.clone(), titles);
            let result = cx
                .background_spawn(
                    async move { rx.recv().unwrap_or_else(|_| Err("stopped".into())) },
                )
                .await;
            let _ = this.update(cx, |this, cx| {
                if this.reading_rule.as_deref() != Some(sentence.as_str()) {
                    return;
                }
                this.reading_rule = None;
                match result.and_then(|v| rules::from_json(&v, repo.as_deref(), &sessions)) {
                    Ok(d) => {
                        this.model_rule = Some((sentence, d));
                        this.refilter(cx);
                        this.selected = this.default_selection();
                    }
                    Err(e) => {
                        this.refilter_keep(cx);
                        this.note = Some((format!("Not read as an automation: {e}"), false));
                    }
                }
                cx.notify();
            });
        }));
        self.refilter_keep(cx);
        cx.notify();
    }

    fn start_answer(&mut self, cx: &mut Context<Self>) {
        let q = self.query(cx);
        if q.is_empty() {
            return;
        }
        let memories = sources::memory_hits_in(&self.memory, &self.memory_docs, &q, 3);
        let ctx = answer::Context {
            question: q.clone(),
            project: self.seed.project.clone(),
            branch: self
                .branch_chip()
                .unwrap_or_else(|| self.seed.branch.clone()),
            memories: memories
                .iter()
                .map(|m| (m.id(), m.memory.title.clone(), m.memory.body.clone()))
                .collect(),
            failures: self
                .seed
                .failures
                .iter()
                .filter(|f| q.contains(f.branch.as_str()) || f.branch == self.seed.branch)
                .take(2)
                .map(|f| (f.branch.clone(), f.what.clone(), f.log.clone()))
                .collect(),
            file: None,
        };
        let (a, rx, job) = answer::start(ctx);
        self.answer = Some(a);
        self.answer_task = Some(cx.spawn(async move |this, cx| {
            // Dropped with the task: a new question or a closed palette stops the process.
            let _job = job;
            loop {
                cx.background_executor()
                    .timer(Duration::from_millis(40))
                    .await;
                let mut done = false;
                let alive = this
                    .update(cx, |this, cx| {
                        let Some(a) = this.answer.as_mut() else {
                            done = true;
                            return;
                        };
                        let mut changed = false;
                        while let Ok(ev) = rx.try_recv() {
                            changed = true;
                            done |= a.take(ev);
                        }
                        if changed {
                            this.refilter_keep(cx);
                            if !this.nav {
                                this.selected = this.default_selection();
                            }
                            cx.notify();
                        }
                    })
                    .is_ok();
                if done || !alive {
                    break;
                }
            }
        }));
        self.refilter(cx);
        cx.notify();
    }

    fn copy_answer(&mut self, cx: &mut Context<Self>) -> bool {
        let Some(a) = &self.answer else { return false };
        if a.text.is_empty() {
            return false;
        }
        cx.write_to_clipboard(ClipboardItem::new_string(a.text.clone()));
        self.show_note("Answer copied", true, cx);
        true
    }

    // ---- filtering -----------------------------------------------------------------------

    fn refilter_keep(&mut self, cx: &mut Context<Self>) {
        let key = self.selected_key();
        let was = self.selected;
        self.refilter(cx);
        self.selected = key
            .and_then(|k| self.entry_index(&k))
            .unwrap_or(was.min(self.entries.len().saturating_sub(1)));
    }

    fn refilter(&mut self, cx: &mut Context<Self>) {
        let placeholder = match (self.pages.last(), self.scope) {
            (Some(p), _) => p.placeholder(),
            (None, Some(s)) => s.placeholder().to_string(),
            (None, None) if !self.chips.is_empty() => "Ask an agent…".to_string(),
            (None, None) => "Search, run a command or ask an agent…".to_string(),
        };
        self.input
            .update(cx, |i, cx| i.set_placeholder(placeholder, cx));
        let q = self.query(cx);
        if (self.scope == Some(Scope::Files) || (!q.is_empty() && q.len() >= 3))
            && self.files.is_some()
        {
            self.index_symbols(cx);
        }
        let now = crate::memory::now();
        let hour = self.seed.hour;
        let mut cands: Vec<Item> = self.seed.items.clone();
        let wants_files =
            !q.is_empty() || self.scope == Some(Scope::Files) || self.tab == Tab::Files;
        if wants_files && let Some(root) = &self.seed.root {
            cands.extend(self.file_items(root, &q));
        }
        cands.extend(self.memory_items(&q));
        cands.extend(self.todo_items());
        // The memory a card already shows is not listed again under it.
        // Built once: each card may search memory, and that is the slow part of a keystroke.
        let cards = self.cards(&q, cx);
        if let Some(Card::Remember(m)) = cards.iter().find(|c| matches!(c, Card::Remember(_))) {
            let key = format!("memory:{}", m.id());
            cands.retain(|i| i.key != key);
        }
        // `#arakun 86` / `#app`: a first word that names a repository or a
        // project narrows the pull requests to it; the rest is the search.
        let mut q = q;
        let mut narrowed: Option<String> = None;
        if self.scope == Some(Scope::Pulls)
            && self.pages.is_empty()
            && let Some((word, rest)) = narrow_word(&q, &cands, &self.seed.repos)
        {
            cands.retain(|i| i.kind != Kind::Pull || i.tags.contains(&word));
            narrowed = Some(word);
            q = rest;
        }
        // `#86` names one pull request; look for it when it isn't listed, and
        // only in the repositories the narrowing names.
        if self.scope == Some(Scope::Pulls)
            && let Ok(n) = q.trim().trim_start_matches('#').parse::<u64>()
            && !cands
                .iter()
                .any(|i| i.kind == Kind::Pull && i.meta.as_ref() == format!("#{n}"))
        {
            let repos: Vec<String> = lookup_repos(&cands, &self.seed.repos, narrowed.as_deref())
                .into_iter()
                .filter(|r| self.asked_pulls.insert((r.clone(), n)))
                .collect();
            if !repos.is_empty() {
                cx.emit(PaletteEvent::FindPull { number: n, repos });
            }
        }
        // Filter by page, scope and the person's tab.
        cands.retain(|i| {
            if let Some(p) = self.pages.last() {
                return self.in_page(p, i);
            }
            match self.scope {
                Some(Scope::Commands) => i.kind == Kind::Command,
                Some(Scope::Files) => matches!(i.kind, Kind::File | Kind::Symbol),
                Some(Scope::Settings) => i.kind == Kind::Setting,
                Some(Scope::People) => {
                    i.kind == Kind::Person || (i.kind == Kind::Pull && !q.is_empty())
                }
                Some(Scope::Pulls) => i.kind == Kind::Pull,
                None => true,
            }
        });
        let mut scored: Vec<(i32, Item, Vec<Range<usize>>)> = Vec::new();
        for item in cands {
            let f = self.frecency.score(&item.key, now, hour);
            if q.is_empty() {
                scored.push((item.boost + (f * 3.) as i32, item, Vec::new()));
                continue;
            }
            let detail = format!("{} {}", item.sub, item.extra);
            let Some(hit) =
                file_name_hit(&item, &q).or_else(|| rank::score_row(&item.title, &detail, &q))
            else {
                continue;
            };
            // The whole title typed out is what the person means.
            let exact = if item.title.eq_ignore_ascii_case(&q) {
                60
            } else {
                0
            };
            scored.push((
                hit.score + exact + item.boost + (f * 3.) as i32,
                item,
                hit.ranges,
            ));
        }
        // Tabs: counts per tab over the matches.
        let mut counts: HashMap<Tab, usize> = HashMap::new();
        for (_, item, _) in &scored {
            *counts.entry(item.tab()).or_default() += 1;
        }
        let base = [
            Tab::Sessions,
            Tab::GitHub,
            Tab::Files,
            Tab::Commands,
            Tab::Memory,
        ];
        let mut tabs: Vec<(Tab, usize)> = vec![(Tab::All, scored.len())];
        if q.is_empty() && self.scope.is_none() && self.pages.is_empty() {
            for t in base {
                tabs.push((t, counts.get(&t).copied().unwrap_or(0)));
            }
            if counts.get(&Tab::Files).copied().unwrap_or(0) == 0 {
                // Files count only once something is typed: say nothing.
                if let Some(t) = tabs.iter_mut().find(|(t, _)| *t == Tab::Files) {
                    t.1 = 0;
                }
            }
        } else {
            let order = [
                Tab::Sessions,
                Tab::GitHub,
                Tab::People,
                Tab::Files,
                Tab::Commands,
                Tab::Settings,
                Tab::Views,
                Tab::Memory,
            ];
            for t in order {
                if let Some(n) = counts.get(&t).copied().filter(|n| *n > 0) {
                    tabs.push((t, n));
                }
            }
        }
        if !tabs.iter().any(|(t, _)| *t == self.tab) {
            self.tab = Tab::All;
        }
        self.tabs = tabs;
        let tab = self.tab;
        scored.retain(|(_, i, _)| tab == Tab::All || i.tab() == tab);
        scored.sort_by_key(|a| std::cmp::Reverse(a.0));

        let mut entries: Vec<Entry> = Vec::new();
        let mut rows: Vec<Row> = Vec::new();
        // Cards first.
        for card in cards {
            let item = card_item(&card);
            rows.push(Row::Entry(entries.len()));
            entries.push(Entry {
                item,
                ranges: Vec::new(),
                card: Some(card),
            });
        }
        // Under an answer: what to do with it. Under a rule: doing it now instead.
        let lead_card = entries.first().and_then(|e| e.card.clone());
        if matches!(lead_card, Some(Card::Answer | Card::Rule(_))) {
            let (label, next) = match &lead_card {
                Some(Card::Rule(d)) => ("Or just now", self.rule_now(d, &q)),
                _ => ("Next", self.answer_next(&q)),
            };
            if !next.is_empty() {
                rows.push(Row::Section {
                    label: label.into(),
                    count: None,
                });
                for item in next {
                    rows.push(Row::Entry(entries.len()));
                    entries.push(Entry {
                        item,
                        ranges: Vec::new(),
                        card: None,
                    });
                }
            }
            self.entries = entries;
            self.rows = rows;
            if self.selected >= self.entries.len() {
                self.selected = 0;
            }
            return;
        }
        let empty_home =
            q.is_empty() && self.scope.is_none() && self.pages.is_empty() && tab == Tab::All;
        if empty_home && !self.pins.items.is_empty() {
            rows.push(Row::Pins);
        }
        // A copied error: the file it points at and what Arakun remembers of it.
        if empty_home && let Some(clip::Clip::Error(e)) = &self.clip {
            let mut related = Vec::new();
            if let Some((path, line)) = &e.location
                && let Some(abs) = self.known_file(path)
            {
                let mut item = file_item(&abs, path.clone(), Some(*line));
                item.sub = format!("line {line}").into();
                item.key = format!("related:{}", abs.display());
                related.push(item);
            }
            for m in self.memory_items(&e.headline).into_iter().take(2) {
                let mut m = m;
                m.title = format!("Arakun remembers: {}", m.title).into();
                related.push(m);
            }
            if !related.is_empty() {
                rows.push(Row::Section {
                    label: "Related".into(),
                    count: Some(related.len().to_string()),
                });
                for item in related {
                    rows.push(Row::Entry(entries.len()));
                    entries.push(Entry {
                        item,
                        ranges: Vec::new(),
                        card: None,
                    });
                }
            }
        }
        let groups = if empty_home {
            home_groups(scored, &self.frecency, now, hour)
        } else {
            let limit = if tab == Tab::All && self.scope.is_none() && self.pages.is_empty() {
                5
            } else {
                60
            };
            kind_groups(scored, limit, self.pages.last())
        };
        // When an answer or rule leads, the rest reads as "what to do next".
        for (label, total, group) in groups {
            if group.is_empty() {
                continue;
            }
            rows.push(Row::Section {
                label: label.into(),
                count: total.map(|n| n.to_string()),
            });
            for (_, item, ranges) in group {
                rows.push(Row::Entry(entries.len()));
                entries.push(Entry {
                    item,
                    ranges,
                    card: None,
                });
            }
        }
        self.entries = entries;
        self.count_lines(cx);
        self.rows = rows;
        if self.selected >= self.entries.len() {
            self.selected = 0;
        }
        // Skip a leading clipboard card when there is a better first row? No: the
        // card is the point of copying a link. Keep the first entry selected.
    }

    /// Line counts for the files on screen: known ones go on the rows now,
    /// the rest are read in the background and added when they arrive.
    fn count_lines(&mut self, cx: &mut Context<Self>) {
        let mut missing = Vec::new();
        for e in self
            .entries
            .iter()
            .filter(|e| e.item.kind == Kind::File)
            .take(12)
        {
            if let Target::File { path, .. } = &e.item.target {
                let key = path.display().to_string();
                if !self.line_counts.contains_key(&key) && self.counting.insert(key.clone()) {
                    missing.push((key, path.clone()));
                }
            }
        }
        self.label_lines();
        if missing.is_empty() {
            return;
        }
        cx.spawn(async move |this, cx| {
            let counts: Vec<(String, usize)> = cx
                .background_spawn(async move {
                    missing
                        .into_iter()
                        .map(|(key, path)| {
                            let n = sources::snippet(&path, None, 0).map_or(0, |(_, n)| n);
                            (key, n)
                        })
                        .collect()
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                for (key, n) in counts {
                    this.counting.remove(&key);
                    this.line_counts.insert(key, n);
                }
                this.label_lines();
                cx.notify();
            });
        })
        .detach();
    }

    fn label_lines(&mut self) {
        for e in self
            .entries
            .iter_mut()
            .filter(|e| e.item.kind == Kind::File && e.item.meta.is_empty())
        {
            if let Target::File { path, .. } = &e.item.target
                && let Some(n) = self.line_counts.get(&path.display().to_string())
                && *n > 0
            {
                e.item.meta = lines_label(*n).into();
            }
        }
    }

    /// A path from an error or an answer, if the project's file list has it.
    /// The list is already in memory, so this asks the disk nothing.
    fn known_file(&self, path: &str) -> Option<PathBuf> {
        known_file(self.seed.root.as_deref()?, self.files.as_deref()?, path)
    }

    /// The cards for this query, in order.
    fn cards(&self, q: &str, cx: &App) -> Vec<Card> {
        let mut out = Vec::new();
        if let Some((item, title, head)) = &self.confirm {
            out.push(Card::Confirm(item.clone(), title.clone(), head.clone()));
            return out;
        }
        if !self.pages.is_empty() {
            return out;
        }
        let home =
            q.is_empty() && self.scope.is_none() && self.chips.is_empty() && self.tab == Tab::All;
        if home && let Some(c) = &self.clip {
            out.push(Card::Clip(c.clone()));
        }
        if q.is_empty()
            || matches!(
                self.scope,
                Some(Scope::Commands | Scope::Settings | Scope::People | Scope::Pulls)
            )
        {
            return out;
        }
        if let Some(text) = todos::parse(q).or_else(|| rules::timeless_reminder(q)) {
            out.push(Card::Todo(text));
            return out;
        }
        let repo = self.repo_for(rules::pr_number(q));
        if let Some(d) = rules::parse(q, repo.as_deref(), &self.seed.sessions).or_else(|| {
            self.model_rule
                .as_ref()
                .filter(|(s, _)| s == q)
                .map(|(_, d)| d.clone())
        }) {
            out.push(Card::Rule(d));
            return out;
        }
        if self.answer.as_ref().is_some_and(|a| a.question == q) {
            out.push(Card::Answer);
            return out;
        }
        // Offers only: the model hears nothing until Enter on one of these.
        if self.offers() && (self.reading_rule.as_deref() == Some(q) || rules::sounds_like_rule(q))
        {
            out.push(Card::OfferRule);
        }
        if self.offers() && query::is_question(q) {
            out.push(Card::OfferAnswer);
        }
        if self.scope.is_none() && (self.tab == Tab::All || self.tab == Tab::Sessions) {
            out.push(Card::Ask);
        }
        if q.chars().count() >= 3
            && self.tab != Tab::Memory
            && let Some(m) = sources::memory_hits_in(&self.memory, &self.memory_docs, q, 1)
                .into_iter()
                .find(|m| m.score >= 2.5)
        {
            out.push(Card::Remember(m));
        }
        let _ = cx;
        out
    }

    /// The rows under an automation draft: the pull request it watches, and
    /// handing the sentence to an agent instead.
    fn rule_now(&self, d: &rules::Draft, q: &str) -> Vec<Item> {
        let mut out = Vec::new();
        if let rules::Trigger::PrChecksPass { repo, number } | rules::Trigger::PrMerged { repo, number } = &d.trigger
            && let Some(mut pull) = self
                .seed
                .items
                .iter()
                .find(|i| matches!(&i.target, Target::Pull(r) if r.number == *number && r.repo.eq_ignore_ascii_case(repo)))
                .cloned()
        {
            pull.title = format!("Open #{number} · {}", pull.title).into();
            pull.pill = None;
            out.push(pull);
        }
        out.push(
            Item::new(
                "next:ask",
                Kind::Command,
                "Ask an agent instead",
                Target::Ask {
                    prompt: q.to_string(),
                    model: self.model_chip().map(|(id, _)| id),
                    branch: self.branch_chip(),
                },
            )
            .icon("sparkles", Tint::Amber),
        );
        out
    }

    /// The rows under a quick answer: fix it in a session, open the file it
    /// names, re-run the run it read.
    fn answer_next(&self, q: &str) -> Vec<Item> {
        let mut out = Vec::new();
        let Some(a) = &self.answer else { return out };
        let branch = self
            .branch_chip()
            .unwrap_or_else(|| self.seed.branch.clone());
        let failure = a
            .failure
            .as_ref()
            .and_then(|w| self.seed.failures.iter().find(|f| f.what == *w));
        let prompt = match failure {
            Some(f) => format!(
                "{q}\n\nThe last failed run: {}.\n```\n{}\n```\n\nA quick answer said: {}",
                f.what,
                f.log.join("\n"),
                a.text
            ),
            None => format!("{q}\n\nA quick answer said: {}", a.text),
        };
        out.push(
            Item::new(
                "next:fix",
                Kind::Command,
                if failure.is_some() {
                    "Fix it in a new session"
                } else {
                    "Go deeper in a new session"
                },
                Target::Ask {
                    prompt,
                    model: self.model_chip().map(|(id, _)| id),
                    branch: self.branch_chip(),
                },
            )
            .icon("sparkles", Tint::Amber)
            .sub(format!("{} · {branch}", self.seed.agent)),
        );
        if let Some((path, line)) = clip::location(&a.text.replace('`', " "))
            && let Some(abs) = self.known_file(&path)
        {
            let mut item = file_item(&abs, format!("Open {path} at line {line}"), Some(line));
            item.key = format!("next:file:{}", abs.display());
            out.push(item);
        }
        if let Some(f) = failure
            && !self.seed.read_only
        {
            out.push(
                Item::new(
                    "next:rerun",
                    Kind::Command,
                    format!(
                        "Re-run {}",
                        f.what.split(" failed").next().unwrap_or("the run")
                    ),
                    Target::Rerun {
                        root: f.root.clone(),
                        id: f.id,
                    },
                )
                .icon("play", Tint::Green),
            );
        }
        out
    }

    /// The repository `#n` means: the project's, or the upstream of a fork
    /// when the queue knows #n there.
    fn repo_for(&self, number: Option<u64>) -> Option<String> {
        let own = self.seed.repo.clone();
        let Some(n) = number else { return own };
        let name = own
            .as_deref()
            .and_then(|r| r.split('/').nth(1))
            .unwrap_or("");
        let pulls: Vec<&crate::github::pr::ItemRef> = self
            .seed
            .items
            .iter()
            .filter_map(|i| match &i.target {
                Target::Pull(r) if r.number == n => Some(r),
                _ => None,
            })
            .collect();
        if own
            .as_deref()
            .is_some_and(|o| pulls.iter().any(|r| r.repo.eq_ignore_ascii_case(o)))
        {
            return own;
        }
        pulls
            .iter()
            .find(|r| {
                r.repo
                    .split('/')
                    .nth(1)
                    .is_some_and(|x| x.eq_ignore_ascii_case(name))
            })
            .map(|r| r.repo.clone())
            .or(own)
    }

    fn file_items(&self, root: &Path, q: &str) -> Vec<Item> {
        let mut out = Vec::new();
        let Some(files) = &self.files else { return out };
        // Path words: "island card" matches src/island_host/cards.rs.
        if q.is_empty() {
            for f in files
                .iter()
                .filter(|f| {
                    self.frecency.score(
                        &format!("file:{}", root.join(f).display()),
                        crate::memory::now(),
                        self.seed.hour,
                    ) > 0.
                })
                .take(20)
            {
                out.push(file_item(&root.join(f), f.clone(), None));
            }
            return out;
        }
        let mut hits: Vec<(i32, &String)> = files
            .iter()
            .filter_map(|f| {
                let name = f.rsplit('/').next().unwrap_or(f);
                let on_name = rank::fuzzy(name, q).map(|h| h.score + 20);
                // A path matches only as typed: scattered letters across folders mean nothing.
                on_name
                    .or_else(|| {
                        rank::contains_words(f, q)
                            .then(|| rank::score_row(f, "", q).map_or(0, |h| h.score))
                    })
                    .map(|s| (s, f))
            })
            .collect();
        hits.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.len().cmp(&b.1.len())));
        for (_, f) in hits.into_iter().take(40) {
            out.push(file_item(&root.join(f), f.clone(), None));
        }
        if let Some(symbols) = &self.symbols
            && q.chars().count() >= 2
            && (self.scope == Some(Scope::Files) || self.tab == Tab::Files)
        {
            let mut syms: Vec<(i32, &sources::Symbol)> = symbols
                .iter()
                .filter_map(|s| {
                    rank::fuzzy(&s.label(), q)
                        .filter(|h| h.ranges.len() <= 2)
                        .map(|h| (h.score, s))
                })
                .collect();
            syms.sort_by_key(|a| std::cmp::Reverse(a.0));
            for (_, s) in syms.into_iter().take(20) {
                let path = root.join(&s.file);
                let file = s.file.rsplit('/').next().unwrap_or(&s.file).to_string();
                let mut item = Item::new(
                    format!("symbol:{}:{}", path.display(), s.line),
                    Kind::Symbol,
                    s.label(),
                    Target::File {
                        path: path.clone(),
                        line: Some(s.line),
                    },
                )
                .sub(file)
                .meta(format!("L{}", s.line))
                .icon("file-code", Tint::Violet)
                .extra(s.file.clone())
                .preview(Preview::File {
                    path,
                    rel: s.file.clone(),
                    line: Some(s.line),
                });
                item.section = "Symbols";
                item.boost = -10;
                out.push(item);
            }
        }
        out
    }

    /// The open to-dos, and the ones ticked off today, dimmed.
    fn todo_items(&self) -> Vec<Item> {
        let now = crate::memory::now();
        self.todos
            .shown(now)
            .into_iter()
            .map(|t| {
                let done = t.done.is_some();
                let mut item = Item::new(
                    format!("todo:{}", t.id),
                    Kind::Todo,
                    t.text.clone(),
                    Target::Todo(t.id),
                )
                .icon(
                    if done { "checkmark-circle01" } else { "circle" },
                    if done { Tint::Green } else { Tint::Neutral },
                )
                .section("To-do")
                .meta(crate::memory::ago(t.done.unwrap_or(t.created), now))
                .extra("todo to-do pendiente pendientes".to_string())
                .boost(if done { -20 } else { 30 });
                item.dim = done;
                item
            })
            .collect()
    }

    fn memory_items(&self, q: &str) -> Vec<Item> {
        let wants = self.tab == Tab::Memory || (!q.is_empty() && q.chars().count() >= 2);
        if !wants || self.memory.iter().all(|(_, s)| s.items.is_empty()) {
            return Vec::new();
        }
        let now = crate::memory::now();
        sources::memory_hits_in(
            &self.memory,
            &self.memory_docs,
            q,
            if q.is_empty() { 60 } else { 15 },
        )
        .into_iter()
        .map(|m| {
            let scope = match m.scope {
                MemScope::Project => "project",
                MemScope::Global => "global",
            };
            Item::new(
                format!("memory:{}", m.id()),
                Kind::Memory,
                m.memory.title.clone(),
                Target::Memory,
            )
            .icon("bot", Tint::Neutral)
            .pill(m.memory.kind.clone(), Tint::Neutral)
            .extra(format!("{} {}", m.memory.body, m.memory.files.join(" ")))
            // BM25 found it: the title alone may not hold the words.
            .boost(((m.score * 8.) as i32).min(40) - 20)
            .preview(Preview::Memory {
                scope: scope.into(),
                id: m.id(),
                kind: m.memory.kind.clone(),
                body: m.memory.body.clone(),
                files: m.memory.files.clone(),
                age: crate::memory::ago(m.memory.updated, now),
                hits: m.memory.hits,
            })
        })
        .collect()
    }

    /// A file's numbered lines for the preview, read once and off the UI
    /// thread. None while the read runs; the preview draws again when it lands.
    pub fn snippet(
        &mut self,
        path: &Path,
        line: Option<u32>,
        cx: &mut Context<Self>,
    ) -> Option<Option<Snippet>> {
        let key = (path.to_path_buf(), line);
        if let Some(s) = self.snippets.get(&key) {
            return Some(s.clone());
        }
        if self.reading.insert(key.clone()) {
            cx.spawn(async move |this, cx| {
                let k = key.clone();
                let snip = cx
                    .background_spawn(async move {
                        let start = k.1.map(|l| l.saturating_sub(2).max(1));
                        sources::snippet(&k.0, start, 9)
                    })
                    .await;
                let _ = this.update(cx, |this, cx| {
                    this.reading.remove(&key);
                    this.snippets.insert(key, snip);
                    cx.notify();
                });
            })
            .detach();
        }
        None
    }
}

impl Focusable for Palette {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.input.focus_handle(cx)
    }
}

/// A file matched on its name first: `sidebar` finds `src/sidebar.rs` ahead
/// of a path that only passes through a `sidebar/` folder.
fn file_name_hit(item: &Item, q: &str) -> Option<rank::Hit> {
    if item.kind != Kind::File {
        return None;
    }
    let at = item.title.rfind('/').map_or(0, |i| i + 1);
    let hit = rank::fuzzy(&item.title[at..], q)?;
    Some(rank::Hit {
        score: hit.score + 10,
        ranges: hit
            .ranges
            .into_iter()
            .map(|r| r.start + at..r.end + at)
            .collect(),
    })
}

/// `path` (relative, `./`-led or under `root`) if `files` lists it.
fn known_file(root: &Path, files: &[String], path: &str) -> Option<PathBuf> {
    let p = Path::new(path);
    let rel = p.strip_prefix(root).unwrap_or(p);
    let rel = rel.strip_prefix(".").unwrap_or(rel);
    let rel = rel.to_string_lossy().replace('\\', "/");
    files.contains(&rel).then(|| root.join(&rel))
}

/// `1,204 lines`, `1 line`.
pub fn lines_label(n: usize) -> String {
    if n == 1 {
        "1 line".into()
    } else {
        format!("{} lines", thousands(n))
    }
}

/// `1 setting`, `5 settings`.
pub fn plural(n: usize, word: &str) -> String {
    if n == 1 {
        format!("1 {word}")
    } else {
        format!("{} {word}s", thousands(n))
    }
}

/// `1,204`.
pub fn thousands(n: usize) -> String {
    let s = n.to_string();
    let mut out = String::new();
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

pub fn file_item(path: &Path, rel: String, line: Option<u32>) -> Item {
    Item::new(
        format!("file:{}", path.display()),
        Kind::File,
        rel.clone(),
        Target::File {
            path: path.to_path_buf(),
            line,
        },
    )
    .icon("folder01", Tint::Blue)
    .section("Files")
    .preview(Preview::File {
        path: path.to_path_buf(),
        rel,
        line,
    })
}

/// What a copied GitHub link opens. A pull request or issue opens in the
/// GitHub view only when the link is on the host `gh` talks to: `gh` reads
/// `owner/name#n` there, and with `GH_HOST` set to another host that is a
/// different item. Any other link opens in the browser.
fn clip_target(url: &str, link: &clip::GitHubLink, host: &str) -> Target {
    match link {
        clip::GitHubLink::Pull { .. } | clip::GitHubLink::Issue { .. } => {
            crate::github::pr::ItemRef::from_url_on(url, host)
                .map_or_else(|| Target::Url(url.to_string()), Target::Pull)
        }
        _ => Target::Url(url.to_string()),
    }
}

fn card_item(card: &Card) -> Item {
    let key = match card {
        Card::Clip(_) => "card:clip",
        Card::Ask => "card:ask",
        Card::Remember(_) => "card:remember",
        Card::Answer => "card:answer",
        Card::OfferAnswer => "card:offer-answer",
        Card::OfferRule => "card:offer-rule",
        Card::Rule(_) => "card:rule",
        Card::Todo(_) => "card:todo",
        Card::Confirm(..) => "card:confirm",
    };
    let target = match card {
        Card::Clip(clip::Clip::GitHub { url, link }) => {
            clip_target(url, link, &crate::github::pr::gh_host())
        }
        Card::Clip(clip::Clip::Error(e)) => Target::FixError(e.clone()),
        Card::Remember(_) => Target::Memory,
        _ => Target::Nothing,
    };
    let mut item = Item::new(key, Kind::Card, "", target);
    if let Card::Clip(clip::Clip::Error(e)) = card {
        item.preview = Preview::Error(e.clone());
    }
    item
}

type Scored = (i32, Item, Vec<Range<usize>>);

/// The groups with nothing typed: what needs the person, what runs, and
/// what they reach for at this hour.
fn home_groups(
    scored: Vec<Scored>,
    f: &rank::Frecency,
    now: u64,
    hour: usize,
) -> Vec<(&'static str, Option<usize>, Vec<Scored>)> {
    let mut needs = Vec::new();
    let mut running = Vec::new();
    let mut rest = Vec::new();
    for s in scored {
        match s.1.section {
            "Needs you" => needs.push(s),
            "Running" => running.push(s),
            _ => rest.push(s),
        }
    }
    let needs_n = needs.len();
    let running_n = running.len();
    needs.truncate(5);
    running.truncate(4);
    // Jump back in: opened before, best frecency first; else the newest sessions.
    let mut back: Vec<Scored> = rest
        .iter()
        .filter(|s| f.score(&s.1.key, now, hour) > 0.)
        .cloned()
        .collect();
    back.sort_by(|a, b| {
        f.score(&b.1.key, now, hour)
            .total_cmp(&f.score(&a.1.key, now, hour))
    });
    let suggested = back.first().is_some_and(|s| f.usual_at(&s.1.key, hour));
    let mut label = if suggested {
        "Suggested now"
    } else {
        "Jump back in"
    };
    if back.is_empty() {
        back = rest
            .iter()
            .filter(|s| s.1.kind == Kind::Session)
            .take(4)
            .cloned()
            .collect();
    }
    if back.is_empty() {
        // Nothing opened yet: the things everyone starts with.
        label = "Quick actions";
        for key in [
            "command:new-session",
            "command:new-terminal",
            "view:github",
            "view:runs",
        ] {
            if let Some(s) = rest.iter().find(|s| s.1.key == key) {
                back.push(s.clone());
            }
        }
    }
    back.truncate(5);
    vec![
        ("Needs you", Some(needs_n), needs),
        ("Running", Some(running_n), running),
        (label, None, back),
    ]
}

fn kind_label(kind: Kind) -> &'static str {
    match kind {
        Kind::Session => "Sessions",
        Kind::Pull => "Pull requests",
        Kind::Person => "People",
        Kind::Run => "Runs",
        Kind::File => "Files",
        Kind::Symbol => "Symbols",
        Kind::Command => "Commands",
        Kind::View => "Views",
        Kind::Setting => "Settings",
        Kind::Project => "Projects",
        Kind::Branch => "Branches",
        Kind::Memory => "Memory",
        Kind::Rule => "Automations",
        Kind::Todo => "To-do",
        Kind::Card => "",
    }
}

/// Results grouped by kind, the group with the best match first; each
/// group capped at `limit` but counted whole.
fn kind_groups(
    scored: Vec<Scored>,
    limit: usize,
    page: Option<&Page>,
) -> Vec<(&'static str, Option<usize>, Vec<Scored>)> {
    let mut groups: Vec<(Kind, i32, Vec<Scored>)> = Vec::new();
    for s in scored {
        // A person's page splits their pull requests by who it waits on.
        let kind = s.1.kind;
        match groups.iter_mut().find(|g| g.0 == kind) {
            Some(g) => g.2.push(s),
            None => groups.push((kind, s.0, vec![s])),
        }
    }
    groups.sort_by_key(|a| std::cmp::Reverse(a.1));
    groups
        .into_iter()
        .map(|(kind, _, mut items)| {
            let n = items.len();
            items.truncate(limit);
            let label = match (page, kind) {
                (Some(Page::Person { .. }), Kind::Pull) => "Their pull requests",
                (Some(Page::Branches { .. }), Kind::Branch) => "Branches",
                _ => kind_label(kind),
            };
            (label, Some(n), items)
        })
        .collect()
}

/// Whether `word` (lower case) names this repository, its owner or its
/// project: the same words `Item::repo_tags` puts on its pull requests.
fn names_repo(word: &str, project: &str, repo: &str) -> bool {
    let repo = repo.to_ascii_lowercase();
    let (owner, name) = repo.split_once('/').unwrap_or(("", repo.as_str()));
    word == repo || word == owner || word == name || project.eq_ignore_ascii_case(word)
}

/// `#arakun 86`: the first word (lower case) when it names a listed pull
/// request's repository or a project's, and the rest of the query. A project
/// counts before its pull requests arrive, so `#arakun 86` still narrows.
fn narrow_word(q: &str, items: &[Item], repos: &[(String, String)]) -> Option<(String, String)> {
    let mut words = q.split_whitespace();
    let first = words.next()?.trim_start_matches('#').to_ascii_lowercase();
    if first.is_empty() {
        return None;
    }
    let known = items
        .iter()
        .any(|i| i.kind == Kind::Pull && i.tags.contains(&first))
        || repos.iter().any(|(p, r)| names_repo(&first, p, r));
    known.then(|| (first, words.collect::<Vec<_>>().join(" ")))
}

/// The repositories (`owner/name`, lower case) to look `#86` up in: those of
/// the listed pull requests and the projects, or only the ones `narrowed` names.
fn lookup_repos(items: &[Item], repos: &[(String, String)], narrowed: Option<&str>) -> Vec<String> {
    let mut out: Vec<String> = items
        .iter()
        .filter(|i| i.kind == Kind::Pull)
        .filter(|i| narrowed.is_none_or(|w| i.tags.iter().any(|t| t == w)))
        .filter_map(|i| i.tags.iter().find(|t| t.contains('/')).cloned())
        .collect();
    out.extend(
        repos
            .iter()
            .filter(|(p, r)| narrowed.is_none_or(|w| names_repo(w, p, r)))
            .map(|(_, r)| r.to_ascii_lowercase()),
    );
    out.sort();
    out.dedup();
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_copied_github_link_opens_in_the_view_only_on_the_gh_host() {
        let url = "https://github.com/o/r/pull/7";
        let link = clip::github_link(url).unwrap();
        assert!(matches!(
            clip_target(url, &link, "github.com"),
            Target::Pull(r) if r.repo == "o/r" && r.number == 7
        ));
        // `gh` talks to an Enterprise host: o/r#7 there is another item.
        assert!(matches!(
            clip_target(url, &link, "ghe.corp"),
            Target::Url(u) if u == url
        ));
        let issue = "https://github.com/o/r/issues/3";
        assert!(matches!(
            clip_target(issue, &clip::github_link(issue).unwrap(), "github.com"),
            Target::Pull(r) if r.kind == crate::github::pr::ItemKind::Issue
        ));
    }

    fn pull(repo: &str, project: Option<&str>, n: u64) -> Item {
        Item::new(
            format!("pull:{repo}#{n}"),
            Kind::Pull,
            "title",
            Target::Nothing,
        )
        .repo_tags(repo, project)
        .meta(format!("#{n}"))
    }

    #[test]
    fn a_first_word_naming_a_repository_or_project_narrows() {
        let repos = vec![
            ("app".to_string(), "ArakunHQ/arakun".to_string()),
            ("site".to_string(), "ArakunHQ/web".to_string()),
        ];
        let items = vec![pull("ArakunHQ/web", Some("site"), 3)];
        // By name, before any of its pull requests is listed.
        assert_eq!(
            narrow_word("#arakun 86", &items, &repos),
            Some(("arakun".into(), "86".into()))
        );
        // By project name, and by `owner/name`.
        assert_eq!(
            narrow_word("app", &items, &repos),
            Some(("app".into(), String::new()))
        );
        assert_eq!(
            narrow_word("arakunhq/web fix", &items, &repos),
            Some(("arakunhq/web".into(), "fix".into()))
        );
        // Any other first word is the search.
        assert_eq!(narrow_word("fix palette", &items, &repos), None);
        assert_eq!(narrow_word("86", &items, &repos), None);
    }

    #[test]
    fn a_narrowed_lookup_asks_only_the_named_repository() {
        let repos = vec![
            ("app".to_string(), "ArakunHQ/arakun".to_string()),
            ("site".to_string(), "ArakunHQ/web".to_string()),
        ];
        let items = vec![
            pull("ArakunHQ/web", Some("site"), 3),
            pull("other/lib", None, 9),
        ];
        assert_eq!(
            lookup_repos(&items, &repos, Some("app")),
            vec!["arakunhq/arakun".to_string()]
        );
        assert_eq!(
            lookup_repos(&items, &repos, Some("site")),
            vec!["arakunhq/web".to_string()]
        );
        assert_eq!(
            lookup_repos(&items, &repos, None),
            vec![
                "arakunhq/arakun".to_string(),
                "arakunhq/web".to_string(),
                "other/lib".to_string()
            ]
        );
    }

    fn item(key: &str, kind: Kind, section: &'static str) -> Item {
        Item::new(key, kind, key, Target::Nothing).section(section)
    }

    #[test]
    fn known_files_come_from_the_list_not_the_disk() {
        let root = Path::new("/nowhere/app");
        let files = vec!["src/main.rs".to_string()];
        let want = Some(root.join("src/main.rs"));
        assert_eq!(known_file(root, &files, "src/main.rs"), want);
        assert_eq!(known_file(root, &files, "./src/main.rs"), want);
        assert_eq!(known_file(root, &files, "/nowhere/app/src/main.rs"), want);
        assert_eq!(known_file(root, &files, "src/gone.rs"), None);
    }

    #[test]
    fn plurals() {
        assert_eq!(plural(1, "setting"), "1 setting");
        assert_eq!(plural(5, "setting"), "5 settings");
    }

    #[test]
    fn thousands_are_separated() {
        assert_eq!(thousands(1204), "1,204");
        assert_eq!(thousands(12), "12");
        assert_eq!(thousands(1_000_000), "1,000,000");
    }

    #[test]
    fn home_puts_what_needs_you_first() {
        let scored = vec![
            (0, item("a", Kind::Session, "Running"), vec![]),
            (0, item("b", Kind::Session, "Needs you"), vec![]),
            (0, item("c", Kind::Session, "Sessions"), vec![]),
        ];
        let g = home_groups(scored, &rank::Frecency::default(), 0, 0);
        assert_eq!(g[0].0, "Needs you");
        assert_eq!(g[0].2[0].1.key, "b");
        assert_eq!(g[1].2[0].1.key, "a");
        assert_eq!(g[2].0, "Jump back in");
        assert_eq!(g[2].2[0].1.key, "c");
    }

    #[test]
    fn frecency_fills_jump_back_in_and_learns_the_hour() {
        let mut f = rank::Frecency::default();
        f.record("v", 100, 9);
        f.record("v", 100, 9);
        let scored = vec![
            (0, item("v", Kind::View, "Views"), vec![]),
            (0, item("s", Kind::Session, "Sessions"), vec![]),
        ];
        let g = home_groups(scored, &f, 100, 9);
        assert_eq!(g[2].0, "Suggested now");
        assert_eq!(g[2].2.len(), 1);
        assert_eq!(g[2].2[0].1.key, "v");
    }

    #[test]
    fn groups_follow_their_best_match_and_count_all() {
        let scored = vec![
            (300, item("f1", Kind::File, ""), vec![]),
            (200, item("s1", Kind::Session, ""), vec![]),
            (100, item("f2", Kind::File, ""), vec![]),
            (90, item("f3", Kind::File, ""), vec![]),
        ];
        let g = kind_groups(scored, 2, None);
        assert_eq!(g[0].0, "Files");
        assert_eq!(g[0].1, Some(3));
        assert_eq!(g[0].2.len(), 2);
        assert_eq!(g[1].0, "Sessions");
    }

    #[test]
    fn store_paths_follow_the_state_file() {
        // Without ARAKUN_STATE the data dir holds them; the name is kept either way.
        assert!(store_path("pins.json").ends_with("pins.json"));
    }

    #[test]
    fn a_merge_is_pinned_to_the_head_read_when_the_palette_opened() {
        use crate::github::pr::{ItemKind, ItemRef};
        let pull = |number| ItemRef {
            repo: "o/r".into(),
            number,
            kind: ItemKind::Pull,
        };
        let mut heads = HashMap::new();
        heads.insert(pull(1), "0123456789abcdef".to_string());
        heads.insert(pull(2), String::new());
        assert_eq!(
            head_to_merge(&heads, &pull(1)).as_deref(),
            Some("0123456789abcdef")
        );
        // Unread or empty: nothing to merge.
        assert_eq!(head_to_merge(&heads, &pull(2)), None);
        assert_eq!(head_to_merge(&heads, &pull(3)), None);
        assert_eq!(short_head("0123456789abcdef"), "0123456");
        assert_eq!(short_head("abc"), "abc");
    }
}
