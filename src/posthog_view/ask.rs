//! The Ask tab: a question about the project's numbers goes to PostHog AI,
//! only when it is sent, and comes back as an answer and the SQL behind it.
//! Answers can be pinned per project; a pin is a saved copy, never asked again.

use super::*;
use crate::posthog::ask::{self, Answer};

pub(super) struct Ask {
    target: Option<Target>,
    question: Entity<TextInput>,
    answer: Option<Answer>,
    pins: Vec<Answer>,
    history: Vec<Answer>,
    error: Option<String>,
    loading: bool,
    task: Option<Task<()>>,
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
            answer: None,
            pins: vec![],
            history: vec![],
            error: None,
            loading: false,
            task: None,
            _input: input,
        }
    }
    pub(super) fn set_target(&mut self, t: Option<Target>, cx: &mut Context<Self>) {
        if self.target == t {
            return;
        }
        self.task = None;
        self.loading = false;
        self.answer = None;
        self.history.clear();
        self.error = None;
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
        if self.loading || question.is_empty() {
            return;
        }
        self.error = None;
        self.answer = None;
        self.loading = true;
        cx.notify();
        self.task = Some(cx.spawn(async move |this, cx| {
            let project = t.project.clone();
            let res = cx
                .background_spawn(crate::guard::task(async move {
                    ask::ask(&t.session, t.project.id, &question)
                }))
                .await;
            this.update(cx, |this, cx| {
                if this.target.as_ref().map(|t| &t.project) != Some(&project) {
                    return;
                }
                this.loading = false;
                match res {
                    Ok(a) => {
                        this.history.retain(|old| old.question != a.question);
                        this.history.insert(0, a.clone());
                        this.history.truncate(8);
                        this.answer = Some(a);
                    }
                    Err(e) => this.error = Some(e.message()),
                }
                cx.notify();
            })
            .ok();
        }));
    }
    fn toggle_pin(&mut self, cx: &mut Context<Self>) {
        let (Some(a), Some(t)) = (&self.answer, &self.target) else {
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
                self.error = None;
            }
            Err(e) => self.error = Some(format!("Could not save the pinned answers: {e}")),
        }
        cx.notify();
    }
}
impl Render for Ask {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let enabled = !self.loading && !self.question.read(cx).text().trim().is_empty();
        let field = ui::field()
            .flex_1()
            .child(div().flex_1().child(self.question.clone()));
        let submit = ui::button(
            "ph-ask-run",
            if self.loading { "Asking…" } else { "Ask" },
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
        if self.loading {
            main = main.child(quiet("PostHog AI is writing and running a query…"));
        }
        if let Some(e) = &self.error {
            main = main.child(notice("alert-circle", Tone::Warning, e.clone()));
        }
        if let Some(a) = &self.answer {
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
                        !self.loading,
                    )
                    .when(!self.loading, |b| {
                        b.on_click(cx.listener(move |this, _, _, cx| {
                            this.error = None;
                            this.answer = Some(a.clone());
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
