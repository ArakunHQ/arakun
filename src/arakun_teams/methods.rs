//! The account's sign-in methods, in Settings (Account): GitHub and an
//! address, each with a way to remove it while another is left, and a way to
//! add the one that is missing. GitHub is added with its device flow, an
//! address with a code mailed to it. The server never joins two accounts: a
//! method that signs in to another account is refused, in its words.

use std::time::Instant;

use gpui::prelude::FluentBuilder as _;
use gpui::{
    AppContext as _, AsyncApp, Context, Entity, Focusable as _, FontWeight, IntoElement,
    ParentElement as _, Render, SharedString, StatefulInteractiveElement as _, Styled as _,
    Subscription, Task, WeakEntity, Window, div, px,
};

use super::{Teams, email_error, fixture, poll_again};
use crate::cloud::{self, Poll, SignInMethod};
use crate::input::{InputEvent, TextInput};
use crate::theme;
use crate::ui::{self, ButtonKind};

/// The providers the app can add, in the order they are listed.
const PROVIDERS: [&str; 2] = ["github", "email"];

/// The list as the server last answered it.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Listed {
    Loading,
    Failed(String),
    Ready(Vec<SignInMethod>),
}

/// A method being added.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Adding {
    None,
    /// Asking GitHub for a code.
    GitHubStarting,
    /// Waiting for `user_code` to be entered at `uri`.
    GitHubWaiting {
        user_code: SharedString,
        uri: SharedString,
    },
    /// GitHub said yes; the server links it.
    GitHubFinishing,
    /// The address box is open.
    Address,
    /// Asking the server to mail a code.
    Mailing,
    /// A code went to `email`; the code box is open.
    Code {
        email: SharedString,
    },
    /// The server checks the code.
    Checking {
        email: SharedString,
    },
}

impl Adding {
    fn busy(&self) -> bool {
        matches!(
            self,
            Adding::GitHubStarting
                | Adding::GitHubFinishing
                | Adding::Mailing
                | Adding::Checking { .. }
        )
    }
}

pub struct MethodsView {
    teams: Entity<Teams>,
    list: Listed,
    /// The session the list is for, so a new sign-in lists again.
    listed_for: Option<String>,
    adding: Adding,
    /// The provider whose Remove was pressed once, waiting to be confirmed.
    confirm: Option<String>,
    /// The provider being removed.
    removing: Option<String>,
    error: Option<SharedString>,
    email: Entity<TextInput>,
    code: Entity<TextInput>,
    _task: Option<Task<()>>,
    _subs: Vec<Subscription>,
}

/// Whether the method of `provider` may be removed: another is left.
fn removable(methods: &[SignInMethod], provider: &str) -> bool {
    methods.iter().any(|m| m.provider == provider) && methods.len() > 1
}

/// The providers the account has none of, that the app can add.
fn missing(methods: &[SignInMethod]) -> Vec<&'static str> {
    PROVIDERS
        .into_iter()
        .filter(|p| !methods.iter().any(|m| m.provider == *p))
        .collect()
}

fn provider_name(provider: &str) -> &str {
    match provider {
        "github" => "GitHub",
        "email" => "Email",
        other => other,
    }
}

impl MethodsView {
    pub fn new(teams: Entity<Teams>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let email = cx.new(|cx| TextInput::new("you@example.com", cx));
        let code = cx.new(|cx| TextInput::new("123456", cx).mono());
        let subs = vec![
            cx.observe(&teams, |this, _, cx| this.list_if_new(cx)),
            cx.subscribe_in(&email, window, |this, _, ev, _, cx| match ev {
                InputEvent::Submit => this.send_code(cx),
                InputEvent::Cancel => this.stop(cx),
                InputEvent::Changed => cx.notify(),
                _ => {}
            }),
            cx.subscribe_in(&code, window, |this, _, ev, _, cx| match ev {
                InputEvent::Submit => this.check_code(cx),
                InputEvent::Cancel => this.stop(cx),
                InputEvent::Changed => cx.notify(),
                _ => {}
            }),
        ];
        let mut this = Self {
            teams,
            list: Listed::Loading,
            listed_for: None,
            adding: Adding::None,
            confirm: None,
            removing: None,
            error: None,
            email,
            code,
            _task: None,
            _subs: subs,
        };
        this.list_if_new(cx);
        this
    }

