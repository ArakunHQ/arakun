//! The Doop tab: the canvases of a Doop instance and the frames of the one
//! picked, each with its render. Every sign-in stays saved; the header names
//! the one shown and switches between them. An instance is Doop itself or a
//! self-hosted one, named by its address when signing in (`doop`).
//!
//! The tab only reads. A click on a frame opens it on the canvas in a browser
//! tab, where the editing happens. The Accounts page also connects the
//! instance's MCP server to the agents, so sessions can read and draw on the
//! same canvases.

mod accounts;

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, AppContext as _, Context, Entity, EventEmitter, FocusHandle,
    InteractiveElement as _, IntoElement, MouseButton, ObjectFit, ParentElement as _, Pixels,
    Point, Render, ScrollHandle, StatefulInteractiveElement as _, Styled as _, StyledImage as _,
    Subscription, Task, Window, div, img, px,
};

use crate::agent::AgentKind;
use crate::doop::{self, Canvas, CanvasRef, Error, Frame, Session, Shown, Tool};
use crate::input::{InputEvent, TextInput};
use crate::theme;
use crate::ui::{self, ButtonKind, Tone, Type, Typography as _, icon};

/// What the instance holds is read again when the tab opens after this long.
const STALE: u64 = 10 * 60;
/// Renders are downloaded this many at a time.
const AT_ONCE: usize = 4;
/// The width of a frame's card and the height of its picture.
const CARD: (f32, f32) = (264., 168.);

pub enum DoopEvent {
    /// Open a Doop page in a browser tab.
    Open(String),
    /// A command to run in a terminal of the visible environment.
    Run(String),
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Page {
    Main,
    Accounts,
}

/// A frame's render in the grid.
#[derive(Clone, Debug, PartialEq)]
enum Thumb {
    Loading,
    Ready(PathBuf),
    /// The instance gave no picture for it.
    Missing,
}

pub struct DoopView {
    /// With their credentials: never rendered, only handed to `doop`.
    sessions: Vec<Session>,
    /// The id of the session shown.
    current: Option<String>,
    tools: Option<Vec<Tool>>,
    /// What the instance lists; None when it has no tool for that.
    listed: Option<Vec<CanvasRef>>,
    /// Canvases added by link on this instance.
    pins: Vec<CanvasRef>,
    picked: Option<String>,
    canvas: Option<Canvas>,
    /// The frames of the canvas that show: a page of it, or all of them.
    shown: Shown,
    thumbs: HashMap<String, Thumb>,
    loading: bool,
    /// The instance's tools and canvases are being read.
    listing: bool,
    error: Option<Error>,
    /// Unix seconds of the last full read.
    read_at: u64,
    /// Goes up when the session or the canvas changes: an answer for an
    /// older one is dropped.
    epoch: u64,
    /// Goes up when the sign-in shown changes and with each read of its
    /// canvases: picking a canvas leaves a read under way alone.
    list_epoch: u64,
    page: Page,
    menu: Option<Point<Pixels>>,
    host_input: Entity<TextInput>,
    key_input: Entity<TextInput>,
    canvas_input: Entity<TextInput>,
    /// The form signs in to a self-hosted instance, not to Doop itself.
    own_instance: bool,
    /// Set while a sign-in runs; the flag stops one waiting on the browser.
    connecting: Option<Arc<doop::Attempt>>,
    connect_note: Option<(String, bool)>,
    add_note: Option<String>,
    /// The name each agent has the shown instance's MCP server under.
    agents: Vec<(AgentKind, Option<String>)>,
    focus: FocusHandle,
    scroll: ScrollHandle,
    _load: Option<Task<()>>,
    _canvas: Option<Task<()>>,
    _thumbs: Option<Task<()>>,
    _connect: Option<Task<()>>,
    _inputs: [Subscription; 3],
}

impl EventEmitter<DoopEvent> for DoopView {}

impl DoopView {
    pub fn new(cx: &mut Context<Self>) -> Self {
        let host_input = cx.new(|cx| TextInput::new("doop.example.com", cx).mono());
        let key_input = cx.new(|cx| TextInput::new("Agent key", cx).mono().secret());
        let canvas_input = cx.new(|cx| TextInput::new("Canvas link or id", cx));
        let clear = |this: &mut Self, cx: &mut Context<Self>| {
            this.connect_note = None;
            cx.notify();
        };
        let inputs = [
            cx.subscribe(
                &host_input,
                move |this: &mut Self, _, ev: &InputEvent, cx| match ev {
                    InputEvent::Submit => this.connect_browser(cx),
                    InputEvent::Changed => clear(this, cx),
                    _ => {}
                },
            ),
            cx.subscribe(
                &key_input,
                move |this: &mut Self, _, ev: &InputEvent, cx| match ev {
                    InputEvent::Submit => this.connect_key(cx),
                    InputEvent::Changed => clear(this, cx),
                    _ => {}
                },
            ),
            cx.subscribe(
                &canvas_input,
                |this: &mut Self, _, ev: &InputEvent, cx| match ev {
                    InputEvent::Submit => this.add_canvas(cx),
                    InputEvent::Changed => {
                        this.add_note = None;
                        cx.notify();
                    }
                    _ => {}
                },
            ),
        ];
        let sessions = doop::sessions();
        let current = sessions.first().map(|s| s.id.clone());
        let pins = sessions
            .first()
            .map(|s| doop::pins(&s.host))
            .unwrap_or_default();
        Self {
            sessions,
            current,
            tools: None,
            listed: None,
            pins,
            picked: None,
            canvas: None,
            shown: Shown::All,
            thumbs: HashMap::new(),
            loading: false,
            listing: false,
            error: None,
            read_at: 0,
            epoch: 0,
            list_epoch: 0,
            page: Page::Main,
            menu: None,
            host_input,
            key_input,
            canvas_input,
            own_instance: false,
            connecting: None,
            connect_note: None,
            add_note: None,
            agents: Vec::new(),
            focus: cx.focus_handle(),
            scroll: ScrollHandle::new(),
            _load: None,
            _canvas: None,
            _thumbs: None,
            _connect: None,
            _inputs: inputs,
        }
    }

