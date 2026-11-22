//! A survey shown natively, one question at a time, with Back and Next.
//! `Flow` walks the questions and holds the answers; the dialog only draws
//! it, so the walk is tested without a window.

use std::collections::HashMap;

use gpui::{
    App, AppContext as _, Context, Entity, EventEmitter, FocusHandle, Focusable,
    InteractiveElement as _, IntoElement, ParentElement as _, Render,
    StatefulInteractiveElement as _, Styled as _, Subscription, Window, div,
    prelude::FluentBuilder as _,
};
use serde_json::Value;

use crate::input::{InputEvent, TextInput};
use crate::surveys::model::{Answer, Body, Question, Survey, next};
use crate::surveys::send;
use crate::telemetry::{Refused, Source};
use crate::theme;
use crate::ui::{self, ButtonKind, Type};

/// Shown before Send on forms a person opened to report something.
pub const PUBLIC: &str = "Bug reports may become a public GitHub issue.";
pub const THANKS: &str = "Thanks. Sent to the Arakun team.";
/// What PostHog records when a link question's button is clicked.
const LINK_CLICKED: &str = "link clicked";
/// Rating labels: a 10-point scale runs from 0 (NPS), the others from 1.
const NUMBERS: [&str; 11] = ["0", "1", "2", "3", "4", "5", "6", "7", "8", "9", "10"];

pub enum SurveyEvent {
    /// Sent; the dialog now thanks the person.
    Sent,
    /// Closed, sent or not (`SurveyDialog::is_sent`).
    Dismissed,
    /// Not sent, for this reason. The dialog shows it and keeps the answers.
    Failed(String),
    /// A link question's button.
    OpenUrl(String),
}

/// What Next did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    /// A required question has no answer yet.
    Blocked,
    Moved,
    /// That was the last question: send it.
    Submit,
}

/// Where a survey is, and what has been answered.
pub struct Flow {
    survey: Survey,
    src: Source,
    at: usize,
    /// The answer to each question, kept when Back leaves it.
    answers: Vec<Option<Answer>>,
    /// Text typed into a choice question's open choice.
    other: Vec<String>,
    /// The questions before this one, for Back.
    history: Vec<usize>,
    error: Option<String>,
    sent: bool,
}

impl Flow {
    pub fn new(survey: Survey, src: Source) -> Self {
        let n = survey.questions.len();
        Self {
            survey,
            src,
            at: 0,
            answers: vec![None; n],
            other: vec![String::new(); n],
            history: Vec::new(),
            error: None,
            sent: false,
        }
    }

    pub fn survey(&self) -> &Survey {
        &self.survey
    }

    pub fn src(&self) -> Source {
        self.src
    }

    pub fn at(&self) -> usize {
        self.at
    }

    pub fn question(&self) -> Option<&Question> {
        self.survey.questions.get(self.at)
    }

    /// The current question's answer, as picked or typed.
    pub fn answer(&self) -> Option<&Answer> {
        self.answers.get(self.at)?.as_ref()
    }

    pub fn set(&mut self, answer: Answer) {
        self.set_at(self.at, answer);
    }

    pub fn set_at(&mut self, i: usize, answer: Answer) {
        if let Some(slot) = self.answers.get_mut(i) {
            *slot = Some(answer);
        }
    }

    pub fn other(&self, i: usize) -> &str {
        self.other.get(i).map_or("", String::as_str)
    }

    pub fn set_other(&mut self, i: usize, text: String) {
        if let Some(slot) = self.other.get_mut(i) {
            *slot = text;
        }
    }

    /// Picks a single choice.
    pub fn pick(&mut self, choice: &str) {
        self.set(Answer::Choice(choice.into()));
    }

    /// Turns a multiple choice on or off, keeping the survey's order.
    pub fn toggle(&mut self, choice: &str) {
        let Some(Body::Multi { choices, .. }) = self.question().map(|q| &q.body) else {
            return;
        };
        let mut on: Vec<String> = match self.answer() {
            Some(Answer::Choices(c)) => c.clone(),
            _ => Vec::new(),
        };
        if let Some(i) = on.iter().position(|c| c == choice) {
            on.remove(i);
        } else {
            on.push(choice.into());
        }
        let ordered = choices.iter().filter(|c| on.contains(c)).cloned().collect();
        self.set(Answer::Choices(ordered));
    }