    fn session(&self, cx: &Context<Self>) -> Option<(String, String)> {
        self.teams.read(cx).session()
    }

    /// Asks for the list when the session is not the one it was asked with.
    fn list_if_new(&mut self, cx: &mut Context<Self>) {
        let session = self.session(cx);
        if session.as_ref().map(|(_, s)| s) == self.listed_for.as_ref() {
            return;
        }
        self.listed_for = session.as_ref().map(|(_, s)| s.clone());
        self.list = Listed::Loading;
        self.adding = Adding::None;
        self.error = None;
        cx.notify();
        let Some((base, session)) = session else {
            return;
        };
        if fixture().is_some() {
            let login = self.teams.read(cx).me().map(|m| m.login.clone());
            self.list = Listed::Ready(
                login
                    .map(|name| SignInMethod {
                        provider: "github".into(),
                        name,
                        linked_at: 0,
                    })
                    .into_iter()
                    .collect(),
            );
            return;
        }
        cx.spawn(async move |this, cx| {
            let got = cx
                .background_spawn(async move { cloud::sign_in_methods(&base, &session) })
                .await;
            let _ = this.update(cx, |this, cx| {
                this.list = match got {
                    Ok(list) => Listed::Ready(list),
                    Err(e) => Listed::Failed(email_error(&e)),
                };
                cx.notify();
            });
        })
        .detach();
    }

    /// Takes the server's answer to a change: the new list, or why not.
    fn answered(&mut self, got: Result<Vec<SignInMethod>, cloud::Error>) {
        match got {
            Ok(list) => {
                self.list = Listed::Ready(list);
                self.error = None;
            }
            Err(e) => self.error = Some(email_error(&e).into()),
        }
    }

    fn stop(&mut self, cx: &mut Context<Self>) {
        if self.adding.busy() {
            return;
        }
        self._task = None;
        self.adding = Adding::None;
        self.error = None;
        cx.notify();
    }

