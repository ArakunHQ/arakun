//! The start screen of an environment and the welcome screen, each with the
//! raccoon's ASCII face.

use super::*;
use crate::ui::Typography as _;

impl Shell {
    pub(super) fn render_home(&self, window: &Window, cx: &mut Context<Self>) -> impl IntoElement {
        let project = self.project().name.clone();
        let face = self.home_face.read(cx);
        let caret = self
            .composer_layout
            .borrow()
            .as_ref()
            .and_then(|l| l.position_for_index(self.env().draft.head()));
        face.caret.set(caret);
        let alpha = face.ui_alpha();
        div().relative().flex().flex_col().size_full().child(
            div()
                .flex()
                .flex_col()
                .flex_1()
                .justify_center()
                .mx_auto()
                .w_full()
                .max_w(px(896.))
                .px(px(6.))
                .pt(px(48.))
                // Optical center: extra room below lifts the group above the
                // geometric middle, where the eye expects it.
                .pb(px(96.))
                .child(div().mb(px(10.)).child(self.home_face.clone()))
                .child(
                    div()
                        .mb(px(16.))
                        .px(px(10.))
                        .ty(ui::Type::Title)
                        .font_weight(gpui::FontWeight::NORMAL)
                        .opacity(alpha)
                        .child(format!("What should we work on in {project}?")),
                )
                .child(
                    div()
                        .opacity(alpha)
                        .child(self.render_composer(true, window, cx)),
                ),
        )
    }

    /// No projects yet: the raccoon over a short welcome and one button.
    pub(super) fn render_welcome(
        &self,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let maximized = window.is_maximized();
        let face = self.home_face.read(cx);
        face.caret.set(None);
        let alpha = face.ui_alpha();
        div()
            .id("welcome")
            .map(crate::ui::stop_window_move)
            .relative()
            .flex()
            .flex_col()
            .flex_1()
            .min_w_0()
            .h_full()
            .child(
                div()
                    .flex()
                    .flex_none()
                    .h(px(40.))
                    .on_mouse_down(MouseButton::Left, |ev, window, _| crate::ui::drag_window(ev, window))
                    .when(cfg!(windows), |d| d.window_control_area(WindowControlArea::Drag))
                    .child(div().flex_1())
                    .when(cfg!(not(target_os = "macos")), |d| d.child(
                        caption_buttons()
                            .child(
                                window_button("win-min", "minus-sign", 14., false)
                                    .on_click(|_, window, _| window.minimize_window()),
                            )
                            .child(max_button(maximized))
                            .child(
                                window_button("win-close", "cancel01", 14., true)
                                    .on_click(|_, _, cx| cx.quit()),
                            ),
                    )),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .items_center()
                    .justify_center()
                    .gap(px(14.))
                    .px(px(24.))
                    .pb(px(40.))
                    .child(div().w_full().max_w(px(896.)).child(self.home_face.clone()))
                    .child(
                        div()
                            .opacity(alpha)
                            .text_size(px(ui::TEXT_TITLE))
                            .font_weight(FontWeight::SEMIBOLD)
                            .child("Welcome to Arakun"),
                    )
                    .child(
                        div()
                            .opacity(alpha)
                            .max_w(px(420.))
                            .text_center()
                            .text_size(px(ui::TEXT_MD))
                            .line_height(px(20.))
                            .text_color(theme::text_subtle())
                            .child("Add a git repository to start. Every branch you work on gets its own environment with agents, terminals and a browser."),
                    )
                    .child(
                        ui::button("welcome-add", "Add project", ui::ButtonKind::Primary, true)
                            .opacity(alpha)
                            .mt(px(8.))
                            .h(px(32.))
                            .px(px(14.))
                            .text_size(px(ui::TEXT_MD))
                            .child(
                                div()
                                    .text_size(px(ui::TEXT_XS))
                                    .text_color(theme::background().opacity(0.5))
                                    .child(crate::platform::shortcut_label("Ctrl+O")),
                            )
                            .on_click(cx.listener(|this, _, window, cx| this.open_picker(window, cx))),
                    ),
            )
    }
}