    /// The answer to question `i` as it is sent: the open choice becomes
    /// what was typed into it, and an empty answer is none.
    fn resolved(&self, i: usize) -> Option<Answer> {
        let q = self.survey.questions.get(i)?;
        let a = self.answers.get(i)?.clone()?;
        let typed = self.other(i).trim();
        let open_label = match &q.body {
            Body::Single {
                choices,
                open_choice: true,
            }
            | Body::Multi {
                choices,
                open_choice: true,
            } => choices.last(),
            _ => None,
        };
        let a = match (a, open_label) {
            (Answer::Choice(c), Some(l)) if c == *l && !typed.is_empty() => {
                Answer::Choice(typed.into())
            }
            (Answer::Choices(cs), Some(l)) if !typed.is_empty() => Answer::Choices(
                cs.into_iter()
                    .map(|c| if c == *l { typed.into() } else { c })
                    .collect(),
            ),
            (a, _) => a,
        };
        match &a {
            Answer::Text(t) if t.trim().is_empty() => None,
            Answer::Choices(c) if c.is_empty() => None,
            Answer::Skipped => None,
            _ => Some(a),
        }
    }

    /// Whether Next may leave the current question.
    pub fn can_advance(&self) -> bool {
        let Some(q) = self.question() else {
            return false;
        };
        q.optional
            || matches!(q.body, Body::Link { .. } | Body::Unsupported)
            || self.resolved(self.at).is_some()
    }

    fn next_index(&self) -> Option<usize> {
        let a = self.resolved(self.at).unwrap_or(Answer::Skipped);
        next(&self.survey, self.at, &a)
    }

    /// Whether Next sends, given the answer so far.
    pub fn is_last(&self) -> bool {
        self.next_index().is_none()
    }

    pub fn advance(&mut self) -> Step {
        if self.sent || !self.can_advance() {
            return Step::Blocked;
        }
        self.error = None;
        match self.next_index() {
            Some(i) => {
                self.history.push(self.at);
                self.at = i;
                Step::Moved
            }
            None => Step::Submit,
        }
    }

    pub fn can_go_back(&self) -> bool {
        !self.sent && !self.history.is_empty()
    }

    pub fn back(&mut self) {
        if self.sent {
            return;
        }
        if let Some(i) = self.history.pop() {
            self.at = i;
            self.error = None;
        }
    }

    /// The answers on the path walked, so a branch left by Back is not sent.
    pub fn answers(&self) -> Vec<(usize, Answer)> {
        self.history
            .iter()
            .chain(std::iter::once(&self.at))
            .filter_map(|&i| Some((i, self.resolved(i)?)))
            .collect()
    }

    /// The notice before Send, on forms a person opened to report something.
    pub fn public_notice(&self) -> Option<&'static str> {
        (!self.sent && self.is_last() && matches!(self.src, Source::User | Source::Error))
            .then_some(PUBLIC)
    }

    pub fn refused(&mut self, why: Refused) {
        self.error = Some(why.to_string());
    }

    pub fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }

    pub fn sent(&mut self) {
        self.sent = true;
        self.error = None;
    }

    pub fn is_sent(&self) -> bool {
        self.sent
    }
}

pub struct SurveyDialog {
    flow: Flow,
    extra: Vec<(String, Value)>,
    focus: FocusHandle,
    /// A text field per question that takes text: open questions, and
    /// choice questions with an open choice. Kept for the whole survey, so
    /// Back and a refused Send never lose what was typed.
    inputs: HashMap<usize, Entity<TextInput>>,
    _subs: Vec<Subscription>,
}

impl EventEmitter<SurveyEvent> for SurveyDialog {}

