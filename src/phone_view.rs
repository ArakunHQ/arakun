//! The Phone tab: turns the phone page on and off, picks how phones reach this
//! computer (the local network, Tailscale or the relay). On the network and
//! over Tailscale it pairs phones with a short code (shown as text and as a QR
//! code) and lists the ones paired so far, each with a way to unpair it.
//! Through the relay a phone signs in to the same account instead: the tab
//! signs in, and lists the account's signed-in devices, each with a way to
//! sign it out, and its computers, each with a way to remove it. The shell
//! runs the server (`shell::phone`).

use gpui::AppContext as _;
use gpui::prelude::FluentBuilder as _;
use gpui::{
    App, ClipboardItem, Context, EventEmitter, FocusHandle, FontWeight, InteractiveElement as _,
    IntoElement, ParentElement as _, Render, SharedString, StatefulInteractiveElement as _,
    Styled as _, Window, div, px,
};

use crate::cloud::BetaPlatform;
use crate::input::{InputEvent, TextInput};
use crate::phone::Reach;
use crate::theme;
use crate::ui::{self, ButtonKind, Type, Typography as _, icon};

pub enum PhoneEvent {
    Enable,
    Disable,
    /// Phones are to reach this computer this way from now on.
    SetReach(Reach),
    /// Make a pairing code for a new phone.
    Pair,
    /// Drop the current code before it expires.
    CancelPairing,
    /// Forget a device; its connections are cut.
    Unpair(String),
    /// A new relay key, so a new address: every paired phone is forgotten too.
    NewIdentity,
    /// Open a link in a browser tab here: the pairing page, to try it without a
    /// phone, or a page that helps set things up.
    OpenHere(String),
    /// Sign in, so the relay lets in the account's devices.
    SignIn,
    /// Sign out one of the account's signed-in devices, by its id.
    SignOutDevice(String),
    /// Remove one of the account's computers, by its label.
    RemoveComputer(String),
    /// Sign up for a phone app beta, to be invited at this email (the
    /// account's GitHub email when empty). Again while pending, it changes
    /// the email.
    JoinBeta {
        platform: BetaPlatform,
        email: String,
    },
}

/// Where the account stands with the phone app betas, which the operator
/// admits testers to by hand.
#[derive(Clone, PartialEq, Eq)]
pub enum BetaState {
    SignedOut,
    Loading,
    Failed(String),
    Ready {
        ios: BetaStatus,
        android: BetaStatus,
    },
}

/// One beta, as the service last said.
#[derive(Clone, PartialEq, Eq)]
pub struct BetaStatus {
    /// "none", "pending", "admitted" or "declined".
    pub status: String,
    pub email: Option<String>,
    /// When a declined account may sign up again.
    pub retry_at: Option<u64>,
    /// A sign-up is on its way.
    pub busy: bool,
    /// Why the last sign-up failed.
    pub error: Option<String>,
}

/// What a beta's row says, and the label of its button when it has one.
fn beta_text(platform: BetaPlatform, b: &BetaStatus, now: u64) -> (String, Option<&'static str>) {
    let email = b
        .email
        .as_deref()
        .map(crate::redact::value)
        .unwrap_or_default();
    let (store, join) = match platform {
        BetaPlatform::Ios => ("TestFlight", "Join the iOS beta"),
        BetaPlatform::Android => ("Google Play", "Join the Android beta"),
    };
    match b.status.as_str() {
        "pending" => (
            format!("Signed up. We add testers by hand; the {store} invite will go to {email}."),
            Some("Change email"),
        ),
        "admitted" => (
            format!("You are in. Look for the {store} invite at {email}."),
            None,
        ),
        "declined" => match b.retry_at.filter(|t| *t > now) {
            Some(t) => {
                let days = (t - now).div_ceil(24 * 60 * 60);
                let when = if days == 1 {
                    "1 day".to_string()
                } else {
                    format!("{days} days")
                };
                (
                    format!("Not this time. You can sign up again in {when}."),
                    None,
                )
            }
            None => (
                "Not last time, but you can sign up again.".to_string(),
                Some(join),
            ),
        },
        _ => (
            format!(
                "Get Arakun for {} through {store}.",
                platform_name(platform)
            ),
            Some(join),
        ),
    }
}

fn platform_name(platform: BetaPlatform) -> &'static str {
    match platform {
        BetaPlatform::Ios => "iOS",
        BetaPlatform::Android => "Android",
    }
}

/// This computer as the relay knows it.
#[derive(Clone, PartialEq, Eq)]
pub struct RelayInfo {
    /// Where phones find it: the relay's own page, which lists the account's
    /// computers, such as `https://app.arakun.dev`.
    pub url: String,
    pub account: RelayAccount,
}

/// The account the relay lets devices in by.
#[derive(Clone, PartialEq, Eq)]
pub enum RelayAccount {
    /// The sign-in is being read, or the service has not said whose it is yet.
    Checking,
    SignedOut,
    SignedIn {
        login: String,
        devices: SignedInList,
        computers: ComputerList,
    },
}

/// The account's computers running the app, as far as the service has said.
#[derive(Clone, PartialEq, Eq)]
pub enum ComputerList {
    Loading,
    Listed(Vec<AccountComputer>),
    Failed(String),
}

/// A computer of the account that runs the app.
#[derive(Clone, PartialEq, Eq)]
pub struct AccountComputer {
    pub label: String,
    pub name: String,
    pub online: bool,
    /// Unix seconds.
    pub last_seen_at: u64,
    /// This computer: listed, but not removed from here.
    pub this: bool,
}

/// What a Remove asks first, naming the machine: a computer still signed in
/// comes back when it next connects, so that is said too.
pub fn remove_question(name: &str) -> String {
    format!(
        "Remove {} from your account? If it is still signed in, it comes back the next time it connects.",
        crate::redact::value(name)
    )
}

