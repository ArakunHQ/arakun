//! The Profile dialog the rail's User row opens: who you are in Arakun, the
//! optional GitHub sign-in and Teams. The sign-in itself runs in the
//! GitHub sign-in dialog (`arakun_teams::sign_in`).

use gpui::prelude::FluentBuilder as _;
use gpui::{
    App, Context, Entity, EventEmitter, FocusHandle, Focusable, FontWeight,
    InteractiveElement as _, IntoElement, ParentElement as _, Render, SharedString,
    StatefulInteractiveElement as _, Styled as _, Subscription, Window, div,
};

use crate::arakun_teams::{Teams, api::TEAMS_FLAG};
use crate::theme;
use crate::ui::{self, ButtonKind, Tone, Type, Typography as _, icon};

pub enum ProfileEvent {
    Dismissed,
    SignIn,
    OpenTeam,
}

pub struct ProfileDialog {
    teams: Entity<Teams>,
    /// A name set in Customize, which wins over the login and the git name.
    label: Option<String>,
    /// The global git `user.name` and `user.email`.
    git: (String, String),
    focus: FocusHandle,
    _sub: Subscription,
}

impl EventEmitter<ProfileEvent> for ProfileDialog {}

impl Focusable for ProfileDialog {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl ProfileDialog {
    pub fn new(
        teams: Entity<Teams>,
        label: Option<String>,
        git: (String, String),
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let focus = cx.focus_handle();
        window.focus(&focus, cx);
        teams.update(cx, |t, cx| {
            t.refresh_if_unknown(cx);
            t.refresh_if_stale(cx);
        });
        let sub = cx.observe(&teams, |_, _, cx| cx.notify());
        Self {
            teams,
            label: label.filter(|l| !l.trim().is_empty()),
            git,
            focus,
            _sub: sub,
        }
    }

    /// The name at the top: Customize's, then the GitHub login, then git's.
    fn shown_name(&self, login: Option<&str>) -> String {
        if crate::redact::on() {
            return "You".into();
        }
        self.label
            .clone()
            .or_else(|| login.map(str::to_string))
            .or_else(|| (!self.git.0.is_empty()).then(|| self.git.0.clone()))
            .unwrap_or_else(|| "You".into())
    }

    fn row(
        title: impl Into<SharedString>,
        hint: impl Into<SharedString>,
        action: impl IntoElement,
    ) -> gpui::Div {
        div()
            .flex()
            .items_center()
            .gap(ui::sp(12.))
            .py(ui::sp(10.))
            .border_t_1()
            .border_color(theme::border_subtle())
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(ui::sp(2.))
                    .flex_1()
                    .min_w_0()
                    .child(
                        div()
                            .ty(Type::Body)
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(theme::text())
                            .child(title.into()),
                    )
                    .child(
                        div()
                            .ty(Type::Caption)
                            .text_color(theme::text_subtle())
                            .child(hint.into()),
                    ),
            )
            .child(div().flex_none().child(action))
    }
}

impl Render for ProfileDialog {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let teams = self.teams.read(cx);
        let (ready, signed_in, enabled) = (teams.ready(), teams.signed_in(), teams.enabled());
        let busy = !teams.phase().at_rest();
        let me = teams.me().cloned();
        let error = teams.error().cloned();
        let redacted = crate::redact::on();
        let login = me.as_ref().map(|m| m.login.clone());
        let picture = login
            .as_deref()
            .filter(|_| !redacted)
            .and_then(|l| teams.picture(l))
            .map(|p| p.to_path_buf());
        let name = self.shown_name(login.as_deref());
        let subtitle = match &login {
            Some(l) => format!("@{} · Signed in with GitHub", crate::redact::value(l)),
            None if signed_in => "Signed in with GitHub".to_string(),
            None if !self.git.1.is_empty() => crate::redact::value(&self.git.1).to_string(),
            None => "Not signed in".to_string(),
        };

        let identity = div()
            .flex()
            .items_center()
            .gap(ui::sp(12.))
            .pb(ui::sp(12.))
            .child(ui::avatar_image(&name, picture.as_deref(), 40.))
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
                            .ty(Type::Heading)
                            .text_color(theme::text())
                            .child(name.clone()),
                    )
                    .child(
                        div()
                            .truncate()
                            .ty(Type::Caption)
                            .text_color(theme::text_subtle())
                            .child(subtitle),
                    ),
            )
            .when(signed_in, |d| {
                d.child(ui::badge("Connected", Tone::Success))
            });

        let github = if !ready {
            Self::row(
                "Checking…",
                "Checking the session with the Teams server.",
                ui::spinner(14., theme::text_subtle()),
            )
        } else if signed_in {
            let hint = match &me {
                Some(m) if m.has(TEAMS_FLAG) => {
                    "Teams is on for this account. The session is kept in the system keyring."
                }
                Some(_) => {
                    "Nothing Arakun is trying out is on for this account yet. The session is kept in the system keyring."
                }
                None => "The Teams server has not said yet what is on for this account.",
            };
            Self::row(
                "GitHub",
                hint,
                ui::button("profile-sign-out", "Sign out", ButtonKind::Danger, true).on_click(
                    cx.listener(|this, _, _, cx| {
                        this.teams.update(cx, |t, cx| t.sign_out(cx));
                    }),
                ),
            )
        } else {
            Self::row(
                "GitHub",
                "An optional sign-in, for what Arakun is still trying out with some people, such as Teams. GitHub shares only your user name and ID. Agents never need it.",
                ui::button_with(
                    "profile-sign-in",
                    Some("github"),
                    if busy { "Signing in…" } else { "Sign in" },
                    ButtonKind::Outline,
                    true,
                )
                .on_click(cx.listener(|_, _, _, cx| cx.emit(ProfileEvent::SignIn))),
            )
        };

        let body = div()
            .flex()
            .flex_col()
            .px(ui::sp(16.))
            .pt(ui::sp(12.))
            .pb(ui::sp(6.))
            .child(identity)
            .child(github)
            .when(signed_in && enabled, |d| {
                d.child(Self::row(
                    "Teams",
                    "Your team's end-to-end encrypted messages.",
                    ui::button("profile-open-team", "Open Team", ButtonKind::Outline, true)
                        .on_click(cx.listener(|_, _, _, cx| cx.emit(ProfileEvent::OpenTeam))),
                ))
            })
            .when_some(error, |d, e| {
                d.child(
                    div()
                        .flex()
                        .items_center()
                        .gap(ui::sp(8.))
                        .py(ui::sp(10.))
                        .border_t_1()
                        .border_color(theme::border_subtle())
                        .ty(Type::Caption)
                        .text_color(theme::error())
                        .child(icon("alert-circle", 14.).text_color(theme::error()))
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .child(crate::redact::text(&e).into_owned()),
                        ),
                )
            });

        ui::modal_panel("profile", 440., "Profile")
            .track_focus(&self.focus)
            .on_key_down(cx.listener(|_, ev: &gpui::KeyDownEvent, _, cx| {
                if ev.keystroke.key == "escape" {
                    cx.emit(ProfileEvent::Dismissed);
                }
            }))
            .child(ui::modal_header(
                "Profile",
                None,
                ui::close_button("profile-close")
                    .on_click(cx.listener(|_, _, _, cx| cx.emit(ProfileEvent::Dismissed))),
            ))
            .child(body)
    }
}