impl SurveyDialog {
    pub fn new(
        survey: Survey,
        src: Source,
        extra: Vec<(String, Value)>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let focus = cx.focus_handle();
        let mut inputs = HashMap::new();
        let mut subs = Vec::new();
        for (i, q) in survey.questions.iter().enumerate() {
            let open = matches!(q.body, Body::Open);
            let other = matches!(
                q.body,
                Body::Single {
                    open_choice: true,
                    ..
                } | Body::Multi {
                    open_choice: true,
                    ..
                }
            );
            if !open && !other {
                continue;
            }
            let placeholder = if open { "Type here" } else { "Say more" };
            let input = cx.new(|cx| TextInput::new(placeholder, cx));
            subs.push(cx.subscribe_in(
                &input,
                window,
                move |this: &mut Self, input, ev, window, cx| match ev {
                    InputEvent::Changed => {
                        let text = input.read(cx).text().to_string();
                        if open {
                            this.flow.set_at(i, Answer::Text(text));
                        } else {
                            this.flow.set_other(i, text);
                        }
                        cx.notify();
                    }
                    InputEvent::Submit => this.go_next(window, cx),
                    InputEvent::Cancel => cx.emit(SurveyEvent::Dismissed),
                    _ => {}
                },
            ));
            inputs.insert(i, input);
        }
        let this = Self {
            flow: Flow::new(survey, src),
            extra,
            focus,
            inputs,
            _subs: subs,
        };
        this.focus_current(window, cx);
        this
    }

    pub fn is_sent(&self) -> bool {
        self.flow.is_sent()
    }

    pub fn survey(&self) -> &Survey {
        self.flow.survey()
    }

    /// The open question's field takes the keys; elsewhere the dialog does,
    /// so Escape still closes it.
    fn focus_current(&self, window: &mut Window, cx: &mut Context<Self>) {
        let input = self
            .flow
            .question()
            .filter(|q| matches!(q.body, Body::Open) && !self.flow.is_sent())
            .and_then(|_| self.inputs.get(&self.flow.at()));
        match input {
            Some(input) => window.focus(&input.focus_handle(cx), cx),
            None => window.focus(&self.focus, cx),
        }
    }

    fn go_next(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        match self.flow.advance() {
            Step::Blocked => {}
            Step::Moved => self.focus_current(window, cx),
            Step::Submit => {
                self.submit(cx);
                self.focus_current(window, cx);
            }
        }
        cx.notify();
    }

    fn go_back(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.flow.back();
        self.focus_current(window, cx);
        cx.notify();
    }

    fn submit(&mut self, cx: &mut Context<Self>) {
        let extra: Vec<(&str, Value)> = self
            .extra
            .iter()
            .map(|(k, v)| (k.as_str(), v.clone()))
            .collect();
        let flow = &self.flow;
        match send::submit(flow.survey(), &flow.answers(), flow.src(), &extra) {
            Ok(()) => {
                self.flow.sent();
                cx.emit(SurveyEvent::Sent);
            }
            Err(why) => {
                self.flow.refused(why);
                cx.emit(SurveyEvent::Failed(why.to_string()));
            }
        }
    }

