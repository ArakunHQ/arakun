//! The sign-in dialog: GitHub or an address first; then GitHub's device code
//! and what to do with it, or the box for the code mailed to the address; then
//! how it ended. The flows themselves run in [`Teams`]; this only draws where
//! they stand.

use std::time::{Duration, Instant};

use gpui::AppContext as _;
use gpui::prelude::FluentBuilder as _;
use gpui::{
    App, ClipboardItem, Context, Entity, EventEmitter, FocusHandle, Focusable, FontWeight,
    InteractiveElement as _, IntoElement, ParentElement as _, Render, SharedString,
    StatefulInteractiveElement as _, Styled as _, Subscription, Task, Window, div, px,
};

use super::{Phase, Teams, hub};
use crate::input::{InputEvent, TextInput};
use crate::theme;
use crate::ui::{self, ButtonKind, Tone, Type, Typography as _, icon};

pub enum SignInEvent {
    Dismissed,
    /// "Open Team", after a sign-in to an account with Teams.
    OpenTeam,
}

pub struct SignInDialog {
    teams: Entity<Teams>,
    /// The address to mail a code to.
    email: Entity<TextInput>,
    /// The code mailed to it.
    code: Entity<TextInput>,
    /// "Send a new code" was pressed: while it mails, the code screen stays.
    resending: bool,
    /// The code screen was up at the last change of the sign-in.
    at_code: bool,
    /// Opened for the account itself, not Teams: once signed in there is
    /// nothing more to say here.
    close_when_in: bool,
    focus: FocusHandle,
    copied: bool,
    /// Ticks the countdown while a code waits.
    _tick: Task<()>,
    _subs: Vec<Subscription>,
}

impl EventEmitter<SignInEvent> for SignInDialog {}

impl Focusable for SignInDialog {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl SignInDialog {
    /// Opens the dialog at the choice of GitHub or an address, or where a
    /// sign-in under way stands. With `close_when_in` it closes itself once
    /// signed in.
    pub fn new(
        teams: Entity<Teams>,
        close_when_in: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let focus = cx.focus_handle();
        let email = cx.new(|cx| TextInput::new("you@example.com", cx));
        let code = cx.new(|cx| TextInput::new("123456", cx).mono());
        window.focus(&email.focus_handle(cx), cx);
        let mut subs = vec![cx.observe_in(&teams, window, |this, teams, window, cx| {
            let (signed_in, code_sent) = {
                let t = teams.read(cx);
                (t.signed_in(), matches!(t.phase(), Phase::CodeSent { .. }))
            };
            if this.close_when_in && signed_in {
                cx.emit(SignInEvent::Dismissed);
            }
            // The code box takes the keys once there is a code to type.
            if code_sent && !this.at_code {
                window.focus(&this.code.focus_handle(cx), cx);
            }
            this.at_code = code_sent;
            if code_sent {
                this.resending = false;
            }
            cx.notify();
        })];
        subs.push(
            cx.subscribe_in(&email, window, |this, _, ev, _, cx| match ev {
                InputEvent::Submit => this.send_code(cx),
                InputEvent::Cancel => this.dismiss(cx),
                InputEvent::Changed => cx.notify(),
                _ => {}
            }),
        );
        subs.push(
            cx.subscribe_in(&code, window, |this, _, ev, _, cx| match ev {
                InputEvent::Submit => this.check_code(cx),
                InputEvent::Cancel => this.dismiss(cx),
                InputEvent::Changed => cx.notify(),
                _ => {}
            }),
        );
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
            email,
            code,
            resending: false,
            at_code: false,
            close_when_in,
            focus,
            copied: false,
            _tick: tick,
            _subs: subs,
        }
    }

    /// The address typed, when it could be one.
    fn address(&self, cx: &App) -> Option<String> {
        let a = self.email.read(cx).text().trim();
        (a.contains('@') && a.len() >= 3).then(|| a.to_string())
    }

