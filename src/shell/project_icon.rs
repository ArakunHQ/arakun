//! The project icon editor: draw the 8×8 pixel-art icon by hand, draw a new
//! one at random, start from one of the classic drawings, and pick its color.

use gpui::prelude::FluentBuilder as _;
use gpui::{
    App, AppContext as _, Context, EventEmitter, FocusHandle, Focusable, Hsla,
    InteractiveElement as _, IntoElement, MouseButton, ParentElement as _, Render, SharedString,
    StatefulInteractiveElement as _, Styled as _, Window, div, px,
};

use super::{Modal, Shell};
use crate::model;
use crate::theme;
use crate::ui::{self, ButtonKind, Type};

/// One pixel of the editor's grid, in pixels.
const CELL: f32 = 26.;

pub enum IconEvent {
    Save { icon: u64, logo: u32 },
    Dismissed,
}

pub struct IconEditor {
    name: SharedString,
    icon: u64,
    logo: u32,
    /// What the path gives this project, for Reset.
    drawn: u64,
    /// Edits also land on the other half, so the icon stays symmetric.
    mirror: bool,
    /// While the button is held on the grid: whether the drag lights pixels
    /// or clears them, set by the first pixel it pressed.
    painting: Option<bool>,
    /// Advances on each Shuffle, so every press gives a new icon.
    shuffles: u64,
    focus: FocusHandle,
}

impl EventEmitter<IconEvent> for IconEditor {}

impl IconEditor {
    fn new(
        name: SharedString,
        icon: u64,
        logo: u32,
        drawn: u64,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let focus = cx.focus_handle();
        window.focus(&focus, cx);
        // Mirror stays on only for an icon that is already symmetric.
        let mirror = (0..8).all(|y| {
            let row = (icon >> (y * 8)) as u8;
            row == row.reverse_bits()
        });
        Self {
            name,
            icon,
            logo,
            drawn,
            mirror,
            painting: None,
            shuffles: 0,
            focus,
        }
    }

    fn set(&mut self, i: usize, on: bool, cx: &mut Context<Self>) {
        let (x, y) = (i % 8, i / 8);
        let mut cells = vec![i];
        if self.mirror {
            cells.push(y * 8 + 7 - x);
        }
        for c in cells {
            if on {
                self.icon |= 1 << c;
            } else {
                self.icon &= !(1 << c);
            }
        }
        cx.notify();
    }

    fn shuffle(&mut self, cx: &mut Context<Self>) {
        self.shuffles += 1;
        let seed = model::unix_now()
            .wrapping_mul(0x9e3779b97f4a7c15)
            .wrapping_add(self.shuffles)
            ^ self.icon;
        self.icon = model::icon_from_seed(seed);
        self.mirror = true;
        cx.notify();
    }

    fn save(&mut self, cx: &mut Context<Self>) {
        cx.emit(IconEvent::Save {
            icon: self.icon,
            logo: self.logo,
        });
    }

    fn render_grid(&self, color: Hsla, cx: &mut Context<Self>) -> impl IntoElement {
        let mut grid = div().flex().flex_col().gap(ui::sp(2.));
        for y in 0..8 {
            let mut row = div().flex().gap(ui::sp(2.));
            for x in 0..8 {
                let i = y * 8 + x;
                let lit = self.icon & (1 << i) != 0;
                row = row.child(
                    div()
                        .id(("icon-cell", i))
                        .size(px(CELL))
                        .rounded(ui::rad(3.))
                        .cursor_pointer()
                        .bg(if lit { color } else { theme::fill_hover() })
                        .when(!lit, |d| d.hover(|s| s.bg(theme::fill_selected())))
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(move |this, _, _, cx| {
                                let on = this.icon & (1 << i) == 0;
                                this.painting = Some(on);
                                this.set(i, on, cx);
                            }),
                        )
                        .on_mouse_move(cx.listener(
                            move |this, ev: &gpui::MouseMoveEvent, _, cx| {
                                if ev.pressed_button != Some(MouseButton::Left) {
                                    this.painting = None;
                                    return;
                                }
                                if let Some(on) = this.painting
                                    && (this.icon & (1 << i) != 0) != on
                                {
                                    this.set(i, on, cx);
                                }
                            },
                        )),
                );
            }
            grid = grid.child(row);
        }
        grid
    }
}