/// The account's signed-in devices, as far as the service has said.
#[derive(Clone, PartialEq, Eq)]
pub enum SignedInList {
    Loading,
    Listed(Vec<SignedInDevice>),
    Failed(String),
}

/// A device signed in to the account.
#[derive(Clone, PartialEq, Eq)]
pub struct SignedInDevice {
    pub id: String,
    pub name: String,
    /// `app`, `server`, `phone`, `ios` or `android`.
    pub kind: String,
    /// Unix seconds.
    pub last_used_at: u64,
    /// This computer's own sign-in.
    pub current: bool,
}

/// What a signed-in device is, in a word or two.
fn kind_text(kind: &str) -> &str {
    match kind {
        "app" => "Arakun app",
        "server" => "server",
        "phone" => "phone page",
        "ios" => "iPhone app",
        "android" => "Android app",
        other => other,
    }
}

/// Where phones find this computer, as far as the shell knows right now.
#[derive(Clone, PartialEq, Eq)]
pub enum Address {
    Ready(String),
    /// Being worked out (asking Tailscale).
    Pending(String),
    /// Not reachable until the user does something; an optional link that helps.
    Problem {
        what: String,
        link: Option<(&'static str, String)>,
    },
}

/// The code a new phone may bring right now.
#[derive(Clone, PartialEq, Eq)]
pub struct PairingInfo {
    pub code: String,
    pub url: String,
    pub seconds_left: u64,
}

#[derive(Clone, PartialEq, Eq)]
pub struct DeviceInfo {
    pub id: String,
    pub name: String,
    pub paired_at: u64,
    pub last_seen: u64,
    pub connected: bool,
}

/// `relay` is there whenever the relay is the chosen way in, on or off.
#[derive(Clone, PartialEq, Eq)]
pub enum PhoneState {
    Off {
        reach: Reach,
        relay: Option<RelayInfo>,
    },
    On {
        reach: Reach,
        relay: Option<RelayInfo>,
        address: Address,
        pairing: Option<PairingInfo>,
        devices: Vec<DeviceInfo>,
        phones: usize,
    },
}

impl PhoneState {
    fn reach(&self) -> Reach {
        match self {
            PhoneState::Off { reach, .. } | PhoneState::On { reach, .. } => *reach,
        }
    }

    fn relay(&self) -> Option<&RelayInfo> {
        match self {
            PhoneState::Off { relay, .. } | PhoneState::On { relay, .. } => relay.as_ref(),
        }
    }
}

pub struct PhoneView {
    state: PhoneState,
    error: Option<SharedString>,
    /// What was last copied to the clipboard, so its button says so.
    copied: Option<String>,
    /// "New identity" was pressed once; the next press is the one that counts.
    confirm_identity: bool,
    /// Remove was pressed once on this computer, by label; the next press counts.
    confirm_remove: Option<String>,
    beta: BetaState,
    /// The email to invite to a beta.
    beta_email: gpui::Entity<TextInput>,
    /// The account's email was put in the field once; after that it is the user's.
    beta_email_filled: bool,
    focus: FocusHandle,
    _beta_email_sub: gpui::Subscription,
}

/// Under the sign-in button: phones paired by link before accounts stopped
/// working through the relay, and the ways in that need no account.
const SIGN_IN_OR_TAILSCALE: &str = "Since 0.19 phones paired by link must sign in to the same account. To use no account, pick Tailscale or Same Wi-Fi above.";
/// Said next to the way in when the page comes from this computer.
const AWAKE_NOTE: &str = "In this mode the page comes from this computer, so it only opens while the computer is on and awake. Relay works from anywhere and says when the computer is offline.";

impl EventEmitter<PhoneEvent> for PhoneView {}

impl PhoneView {
    pub fn new(cx: &mut Context<Self>) -> Self {
        let beta_email = cx.new(|cx| TextInput::new("you@example.com", cx).redacted());
        // Typing counts as filling it: the account's email no longer replaces it.
        let sub = cx.subscribe(&beta_email, |this: &mut Self, _, ev, _| {
            if let InputEvent::Changed = ev {
                this.beta_email_filled = true;
            }
        });
        Self {
            state: PhoneState::Off {
                reach: Reach::Lan,
                relay: None,
            },
            error: None,
            copied: None,
            confirm_identity: false,
            confirm_remove: None,
            beta: BetaState::SignedOut,
            beta_email,
            beta_email_filled: false,
            focus: cx.focus_handle(),
            _beta_email_sub: sub,
        }
    }

    /// What the service says of the betas; `email` is the account's GitHub
    /// email, put in the field the first time there is one.
    pub fn set_beta(&mut self, beta: BetaState, email: Option<&str>, cx: &mut Context<Self>) {
        if let Some(email) = email.filter(|_| !self.beta_email_filled) {
            self.beta_email
                .update(cx, |i, cx| i.set_text(email.to_string(), cx));
        }
        if self.beta != beta {
            self.beta = beta;
            cx.notify();
        }
    }

    pub fn focus(&self, window: &mut Window, cx: &mut App) {
        window.focus(&self.focus, cx);
    }

    pub fn set_state(&mut self, state: PhoneState, cx: &mut Context<Self>) {
        if self.state != state {
            let pairing_changed = match (&self.state, &state) {
                (PhoneState::On { pairing: a, .. }, PhoneState::On { pairing: b, .. }) => {
                    a.as_ref().map(|p| &p.url) != b.as_ref().map(|p| &p.url)
                }
                _ => true,
            };
            if self.state.relay() != state.relay() {
                self.confirm_identity = false;
            }
            self.state = state;
            self.error = None;
            if pairing_changed {
                self.copied = None;
            }
            cx.notify();
        }
    }

