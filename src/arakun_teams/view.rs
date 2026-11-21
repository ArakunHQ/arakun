//! The Teams tab. For now a placeholder that says who is signed in; team chat
//! comes next.

use gpui::prelude::FluentBuilder as _;
use gpui::{
    Context, Entity, EventEmitter, FocusHandle, InteractiveElement as _, IntoElement,
    ParentElement as _, Render, StatefulInteractiveElement as _, Styled as _, Subscription, Window,
    div, px,
};

use super::Teams;
use crate::theme;
use crate::ui::{self, ButtonKind};

pub enum TeamsViewEvent {
    /// Settings → Account, to sign in again.
    OpenSettings,
}

pub struct TeamsView {
    teams: Entity<Teams>,
    focus: FocusHandle,
    _teams: Subscription,
}

impl EventEmitter<TeamsViewEvent> for TeamsView {}

impl TeamsView {
    pub fn new(teams: Entity<Teams>, cx: &mut Context<Self>) -> Self {
        Self {
            _teams: cx.observe(&teams, |_, _, cx| cx.notify()),
            teams,
            focus: cx.focus_handle(),
        }
    }

    pub fn focus(&self, window: &mut Window, cx: &mut Context<Self>) {
        window.focus(&self.focus, cx);
    }
}

impl Render for TeamsView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let teams = self.teams.read(cx);
        let login = teams
            .me()
            .filter(|_| teams.enabled())
            .map(|m| crate::redact::value(&m.login));
        let subtitle = login
            .as_ref()
            .map(|l| format!("Signed in as @{l}"))
            .unwrap_or_else(|| "Signed out".into());
        let body = if login.is_some() {
            ui::empty_state(
                "message-multiple01",
                "Nothing here yet",
                "Teams is on for your account. Team channels come in a later version of Arakun.",
                None,
            )
        } else {
            // The tab can stay open after a sign-out, or after the server took
            // the flag away.
            ui::empty_state(
                "user",
                "Teams is not on here",
                "Sign in with GitHub in Settings, with an account that has Teams.",
                Some(
                    ui::button(
                        "teams-open-settings",
                        "Open Settings",
                        ButtonKind::Outline,
                        true,
                    )
                    .on_click(cx.listener(|_, _, _, cx| cx.emit(TeamsViewEvent::OpenSettings)))
                    .into_any_element(),
                ),
            )
        };
        let signed_in = login.is_some();
        div()
            .track_focus(&self.focus)
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
                    .items_start()
                    .justify_between()
                    .gap(ui::sp(16.))
                    .px(ui::sp(ui::PAGE_PAD))
                    .pt(ui::sp(ui::PAGE_PAD))
                    .pb(ui::sp(12.))
                    .border_b_1()
                    .border_color(theme::stroke())
                    .child(ui::page_title("Teams", Some(subtitle.into_any_element())))
                    .when(signed_in, |d| {
                        d.child(
                            ui::button("teams-sign-out", "Sign out", ButtonKind::Outline, true)
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.teams.update(cx, |t, cx| t.sign_out(cx));
                                })),
                        )
                    }),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_h_0()
                    .items_center()
                    .justify_center()
                    .px(ui::sp(ui::PAGE_PAD))
                    .child(div().max_w(px(420.)).child(body)),
            )
    }
}