impl Focusable for IconEditor {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for IconEditor {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let color: Hsla = gpui::rgb(self.logo).into();
        let side = div()
            .flex()
            .flex_col()
            .flex_1()
            .min_w_0()
            .gap(ui::sp(16.))
            // How it looks in the sidebar.
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(ui::sp(6.))
                    .child(ui::overline("Preview"))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(ui::sp(8.))
                            .h(px(32.))
                            .px(ui::sp(8.))
                            .rounded(ui::rad(8.))
                            .bg(theme::selection_strong())
                            .child(ui::project_icon(self.icon, 1.5, color))
                            .child(
                                ui::text(Type::Body, theme::content(), self.name.clone())
                                    .flex_1()
                                    .min_w_0()
                                    .truncate(),
                            ),
                    )
                    .child(
                        div()
                            .flex()
                            .items_end()
                            .gap(ui::sp(12.))
                            .pt(ui::sp(4.))
                            .child(ui::project_icon(self.icon, 4., color))
                            .child(ui::project_icon(self.icon, 2., color))
                            .child(ui::project_icon(self.icon, 1., color)),
                    ),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(ui::sp(6.))
                    .child(ui::overline("Color"))
                    .child(div().flex().flex_wrap().gap(ui::sp(6.)).children(
                        model::LOGO_COLORS.iter().enumerate().map(|(n, &c)| {
                            ui::swatch(("icon-color", n), gpui::rgb(c), c == self.logo).on_click(
                                cx.listener(move |this, _, _, cx| {
                                    this.logo = c;
                                    cx.notify();
                                }),
                            )
                        }),
                    )),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(ui::sp(6.))
                    .child(ui::overline("Start from"))
                    .child(div().flex().flex_wrap().gap(ui::sp(4.)).children(
                        ui::PROJECT_ART.iter().enumerate().map(|(n, rows)| {
                            let bits = ui::rows_to_bits(rows);
                            div()
                                .id(("icon-preset", n))
                                .flex()
                                .items_center()
                                .justify_center()
                                .size(px(ui::INPUT_H))
                                .rounded(ui::rad(ui::RADIUS))
                                .cursor_pointer()
                                .map(ui::focusable)
                                .border_1()
                                .border_color(theme::border())
                                .hover(|s| s.bg(theme::fill_hover()))
                                .child(ui::project_icon(bits, 2., theme::text_muted()))
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.icon = bits;
                                    this.mirror = false;
                                    cx.notify();
                                }))
                        }),
                    )),
            )
            .child(
                ui::switch_row("icon-mirror", "Mirror", self.mirror).on_click(cx.listener(
                    |this, _, _, cx| {
                        this.mirror = !this.mirror;
                        cx.notify();
                    },
                )),
            );

        ui::modal_panel("project-icon", 540.)
            .track_focus(&self.focus)
            .on_key_down(cx.listener(|this, ev: &gpui::KeyDownEvent, _, cx| {
                match ev.keystroke.key.as_str() {
                    "escape" => cx.emit(IconEvent::Dismissed),
                    "enter" => this.save(cx),
                    _ => {}
                }
            }))
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, _, _, _| this.painting = None),
            )
            .child(ui::modal_header(
                "Project icon",
                None,
                ui::close_button("project-icon-close")
                    .on_click(cx.listener(|_, _, _, cx| cx.emit(IconEvent::Dismissed))),
            ))
            .child(
                div()
                    .flex()
                    .gap(ui::sp(24.))
                    .p(ui::sp(16.))
                    .child(self.render_grid(color, cx))
                    .child(side),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(ui::sp(8.))
                    .px(ui::sp(16.))
                    .pb(ui::sp(16.))
                    .child(
                        ui::button_with(
                            "icon-shuffle",
                            Some("refresh01"),
                            "Shuffle",
                            ButtonKind::Outline,
                            true,
                        )
                        .on_click(cx.listener(|this, _, _, cx| this.shuffle(cx))),
                    )
                    .child(
                        ui::button("icon-clear", "Clear", ButtonKind::Ghost, self.icon != 0)
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.icon = 0;
                                cx.notify();
                            })),
                    )
                    .child(
                        ui::button(
                            "icon-reset",
                            "Reset",
                            ButtonKind::Ghost,
                            self.icon != self.drawn,
                        )
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.icon = this.drawn;
                            this.mirror = true;
                            cx.notify();
                        })),
                    )
                    .child(div().flex_1())
                    .child(
                        ui::button("icon-cancel", "Cancel", ButtonKind::Ghost, true)
                            .on_click(cx.listener(|_, _, _, cx| cx.emit(IconEvent::Dismissed))),
                    )
                    .child(
                        ui::button("icon-save", "Save", ButtonKind::Primary, self.icon != 0)
                            .on_click(cx.listener(|this, _, _, cx| this.save(cx))),
                    ),
            )
    }
}

impl Shell {
    pub(super) fn open_icon_editor(
        &mut self,
        pi: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.menu = None;
        let Some(p) = self.projects.get(pi) else {
            return;
        };
        let (name, icon, logo, path) = (p.name.clone(), p.icon, p.logo, p.path.clone());
        let drawn = model::logo_for(&path).1;
        let dialog = cx.new(|cx| IconEditor::new(name, icon, logo, drawn, window, cx));
        let sub = cx.subscribe_in(
            &dialog,
            window,
            move |this, _, ev: &IconEvent, window, cx| {
                if let IconEvent::Save { icon, logo } = *ev
                && icon != 0
                // The list may have changed under the dialog; find it again.
                && let Some(p) = this.projects.iter_mut().find(|p| p.path == path)
                {
                    p.icon = icon;
                    p.logo = logo;
                    this.save(cx);
                }
                this.close_modal(window, cx);
            },
        );
        self.modal = Some((Modal::Session(dialog.into()), sub));
        cx.notify();
    }
}