    fn remove(&mut self, provider: String, cx: &mut Context<Self>) {
        let Some((base, session)) = self.session(cx) else {
            return;
        };
        self.confirm = None;
        self.removing = Some(provider.clone());
        self.error = None;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let got = cx
                .background_spawn(async move { cloud::unlink(&base, &session, &provider) })
                .await;
            let _ = this.update(cx, |this, cx| {
                this.removing = None;
                this.answered(got);
                cx.notify();
            });
        })
        .detach();
    }

    fn add_address(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.adding = Adding::Address;
        self.error = None;
        self.email.update(cx, |i, cx| i.clear(cx));
        window.focus(&self.email.focus_handle(cx), cx);
        cx.notify();
    }

    fn typed_address(&self, cx: &Context<Self>) -> Option<String> {
        let a = self.email.read(cx).text().trim();
        (a.contains('@') && a.len() >= 3).then(|| a.to_string())
    }

    fn typed_code(&self, cx: &Context<Self>) -> Option<String> {
        let c: String = self
            .code
            .read(cx)
            .text()
            .chars()
            .filter(|c| !c.is_whitespace())
            .collect();
        (c.len() == 6 && c.bytes().all(|b| b.is_ascii_digit())).then_some(c)
    }

    fn send_code(&mut self, cx: &mut Context<Self>) {
        if !matches!(self.adding, Adding::Address | Adding::Code { .. }) {
            return;
        }
        let (Some(address), Some((base, session))) = (self.typed_address(cx), self.session(cx))
        else {
            return;
        };
        let before = self.adding.clone();
        self.adding = Adding::Mailing;
        self.error = None;
        self.code.update(cx, |i, cx| i.clear(cx));
        cx.notify();
        self._task = Some(cx.spawn(async move |this, cx| {
            let to = address.clone();
            let sent = cx
                .background_spawn(async move { cloud::send_link_code(&base, &session, &to) })
                .await;
            let _ = this.update(cx, |this, cx| {
                match sent {
                    Ok(()) => {
                        this.adding = Adding::Code {
                            email: address.into(),
                        }
                    }
                    Err(e) => {
                        this.adding = match before {
                            Adding::Code { .. } => before,
                            _ => Adding::Address,
                        };
                        this.error = Some(email_error(&e).into());
                    }
                }
                cx.notify();
            });
        }));
    }

    fn check_code(&mut self, cx: &mut Context<Self>) {
        let Adding::Code { email } = self.adding.clone() else {
            return;
        };
        let (Some(code), Some((base, session))) = (self.typed_code(cx), self.session(cx)) else {
            return;
        };
        self.adding = Adding::Checking {
            email: email.clone(),
        };
        self.error = None;
        cx.notify();
        self._task = Some(cx.spawn(async move |this, cx| {
            let address = email.to_string();
            let got = cx
                .background_spawn(
                    async move { cloud::link_email(&base, &session, &address, &code) },
                )
                .await;
            let _ = this.update(cx, |this, cx| {
                this.adding = if got.is_ok() {
                    Adding::None
                } else {
                    Adding::Code { email }
                };
                this.answered(got);
                cx.notify();
            });
        }));
    }

    fn add_github(&mut self, cx: &mut Context<Self>) {
        let Some((base, session)) = self.session(cx) else {
            return;
        };
        self.adding = Adding::GitHubStarting;
        self.error = None;
        cx.notify();
        self._task = Some(cx.spawn(async move |this, cx| {
            let got = github_flow(&this, base, session, cx).await;
            let _ = this.update(cx, |this, cx| {
                this.adding = Adding::None;
                match got {
                    Ok(answer) => this.answered(answer),
                    Err(e) => this.error = Some(e.into()),
                }
                cx.notify();
            });
        }));
    }

    fn render_method(
        &self,
        m: &SignInMethod,
        can_remove: bool,
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        let provider = m.provider.clone();
        let confirming = self.confirm.as_deref() == Some(m.provider.as_str());
        let removing = self.removing.as_deref() == Some(m.provider.as_str());
        let hint = if can_remove {
            crate::redact::value(&m.name).to_string()
        } else {
            format!(
                "{}. The only way to sign in, so it stays until you add another.",
                crate::redact::value(&m.name)
            )
        };
        let id = |what: &str| SharedString::from(format!("signin-method-{what}-{}", m.provider));
        let control = if confirming {
            let keep = provider.clone();
            div()
                .flex()
                .gap(ui::sp(8.))
                .child(
                    ui::button(id("keep"), "Keep", ButtonKind::Ghost, true).on_click(cx.listener(
                        move |this, _, _, cx| {
                            if this.confirm.as_deref() == Some(keep.as_str()) {
                                this.confirm = None;
                            }
                            cx.notify();
                        },
                    )),
                )
                .child(
                    ui::button(id("remove-confirm"), "Remove", ButtonKind::Danger, true).on_click(
                        cx.listener(move |this, _, _, cx| this.remove(provider.clone(), cx)),
                    ),
                )
                .into_any_element()
        } else {
            let enabled = can_remove && self.removing.is_none();
            ui::button(
                id("remove"),
                if removing { "Removing…" } else { "Remove" },
                ButtonKind::Outline,
                enabled,
            )
            .on_click(cx.listener(move |this, _, _, cx| {
                if enabled {
                    this.confirm = Some(provider.clone());
                    cx.notify();
                }
            }))
            .into_any_element()
        };
        crate::settings_view::row(provider_name(&m.provider).to_string(), hint, control)
    }

    /// The row of a provider the account has none of.
    fn render_missing(&self, provider: &'static str, cx: &mut Context<Self>) -> gpui::Div {
        let free = matches!(self.adding, Adding::None);
        match provider {
            "github" => {
                let (hint, label): (String, &str) = match &self.adding {
                    Adding::GitHubStarting => ("Asking GitHub for a code…".into(), "Adding…"),
                    Adding::GitHubWaiting { user_code, uri } => (
                        format!("Enter {} at {uri} to add your GitHub account.", crate::redact::value(user_code)),
                        "Adding…",
                    ),
                    Adding::GitHubFinishing => ("Adding your GitHub account…".into(), "Adding…"),
                    _ => (
                        "Sign in with GitHub too. A GitHub account that already has its own Arakun account can't be added here.".into(),
                        "Add GitHub",
                    ),
                };
                let waiting = matches!(self.adding, Adding::GitHubWaiting { .. });
                let control = if waiting {
                    ui::button(
                        "signin-method-github-stop",
                        "Cancel",
                        ButtonKind::Ghost,
                        true,
                    )
                    .on_click(cx.listener(|this, _, _, cx| {
                        this._task = None;
                        this.adding = Adding::None;
                        cx.notify();
                    }))
                } else {
                    ui::button("signin-method-github-add", label, ButtonKind::Outline, free)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            if free {
                                this.add_github(cx);
                            }
                        }))
                };
                crate::settings_view::row("GitHub", hint, control.into_any_element())
            }
            _ => match &self.adding {
                Adding::Address | Adding::Mailing => {
                    let mailing = matches!(self.adding, Adding::Mailing);
                    let can_send = !mailing && self.typed_address(cx).is_some();
                    self.input_row(
                        "Email",
                        "We'll mail a code to the address. An address that already has its own Arakun account can't be added here.".into(),
                        self.email.clone(),
                        ("signin-method-email-send", if mailing { "Sending…" } else { "Email me a code" }, can_send),
                        |this, cx| this.send_code(cx),
                        cx,
                    )
                }
                Adding::Code { email } | Adding::Checking { email } => {
                    let checking = matches!(self.adding, Adding::Checking { .. });
                    let can_check = !checking && self.typed_code(cx).is_some();
                    self.input_row(
                        "Email",
                        format!(
                            "Type the code we mailed to {}.",
                            crate::redact::value(email)
                        ),
                        self.code.clone(),
                        (
                            "signin-method-email-check",
                            if checking { "Adding…" } else { "Add address" },
                            can_check,
                        ),
                        |this, cx| this.check_code(cx),
                        cx,
                    )
                }
                _ => crate::settings_view::row(
                    "Email",
                    "Sign in with a code mailed to an address too.",
                    ui::button(
                        "signin-method-email-add",
                        "Add address",
                        ButtonKind::Outline,
                        free,
                    )
                    .on_click(cx.listener(move |this, _, window, cx| {
                        if free {
                            this.add_address(window, cx);
                        }
                    }))
                    .into_any_element(),
                ),
            },
        }
    }

    /// A row with a text box and its button under the text, and Cancel.
    fn input_row(
        &self,
        title: &str,
        hint: String,
        input: Entity<TextInput>,
        (id, label, enabled): (&'static str, &'static str, bool),
        act: fn(&mut Self, &mut Context<Self>),
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        let busy = self.adding.busy();
        crate::settings_view::stacked_row(
            title.to_string(),
            hint,
            div()
                .flex()
                .gap(ui::sp(8.))
                .w(px(420.))
                .child(
                    ui::field()
                        .flex_1()
                        .min_w_0()
                        .bg(theme::background())
                        .child(div().flex_1().min_w_0().child(input)),
                )
                .child(
                    ui::button(id, label, ButtonKind::Primary, enabled).on_click(cx.listener(
                        move |this, _, _, cx| {
                            if enabled {
                                act(this, cx);
                            }
                        },
                    )),
                )
                .child(
                    ui::button(
                        "signin-method-email-cancel",
                        "Cancel",
                        ButtonKind::Ghost,
                        !busy,
                    )
                    .on_click(cx.listener(|this, _, _, cx| this.stop(cx))),
                )
                .into_any_element(),
        )
    }
}

