//! The question asked once, on first launch, before Arakun sends anything to
//! PostHog (`telemetry`). Both answers carry the same weight; closing it
//! answers nothing, and it comes back next launch.

use gpui::{
    App, Context, EventEmitter, FocusHandle, Focusable, InteractiveElement as _, IntoElement,
    ParentElement as _, Render, StatefulInteractiveElement as _, Styled as _, Window, div, px,
};

use crate::theme;
use crate::ui::{self, ButtonKind, icon};

/// What is sent, said in full.
pub const DETAILS_URL: &str = "https://github.com/ArakunHQ/arakun#usage-and-crash-reports";

pub enum TelemetryEvent {
    /// The user chose; Settings › General can change it later.
    Answered(bool),
    /// "What is sent" was clicked.
    OpenDetails,
    /// Closed without an answer.
    Dismissed,
}

pub struct TelemetryDialog {
    focus: FocusHandle,
}

impl EventEmitter<TelemetryEvent> for TelemetryDialog {}

impl TelemetryDialog {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let focus = cx.focus_handle();
        window.focus(&focus, cx);
        Self { focus }
    }
}

impl Focusable for TelemetryDialog {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

/// What is and is not sent, as an icon and a sentence each. The setup's step
/// (`shell::onboarding`) shows the same ones.
pub const POINTS: [(&str, &str); 3] = [
    (
        "alert-circle",
        "Crashes with their stack trace, and the kind of error when an agent stops. A crash's message loses paths, links and keys; its other words are sent.",
    ),
    (
        "activity01",
        "Counts such as sessions started, with the agent's name.",
    ),
    (
        "view-off",
        "Never prompts, replies or files. A random id, not your account.",
    ),
];

pub fn point(icon_name: &str, text: &'static str) -> impl IntoElement {
    div()
        .flex()
        .items_start()
        .gap(ui::sp(10.))
        .child(
            div()
                .pt(ui::sp(2.))
                .child(icon(icon_name, 14.).text_color(theme::text_subtle())),
        )
        .child(div().flex_1().min_w_0().child(text))
}

impl Render for TelemetryDialog {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // Equal width and the same kind: neither answer is the easy one.
        let answer = |id: &'static str, label: &'static str, yes: bool| {
            ui::button(id, label, ButtonKind::Outline, true)
                .flex_1()
                .on_click(cx.listener(move |_, _, _, cx| cx.emit(TelemetryEvent::Answered(yes))))
        };
        ui::modal_panel("telemetry", 440.)
            .track_focus(&self.focus)
            .on_key_down(cx.listener(|_, ev: &gpui::KeyDownEvent, _, cx| {
                if ev.keystroke.key == "escape" {
                    cx.emit(TelemetryEvent::Dismissed);
                }
            }))
            .child(ui::modal_header(
                "Send crash reports and usage counts?",
                None,
                ui::close_button("telemetry-close")
                    .on_click(cx.listener(|_, _, _, cx| cx.emit(TelemetryEvent::Dismissed))),
            ))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(ui::sp(12.))
                    .p(ui::sp(16.))
                    .text_size(px(ui::TEXT_SM))
                    .line_height(px(18.))
                    .text_color(theme::text_muted())
                    .child(
                        "They tell us what breaks and what gets used. Nothing is sent unless you say yes.",
                    )
                    .children(POINTS.map(|(icon_name, text)| point(icon_name, text)))
                    .child(
                        div()
                            .id("telemetry-details")
                            .text_color(theme::accent())
                            .cursor_pointer()
                            .hover(|s| s.underline())
                            .child("What is sent")
                            .on_click(
                                cx.listener(|_, _, _, cx| cx.emit(TelemetryEvent::OpenDetails)),
                            ),
                    )
                    .child(
                        div()
                            .text_size(px(ui::TEXT_XS))
                            .text_color(theme::text_subtle())
                            .child("Change it any time in Settings › General."),
                    )
                    .child(
                        div()
                            .flex()
                            .gap(ui::sp(8.))
                            .pt(ui::sp(4.))
                            .child(answer("telemetry-no", "Don't send", false))
                            .child(answer("telemetry-yes", "Send reports", true)),
                    ),
            )
    }
}