    pub fn focus(&self, window: &mut Window, cx: &mut gpui::App) {
        window.focus(&self.focus, cx);
    }

    fn session(&self) -> Option<&Session> {
        let id = self.current.as_ref()?;
        self.sessions.iter().find(|s| &s.id == id)
    }

    /// The canvases to pick from: what the instance lists, then the ones
    /// added by link that it did not list. The flag marks the added ones.
    fn canvases(&self) -> Vec<(CanvasRef, bool)> {
        let listed = self.listed.clone().unwrap_or_default();
        let pinned: Vec<CanvasRef> = self
            .pins
            .iter()
            .filter(|p| !listed.iter().any(|l| l.id == p.id))
            .cloned()
            .collect();
        listed
            .into_iter()
            .map(|c| (c, false))
            .chain(pinned.into_iter().map(|c| (c, true)))
            .collect()
    }

    /// Opening the tab: what the instance holds, if old, and what the agents have.
    pub fn refresh_if_stale(&mut self, cx: &mut Context<Self>) {
        self.read_agents();
        if self.session().is_some()
            && !self.listing
            && crate::model::unix_now().saturating_sub(self.read_at) >= STALE
        {
            self.refresh(false, cx);
        }
    }

    fn read_agents(&mut self) {
        self.agents = match self.session() {
            Some(s) => doop::AGENTS
                .into_iter()
                .map(|a| (a, doop::agent_server(a, &s.host)))
                .collect(),
            None => Vec::new(),
        };
    }

    /// Shows another sign-in: nothing read for the last one applies.
    fn switch(&mut self, id: Option<String>, cx: &mut Context<Self>) {
        self.current = id;
        self.epoch += 1;
        self.list_epoch += 1;
        self.listing = false;
        self.tools = None;
        self.listed = None;
        self.picked = None;
        self.shown = Shown::All;
        self.canvas = None;
        self.thumbs.clear();
        self.error = None;
        self.add_note = None;
        self.loading = false;
        self.read_at = 0;
        self.menu = None;
        self._load = None;
        self._canvas = None;
        self._thumbs = None;
        self.pins = self
            .session()
            .map(|s| doop::pins(&s.host))
            .unwrap_or_default();
        self.read_agents();
        if self.session().is_some() {
            self.refresh(false, cx);
        }
        cx.notify();
    }

