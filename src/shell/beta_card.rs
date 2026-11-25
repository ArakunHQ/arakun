//! The card at the top of the rail that invites to the phone app beta. It
//! signs up the same way as the Phone tab, so each sign-up lands in the
//! operator's queue in the admin dashboard. The operator adds testers by
//! email in TestFlight and Play Console, so the card asks for the address.
//! Closing it is kept in `phone.json`. Only accounts the operator gave the
//! `mobile_beta` flag in the admin dashboard see it.

use super::*;
use crate::cloud::BetaPlatform;
use crate::phone_view::{BetaState, BetaStatus};

/// The account flag that shows the card. The phone page reads the same one.
const BETA_FLAG: &str = "mobile_beta";

/// The card's own email field, filled with the account's email once.
pub(super) struct BetaCard {
    closed: bool,
    email: Entity<TextInput>,
    /// The account's email was put in once; after that it is the user's.
    filled: bool,
    _email_sub: Subscription,
}

impl BetaCard {
    pub(super) fn new(cx: &mut Context<Shell>) -> Self {
        let email = cx.new(|cx| TextInput::new("Email for the invite", cx).redacted());
        let sub = cx.subscribe(&email, |this: &mut Shell, _, ev: &InputEvent, _| {
            if let InputEvent::Changed = ev {
                this.beta_card.filled = true;
            }
        });
        Self {
            closed: crate::phone::Settings::load().beta_card_closed,
            email,
            filled: false,
            _email_sub: sub,
        }
    }
}

/// What the card shows, if anything.
#[derive(Debug, PartialEq, Eq)]
enum Shape {
    /// The email field and a button per platform.
    Form {
        busy: Option<BetaPlatform>,
        error: Option<String>,
    },
    /// Signed up or admitted: what comes next, and only a way to close.
    Told(String),
}

fn store(platform: BetaPlatform) -> &'static str {
    match platform {
        BetaPlatform::Ios => "TestFlight",
        BetaPlatform::Android => "Google Play",
    }
}

/// Whether one platform may be signed up for now: never, or declined long
/// enough ago.
fn open(b: &BetaStatus, now: u64) -> bool {
    match b.status.as_str() {
        "none" => true,
        "declined" => b.retry_at.is_none_or(|t| t <= now),
        _ => false,
    }
}

fn shape(beta: &BetaState, now: u64) -> Option<Shape> {
    let BetaState::Ready { ios, android } = beta else {
        return None;
    };
    let both = [(BetaPlatform::Ios, ios), (BetaPlatform::Android, android)];
    let email = |b: &BetaStatus| {
        b.email
            .as_deref()
            .map(crate::redact::value)
            .unwrap_or_default()
    };
    if let Some((p, b)) = both.iter().find(|(_, b)| b.status == "admitted") {
        return Some(Shape::Told(format!(
            "You are in. Look for the {} invite at {}.",
            store(*p),
            email(b)
        )));
    }
    if let Some((p, b)) = both.iter().find(|(_, b)| b.status == "pending") {
        return Some(Shape::Told(format!(
            "Signed up. We add testers by hand; the {} invite will go to {}.",
            store(*p),
            email(b)
        )));
    }
    if !both.iter().any(|(_, b)| open(b, now)) {
        return None;
    }
    Some(Shape::Form {
        busy: both.iter().find(|(_, b)| b.busy).map(|(p, _)| *p),
        error: both.iter().find_map(|(_, b)| b.error.clone()),
    })
}

impl Shell {
    /// Closes the card for good.
    fn close_beta_card(&mut self, cx: &mut Context<Self>) {
        self.beta_card.closed = true;
        let mut settings = crate::phone::Settings::load();
        settings.beta_card_closed = true;
        settings.save_soon();
        cx.notify();
    }

    /// Puts the account's email in the card's field, once the service said it.
    pub(super) fn fill_beta_card_email(&mut self, cx: &mut Context<Self>) {
        if self.beta_card.filled {
            return;
        }
        if let (_, Some(email)) = self.beta_state(cx) {
            self.beta_card
                .email
                .update(cx, |i, cx| i.set_text(email, cx));
            self.beta_card.filled = true;
        }
    }