    pub fn set_phones(&mut self, phones: usize, cx: &mut Context<Self>) {
        if let PhoneState::On { phones: p, .. } = &mut self.state
            && *p != phones
        {
            *p = phones;
            cx.notify();
        }
    }

    /// Whether a code is on screen, so the shell keeps the countdown moving.
    pub fn pairing_shown(&self) -> bool {
        matches!(
            &self.state,
            PhoneState::On {
                pairing: Some(_),
                ..
            }
        )
    }

    pub fn set_error(&mut self, message: String, cx: &mut Context<Self>) {
        self.error = Some(message.into());
        cx.notify();
    }

    fn copy(&mut self, text: &str, cx: &mut Context<Self>) {
        cx.write_to_clipboard(ClipboardItem::new_string(text.to_string()));
        self.copied = Some(text.to_string());
        cx.notify();
    }

    fn render_qr(url: &str) -> impl IntoElement {
        const CELL: f32 = 4.;
        const QUIET: usize = 4;
        let code = qrcode::QrCode::new(url.as_bytes()).ok();
        let Some(code) = code else {
            return div().child("QR code unavailable");
        };
        let width = code.width();
        let colors = code.to_colors();
        // The quiet zone is part of the code: four modules, the size the
        // standard asks for, whatever the Spacing setting. A fixed box with
        // the modules centred makes it, so no padding scales it.
        let side = px((width + 2 * QUIET) as f32 * CELL);
        let mut rows = div()
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .flex_none()
            .size(side)
            .bg(gpui::white())
            .rounded(crate::ui::rad(ui::RADIUS));
        for y in 0..width {
            let mut row = div().flex();
            for x in 0..width {
                let dark = colors[y * width + x] == qrcode::Color::Dark;
                row = row.child(div().size(px(CELL)).when(dark, |d| d.bg(gpui::black())));
            }
            rows = rows.child(row);
        }
        rows
    }

    /// Where the QR code goes while redacted mode is on: a code on screen is a
    /// code anyone watching can scan. Same size, so nothing moves.
    fn render_hidden_qr(url: &str) -> impl IntoElement {
        const CELL: f32 = 4.;
        const QUIET: usize = 4;
        let width = qrcode::QrCode::new(url.as_bytes()).map_or(25, |c| c.width());
        div()
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap(crate::ui::sp(6.))
            .flex_none()
            .size(px((width + 2 * QUIET) as f32 * CELL))
            .p(crate::ui::sp(12.))
            .rounded(crate::ui::rad(ui::RADIUS))
            .border_1()
            .border_color(theme::border())
            .bg(theme::fill_raised())
            .text_size(px(ui::TEXT_XS))
            .text_color(theme::text_faint())
            .text_center()
            .child(icon("view-off", 20.).text_color(theme::text_subtle()))
            .child(
                "Hidden in redacted mode. Copy the link, or turn the mode off with Ctrl+Shift+H.",
            )
    }

    fn section_title(text: &'static str) -> impl IntoElement {
        div()
            .text_size(px(ui::TEXT_SM))
            .font_weight(FontWeight::SEMIBOLD)
            .text_color(theme::text_muted())
            .child(text)
    }

    /// The phone app betas: the email to invite and a row for each store.
    fn render_beta(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let note = |text: String, color: gpui::Hsla| {
            div()
                .text_size(px(ui::TEXT_SM))
                .text_color(color)
                .child(text)
        };
        let section = div()
            .flex()
            .flex_col()
            .gap(crate::ui::sp(10.))
            .child(Self::section_title("Phone app beta"))
            .child(note(
                "Arakun for iOS and Android is in beta. Sign up and we add testers by hand, a few at a time. The invite comes by email: use your Apple ID for iOS and your Google account for Android.".into(),
                theme::text_muted(),
            ));
        match &self.beta {
            BetaState::SignedOut => section
                .child(note("Sign in to sign up.".into(), theme::text_faint()))
                .child(
                    div().flex().child(
                        ui::button_with(
                            "phone-beta-sign-in",
                            Some("user"),
                            "Sign in",
                            ButtonKind::Outline,
                            true,
                        )
                        .on_click(cx.listener(|_, _, _, cx| cx.emit(PhoneEvent::SignIn))),
                    ),
                )
                .into_any_element(),
            BetaState::Loading => section
                .child(note("Asking your account…".into(), theme::text_faint()))
                .into_any_element(),
            BetaState::Failed(why) => section
                .child(note(
                    crate::redact::text(why).into_owned(),
                    theme::attention().into(),
                ))
                .into_any_element(),
            BetaState::Ready { ios, android } => {
                let now = crate::model::unix_now();
                section
                    .child(ui::field().max_w(px(360.)).child(self.beta_email.clone()))
                    .child(self.render_beta_row(BetaPlatform::Ios, ios, now, cx))
                    .child(self.render_beta_row(BetaPlatform::Android, android, now, cx))
                    .into_any_element()
            }
        }
    }

