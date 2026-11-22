//! The corner card that offers a PostHog popover survey, and the clock that
//! decides when it may appear. `store::eligible` holds the rules; this side
//! only reports the moment accurately.

use std::collections::HashSet;
use std::time::{Duration, Instant};

use super::*;
use crate::surveys::model::Survey;
use crate::surveys::store::{self, Moment, Seen};

/// How often the clock looks for a survey to offer.
const CHECK_EVERY: Duration = Duration::from_secs(30);

/// A debug build can skip the waits and the consent check with
/// `ARAKUN_SURVEY_NOW=1`, to see the card (`arakun dev` never has consent).
fn debug_skip() -> bool {
    cfg!(debug_assertions) && std::env::var_os("ARAKUN_SURVEY_NOW").is_some()
}

/// What the card's clock knows.
pub(super) struct CardState {
    pub launched: Instant,
    pub last_typed: Instant,
    pub shown_this_launch: bool,
    /// The survey on screen.
    pub card: Option<Survey>,
}

impl CardState {
    pub fn new(now: Instant) -> Self {
        Self {
            launched: now,
            last_typed: now,
            shown_this_launch: false,
            card: None,
        }
    }

    /// The facts `store::eligible` judges.
    pub fn moment<'a>(
        &self,
        now: Instant,
        usage: bool,
        now_unix: u64,
        installed_for: Duration,
        events_seen: &'a HashSet<String>,
    ) -> Moment<'a> {
        let skip = debug_skip();
        let long = Duration::from_secs(24 * 3600);
        Moment {
            usage,
            since_launch: if skip {
                long
            } else {
                now.saturating_duration_since(self.launched)
            },
            since_typing: if skip {
                long
            } else {
                now.saturating_duration_since(self.last_typed)
            },
            shown_this_launch: self.shown_this_launch,
            now_unix,
            installed_for,
            events_seen,
        }
    }
}

/// What closing the card with its X does.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum OnClose {
    Mark(Seen),
    SendDismissed,
}

pub(super) fn on_close() -> Vec<OnClose> {
    vec![OnClose::Mark(Seen::Dismissed), OnClose::SendDismissed]
}

impl Shell {
    /// Looks for a survey now and every 30 s, and fetches the list (which
    /// limits itself to once in six hours).
    pub fn start_survey_card(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        cx.background_spawn(crate::guard::task(async { store::refresh() }))
            .detach();
        cx.spawn_in(window, async move |this, cx| {
            loop {
                cx.background_executor().timer(CHECK_EVERY).await;
                cx.background_spawn(crate::guard::task(async { store::refresh() }))
                    .detach();
                let alive = this
                    .update_in(cx, |this, window, cx| this.offer_survey(window, cx))
                    .is_ok();
                if !alive {
                    break;
                }
            }
        })
        .detach();
    }

    /// The window's own: another dialog, the setup or a card already up
    /// holds the person's attention.
    fn offer_survey(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        if self.modal.is_some() || self.onboarding.is_some() || self.survey.card.is_some() {
            return;
        }
        let surveys = store::cached();
        if surveys.is_empty() {
            return;
        }
        let events = crate::telemetry::seen_events();
        let moment = self.survey.moment(
            Instant::now(),
            crate::telemetry::effective().usage || debug_skip(),
            store::now_unix(),
            store::installed_for(),
            &events,
        );
        let Some(survey) = store::eligible(&surveys, &store::seen(), &moment).cloned() else {
            return;
        };
        self.survey.shown_this_launch = true;
        store::mark(&survey.id, Seen::Shown);
        crate::surveys::send::shown(&survey);
        self.survey.card = Some(survey);
        cx.notify();
    }

    fn close_survey_card(&mut self, cx: &mut Context<Self>) {
        let Some(survey) = self.survey.card.take() else {
            return;
        };
        for act in on_close() {
            match act {
                OnClose::Mark(s) => store::mark(&survey.id, s),
                OnClose::SendDismissed => crate::surveys::send::dismissed(&survey),
            }
        }
        cx.notify();
    }