/// GitHub's device flow, then the server links the GitHub user. The token
/// lives only in here and is dropped once the server has it.
async fn github_flow(
    this: &WeakEntity<MethodsView>,
    base: String,
    session: String,
    cx: &mut AsyncApp,
) -> Result<Result<Vec<SignInMethod>, cloud::Error>, String> {
    let asked = base.clone();
    let client_id: String = cx
        .background_spawn(async move { cloud::github_client_id(&asked) })
        .await
        .map_err(|e| format!("Sign-in: {e}"))?;
    let id = client_id.clone();
    let code = cx
        .background_spawn(async move { cloud::start_device_flow(&id) })
        .await
        .map_err(|e| format!("GitHub: {e}"))?;
    let deadline = Instant::now() + code.expires_in;
    let uri = code.verification_uri.clone();
    this.update(cx, |this, cx| {
        this.adding = Adding::GitHubWaiting {
            user_code: code.user_code.clone().into(),
            uri: uri.clone().into(),
        };
        cx.open_url(&uri);
        cx.notify();
    })
    .map_err(|e| e.to_string())?;

    let mut interval = code.interval;
    let mut dropped = 0;
    let token = loop {
        cx.background_executor().timer(interval).await;
        if Instant::now() >= deadline {
            return Err(super::EXPIRED.into());
        }
        let device = code.device_code.clone();
        let id = client_id.clone();
        let answer = match cx
            .background_spawn(async move { cloud::poll_once(&id, device.expose(), interval) })
            .await
        {
            Ok(answer) => {
                dropped = 0;
                answer
            }
            Err(e) if poll_again(&e, dropped) => {
                dropped += 1;
                continue;
            }
            Err(e) => return Err(format!("GitHub: {e}")),
        };
        match answer {
            Poll::Wait(pace) => interval = pace,
            Poll::Token(token) => break token,
            Poll::Expired => return Err(super::EXPIRED.into()),
            Poll::Denied => return Err("Adding GitHub was turned down on GitHub.".into()),
            Poll::Failed(e) => return Err(format!("GitHub: {e}")),
        }
    };
    this.update(cx, |this, cx| {
        this.adding = Adding::GitHubFinishing;
        cx.notify();
    })
    .map_err(|e| e.to_string())?;
    Ok(cx
        .background_spawn(async move {
            let got = cloud::link_github(&base, &session, token.expose());
            drop(token);
            got
        })
        .await)
}