    /// Reads the instance's tools and canvases, then the picked canvas.
    /// `renders`: also download the renders again.
    fn refresh(&mut self, renders: bool, cx: &mut Context<Self>) {
        let Some(s) = self.session().cloned() else {
            return;
        };
        self.listing = true;
        self.error = None;
        self.list_epoch += 1;
        let list_epoch = self.list_epoch;
        self._load = Some(cx.spawn(async move |this, cx| {
            let got = cx
                .background_spawn(crate::guard::task(async move {
                    let tools = doop::tools(&s)?;
                    let listed = doop::canvases(&s, &tools)?;
                    Ok::<_, Error>((tools, listed))
                }))
                .await;
            this.update(cx, |this, cx| {
                if this.list_epoch != list_epoch {
                    return;
                }
                this.listing = false;
                match got {
                    Ok((tools, listed)) => {
                        // Fresh from now; a read that failed is tried again
                        // when the tab next opens.
                        this.read_at = crate::model::unix_now();
                        this.tools = Some(tools);
                        this.listed = listed;
                        let all = this.canvases();
                        if !all.iter().any(|(c, _)| Some(&c.id) == this.picked.as_ref()) {
                            this.epoch += 1;
                            this.picked = all.first().map(|(c, _)| c.id.clone());
                            this.shown = Shown::All;
                            this.canvas = None;
                            this.thumbs.clear();
                        }
                        this.read_canvas(renders, cx);
                    }
                    Err(e) => {
                        this.loading = false;
                        this.error = Some(e);
                    }
                }
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }

    /// Reads the frames of the picked canvas.
    fn read_canvas(&mut self, renders: bool, cx: &mut Context<Self>) {
        let (Some(s), Some(tools), Some(id)) = (
            self.session().cloned(),
            self.tools.clone(),
            self.picked.clone(),
        ) else {
            self.loading = false;
            return;
        };
        self.loading = true;
        self.error = None;
        let epoch = self.epoch;
        self._canvas = Some(cx.spawn(async move |this, cx| {
            let wanted = id.clone();
            let got = cx
                .background_spawn(crate::guard::task(async move {
                    doop::canvas(&s, &tools, &wanted)
                }))
                .await;
            this.update(cx, |this, cx| {
                if this.epoch != epoch || this.picked.as_ref() != Some(&id) {
                    return;
                }
                this.loading = false;
                match got {
                    Ok(canvas) => {
                        this.name_pin(&canvas);
                        // Keep the page shown if it is still there, else the first.
                        let groups = canvas.groups();
                        if !groups.iter().any(|(g, _, _)| *g == this.shown) {
                            this.shown =
                                groups.into_iter().next().map_or(Shown::All, |(g, _, _)| g);
                        }
                        this.canvas = Some(canvas);
                        this.read_thumbs(renders, cx);
                    }
                    Err(e) => this.error = Some(e),
                }
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }

    /// A canvas added by link is known only by its id until it is read.
    fn name_pin(&mut self, canvas: &Canvas) {
        let Some(host) = self.session().map(|s| s.host.clone()) else {
            return;
        };
        if canvas.name.is_empty() {
            return;
        }
        if let Some(pin) = self
            .pins
            .iter_mut()
            .find(|p| p.id == canvas.id && p.name != canvas.name)
        {
            // Only a nicer label: if it can't be saved, the id stays the name.
            let named = CanvasRef {
                name: canvas.name.clone(),
                ..pin.clone()
            };
            if doop::set_pin(&host, &canvas.id, Some(named.clone())).is_ok() {
                *pin = named;
            }
        }
    }

    /// Shows the renders on disk and downloads the missing and the old ones
    /// (all of them when `force`), a few at a time.
    fn read_thumbs(&mut self, force: bool, cx: &mut Context<Self>) {
        let (Some(s), Some(tools), Some(canvas)) =
            (self.session().cloned(), self.tools.clone(), &self.canvas)
        else {
            return;
        };
        // Only the page shown: a canvas can hold hundreds of frames.
        let mut wanted: Vec<Frame> = Vec::new();
        for f in canvas.frames_on(&self.shown) {
            match doop::cached_thumb(&s, f) {
                Some((path, due)) => {
                    self.thumbs.insert(f.id.clone(), Thumb::Ready(path));
                    if due || (force && f.version.is_none()) {
                        wanted.push(f.clone());
                    }
                }
                None => {
                    // A picture still shown here is of another version of
                    // the frame: it goes, even if the new one never arrives.
                    self.thumbs.insert(f.id.clone(), Thumb::Loading);
                    wanted.push(f.clone());
                }
            }
        }
        let epoch = self.epoch;
        let tools = Arc::new(tools);
        self._thumbs = Some(cx.spawn(async move |this, cx| {
            for group in wanted.chunks(AT_ONCE) {
                let running: Vec<_> = group
                    .iter()
                    .map(|frame| {
                        let (s, tools, frame) = (s.clone(), tools.clone(), frame.clone());
                        cx.background_spawn(crate::guard::task_or(
                            async move { doop::download_thumb(&s, &tools, &frame).ok() },
                            |_| None,
                        ))
                    })
                    .collect();
                let mut done = Vec::new();
                for (frame, task) in group.iter().zip(running) {
                    done.push((frame.id.clone(), task.await));
                }
                let alive = this.update(cx, |this, cx| {
                    if this.epoch != epoch {
                        return false;
                    }
                    for (frame, path) in done {
                        match path {
                            Some(p) => {
                                this.thumbs.insert(frame, Thumb::Ready(p));
                            }
                            // A render that failed to renew stays as it was.
                            None if this.thumbs.get(&frame) == Some(&Thumb::Loading) => {
                                this.thumbs.insert(frame, Thumb::Missing);
                            }
                            None => {}
                        }
                    }
                    cx.notify();
                    true
                });
                if !alive.unwrap_or(false) {
                    return;
                }
            }
        }));
    }

    /// Shows another page of the canvas.
    fn pick_page(&mut self, shown: Shown, cx: &mut Context<Self>) {
        if self.shown == shown {
            return;
        }
        self.shown = shown;
        self.read_thumbs(false, cx);
        cx.notify();
    }

    fn pick(&mut self, id: String, cx: &mut Context<Self>) {
        if self.picked.as_ref() == Some(&id) {
            return;
        }
        self.epoch += 1;
        self.picked = Some(id);
        self.shown = Shown::All;
        self.canvas = None;
        self.thumbs.clear();
        self._thumbs = None;
        self.read_canvas(false, cx);
    }

    /// Adds the canvas typed in the field, by its link or id.
    fn add_canvas(&mut self, cx: &mut Context<Self>) {
        let typed = self.canvas_input.read(cx).text().trim().to_string();
        let Some(host) = self.session().map(|s| s.host.clone()) else {
            return;
        };
        if typed.is_empty() {
            return;
        }
        let Some((its_host, id)) = doop::canvas_ref(&typed) else {
            self.add_note =
                Some("Paste a canvas link (…/c/bnj8ZjYM5d) or the id at its end.".into());
            cx.notify();
            return;
        };
        if let Some(other) = its_host.filter(|h| *h != host) {
            self.add_note = Some(format!(
                "That canvas lives on {}. Add that instance in Accounts first.",
                doop::host_label(&other)
            ));
            cx.notify();
            return;
        }
        if !self.canvases().iter().any(|(c, _)| c.id == id) {
            let pin = CanvasRef {
                id: id.clone(),
                name: String::new(),
            };
            // Not saved: the link stays in the field to try again.
            if let Err(e) = doop::set_pin(&host, &id, Some(pin.clone())) {
                self.add_note = Some(e);
                cx.notify();
                return;
            }
            self.pins.push(pin);
        }
        self.canvas_input.update(cx, |i, cx| i.set_text("", cx));
        self.add_note = None;
        self.pick(id, cx);
        cx.notify();
    }

    fn remove_canvas(&mut self, id: String, cx: &mut Context<Self>) {
        let Some(host) = self.session().map(|s| s.host.clone()) else {
            return;
        };
        if let Err(e) = doop::set_pin(&host, &id, None) {
            self.add_note = Some(e);
            cx.notify();
            return;
        }
        self.pins.retain(|p| p.id != id);
        if self.picked.as_ref() == Some(&id) {
            // Whatever was being read for it no longer has a place to land.
            self.epoch += 1;
            self.loading = false;
            self._canvas = None;
            self._thumbs = None;
            self.picked = None;
            self.shown = Shown::All;
            self.canvas = None;
            self.thumbs.clear();
            self.error = None;
            if let Some((next, _)) = self.canvases().into_iter().next() {
                self.pick(next.id, cx);
            }
        }
        cx.notify();
    }

    fn open(&self, frame: Option<&str>, cx: &mut Context<Self>) {
        let Some(s) = self.session() else { return };
        let url = match &self.picked {
            Some(c) => doop::canvas_url(&s.host, c, frame),
            None => s.host.clone(),
        };
        cx.emit(DoopEvent::Open(url));
    }
}

fn notice(icon_name: &str, tone: Tone, text: String) -> AnyElement {
    div()
        .flex()
        .items_center()
        .gap(crate::ui::sp(8.))
        .px(crate::ui::sp(12.))
        .py(crate::ui::sp(8.))
        .rounded(crate::ui::rad(ui::RADIUS))
        .bg(tone.color().opacity(0.1))
        .child(icon(icon_name, 14.).text_color(tone.color()))
        .child(ui::text(Type::Body, theme::text_muted(), text))
        .into_any_element()
}

/// "12 frames", "1 frame".
fn frames_label(n: usize) -> String {
    format!("{n} frame{}", if n == 1 { "" } else { "s" })
}

/// A canvas as the list names it: its name, or its id until it was read.
fn canvas_label(c: &CanvasRef) -> String {
    if c.name.is_empty() {
        c.id.clone()
    } else {
        c.name.clone()
    }
}

impl DoopView {
    fn render_header(&self, cx: &mut Context<Self>) -> AnyElement {
        let now = crate::model::unix_now();
        let subtitle: AnyElement = match self.session() {
            Some(s) => div()
                .id("doop-switch")
                .flex()
                .items_center()
                .gap(crate::ui::sp(6.))
                .px(crate::ui::sp(6.))
                .ml(px(-6.))
                .h(px(ui::BUTTON_H))
                .rounded(crate::ui::rad(ui::RADIUS))
                .cursor_pointer()
                .hover(|d| d.bg(theme::fill_hover()))
                .child(ui::text(
                    Type::Body,
                    theme::text_muted(),
                    format!("{} · ", crate::redact::value(&s.who())),
                ))
                .child(ui::text(
                    Type::Body,
                    theme::text(),
                    doop::host_label(&s.host).to_string(),
                ))
                .when(s.self_hosted(), |d| {
                    d.child(ui::badge("self-hosted", Tone::Neutral))
                })
                .child(icon("unfold-more", 14.).text_color(theme::text_subtle()))
                .on_click(cx.listener(|this, ev: &gpui::ClickEvent, _, cx| {
                    this.menu = if this.menu.is_some() {
                        None
                    } else {
                        Some(ev.position() + gpui::point(px(-8.), px(14.)))
                    };
                    cx.notify();
                }))
                .into_any_element(),
            None => ui::text(
                Type::Body,
                theme::text_subtle(),
                "Design canvases and their frames",
            )
            .into_any_element(),
        };
        let updated = self
            .canvas
            .as_ref()
            .map(|c| crate::memory::ago(c.fetched_at, now));
        let refresh = ui::refresh_control(
            "doop-refresh",
            self.loading || self.listing,
            updated,
            cx.listener(|this, _, _, cx| this.refresh(true, cx)),
        );
        let accounts = ui::button_with(
            "doop-accounts",
            Some("user"),
            if self.page == Page::Accounts {
                "Back to the canvases"
            } else {
                "Accounts"
            },
            ButtonKind::Outline,
            true,
        )
        .on_click(cx.listener(|this, _, _, cx| {
            this.page = if this.page == Page::Accounts {
                Page::Main
            } else {
                Page::Accounts
            };
            this.menu = None;
            this.read_agents();
            cx.notify();
        }));
        let open = self.session().map(|_| {
            ui::button_with(
                "doop-open",
                Some("link-square02"),
                "Open in Doop",
                ButtonKind::Outline,
                true,
            )
            .on_click(cx.listener(|this, _, _, cx| this.open(None, cx)))
        });
        let signed_in = !self.sessions.is_empty();
        div()
            .flex()
            .flex_none()
            .items_end()
            .justify_between()
            .gap(crate::ui::sp(16.))
            .px(crate::ui::sp(ui::PAGE_PAD))
            .pt(crate::ui::sp(ui::PAGE_PAD))
            .pb(crate::ui::sp(18.))
            .child(ui::page_title("Doop", Some(subtitle)))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(crate::ui::sp(8.))
                    .children(open)
                    .when(signed_in, |d| d.child(accounts).child(refresh)),
            )
            .into_any_element()
    }

    /// The sign-ins to switch between.
    fn render_menu(&self, cx: &mut Context<Self>) -> AnyElement {
        let mut panel = ui::menu_panel("doop-menu", 340.);
        for (i, s) in self.sessions.iter().enumerate() {
            let on = self.current.as_ref() == Some(&s.id);
            let id = s.id.clone();
            panel = panel.child(
                ui::choice_row(("doop-menu-row", i), on)
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .child(crate::redact::value(&s.who())),
                    )
                    .child(ui::text(
                        Type::Caption,
                        theme::text_faint(),
                        doop::host_label(&s.host).to_string(),
                    ))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if this.current.as_ref() != Some(&id) {
                            this.switch(Some(id.clone()), cx);
                        }
                        this.menu = None;
                        cx.notify();
                    })),
            );
        }
        panel
            .child(ui::menu_divider())
            .child(
                ui::menu_item("doop-menu-accounts", "user", "Accounts…", false, true).on_click(
                    cx.listener(|this, _, _, cx| {
                        this.menu = None;
                        this.page = Page::Accounts;
                        this.read_agents();
                        cx.notify();
                    }),
                ),
            )
            .into_any_element()
    }

    /// The canvases at the left, and the field that adds one by its link.
    fn render_canvases(&self, cx: &mut Context<Self>) -> AnyElement {
        let mut rows = div().flex().flex_col().gap(crate::ui::sp(1.));
        for (i, (c, added)) in self.canvases().into_iter().enumerate() {
            let on = self.picked.as_ref() == Some(&c.id);
            let (pick, remove) = (c.id.clone(), c.id.clone());
            rows = rows.child(
                ui::choice_row(("doop-canvas", i), on)
                    .child(div().flex_1().min_w_0().truncate().child(canvas_label(&c)))
                    .when(added, |d| {
                        d.child(
                            ui::icon_button(("doop-canvas-remove", i), "cancel01", 12.).on_click(
                                cx.listener(move |this, _, _, cx| {
                                    cx.stop_propagation();
                                    this.remove_canvas(remove.clone(), cx);
                                }),
                            ),
                        )
                    })
                    .on_click(cx.listener(move |this, _, _, cx| this.pick(pick.clone(), cx))),
            );
        }
        let field = ui::field()
            .flex_1()
            .min_w_0()
            .child(icon("link-square02", 14.).text_color(theme::text_subtle()))
            .child(self.canvas_input.clone());
        div()
            .flex()
            .flex_none()
            .flex_col()
            .gap(crate::ui::sp(12.))
            .w(px(264.))
            .child(ui::text(Type::Label, theme::text_subtle(), "Canvases"))
            .child(rows)
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(crate::ui::sp(8.))
                    .child(field)
                    .child(
                        ui::button("doop-canvas-add", "Add", ButtonKind::Outline, true)
                            .on_click(cx.listener(|this, _, _, cx| this.add_canvas(cx))),
                    ),
            )
            .child(ui::text(
                Type::Caption,
                theme::text_faint(),
                self.add_note.clone().unwrap_or_else(|| {
                    if self.listed.is_some() {
                        "A canvas someone shared with you: add it by its link.".to_string()
                    } else {
                        "This instance doesn't list its canvases. Add each one by its link."
                            .to_string()
                    }
                }),
            ))
            .into_any_element()
    }

    fn render_frame(&self, i: usize, f: &Frame, cx: &mut Context<Self>) -> AnyElement {
        let picture: AnyElement = match self.thumbs.get(&f.id) {
            Some(Thumb::Ready(path)) => img(path.clone())
                .size_full()
                .object_fit(ObjectFit::Contain)
                .into_any_element(),
            Some(Thumb::Loading) => {
                ui::spinner(("doop-frame-spin", i), 14., theme::text_faint()).into_any_element()
            }
            _ => icon("image01", 18.)
                .text_color(theme::text_faint())
                .into_any_element(),
        };
        let size = f.size.map(|(w, h)| format!("{w} × {h}"));
        let id = f.id.clone();
        div()
            .id(("doop-frame", i))
            .flex()
            .flex_col()
            .w(px(CARD.0))
            .rounded(crate::ui::rad(ui::RADIUS_LG))
            .border_1()
            .border_color(theme::border_subtle())
            .overflow_hidden()
            .cursor_pointer()
            .hover(|d| d.border_color(theme::border_strong()))
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_center()
                    .h(px(CARD.1))
                    .bg(theme::fill_raised())
                    .child(picture),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap(crate::ui::sp(8.))
                    .px(crate::ui::sp(12.))
                    .py(crate::ui::sp(8.))
                    .border_t_1()
                    .border_color(theme::border_subtle())
                    .child(ui::text(Type::Body, theme::text(), f.name.clone()).truncate())
                    .children(
                        size.map(|s| ui::text(Type::Caption, theme::text_faint(), s).flex_none()),
                    ),
            )
            .on_click(cx.listener(move |this, _, _, cx| this.open(Some(&id), cx)))
            .into_any_element()
    }

    /// What went wrong, and the one thing to do about it.
    fn render_error(&self, e: &Error, cx: &mut Context<Self>) -> AnyElement {
        let state = match e {
            Error::SignedOut(why) => ui::empty_state(
                "square-lock02",
                "Doop refused this sign-in",
                &format!("{why} Sign in to the instance again."),
                Some(
                    ui::button("doop-reauth", "Accounts", ButtonKind::Primary, true)
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.page = Page::Accounts;
                            cx.notify();
                        }))
                        .into_any_element(),
                ),
            ),
            Error::Failed(why) => ui::empty_state(
                "alert-circle",
                "Couldn't read Doop",
                why,
                Some(
                    ui::button("doop-retry", "Try again", ButtonKind::Outline, true)
                        .on_click(cx.listener(|this, _, _, cx| this.refresh(false, cx)))
                        .into_any_element(),
                ),
            ),
        };
        div()
            .flex()
            .justify_center()
            .pt(crate::ui::sp(24.))
            .child(state)
            .into_any_element()
    }

