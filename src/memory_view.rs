//! The Memory tab: what agents remember about the visible project and about
//! the user (`arakun mem`), to read, search, edit and prune. Agents write the
//! files from their own processes, so the tab watches them.

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use gpui::prelude::FluentBuilder as _;
use gpui::{
    App, AppContext as _, Context, Entity, EventEmitter, Focusable as _, FontWeight, Hsla,
    InteractiveElement as _, IntoElement, ParentElement as _, Render, ScrollHandle, SharedString,
    StatefulInteractiveElement as _, Styled as _, Subscription, Task, Window, div, px, rgb,
};

use crate::input::{InputEvent, TextInput};
use crate::memory::{self, Memory, Scope};
use crate::theme;
use crate::ui::{self, icon, icon_button};

const POLL: Duration = Duration::from_secs(2);

pub enum MemoryEvent {
    /// Open a memory file in the user's editor.
    Edit(PathBuf),
}

struct Source {
    file: PathBuf,
    /// What `project` is written as in the file (empty for the global one).
    owner: String,
    stamp: Option<SystemTime>,
    loaded: bool,
    items: Vec<Memory>,
}

impl Source {
    fn new(file: PathBuf, owner: String) -> Self {
        let mut s = Self {
            file,
            owner,
            stamp: None,
            loaded: false,
            items: Vec::new(),
        };
        s.reload();
        s
    }

    /// Re-reads the file if it changed since the last read; says whether it did.
    fn reload(&mut self) -> bool {
        let stamp = std::fs::metadata(&self.file)
            .and_then(|m| m.modified())
            .ok();
        if self.loaded && stamp == self.stamp {
            return false;
        }
        self.stamp = stamp;
        self.loaded = true;
        self.items = memory::load(&self.file).items;
        true
    }
}

pub struct MemoryView {
    dir: Option<PathBuf>,
    name: SharedString,
    /// This project's memory, then the user's.
    sources: Vec<(Scope, Source)>,
    query: Entity<TextInput>,
    /// The memory whose delete was clicked once, waiting for the second click.
    confirm: Option<(Scope, u64)>,
    error: Option<SharedString>,
    scroll: ScrollHandle,
    /// Whether the tab is on screen in a window in front; the files are only
    /// checked then.
    shown: bool,
    _query: Subscription,
    _poll: Task<()>,
}

impl EventEmitter<MemoryEvent> for MemoryView {}

impl MemoryView {
    pub fn new(cx: &mut Context<Self>) -> Self {
        let query = cx.new(|cx| TextInput::new("Search memory...", cx));
        let sub = cx.subscribe(&query, |_, input, ev: &InputEvent, cx| match ev {
            InputEvent::Changed => cx.notify(),
            InputEvent::Cancel => input.update(cx, |i, cx| i.set_text("", cx)),
            _ => {}
        });
        let poll = crate::poll::poll_every(cx, POLL, POLL, |this, cx| {
            if this.shown {
                this.reload(cx);
            }
        });
        Self {
            dir: None,
            name: SharedString::default(),
            sources: Vec::new(),
            query,
            confirm: None,
            error: None,
            scroll: ScrollHandle::new(),
            shown: false,
            _query: sub,
            _poll: poll,
        }
    }

    /// Shows the memory of the project `dir` belongs to.
    pub fn set_dir(&mut self, dir: &Path, cx: &mut Context<Self>) {
        if self.dir.as_deref() == Some(dir) {
            return;
        }
        self.dir = Some(dir.to_path_buf());
        let project = memory::project_of(dir);
        let root = memory::root();
        self.name = project
            .file_name()
            .map_or_else(
                || project.display().to_string(),
                |n| n.to_string_lossy().to_string(),
            )
            .into();
        self.sources = vec![
            (
                Scope::Project,
                Source::new(
                    memory::store_file(&root, Scope::Project, &project),
                    project.display().to_string(),
                ),
            ),
            (
                Scope::Global,
                Source::new(
                    memory::store_file(&root, Scope::Global, &project),
                    String::new(),
                ),
            ),
        ];
        self.confirm = None;
        self.error = None;
        cx.notify();
    }

    pub fn focus(&self, window: &mut Window, cx: &mut App) {
        window.focus(&self.query.focus_handle(cx), cx);
    }

    pub fn is_shown(&self) -> bool {
        self.shown
    }

    /// The shell says whether this tab is on screen in a window in front.
    /// Coming back, it catches up with what changed meanwhile.
    pub fn set_shown(&mut self, shown: bool, cx: &mut Context<Self>) {
        let back = shown && !self.shown;
        self.shown = shown;
        if back {
            self.reload(cx);
        }
    }

