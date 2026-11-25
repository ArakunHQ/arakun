//! The Linear tab: the issues assigned to you, in whichever workspace you
//! pick. Every sign-in (`linear::Session`) stays saved; the header names the
//! workspace in view and switches between them, and `accounts` adds and
//! removes them.
//!
//! In this module "session" is a Linear sign-in. The interface says sign-in
//! or workspace, since a session in Arakun is an agent's.

mod accounts;
mod list;
mod sheet;

use std::collections::HashMap;

use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, AppContext as _, Context, Entity, EventEmitter, FocusHandle,
    InteractiveElement as _, IntoElement, KeyDownEvent, MouseButton, ParentElement as _, Pixels,
    Point, Render, Role, ScrollHandle, StatefulInteractiveElement as _, Styled as _, Subscription,
    Window, div, px,
};

use crate::agent::markdown::Block;
use crate::input::{InputEvent, TextInput};
use crate::linear::issues::{Assigned, Detail};
use crate::linear::{self, ApiError, Session};
use crate::loadable::Loadable;
use crate::theme;
use crate::ui::{self, ButtonKind, Type, icon};

/// Opening the tab reads the list again once it is this old (seconds).
const STALE: u64 = 5 * 60;
/// The refresh button reads at most this often (seconds).
const GAP: u64 = 30;

pub enum LinearEvent {
    /// Open a Linear page.
    Open(String),
    /// Start an agent session on an issue.
    Ask { title: String, prompt: String },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Page {
    Main,
    Accounts,
}

/// With nobody signed in there is only the form; otherwise the page asked for.
fn page_for(sessions: &[Session], page: Page) -> Page {
    if sessions.is_empty() {
        Page::Accounts
    } else {
        page
    }
}

/// Whether to read the list now. `read_at` is when the last answer came,
/// `asked_at` when the last read started, `manual` the refresh button, and
/// `until` the end of a rate-limit wait.
fn may_read(now: u64, read_at: Option<u64>, asked_at: u64, manual: bool, until: u64) -> bool {
    if now < until {
        return false;
    }
    if manual {
        now.saturating_sub(asked_at) >= GAP
    } else {
        read_at.is_none_or(|t| now.saturating_sub(t) >= STALE)
    }
}

/// A failed read, as the tab says it.
fn error_line(e: &ApiError) -> String {
    match e {
        ApiError::SignedOut => {
            "This Linear sign-in no longer works. Sign in again under Accounts.".into()
        }
        ApiError::Limited { .. } => {
            "Linear's request limit is reached. Try again in a moment.".into()
        }
        ApiError::Failed(m) => m.clone(),
    }
}

/// The session in view: the one picked while it is still saved, else the first.
fn pick<'a>(sessions: &'a [Session], current: Option<&str>) -> Option<&'a Session> {
    current
        .and_then(|id| sessions.iter().find(|s| s.id == id))
        .or_else(|| sessions.first())
}

pub struct LinearView {
    /// With their keys: never rendered, only handed to `linear`.
    sessions: Vec<Session>,
    /// The id of the session in view.
    current: Option<String>,
    page: Page,
    key_input: Entity<TextInput>,
    signing_in: Loadable<Session, ApiError>,
    menu: Option<Point<Pixels>>,
    focus: FocusHandle,
    /// The issues of the session in view.
    assigned: Loadable<Assigned, ApiError>,
    /// When the last read of the list started, for the refresh button's gap.
    asked_at: u64,
    /// When the list in view arrived.
    read_at: Option<u64>,
    /// Per session id, the unix second Linear asked us to wait until.
    limited: HashMap<String, u64>,
    search: Entity<TextInput>,
    /// The issue the arrow keys are on.
    picked: Option<String>,
    list_scroll: ScrollHandle,
    /// The issue id the sheet shows.
    open: Option<String>,
    detail: Loadable<Detail, ApiError>,
    /// The detail's description, then each comment, parsed once.
    detail_md: Vec<Vec<Block>>,
    _subs: Vec<Subscription>,
}

impl EventEmitter<LinearEvent> for LinearView {}