    /// The frames of the picked canvas.
    fn render_canvas(&self, cx: &mut Context<Self>) -> AnyElement {
        if let Some(e) = &self.error {
            return self.render_error(e, cx);
        }
        let Some(canvas) = &self.canvas else {
            if self.loading || self.listing {
                return ui::loading("doop-loading", "Reading the canvas…").into_any_element();
            }
            return ui::empty_state(
                "paint-board",
                "No canvas yet",
                "Paste the link of a canvas at the left to see its frames here.",
                None,
            )
            .into_any_element();
        };
        let name = if canvas.name.is_empty() {
            canvas.id.clone()
        } else {
            canvas.name.clone()
        };
        let shown = canvas.frames_on(&self.shown);
        let mut grid = div().flex().flex_wrap().gap(crate::ui::sp(16.));
        for (i, f) in shown.iter().enumerate() {
            grid = grid.child(self.render_frame(i, f, cx));
        }
        let groups = canvas.groups();
        let strip = (groups.len() > 1).then(|| {
            let mut strip = div().flex().flex_wrap().gap(crate::ui::sp(6.));
            for (i, (group, name, count)) in groups.into_iter().enumerate() {
                let on = self.shown == group;
                strip = strip.child(
                    ui::segment(("doop-page", i), None, name, on)
                        .child(
                            div()
                                .ty(Type::Caption)
                                .font_family(theme::FONT_MONO)
                                .text_color(theme::text_faint())
                                .child(count.to_string()),
                        )
                        .on_click(
                            cx.listener(move |this, _, _, cx| this.pick_page(group.clone(), cx)),
                        ),
                );
            }
            strip
        });
        div()
            .flex()
            .flex_col()
            .gap(crate::ui::sp(16.))
            .child(
                div()
                    .flex()
                    .items_baseline()
                    .gap(crate::ui::sp(10.))
                    .child(ui::heading(name))
                    .child(ui::text(
                        Type::Caption,
                        theme::text_faint(),
                        frames_label(canvas.frames.len()),
                    )),
            )
            .children(strip)
            .when(canvas.frames.is_empty(), |d| {
                d.child(ui::text(
                    Type::Body,
                    theme::text_subtle(),
                    "This canvas has no frames yet.",
                ))
            })
            .child(grid)
            .into_any_element()
    }