    pub(super) fn render_beta_card(&self, cx: &mut Context<Self>) -> Option<gpui::Div> {
        let flagged = self.teams.read(cx).me().is_some_and(|m| m.has(BETA_FLAG));
        if self.beta_card.closed || !flagged {
            return None;
        }
        let (beta, _) = self.beta_state(cx);
        let shape = shape(&beta, crate::model::unix_now())?;
        let close = ui::icon_button("beta-card-close", "cancel01", 12., "Not now")
            .on_click(cx.listener(|this, _, _, cx| this.close_beta_card(cx)));
        let text = |t: String| {
            div()
                .text_size(px(ui::TEXT_SM))
                .text_color(theme::text_subtle())
                .child(t)
        };
        let body = match shape {
            Shape::Form { busy, error } => {
                let join = |platform: BetaPlatform, label: &'static str| {
                    let input = self.beta_card.email.clone();
                    let label = if busy == Some(platform) {
                        "Signing up…"
                    } else {
                        label
                    };
                    ui::button(
                        match platform {
                            BetaPlatform::Ios => "beta-card-ios",
                            BetaPlatform::Android => "beta-card-android",
                        },
                        label,
                        ui::ButtonKind::Outline,
                        busy.is_none(),
                    )
                    .flex_1()
                    .justify_center()
                    .when(busy.is_none(), |b| {
                        b.on_click(cx.listener(move |this, _, window, cx| {
                            let email = input.read(cx).text().trim().to_string();
                            this.join_beta(platform, email, window, cx);
                        }))
                    })
                };
                div()
                    .flex()
                    .flex_col()
                    .gap(crate::ui::sp(8.))
                    .child(text(
                        "The iOS and Android apps are in beta. The invite goes to this email: your Apple ID or Google account.".into(),
                    ))
                    .child(ui::field().child(self.beta_card.email.clone()))
                    .child(
                        div()
                            .flex()
                            .gap(crate::ui::sp(6.))
                            .child(join(BetaPlatform::Ios, "iOS"))
                            .child(join(BetaPlatform::Android, "Android")),
                    )
                    .when_some(error, |d, e| {
                        d.child(
                            div()
                                .text_size(px(ui::TEXT_XS))
                                .text_color(theme::attention())
                                .child(crate::redact::text(&e).into_owned()),
                        )
                    })
            }
            Shape::Told(t) => div().child(text(t)),
        };
        Some(
            div()
                .flex()
                .flex_col()
                .flex_none()
                .gap(crate::ui::sp(8.))
                .mx(crate::ui::sp(8.))
                .mb(crate::ui::sp(8.))
                .p(crate::ui::sp(12.))
                .rounded(crate::ui::rad(10.))
                .border_1()
                .border_color(theme::stroke())
                .child(
                    div()
                        .flex()
                        .items_center()
                        .child(
                            div()
                                .flex_1()
                                .text_size(px(ui::TEXT_MD))
                                .font_weight(FontWeight::SEMIBOLD)
                                .text_color(theme::text())
                                .child("Arakun on your phone"),
                        )
                        .child(close),
                )
                .child(body),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn status(status: &str) -> BetaStatus {
        BetaStatus {
            status: status.into(),
            email: Some("me@icloud.com".into()),
            retry_at: None,
            busy: false,
            error: None,
        }
    }

    fn ready(ios: BetaStatus, android: BetaStatus) -> BetaState {
        BetaState::Ready { ios, android }
    }

    #[test]
    fn asks_for_the_email_until_signed_up() {
        let s = shape(&ready(status("none"), status("none")), 0);
        assert_eq!(
            s,
            Some(Shape::Form {
                busy: None,
                error: None
            })
        );
        let Some(Shape::Told(t)) = shape(&ready(status("none"), status("pending")), 0) else {
            panic!("a pending sign-up says so");
        };
        assert!(
            t.contains("Google Play") && t.contains("me@icloud.com"),
            "{t}"
        );
        let Some(Shape::Told(t)) = shape(&ready(status("admitted"), status("pending")), 0) else {
            panic!("an admitted sign-up says so");
        };
        assert!(
            t.starts_with("You are in") && t.contains("TestFlight"),
            "{t}"
        );
    }

    #[test]
    fn hides_while_declined_and_unknown() {
        let mut later = status("declined");
        later.retry_at = Some(100);
        assert_eq!(shape(&ready(later.clone(), later.clone()), 50), None);
        assert!(matches!(
            shape(&ready(later.clone(), later), 100),
            Some(Shape::Form { .. })
        ));
        assert_eq!(shape(&BetaState::Loading, 0), None);
        assert_eq!(shape(&BetaState::Failed("x".into()), 0), None);
        assert_eq!(shape(&BetaState::SignedOut, 0), None);
    }
}