    fn answer_survey_card(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        // Opening one closes the other modal, so wait for it to be done.
        if self.modal.is_some() || self.onboarding.is_some() {
            return;
        }
        if let Some(survey) = self.survey.card.take() {
            self.open_survey(
                survey,
                crate::telemetry::Source::Popover,
                vec![],
                window,
                cx,
            );
        }
    }

    /// The card, for the bottom-right corner above the dock. `None` while
    /// there is nothing to offer or a dialog or the setup is up.
    pub(super) fn render_survey_card(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        if self.modal.is_some() || self.onboarding.is_some() {
            return None;
        }
        let survey = self.survey.card.as_ref()?;
        let text = survey.questions.first().map_or("", |q| q.text.as_str());
        Some(
            div()
                .absolute()
                .right(ui::sp(16.))
                .bottom(px(ui::DOCK_BAR_H) + ui::sp(16.))
                .w(px(320.))
                // The card's fill is see-through; what is behind must not show.
                .bg(theme::window_bg())
                .rounded(ui::rad(ui::RADIUS_XL))
                .child(
                    ui::surface_card()
                        .gap(ui::sp(12.))
                        .p(ui::sp(16.))
                        .child(
                            div()
                                .flex()
                                .items_start()
                                .gap(ui::sp(8.))
                                .child(
                                    div()
                                        .flex_1()
                                        .min_w_0()
                                        .text_size(px(ui::TEXT_MD))
                                        .text_color(theme::content())
                                        .child(text.to_string()),
                                )
                                .child(ui::close_button("survey-card-close").on_click(
                                    cx.listener(|this, _, _, cx| this.close_survey_card(cx)),
                                )),
                        )
                        .child(
                            div().flex().justify_end().child(
                                ui::button(
                                    "survey-card-answer",
                                    "Answer",
                                    ui::ButtonKind::Primary,
                                    true,
                                )
                                .on_click(cx.listener(
                                    |this, _, window, cx| this.answer_survey_card(window, cx),
                                )),
                            ),
                        ),
                )
                .into_any_element(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::surveys::model::{Conditions, SurveyType};

    #[test]
    fn moment_from_shell_state() {
        let base = Instant::now();
        let mut state = CardState::new(base);
        state.last_typed = base + Duration::from_secs(100);
        state.shown_this_launch = true;
        let events: HashSet<String> = ["a".to_string()].into();
        let m = state.moment(
            base + Duration::from_secs(130),
            true,
            42,
            Duration::from_secs(9),
            &events,
        );
        // Debug builds may skip the waits; the test needs the real ones.
        if std::env::var_os("ARAKUN_SURVEY_NOW").is_none() {
            assert_eq!(m.since_launch, Duration::from_secs(130));
            assert_eq!(m.since_typing, Duration::from_secs(30));
        }
        assert!(m.usage && m.shown_this_launch);
        assert_eq!((m.now_unix, m.installed_for), (42, Duration::from_secs(9)));
        assert!(m.events_seen.contains("a"));
    }

    #[test]
    fn a_fresh_launch_is_not_yet_time() {
        let base = Instant::now();
        let state = CardState::new(base);
        let events = HashSet::new();
        let m = state.moment(base, true, 0, Duration::ZERO, &events);
        let s = Survey {
            id: "x".into(),
            name: "x".into(),
            kind: SurveyType::Popover,
            questions: vec![],
            start_date: None,
            end_date: None,
            archived: false,
            conditions: Conditions::default(),
        };
        assert!(store::eligible(&[s], &Default::default(), &m).is_none());
    }

    #[test]
    fn close_marks_dismissed() {
        assert_eq!(
            on_close(),
            vec![OnClose::Mark(Seen::Dismissed), OnClose::SendDismissed]
        );
    }
}