    fn body(&self, q: &Question, cx: &mut Context<Self>) -> gpui::AnyElement {
        let at = self.flow.at();
        let answer = self.flow.answer();
        match &q.body {
            Body::Open => self.field(at),
            Body::Rating {
                scale,
                lower,
                upper,
                ..
            } => {
                let from = if *scale == 10 { 0 } else { 1 };
                let options: Vec<(u8, &'static str)> = (from..=*scale)
                    .filter_map(|n| Some((n, *NUMBERS.get(n as usize)?)))
                    .collect();
                let current = match answer {
                    Some(Answer::Rating(n)) => *n,
                    _ => u8::MAX,
                };
                div()
                    .flex()
                    .flex_col()
                    .gap(ui::sp(6.))
                    .child(ui::toggle_group(
                        "survey-rating",
                        &options,
                        current,
                        |seg, n| {
                            // Equal steps across the dialog, under the scale's labels.
                            seg.flex_1().justify_center().on_click(cx.listener(
                                move |this, _, _, cx| {
                                    this.flow.set(Answer::Rating(n));
                                    cx.notify();
                                },
                            ))
                        },
                    ))
                    .when(!lower.is_empty() || !upper.is_empty(), |d| {
                        d.child(
                            div()
                                .flex()
                                .justify_between()
                                .child(ui::text(Type::Caption, theme::text_subtle(), lower.clone()))
                                .child(ui::text(
                                    Type::Caption,
                                    theme::text_subtle(),
                                    upper.clone(),
                                )),
                        )
                    })
                    .into_any_element()
            }
            Body::Single {
                choices,
                open_choice,
            } => {
                let picked = match answer {
                    Some(Answer::Choice(c)) => Some(c.as_str()),
                    _ => None,
                };
                let open_on = *open_choice
                    && picked.is_some()
                    && picked == choices.last().map(String::as_str);
                div()
                    .flex()
                    .flex_col()
                    .gap(ui::sp(2.))
                    .children(choices.iter().enumerate().map(|(i, c)| {
                        let choice = c.clone();
                        ui::choice_row(("survey-choice", i), picked == Some(c.as_str()))
                            .child(c.clone())
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.flow.pick(&choice);
                                this.focus_other(window, cx);
                                cx.notify();
                            }))
                    }))
                    .when(open_on, |d| {
                        d.child(div().pt(ui::sp(6.)).child(self.field(at)))
                    })
                    .into_any_element()
            }
            Body::Multi {
                choices,
                open_choice,
            } => {
                let on: &[String] = match answer {
                    Some(Answer::Choices(c)) => c,
                    _ => &[],
                };
                let open_on = *open_choice && choices.last().is_some_and(|l| on.contains(l));
                div()
                    .flex()
                    .flex_col()
                    .gap(ui::sp(2.))
                    .children(choices.iter().enumerate().map(|(i, c)| {
                        let choice = c.clone();
                        ui::switch_row(("survey-multi", i), c, on.contains(c)).on_click(
                            cx.listener(move |this, _, window, cx| {
                                this.flow.toggle(&choice);
                                this.focus_other(window, cx);
                                cx.notify();
                            }),
                        )
                    }))
                    .when(open_on, |d| {
                        d.child(div().pt(ui::sp(6.)).child(self.field(at)))
                    })
                    .into_any_element()
            }
            Body::Link { url } => {
                let url = url.clone();
                let label = q.button_text.clone().unwrap_or_else(|| "Open link".into());
                div()
                    .flex()
                    .child(
                        ui::button("survey-link", label, ButtonKind::Outline, !url.is_empty())
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.flow.set(Answer::Text(LINK_CLICKED.into()));
                                cx.emit(SurveyEvent::OpenUrl(url.clone()));
                                cx.notify();
                            })),
                    )
                    .into_any_element()
            }
            Body::Unsupported => ui::text(
                Type::Body,
                theme::text_subtle(),
                "This version of Arakun can't show this question. Update Arakun to answer it.",
            )
            .into_any_element(),
        }
    }

    /// After a pick, the open choice's field takes the keys when it shows.
    fn focus_other(&self, window: &mut Window, cx: &mut Context<Self>) {
        let at = self.flow.at();
        let shows = match (self.flow.question().map(|q| &q.body), self.flow.answer()) {
            (Some(Body::Single { choices, .. }), Some(Answer::Choice(c))) => {
                choices.last() == Some(c)
            }
            (Some(Body::Multi { choices, .. }), Some(Answer::Choices(cs))) => {
                choices.last().is_some_and(|l| cs.contains(l))
            }
            _ => false,
        };
        match self.inputs.get(&at).filter(|_| shows) {
            Some(input) => window.focus(&input.focus_handle(cx), cx),
            None => window.focus(&self.focus, cx),
        }
    }

    fn field(&self, i: usize) -> gpui::AnyElement {
        match self.inputs.get(&i) {
            Some(input) => ui::field().child(input.clone()).into_any_element(),
            None => div().into_any_element(),
        }
    }
}