    fn reload(&mut self, cx: &mut Context<Self>) {
        let mut changed = false;
        for (_, s) in &mut self.sources {
            changed |= s.reload();
        }
        if changed {
            cx.notify();
        }
    }

    fn forget(&mut self, scope: Scope, id: u64, cx: &mut Context<Self>) {
        self.confirm = None;
        if let Some((_, s)) = self.sources.iter_mut().find(|(sc, _)| *sc == scope) {
            let done = memory::update(&s.file, &s.owner, |store| {
                let before = store.items.len();
                store.items.retain(|m| m.id != id);
                (store.items.len() != before, ())
            });
            self.error = done.err().map(|e| format!("Could not delete: {e}").into());
            s.loaded = false;
            s.reload();
        }
        cx.notify();
    }

    fn render_memory(
        &self,
        scope: Scope,
        m: &Memory,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let key = (scope, m.id);
        let group: SharedString = format!(
            "mem-{}{}",
            if scope == Scope::Global { "g" } else { "" },
            m.id
        )
        .into();
        let confirming = self.confirm == Some(key);
        let mut meta = vec![
            format!("#{}{}", if scope == Scope::Global { "g" } else { "" }, m.id),
            memory::ago(m.updated, memory::now()),
        ];
        if let Some(t) = &m.topic {
            meta.push(t.clone());
        }
        if !m.files.is_empty() {
            meta.push(m.files.join(", "));
        }
        if m.hits > 0 {
            meta.push(format!("read {}×", m.hits));
        }
        div()
            .group(group.clone())
            .flex()
            .flex_col()
            .gap(crate::ui::sp(4.))
            .px(crate::ui::sp(12.))
            .py(crate::ui::sp(8.))
            .rounded(crate::ui::rad(ui::RADIUS))
            .bg(theme::fill_raised())
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(crate::ui::sp(8.))
                    .child(
                        div()
                            .flex_none()
                            .px(crate::ui::sp(6.))
                            .py(crate::ui::sp(1.))
                            .rounded(crate::ui::rad(ui::RADIUS_SM))
                            .bg(kind_color(&m.kind).opacity(0.14))
                            .text_color(kind_color(&m.kind))
                            .text_size(px(ui::TEXT_XS))
                            .font_weight(FontWeight::MEDIUM)
                            .child(m.kind.clone()),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_size(px(ui::TEXT_MD))
                            .font_weight(FontWeight::MEDIUM)
                            .child(m.title.clone()),
                    )
                    .child(
                        div()
                            .id(SharedString::from(format!("{group}-del")))
                            .flex()
                            .flex_none()
                            .items_center()
                            .gap(crate::ui::sp(4.))
                            .px(crate::ui::sp(4.))
                            .h(px(20.))
                            .rounded(crate::ui::rad(ui::RADIUS_SM))
                            .cursor_pointer()
                            .when(!confirming, |d| {
                                d.invisible().group_hover(group.clone(), |s| s.visible())
                            })
                            .hover(|s| s.bg(theme::fill_selected()))
                            .map(|d| {
                                if confirming {
                                    d.text_size(px(ui::TEXT_XS))
                                        .text_color(theme::error())
                                        .child("Delete?")
                                } else {
                                    d.child(icon("delete02", 13.).text_color(theme::text_subtle()))
                                }
                            })
                            .on_click(cx.listener(move |this, _, _, cx| {
                                if this.confirm == Some(key) {
                                    this.forget(key.0, key.1, cx);
                                } else {
                                    this.confirm = Some(key);
                                    cx.notify();
                                }
                            })),
                    ),
            )
            .child(
                div()
                    .text_size(px(ui::TEXT_XS))
                    .text_color(theme::text_subtle())
                    .child(meta.join(" · ")),
            )
            .when(!m.body.is_empty(), |d| {
                d.child(
                    div()
                        .text_size(px(ui::TEXT_SM))
                        .line_height(px(18.))
                        .text_color(theme::text_muted())
                        .child(m.body.clone()),
                )
            })
    }

    fn render_section(
        &self,
        title: String,
        scope: Scope,
        cx: &mut Context<Self>,
    ) -> Option<impl IntoElement + use<>> {
        let (_, source) = self.sources.iter().find(|(s, _)| *s == scope)?;
        let query = self.query.read(cx).text().trim().to_string();
        let mut items: Vec<&Memory> = if query.is_empty() {
            let mut all: Vec<&Memory> = source.items.iter().collect();
            all.sort_by_key(|m| std::cmp::Reverse(m.updated));
            all
        } else {
            memory::search(&source.items, &query)
                .into_iter()
                .map(|(_, m)| m)
                .collect()
        };
        if !query.is_empty() && items.is_empty() {
            return None;
        }
        let count = items.len();
        let file = source.file.clone();
        let editable = file.exists();
        let mut list = div().flex().flex_col().gap(crate::ui::sp(6.));
        if items.is_empty() {
            list = list.child(
                div().px(crate::ui::sp(2.)).py(crate::ui::sp(4.)).text_size(px(ui::TEXT_SM)).text_color(theme::text_faint()).child(match scope {
                    Scope::Project => "Nothing yet. Agents save what they learn here with `arakun mem save`.",
                    Scope::Global => "Nothing yet. Preferences you state to an agent end up here, for every project.",
                }),
            );
        }
        for m in items.drain(..) {
            list = list.child(self.render_memory(scope, m, cx));
        }
        Some(
            div()
                .flex()
                .flex_col()
                .gap(crate::ui::sp(8.))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(crate::ui::sp(6.))
                        .h(px(ui::ICON_BUTTON))
                        .text_size(px(ui::TEXT_SM))
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(theme::text_subtle())
                        .child(div().min_w_0().truncate().child(title))
                        .child(
                            div()
                                .flex_none()
                                .text_color(theme::text_faint())
                                .child(count.to_string()),
                        )
                        .child(div().flex_1())
                        .when(editable, |d| {
                            d.child(
                                icon_button(
                                    SharedString::from(format!("mem-edit-{scope:?}")),
                                    "pencil-edit01",
                                    13.,
                                    "Edit",
                                )
                                .on_click(cx.listener(
                                    move |_, _, _, cx| cx.emit(MemoryEvent::Edit(file.clone())),
                                )),
                            )
                        }),
                )
                .child(list),
        )
    }
}

