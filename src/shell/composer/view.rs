//! How the composer is drawn: what waits above the text, the text with its
//! caret, and the bar of chips and buttons under it.

use super::*;
use crate::agent::notes::NoteKind;

/// The composer caret, in pixels.
pub(super) const CARET_WIDTH: f32 = 1.5;
const CARET_HEIGHT: f32 = 16.;
/// The line the placeholder sits on, which centers the caret before the text.
const PLACEHOLDER_LINE: f32 = 22.;
/// Space above and below the composer's text, kept around the caret when it scrolls.
const COMPOSER_PAD: f32 = 12.;

impl Render for Composer {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let focused = self.focus.is_focused(window);
        let here = self.here();
        let ready = !here.text.bare().trim().is_empty() || here.has_extras();
        let completion = self.render_completion(cx);
        let images = self.render_file_row(cx);
        let pastes = self.render_paste_row(cx);
        let notes = self.render_note_row(cx);
        let context = self.render_context_row(cx);
        let limits = self.render_limits(cx);
        let secret = self.render_secret_notice(cx);
        let flash = self.flash.clone().map(|f| {
            div()
                .px(crate::ui::sp(12.))
                .pt(crate::ui::sp(8.))
                .text_size(px(ui::TEXT_SM))
                .text_color(theme::text_subtle())
                .child(f)
        });
        let text = self.render_text(focused, cx);
        let bar = self.render_bar(ready, cx);
        div()
            .id("composer")
            .track_focus(&self.focus)
            .on_key_down(cx.listener(Self::key_down))
            .drag_over::<drag::DraggedSession>(|s, _, _, _| {
                s.border_color(theme::active())
                    .bg(theme::active().opacity(0.06))
            })
            .on_drop(cx.listener(|_, d: &drag::DraggedSession, _, cx| {
                cx.emit(ComposerEvent::DropSession(d.clone()))
            }))
            // Files dropped from a file manager wait above the text; folders go in as mentions.
            .drag_over::<gpui::ExternalPaths>(|s, _, _, _| {
                s.border_color(theme::active())
                    .bg(theme::active().opacity(0.06))
            })
            .on_drop(
                cx.listener(|this, paths: &gpui::ExternalPaths, window, cx| {
                    this.push_mentions(paths.paths());
                    window.focus(&this.focus, cx);
                    this.changed(cx);
                    cx.notify();
                }),
            )
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, window, cx| {
                    window.focus(&this.focus, cx);
                    cx.notify();
                }),
            )
            // Linux middle-click pastes the primary selection.
            .on_mouse_down(
                MouseButton::Middle,
                cx.listener(|this, _, window, cx| {
                    window.focus(&this.focus, cx);
                    this.paste(crate::platform::read_primary(cx), cx);
                }),
            )
            .cursor_text()
            .flex()
            .flex_col()
            .w_full()
            // As round as the window's cards; its buttons sit 8 px in. Raised
            // off the sheet by its tone; the border only shows while typing
            // (and lights up when something is dragged over).
            .rounded(crate::ui::rad(ui::RADIUS_XL))
            .border_1()
            .border_color(if focused {
                theme::border()
            } else {
                gpui::transparent_black()
            })
            .bg(theme::raised())
            .children(completion)
            .children(images)
            .children(pastes)
            .children(notes)
            .children(context)
            .children(limits)
            .children(secret)
            .children(flash)
            .child(text)
            .child(bar)
    }
}