impl LinearView {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let key_input = cx.new(|cx| TextInput::new("lin_api_…", cx).mono().secret());
        let sub = cx.subscribe(
            &key_input,
            |this: &mut Self, _, ev: &InputEvent, cx| match ev {
                InputEvent::Submit => this.sign_in(cx),
                InputEvent::Changed => {
                    this.signing_in.error = None;
                    cx.notify();
                }
                _ => {}
            },
        );
        let search = cx.new(|cx| TextInput::new("Search by id, title or team", cx));
        let search_sub = cx.subscribe_in(
            &search,
            window,
            |this: &mut Self, input, ev: &InputEvent, window, cx| match ev {
                InputEvent::Changed => cx.notify(),
                InputEvent::Down => {
                    window.focus(&this.focus, cx);
                    this.step_pick(1, cx);
                }
                InputEvent::Submit => {
                    if this.picked.is_none() {
                        this.step_pick(1, cx);
                    }
                    if let Some(id) = this.picked.clone() {
                        this.open_issue(id, window, cx);
                    }
                }
                InputEvent::Cancel => {
                    if input.read(cx).text().is_empty() {
                        window.focus(&this.focus, cx);
                    } else {
                        input.update(cx, |i, cx| i.set_text("", cx));
                    }
                }
                _ => {}
            },
        );
        Self {
            sessions: linear::sessions(),
            current: None,
            page: Page::Main,
            key_input,
            signing_in: Loadable::default(),
            menu: None,
            focus: cx.focus_handle(),
            assigned: Loadable::default(),
            asked_at: 0,
            read_at: None,
            limited: HashMap::new(),
            search,
            picked: None,
            list_scroll: ScrollHandle::new(),
            open: None,
            detail: Loadable::default(),
            detail_md: Vec::new(),
            _subs: vec![sub, search_sub],
        }
    }

    pub fn focus(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        // The form takes the keys when it is all there is.
        if self.page() == Page::Accounts {
            let focus = gpui::Focusable::focus_handle(self.key_input.read(cx), cx);
            window.focus(&focus, cx);
        } else {
            window.focus(&self.focus, cx);
        }
    }

    /// Opening the tab: a sign-in added or removed elsewhere shows up, and
    /// the list is read if it is old.
    pub fn refresh_if_stale(&mut self, cx: &mut Context<Self>) {
        self.reload(cx);
        self.read(false, cx);
    }

    /// Reads the saved sign-ins again. Another workspace in view starts
    /// from an empty list.
    fn reload(&mut self, cx: &mut Context<Self>) {
        let before = self.current_id();
        self.sessions = linear::sessions();
        self.current = self.current_id();
        if self.current != before {
            self.clear();
        }
        cx.notify();
    }

    fn current_id(&self) -> Option<String> {
        self.session().map(|s| s.id.clone())
    }

    /// Forgets the list and the sheet, for another workspace.
    fn clear(&mut self) {
        self.assigned.reset();
        self.detail.reset();
        self.detail_md.clear();
        self.open = None;
        self.picked = None;
        self.asked_at = 0;
        self.read_at = None;
    }

    /// The end of the rate-limit wait for `session`, or 0.
    fn until(&self, session: &str) -> u64 {
        self.limited.get(session).copied().unwrap_or(0)
    }

    /// Remembers a rate limit, so nothing reads as this session until it ends.
    fn note_limit<T>(&mut self, session: &str, got: &Result<T, ApiError>) {
        if let Err(ApiError::Limited { until }) = got {
            self.limited.insert(session.to_string(), *until);
        }
    }

    /// Reads the issues assigned in the workspace in view. `manual` is the
    /// refresh button. While Linear's limit holds, only says so.
    fn read(&mut self, manual: bool, cx: &mut Context<Self>) {
        let Some(s) = self.session().cloned() else {
            return;
        };
        if self.assigned.loading() {
            return;
        }
        let now = crate::model::unix_now();
        let until = self.until(&s.id);
        if !may_read(now, self.read_at, self.asked_at, manual, until) {
            if now < until {
                self.assigned.error = Some(ApiError::Limited { until });
                cx.notify();
            }
            return;
        }
        self.asked_at = now;
        let id = s.id.clone();
        self.assigned.start(
            cx,
            |this| &mut this.assigned,
            async move { linear::issues::assigned(&s) },
            move |this, got, _| {
                if this.current_id().as_deref() != Some(id.as_str()) {
                    return;
                }
                this.note_limit(&id, &got);
                if got.is_ok() {
                    this.read_at = Some(crate::model::unix_now());
                }
                this.assigned.settle(got);
            },
        );
    }

    fn page(&self) -> Page {
        page_for(&self.sessions, self.page)
    }

    fn session(&self) -> Option<&Session> {
        pick(&self.sessions, self.current.as_deref())
    }

    fn switch(&mut self, id: String, cx: &mut Context<Self>) {
        self.menu = None;
        self.page = Page::Main;
        if self.current_id().as_deref() != Some(id.as_str()) {
            self.current = Some(id);
            self.clear();
        }
        self.read(false, cx);
        cx.notify();
    }

    fn on_key(&mut self, ev: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let k = &ev.keystroke;
        // Keys typed in an input are text, not shortcuts.
        if k.modifiers.control
            || k.modifiers.alt
            || k.modifiers.platform
            || !self.focus.is_focused(window)
            || self.page() != Page::Main
        {
            return;
        }
        let sheet = self.open.is_some();
        match k.key.as_str() {
            "escape" if sheet => self.close_sheet(window, cx),
            "escape" if self.menu.is_some() => {
                self.menu = None;
                cx.notify();
            }
            "j" | "down" if !sheet => self.step_pick(1, cx),
            "k" | "up" if !sheet => self.step_pick(-1, cx),
            "enter" if !sheet && self.picked.is_some() => {
                if let Some(id) = self.picked.clone() {
                    self.open_issue(id, window, cx);
                }
            }
            "/" if !sheet => {
                let focus = gpui::Focusable::focus_handle(self.search.read(cx), cx);
                window.focus(&focus, cx);
            }
            _ => return,
        }
        cx.stop_propagation();
    }

    fn render_header(&self, cx: &mut Context<Self>) -> AnyElement {
        let now = crate::model::unix_now();
        let subtitle: AnyElement = match self.session() {
            Some(s) => ui::labelled(div().id("lin-switch"), Role::Button, "Switch workspace")
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
                    format!("{} · ", crate::redact::value(&s.email)),
                ))
                .child(ui::text(
                    Type::Body,
                    theme::text(),
                    crate::redact::value(&s.workspace),
                ))
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
            None => {
                ui::text(Type::Body, theme::text_subtle(), "Your Linear issues").into_any_element()
            }
        };
        let accounts = ui::button_with(
            "lin-accounts",
            Some("user"),
            if self.page() == Page::Accounts {
                "Back to the issues"
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
            cx.notify();
        }));
        let updated = self
            .read_at
            .filter(|_| self.session().is_some())
            .map(|t| crate::memory::ago(t, now));
        let refresh = ui::refresh_control(
            "lin-refresh",
            self.assigned.loading(),
            updated,
            cx.listener(|this, _, _, cx| {
                this.reload(cx);
                this.read(true, cx);
            }),
        );
        div()
            .flex()
            .flex_none()
            .items_end()
            .justify_between()
            .gap(crate::ui::sp(16.))
            .px(crate::ui::sp(ui::PAGE_PAD))
            .pt(crate::ui::sp(ui::PAGE_PAD))
            .pb(crate::ui::sp(18.))
            .child(ui::page_title("Linear", Some(subtitle)))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(crate::ui::sp(8.))
                    .when(!self.sessions.is_empty(), |d| d.child(accounts))
                    .child(refresh),
            )
            .into_any_element()
    }

    /// The workspaces to switch between, then the accounts page.
    fn render_menu(&self, cx: &mut Context<Self>) -> AnyElement {
        let on = self.session().map(|s| s.id.clone());
        let rows = self.sessions.iter().enumerate().map(|(i, s)| {
            let id = s.id.clone();
            let label = format!(
                "{} · {}",
                crate::redact::value(&s.workspace),
                crate::redact::value(&s.email)
            );
            ui::choice_row(
                ("lin-menu-row", i),
                label.clone(),
                on.as_ref() == Some(&s.id),
            )
            .child(div().flex_1().min_w_0().truncate().child(label))
            .on_click(cx.listener(move |this, _, _, cx| this.switch(id.clone(), cx)))
        });
        ui::menu_panel("lin-menu", 320.)
            .child(ui::menu_heading("Workspace"))
            .children(rows)
            .child(ui::menu_divider())
            .child(
                ui::menu_item("lin-menu-accounts", "user", "Accounts…", false, true).on_click(
                    cx.listener(|this, _, _, cx| {
                        this.menu = None;
                        this.page = Page::Accounts;
                        cx.notify();
                    }),
                ),
            )
            .into_any_element()
    }

    fn render_body(&self, cx: &mut Context<Self>) -> AnyElement {
        match self.page() {
            Page::Accounts => self.render_accounts(cx),
            Page::Main => self.render_list(cx),
        }
    }
}