impl Render for MethodsView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let rows: Vec<gpui::AnyElement> = match &self.list {
            _ if self.listed_for.is_none() => vec![
                crate::settings_view::row(
                    "Not signed in",
                    "Sign in from your profile to see how your account signs in.",
                    div().into_any_element(),
                )
                .into_any_element(),
            ],
            Listed::Loading => vec![
                crate::settings_view::row("Asking your account…", "", div().into_any_element())
                    .into_any_element(),
            ],
            Listed::Failed(why) => vec![
                crate::settings_view::row(
                    "Could not list them",
                    crate::redact::text(why).into_owned(),
                    div().into_any_element(),
                )
                .into_any_element(),
            ],
            Listed::Ready(list) => {
                let list = list.clone();
                let mut rows = Vec::new();
                for m in &list {
                    rows.push(
                        self.render_method(m, removable(&list, &m.provider), cx)
                            .into_any_element(),
                    );
                }
                for p in missing(&list) {
                    rows.push(self.render_missing(p, cx).into_any_element());
                }
                rows
            }
        };
        let error = self.error.clone();
        div()
            .flex()
            .flex_col()
            .gap(ui::sp(8.))
            .child(crate::settings_view::titled("Sign-in methods", rows))
            .when_some(error, |d, e| {
                d.child(
                    div()
                        .px(ui::sp(4.))
                        .text_size(px(ui::TEXT_SM))
                        .font_weight(FontWeight::NORMAL)
                        .text_color(theme::error())
                        .child(crate::redact::text(&e).into_owned()),
                )
            })
            .child(
                div()
                    .px(ui::sp(4.))
                    .text_size(px(ui::TEXT_XS))
                    .text_color(theme::text_faint())
                    .child("Each change is mailed to your account's addresses. Two accounts are never joined because their addresses match."),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn method(provider: &str) -> SignInMethod {
        SignInMethod {
            provider: provider.into(),
            name: "x".into(),
            linked_at: 0,
        }
    }

    #[test]
    fn the_last_method_cannot_be_removed() {
        assert!(!removable(&[method("github")], "github"));
        let both = [method("github"), method("email")];
        assert!(removable(&both, "github"));
        assert!(removable(&both, "email"));
        assert!(!removable(&[method("github")], "email"));
    }

    #[test]
    fn what_is_missing_can_be_added() {
        assert_eq!(missing(&[]), ["github", "email"]);
        assert_eq!(missing(&[method("email")]), ["github"]);
        assert!(missing(&[method("github"), method("email")]).is_empty());
        // A provider of a later server is listed, and adds nothing.
        assert_eq!(missing(&[method("google"), method("github")]), ["email"]);
    }
}