impl Focusable for SurveyDialog {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for SurveyDialog {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let panel = ui::modal_panel("survey", 440.)
            .track_focus(&self.focus)
            .on_key_down(cx.listener(|_, ev: &gpui::KeyDownEvent, _, cx| {
                if ev.keystroke.key == "escape" {
                    cx.emit(SurveyEvent::Dismissed);
                }
            }));
        let close = |cx: &mut Context<Self>| {
            ui::close_button("survey-close")
                .on_click(cx.listener(|_, _, _, cx| cx.emit(SurveyEvent::Dismissed)))
        };
        if self.flow.is_sent() {
            return panel
                .child(ui::modal_header(THANKS, None, close(cx)))
                .child(
                    div().flex().justify_end().p(ui::sp(16.)).child(
                        ui::button("survey-done", "Close", ButtonKind::Primary, true)
                            .on_click(cx.listener(|_, _, _, cx| cx.emit(SurveyEvent::Dismissed))),
                    ),
                );
        }
        let Some(q) = self.flow.question().cloned() else {
            return panel.child(ui::modal_header(
                self.flow.survey().name.clone(),
                None,
                close(cx),
            ));
        };
        let description = (!q.description.is_empty()).then(|| q.description.clone().into());
        let last = self.flow.is_last();
        let label = match (&q.button_text, last) {
            // A link question's button text is the link's own.
            (Some(t), _) if !matches!(q.body, Body::Link { .. }) => t.clone(),
            (_, true) => "Send".to_string(),
            (_, false) => "Next".to_string(),
        };
        let notice = self.flow.public_notice();
        let error = self.flow.error().map(str::to_string);
        let body = self.body(&q, cx);
        panel
            .child(ui::modal_header(q.text.clone(), description, close(cx)))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(ui::sp(12.))
                    .p(ui::sp(16.))
                    .child(body)
                    .when_some(notice, |d, n| {
                        d.child(ui::text(Type::Caption, theme::text_subtle(), n))
                    })
                    .when_some(error, |d, e| {
                        d.child(ui::text(Type::Caption, theme::error().into(), e))
                    })
                    .child(
                        div()
                            .flex()
                            .justify_end()
                            .gap(ui::sp(8.))
                            .pt(ui::sp(4.))
                            .when(self.flow.can_go_back(), |d| {
                                d.child(
                                    ui::button("survey-back", "Back", ButtonKind::Ghost, true)
                                        .on_click(cx.listener(|this, _, window, cx| {
                                            this.go_back(window, cx)
                                        })),
                                )
                            })
                            .child(
                                ui::button(
                                    "survey-next",
                                    label,
                                    ButtonKind::Primary,
                                    self.flow.can_advance(),
                                )
                                .on_click(
                                    cx.listener(|this, _, window, cx| this.go_next(window, cx)),
                                ),
                            ),
                    ),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::surveys::model::{Branching, Conditions, SurveyType, Target};
    use crate::telemetry::Refused;

    fn q(id: &str, body: Body, optional: bool, branching: Branching) -> Question {
        Question {
            id: id.into(),
            text: format!("{id}?"),
            description: String::new(),
            optional,
            button_text: None,
            body,
            branching,
        }
    }

    fn survey(questions: Vec<Question>) -> Survey {
        Survey {
            id: "s".into(),
            name: "s".into(),
            kind: SurveyType::Api,
            questions,
            start_date: None,
            end_date: None,
            archived: false,
            conditions: Conditions::default(),
        }
    }

    fn rating() -> Body {
        Body::Rating {
            scale: 5,
            emoji: false,
            lower: String::new(),
            upper: String::new(),
        }
    }

    fn branching() -> Survey {
        survey(vec![
            q(
                "r",
                rating(),
                false,
                Branching::ResponseBased(vec![
                    ("negative".into(), Target::Question(2)),
                    ("positive".into(), Target::End),
                ]),
            ),
            q("a", Body::Open, false, Branching::End),
            q("b", Body::Open, true, Branching::Next),
        ])
    }

    #[test]
    fn walks_with_branching() {
        let mut f = Flow::new(branching(), Source::User);
        assert_eq!(f.at(), 0);
        f.set(Answer::Rating(1));
        assert_eq!(f.advance(), Step::Moved);
        assert_eq!(f.at(), 2);
        assert!(f.is_last());
        assert_eq!(f.advance(), Step::Submit);
        // Only the questions on the path are sent.
        assert_eq!(f.answers(), vec![(0, Answer::Rating(1))]);

        let mut f = Flow::new(branching(), Source::User);
        f.set(Answer::Rating(5));
        assert!(f.is_last());
        assert_eq!(f.advance(), Step::Submit);

        let mut f = Flow::new(branching(), Source::User);
        f.set(Answer::Rating(3));
        assert_eq!(f.advance(), Step::Moved);
        assert_eq!(f.at(), 1);
        f.set(Answer::Text("hi".into()));
        assert!(f.is_last());
        assert_eq!(f.advance(), Step::Submit);
        assert_eq!(
            f.answers(),
            vec![(0, Answer::Rating(3)), (1, Answer::Text("hi".into()))]
        );
    }