    fn render_beta_row(
        &self,
        platform: BetaPlatform,
        b: &BetaStatus,
        now: u64,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let (text, action) = beta_text(platform, b, now);
        let input = self.beta_email.clone();
        div()
            .flex()
            .flex_col()
            .gap(crate::ui::sp(4.))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(crate::ui::sp(10.))
                    .child(
                        div()
                            .w(px(64.))
                            .flex_none()
                            .text_size(px(ui::TEXT_SM))
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(platform_name(platform)),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_size(px(ui::TEXT_SM))
                            .text_color(theme::text_muted())
                            .child(text),
                    )
                    .when_some(action, |d, label| {
                        let label = if b.busy { "Signing up…" } else { label };
                        d.child(
                            ui::button(
                                match platform {
                                    BetaPlatform::Ios => "phone-beta-ios",
                                    BetaPlatform::Android => "phone-beta-android",
                                },
                                label,
                                ButtonKind::Outline,
                                !b.busy,
                            )
                            .when(!b.busy, |btn| {
                                btn.on_click(cx.listener(move |_, _, _, cx| {
                                    let email = input.read(cx).text().trim().to_string();
                                    cx.emit(PhoneEvent::JoinBeta { platform, email });
                                }))
                            }),
                        )
                    }),
            )
            .when_some(b.error.clone(), |d, e| {
                d.child(
                    div()
                        .pl(crate::ui::sp(74.))
                        .text_size(px(ui::TEXT_XS))
                        .text_color(theme::attention())
                        .child(crate::redact::text(&e).into_owned()),
                )
            })
    }

    /// The ways in, and what each means.
    fn render_reach(reach: Reach, cx: &mut Context<Self>) -> impl IntoElement {
        let explain = match reach {
            Reach::Lan => {
                "Phones on the same Wi-Fi reach this computer directly, in the clear. Nothing leaves your network; use it on networks you trust."
            }
            Reach::Tailscale => {
                "Phones in your tailnet reach this computer from anywhere over HTTPS, at its Tailscale name. Needs Tailscale on both, and HTTPS certificates turned on in the tailnet. Push notifications need this."
            }
            Reach::Relay => {
                "Phones signed in to your GitHub account reach this computer from anywhere through Arakun's relay, with nothing to install or set up. The relay only passes bytes along: everything is encrypted end to end between the phone and this computer, with a key the relay never sees."
            }
        };
        div()
            .flex()
            .flex_col()
            .gap(crate::ui::sp(10.))
            .child(Self::section_title("How phones reach this computer"))
            .child(
                div().flex().child(
                    ui::segmented()
                        .child(
                            ui::segment(
                                "phone-reach-lan",
                                Some("signal-full02"),
                                "Same Wi-Fi",
                                reach == Reach::Lan,
                            )
                            .on_click(
                                cx.listener(|_, _, _, cx| {
                                    cx.emit(PhoneEvent::SetReach(Reach::Lan))
                                }),
                            ),
                        )
                        .child(
                            ui::segment(
                                "phone-reach-ts",
                                Some("globe"),
                                "Tailscale",
                                reach == Reach::Tailscale,
                            )
                            .on_click(cx.listener(|_, _, _, cx| {
                                cx.emit(PhoneEvent::SetReach(Reach::Tailscale))
                            })),
                        )
                        .child(
                            ui::segment(
                                "phone-reach-relay",
                                Some("cloud-upload"),
                                "Relay",
                                reach == Reach::Relay,
                            )
                            .on_click(cx.listener(|_, _, _, cx| {
                                cx.emit(PhoneEvent::SetReach(Reach::Relay))
                            })),
                        ),
                ),
            )
            .child(
                div()
                    .text_size(px(ui::TEXT_XS))
                    .text_color(theme::text_faint())
                    .child(explain),
            )
            .when(reach != Reach::Relay, |d| {
                // The page is served by this computer in these two modes, so a
                // sleeping computer leaves the phone with a blank page and no hint why.
                d.child(
                    div()
                        .flex()
                        .flex_col()
                        .items_start()
                        .gap(crate::ui::sp(8.))
                        .child(
                            div()
                                .text_size(px(ui::TEXT_XS))
                                .text_color(theme::text_muted())
                                .child(AWAKE_NOTE),
                        )
                        .child(
                            ui::button("phone-use-relay", "Use Relay", ButtonKind::Outline, true)
                                .on_click(cx.listener(|_, _, _, cx| {
                                    cx.emit(PhoneEvent::SetReach(Reach::Relay))
                                })),
                        ),
                )
            })
    }

    /// The account the relay lets devices in by: the sign-in, the page to
    /// open on the phone, and the account's signed-in devices.
    fn render_relay(&self, relay: &RelayInfo, cx: &mut Context<Self>) -> gpui::AnyElement {
        let section = div()
            .flex()
            .flex_col()
            .gap(crate::ui::sp(10.))
            .child(Self::section_title("Your account"));
        match &relay.account {
            RelayAccount::Checking => section
                .child(
                    div()
                        .text_size(px(ui::TEXT_SM))
                        .text_color(theme::text_faint())
                        .child("Checking your sign-in…"),
                )
                .into_any_element(),
            RelayAccount::SignedOut => section
                .child(
                    div()
                        .text_size(px(ui::TEXT_SM))
                        .text_color(theme::text_muted())
                        .child("Sign in to reach this computer from anywhere"),
                )
                .child(
                    div().flex().child(
                        ui::button_with(
                            "phone-sign-in",
                            Some("user"),
                            "Sign in",
                            ButtonKind::Primary,
                            true,
                        )
                        .on_click(cx.listener(|_, _, _, cx| cx.emit(PhoneEvent::SignIn))),
                    ),
                )
                .child(
                    div()
                        .text_size(px(ui::TEXT_XS))
                        .text_color(theme::text_faint())
                        .child(SIGN_IN_OR_TAILSCALE),
                )
                .into_any_element(),
            RelayAccount::SignedIn {
                login,
                devices,
                computers,
            } => section
                .child(
                    div()
                        .text_size(px(ui::TEXT_SM))
                        .text_color(theme::text_muted())
                        .child(format!(
                            "Signed in as {}",
                            crate::arakun_teams::account_name(login)
                        )),
                )
                .child(self.render_page(&relay.url, cx))
                .child(Self::render_signed_in(devices, cx))
                .child(self.render_computers(computers, cx))
                .child(self.render_identity(cx))
                .into_any_element(),
        }
    }

