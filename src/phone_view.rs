//! The Phone tab: turns the phone page on and off, picks how phones reach this
//! computer (the local network, Tailscale or the relay), pairs phones with a
//! short code (shown as text and as a QR code) and lists the ones paired so far,
//! each with a way to unpair it. The shell runs the server (`shell::phone`).

use gpui::prelude::FluentBuilder as _;
use gpui::{
    App, ClipboardItem, Context, EventEmitter, FocusHandle, FontWeight, InteractiveElement as _,
    IntoElement, ParentElement as _, Render, SharedString, StatefulInteractiveElement as _,
    Styled as _, Window, div, px,
};

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
}

/// This computer as the relay knows it.
#[derive(Clone, PartialEq, Eq)]
pub struct RelayInfo {
    /// Where phones find it: `https://<label>.<relay host>/`, the label being
    /// derived from the computer's public key.
    pub url: String,
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
    focus: FocusHandle,
}

/// Said next to the way in when the page comes from this computer.
const AWAKE_NOTE: &str = "In this mode the page comes from this computer, so it only opens while the computer is on and awake. Relay works from anywhere and says when the computer is offline.";

impl EventEmitter<PhoneEvent> for PhoneView {}

impl PhoneView {
    pub fn new(cx: &mut Context<Self>) -> Self {
        Self {
            state: PhoneState::Off {
                reach: Reach::Lan,
                relay: None,
            },
            error: None,
            copied: None,
            confirm_identity: false,
            focus: cx.focus_handle(),
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
                "Phones reach this computer from anywhere through Arakun's relay, with nothing to install or set up. The relay only passes bytes along: everything is encrypted end to end between the phone and this computer, with a key the relay never sees."
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

    /// This computer's address on the relay, and the way to change it.
    fn render_relay(&self, relay: &RelayInfo, cx: &mut Context<Self>) -> gpui::AnyElement {
        let url = relay.url.clone();
        let copied = self.copied.as_deref() == Some(relay.url.as_str());
        let identity = if self.confirm_identity {
            div()
                .flex()
                .items_center()
                .flex_wrap()
                .gap(crate::ui::sp(8.))
                .text_size(px(ui::TEXT_SM))
                .text_color(theme::attention())
                .child("A new key means a new address: every paired phone has to pair again.")
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
                .child("The address comes from a key only this computer has; the relay cannot hand it to anyone else.")
                .child(ui::button("phone-identity-new", "New identity", ButtonKind::Ghost, true).on_click(cx.listener(
                    |this, _, _, cx| {
                        this.confirm_identity = true;
                        cx.notify();
                    },
                )))
                .into_any_element()
        };
        div()
            .flex()
            .flex_col()
            .gap(crate::ui::sp(10.))
            .child(Self::section_title("This computer on the relay"))
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
                            .child(crate::redact::value(&relay.url)),
                    )
                    .child(
                        ui::button(
                            "phone-relay-copy",
                            if copied { "Copied" } else { "Copy" },
                            ButtonKind::Outline,
                            true,
                        )
                        .on_click(cx.listener(move |this, _, _, cx| this.copy(&url, cx))),
                    ),
            )
            .child(identity)
            .into_any_element()
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
                    .child(Self::render_reach(reach, cx))
                    .children(relay)
                    .child(Self::render_address(&address, reach, cx))
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap(crate::ui::sp(10.))
                            .child(Self::section_title("Paired phones"))
                            .child(list)
                            .child(match &pairing {
                                Some(p) => self.render_pairing(p, cx).into_any_element(),
                                None => div()
                                    .flex()
                                    .child(
                                        ui::button(
                                            "phone-pair",
                                            "Pair a phone",
                                            ButtonKind::Primary,
                                            ready,
                                        )
                                        .when(ready, |b| {
                                            b.on_click(
                                                cx.listener(|_, _, _, cx| {
                                                    cx.emit(PhoneEvent::Pair)
                                                }),
                                            )
                                        }),
                                    )
                                    .into_any_element(),
                            }),
                    )
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
