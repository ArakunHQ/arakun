//! Arranging the sidebar where it is: while Customize shows its Sidebar tab,
//! a click on a piece or a segment of the real rail selects it instead of
//! opening it, and the panel shows what can be added and the options of what
//! is selected. The editing itself is the sidebar builder's.

use super::sidebar_builder::{BuilderSel, DraggedPiece, DraggedSection, PinAsk};
use super::*;
use crate::sidebar::{Item, Piece};
use gpui::{BorderStyle, Bounds, Corners, Edges, Hsla, Pixels, Point, canvas, quad};

impl Shell {
    /// Customize is open on its Sidebar tab: the rail is being arranged.
    pub(super) fn arranging(&self) -> bool {
        self.panel.customize.is_some() && self.panel.customize_tab == CustomizeTab::Sidebar
    }

    /// Lays a click target over a rail piece or segment title while
    /// arranging: it selects instead of opening, and outlines the selection.
    /// `compact` is a piece drawn as its icon alone (narrowed or in a row),
    /// where the remove button sits on the corner instead of the row's end.
    pub(super) fn arrange_target(
        &self,
        sel: BuilderSel,
        el: AnyElement,
        compact: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let selected = self.builder.sel == Some(sel);
        let id: gpui::ElementId = match sel {
            BuilderSel::Item((s, i)) => ("arrange-item", s * 1000 + i).into(),
            BuilderSel::Section(s) => ("arrange-section", s).into(),
        };
        let lifted = cx.has_active_drag() && self.builder.lifted == Some(sel);
        let target = div()
            .id(id)
            .group("arrange-target")
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
            }));
        let shell = cx.entity().downgrade();
        let target = match sel {
            BuilderSel::Item(path) => {
                let Some(item) = self.sidebar.get(path).cloned() else {
                    return el;
                };
                let dragged = DraggedPiece {
                    from: Some(path),
                    title: item.piece.label().into(),
                    item,
                    offset: Point::default(),
                };
                target
                    .when(!cx.has_active_drag(), |d| {
                        d.child(self.arrange_remove(path, compact, cx))
                    })
                    .on_drag(dragged, move |d, offset, _, cx| {
                        let _ = shell.update(cx, |this, _| {
                            this.builder.lifted = d.from.map(BuilderSel::Item)
                        });
                        cx.new(|_| DraggedPiece {
                            offset,
                            ..d.clone()
                        })
                    })
                    // The line above says where a dropped piece lands.
                    .drag_over::<DraggedPiece>(|s, _, _, _| {
                        s.border_t_2().border_color(theme::active())
                    })
                    .on_drop(cx.listener(move |this, d: &DraggedPiece, _, cx| {
                        this.drop_piece(d, path, cx)
                    }))
            }
            BuilderSel::Section(si) => {
                let title = self
                    .sidebar
                    .sections
                    .get(si)
                    .map(|s| s.title.clone())
                    .unwrap_or_default();
                let dragged = DraggedSection {
                    from: si,
                    title: title.into(),
                    offset: Point::default(),
                };
                target
                    .on_drag(dragged, move |d, offset, _, cx| {
                        let _ = shell.update(cx, |this, _| {
                            this.builder.lifted = Some(BuilderSel::Section(d.from))
                        });
                        cx.new(|_| DraggedSection {
                            offset,
                            ..d.clone()
                        })
                    })
                    // A segment dropped on another lands above it; a piece,
                    // at its start.
                    .drag_over::<DraggedSection>(|s, _, _, _| {
                        s.border_t_2().border_color(theme::active())
                    })
                    .drag_over::<DraggedPiece>(|s, _, _, _| s.bg(theme::active().opacity(0.12)))
                    .on_drop(cx.listener(move |this, d: &DraggedSection, _, cx| {
                        this.builder.lifted = None;
                        this.change_sidebar(cx, |s| {
                            s.move_section(d.from, si);
                        });
                        this.clear_builder_selection();
                    }))
                    .on_drop(cx.listener(move |this, d: &DraggedPiece, _, cx| {
                        this.drop_piece(d, (si, 0), cx)
                    }))
            }
        };
        div()
            .relative()
            .when(lifted, |d| d.opacity(0.4))
            .child(el)
            .child(target)
            .into_any_element()
    }

    /// The button that takes a piece out of the sidebar, shown while the
    /// pointer is on it: removing should not need selecting first.
    fn arrange_remove(
        &self,
        path: crate::sidebar::Path,
        compact: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let (s, i) = path;
        let button = div()
            .id(("arrange-remove", s * 1000 + i))
            .flex()
            .items_center()
            .justify_center()
            .size(px(if compact { 16. } else { 20. }))
            .rounded_full()
            .bg(theme::raised())
            .border_1()
            .border_color(theme::border_strong())
            .cursor_pointer()
            .group("arrange-remove")
            .hover(|st| st.border_color(theme::danger()))
            .tooltip(ui::tip("Remove from the sidebar"))
            .child(ui::hover_icon(
                "cancel01",
                if compact { 10. } else { 12. },
                theme::text_subtle(),
                theme::danger().into(),
                "arrange-remove",
            ))
            .on_click(cx.listener(move |this, _, _, cx| {
                // Not a click on the piece: that would select it.
                cx.stop_propagation();
                this.change_sidebar(cx, |sb| {
                    sb.remove(path);
                });
                this.clear_builder_selection();
            }));
        // Over the icon's corner when the piece is an icon, at the end of
        // the row otherwise.
        div()
            .absolute()
            .invisible()
            .group_hover("arrange-target", |st| st.visible())
            .map(|d| {
                if compact {
                    d.top(px(-6.)).right(px(-6.))
                } else {
                    d.top_0()
                        .bottom_0()
                        .right(crate::ui::sp(6.))
                        .flex()
                        .items_center()
                }
            })
            .child(button)
            .into_any_element()
    }

    /// A row for a piece that draws nothing right now (an empty list, a
    /// card with nothing to say), so it can be reached while arranging.
    pub(super) fn arrange_placeholder(&self, item: &Item) -> AnyElement {
        div()
            .flex()
            .items_center()
            .gap(crate::ui::sp(8.))
            .h(px(self.sidebar.style.density.row_h()))
            .px(crate::ui::sp(8.))
            .text_size(px(ui::TEXT_SM))
            .text_color(theme::text_faint())
            .child(icon(item.piece.icon(), 14.).text_color(theme::text_faint()))
            .child(div().min_w_0().truncate().child(item.name()))
            .into_any_element()
    }

    /// The end of a segment while arranging: a piece dropped here lands
    /// last in it, and a segment dropped here goes after it.
    pub(super) fn arrange_end(&self, si: usize, len: usize, cx: &mut Context<Self>) -> AnyElement {
        div()
            .id(("arrange-end", si))
            .h(px(10.))
            .mx(crate::ui::sp(8.))
            .rounded(crate::ui::rad(4.))
            .drag_over::<DraggedPiece>(|s, _, _, _| s.bg(theme::active().opacity(0.25)))
            .drag_over::<DraggedSection>(|s, _, _, _| s.bg(theme::active().opacity(0.25)))
            .on_drop(
                cx.listener(move |this, d: &DraggedPiece, _, cx| this.drop_piece(d, (si, len), cx)),
            )
            .on_drop(cx.listener(move |this, d: &DraggedSection, _, cx| {
                this.builder.lifted = None;
                this.change_sidebar(cx, |s| {
                    s.move_section(d.from, si + 1);
                });
                this.clear_builder_selection();
            }))
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
            Piece::Help,
            Piece::Running,
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
        // "This tab" needs a project: with none there is no tab to pin.
        if self.has_projects()
            && let Some(pin) = self.tab_pin()
        {
            let piece = Piece::Pin { pin };
            chips = chips.child(
                chip("arr-tab".into(), piece.icon(), "This tab".into())
                    .on_click(cx.listener(move |this, _, _, cx| this.add_piece(piece.clone(), cx))),
            );
        }
        for (i, piece) in pieces.into_iter().enumerate() {
            let label: SharedString = piece.label().into();
            // A view this version just added, until Customize closes.
            let new = matches!(piece, Piece::View { view } if self.sidebar.is_new(view));
            let dragged = DraggedPiece {
                from: None,
                item: Item::new(piece.clone()),
                title: label.clone(),
                offset: Point::default(),
            };
            chips = chips.child(
                chip(format!("arr-piece-{i}").into(), piece.icon(), label)
                    .when(new, |d| {
                        d.child(div().size(px(6.)).rounded_full().bg(theme::active()))
                    })
                    .on_click(cx.listener(move |this, _, _, cx| this.add_piece(piece.clone(), cx)))
                    .on_drag(dragged, |d, offset, _, cx| {
                        cx.new(|_| DraggedPiece {
                            offset,
                            ..d.clone()
                        })
                    }),
            );
        }
        chips = chips.child(
            chip("arr-more".into(), "store01", "More in Marketplace…".into()).on_click(
                cx.listener(|this, _, window, cx| {
                    this.open_extensions_tab(window, cx);
                    this.views
                        .extensions
                        .update(cx, |v, cx| v.show_extensions(cx));
                }),
            ),
        );
        let target = self
            .add_target()
            .map(|s| self.sidebar.sections[s].title.clone());
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
                            "Click a piece or a group's title in the sidebar to see its options, and drag it to move it. Pieces in Add also drag onto the sidebar; Group, Link, Command and Title are added with a click.",
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

/// A dashed outline GPUI's own dashes can't give: they grow with the
/// border's width, so at one pixel they are two long with one between and
/// read as a solid line. Arranging should look like another mode.
pub(super) fn dashed_outline(color: Hsla, radius: Pixels) -> impl IntoElement {
    canvas(
        |_, _, _| {},
        move |b: Bounds<Pixels>, _, window, _| {
            let w = px(1.);
            let r = radius.min(b.size.width / 2.).min(b.size.height / 2.);
            let (x0, y0) = (b.origin.x, b.origin.y);
            let (x1, y1) = (x0 + b.size.width, y0 + b.size.height);
            let none = Pixels::default();
            // The corners are quarter circles, each one a dash of its own.
            let corners = [
                (
                    x0,
                    y0,
                    Corners {
                        top_left: r,
                        top_right: none,
                        bottom_left: none,
                        bottom_right: none,
                    },
                    Edges {
                        top: w,
                        left: w,
                        right: none,
                        bottom: none,
                    },
                ),
                (
                    x1 - r,
                    y0,
                    Corners {
                        top_left: none,
                        top_right: r,
                        bottom_left: none,
                        bottom_right: none,
                    },
                    Edges {
                        top: w,
                        right: w,
                        left: none,
                        bottom: none,
                    },
                ),
                (
                    x0,
                    y1 - r,
                    Corners {
                        top_left: none,
                        top_right: none,
                        bottom_left: r,
                        bottom_right: none,
                    },
                    Edges {
                        bottom: w,
                        left: w,
                        top: none,
                        right: none,
                    },
                ),
                (
                    x1 - r,
                    y1 - r,
                    Corners {
                        top_left: none,
                        top_right: none,
                        bottom_left: none,
                        bottom_right: r,
                    },
                    Edges {
                        bottom: w,
                        right: w,
                        top: none,
                        left: none,
                    },
                ),
            ];
            for (x, y, radii, edges) in corners {
                window.paint_quad(quad(
                    Bounds::new(point(x, y), size(r, r)),
                    radii,
                    gpui::transparent_black(),
                    edges,
                    color,
                    BorderStyle::Solid,
                ));
            }
            for (start, len) in dashes((b.size.width - r * 2.) / px(1.)) {
                for y in [y0, y1 - w] {
                    window.paint_quad(gpui::fill(
                        Bounds::new(point(x0 + r + px(start), y), size(px(len), w)),
                        color,
                    ));
                }
            }
            for (start, len) in dashes((b.size.height - r * 2.) / px(1.)) {
                for x in [x0, x1 - w] {
                    window.paint_quad(gpui::fill(
                        Bounds::new(point(x, y0 + r + px(start)), size(w, px(len))),
                        color,
                    ));
                }
            }
        },
    )
    .absolute()
    .inset_0()
}

/// Where the dashes of a straight side go, as (start, length): about four
/// pixels with three between, a half gap at each end so they stand apart
/// from the corners, stretched a little so the side ends evenly.
fn dashes(len: f32) -> Vec<(f32, f32)> {
    const DASH: f32 = 4.;
    const GAP: f32 = 3.;
    if len < DASH {
        return Vec::new();
    }
    let n = (len / (DASH + GAP)).round().max(1.) as usize;
    let period = len / n as f32;
    (0..n)
        .map(|k| (k as f32 * period + GAP / 2., period - GAP))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::dashes;

    #[test]
    fn dashes_leave_gaps_and_stay_on_the_side() {
        let d = dashes(100.);
        assert_eq!(d.len(), 14);
        for pair in d.windows(2) {
            let gap = pair[1].0 - (pair[0].0 + pair[0].1);
            assert!((gap - 3.).abs() < 0.01);
        }
        let last = d.last().unwrap();
        assert!(d[0].0 > 0. && last.0 + last.1 < 100.);
        assert!(dashes(2.).is_empty());
    }
}