fn kind_color(kind: &str) -> Hsla {
    let [blue, amber, green, violet, coral, ..] = theme::CATEGORY;
    match kind {
        "preference" | "convention" => rgb(violet).into(),
        "gotcha" => rgb(amber).into(),
        "bugfix" => rgb(coral).into(),
        "decision" | "config" => rgb(blue).into(),
        "session" => theme::mix(0.5),
        _ => rgb(green).into(),
    }
}

impl Render for MemoryView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let project = self.render_section(format!("{}", self.name), Scope::Project, cx);
        let global = self.render_section("About you, in every project".into(), Scope::Global, cx);
        let nothing = project.is_none() && global.is_none();
        let blank = self.query.read(cx).text().trim().is_empty()
            && self.sources.iter().all(|(_, s)| s.items.is_empty());
        div()
            .flex()
            .flex_col()
            .size_full()
            .bg(theme::background())
            .font_family(theme::FONT_SANS)
            .text_color(theme::content())
            .child(
                div()
                    .flex()
                    .flex_none()
                    .px(crate::ui::sp(ui::PAGE_PAD))
                    .pt(crate::ui::sp(ui::PAGE_PAD))
                    .pb(crate::ui::sp(12.))
                    .child(ui::page_title(
                        "Memory",
                        Some("What agents carry from one session to the next".into_any_element()),
                    )),
            )
            .child(
                div().flex_none().px(crate::ui::sp(ui::PAGE_PAD)).pb(crate::ui::sp(12.)).border_b_1().border_color(theme::stroke()).child(
                    div()
                        .flex()
                        .items_center()
                        .gap(crate::ui::sp(8.))
                        .h(px(ui::INPUT_H))
                        .px(crate::ui::sp(10.))
                        .rounded(crate::ui::rad(ui::RADIUS))
                        .border_1()
                        .border_color(theme::border_strong())
                        .bg(theme::background())
                        .text_size(px(ui::TEXT_MD))
                        .child(icon("search01", 14.).text_color(theme::text_faint()))
                        .child(div().flex_1().min_w_0().child(self.query.clone())),
                ),
            )
            .child(
                div()
                    .id("memory-list")
                    .track_scroll(&self.scroll)
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .px(crate::ui::sp(ui::PAGE_PAD))
                    .py(crate::ui::sp(16.))
                    .gap(crate::ui::sp(20.))
                    .when_some(self.error.clone(), |d, e| {
                        d.child(div().text_size(px(ui::TEXT_SM)).text_color(theme::error()).child(e))
                    })
                    .when(nothing, |d| d.child(ui::empty_state("search01", "Nothing matches", "Try fewer or other words: it searches titles, text and files.", None)))
                    .when(blank, |d| {
                        d.child(ui::empty_state(
                            "ai-idea",
                            "Nothing remembered yet",
                            "Agents save what they learn about this project as they work. Preferences you tell them land under About you, for every project.",
                            None,
                        ))
                    })
                    .when(!blank, |d| d.children(project).children(global)),
            )
    }
}