    /// The code typed, spaces left out, when it is six digits.
    fn typed_code(&self, cx: &App) -> Option<String> {
        let c: String = self
            .code
            .read(cx)
            .text()
            .chars()
            .filter(|c| !c.is_whitespace())
            .collect();
        (c.len() == 6 && c.bytes().all(|b| b.is_ascii_digit())).then_some(c)
    }

    fn github(&mut self, cx: &mut Context<Self>) {
        self.copied = false;
        self.teams.update(cx, |t, cx| {
            t.cancel(cx);
            t.sign_in(cx);
        });
    }

    fn send_code(&mut self, cx: &mut Context<Self>) {
        let Some(address) = self.address(cx) else {
            return;
        };
        self.resending = matches!(self.teams.read(cx).phase(), Phase::CodeSent { .. });
        self.code.update(cx, |c, cx| c.clear(cx));
        self.teams
            .update(cx, |t, cx| t.send_email_code(address, cx));
    }

    fn check_code(&mut self, cx: &mut Context<Self>) {
        let Some(code) = self.typed_code(cx) else {
            return;
        };
        self.teams.update(cx, |t, cx| t.sign_in_with_code(code, cx));
    }

    /// Back to the choice, with the address kept to fix it.
    fn other_address(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.resending = false;
        self.code.update(cx, |c, cx| c.clear(cx));
        self.teams.update(cx, |t, cx| t.cancel(cx));
        window.focus(&self.email.focus_handle(cx), cx);
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
        self.github(cx);
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
        let error = teams.error().cloned();
        let login = me
            .as_ref()
            .map(|m| crate::redact::value(&m.login))
            .unwrap_or_default();
        let name = me
            .as_ref()
            .map(|m| super::account_name(&m.login))
            .unwrap_or_default();
        let via = me
            .as_ref()
            .map_or("Signed in", |m| super::signed_in_with(&m.login));
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
                                    .child(format!("Signed in as {name}")),
                            )
                            .child(
                                div()
                                    .truncate()
                                    .ty(Type::Caption)
                                    .text_color(theme::text_subtle())
                                    .child(format!("{via} · this device is {device}")),
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
            // Signed in, but something still went wrong: the keyring could
            // not keep the session, so the next start asks again.
            .when_some(error, |d, e| {
                d.child(Self::note(
                    icon("alert-circle", 14.)
                        .text_color(theme::attention())
                        .into_any_element(),
                    crate::redact::text(&e).into_owned(),
                ))
            })
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
            "This device is now signed in to your account.".into(),
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
                                    ui::spinner(14., theme::text_subtle()).into_any_element(),
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
                    ui::spinner(14., theme::text_subtle()).into_any_element(),
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
            Phase::Idle => self.render_choose(false, error, cx),
            Phase::Mailing { email } if self.resending => self.render_code(email, true, error, cx),
            Phase::Mailing { .. } => self.render_choose(true, error, cx),
            Phase::CodeSent { email } => self.render_code(email, false, error, cx),
            Phase::Checking { email } => self.render_code(email, true, error, cx),
        }
    }

    /// A line saying what went wrong, under the form it is about.
    fn error_note(error: Option<SharedString>) -> Option<gpui::Div> {
        error.map(|e| {
            Self::note(
                icon("alert-circle", 14.)
                    .text_color(theme::error())
                    .into_any_element(),
                crate::redact::text(&e).into_owned(),
            )
        })
    }

    /// GitHub, or an address to mail a code to. `mailing` while the server
    /// is asked to send one.
    fn render_choose(
        &self,
        mailing: bool,
        error: Option<SharedString>,
        cx: &mut Context<Self>,
    ) -> (String, String, gpui::Div) {
        let can_send = !mailing && self.address(cx).is_some();
        let body = div()
            .flex()
            .flex_col()
            .gap(ui::sp(16.))
            .p(ui::sp(16.))
            .child(
                ui::button_with(
                    "signin-github",
                    Some("github"),
                    "Continue with GitHub",
                    ButtonKind::Outline,
                    !mailing,
                )
                .w_full()
                .on_click(cx.listener(|this, _, _, cx| this.github(cx))),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(ui::sp(8.))
                    .child(div().flex_1().h(px(1.)).bg(theme::border()))
                    .child(ui::text(
                        Type::Caption,
                        theme::text_subtle(),
                        "or with your email",
                    ))
                    .child(div().flex_1().h(px(1.)).bg(theme::border())),
            )
            .child(
                div()
                    .flex()
                    .gap(ui::sp(8.))
                    .child(
                        ui::field()
                            .flex_1()
                            .min_w_0()
                            .bg(theme::background())
                            .child(div().flex_1().min_w_0().child(self.email.clone())),
                    )
                    .child(
                        ui::button(
                            "signin-email",
                            if mailing {
                                "Sending…"
                            } else {
                                "Email me a code"
                            },
                            ButtonKind::Primary,
                            can_send,
                        )
                        .on_click(cx.listener(|this, _, _, cx| this.send_code(cx))),
                    ),
            )
            .children(Self::error_note(error))
            .child(Self::note(
                icon("inbox", 14.)
                    .text_color(theme::text_subtle())
                    .into_any_element(),
                "No account yet? Either way makes one.",
            ));
        (
            "Sign in".into(),
            "Use GitHub, or a code we email you.".into(),
            body,
        )
    }

    /// The box for the code mailed to `email`. `busy` while it is checked, or
    /// a new one is mailed.
    fn render_code(
        &self,
        email: SharedString,
        busy: bool,
        error: Option<SharedString>,
        cx: &mut Context<Self>,
    ) -> (String, String, gpui::Div) {
        let checking = matches!(self.teams.read(cx).phase(), Phase::Checking { .. });
        let can_check = !busy && self.typed_code(cx).is_some();
        let shown = crate::redact::value(&email);
        let body = div()
            .flex()
            .flex_col()
            .gap(ui::sp(16.))
            .p(ui::sp(16.))
            .child(
                div()
                    .flex()
                    .gap(ui::sp(8.))
                    .child(
                        ui::field()
                            .flex_1()
                            .min_w_0()
                            .bg(theme::background())
                            .child(div().flex_1().min_w_0().child(self.code.clone())),
                    )
                    .child(
                        ui::button(
                            "signin-code",
                            if checking { "Signing in…" } else { "Sign in" },
                            ButtonKind::Primary,
                            can_check,
                        )
                        .on_click(cx.listener(|this, _, _, cx| this.check_code(cx))),
                    ),
            )
            .children(Self::error_note(error))
            .child(Self::note(
                if busy {
                    ui::spinner(14., theme::text_subtle()).into_any_element()
                } else {
                    icon("inbox", 14.)
                        .text_color(theme::text_subtle())
                        .into_any_element()
                },
                if checking {
                    "Checking the code…".to_string()
                } else if busy {
                    format!("Sending a new code to {shown}…")
                } else {
                    "It works once, for 10 minutes. Not there? Look in spam.".to_string()
                },
            ))
            .child(
                Self::buttons()
                    .child(
                        ui::button(
                            "signin-other",
                            "Use another address",
                            ButtonKind::Ghost,
                            !checking,
                        )
                        .on_click(
                            cx.listener(|this, _, window, cx| this.other_address(window, cx)),
                        ),
                    )
                    .child(
                        ui::button(
                            "signin-resend",
                            "Send a new code",
                            ButtonKind::Outline,
                            !busy,
                        )
                        .on_click(cx.listener(|this, _, _, cx| this.send_code(cx))),
                    ),
            );
        (
            "Check your email".into(),
            format!("We sent a six-digit code to {shown}. Type it here."),
            body,
        )
    }
}

impl Render for SignInDialog {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let (title, description, body) = if self.teams.read(cx).signed_in() {
            self.render_signed_in(cx)
        } else {
            self.render_flow(cx)
        };
        ui::modal_panel("teams-sign-in", 440., title.clone())
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
