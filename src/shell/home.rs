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
                .min_h_0()
                .justify_center()
                .mx_auto()
                .w_full()
                .max_w(px(896.))
                .px(crate::ui::sp(6.))
                // Optical center: twice the room below as above lifts the group
                // above the geometric middle, where the eye expects it. On a
                // short window both go first, then the face shrinks.
                .child(div().flex_1().max_h(crate::ui::sp(48.)))
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .flex_shrink(1.)
                        .min_h_0()
                        .mb(px(10.))
                        .child(self.home_face.clone()),
                )
                .child(
                    div()
                        .mb(px(16.))
                        .px(crate::ui::sp(10.))
                        .ty(ui::Type::Title)
                        .opacity(alpha)
                        .child(format!("What should we work on in {project}?")),
                )
                .child(
                    div()
                        .flex_none()
                        .opacity(alpha)
                        .child(self.render_composer(true, window, cx)),
                )
                .child(div().flex_1().max_h(crate::ui::sp(96.))),
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
            .child(welcome_bar(maximized, None, px(0.)))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .items_center()
                    .justify_center()
                    .gap(crate::ui::sp(14.))
                    .px(crate::ui::sp(24.))
                    .pb(crate::ui::sp(40.))
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
                            .px(crate::ui::sp(14.))
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

    /// What shows in the welcome's place. Leaving the Servers tab tells the
    /// servers nobody is looking, as when its tab closes.
    pub(super) fn show_alone(&mut self, alone: Option<Alone>, cx: &mut Context<Self>) {
        let left = std::mem::replace(&mut self.alone, alone);
        if left == Some(Alone::Servers) && alone != left {
            self.servers_tab_closed(cx);
        }
        cx.notify();
    }

    /// A view that needs no project, with none added: the window's bar with
    /// a way back to the welcome, over the view.
    pub(super) fn render_view_alone(
        &self,
        alone: Alone,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let back = icon_button("alone-back", "arrow-left01", 14.)
            .tooltip(ui::tip("Back to the welcome screen"))
            .on_click(cx.listener(|this, _, _, cx| this.show_alone(None, cx)));
        let view = match alone {
            Alone::GitHub => self.github_view.clone().into_any_element(),
            Alone::Servers => self.servers_view.clone().into_any_element(),
            Alone::Usage => self.usage_view.clone().into_any_element(),
        };
        self.render_alone("view-alone", back, view, window)
    }

    /// A sign-in's terminal with no project added. Going back closes it,
    /// which stops what runs in it.
    pub(super) fn render_terminal_alone(
        &self,
        alone: &super::dock::TerminalAlone,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let back = icon_button("terminal-alone-back", "arrow-left01", 14.)
            .tooltip(ui::tip("Close the terminal and go back"))
            .on_click(cx.listener(|this, _, _, cx| {
                this.terminal_alone = None;
                cx.notify();
            }));
        self.render_alone(
            "terminal-alone",
            back,
            alone.view().clone().into_any_element(),
            window,
        )
    }

    /// A view in the welcome's place: the window's bar with `back`, over it.
    fn render_alone(
        &self,
        id: &'static str,
        back: gpui::Stateful<gpui::Div>,
        view: AnyElement,
        window: &Window,
    ) -> gpui::Stateful<gpui::Div> {
        div()
            .id(id)
            .map(crate::ui::stop_window_move)
            .flex()
            .flex_col()
            .flex_1()
            .min_w_0()
            .h_full()
            .child(welcome_bar(
                window.is_maximized(),
                Some(back.into_any_element()),
                // Clear of the traffic lights when no rail is left of it.
                ui::traffic_lights_inset(window) - px(self.titlebar_x()),
            ))
            .child(div().flex().flex_1().min_h_0().child(view))
    }
}

/// A view that needs no project, shown in the welcome's place before any
/// project was added. One at a time: the way back leads to the welcome.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Alone {
    GitHub,
    Servers,
    Usage,
}

/// The welcome screen's top: room to drag the window, its buttons, and
/// `lead` at the left, clear of the `lights` the traffic lights take.
pub(super) fn welcome_bar(maximized: bool, lead: Option<AnyElement>, lights: Pixels) -> gpui::Div {
    div()
        .flex()
        .flex_none()
        .h(px(40.))
        .on_mouse_down(MouseButton::Left, |ev, window, _| {
            crate::ui::drag_window(ev, window)
        })
        .when(cfg!(windows), |d| {
            d.window_control_area(WindowControlArea::Drag)
        })
        .when_some(lead, |d, lead| {
            d.child(
                div()
                    .flex()
                    .items_center()
                    .pl(ui::icon_inset().max(lights))
                    // A press on the button isn't a drag of the window.
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .child(lead),
            )
        })
        .child(div().flex_1())
        .when(cfg!(not(target_os = "macos")), |d| {
            d.child(
                caption_buttons()
                    .child(
                        window_button("win-min", "minus-sign", 14., false)
                            .on_click(|_, window, _| window.minimize_window()),
                    )
                    .child(max_button(maximized))
                    .child(
                        window_button("win-close", "cancel01", 14., true)
                            .on_click(|_, window, cx| crate::background::close(window, cx)),
                    ),
            )
        })
}