    fn render_body(&self, cx: &mut Context<Self>) -> AnyElement {
        if self.sessions.is_empty() {
            return div()
                .flex()
                .justify_center()
                .pt(crate::ui::sp(24.))
                .child(self.render_connect(true, cx))
                .into_any_element();
        }
        if self.page == Page::Accounts {
            return self.render_accounts(cx);
        }
        // Nothing read yet: the tools come before anything else can show.
        if self.tools.is_none() {
            return match &self.error {
                Some(e) => self.render_error(e, cx),
                None => ui::loading("doop-reading", "Reading Doop…").into_any_element(),
            };
        }
        div()
            .flex()
            .items_start()
            .gap(crate::ui::sp(32.))
            .child(self.render_canvases(cx))
            .child(div().flex_1().min_w_0().child(self.render_canvas(cx)))
            .into_any_element()
    }
}

impl gpui::Focusable for DoopView {
    fn focus_handle(&self, _: &gpui::App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for DoopView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let header = self.render_header(cx);
        let body = self.render_body(cx);
        let menu = self.menu.filter(|_| self.session().is_some());
        div()
            .track_focus(&self.focus)
            .relative()
            .flex()
            .flex_col()
            .size_full()
            .bg(theme::background())
            .font_family(theme::FONT_SANS)
            .text_color(theme::text())
            .child(header)
            .child(
                div()
                    .id("doop-body")
                    .track_scroll(&self.scroll)
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .px(crate::ui::sp(ui::PAGE_PAD))
                    .pb(crate::ui::sp(32.))
                    .child(div().w_full().max_w(px(1320.)).child(body)),
            )
            .when_some(menu, |d, at| {
                d.child(
                    div()
                        .id("doop-menu-backdrop")
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
    fn labels_read_plainly() {
        assert_eq!(frames_label(1), "1 frame");
        assert_eq!(frames_label(22), "22 frames");
        let named = CanvasRef {
            id: "bnj8ZjYM5d".into(),
            name: "Brandbook".into(),
        };
        assert_eq!(canvas_label(&named), "Brandbook");
        let bare = CanvasRef {
            id: "bnj8ZjYM5d".into(),
            name: String::new(),
        };
        assert_eq!(canvas_label(&bare), "bnj8ZjYM5d");
    }
}