    /// The page to open on the phone, as text and as a QR code.
    fn render_page(&self, url: &str, cx: &mut Context<Self>) -> impl IntoElement {
        let copied = self.copied.as_deref() == Some(url);
        let copy = url.to_string();
        // The relay's page is public, and opens nothing without a sign-in:
        // unlike a pairing code, it shows in redacted mode too, text and code.
        div()
            .flex()
            .gap(crate::ui::sp(24.))
            .items_start()
            .child(Self::render_qr(url))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(crate::ui::sp(10.))
                    .min_w_0()
                    .child(
                        div()
                            .text_size(px(ui::TEXT_SM))
                            .text_color(theme::text_muted())
                            .child("On your phone, open this page and sign in with the same GitHub account:"),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .flex_wrap()
                            .gap(crate::ui::sp(8.))
                            .child(
                                div()
                                    .font_family(theme::FONT_MONO)
                                    .text_size(px(ui::TEXT_SM))
                                    .px(crate::ui::sp(8.))
                                    .py(crate::ui::sp(3.))
                                    .rounded(crate::ui::rad(ui::RADIUS))
                                    .bg(theme::fill_raised())
                                    .text_color(theme::content())
                                    .child(url.to_string()),
                            )
                            .child(
                                ui::button(
                                    "phone-relay-copy",
                                    if copied { "Copied" } else { "Copy" },
                                    ButtonKind::Outline,
                                    true,
                                )
                                .on_click(cx.listener(move |this, _, _, cx| this.copy(&copy, cx))),
                            ),
                    ),
            )
    }

    /// The account's signed-in devices, each but this computer with a way to
    /// sign it out.
    fn render_signed_in(devices: &SignedInList, cx: &mut Context<Self>) -> impl IntoElement {
        let faint = |text: String| {
            div()
                .text_size(px(ui::TEXT_SM))
                .text_color(theme::text_faint())
                .child(text)
        };
        let list = match devices {
            SignedInList::Loading => faint("Asking your account…".into()).into_any_element(),
            SignedInList::Failed(why) => div()
                .text_size(px(ui::TEXT_SM))
                .text_color(theme::attention())
                .child(crate::redact::text(why).into_owned())
                .into_any_element(),
            SignedInList::Listed(list) => {
                let now = crate::model::unix_now();
                let mut rows = div().flex().flex_col().gap(crate::ui::sp(6.));
                for d in list {
                    rows = rows.child(Self::render_signed_in_device(d, now, cx));
                }
                rows.into_any_element()
            }
        };
        div()
            .flex()
            .flex_col()
            .gap(crate::ui::sp(10.))
            .child(Self::section_title("Signed-in devices"))
            .child(list)
    }

    fn render_signed_in_device(
        d: &SignedInDevice,
        now: u64,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let id = d.id.clone();
        div()
            .id(SharedString::from(format!("phone-device-{}", d.id)))
            .flex()
            .items_center()
            .gap(crate::ui::sp(12.))
            .px(crate::ui::sp(12.))
            .py(crate::ui::sp(8.))
            .rounded(crate::ui::rad(ui::RADIUS))
            .bg(theme::fill_raised())
            .border_1()
            .border_color(theme::border())
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_w_0()
                    .child(
                        div()
                            .text_size(px(ui::TEXT_SM))
                            .child(crate::redact::value(&d.name)),
                    )
                    .child(
                        div()
                            .text_size(px(ui::TEXT_XS))
                            .text_color(theme::text_faint())
                            .child(format!(
                                "{} · last used {}",
                                kind_text(&d.kind),
                                crate::memory::ago(d.last_used_at, now)
                            )),
                    ),
            )
            .child(if d.current {
                ui::badge("This computer", ui::Tone::Neutral).into_any_element()
            } else {
                ui::button(
                    SharedString::from(format!("phone-sign-out-{}", d.id)),
                    "Sign out",
                    ButtonKind::Ghost,
                    true,
                )
                .on_click(
                    cx.listener(move |_, _, _, cx| cx.emit(PhoneEvent::SignOutDevice(id.clone()))),
                )
                .into_any_element()
            })
    }

    /// The account's computers running the app, each but this one with a way
    /// to remove it from the account.
    fn render_computers(
        &self,
        computers: &ComputerList,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let list = match computers {
            ComputerList::Loading => div()
                .text_size(px(ui::TEXT_SM))
                .text_color(theme::text_faint())
                .child("Asking your account…")
                .into_any_element(),
            ComputerList::Failed(why) => div()
                .text_size(px(ui::TEXT_SM))
                .text_color(theme::attention())
                .child(crate::redact::text(why).into_owned())
                .into_any_element(),
            ComputerList::Listed(list) => {
                let now = crate::model::unix_now();
                let mut rows = div().flex().flex_col().gap(crate::ui::sp(6.));
                for c in list {
                    rows = rows.child(self.render_computer(c, now, cx));
                }
                rows.into_any_element()
            }
        };
        div()
            .flex()
            .flex_col()
            .gap(crate::ui::sp(10.))
            .child(Self::section_title("Your computers"))
            .child(list)
    }

    fn render_computer(
        &self,
        c: &AccountComputer,
        now: u64,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let label = c.label.clone();
        let seen = if c.online {
            "online".to_string()
        } else {
            format!("last seen {}", crate::memory::ago(c.last_seen_at, now))
        };
        let row = div()
            .id(SharedString::from(format!("phone-computer-{}", c.name)))
            .flex()
            .items_center()
            .gap(crate::ui::sp(12.))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_w_0()
                    .child(
                        div()
                            .text_size(px(ui::TEXT_SM))
                            .child(crate::redact::value(&c.name)),
                    )
                    .child(
                        div()
                            .text_size(px(ui::TEXT_XS))
                            .text_color(theme::text_faint())
                            .child(format!("Arakun app · {seen}")),
                    ),
            )
            .child(if c.this {
                ui::badge("This computer", ui::Tone::Neutral).into_any_element()
            } else {
                ui::button(
                    SharedString::from(format!("phone-remove-{}", c.label)),
                    "Remove",
                    ButtonKind::Ghost,
                    true,
                )
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.confirm_remove = Some(label.clone());
                    cx.notify();
                }))
                .into_any_element()
            });
        let confirm =
            (!c.this && self.confirm_remove.as_deref() == Some(c.label.as_str())).then(|| {
                let label = c.label.clone();
                div()
                    .flex()
                    .items_center()
                    .flex_wrap()
                    .gap(crate::ui::sp(8.))
                    .text_size(px(ui::TEXT_SM))
                    .text_color(theme::attention())
                    .child(remove_question(&c.name))
                    .child(
                        ui::button(
                            SharedString::from(format!("phone-remove-yes-{}", c.label)),
                            "Remove",
                            ButtonKind::Danger,
                            true,
                        )
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.confirm_remove = None;
                            cx.emit(PhoneEvent::RemoveComputer(label.clone()));
                        })),
                    )
                    .child(
                        ui::button(
                            SharedString::from(format!("phone-remove-no-{}", c.label)),
                            "Keep it",
                            ButtonKind::Ghost,
                            true,
                        )
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.confirm_remove = None;
                            cx.notify();
                        })),
                    )
            });
        div()
            .flex()
            .flex_col()
            .gap(crate::ui::sp(8.))
            .px(crate::ui::sp(12.))
            .py(crate::ui::sp(8.))
            .rounded(crate::ui::rad(ui::RADIUS))
            .bg(theme::fill_raised())
            .border_1()
            .border_color(theme::border())
            .child(row)
            .children(confirm)
    }

    /// The key the relay knows this computer by, and the way to change it.
    fn render_identity(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        if self.confirm_identity {
            div()
                .flex()
                .items_center()
                .flex_wrap()
                .gap(crate::ui::sp(8.))
                .text_size(px(ui::TEXT_SM))
                .text_color(theme::attention())
                .child("A new key makes this a new computer in your account, and forgets the phones paired here.")
                .child(
                    ui::button(
                        "phone-identity-confirm",
                        "Replace it",
                        ButtonKind::Outline,
                        true,
                    )
                    .on_click(cx.listener(|_, _, _, cx| cx.emit(PhoneEvent::NewIdentity))),
                )
                .child(
                    ui::button("phone-identity-keep", "Keep it", ButtonKind::Ghost, true).on_click(
                        cx.listener(|this, _, _, cx| {
                            this.confirm_identity = false;
                            cx.notify();
                        }),
                    ),
                )
                .into_any_element()
        } else {
            div()
                .flex()
                .items_center()
                .flex_wrap()
                .gap(crate::ui::sp(8.))
                .text_size(px(ui::TEXT_XS))
                .text_color(theme::text_faint())
                .child("The relay knows this computer by a key only it has; phones check every connection against it.")
                .child(ui::button("phone-identity-new", "New identity", ButtonKind::Ghost, true).on_click(cx.listener(
                    |this, _, _, cx| {
                        this.confirm_identity = true;
                        cx.notify();
                    },
                )))
                .into_any_element()
        }
    }

    /// Where phones find this computer, or what is in the way. Through the relay
    /// the address is known from the start (the section above shows it), so a
    /// ready one is news about the connection itself.
    fn render_address(address: &Address, reach: Reach, cx: &mut Context<Self>) -> impl IntoElement {
        let row = div()
            .flex()
            .items_center()
            .flex_wrap()
            .gap(crate::ui::sp(8.))
            .text_size(px(ui::TEXT_SM));
        match address {
            Address::Ready(_) if reach == Reach::Relay => row
                .text_color(theme::text_muted())
                .child(
                    div()
                        .size(px(8.))
                        .rounded_full()
                        .bg(gpui::Hsla::from(theme::success())),
                )
                .child("Connected to the relay."),
            Address::Ready(url) => row
                .text_color(theme::text_muted())
                .child("Phones reach this computer at")
                .child(
                    div()
                        .font_family(theme::FONT_MONO)
                        .px(crate::ui::sp(8.))
                        .py(crate::ui::sp(3.))
                        .rounded(crate::ui::rad(ui::RADIUS))
                        .bg(theme::fill_raised())
                        .text_color(theme::content())
                        .child(crate::redact::value(url)),
                ),
            Address::Pending(what) => row
                .text_color(theme::text_faint())
                .child(crate::redact::text(what).into_owned()),
            Address::Problem { what, link } => row
                .text_color(theme::attention())
                .child(crate::redact::text(what).into_owned())
                .when_some(link.clone(), |d, (label, url)| {
                    d.child(
                        ui::button("phone-address-help", label, ButtonKind::Outline, true)
                            .on_click(cx.listener(move |_, _, _, cx| {
                                cx.emit(PhoneEvent::OpenHere(url.clone()))
                            })),
                    )
                }),
        }
    }

    fn render_pairing(&self, pairing: &PairingInfo, cx: &mut Context<Self>) -> impl IntoElement {
        let (url_open, url_copy) = (pairing.url.clone(), pairing.url.clone());
        let copied = self.copied.as_deref() == Some(pairing.url.as_str());
        // Through the relay the link carries everything: there is no code to type.
        let code = (pairing.code.len() == 6)
            .then(|| format!("{} {}", &pairing.code[..3], &pairing.code[3..]));
        let left = format!(
            "{}:{:02}",
            pairing.seconds_left / 60,
            pairing.seconds_left % 60
        );
        let how = match code {
            Some(_) => {
                "Scan the code with the phone's camera, or open the page on it and type this code:"
            }
            None => "Scan the code with the phone's camera, or send it the link:",
        };
        let works_once = match code {
            Some(_) => {
                "The code works once. The phone gets a key of its own, which you can revoke below."
            }
            None => {
                "The link works once. The phone gets a key of its own, which you can revoke below."
            }
        };
        div()
            .flex()
            .gap(crate::ui::sp(24.))
            .items_start()
            .child(if crate::redact::on() {
                Self::render_hidden_qr(&pairing.url).into_any_element()
            } else {
                Self::render_qr(&pairing.url).into_any_element()
            })
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(crate::ui::sp(10.))
                    .min_w_0()
                    .child(
                        div()
                            .text_size(px(ui::TEXT_SM))
                            .text_color(theme::text_muted())
                            .child(how),
                    )
                    .child(
                        div()
                            .flex()
                            .items_baseline()
                            .gap(crate::ui::sp(12.))
                            .children(code.map(|code| {
                                div()
                                    .ty(Type::Display)
                                    .font_family(theme::FONT_MONO)
                                    .text_color(theme::content())
                                    .child(crate::redact::value(&code))
                            }))
                            .child(
                                div()
                                    .text_size(px(ui::TEXT_XS))
                                    .text_color(theme::text_faint())
                                    .child(format!("expires in {left}")),
                            ),
                    )
                    .child(
                        div()
                            .flex()
                            .gap(crate::ui::sp(8.))
                            .child(
                                ui::button(
                                    "phone-copy",
                                    if copied { "Copied" } else { "Copy link" },
                                    ButtonKind::Outline,
                                    true,
                                )
                                .on_click(
                                    cx.listener(move |this, _, _, cx| this.copy(&url_copy, cx)),
                                ),
                            )
                            .child(
                                ui::button("phone-open", "Open here", ButtonKind::Outline, true)
                                    .on_click(cx.listener(move |_, _, _, cx| {
                                        cx.emit(PhoneEvent::OpenHere(url_open.clone()))
                                    })),
                            )
                            .child(
                                ui::button("phone-cancel-pair", "Cancel", ButtonKind::Ghost, true)
                                    .on_click(cx.listener(|_, _, _, cx| {
                                        cx.emit(PhoneEvent::CancelPairing)
                                    })),
                            ),
                    )
                    .child(
                        div()
                            .text_size(px(ui::TEXT_XS))
                            .text_color(theme::text_faint())
                            .child(works_once),
                    ),
            )
    }

    fn render_device(d: &DeviceInfo, now: u64, cx: &mut Context<Self>) -> impl IntoElement {
        let id = d.id.clone();
        let seen = if d.connected {
            "connected now".to_string()
        } else {
            format!("last seen {}", crate::memory::ago(d.last_seen, now))
        };
        div()
            .flex()
            .items_center()
            .gap(crate::ui::sp(12.))
            .px(crate::ui::sp(12.))
            .py(crate::ui::sp(8.))
            .rounded(crate::ui::rad(ui::RADIUS))
            .bg(theme::fill_raised())
            .border_1()
            .border_color(theme::border())
            .child(div().size(px(8.)).rounded_full().bg(if d.connected {
                gpui::Hsla::from(theme::success())
            } else {
                theme::text_faint()
            }))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_w_0()
                    .child(
                        div()
                            .text_size(px(ui::TEXT_SM))
                            .child(crate::redact::value(&d.name)),
                    )
                    .child(
                        div()
                            .text_size(px(ui::TEXT_XS))
                            .text_color(theme::text_faint())
                            .child(format!(
                                "paired {} · {seen}",
                                crate::memory::ago(d.paired_at, now)
                            )),
                    ),
            )
            .child(
                ui::button(
                    SharedString::from(format!("phone-unpair-{}", d.id)),
                    "Unpair",
                    ButtonKind::Ghost,
                    true,
                )
                .on_click(cx.listener(move |_, _, _, cx| cx.emit(PhoneEvent::Unpair(id.clone())))),
            )
    }
}

