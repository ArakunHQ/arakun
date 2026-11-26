//! The composer and input methods, which speak UTF-16 offsets.

use super::*;

impl EntityInputHandler for Composer {
    fn text_for_range(
        &mut self,
        range_utf16: Range<usize>,
        actual_range: &mut Option<Range<usize>>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<String> {
        let draft = &self.here().text;
        let (a, b) = (
            draft.utf16_to_byte(range_utf16.start),
            draft.utf16_to_byte(range_utf16.end),
        );
        let range = a.min(b)..a.max(b);
        actual_range.replace(draft.byte_to_utf16(range.start)..draft.byte_to_utf16(range.end));
        Some(draft[range].to_string())
    }

    fn selected_text_range(
        &mut self,
        _: bool,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<UTF16Selection> {
        let draft = &self.here().text;
        let sel = draft.selection();
        Some(UTF16Selection {
            range: draft.byte_to_utf16(sel.start)..draft.byte_to_utf16(sel.end),
            reversed: draft.reversed(),
        })
    }

    fn marked_text_range(&self, _: &mut Window, _: &mut Context<Self>) -> Option<Range<usize>> {
        let draft = &self.here().text;
        draft
            .marked()
            .map(|m| draft.byte_to_utf16(m.start)..draft.byte_to_utf16(m.end))
    }

    fn unmark_text(&mut self, _: &mut Window, cx: &mut Context<Self>) {
        self.here_mut().text.unmark();
        cx.notify();
    }

    fn replace_text_in_range(
        &mut self,
        range_utf16: Option<Range<usize>>,
        text: &str,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let draft = &mut self.here_mut().text;
        match range_utf16 {
            Some(r) => {
                let range = draft.utf16_to_byte(r.start)..draft.utf16_to_byte(r.end);
                draft.replace(range, text);
            }
            None => draft.insert(text),
        }
        self.goal_x = None;
        self.changed(cx);
        self.update_completion(cx);
        cx.notify();
    }

    fn replace_and_mark_text_in_range(
        &mut self,
        range_utf16: Option<Range<usize>>,
        new_text: &str,
        new_selected_range_utf16: Option<Range<usize>>,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let draft = &mut self.here_mut().text;
        let range = range_utf16.map(|r| draft.utf16_to_byte(r.start)..draft.utf16_to_byte(r.end));
        let selected = new_selected_range_utf16.map(|r| {
            crate::draft::byte_of_utf16(new_text, r.start)
                ..crate::draft::byte_of_utf16(new_text, r.end)
        });
        draft.compose(range, new_text, selected);
        self.changed(cx);
        cx.notify();
    }

    /// Where the input method's candidate window goes: at the caret.
    fn bounds_for_range(
        &mut self,
        range_utf16: Range<usize>,
        element_bounds: Bounds<Pixels>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        let layout = self.layout.borrow().clone();
        let ix = self.here().text.utf16_to_byte(range_utf16.start);
        Some(
            match layout
                .as_ref()
                .and_then(|l| Some((l.position_for_index(ix)?, l.line_height())))
            {
                Some((at, line)) => Bounds::new(at, size(px(view::CARET_WIDTH), line)),
                None => element_bounds,
            },
        )
    }

    fn character_index_for_point(
        &mut self,
        point: gpui::Point<Pixels>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<usize> {
        let ix = self.index_at(point)?;
        Some(self.here().text.byte_to_utf16(ix))
    }
}
