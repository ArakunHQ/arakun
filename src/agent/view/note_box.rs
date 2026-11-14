//! Notes on selected transcript text: the menu over a selection and the box to write the note.

use super::*;

impl ThreadView {
    /// Opens the note box for the current selection.
    pub(super) fn start_note(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let (Some(sel), Some(at)) = (self.selection.as_ref(), self.menu_at) else {
            return;
        };
        let Some(quote) = self.selected_text() else {
            return;
        };
        let (surface, range) = (sel.surface, sel.range());
        let input = cx.new(|cx| TextInput::new("Add a note (optional), Enter adds it", cx));
        let sub = cx.subscribe_in(
            &input,
            window,
            |this, _, ev: &InputEvent, window, cx| match ev {
                InputEvent::Submit | InputEvent::SubmitAlt => this.finish_note(true, window, cx),
                InputEvent::Cancel => this.finish_note(false, window, cx),
                _ => {}
            },
        );
        window.focus(&input.focus_handle(cx), cx);
        self.note = Some(NoteBox {
            surface,
            range,
            quote,
            input,
            at,
            _sub: sub,
        });
        self.menu_at = None;
        cx.notify();
    }

    pub(super) fn finish_note(&mut self, add: bool, window: &mut Window, cx: &mut Context<Self>) {
        let Some(n) = self.note.take() else {
            return;
        };
        if add {
            let note = n.input.read(cx).text().trim().to_string();
            cx.emit(ThreadViewEvent::AddNote {
                surface: n.surface,
                range: n.range,
                quote: n.quote,
                note,
            });
            self.clear_selection();
        } else {
            window.focus(&self.focus, cx);
        }
        cx.notify();
    }

    pub(super) fn render_selection_menu(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let at = self.menu_at?;
        self.selection
            .as_ref()
            .filter(|s| !s.dragging && !s.range().is_empty())?;
        let button = |id: &'static str, icon_name: &'static str, label: &'static str| {
            div()
                .id(id)
                .flex()
                .items_center()
                .gap(px(6.))
                .h(px(26.))
                .px(px(8.))
                .rounded(px(8.))
                .cursor_pointer()
                .text_size(px(crate::ui::TEXT_SM))
                .text_color(theme::text_muted())
                .hover(|s| s.bg(theme::fill_selected()))
                .child(icon(icon_name, 13.).text_color(theme::text_subtle()))
                .child(label)
        };
        Some(
            deferred(
                anchored()
                    .position(point(at.x, at.y - px(10.)))
                    .anchor(Anchor::BottomLeft)
                    .snap_to_window_with_margin(px(8.))
                    .child(
                        div()
                            .id("selection-menu")
                            .occlude()
                            .flex()
                            .items_center()
                            .gap(px(2.))
                            .p(px(3.))
                            .rounded(px(12.))
                            .border_1()
                            .border_color(theme::border_strong())
                            .bg(theme::dialog())
                            .shadow_lg()
                            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                            .child(button("sel-copy", "copy01", "Copy").on_click(cx.listener(
                                |this, _, _, cx| {
                                    if let Some(text) = this.selected_text() {
                                        cx.write_to_clipboard(ClipboardItem::new_string(text));
                                    }
                                    this.clear_selection();
                                    cx.notify();
                                },
                            )))
                            .child(button("sel-note", "comment-add01", "Add to chat").on_click(
                                cx.listener(|this, _, window, cx| this.start_note(window, cx)),
                            )),
                    ),
            )
            .with_priority(1)
            .into_any_element(),
        )
    }

    pub(super) fn render_note_box(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let n = self.note.as_ref()?;
        let preview: String = n.quote.lines().take(3).collect::<Vec<_>>().join("\n");
        Some(
            deferred(
                anchored()
                    .position(point(n.at.x - px(16.), n.at.y + px(12.)))
                    .snap_to_window_with_margin(px(8.))
                    .child(
                        div()
                            .id("note-box")
                            .occlude()
                            .w(px(340.))
                            .flex()
                            .flex_col()
                            .gap(px(8.))
                            .p(px(10.))
                            .rounded(px(crate::ui::RADIUS_LG))
                            .border_1()
                            .border_color(theme::border_strong())
                            .bg(theme::dialog())
                            .shadow_lg()
                            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                            .child(
                                div()
                                    .flex()
                                    .gap(px(8.))
                                    .child(
                                        div()
                                            .w(px(2.))
                                            .flex_none()
                                            .rounded(px(4.))
                                            .bg(crate::agent::selection::mark_color()),
                                    )
                                    .child(
                                        div()
                                            .flex_1()
                                            .min_w_0()
                                            .max_h(px(60.))
                                            .overflow_hidden()
                                            .text_size(px(crate::ui::TEXT_SM))
                                            .text_color(theme::text_subtle())
                                            .child(preview),
                                    ),
                            )
                            .child(
                                div()
                                    .h(px(crate::ui::INPUT_H))
                                    .flex()
                                    .items_center()
                                    .px(px(8.))
                                    .rounded(px(8.))
                                    .border_1()
                                    .border_color(theme::border_strong())
                                    .bg(theme::fill_raised())
                                    .text_size(px(crate::ui::TEXT_MD))
                                    .child(n.input.clone()),
                            )
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap(px(6.))
                                    .child(
                                        div()
                                            .flex_1()
                                            .text_size(px(crate::ui::TEXT_XS))
                                            .text_color(theme::text_faint())
                                            .child("Sent with your next message · Esc cancels"),
                                    )
                                    .child(
                                        div()
                                            .id("note-add")
                                            .px(px(10.))
                                            .py(px(4.))
                                            .rounded(px(8.))
                                            .cursor_pointer()
                                            .bg(theme::content())
                                            .text_color(theme::background())
                                            .text_size(px(crate::ui::TEXT_SM))
                                            .font_weight(FontWeight::MEDIUM)
                                            .child("Add")
                                            .on_click(cx.listener(|this, _, window, cx| {
                                                this.finish_note(true, window, cx)
                                            })),
                                    ),
                            ),
                    ),
            )
            .with_priority(1)
            .into_any_element(),
        )
    }
}
