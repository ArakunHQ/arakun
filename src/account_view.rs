//! The Factory account dialog and the usage card at the bottom of the rail.

use std::sync::atomic::{AtomicBool, Ordering};

use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, App, AppContext as _, Context, Entity, EventEmitter, FocusHandle, Focusable,
    FontWeight, Hsla, InteractiveElement as _, IntoElement, ParentElement as _, Render,
    SharedString, StatefulInteractiveElement as _, Styled as _, Subscription, Window, div, px,
    relative,
};

use crate::account::{self, Account, AccountEvent, Bucket, KeySource, Pool, UsageMode};
use crate::agent::AgentKind;
use crate::agent::plan::{PlanUsage, Seats};
use crate::input::{InputEvent, TextInput};
use crate::theme;
use crate::ui::{self, ButtonKind, icon};

pub enum AccountDialogEvent {
    Dismissed,
    OpenUrl(String),
}

pub struct AccountDialog {
    account: Entity<Account>,
    input: Entity<TextInput>,
    _subs: [Subscription; 3],
}

impl EventEmitter<AccountDialogEvent> for AccountDialog {}

impl AccountDialog {
    pub fn new(account: Entity<Account>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let input = cx.new(|cx| TextInput::new("fk-…", cx).mono().secret());
        let input_sub = cx.subscribe_in(&input, window, |this: &mut Self, _, ev, _, cx| match ev {
            InputEvent::Submit => this.connect(cx),
            InputEvent::Cancel => cx.emit(AccountDialogEvent::Dismissed),
            _ => {}
        });
        let account_sub = cx.subscribe(&account, |this: &mut Self, _, ev, cx| {
            if let AccountEvent::Connected = ev {
                this.input.update(cx, |i, cx| i.set_text("", cx));
            }
        });
        let observe = cx.observe(&account, |_, _, cx| cx.notify());
        account.update(cx, |a, cx| a.refresh(cx));
        if !account.read(cx).connected() {
            window.focus(&input.focus_handle(cx), cx);
        }
        Self {
            account,
            input,
            _subs: [input_sub, account_sub, observe],
        }
    }

    fn connect(&mut self, cx: &mut Context<Self>) {
        let key = self.input.read(cx).text().to_string();
        if key.trim().is_empty() || self.account.read(cx).busy() {
            return;
        }
        self.account.update(cx, |a, cx| a.connect(key, cx));
    }
}

impl Focusable for AccountDialog {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.input.focus_handle(cx)
    }
}

pub fn bar_color(pct: f32) -> Hsla {
    if pct >= 100. {
        theme::danger().into()
    } else if pct >= 70. {
        theme::warn().into()
    } else {
        theme::diff_add().into()
    }
}

pub fn bar(pct: f32, height: f32) -> impl IntoElement {
    let fill = (pct / 100.).clamp(0., 1.);
    div()
        .flex_1()
        .h(px(height))
        .rounded_full()
        .bg(theme::fill_selected())
        .child(
            div()
                .h_full()
                .rounded_full()
                .w(relative(fill))
                .when(fill > 0., |d| d.min_w(px(height)))
                .bg(bar_color(pct)),
        )
}

pub fn pool_rows(id: &'static str, title: &str, pool: &Pool, now: u64) -> impl IntoElement {
    div()
        .flex()
        .flex_col()
        .gap(crate::ui::sp(8.))
        .child(
            div()
                .text_size(px(ui::TEXT_XS))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(theme::text_subtle())
                .child(title.to_string()),
        )
        .children(
            pool.buckets()
                .into_iter()
                .enumerate()
                .map(|(i, (label, b))| {
                    let pct = b.current(now);
                    div()
                        .id((id, i))
                        .flex()
                        .items_center()
                        .gap(crate::ui::sp(10.))
                        .text_size(px(ui::TEXT_SM))
                        .child(
                            div()
                                .w(px(56.))
                                .text_color(theme::text_muted())
                                .child(label),
                        )
                        .child(bar(pct, 6.))
                        .child(
                            div()
                                .w(px(40.))
                                .flex()
                                .justify_end()
                                .font_family(theme::FONT_MONO)
                                .text_color(theme::content())
                                .child(format!("{:.0}%", pct.ceil())),
                        )
                        .child(
                            div()
                                .w(px(96.))
                                .text_size(px(ui::TEXT_XS))
                                .text_color(theme::text_subtle())
                                .child(reset_label(&b, now)),
                        )
                }),
        )
}