    #[test]
    fn back_restores_previous_answer() {
        let mut f = Flow::new(branching(), Source::User);
        f.set(Answer::Rating(1));
        f.advance();
        assert_eq!(f.at(), 2);
        assert!(f.can_go_back());
        f.back();
        assert_eq!(f.at(), 0);
        assert_eq!(f.answer(), Some(&Answer::Rating(1)));
        assert!(!f.can_go_back());
        // A different answer takes another branch.
        f.set(Answer::Rating(3));
        f.advance();
        assert_eq!(f.at(), 1);
    }

    #[test]
    fn required_open_blocks_next_when_empty() {
        let s = survey(vec![
            q("a", Body::Open, false, Branching::Next),
            q("b", Body::Open, true, Branching::Next),
        ]);
        let mut f = Flow::new(s, Source::User);
        assert!(!f.can_advance());
        assert_eq!(f.advance(), Step::Blocked);
        f.set(Answer::Text("   ".into()));
        assert_eq!(f.advance(), Step::Blocked);
        assert_eq!(f.at(), 0);
        f.set(Answer::Text("it broke".into()));
        assert_eq!(f.advance(), Step::Moved);
        // Optional: an empty answer goes through, and is not sent.
        assert!(f.can_advance());
        assert_eq!(f.advance(), Step::Submit);
        assert_eq!(f.answers(), vec![(0, Answer::Text("it broke".into()))]);
    }

    #[test]
    fn last_step_shows_public_notice_for_user_and_error() {
        let s = survey(vec![
            q("a", Body::Open, false, Branching::Next),
            q("b", Body::Open, true, Branching::Next),
        ]);
        for src in [Source::User, Source::Error] {
            let mut f = Flow::new(s.clone(), src);
            assert_eq!(f.public_notice(), None);
            f.set(Answer::Text("x".into()));
            f.advance();
            assert_eq!(
                f.public_notice(),
                Some("Bug reports may become a public GitHub issue.")
            );
        }
        for src in [Source::Popover, Source::Agent] {
            let mut f = Flow::new(s.clone(), src);
            f.set(Answer::Text("x".into()));
            f.advance();
            assert!(f.is_last());
            assert_eq!(f.public_notice(), None);
        }
    }

    #[test]
    fn refused_shows_reason() {
        let s = survey(vec![q("a", Body::Open, false, Branching::Next)]);
        let mut f = Flow::new(s, Source::Popover);
        f.set(Answer::Text("typed".into()));
        assert_eq!(f.advance(), Step::Submit);
        f.refused(Refused::NoUsage);
        assert_eq!(
            f.error(),
            Some("Usage telemetry is off, so this was not sent.")
        );
        assert!(!f.is_sent());
        assert_eq!(f.answer(), Some(&Answer::Text("typed".into())));
        f.sent();
        assert!(f.is_sent());
        assert_eq!(f.error(), None);
        assert_eq!(THANKS, "Thanks. Sent to the Arakun team.");
    }
    #[test]
    fn open_choice_sends_typed_text() {
        let choices: Vec<String> = ["Bug", "Other"].iter().map(|s| s.to_string()).collect();
        let s = survey(vec![
            q(
                "k",
                Body::Single {
                    choices: choices.clone(),
                    open_choice: true,
                },
                false,
                Branching::Next,
            ),
            q(
                "m",
                Body::Multi {
                    choices,
                    open_choice: true,
                },
                false,
                Branching::Next,
            ),
        ]);
        let mut f = Flow::new(s, Source::User);
        f.pick("Other");
        f.set_other(0, "a crash".into());
        assert_eq!(f.advance(), Step::Moved);
        assert!(!f.can_advance());
        f.toggle("Other");
        f.toggle("Bug");
        f.set_other(1, "slow".into());
        f.toggle("Bug");
        f.toggle("Bug");
        assert_eq!(f.advance(), Step::Submit);
        assert_eq!(
            f.answers(),
            vec![
                (0, Answer::Choice("a crash".into())),
                (1, Answer::Choices(vec!["Bug".into(), "slow".into()])),
            ]
        );
    }
}