impl Composer {
    /// The large pastes waiting, as chips with a cross.
    fn render_paste_row(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let pastes = &self.here().pastes;
        if pastes.is_empty() {
            return None;
        }
        Some(
            div()
                .flex()
                .flex_wrap()
                .gap(crate::ui::sp(6.))
                .px(crate::ui::sp(12.))
                .pt(crate::ui::sp(10.))
                .children(pastes.iter().enumerate().map(|(i, p)| {
                    div()
                        .id(("paste-chip", i))
                        .flex()
                        .items_center()
                        .gap(crate::ui::sp(6.))
                        .h(px(26.))
                        .pl(crate::ui::sp(8.))
                        .pr(crate::ui::sp(4.))
                        .rounded(crate::ui::rad(8.))
                        .border_1()
                        .border_color(theme::border())
                        .bg(theme::fill_hover())
                        .text_size(px(ui::TEXT_SM))
                        .child(icon("file01", 12.).text_color(theme::text_subtle()))
                        .child(div().text_color(theme::text_muted()).child("Pasted text"))
                        .child(div().text_color(theme::text_subtle()).child(format!(
                            "{} lines · {:.1}k chars",
                            p.lines,
                            p.chars as f32 / 1000.
                        )))
                        .child(
                            div()
                                .id(("paste-remove", i))
                                .size(px(18.))
                                .flex()
                                .items_center()
                                .justify_center()
                                .rounded(crate::ui::rad(4.))
                                .cursor_pointer()
                                .hover(|s| s.bg(theme::fill_selected()))
                                .child(icon("cancel01", 11.).text_color(theme::text_subtle()))
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    cx.stop_propagation();
                                    let pastes = &mut this.here_mut().pastes;
                                    if i < pastes.len() {
                                        pastes.remove(i);
                                    }
                                    cx.notify();
                                })),
                        )
                }))
                .into_any_element(),
        )
    }

    /// The annotations and review comments waiting, with "Submit review".
    fn render_note_row(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let notes = Some(&self.here().notes).filter(|v| !v.is_empty())?;
        let annotations = notes
            .iter()
            .filter(|n| n.kind == NoteKind::Annotation)
            .count();
        let reviews = notes.len() - annotations;
        let mut summary = Vec::new();
        if annotations > 0 {
            summary.push(format!(
                "{annotations} annotation{}",
                if annotations == 1 { "" } else { "s" }
            ));
        }
        if reviews > 0 {
            summary.push(format!(
                "{reviews} review comment{}",
                if reviews == 1 { "" } else { "s" }
            ));
        }
        Some(
            div()
                .flex()
                .flex_col()
                .gap(crate::ui::sp(6.))
                .px(crate::ui::sp(12.))
                .pt(crate::ui::sp(10.))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(crate::ui::sp(8.))
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .gap(crate::ui::sp(4.))
                                .h(px(20.))
                                .px(crate::ui::sp(8.))
                                .rounded_full()
                                .bg(crate::agent::mark_color().opacity(0.14))
                                .text_size(px(crate::ui::TEXT_XS))
                                .font_weight(FontWeight::MEDIUM)
                                .text_color(crate::agent::mark_color())
                                .child(
                                    icon("comment01", 11.).text_color(crate::agent::mark_color()),
                                )
                                .child(summary.join(" · ")),
                        )
                        .child(div().flex_1())
                        .when(reviews > 0, |d| {
                            d.child(
                                div()
                                    .id("submit-review")
                                    .flex()
                                    .items_center()
                                    .gap(crate::ui::sp(6.))
                                    .h(px(24.))
                                    .px(crate::ui::sp(8.))
                                    .rounded(crate::ui::rad(8.))
                                    .cursor_pointer()
                                    .bg(theme::content())
                                    .text_color(theme::background())
                                    .text_size(px(crate::ui::TEXT_XS))
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .child("Submit review")
                                    .on_click(cx.listener(|_, _, _, cx| {
                                        cx.stop_propagation();
                                        cx.emit(ComposerEvent::Submit { now: false });
                                    })),
                            )
                        })
                        .child(
                            div()
                                .id("notes-clear")
                                .text_size(px(crate::ui::TEXT_XS))
                                .text_color(theme::text_subtle())
                                .cursor_pointer()
                                .hover(|s| s.text_color(theme::text_muted()))
                                .child("Clear")
                                .on_click(cx.listener(|this, _, _, cx| {
                                    cx.stop_propagation();
                                    this.clear_notes(cx);
                                })),
                        ),
                )
                .child(div().flex().flex_wrap().gap(crate::ui::sp(6.)).children(
                    notes.iter().enumerate().map(|(i, n)| {
                        let review = n.kind == NoteKind::Review;
                        div()
                            .id(("note-chip", i))
                            .flex()
                            .items_center()
                            .gap(crate::ui::sp(6.))
                            .h(px(26.))
                            .max_w(px(320.))
                            .pl(crate::ui::sp(8.))
                            .pr(crate::ui::sp(4.))
                            .rounded(crate::ui::rad(8.))
                            .border_1()
                            .border_color(theme::border())
                            .bg(theme::fill_hover())
                            .text_size(px(crate::ui::TEXT_SM))
                            .child(
                                icon(if review { "comment-add01" } else { "note01" }, 12.)
                                    .text_color(theme::text_subtle()),
                            )
                            .child(
                                div()
                                    .min_w_0()
                                    .truncate()
                                    .text_color(theme::text_muted())
                                    .child(n.label()),
                            )
                            .child(
                                div()
                                    .id(("note-remove", i))
                                    .flex_none()
                                    .size(px(18.))
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .rounded(crate::ui::rad(4.))
                                    .cursor_pointer()
                                    .hover(|s| s.bg(theme::fill_selected()))
                                    .child(icon("cancel01", 11.).text_color(theme::text_subtle()))
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        cx.stop_propagation();
                                        this.remove_note(i, cx);
                                    })),
                            )
                    }),
                ))
                .into_any_element(),
        )
    }

    /// The other sessions' conversations dropped on the composer.
    fn render_context_row(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let list = Some(&self.here().attached).filter(|v| !v.is_empty())?;
        Some(
            div()
                .flex()
                .flex_wrap()
                .gap(crate::ui::sp(6.))
                .px(crate::ui::sp(12.))
                .pt(crate::ui::sp(10.))
                .children(list.iter().enumerate().map(|(i, c)| {
                    div()
                        .id(("context-chip", i))
                        .flex()
                        .items_center()
                        .gap(crate::ui::sp(6.))
                        .h(px(26.))
                        .max_w(px(320.))
                        .pl(crate::ui::sp(8.))
                        .pr(crate::ui::sp(4.))
                        .rounded(crate::ui::rad(8.))
                        .border_1()
                        .border_color(theme::active().opacity(0.3))
                        .bg(theme::active().opacity(0.08))
                        .text_size(px(crate::ui::TEXT_SM))
                        .child(icon("chatting01", 12.).text_color(theme::active()))
                        .child(div().text_color(theme::text_subtle()).child("Context"))
                        .child(
                            div()
                                .min_w_0()
                                .truncate()
                                .text_color(theme::text_muted())
                                .child(c.title.clone()),
                        )
                        .child(
                            div()
                                .text_color(theme::text_subtle())
                                .child(format!("{:.1}k", c.block.len() as f32 / 1000.)),
                        )
                        .child(
                            div()
                                .id(("context-remove", i))
                                .flex_none()
                                .size(px(18.))
                                .flex()
                                .items_center()
                                .justify_center()
                                .rounded(crate::ui::rad(4.))
                                .cursor_pointer()
                                .hover(|s| s.bg(theme::fill_selected()))
                                .child(icon("cancel01", 11.).text_color(theme::text_subtle()))
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    cx.stop_propagation();
                                    let list = &mut this.here_mut().attached;
                                    if i < list.len() {
                                        list.remove(i);
                                    }
                                    cx.notify();
                                })),
                        )
                }))
                .into_any_element(),
        )
    }

    /// The `@`/`/` completion popup, right above the text line.
    fn render_completion(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let comp = self.completion.as_ref()?;
        Some(
            div()
                .id("completion")
                .flex()
                .flex_col()
                .mx(px(8.))
                .mt(px(8.))
                .py(crate::ui::sp(4.))
                .rounded(crate::ui::rad(12.))
                .border_1()
                .border_color(theme::border())
                .bg(theme::fill_hover())
                .children(comp.items.iter().enumerate().map(|(i, item)| {
                    let selected = i == comp.selected;
                    div()
                        .id(("compl", i))
                        .flex()
                        .items_center()
                        .gap(crate::ui::sp(8.))
                        .h(px(26.))
                        .px(crate::ui::sp(10.))
                        .rounded(crate::ui::rad(8.))
                        .cursor_pointer()
                        .when(selected, |d| d.bg(theme::selection()))
                        .child(
                            div()
                                .flex_none()
                                .font_family(theme::FONT_MONO)
                                .text_size(px(ui::TEXT_SM))
                                .text_color(if selected { theme::content() } else { theme::mix(0.75) })
                                .child(item.label.clone()),
                        )
                        .when_some(item.detail.clone(), |d, det| {
                            d.child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .truncate()
                                    .text_size(px(ui::TEXT_XS))
                                    .text_color(theme::text_subtle())
                                    .child(det),
                            )
                        })
                        .on_click(cx.listener(move |this, _, _, cx| {
                            if let Some(c) = &mut this.completion {
                                c.selected = i;
                            }
                            this.accept_completion(cx);
                        }))
                }))
                .when(comp.prompts, |d| {
                    d.child(
                        div()
                            .px(crate::ui::sp(10.))
                            .pt(crate::ui::sp(4.))
                            .pb(crate::ui::sp(2.))
                            .mt(px(2.))
                            .border_t_1()
                            .border_color(theme::border_subtle())
                            .text_size(px(ui::TEXT_XS))
                            .text_color(theme::text_faint())
                            .child(crate::platform::shortcut_label("Enter inserts · Alt+P pins · Alt+D deletes · Ctrl+S saves the draft")),
                    )
                })
                .into_any_element(),
        )
    }

    /// The agent is out of its limits: why, and the other agent when it can run.
    fn render_limits(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let (why, other, other_ok) = self.frame.limits.clone()?;
        let offer = !self.frame.in_session && other_ok;
        Some(
            div()
                .flex()
                .items_center()
                .gap(crate::ui::sp(8.))
                .mx(px(8.))
                .mt(px(8.))
                .px(crate::ui::sp(10.))
                .py(crate::ui::sp(6.))
                .rounded(crate::ui::rad(ui::RADIUS))
                .bg(Hsla::from(theme::attention()).opacity(0.10))
                .text_size(px(ui::TEXT_SM))
                .text_color(theme::text_muted())
                .child(icon("alert-circle", 14.).text_color(theme::attention()))
                .child(div().flex_1().min_w_0().child(why))
                .when(offer, |d| {
                    d.child(
                        ui::button(
                            "composer-switch-agent",
                            format!("Use {}", other.label()),
                            ui::ButtonKind::Outline,
                            true,
                        )
                        .on_click(
                            cx.listener(move |_, _, _, cx| cx.emit(ComposerEvent::UseAgent(other))),
                        ),
                    )
                })
                .into_any_element(),
        )
    }

    /// The draft, or the placeholder, with the caret and the input method's
    /// canvas over it.
    fn render_text(&self, focused: bool, cx: &mut Context<Self>) -> AnyElement {
        let draft = self.here().text.bare();
        let home = self.frame.home;
        // The caret is painted at the end of the laid-out draft rather than typed
        // into it as a glyph, which sat off the last letter by the glyph's side
        // bearings and could wrap onto a line of its own.
        let mut painted: Option<(TextLayout, usize)> = None;
        let mut show_caret = focused;
        if draft.is_empty() {
            self.layout.borrow_mut().take();
        }
        let text: AnyElement = if draft.is_empty() {
            div()
                .relative()
                .text_color(theme::text_faint())
                .child(PLACEHOLDER)
                .into_any_element()
        } else {
            let sel = draft.selection();
            // Highlights may not overlap; while composing, the input method's
            // text is underlined and any selection inside it is its own.
            let highlight = match draft.marked() {
                Some(marked) => Some((
                    marked,
                    HighlightStyle {
                        underline: Some(UnderlineStyle {
                            thickness: px(1.),
                            color: Some(theme::content()),
                            wavy: false,
                        }),
                        ..Default::default()
                    },
                )),
                None => (!sel.is_empty()).then(|| {
                    let alpha = if focused { 0.35 } else { 0.18 };
                    (
                        sel.clone(),
                        HighlightStyle {
                            background_color: Some(gpui::Hsla {
                                a: alpha,
                                ..theme::text_selection()
                            }),
                            ..Default::default()
                        },
                    )
                }),
            };
            let shown = Self::shown_text(&draft, &self.masked_ranges());
            let styled = StyledText::new(shown).with_highlights(highlight);
            painted = Some((styled.layout().clone(), draft.head()));
            show_caret = focused && sel.is_empty();
            styled.into_any_element()
        };
        div()
            .id("composer-text")
            .relative()
            .px(crate::ui::sp(12.))
            .pt(crate::ui::sp(if home { 12. } else { COMPOSER_PAD }))
            .pb(crate::ui::sp(if home { 16. } else { COMPOSER_PAD }))
            .text_size(px(ui::TEXT_LG))
            .line_height(px(22.))
            .max_h(px(160.))
            .overflow_y_scroll()
            .track_scroll(&self.scroll)
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, ev: &MouseDownEvent, window, cx| {
                    window.focus(&this.focus, cx);
                    this.click(ev.position, ev.click_count, ev.modifiers.shift, cx);
                }),
            )
            .on_mouse_move(cx.listener(|this, ev: &MouseMoveEvent, _, cx| {
                if this.selecting && ev.pressed_button == Some(MouseButton::Left) {
                    this.drag_to(ev.position, cx);
                }
            }))
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, _, _, cx| this.release(cx)),
            )
            .on_mouse_up_out(
                MouseButton::Left,
                cx.listener(|this, _, _, cx| this.release(cx)),
            )
            // The caret and input canvas cover the text only: sized to the
            // scroll box itself, the bottom padding would count as overflow
            // and the composer would scroll even when empty.
            .child(
                div()
                    .relative()
                    .child(text)
                    .child(self.caret_canvas(painted, show_caret, cx)),
            )
            .into_any_element()
    }

    /// Hands the text to input methods, keeps its layout for clicks, paints
    /// the caret at `painted`'s head, and scrolls it into view after an edit.
    fn caret_canvas(
        &self,
        painted: Option<(TextLayout, usize)>,
        show_caret: bool,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let entity = cx.entity();
        let focus = self.focus.clone();
        let caret = self.caret.clone();
        let (layout_cell, reveal, scroll) = (
            self.layout.clone(),
            self.reveal.clone(),
            self.scroll.clone(),
        );
        canvas(
            |_, _, _| {},
            move |bounds, _, window, cx| {
                window.handle_input(&focus, ElementInputHandler::new(bounds, entity.clone()), cx);
                // The text is laid out and placed by now: keep it for
                // clicks and the IME, paint the caret, and bring the
                // caret into view after an edit.
                let (at, line) = match &painted {
                    Some((layout, head)) => {
                        *layout_cell.borrow_mut() = Some(layout.clone());
                        let Some(at) = layout.position_for_index(*head) else {
                            caret.borrow_mut().hide();
                            return;
                        };
                        (at, layout.line_height())
                    }
                    // Empty: just before the placeholder.
                    None => (bounds.origin + point(px(-1.), px(0.)), px(PLACEHOLDER_LINE)),
                };
                if show_caret {
                    let (drawn, alpha) = caret.borrow_mut().frame(at - bounds.origin, window, cx);
                    let drawn = bounds.origin + drawn;
                    window.paint_quad(
                        fill(
                            Bounds::new(
                                point(drawn.x, drawn.y + (line - px(CARET_HEIGHT)) / 2.),
                                size(px(CARET_WIDTH), px(CARET_HEIGHT)),
                            ),
                            crate::caret::faded(theme::active(), alpha),
                        )
                        .corner_radii(crate::caret::radius(CARET_WIDTH)),
                    );
                } else {
                    caret.borrow_mut().hide();
                }
                if painted.is_none() || !reveal.take() {
                    return;
                }
                let view = scroll.bounds();
                let (top, bottom) = (
                    view.top() + px(COMPOSER_PAD),
                    view.bottom() - px(COMPOSER_PAD),
                );
                let mut offset = scroll.offset();
                let shift = if at.y < top {
                    top - at.y
                } else if at.y + line > bottom {
                    bottom - (at.y + line)
                } else {
                    px(0.)
                };
                if shift != px(0.) {
                    offset.y = (offset.y + shift).clamp(-scroll.max_offset().y, px(0.));
                    scroll.set_offset(offset);
                    // A refresh asked for while drawing is dropped.
                    window.on_next_frame(|window, _| window.refresh());
                }
            },
        )
        .absolute()
        .top_0()
        .left_0()
        .size_full()
    }

    /// The bar under the text: attach, the agent and permission chips, the
    /// session's usage and the send (or stop) button. `ready` is whether
    /// there is anything to send.
    fn render_bar(&self, ready: bool, cx: &mut Context<Self>) -> AnyElement {
        div()
            .flex()
            // With the chips wrapped, + and send sit on their last line.
            .items_end()
            .gap(crate::ui::sp(4.))
            .px(crate::ui::sp(8.))
            .pb(crate::ui::sp(8.))
            .child(
                // Opens the attach picker; picked files become `@` mentions.
                div()
                    .id("composer-add")
                    .group("composer-add")
                    .flex_none()
                    .flex()
                    .items_center()
                    .justify_center()
                    .size(px(26.))
                    .rounded(ui::nested(ui::RADIUS_XL, 8.))
                    .bg(theme::selection())
                    .cursor_pointer()
                    .hover(|s| s.bg(theme::selection_hover()))
                    .tooltip(ui::tip("Attach files"))
                    .child(hover_icon(
                        "add01",
                        14.,
                        theme::text_subtle(),
                        theme::content(),
                        "composer-add",
                    ))
                    .on_click(cx.listener(|_, _, _, cx| cx.emit(ComposerEvent::AttachFiles))),
            )
            .child(self.render_chips(cx))
            .when_some(self.frame.usage_line.clone(), |d, line| {
                d.child(
                    div()
                        .flex_none()
                        .px(crate::ui::sp(6.))
                        .text_size(px(ui::TEXT_XS))
                        .text_color(theme::text_subtle())
                        .child(line),
                )
            })
            .child(self.render_send(ready, cx))
            .into_any_element()
    }

    /// The agent, model and reasoning chip, the seat and the permissions
    /// chip. Narrow panes wrap them onto a second line rather than push the
    /// send button out of the composer.
    fn render_chips(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let f = &self.frame;
        let agent = f.agent;
        let fast = f.fast;
        div()
            .flex()
            .flex_1()
            .min_w_0()
            .flex_wrap()
            .items_center()
            .gap(crate::ui::sp(4.))
            .child(
                // One chip for the agent, its model and reasoning.
                chip(
                    "composer-model",
                    Some(
                        div()
                            .flex()
                            .items_center()
                            .gap(crate::ui::sp(3.))
                            .child(agent.icon(14.))
                            .when(fast, |d| {
                                d.child(icon("flash", 12.).text_color(theme::text_subtle()))
                            })
                            .into_any_element(),
                    ),
                    f.model_label
                        .clone()
                        .unwrap_or_else(|| agent.label().into()),
                    f.effort_label.clone(),
                    true,
                )
                .tooltip(ui::tip("Agent, model and reasoning"))
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .on_click(cx.listener(|_, ev: &gpui::ClickEvent, _, cx| {
                    cx.emit(ComposerEvent::ModelMenu(ev.position()))
                })),
            )
            .when_some(f.seat.clone(), |d, seat| {
                d.child(
                    div()
                        .id("composer-seat")
                        .flex_none()
                        .flex()
                        .items_center()
                        .gap(crate::ui::sp(4.))
                        .px(crate::ui::sp(6.))
                        .text_size(px(ui::TEXT_XS))
                        .text_color(theme::text_subtle())
                        .child(icon("user", 11.).text_color(theme::text_faint()))
                        .child(seat)
                        .tooltip(ui::tip(
                            "The Claude seat this session runs on, for its whole life",
                        )),
                )
            })
            .when_some(f.permissions.clone(), |d, (label, risky)| {
                d.child(
                    chip(
                        "composer-permissions",
                        Some(
                            icon(
                                if risky {
                                    "shield-alert"
                                } else {
                                    "square-lock02"
                                },
                                12.,
                            )
                            .text_color(if risky {
                                theme::warn().into()
                            } else {
                                theme::mix(0.5)
                            })
                            .into_any_element(),
                        ),
                        label,
                        None,
                        true,
                    )
                    .tooltip(ui::tip("Permissions · Shift+Tab to switch"))
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .on_click(cx.listener(
                        |_, ev: &gpui::ClickEvent, _, cx| {
                            cx.emit(ComposerEvent::PermissionMenu(ev.position()))
                        },
                    )),
                )
            })
    }

    /// Sends what is written, or stops the running turn when nothing is.
    fn render_send(&self, ready: bool, cx: &mut Context<Self>) -> impl IntoElement {
        let stop = self.frame.busy && !ready;
        let lit = ready || stop;
        div()
            .id("composer-send")
            .flex_none()
            .flex()
            .items_center()
            .justify_center()
            .size(px(26.))
            .rounded(ui::nested(ui::RADIUS_XL, 8.))
            .bg(if lit { gpui::white() } else { theme::mix(0.14) })
            .when(lit, |d| {
                d.cursor_pointer()
                    .hover(|s| s.bg(gpui::white().opacity(0.9)))
            })
            .child(
                icon(
                    if stop { "square" } else { "arrow-up01" },
                    if stop { 11. } else { 14. },
                )
                .text_color(if lit { gpui::black() } else { theme::mix(0.5) }),
            )
            .on_click(cx.listener(move |_, _, _, cx| {
                cx.emit(if stop {
                    ComposerEvent::Stop
                } else {
                    ComposerEvent::Submit { now: false }
                })
            }))
    }
}