pub fn reset_label(b: &Bucket, now: u64) -> String {
    match b.window_end {
        Some(end) if end > now => format!("resets {}", account::when(end, now)),
        _ => "not started".into(),
    }
}

impl Render for AccountDialog {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let account = self.account.read(cx);
        let now = account::unix_now();
        let connected = account.connected();
        let busy = account.busy();
        let error = account.error().cloned();
        let description: SharedString = if connected {
            "Plan usage of the Factory account, the same limits droid checks before each turn."
                .into()
        } else {
            "Connect a Factory API key to see your plan usage in Arakun.".into()
        };

        let mut body = div()
            .id("acct-body")
            .flex()
            .flex_col()
            .min_h_0()
            .overflow_y_scroll()
            .gap(crate::ui::sp(16.))
            .p(crate::ui::sp(16.));

        if connected {
            match account.limits() {
                Some(limits) => {
                    let mode = limits.mode(now);
                    if let Some(p) = &limits.standard {
                        body = body.child(pool_rows("acct-std", "Standard Usage", p, now));
                    }
                    if let Some(p) = &limits.core
                        && (mode == UsageMode::DroidCore || p.highest(now).is_some())
                    {
                        body = body.child(pool_rows("acct-core", "Droid Core", p, now));
                    }
                    body = body
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .text_size(px(ui::TEXT_SM))
                                .child(
                                    div()
                                        .flex_1()
                                        .text_color(theme::text_muted())
                                        .child("Extra Usage balance"),
                                )
                                .child(div().font_family(theme::FONT_MONO).child(format!(
                                    "${:.2}",
                                    limits.extra_usage_cents as f64 / 100.
                                ))),
                        )
                        .child(
                            div()
                                .text_size(px(ui::TEXT_SM))
                                .line_height(px(18.))
                                .text_color(match mode {
                                    UsageMode::Blocked => theme::danger().into(),
                                    UsageMode::Standard => theme::mix(0.6),
                                    _ => theme::warn().into(),
                                })
                                .child(limits.summary(now)),
                        );
                }
                None if busy => {
                    body = body.child(
                        div()
                            .flex()
                            .items_center()
                            .gap(crate::ui::sp(8.))
                            .text_size(px(ui::TEXT_SM))
                            .text_color(theme::text_subtle())
                            .child(ui::spinner("acct-loading", 12., theme::mix(0.5)))
                            .child("Reading usage…"),
                    );
                }
                None => {}
            }
        } else {
            body = body
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .gap(crate::ui::sp(4.))
                        .text_size(px(ui::TEXT_SM))
                        .line_height(px(18.))
                        .text_color(theme::text_subtle())
                        .child("1. Create an API key in Factory: Settings → API Keys.")
                        .child("2. Paste it below. Arakun keeps it in the system keyring and only uses it to read usage; droid keeps using its own login."),
                )
                .child(
                    div()
                        .flex()
                        .gap(crate::ui::sp(8.))
                        .child(
                            div()
                                .flex_1()
                                .flex()
                                .items_center()
                                .h(px(ui::INPUT_H))
                                .px(crate::ui::sp(10.))
                                .rounded(crate::ui::rad(ui::RADIUS))
                                .border_1()
                                .border_color(theme::border_strong())
                                .bg(theme::background())
                                .text_size(px(ui::TEXT_MD))
                                .child(self.input.clone()),
                        )
                        .child(
                            ui::button("acct-connect", if busy { "Checking…" } else { "Connect" }, ButtonKind::Primary, !busy)
                                .h(px(32.))
                                .on_click(cx.listener(|this, _, _, cx| this.connect(cx))),
                        ),
                );
        }

        if let Some(e) = error {
            body = body.child(
                div()
                    .text_size(px(ui::TEXT_SM))
                    .text_color(theme::danger())
                    .child(e),
            );
        }

        let source = account.key_source();
        let fetched = account.fetched_at();
        let mut footer = div()
            .flex()
            .flex_none()
            .items_center()
            .gap(crate::ui::sp(8.))
            .px(crate::ui::sp(16.))
            .py(crate::ui::sp(10.))
            .border_t_1()
            .border_color(theme::border_subtle())
            .text_size(px(ui::TEXT_XS))
            .text_color(theme::text_subtle());
        if connected {
            footer = footer
                .child(div().flex_1().child(match (source, fetched) {
                    (Some(KeySource::Env), Some(t)) => format!(
                        "Key from FACTORY_API_KEY · updated {}",
                        account::ago(t, now)
                    ),
                    (Some(KeySource::Droid), Some(t)) => {
                        format!("From droid's login · updated {}", account::ago(t, now))
                    }
                    (_, Some(t)) => format!("Updated {}", account::ago(t, now)),
                    _ => String::new(),
                }))
                .child(
                    ui::button("acct-refresh", "Refresh", ButtonKind::Ghost, !busy)
                        .when(busy, |d| {
                            d.child(ui::spinner("acct-refresh-spin", 11., theme::mix(0.6)))
                        })
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.account.update(cx, |a, cx| a.refresh(cx))
                        })),
                )
                .when(source == Some(KeySource::Keyring), |d| {
                    d.child(
                        ui::button("acct-disconnect", "Disconnect", ButtonKind::Ghost, !busy)
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.account.update(cx, |a, cx| a.disconnect(cx));
                                window.focus(&this.input.focus_handle(cx), cx);
                            })),
                    )
                })
                .child(
                    ui::button("acct-open-usage", "Usage page", ButtonKind::Ghost, true)
                        .child(icon("link-square02", 12.).text_color(theme::text_subtle()))
                        .on_click(cx.listener(|_, _, _, cx| {
                            cx.emit(AccountDialogEvent::OpenUrl(account::USAGE_URL.into()))
                        })),
                );
        } else {
            footer = footer.child(div().flex_1()).child(
                ui::button(
                    "acct-open-keys",
                    "Open API Keys page",
                    ButtonKind::Ghost,
                    true,
                )
                .child(icon("link-square02", 12.).text_color(theme::text_subtle()))
                .on_click(cx.listener(|_, _, _, cx| {
                    cx.emit(AccountDialogEvent::OpenUrl(account::API_KEYS_URL.into()))
                })),
            );
        }

        ui::modal_panel("account", 460.)
            .child(ui::modal_header(
                "Factory account",
                Some(description),
                ui::close_button("account-close")
                    .on_click(cx.listener(|_, _, _, cx| cx.emit(AccountDialogEvent::Dismissed))),
            ))
            .child(body)
            .child(footer)
    }
}

