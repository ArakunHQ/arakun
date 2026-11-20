//! The "Sign in with GitHub" dialog: GitHub's device code and what to do
//! with it, then how it ended. The flow itself runs in [`Teams`]; this only
//! draws where it stands.

use std::time::{Duration, Instant};

use gpui::prelude::FluentBuilder as _;
use gpui::{
    App, ClipboardItem, Context, Entity, EventEmitter, FocusHandle, Focusable, FontWeight,
    InteractiveElement as _, IntoElement, ParentElement as _, Render, SharedString,
    StatefulInteractiveElement as _, Styled as _, Subscription, Task, Window, div, px,
};

use super::{Phase, Teams, hub};
use crate::theme;
use crate::ui::{self, ButtonKind, Tone, Type, Typography as _, icon};

pub enum SignInEvent {
    Dismissed,
    /// "Open Team", after a sign-in to an account with Teams.
    OpenTeam,
}

pub struct SignInDialog {
    teams: Entity<Teams>,
    focus: FocusHandle,
    copied: bool,
    /// Ticks the countdown while a code waits.
    _tick: Task<()>,
    _sub: Subscription,
}

impl EventEmitter<SignInEvent> for SignInDialog {}

impl Focusable for SignInDialog {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl SignInDialog {
    /// Opens the dialog and starts a sign-in when none is under way.
    pub fn new(teams: Entity<Teams>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let focus = cx.focus_handle();
        window.focus(&focus, cx);
        teams.update(cx, |t, cx| {
            if !t.signed_in() && t.phase().at_rest() {
                t.sign_in(cx);
            }
        });
        let sub = cx.observe(&teams, |_, _, cx| cx.notify());
        let tick = cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(Duration::from_secs(1)).await;
                let ticked = this.update(cx, |this, cx| {
                    if matches!(this.teams.read(cx).phase(), Phase::Waiting { .. }) {
                        cx.notify();
                    }
                });
                if ticked.is_err() {
                    return;
                }
            }
        });
        Self {
            teams,
            focus,
            copied: false,
            _tick: tick,
            _sub: sub,
        }
    }

    /// Closes the dialog; a code still waiting on GitHub is given up.
    fn dismiss(&mut self, cx: &mut Context<Self>) {
        self.teams.update(cx, |t, cx| {
            if !t.signed_in() {
                t.cancel(cx);
            }
        });
        cx.emit(SignInEvent::Dismissed);
    }

    fn again(&mut self, cx: &mut Context<Self>) {
        self.copied = false;
        self.teams.update(cx, |t, cx| {
            t.cancel(cx);
            t.sign_in(cx);
        });
    }

    /// GitHub's code, a box per character, split where GitHub puts its dash.
    fn code_boxes(code: &str, faded: bool) -> gpui::Div {
        let shown = crate::redact::value(code);
        let mut row = div()
            .flex()
            .items_center()
            .gap(ui::sp(4.))
            .when(faded, |d| d.opacity(0.4));
        for c in shown.chars() {
            if c == '-' {
                row = row.child(
                    div()
                        .w(ui::sp(8.))
                        .h(px(1.))
                        .mx(ui::sp(4.))
                        .bg(theme::border_strong()),
                );
                continue;
            }
            row = row.child(
                div()
                    .flex()
                    .flex_none()
                    .items_center()
                    .justify_center()
                    .size(px(ui::INPUT_H))
                    .rounded(ui::rad(ui::RADIUS))
                    .border_1()
                    .border_color(theme::border())
                    .bg(theme::background())
                    .font_family(theme::FONT_MONO)
                    .text_size(px(ui::TEXT_LG))
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(theme::text())
                    .child(c.to_string()),
            );
        }
        row
    }

    fn step(n: usize, text: impl Into<SharedString>) -> gpui::Div {
        div()
            .flex()
            .gap(ui::sp(8.))
            .ty(Type::Body)
            .text_color(theme::text())
            .child(
                div()
                    .flex()
                    .flex_none()
                    .items_center()
                    .justify_center()
                    .size(px(18.))
                    .rounded_full()
                    .border_1()
                    .border_color(theme::border_strong())
                    .font_family(theme::FONT_MONO)
                    .text_size(px(ui::TEXT_XS))
                    .text_color(theme::text_subtle())
                    .child(n.to_string()),
            )
            .child(div().flex_1().min_w_0().child(text.into()))
    }

    /// A line under the code: an icon and what happened.
    fn note(lead: gpui::AnyElement, text: impl Into<SharedString>) -> gpui::Div {
        div()
            .flex()
            .items_center()
            .gap(ui::sp(8.))
            .ty(Type::Label)
            .font_weight(FontWeight::NORMAL)
            .text_color(theme::text_subtle())
            .child(lead)
            .child(div().flex_1().min_w_0().child(text.into()))
    }

    fn buttons() -> gpui::Div {
        div().flex().justify_end().gap(ui::sp(8.))
    }

    fn render_signed_in(&self, cx: &mut Context<Self>) -> (String, String, gpui::Div) {
        let teams = self.teams.read(cx);
        let me = teams.me().cloned();
        let enabled = teams.enabled();
        let login = me
            .as_ref()
            .map(|m| crate::redact::value(&m.login))
            .unwrap_or_default();
        let picture = me
            .as_ref()
            .filter(|_| !crate::redact::on())
            .and_then(|m| teams.picture(&m.login))
            .map(|p| p.to_path_buf());
        let device = crate::redact::value(&hub::device_name());
        let body = div()
            .flex()
            .flex_col()
            .gap(ui::sp(16.))
            .p(ui::sp(16.))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(ui::sp(12.))
                    .child(ui::avatar_image(&login, picture.as_deref(), 32.))
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap(ui::sp(3.))
                            .flex_1()
                            .min_w_0()
                            .child(
                                div()
                                    .truncate()
                                    .ty(Type::Body)
                                    .font_weight(FontWeight::MEDIUM)
                                    .text_color(theme::text())
                                    .child(format!("Signed in as @{login}")),
                            )
                            .child(
                                div()
                                    .truncate()
                                    .ty(Type::Caption)
                                    .text_color(theme::text_subtle())
                                    .child(format!(
                                        "Signed in with GitHub · this device is {device}"
                                    )),
                            ),
                    )
                    .child(ui::badge("Connected", Tone::Success)),
            )
            .child(Self::note(
                icon(if enabled { "ungroup-items" } else { "user" }, 14.)
                    .text_color(theme::text_subtle())
                    .into_any_element(),
                if enabled {
                    "Teams is on for your account. It is in the sidebar now."
                } else {
                    "Nothing Arakun is trying out is on for this account yet."
                },
            ))
            .child(
                Self::buttons()
                    .child(
                        ui::button("signin-close", "Close", ButtonKind::Ghost, true)
                            .on_click(cx.listener(|_, _, _, cx| cx.emit(SignInEvent::Dismissed))),
                    )
                    .when(enabled, |d| {
                        d.child(
                            ui::button("signin-open-team", "Open Team", ButtonKind::Primary, true)
                                .on_click(
                                    cx.listener(|_, _, _, cx| cx.emit(SignInEvent::OpenTeam)),
                                ),
                        )
                    }),
            );
        (
            "Signed in".into(),
            "This device is now yours on GitHub.".into(),
            body,
        )
    }

    fn render_flow(&self, cx: &mut Context<Self>) -> (String, String, gpui::Div) {
        let teams = self.teams.read(cx);
        let phase = teams.phase().clone();
        let error = teams.error().cloned();
        let copied = self.copied;
        let copy = |code: Option<SharedString>, enabled: bool, cx: &mut Context<Self>| {
            ui::button_with(
                "signin-copy",
                Some(if copied && enabled {
                    "tick02"
                } else {
                    "copy01"
                }),
                if copied && enabled { "Copied" } else { "Copy" },
                ButtonKind::Outline,
                enabled,
            )
            .when_some(code.filter(|_| enabled), |b, code| {
                b.on_click(cx.listener(move |this, _, _, cx| {
                    cx.write_to_clipboard(ClipboardItem::new_string(code.to_string()));
                    this.copied = true;
                    cx.notify();
                }))
            })
        };
        let code_row = |code: &SharedString, live: bool, cx: &mut Context<Self>| {
            div()
                .flex()
                .items_center()
                .justify_between()
                .child(Self::code_boxes(code, !live))
                .child(copy(Some(code.clone()), live, cx))
        };
        let ended = |what: &'static str,
                     lead: gpui::AnyElement,
                     text: String,
                     again: &'static str,
                     code: Option<SharedString>,
                     cx: &mut Context<Self>| {
            (
                "Sign in with GitHub".to_string(),
                what.to_string(),
                div()
                    .flex()
                    .flex_col()
                    .gap(ui::sp(16.))
                    .p(ui::sp(16.))
                    .when_some(code, |d, code| d.child(code_row(&code, false, cx)))
                    .child(Self::note(lead, text))
                    .child(
                        Self::buttons()
                            .child(
                                ui::button("signin-cancel", "Cancel", ButtonKind::Ghost, true)
                                    .on_click(cx.listener(|this, _, _, cx| this.dismiss(cx))),
                            )
                            .child(
                                ui::button("signin-again", again, ButtonKind::Primary, true)
                                    .on_click(cx.listener(|this, _, _, cx| this.again(cx))),
                            ),
                    ),
            )
        };
        match phase {
            Phase::Waiting {
                user_code,
                verification_uri,
                expires_at,
            } => {
                let left = expires_at
                    .saturating_duration_since(Instant::now())
                    .as_secs();
                let uri = verification_uri.to_string();
                let shown_uri = uri
                    .trim_start_matches("https://")
                    .trim_start_matches("http://")
                    .to_string();
                (
                    "Sign in with GitHub".into(),
                    "Arakun never sees your password.".into(),
                    div()
                        .flex()
                        .flex_col()
                        .gap(ui::sp(16.))
                        .p(ui::sp(16.))
                        .child(code_row(&user_code, true, cx))
                        .child(
                            div()
                                .flex()
                                .flex_col()
                                .gap(ui::sp(8.))
                                .child(Self::step(1, "Copy this code."))
                                .child(Self::step(2, format!("{shown_uri} opens in your browser.")))
                                .child(Self::step(3, "Paste the code there and approve Arakun.")),
                        )
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .justify_between()
                                .gap(ui::sp(8.))
                                .child(Self::note(
                                    ui::spinner("signin-wait", 14., theme::text_subtle())
                                        .into_any_element(),
                                    format!(
                                        "Waiting for GitHub · the code lasts {}:{:02}",
                                        left / 60,
                                        left % 60
                                    ),
                                ))
                                .child(
                                    ui::button(
                                        "signin-open",
                                        "Open GitHub",
                                        ButtonKind::Primary,
                                        true,
                                    )
                                    .on_click(move |_, _, cx| cx.open_url(&uri)),
                                ),
                        ),
                )
            }
            Phase::Starting | Phase::Finishing => (
                "Sign in with GitHub".into(),
                "Arakun never sees your password.".into(),
                div().p(ui::sp(16.)).child(Self::note(
                    ui::spinner("signin-busy", 14., theme::text_subtle()).into_any_element(),
                    if phase == Phase::Starting {
                        "Asking GitHub for a code…"
                    } else {
                        "GitHub said yes. Signing in…"
                    },
                )),
            ),
            Phase::Expired { user_code } => ended(
                "The code ran out before GitHub heard back.",
                icon("clock01", 14.)
                    .text_color(theme::text_subtle())
                    .into_any_element(),
                "This code expired after 15 minutes. Nothing was signed in.".into(),
                "Get a new code",
                Some(user_code),
                cx,
            ),
            Phase::Denied { user_code } => ended(
                "GitHub said no.",
                icon("cancel-circle", 14.)
                    .text_color(theme::error())
                    .into_any_element(),
                "The request was denied on GitHub. Nothing was signed in.".into(),
                "Try again",
                Some(user_code),
                cx,
            ),
            Phase::Idle => ended(
                "It did not go through.",
                icon("alert-circle", 14.)
                    .text_color(theme::error())
                    .into_any_element(),
                error
                    .map(|e| crate::redact::text(&e).into_owned())
                    .unwrap_or_else(|| "Nothing was signed in.".into()),
                "Try again",
                None,
                cx,
            ),
        }
    }
}

impl Render for SignInDialog {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let (title, description, body) = if self.teams.read(cx).signed_in() {
            self.render_signed_in(cx)
        } else {
            self.render_flow(cx)
        };
        ui::modal_panel("teams-sign-in", 440.)
            .track_focus(&self.focus)
            .on_key_down(cx.listener(|this, ev: &gpui::KeyDownEvent, _, cx| {
                if ev.keystroke.key == "escape" {
                    this.dismiss(cx);
                }
            }))
            .child(ui::modal_header(
                title,
                Some(description.into()),
                ui::close_button("teams-sign-in-close")
                    .on_click(cx.listener(|this, _, _, cx| this.dismiss(cx))),
            ))
            .child(body)
    }
}
