//! The Linear tab: the issues assigned to you, in whichever workspace you
//! pick. Every sign-in (`linear::Session`) stays saved; the header names the
//! workspace in view and switches between them, and `accounts` adds and
//! removes them.
//!
//! In this module "session" is a Linear sign-in. The interface says sign-in
//! or workspace, since a session in Arakun is an agent's.

mod accounts;

use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, AppContext as _, Context, Entity, EventEmitter, FocusHandle,
    InteractiveElement as _, IntoElement, MouseButton, ParentElement as _, Pixels, Point, Render,
    StatefulInteractiveElement as _, Styled as _, Subscription, Window, div, px,
};

use crate::input::{InputEvent, TextInput};
use crate::linear::{self, ApiError, Session};
use crate::loadable::Loadable;
use crate::theme;
use crate::ui::{self, ButtonKind, Type, icon};

#[allow(dead_code)] // Emitted by the issue list (task 5).
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
    _input: Subscription,
}

impl EventEmitter<LinearEvent> for LinearView {}

impl LinearView {
    pub fn new(cx: &mut Context<Self>) -> Self {
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
        Self {
            sessions: linear::sessions(),
            current: None,
            page: Page::Main,
            key_input,
            signing_in: Loadable::default(),
            menu: None,
            focus: cx.focus_handle(),
            _input: sub,
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

    /// Opening the tab: a sign-in added or removed elsewhere shows up.
    pub fn refresh_if_stale(&mut self, cx: &mut Context<Self>) {
        self.reload(cx);
    }

    fn reload(&mut self, cx: &mut Context<Self>) {
        self.sessions = linear::sessions();
        self.current = self.session().map(|s| s.id.clone());
        cx.notify();
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
        self.current = Some(id);
        cx.notify();
    }

    fn render_header(&self, cx: &mut Context<Self>) -> AnyElement {
        let subtitle: AnyElement = match self.session() {
            Some(s) => div()
                .id("lin-switch")
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
        let refresh = ui::refresh_control(
            "lin-refresh",
            self.signing_in.loading(),
            None,
            cx.listener(|this, _, _, cx| this.reload(cx)),
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
            Page::Main => ui::empty_state(
                "check-list",
                "Your issues",
                "The issues assigned to you show here.",
                None,
            )
            .into_any_element(),
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
                    .id("lin-body")
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
}