// ---- the rail's usage card ----------------------------------------------------

static RAIL_CLAUDE: AtomicBool = AtomicBool::new(true);
static RAIL_DROID: AtomicBool = AtomicBool::new(true);
static RAIL_OPENCODE: AtomicBool = AtomicBool::new(true);

/// Whether the rail's usage card shows this provider. The Usage tab always
/// shows them all; the card is for a glance, so a plan that is used up or not
/// used can leave it. OpenCode shows its Go plan, only when a Go key exists.
pub fn rail_shows(agent: AgentKind) -> bool {
    match agent {
        AgentKind::Claude => RAIL_CLAUDE.load(Ordering::Relaxed),
        AgentKind::Droid => RAIL_DROID.load(Ordering::Relaxed),
        AgentKind::OpenCode => RAIL_OPENCODE.load(Ordering::Relaxed),
        // Pay as you go: no plan to keep an eye on. Codex's ChatGPT limits are
        // not read yet.
        AgentKind::Codex | AgentKind::WorkersAi => false,
    }
}

pub fn set_rail_shows(agent: AgentKind, on: bool) {
    match agent {
        AgentKind::Claude => RAIL_CLAUDE.store(on, Ordering::Relaxed),
        AgentKind::Droid => RAIL_DROID.store(on, Ordering::Relaxed),
        AgentKind::OpenCode => RAIL_OPENCODE.store(on, Ordering::Relaxed),
        AgentKind::Codex | AgentKind::WorkersAi => {}
    }
}

/// The usage card at the foot of the rail: each shown provider under its own
/// name, its windows as short bars. Clicking opens the Usage tab; right-click
/// or the ⋯ button picks which providers it shows.
/// Time left until a window resets, short enough for the rail: "42m", "2h 14m", "3d 4h".
fn reset_in(at: u64, now: u64) -> String {
    let left = at.saturating_sub(now).div_ceil(60) * 60;
    if left < 3600 {
        format!("{}m", left / 60)
    } else if left < 86_400 {
        format!("{}h {:02}m", left / 3600, left % 3600 / 60)
    } else {
        format!("{}d {}h", left / 86_400, left % 86_400 / 3600)
    }
}

