//! The Ask tab: a question about the project's numbers goes to PostHog AI,
//! only when it is sent, and comes back as an answer and the SQL behind it.
//! Answers can be pinned per project; a pin is a saved copy, never asked again.

use super::*;
use crate::posthog::ask::{self, Answer};

pub(super) struct Ask {
    target: Option<Target>,
    question: Entity<TextInput>,
    /// The answer on screen. Its error also carries a failed pin save.
    answer: Loadable<Answer>,
    pins: Vec<Answer>,
    history: Vec<Answer>,
    _input: Subscription,
}
impl Ask {
    pub(super) fn new(cx: &mut Context<Self>) -> Self {
        let question = cx.new(|cx| TextInput::new("Ask about this project's metrics", cx));
        let input = cx.subscribe(
            &question,
            |this: &mut Self, _, ev: &InputEvent, cx| match ev {
                InputEvent::Submit => this.run(cx),
                InputEvent::Changed => cx.notify(),
                _ => {}
            },
        );
        Self {
            target: None,
            question,
            answer: Loadable::default(),
            pins: vec![],
            history: vec![],
            _input: input,
        }
    }
    pub(super) fn set_target(&mut self, t: Option<Target>, cx: &mut Context<Self>) {
        if self.target == t {
            return;
        }
        self.answer.reset();
        self.history.clear();
        self.question.update(cx, |i, cx| i.clear(cx));
        self.pins = t
            .as_ref()
            .map(|t| ask::pins(&t.project))
            .unwrap_or_default();
        self.target = t;
        cx.notify();
    }
    fn run(&mut self, cx: &mut Context<Self>) {
        let Some(t) = self.target.clone() else { return };
        let question = self.question.read(cx).text().trim().to_string();
        // Checked before the answer is cleared: start alone would skip the
        // read but still blank the answer on screen.
        if self.answer.loading() || question.is_empty() {
            return;
        }
        self.answer.error = None;
        self.answer.data = None;
        let project = t.project.clone();
        self.answer.start(
            cx,
            |this| &mut this.answer,
            async move { ask::ask(&t.session, t.project.id, &question) },
            move |this, res, _| {
                if this.target.as_ref().map(|t| &t.project) != Some(&project) {
                    return;
                }
                if let Ok(a) = &res {
                    this.history.retain(|old| old.question != a.question);
                    this.history.insert(0, a.clone());
                    this.history.truncate(8);
                }
                this.answer.settle(res.map_err(|e| e.message()));
            },
        );
    }
    fn toggle_pin(&mut self, cx: &mut Context<Self>) {
        let (Some(a), Some(t)) = (&self.answer.data, &self.target) else {
            return;
        };
        let mut pins = self.pins.clone();
        if let Some(i) = pins.iter().position(|p| p.question == a.question) {
            pins.remove(i);
        } else if pins.len() < 16 {
            pins.insert(0, a.clone());
        }
        match ask::save_pins(&t.session, t.project.id, &pins) {
            Ok(()) => {
                self.pins = pins;
                self.answer.error = None;
            }
            Err(e) => self.answer.error = Some(format!("Could not save the pinned answers: {e}")),
        }
        cx.notify();
    }
}
impl Render for Ask {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let loading = self.answer.loading();
        let enabled = !loading && !self.question.read(cx).text().trim().is_empty();
        let field = ui::field()
            .flex_1()
            .child(div().flex_1().child(self.question.clone()));
        let submit = ui::button(
            "ph-ask-run",
            if loading { "Asking…" } else { "Ask" },
            ButtonKind::Primary,
            enabled,
        )
        .when(enabled, |b| {
            b.on_click(cx.listener(|this, _, _, cx| this.run(cx)))
        });
        let mut main = div()
            .flex()
            .flex_col()
            .flex_1()
            .min_w(px(280.))
            .gap(ui::sp(12.))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(ui::sp(8.))
                    .child(field)
                    .child(submit),
            )
            .child(quiet(
                "The question goes to PostHog AI in this project, not to a coding agent. \
                 It counts against the hourly query budget.",
            ));
        if loading {
            main = main.child(quiet("PostHog AI is writing and running a query…"));
        }
        if let Some(e) = &self.answer.error {
            main = main.child(notice("alert-circle", Tone::Warning, e.clone()));
        }
        if let Some(a) = &self.answer.data {
            let pinned = self.pins.iter().any(|p| p.question == a.question);
            let can_pin = pinned || self.pins.len() < 16;
            main = main
                .child(ui::heading(crate::redact::text(&a.question).into_owned()))
                .child(quiet(crate::redact::text(&a.text).into_owned()));
            if !a.sql.is_empty() {
                main = main
                    .child(ui::heading("HogQL from PostHog"))
                    .child(ui::text(
                        Type::Mono,
                        theme::text_muted(),
                        crate::redact::text(&a.sql).into_owned(),
                    ));
            }
            main = main.child(
                div().flex().child(
                    ui::button(
                        "ph-ask-pin",
                        if pinned { "Unpin answer" } else { "Pin answer" },
                        ButtonKind::Outline,
                        can_pin,
                    )
                    .when(can_pin, |b| {
                        b.on_click(cx.listener(|this, _, _, cx| this.toggle_pin(cx)))
                    }),
                ),
            );
        }
        let mut side = div().flex().flex_col().w(px(240.)).gap(ui::sp(8.));
        for (id, label, answers) in [
            ("ph-pin", "Pinned", &self.pins),
            ("ph-history", "Asked before", &self.history),
        ] {
            side = side.child(ui::heading(label));
            if answers.is_empty() {
                side = side.child(quiet("None yet."));
            }
            for (i, a) in answers.iter().enumerate() {
                let a = a.clone();
                side = side.child(
                    ui::button(
                        (id, i),
                        crate::redact::text(&a.question).into_owned(),
                        ButtonKind::Ghost,
                        !loading,
                    )
                    .when(!loading, |b| {
                        b.on_click(cx.listener(move |this, _, _, cx| {
                            this.answer.error = None;
                            this.answer.data = Some(a.clone());
                            this.question
                                .update(cx, |i, cx| i.set_text(&a.question, cx));
                            cx.notify();
                        }))
                    }),
                );
            }
        }
        div()
            .flex()
            .flex_wrap()
            .gap(ui::sp(24.))
            .items_start()
            .child(main)
            .child(side)
    }
}