impl Render for PhoneView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let header = div()
            .flex()
            .flex_none()
            .items_center()
            .gap(crate::ui::sp(8.))
            .h(px(ui::TOOLBAR_H))
            .px(crate::ui::sp(12.))
            .border_b_1()
            .border_color(theme::stroke())
            .text_size(px(ui::TEXT_SM))
            .text_color(theme::text_subtle())
            .child(icon("signal-full02", 14.).text_color(theme::text_subtle()))
            .child(div().flex_none().child("Phone"))
            .child(div().flex_1())
            .when_some(
                match &self.state {
                    PhoneState::On { phones, .. } => Some(*phones),
                    PhoneState::Off { .. } => None,
                },
                |d, phones| {
                    d.child(div().text_color(theme::text_faint()).child(match phones {
                        0 => "no phone connected".to_string(),
                        1 => "1 phone connected".to_string(),
                        n => format!("{n} phones connected"),
                    }))
                },
            );

        let reach = self.state.reach();
        let intro = div()
            .flex()
            .flex_col()
            .gap(crate::ui::sp(6.))
            .child(div().text_size(px(ui::TEXT_LG)).font_weight(FontWeight::SEMIBOLD).child("Arakun on your phone"))
            .child(
                div().text_size(px(ui::TEXT_SM)).text_color(theme::text_muted()).child(
                    "Watch your sessions, read them live, send a message, stop a turn and answer the agent from your phone.",
                ),
            );

        // Only the LAN mode listens on the network; the loopback needs no firewall rule.
        let firewall = (reach == Reach::Lan).then(|| {
            div().text_size(px(ui::TEXT_XS)).text_color(theme::text_faint()).child(
                "The first time, Windows may ask whether to allow Arakun on the network: choose private networks, or the phone cannot reach this computer.",
            )
        });

        let relay = self
            .state
            .relay()
            .cloned()
            .map(|r| self.render_relay(&r, cx));
        let body = match self.state.clone() {
            PhoneState::Off { .. } => div()
                .flex()
                .flex_col()
                .gap(crate::ui::sp(20.))
                .child(intro)
                .child(self.render_beta(cx))
                .child(Self::render_reach(reach, cx))
                .children(relay)
                .when_some(self.error.clone(), |d, e| {
                    d.child(
                        div()
                            .text_size(px(ui::TEXT_SM))
                            .text_color(theme::error())
                            .child(e),
                    )
                })
                .child(
                    div().flex().child(
                        ui::button("phone-enable", "Enable", ButtonKind::Primary, true)
                            .on_click(cx.listener(|_, _, _, cx| cx.emit(PhoneEvent::Enable))),
                    ),
                )
                .children(firewall)
                .into_any_element(),
            PhoneState::On {
                address,
                pairing,
                devices,
                ..
            } => {
                let ready = matches!(address, Address::Ready(_));
                // Through the relay, devices come in by signing in: no code,
                // and the account says whether to sign in at all.
                let relay_reach = reach == Reach::Relay;
                let signed_out = matches!(
                    self.state.relay().map(|r| &r.account),
                    Some(RelayAccount::SignedOut)
                );
                let now = crate::model::unix_now();
                let mut list = div().flex().flex_col().gap(crate::ui::sp(6.));
                if devices.is_empty() {
                    list = list.child(
                        div().text_size(px(ui::TEXT_SM)).text_color(theme::text_faint()).child(
                            "No phone paired yet. Press Pair a phone and scan the code with it.",
                        ),
                    );
                }
                for d in &devices {
                    list = list.child(Self::render_device(d, now, cx));
                }
                div()
                    .flex()
                    .flex_col()
                    .gap(crate::ui::sp(20.))
                    .child(intro)
                    .child(self.render_beta(cx))
                    .child(Self::render_reach(reach, cx))
                    .children(relay)
                    .when(!(relay_reach && signed_out), |d| {
                        d.child(Self::render_address(&address, reach, cx))
                    })
                    .when(!relay_reach, |d| {
                        d.child(
                            div()
                                .flex()
                                .flex_col()
                                .gap(crate::ui::sp(10.))
                                .child(Self::section_title("Paired phones"))
                                .child(list)
                                .child(match &pairing {
                                    Some(p) => self.render_pairing(p, cx).into_any_element(),
                                    None => {
                                        div()
                                            .flex()
                                            .child(
                                                ui::button(
                                                    "phone-pair",
                                                    "Pair a phone",
                                                    ButtonKind::Primary,
                                                    ready,
                                                )
                                                .when(ready, |b| {
                                                    b.on_click(cx.listener(|_, _, _, cx| {
                                                        cx.emit(PhoneEvent::Pair)
                                                    }))
                                                }),
                                            )
                                            .into_any_element()
                                    }
                                }),
                        )
                    })
                    .children(firewall)
                    .child(
                        div().flex().child(
                            ui::button("phone-disable", "Disable", ButtonKind::Outline, true)
                                .on_click(cx.listener(|_, _, _, cx| cx.emit(PhoneEvent::Disable))),
                        ),
                    )
                    .into_any_element()
            }
        };

        div()
            .track_focus(&self.focus)
            .flex()
            .flex_col()
            .size_full()
            .bg(theme::background())
            .font_family(theme::FONT_SANS)
            .text_color(theme::content())
            .child(header)
            .child(
                div()
                    .id("phone-body")
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .child(
                        div()
                            .w_full()
                            .max_w(px(720.))
                            .mx_auto()
                            .p(crate::ui::sp(ui::PAGE_PAD))
                            .child(body),
                    ),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn status(status: &str, retry_at: Option<u64>) -> BetaStatus {
        BetaStatus {
            status: status.into(),
            email: Some("me@icloud.com".into()),
            retry_at,
            busy: false,
            error: None,
        }
    }

    #[test]
    fn each_beta_state_says_what_comes_next() {
        let now = 1_000_000;
        let (text, action) = beta_text(BetaPlatform::Ios, &status("none", None), now);
        assert!(text.contains("TestFlight"), "{text}");
        assert_eq!(action, Some("Join the iOS beta"));
        let (text, action) = beta_text(BetaPlatform::Android, &status("pending", None), now);
        assert!(
            text.contains("Google Play") && text.contains("me@icloud.com"),
            "{text}"
        );
        assert_eq!(action, Some("Change email"));
        let (_, action) = beta_text(BetaPlatform::Ios, &status("admitted", None), now);
        assert_eq!(action, None);
        let (text, action) = beta_text(
            BetaPlatform::Ios,
            &status("declined", Some(now + 2 * 86_400 - 5)),
            now,
        );
        assert!(text.contains("2 days"), "{text}");
        assert_eq!(action, None);
        let (_, action) = beta_text(BetaPlatform::Ios, &status("declined", Some(now - 1)), now);
        assert_eq!(action, Some("Join the iOS beta"));
    }

    #[test]
    fn removing_a_computer_asks_by_its_name() {
        let q = remove_question("Rik's MacBook");
        assert!(
            q.starts_with("Remove Rik's MacBook from your account?"),
            "{q}"
        );
        assert!(q.contains("comes back"), "{q}");
    }
}