pub fn rail_card(
    account: &Account,
    seats: Option<&Seats>,
    go: Option<&PlanUsage>,
    on_click: impl Fn(&gpui::ClickEvent, &mut Window, &mut App) + 'static,
    on_options: impl Fn(gpui::Point<gpui::Pixels>, &mut Window, &mut App) + 'static,
) -> Option<AnyElement> {
    let plan = seats.and_then(Seats::active);
    if !account.ready() && plan.is_none() && go.is_none() && seats.is_none() {
        return None;
    }
    let now = account::unix_now();
    let on_options = std::rc::Rc::new(on_options);
    let right_click = on_options.clone();
    let base = div()
        .id("rail-usage")
        .group("rail-usage")
        .w_full()
        .rounded(crate::ui::rad(ui::RADIUS))
        .cursor_pointer()
        .on_click(on_click)
        .on_mouse_down(gpui::MouseButton::Right, move |ev, window, cx| {
            cx.stop_propagation();
            right_click(ev.position, window, cx);
        });
    let show_claude = rail_shows(AgentKind::Claude);
    let show_droid = rail_shows(AgentKind::Droid);
    let show_go = rail_shows(AgentKind::OpenCode);
    #[cfg(feature = "coderlabs")]
    let (own, borrowed) = (
        seats.and_then(|s| s.own.as_ref()),
        seats.and_then(|s| s.borrowed.as_ref()),
    );
    // One seat: the one Claude Code runs on.
    #[cfg(not(feature = "coderlabs"))]
    let (own, borrowed) = (plan, None::<&PlanUsage>);
    // Three providers under their own names make the pinned footer tall:
    // then each provider's icon leads its first row instead, and a name only
    // shows when it says something (Droid's extra usage, Go's status, the
    // borrowed seat's own name).
    let compact = [
        show_claude && (own.is_some() || borrowed.is_some()),
        show_droid,
        show_go && go.is_some(),
    ]
    .iter()
    .filter(|b| **b)
    .count()
        >= 3;

    let options = div()
        .id("rail-usage-options")
        .flex()
        .items_center()
        .justify_center()
        .size(px(18.))
        .rounded(crate::ui::rad(ui::RADIUS_SM))
        .invisible()
        .group_hover("rail-usage", |s| s.visible())
        .hover(|s| s.bg(theme::fill_selected()))
        .child(icon("more-horizontal", 12.).text_color(theme::text_subtle()))
        .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
        .on_click(move |ev, window, cx| {
            cx.stop_propagation();
            on_options(ev.position(), window, cx);
        });
    let header = div()
        .flex()
        .items_center()
        .gap(crate::ui::sp(8.))
        .h(px(22.))
        .text_size(px(ui::TEXT_SM))
        .font_weight(FontWeight::MEDIUM)
        .text_color(theme::text_subtle())
        .child(icon("gauge", 14.).text_color(theme::text_faint()))
        .child(div().flex_1().truncate().child("Usage"))
        .when(account.busy() && show_droid, |d| {
            d.child(ui::spinner("rail-usage-spin", 10., theme::text_faint()))
        })
        .when(
            account.error().is_some() && !account.busy() && show_droid,
            |d| d.child(icon("alert-circle", 12.).text_color(theme::warn())),
        )
        .child(options);

    let lead = move |agent: AgentKind, first: bool| {
        compact.then(|| {
            div()
                .w(px(10.))
                .flex_none()
                .flex()
                .items_center()
                .when(first, |d| d.child(agent.icon(9.)))
        })
    };
    let row =
        |agent: AgentKind, i: usize, label: String, pct: f32, dim: bool, resets: Option<u64>| {
            div()
                .id((agent.id(), i))
                .flex()
                .items_center()
                .gap(crate::ui::sp(6.))
                .h(px(14.))
                .text_size(px(ui::TEXT_XS))
                .text_color(if dim {
                    theme::text_faint()
                } else {
                    theme::text_subtle()
                })
                .when(compact && i == 0, |d| d.mt(px(3.)))
                .child(
                    div()
                        .w(px(if compact { 56. } else { 46. }))
                        .flex_none()
                        .flex()
                        .items_center()
                        .gap(crate::ui::sp(4.))
                        .children(lead(agent, i == 0))
                        .child(div().min_w_0().truncate().child(label)),
                )
                .child(bar(pct, 4.))
                .child(
                    div()
                        .w(px(30.))
                        .flex()
                        .justify_end()
                        .font_family(theme::FONT_MONO)
                        .text_color(if !dim && pct >= 70. {
                            bar_color(pct)
                        } else {
                            theme::text_muted()
                        })
                        .child(format!("{:.0}%", pct.ceil())),
                )
                .child(
                    div()
                        .w(px(44.))
                        .flex_none()
                        .ml(px(2.))
                        .flex()
                        .justify_end()
                        .font_family(theme::FONT_MONO)
                        .text_color(theme::text_faint())
                        .child(resets.map(|at| reset_in(at, now)).unwrap_or_default()),
                )
        };
    // A provider's name over its rows.
    let provider = |agent: AgentKind, name: String| {
        div()
            .flex()
            .items_center()
            .gap(crate::ui::sp(5.))
            .h(px(16.))
            .mt(px(3.))
            .text_size(px(ui::TEXT_XS))
            .font_weight(FontWeight::MEDIUM)
            .text_color(theme::text_muted())
            .child(agent.icon(10.))
            .child(div().truncate().child(name))
    };
    let note = |agent: AgentKind, text: String| {
        div()
            .flex()
            .gap(crate::ui::sp(4.))
            .when(compact, |d| d.mt(px(3.)))
            .children(lead(agent, true).map(|d| d.h(px(14.))))
            .text_size(px(ui::TEXT_XS))
            .line_height(px(14.))
            .text_color(theme::text_faint())
            .child(div().flex_1().min_w_0().child(text))
    };

    let mut card = base
        .hover(|s| s.bg(theme::fill_hover()))
        .flex()
        .flex_col()
        .gap(crate::ui::sp(3.))
        .px(crate::ui::sp(8.))
        .pt(crate::ui::sp(4.))
        .pb(crate::ui::sp(6.))
        .child(header);
    if show_claude {
        // The machine's own sign-in, then a borrowed seat under its own name:
        // two accounts, each with its own 5-hour and weekly windows.
        if let Some(plan) = own {
            if !compact {
                card = card.child(provider(AgentKind::Claude, "Claude Code".into()));
            }
            for (i, w) in plan.windows.iter().take(3).enumerate() {
                card = card.child(row(
                    AgentKind::Claude,
                    i,
                    w.short_label(),
                    w.percent as f32,
                    !w.active,
                    w.resets_at,
                ));
            }
        }
        #[cfg(feature = "coderlabs")]
        if let Some(plan) = borrowed {
            let in_use = crate::agent::plan::active_is_borrowed();
            let name = if in_use {
                "Claude Code · borrowed, in use"
            } else {
                "Claude Code · borrowed"
            };
            card = card.child(provider(AgentKind::Claude, name.into()));
            for (i, w) in plan.windows.iter().take(3).enumerate() {
                card = card.child(row(
                    AgentKind::Claude,
                    i + 10,
                    w.short_label(),
                    w.percent as f32,
                    !w.active,
                    w.resets_at,
                ));
            }
        }
    }
    if show_droid {
        if account.connected() {
            let mode = account.limits().map(|l| l.mode(now));
            let name = match mode {
                Some(UsageMode::DroidCore) => "Droid · Core",
                Some(UsageMode::ExtraUsage) => "Droid · Extra",
                Some(UsageMode::Blocked) => "Droid · limit reached",
                _ => "Droid",
            };
            if !compact || matches!(mode, Some(UsageMode::ExtraUsage | UsageMode::Blocked)) {
                card = card.child(provider(AgentKind::Droid, name.into()));
            }
            let pool = account.limits().and_then(|l| match l.mode(now) {
                UsageMode::DroidCore => l.core.or(l.standard),
                _ => l.standard,
            });
            if let Some(pool) = pool {
                for (i, (label, b)) in pool.buckets().into_iter().enumerate() {
                    card = card.child(row(
                        AgentKind::Droid,
                        i,
                        label.to_string(),
                        b.current(now),
                        false,
                        b.window_end.filter(|_| b.active(now)),
                    ));
                }
            } else if let Some(e) = account.error() {
                card = card.child(note(AgentKind::Droid, e.to_string()));
            }
        } else {
            card = card.child(note(AgentKind::Droid, "Droid — connect API key".into()));
        }
    }
    if show_go && let Some(go) = go {
        // A status other than "ok" is the endpoint's own word; say it as is.
        let status = go
            .windows
            .iter()
            .find_map(crate::agent::opencode_plan::status);
        if !compact || status.is_some() {
            let name =
                status.map_or_else(|| "OpenCode Go".into(), |s| format!("OpenCode Go · {s}"));
            card = card.child(provider(AgentKind::OpenCode, name));
        }
        for (i, w) in go.windows.iter().enumerate() {
            card = card.child(row(
                AgentKind::OpenCode,
                i,
                w.short_label(),
                w.percent as f32,
                false,
                w.resets_at,
            ));
        }
    }
    if !show_claude && !show_droid && !show_go {
        card = card.child(
            div()
                .text_size(px(ui::TEXT_XS))
                .line_height(px(14.))
                .text_color(theme::text_faint())
                .child("All providers hidden here"),
        );
    }
    Some(card.into_any_element())
}