impl gpui::Focusable for LinearView {
    fn focus_handle(&self, _: &gpui::App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for LinearView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let header = self.render_header(cx);
        let body = self.render_body(cx);
        let menu = self.menu.filter(|_| self.session().is_some());
        let sheet = self.render_sheet(cx);
        let body = match self.page() {
            // The list scrolls on its own, under its search box.
            Page::Main => div().flex_1().min_h_0().child(body),
            Page::Accounts => div().flex_1().min_h_0().child(
                div()
                    .id("lin-body")
                    .size_full()
                    .overflow_y_scroll()
                    .px(crate::ui::sp(ui::PAGE_PAD))
                    .pb(crate::ui::sp(32.))
                    .child(div().w_full().max_w(px(1320.)).child(body)),
            ),
        };
        div()
            .id("linear-view")
            .track_focus(&self.focus)
            .on_key_down(cx.listener(Self::on_key))
            .relative()
            .flex()
            .flex_col()
            .size_full()
            .bg(theme::background())
            .font_family(theme::FONT_SANS)
            .text_color(theme::text())
            .child(header)
            .child(body)
            .children(sheet)
            .when_some(menu, |d, at| {
                d.child(
                    div()
                        .id("lin-menu-backdrop")
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
    use crate::linear::Credential;

    fn session_with(id: &str) -> Session {
        Session {
            id: id.into(),
            user_id: "u1".into(),
            name: "Rik".into(),
            email: "r@x.dev".into(),
            workspace_id: "w1".into(),
            workspace: "Acme".into(),
            url_key: "acme".into(),
            credential: Credential::ApiKey { key: "k".into() },
            added_at: 1,
        }
    }

    fn session() -> Session {
        session_with("w1#u1")
    }

    #[test]
    fn the_page_follows_the_sign_ins() {
        assert_eq!(page_for(&[], Page::Main), Page::Accounts); // nobody signed in: the form
        assert_eq!(page_for(&[session()], Page::Main), Page::Main);
        assert_eq!(page_for(&[session()], Page::Accounts), Page::Accounts); // opened by hand: stays
    }

    #[test]
    fn the_current_session_survives_a_reload_or_falls_back() {
        let (a, b) = (session_with("w1#u1"), session_with("w2#u1"));
        assert_eq!(
            pick(&[a.clone(), b.clone()], Some("w2#u1")).unwrap().id,
            "w2#u1"
        );
        assert_eq!(
            pick(std::slice::from_ref(&a), Some("gone")).unwrap().id,
            "w1#u1"
        );
        assert!(pick(&[], Some("w1#u1")).is_none());
    }

    #[test]
    fn errors_read_plainly() {
        assert_eq!(
            error_line(&ApiError::SignedOut),
            "This Linear sign-in no longer works. Sign in again under Accounts."
        );
        assert_eq!(
            error_line(&ApiError::Limited { until: 0 }),
            "Linear's request limit is reached. Try again in a moment."
        );
    }

    #[test]
    fn the_list_is_read_when_old_or_asked_for_but_not_while_limited() {
        let now = 10_000;
        // Opening the tab: never read, or older than STALE.
        assert!(may_read(now, None, 0, false, 0));
        assert!(may_read(now, Some(now - STALE), now - STALE, false, 0));
        assert!(!may_read(now, Some(now - 60), now - 60, false, 0));
        // The refresh button: at most every GAP.
        assert!(may_read(now, Some(now - 60), now - GAP, true, 0));
        assert!(!may_read(now, Some(now - 60), now - 5, true, 0));
        // Limited: nothing until `until`, by either way.
        assert!(!may_read(now, None, 0, false, now + 1));
        assert!(!may_read(now, None, 0, true, now + 1));
        assert!(may_read(now, None, 0, true, now));
    }
}
