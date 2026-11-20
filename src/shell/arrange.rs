//! Arranging the sidebar where it is: while Customize shows its Sidebar tab,
//! a click on a piece or a segment of the real rail selects it instead of
//! opening it, and the panel shows what can be added and the options of what
//! is selected. The editing itself is the sidebar builder's.

use super::sidebar_builder::{BuilderSel, PinAsk};
use super::*;
use crate::sidebar::{Mode, Piece};

impl Shell {
    /// Customize is open on its Sidebar tab: the rail is being arranged.
    pub(super) fn arranging(&self, cx: &App) -> bool {
        self.customize
            .as_ref()
            .is_some_and(|(view, _)| view.read(cx).tab() == CustomizeTab::Sidebar)
    }

    /// Lays a click target over a rail piece or segment title while
    /// arranging: it selects instead of opening, and outlines the selection.
    pub(super) fn arrange_target(
        &self,
        sel: BuilderSel,
        el: AnyElement,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let selected = self.builder.sel == Some(sel);
        let id: gpui::ElementId = match sel {
            BuilderSel::Item((s, i)) => ("arrange-item", s * 1000 + i).into(),
            BuilderSel::Section(s) => ("arrange-section", s).into(),
        };
        div()
            .relative()
            .child(el)
            .child(
                div()
                    .id(id)
                    .absolute()
                    .inset_0()
                    .rounded(crate::ui::rad(8.))
                    .border_1()
                    .border_color(if selected {
                        theme::active()
                    } else {
                        gpui::transparent_black()
                    })
                    .when(!selected, |d| {
                        d.hover(|s| s.border_color(theme::border_strong()))
                    })
                    .cursor_pointer()
                    .occlude()
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.select_builder(sel, window, cx);
                        cx.notify();
                    })),
            )
            .into_any_element()
    }

    /// The bar over the rail while it is being arranged, with the way out.
    pub(super) fn render_arranging_bar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .flex()
            .flex_none()
            .items_center()
            .gap(crate::ui::sp(8.))
            .mx(crate::ui::sp(8.))
            .mb(crate::ui::sp(4.))
            .px(crate::ui::sp(8.))
            .h(px(ui::BUTTON_H + 4.))
            .rounded(crate::ui::rad(8.))
            .bg(theme::fill_hover())
            .child(icon("sidebar-left", 13.).text_color(theme::text_subtle()))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_size(px(ui::TEXT_SM))
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(theme::text())
                    .child("Arranging"),
            )
            .child(
                ui::button("arranging-done", "Done", ui::ButtonKind::Primary, true)
                    .on_click(cx.listener(|this, _, _, cx| this.close_customize(cx))),
            )
    }

    /// Customize's Add and Selected parts for the Sidebar tab.
    pub(super) fn render_arrange_panel(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let _ = window;
        // What the sidebar does not hold yet; one of a kind pieces only once.
        let mut pieces: Vec<Piece> = [
            Piece::Search,
            Piece::projects(),
            Piece::active(),
            Piece::User,
            Piece::Usage,
            Piece::Settings,
            Piece::Mascot,
        ]
        .into_iter()
        .filter(|p| !self.sidebar.has(p))
        .collect();
        pieces.extend(
            self.sidebar
                .hidden_views()
                .into_iter()
                .map(Piece::view)
                .filter(|p| self.rail_shows(p, cx)),
        );
        pieces.extend([Piece::Separator, Piece::Spacer]);
        let chip = |id: SharedString, icon_name: &'static str, label: SharedString| {
            div()
                .id(id)
                .flex()
                .flex_none()
                .items_center()
                .gap(crate::ui::sp(6.))
                .h(px(ui::BUTTON_H))
                .px(crate::ui::sp(10.))
                .rounded(crate::ui::rad(ui::RADIUS))
                .border_1()
                .border_color(theme::border())
                .text_size(px(ui::TEXT_SM))
                .text_color(theme::text_muted())
                .cursor_pointer()
                .hover(|s| s.bg(theme::fill_hover()).text_color(theme::content()))
                .child(icon(icon_name, 13.).text_color(theme::text_subtle()))
                .child(label)
        };
        let mut chips = div()
            .flex()
            .flex_wrap()
            .gap(crate::ui::sp(6.))
            .child(
                chip("arr-group".into(), "add01", "Group".into()).on_click(cx.listener(
                    |this, _, window, cx| {
                        let after = match this.builder.sel {
                            Some(BuilderSel::Section(s)) | Some(BuilderSel::Item((s, _))) => s + 1,
                            None => this.sidebar.sections.len(),
                        };
                        let mut at = after;
                        this.change_sidebar(cx, |sb| at = sb.add_section(after));
                        this.select_builder(BuilderSel::Section(at), window, cx);
                    },
                )),
            )
            .child(
                chip("arr-link".into(), "link01", "Link…".into()).on_click(
                    cx.listener(|this, _, window, cx| this.ask_pin(PinAsk::Url, window, cx)),
                ),
            )
            .child(
                chip("arr-cmd".into(), "computer-terminal01", "Command…".into()).on_click(
                    cx.listener(|this, _, window, cx| this.ask_pin(PinAsk::Command, window, cx)),
                ),
            )
            .child(
                chip("arr-title".into(), "pencil-edit01", "Title…".into()).on_click(
                    cx.listener(|this, _, window, cx| this.ask_pin(PinAsk::Title, window, cx)),
                ),
            );
        if let Some(pin) = self.tab_pin() {
            let piece = Piece::Pin { pin };
            chips = chips.child(
                chip("arr-tab".into(), piece.icon(), "This tab".into())
                    .on_click(cx.listener(move |this, _, _, cx| this.add_piece(piece.clone(), cx))),
            );
        }
        for (i, piece) in pieces.into_iter().enumerate() {
            let label: SharedString = piece.label().into();
            chips = chips.child(
                chip(format!("arr-piece-{i}").into(), piece.icon(), label)
                    .on_click(cx.listener(move |this, _, _, cx| this.add_piece(piece.clone(), cx))),
            );
        }
        let target = match self.builder.sel {
            Some(BuilderSel::Section(s)) | Some(BuilderSel::Item((s, _))) => {
                self.sidebar.sections.get(s).map(|sec| sec.title.clone())
            }
            None => self
                .sidebar
                .sections
                .iter()
                .find(|s| s.mode == Mode::Scroll)
                .map(|s| s.title.clone()),
        };
        let add = div()
            .flex()
            .flex_col()
            .gap(crate::ui::sp(8.))
            .child(arrange_heading(
                "Add",
                target
                    .filter(|t| !t.is_empty())
                    .map(|t| format!("to {t}"))
                    .unwrap_or_default(),
            ))
            .child(chips)
            .when_some(self.builder.prompt.as_ref(), |d, prompt| {
                d.child(ui::field().child(prompt.input.clone()))
            });
        let selected: AnyElement = match (self.builder.sel, &self.builder.inputs) {
            (Some(BuilderSel::Item(p)), Some(inputs)) if inputs.sel == BuilderSel::Item(p) => {
                self.render_item_options(p, inputs, cx).into_any_element()
            }
            (Some(BuilderSel::Section(s)), Some(inputs))
                if inputs.sel == BuilderSel::Section(s) =>
            {
                self.render_section_options(s, inputs, cx)
                    .into_any_element()
            }
            _ => div()
                .flex()
                .flex_col()
                .gap(crate::ui::sp(6.))
                .child(arrange_heading("Selected", String::new()))
                .child(
                    div()
                        .text_size(px(ui::TEXT_SM))
                        .text_color(theme::text_subtle())
                        .child(
                            "Click a piece or a group's title in the sidebar to see its options.",
                        ),
                )
                .into_any_element(),
        };
        div()
            .flex()
            .flex_col()
            .gap(crate::ui::sp(16.))
            .child(add)
            .child(
                div()
                    .pt(crate::ui::sp(12.))
                    .border_t_1()
                    .border_color(theme::border_subtle())
                    .child(selected),
            )
            .into_any_element()
    }
}

/// A part's name, with a short note on its right.
fn arrange_heading(title: &'static str, note: String) -> gpui::Div {
    div()
        .flex()
        .items_center()
        .justify_between()
        .gap(crate::ui::sp(8.))
        .child(
            div()
                .text_size(px(ui::TEXT_MD))
                .font_weight(FontWeight::MEDIUM)
                .text_color(theme::text())
                .child(title),
        )
        .when(!note.is_empty(), |d| {
            d.child(
                div()
                    .min_w_0()
                    .truncate()
                    .text_size(px(ui::TEXT_XS))
                    .text_color(theme::text_faint())
                    .child(note),
            )
        })
}
