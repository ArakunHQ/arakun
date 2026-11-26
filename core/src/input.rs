//! Text field, adapted from GPUI's `examples/input.rs`: IME-aware via
//! `EntityInputHandler`, mouse selection, clipboard, word motions. Owners listen to
//! `InputEvent` for edits, Enter, Escape and list navigation keys.
//!
//! One line by default. `multiline` makes it wrap to its width and grow
//! between a least and a most number of rows, then scroll; Enter then types a
//! newline, and Ctrl+Enter (Cmd+Enter on macOS) is the only Submit.

use std::ops::Range;

use gpui::{
    App, Bounds, ClipboardItem, Context, CursorStyle, ElementId, ElementInputHandler, Entity,
    EntityInputHandler, EventEmitter, FocusHandle, Focusable, GlobalElementId, KeyBinding,
    LayoutId, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, PaintQuad, Pixels, Point,
    ShapedLine, SharedString, Style, TextRun, UTF16Selection, UnderlineStyle, Window, WrappedLine,
    actions, div, fill, point, prelude::*, px, relative, size,
};
use unicode_segmentation::UnicodeSegmentation as _;

use crate::theme;

actions!(
    text_input,
    [
        Backspace,
        BackspaceWord,
        Delete,
        Left,
        Right,
        WordLeft,
        WordRight,
        SelectLeft,
        SelectRight,
        SelectAll,
        Home,
        End,
        Paste,
        Cut,
        Copy,
        Submit,
        SubmitAlt,
        Cancel,
        Up,
        Down,
        Complete,
    ]
);

pub fn bind_keys(cx: &mut App) {
    use crate::platform::keystroke;
    let ctx = Some("TextInput");
    // Option moves by word on macOS, and Cmd+arrows go to the ends.
    let word = if cfg!(target_os = "macos") {
        "alt"
    } else {
        "ctrl"
    };
    if cfg!(target_os = "macos") {
        cx.bind_keys([
            KeyBinding::new("cmd-left", Home, ctx),
            KeyBinding::new("cmd-right", End, ctx),
        ]);
    }
    cx.bind_keys([
        KeyBinding::new("backspace", Backspace, ctx),
        KeyBinding::new(
            crate::platform::static_keys(&format!("{word}-backspace")),
            BackspaceWord,
            ctx,
        ),
        KeyBinding::new("delete", Delete, ctx),
        KeyBinding::new("left", Left, ctx),
        KeyBinding::new("right", Right, ctx),
        KeyBinding::new(
            crate::platform::static_keys(&format!("{word}-left")),
            WordLeft,
            ctx,
        ),
        KeyBinding::new(
            crate::platform::static_keys(&format!("{word}-right")),
            WordRight,
            ctx,
        ),
        KeyBinding::new("shift-left", SelectLeft, ctx),
        KeyBinding::new("shift-right", SelectRight, ctx),
        KeyBinding::new(
            crate::platform::static_keys(&keystroke("ctrl-a")),
            SelectAll,
            ctx,
        ),
        KeyBinding::new("home", Home, ctx),
        KeyBinding::new("end", End, ctx),
        KeyBinding::new(
            crate::platform::static_keys(&keystroke("ctrl-v")),
            Paste,
            ctx,
        ),
        KeyBinding::new(
            crate::platform::static_keys(&keystroke("ctrl-c")),
            Copy,
            ctx,
        ),
        KeyBinding::new(crate::platform::static_keys(&keystroke("ctrl-x")), Cut, ctx),
        KeyBinding::new("enter", Submit, ctx),
        KeyBinding::new(
            crate::platform::static_keys(&keystroke("ctrl-enter")),
            SubmitAlt,
            ctx,
        ),
        KeyBinding::new("escape", Cancel, ctx),
        KeyBinding::new("up", Up, ctx),
        KeyBinding::new("down", Down, ctx),
        KeyBinding::new("tab", Complete, ctx),
    ]);
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InputEvent {
    Changed,
    Submit,
    /// Ctrl+Enter.
    SubmitAlt,
    Cancel,
    Up,
    Down,
    /// Tab.
    Complete,
}

pub struct TextInput {
    focus_handle: FocusHandle,
    content: SharedString,
    placeholder: SharedString,
    mono: bool,
    /// Draws every character as `*` and keeps the text off the clipboard.
    secret: bool,
    /// A secret drawn as typed, for the user to check it. Still off the
    /// clipboard, and masked while redacted mode is on.
    revealed: bool,
    /// Draws every character as `*` while redacted mode is on, for what is
    /// private but fine to copy: a pairing link, an email.
    redacted: bool,
    /// A caret of its own color, drawn 2 px wide (the palette's accent caret).
    caret: Option<gpui::Hsla>,
    /// How the caret glides and blinks.
    motion: crate::caret::Caret,
    selected_range: Range<usize>,
    selection_reversed: bool,
    marked_range: Option<Range<usize>>,
    last_layout: Option<ShapedLine>,
    last_bounds: Option<Bounds<Pixels>>,
    /// How far long content is shifted left to keep the caret in view.
    scroll_x: Pixels,
    is_selecting: bool,
    /// The least and most rows shown; `None` is one line.
    multiline: Option<(usize, usize)>,
    /// The wrapped text as last painted, in multiline mode.
    last_wrapped: Option<Wrapped>,
    /// How far multiline content is shifted up to keep the caret in view.
    scroll_y: Pixels,
}

impl EventEmitter<InputEvent> for TextInput {}

impl TextInput {
    pub fn new(placeholder: impl Into<SharedString>, cx: &mut Context<Self>) -> Self {
        Self {
            focus_handle: cx.focus_handle(),
            content: SharedString::default(),
            placeholder: placeholder.into(),
            mono: false,
            secret: false,
            revealed: false,
            redacted: false,
            caret: None,
            motion: Default::default(),
            selected_range: 0..0,
            selection_reversed: false,
            marked_range: None,
            last_layout: None,
            last_bounds: None,
            scroll_x: px(0.),
            is_selecting: false,
            multiline: None,
            last_wrapped: None,
            scroll_y: px(0.),
        }
    }

    /// Wraps to the field's width and shows `min` to `max` rows, then
    /// scrolls. Enter types a newline; Ctrl+Enter submits (`SubmitAlt`).
    pub fn multiline(mut self, min: usize, max: usize) -> Self {
        let min = min.max(1);
        self.multiline = Some((min, max.max(min)));
        self
    }

    pub fn mono(mut self) -> Self {
        self.mono = true;
        self
    }

    pub fn secret(mut self) -> Self {
        self.secret = true;
        self
    }

    /// Shows a secret's text as typed, or masks it again.
    pub fn set_revealed(&mut self, revealed: bool, cx: &mut Context<Self>) {
        if self.revealed != revealed {
            self.revealed = revealed;
            cx.notify();
        }
    }

    pub fn redacted(mut self) -> Self {
        self.redacted = true;
        self
    }

    pub fn text(&self) -> &str {
        &self.content
    }

    /// What shows while it is empty.
    pub fn set_placeholder(&mut self, text: impl Into<SharedString>, cx: &mut Context<Self>) {
        let text = text.into();
        if self.placeholder != text {
            self.placeholder = text;
            cx.notify();
        }
    }

    /// A 2 px caret in `color` instead of the thin one in the text color.
    pub fn caret_color(mut self, color: gpui::Hsla) -> Self {
        self.caret = Some(color);
        self
    }

    /// Whether the caret sits after the last character with nothing selected.
    pub fn caret_at_end(&self) -> bool {
        self.selected_range.is_empty() && self.cursor_offset() == self.content.len()
    }

    pub fn has_selection(&self) -> bool {
        !self.selected_range.is_empty()
    }

    /// Types `text` at the caret, over any selection.
    pub fn insert(&mut self, text: &str, cx: &mut Context<Self>) {
        let range = self.selected_range.clone();
        self.content =
            (self.content[..range.start].to_owned() + text + &self.content[range.end..]).into();
        let at = range.start + text.len();
        self.selected_range = at..at;
        self.selection_reversed = false;
        self.marked_range = None;
        self.motion.hold();
        cx.emit(InputEvent::Changed);
        cx.notify();
    }

    /// Empties the input without a Changed event, for callers resetting it
    /// rather than answering what the user typed.
    pub fn clear(&mut self, cx: &mut Context<Self>) {
        self.content = "".into();
        self.selected_range = 0..0;
        self.selection_reversed = false;
        self.marked_range = None;
        self.motion.hold();
        cx.notify();
    }

    /// Replaces the content and puts the caret at the end.
    pub fn set_text(&mut self, text: impl Into<SharedString>, cx: &mut Context<Self>) {
        self.content = text.into();
        let end = self.content.len();
        self.selected_range = end..end;
        self.selection_reversed = false;
        self.marked_range = None;
        self.motion.hold();
        cx.emit(InputEvent::Changed);
        cx.notify();
    }

    pub fn select_all_text(&mut self, cx: &mut Context<Self>) {
        self.selected_range = 0..self.content.len();
        self.selection_reversed = false;
        cx.notify();
    }

    fn left(&mut self, _: &Left, _: &mut Window, cx: &mut Context<Self>) {
        if self.selected_range.is_empty() {
            self.move_to(self.previous_boundary(self.cursor_offset()), cx);
        } else {
            self.move_to(self.selected_range.start, cx)
        }
    }

    fn right(&mut self, _: &Right, _: &mut Window, cx: &mut Context<Self>) {
        if self.selected_range.is_empty() {
            self.move_to(self.next_boundary(self.selected_range.end), cx);
        } else {
            self.move_to(self.selected_range.end, cx)
        }
    }

    fn word_left(&mut self, _: &WordLeft, _: &mut Window, cx: &mut Context<Self>) {
        self.move_to(self.previous_word(self.cursor_offset()), cx);
    }

    fn word_right(&mut self, _: &WordRight, _: &mut Window, cx: &mut Context<Self>) {
        self.move_to(self.next_word(self.cursor_offset()), cx);
    }

    fn select_left(&mut self, _: &SelectLeft, _: &mut Window, cx: &mut Context<Self>) {
        self.select_to(self.previous_boundary(self.cursor_offset()), cx);
    }

    fn select_right(&mut self, _: &SelectRight, _: &mut Window, cx: &mut Context<Self>) {
        self.select_to(self.next_boundary(self.cursor_offset()), cx);
    }

    fn select_all(&mut self, _: &SelectAll, _: &mut Window, cx: &mut Context<Self>) {
        self.move_to(0, cx);
        self.select_to(self.content.len(), cx)
    }

    fn home(&mut self, _: &Home, _: &mut Window, cx: &mut Context<Self>) {
        let to = match (&self.multiline, &self.last_wrapped) {
            // No rows only if shaping failed; stay put rather than panic.
            (Some(_), Some(w)) => w
                .rows
                .get(w.row_of(self.cursor_offset()))
                .map_or(self.cursor_offset(), |r| r.start),
            (Some(_), None) => line_start(&self.content, self.cursor_offset()),
            (None, _) => 0,
        };
        self.move_to(to, cx);
    }

    fn end(&mut self, _: &End, _: &mut Window, cx: &mut Context<Self>) {
        let to = match (&self.multiline, &self.last_wrapped) {
            (Some(_), Some(w)) => row_end(&w.rows, &self.content, w.row_of(self.cursor_offset())),
            (Some(_), None) => line_end(&self.content, self.cursor_offset()),
            (None, _) => self.content.len(),
        };
        self.move_to(to, cx);
    }

    /// Enter: a newline in multiline mode, else Submit.
    fn enter(&mut self, _: &Submit, window: &mut Window, cx: &mut Context<Self>) {
        match entered(self.multiline.is_some()) {
            Some(text) => self.replace_text_in_range(None, text, window, cx),
            None => cx.emit(InputEvent::Submit),
        }
    }

    /// Up and Down: a row up or down in multiline mode, else the owner's.
    fn vertical(&mut self, by: isize, cx: &mut Context<Self>) {
        let (Some(_), Some(w)) = (&self.multiline, &self.last_wrapped) else {
            return cx.emit(if by < 0 {
                InputEvent::Up
            } else {
                InputEvent::Down
            });
        };
        let at = self.cursor_offset();
        let row = w.row_of(at);
        let to = match row.checked_add_signed(by).filter(|r| *r < w.rows.len()) {
            Some(r) => {
                let x = w.position(at).x;
                w.index_at(point(x, w.line_height * r as f32 + w.line_height / 2.))
            }
            None if by < 0 => 0,
            None => self.content.len(),
        };
        self.move_to(to, cx);
    }

    fn backspace(&mut self, _: &Backspace, window: &mut Window, cx: &mut Context<Self>) {
        if self.selected_range.is_empty() {
            let prev = self.previous_boundary(self.cursor_offset());
            if self.cursor_offset() == prev {
                return;
            }
            self.select_to(prev, cx)
        }
        self.replace_text_in_range(None, "", window, cx)
    }

    fn backspace_word(&mut self, _: &BackspaceWord, window: &mut Window, cx: &mut Context<Self>) {
        if self.selected_range.is_empty() {
            let prev = self.previous_word(self.cursor_offset());
            if self.cursor_offset() == prev {
                return;
            }
            self.select_to(prev, cx)
        }
        self.replace_text_in_range(None, "", window, cx)
    }

    fn delete(&mut self, _: &Delete, window: &mut Window, cx: &mut Context<Self>) {
        if self.selected_range.is_empty() {
            let next = self.next_boundary(self.cursor_offset());
            if self.cursor_offset() == next {
                return;
            }
            self.select_to(next, cx)
        }
        self.replace_text_in_range(None, "", window, cx)
    }

    fn on_mouse_down(
        &mut self,
        event: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        window.focus(&self.focus_handle, cx);
        self.is_selecting = true;
        if event.modifiers.shift {
            self.select_to(self.index_for_mouse_position(event.position), cx);
        } else {
            self.move_to(self.index_for_mouse_position(event.position), cx)
        }
    }

    fn on_mouse_up(&mut self, _: &MouseUpEvent, _: &mut Window, _: &mut Context<Self>) {
        self.is_selecting = false;
    }

    fn on_mouse_move(&mut self, event: &MouseMoveEvent, _: &mut Window, cx: &mut Context<Self>) {
        if self.is_selecting {
            self.select_to(self.index_for_mouse_position(event.position), cx);
        }
    }

    fn paste(&mut self, _: &Paste, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) {
            self.replace_text_in_range(None, &pasted(&text, self.multiline.is_some()), window, cx);
        }
    }

    fn copy(&mut self, _: &Copy, _: &mut Window, cx: &mut Context<Self>) {
        if !self.selected_range.is_empty() && !self.secret {
            cx.write_to_clipboard(ClipboardItem::new_string(
                self.content[self.selected_range.clone()].to_string(),
            ));
        }
    }

    fn cut(&mut self, _: &Cut, window: &mut Window, cx: &mut Context<Self>) {
        if !self.selected_range.is_empty() && !self.secret {
            cx.write_to_clipboard(ClipboardItem::new_string(
                self.content[self.selected_range.clone()].to_string(),
            ));
            self.replace_text_in_range(None, "", window, cx)
        }
    }

    fn move_to(&mut self, offset: usize, cx: &mut Context<Self>) {
        self.selected_range = offset..offset;
        cx.notify()
    }

    fn cursor_offset(&self) -> usize {
        if self.selection_reversed {
            self.selected_range.start
        } else {
            self.selected_range.end
        }
    }

    fn index_for_mouse_position(&self, position: Point<Pixels>) -> usize {
        if self.content.is_empty() {
            return 0;
        }
        if let (Some(_), Some(w), Some(bounds)) =
            (&self.multiline, &self.last_wrapped, &self.last_bounds)
        {
            return w.index_at(position - bounds.origin);
        }
        let (Some(bounds), Some(line)) = (self.last_bounds.as_ref(), self.last_layout.as_ref())
        else {
            return 0;
        };
        if position.y < bounds.top() {
            return 0;
        }
        if position.y > bounds.bottom() {
            return self.content.len();
        }
        line.closest_index_for_x(position.x - bounds.left())
    }

    fn select_to(&mut self, offset: usize, cx: &mut Context<Self>) {
        if self.selection_reversed {
            self.selected_range.start = offset
        } else {
            self.selected_range.end = offset
        };
        if self.selected_range.end < self.selected_range.start {
            self.selection_reversed = !self.selection_reversed;
            self.selected_range = self.selected_range.end..self.selected_range.start;
        }
        cx.notify()
    }

    fn offset_from_utf16(&self, offset: usize) -> usize {
        let mut utf8_offset = 0;
        let mut utf16_count = 0;
        for ch in self.content.chars() {
            if utf16_count >= offset {
                break;
            }
            utf16_count += ch.len_utf16();
            utf8_offset += ch.len_utf8();
        }
        utf8_offset
    }

    fn offset_to_utf16(&self, offset: usize) -> usize {
        let mut utf16_offset = 0;
        let mut utf8_count = 0;
        for ch in self.content.chars() {
            if utf8_count >= offset {
                break;
            }
            utf8_count += ch.len_utf8();
            utf16_offset += ch.len_utf16();
        }
        utf16_offset
    }

    fn range_to_utf16(&self, range: &Range<usize>) -> Range<usize> {
        self.offset_to_utf16(range.start)..self.offset_to_utf16(range.end)
    }

    fn range_from_utf16(&self, range_utf16: &Range<usize>) -> Range<usize> {
        self.offset_from_utf16(range_utf16.start)..self.offset_from_utf16(range_utf16.end)
    }

    fn previous_boundary(&self, offset: usize) -> usize {
        self.content
            .grapheme_indices(true)
            .rev()
            .find_map(|(idx, _)| (idx < offset).then_some(idx))
            .unwrap_or(0)
    }

    fn next_boundary(&self, offset: usize) -> usize {
        self.content
            .grapheme_indices(true)
            .find_map(|(idx, _)| (idx > offset).then_some(idx))
            .unwrap_or(self.content.len())
    }

    /// Word motions stop at `/` too, so Ctrl+Backspace in a path removes one segment.
    fn previous_word(&self, offset: usize) -> usize {
        let text = &self.content[..offset];
        let trimmed = text.trim_end_matches(|c: char| c.is_whitespace() || c == '/');
        trimmed
            .rfind(|c: char| c.is_whitespace() || c == '/')
            .map_or(0, |i| i + 1)
    }

    fn next_word(&self, offset: usize) -> usize {
        let rest = &self.content[offset..];
        let skip = rest.len()
            - rest
                .trim_start_matches(|c: char| c.is_whitespace() || c == '/')
                .len();
        let after = &rest[skip..];
        let word = after
            .find(|c: char| c.is_whitespace() || c == '/')
            .unwrap_or(after.len());
        offset + skip + word
    }
}

/// The selection after an input method puts `text` at byte `start`. The input method
/// counts `selected` in UTF-16 units from the start of `text`, not of the whole field.
// The examples build this file without the app's `draft` module, so it keeps its own copy.
fn composed_selection(start: usize, text: &str, selected: Option<Range<usize>>) -> Range<usize> {
    let byte = |ix: usize| {
        let mut units = 0;
        for (i, c) in text.char_indices() {
            if units >= ix {
                return i;
            }
            units += c.len_utf16();
        }
        text.len()
    };
    let end = start + text.len();
    selected.map_or(end..end, |r| start + byte(r.start)..start + byte(r.end))
}

impl EntityInputHandler for TextInput {
    fn text_for_range(
        &mut self,
        range_utf16: Range<usize>,
        actual_range: &mut Option<Range<usize>>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<String> {
        let range = self.range_from_utf16(&range_utf16);
        actual_range.replace(self.range_to_utf16(&range));
        Some(self.content[range].to_string())
    }

    fn selected_text_range(
        &mut self,
        _: bool,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<UTF16Selection> {
        Some(UTF16Selection {
            range: self.range_to_utf16(&self.selected_range),
            reversed: self.selection_reversed,
        })
    }

    fn marked_text_range(&self, _: &mut Window, _: &mut Context<Self>) -> Option<Range<usize>> {
        self.marked_range
            .as_ref()
            .map(|range| self.range_to_utf16(range))
    }

    fn unmark_text(&mut self, _: &mut Window, _: &mut Context<Self>) {
        self.marked_range = None;
    }

    fn replace_text_in_range(
        &mut self,
        range_utf16: Option<Range<usize>>,
        new_text: &str,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let range = range_utf16
            .as_ref()
            .map(|r| self.range_from_utf16(r))
            .or(self.marked_range.clone())
            .unwrap_or(self.selected_range.clone());
        self.content =
            (self.content[0..range.start].to_owned() + new_text + &self.content[range.end..])
                .into();
        self.selected_range = range.start + new_text.len()..range.start + new_text.len();
        self.marked_range.take();
        self.motion.hold();
        cx.emit(InputEvent::Changed);
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
        let range = range_utf16
            .as_ref()
            .map(|r| self.range_from_utf16(r))
            .or(self.marked_range.clone())
            .unwrap_or(self.selected_range.clone());
        self.content =
            (self.content[0..range.start].to_owned() + new_text + &self.content[range.end..])
                .into();
        self.marked_range =
            (!new_text.is_empty()).then(|| range.start..range.start + new_text.len());
        self.selected_range = composed_selection(range.start, new_text, new_selected_range_utf16);
        cx.emit(InputEvent::Changed);
        cx.notify();
    }

    fn bounds_for_range(
        &mut self,
        range_utf16: Range<usize>,
        bounds: Bounds<Pixels>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        let range = self.range_from_utf16(&range_utf16);
        if let (Some(_), Some(w), Some(text)) =
            (&self.multiline, &self.last_wrapped, &self.last_bounds)
        {
            let start = text.origin + w.position(range.start);
            let end = text.origin + w.position(range.end);
            return Some(Bounds::from_corners(
                start,
                point(end.x.max(start.x), end.y + w.line_height),
            ));
        }
        let last_layout = self.last_layout.as_ref()?;
        Some(Bounds::from_corners(
            point(
                bounds.left() + last_layout.x_for_index(range.start),
                bounds.top(),
            ),
            point(
                bounds.left() + last_layout.x_for_index(range.end),
                bounds.bottom(),
            ),
        ))
    }

    fn character_index_for_point(
        &mut self,
        point: Point<Pixels>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<usize> {
        if let (Some(_), Some(w), Some(text)) =
            (&self.multiline, &self.last_wrapped, &self.last_bounds)
        {
            return Some(self.offset_to_utf16(w.index_at(point - text.origin)));
        }
        let line_point = self.last_bounds?.localize(&point)?;
        let last_layout = self.last_layout.as_ref()?;
        let utf8_index = last_layout.index_for_x(point.x - line_point.x)?;
        Some(self.offset_to_utf16(utf8_index))
    }
}

struct TextElement {
    input: Entity<TextInput>,
}

struct PrepaintState {
    line: Option<ShapedLine>,
    /// Where the caret belongs along the line, when nothing is selected.
    cursor: Option<Pixels>,
    selection: Option<PaintQuad>,
    /// Horizontal shift that keeps the caret visible in long content.
    scroll: Pixels,
    /// Multiline mode: the wrapped text, the caret's point in it, one quad
    /// per selected row, and the vertical shift that keeps the caret in view.
    wrapped: Option<Wrapped>,
    cursor_at: Option<Point<Pixels>>,
    selections: Vec<PaintQuad>,
    scroll_y: Pixels,
}

impl IntoElement for TextElement {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl TextInput {
    /// What the field shows, and its runs: the placeholder when empty,
    /// stars when masked, the marked text underlined.
    fn display(&self, window: &Window) -> (SharedString, Vec<TextRun>) {
        let style = window.text_style();
        let content = self.content.clone();
        let (display_text, text_color) = if content.is_empty() {
            (self.placeholder.clone(), theme::text_subtle())
        } else if (self.secret && (!self.revealed || crate::redact::on()))
            || (self.redacted && crate::redact::on())
        {
            // Same byte length per char, so offsets into `content` still index the line.
            let masked: String = content.chars().map(|c| "*".repeat(c.len_utf8())).collect();
            (masked.into(), style.color)
        } else {
            (content, style.color)
        };
        let run = TextRun {
            len: display_text.len(),
            font: style.font(),
            color: text_color,
            background_color: None,
            underline: None,
            strikethrough: None,
        };
        let runs = match self.marked_range.as_ref() {
            Some(marked) => vec![
                TextRun {
                    len: marked.start,
                    ..run.clone()
                },
                TextRun {
                    len: marked.end - marked.start,
                    underline: Some(UnderlineStyle {
                        color: Some(run.color),
                        thickness: px(1.0),
                        wavy: false,
                    }),
                    ..run.clone()
                },
                TextRun {
                    len: display_text.len() - marked.end,
                    ..run
                },
            ]
            .into_iter()
            .filter(|run| run.len > 0)
            .collect(),
            None => vec![run],
        };
        (display_text, runs)
    }
}

/// Shapes `text` wrapped to `width`.
fn wrap(
    text: SharedString,
    runs: &[TextRun],
    font_size: Pixels,
    width: Pixels,
    line_height: Pixels,
    window: &Window,
) -> Wrapped {
    let lines = window
        .text_system()
        .shape_text(text, font_size, runs, Some(width), None)
        .map(|l| l.into_iter().collect())
        .unwrap_or_default();
    Wrapped::new(lines, line_height)
}

impl Element for TextElement {
    type RequestLayoutState = ();
    type PrepaintState = PrepaintState;

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&gpui::InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, Self::RequestLayoutState) {
        let mut style = Style::default();
        style.size.width = relative(1.).into();
        let input = self.input.read(cx);
        let Some((min, max)) = input.multiline else {
            style.size.height = window.line_height().into();
            return (window.request_layout(style, [], cx), ());
        };
        // As tall as the wrapped text, between `min` and `max` rows.
        let (text, runs) = input.display(window);
        let line_height = window.line_height();
        let font_size = window.text_style().font_size.to_pixels(window.rem_size());
        let id = window.request_measured_layout(style, move |known, available, window, _| {
            let width = known.width.unwrap_or(match available.width {
                gpui::AvailableSpace::Definite(w) => w,
                _ => px(0.),
            });
            let rows = wrap(text.clone(), &runs, font_size, width, line_height, window)
                .rows
                .len();
            size(width, line_height * rows.clamp(min, max) as f32)
        });
        (id, ())
    }

    fn prepaint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&gpui::InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut Self::RequestLayoutState,
        window: &mut Window,
        cx: &mut App,
    ) -> Self::PrepaintState {
        let input = self.input.read(cx);
        let selected_range = input.selected_range.clone();
        let cursor = input.cursor_offset();
        let (display_text, runs) = input.display(window);
        let font_size = window.text_style().font_size.to_pixels(window.rem_size());

        if input.multiline.is_some() {
            let line_height = window.line_height();
            let w = wrap(
                display_text,
                &runs,
                font_size,
                bounds.size.width,
                line_height,
                window,
            );
            let at = if input.content.is_empty() {
                point(px(0.), px(0.))
            } else {
                w.position(cursor)
            };
            let scroll_y = follow(
                input.scroll_y,
                at.y,
                at.y + line_height,
                bounds.size.height,
                w.height(),
            );
            let top = bounds.top() - scroll_y;
            let selections = if selected_range.is_empty() {
                Vec::new()
            } else {
                w.rows
                    .iter()
                    .enumerate()
                    .filter(|(_, r)| r.start <= selected_range.end && selected_range.start <= r.end)
                    .map(|(i, r)| {
                        let from = selected_range.start.max(r.start);
                        let to = selected_range.end.min(r.end);
                        let y = top + line_height * i as f32;
                        // A selected newline shows as a little room past the row.
                        let past = if to < selected_range.end {
                            px(4.)
                        } else {
                            px(0.)
                        };
                        fill(
                            Bounds::from_corners(
                                point(bounds.left() + w.x_in_row(i, from), y),
                                point(bounds.left() + w.x_in_row(i, to) + past, y + line_height),
                            ),
                            theme::text_selection(),
                        )
                    })
                    .collect()
            };
            return PrepaintState {
                line: None,
                cursor: None,
                selection: None,
                scroll: px(0.),
                cursor_at: selected_range.is_empty().then_some(at),
                wrapped: Some(w),
                selections,
                scroll_y,
            };
        }

        let line = window
            .text_system()
            .shape_line(display_text, font_size, &runs, None);

        let cursor_x = if input.content.is_empty() {
            px(0.)
        } else {
            line.x_for_index(cursor)
        };
        let width = bounds.size.width;
        // Keep the last shift and move it only as far as needed to show the caret,
        // so long content doesn't jump back to its start when the caret moves left.
        let max_scroll = (line.width - width + px(4.)).max(px(0.));
        let mut scroll = input.scroll_x;
        if cursor_x - scroll > width - px(4.) {
            scroll = cursor_x - width + px(4.);
        }
        if cursor_x < scroll {
            scroll = cursor_x;
        }
        let scroll = scroll.min(max_scroll).max(px(0.));
        let left = bounds.left() - scroll;
        let (selection, cursor) = if selected_range.is_empty() {
            (None, Some(cursor_x))
        } else {
            (
                Some(fill(
                    Bounds::from_corners(
                        point(left + line.x_for_index(selected_range.start), bounds.top()),
                        point(left + line.x_for_index(selected_range.end), bounds.bottom()),
                    ),
                    theme::text_selection(),
                )),
                None,
            )
        };
        PrepaintState {
            line: Some(line),
            cursor,
            selection,
            scroll,
            wrapped: None,
            cursor_at: None,
            selections: Vec::new(),
            scroll_y: px(0.),
        }
    }

    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&gpui::InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut Self::RequestLayoutState,
        prepaint: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        let focus_handle = self.input.read(cx).focus_handle.clone();
        window.handle_input(
            &focus_handle,
            ElementInputHandler::new(bounds, self.input.clone()),
            cx,
        );
        if let Some(w) = prepaint.wrapped.take() {
            let origin = point(bounds.left(), bounds.top() - prepaint.scroll_y);
            window.with_content_mask(Some(gpui::ContentMask { bounds }), |window| {
                for quad in prepaint.selections.drain(..) {
                    window.paint_quad(quad);
                }
                let mut y = origin.y;
                for line in &w.lines {
                    let _ = line.paint(
                        point(origin.x, y),
                        w.line_height,
                        gpui::TextAlign::Left,
                        None,
                        window,
                        cx,
                    );
                    y += line.size(w.line_height).height;
                }
                match prepaint.cursor_at.take() {
                    Some(at) if focus_handle.is_focused(window) => {
                        let (caret, (at, alpha)) = self.input.update(cx, |input, cx| {
                            (input.caret, input.motion.frame(at, window, cx))
                        });
                        let row = Bounds::new(
                            point(origin.x, origin.y + at.y),
                            size(bounds.size.width, w.line_height),
                        );
                        window.paint_quad(caret_quad(origin.x + at.x, row, caret, alpha));
                    }
                    _ => self.input.update(cx, |input, _| input.motion.hide()),
                }
            });
            let text_bounds = Bounds::new(origin, bounds.size);
            let scroll_y = prepaint.scroll_y;
            self.input.update(cx, |input, _| {
                input.last_wrapped = Some(w);
                input.last_bounds = Some(text_bounds);
                input.scroll_y = scroll_y;
            });
            return;
        }
        let origin = point(bounds.left() - prepaint.scroll, bounds.top());
        window.with_content_mask(Some(gpui::ContentMask { bounds }), |window| {
            if let Some(selection) = prepaint.selection.take() {
                window.paint_quad(selection)
            }
            let line = prepaint.line.take().unwrap();
            let _ = line.paint(
                origin,
                window.line_height(),
                gpui::TextAlign::Left,
                None,
                window,
                cx,
            );
            match prepaint.cursor.take() {
                Some(x) if focus_handle.is_focused(window) => {
                    let (caret, (at, alpha)) = self.input.update(cx, |input, cx| {
                        (
                            input.caret,
                            input.motion.frame(point(x, px(0.)), window, cx),
                        )
                    });
                    window.paint_quad(caret_quad(origin.x + at.x, bounds, caret, alpha));
                }
                _ => self.input.update(cx, |input, _| input.motion.hide()),
            }
            let text_bounds = Bounds::new(origin, bounds.size);
            let scroll = prepaint.scroll;
            self.input.update(cx, |input, _| {
                input.last_layout = Some(line);
                input.last_bounds = Some(text_bounds);
                input.scroll_x = scroll;
            });
        });
    }
}

/// Text shaped and wrapped to the field, with each row's byte range.
struct Wrapped {
    /// One per line between newlines; each may wrap into several rows.
    lines: Vec<WrappedLine>,
    /// Where each line starts in the text.
    starts: Vec<usize>,
    /// Each row's range in the text, top to bottom.
    rows: Vec<Range<usize>>,
    /// The line each row belongs to.
    row_line: Vec<usize>,
    line_height: Pixels,
}

impl Wrapped {
    fn new(lines: Vec<WrappedLine>, line_height: Pixels) -> Self {
        let mut starts = Vec::with_capacity(lines.len());
        let mut at = 0;
        for l in &lines {
            starts.push(at);
            at += l.len() + 1;
        }
        let spans: Vec<(usize, usize, Vec<usize>)> = lines
            .iter()
            .zip(&starts)
            .map(|(l, s)| {
                let breaks = l
                    .wrap_boundaries()
                    .iter()
                    .map(|b| l.runs()[b.run_ix].glyphs[b.glyph_ix].index)
                    .collect();
                (*s, l.len(), breaks)
            })
            .collect();
        let (rows, row_line) = row_ranges(&spans);
        Self {
            lines,
            starts,
            rows,
            row_line,
            line_height,
        }
    }

    fn height(&self) -> Pixels {
        self.line_height * self.rows.len() as f32
    }

    fn row_of(&self, offset: usize) -> usize {
        row_index(&self.rows, offset)
    }

    /// Where `offset` sits, from the top left of the text.
    fn position(&self, offset: usize) -> Point<Pixels> {
        let r = self.row_of(offset);
        point(self.x_in_row(r, offset), self.line_height * r as f32)
    }

    /// How far along row `r` the offset is, clamped to the row.
    fn x_in_row(&self, r: usize, offset: usize) -> Pixels {
        let (Some(row), Some(&li)) = (self.rows.get(r), self.row_line.get(r)) else {
            return px(0.);
        };
        let layout = &self.lines[li].unwrapped_layout;
        let s = self.starts[li];
        let at = offset.clamp(row.start, row.end);
        layout.x_for_index(at - s) - layout.x_for_index(row.start - s)
    }

    /// The offset closest to a point from the top left of the text.
    fn index_at(&self, p: Point<Pixels>) -> usize {
        if self.rows.is_empty() {
            return 0;
        }
        let r = ((p.y / self.line_height).floor().max(0.) as usize).min(self.rows.len() - 1);
        let row = &self.rows[r];
        let li = self.row_line[r];
        let layout = &self.lines[li].unwrapped_layout;
        let s = self.starts[li];
        let x0 = layout.x_for_index(row.start - s);
        // A wrapped row's end starts the row below; stop before it.
        let text = &self.lines[li].text;
        let end = if self.rows.get(r + 1).is_some_and(|n| n.start == row.end) {
            text[..row.end - s]
                .char_indices()
                .next_back()
                .map_or(row.end, |(at, _)| (s + at).max(row.start))
        } else {
            row.end
        };
        (layout.closest_index_for_x(p.x.max(px(0.)) + x0) + s).clamp(row.start, end)
    }
}

/// The rows of wrapped text, from each line's start, length and the line
/// offsets where it wraps. Returns each row's range in the whole text, and
/// the line it is on.
fn row_ranges(lines: &[(usize, usize, Vec<usize>)]) -> (Vec<Range<usize>>, Vec<usize>) {
    let mut rows = Vec::new();
    let mut row_line = Vec::new();
    for (i, (start, len, breaks)) in lines.iter().enumerate() {
        let mut from = 0;
        for &b in breaks.iter().chain(std::iter::once(len)) {
            rows.push(start + from..start + b);
            row_line.push(i);
            from = b;
        }
    }
    (rows, row_line)
}

/// The row an offset is on. An offset where a line wraps starts the row
/// below, so Home on a wrapped row puts the caret at its start; the end of
/// a line, before its newline, stays on its own row.
fn row_index(rows: &[Range<usize>], offset: usize) -> usize {
    rows.iter()
        .enumerate()
        .position(|(i, r)| {
            let wraps_on = rows.get(i + 1).is_some_and(|n| n.start == r.end);
            offset < r.end || (offset == r.end && !wraps_on)
        })
        .unwrap_or(rows.len().saturating_sub(1))
}

/// Where End goes on row `i`: its end, or just before the space a wrapped
/// row breaks after, so the caret stays on that row.
fn row_end(rows: &[Range<usize>], text: &str, i: usize) -> usize {
    let Some(r) = rows.get(i) else {
        return text.len();
    };
    let wraps_on = rows.get(i + 1).is_some_and(|n| n.start == r.end);
    if !wraps_on || r.is_empty() {
        return r.end;
    }
    text[r.start..r.end]
        .char_indices()
        .next_back()
        .map_or(r.end, |(at, _)| r.start + at)
}

/// The vertical shift that keeps the caret's row (`top` to `bottom`) in a
/// view `view` tall, moving the last shift only as far as needed.
fn follow(scroll: Pixels, top: Pixels, bottom: Pixels, view: Pixels, content: Pixels) -> Pixels {
    let mut scroll = scroll;
    if bottom - scroll > view {
        scroll = bottom - view;
    }
    if top < scroll {
        scroll = top;
    }
    scroll.min((content - view).max(px(0.))).max(px(0.))
}

/// What Enter types: a newline in multiline mode; `None` submits.
fn entered(multiline: bool) -> Option<&'static str> {
    multiline.then_some("\n")
}

/// Pasted text as the field keeps it: newlines become spaces on one line.
fn pasted(text: &str, multiline: bool) -> String {
    let text = text.replace("\r\n", "\n");
    if multiline {
        text
    } else {
        text.replace('\n', " ")
    }
}

/// The start of the line `offset` is on.
fn line_start(text: &str, offset: usize) -> usize {
    text[..offset].rfind('\n').map_or(0, |i| i + 1)
}

/// The end of the line `offset` is on, before its newline.
fn line_end(text: &str, offset: usize) -> usize {
    text[offset..].find('\n').map_or(text.len(), |i| offset + i)
}

/// The caret at `x`: 2 px in its own `color`, or the thin one in the text color.
fn caret_quad(
    x: Pixels,
    bounds: Bounds<Pixels>,
    color: Option<gpui::Hsla>,
    alpha: f32,
) -> PaintQuad {
    let (inset, width, color) = match color {
        Some(color) => (1., 2., color),
        None => (2., 1.5, theme::content()),
    };
    fill(
        Bounds::new(
            point(x, bounds.top() + px(inset)),
            size(px(width), bounds.size.height - px(inset * 2.)),
        ),
        crate::caret::faded(color, alpha),
    )
    .corner_radii(crate::caret::radius(width))
}

impl Render for TextInput {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // Hidden text stays hidden from screen readers too.
        let hidden = self.secret || (self.redacted && crate::redact::on());
        div()
            // The view is the field, so its entity keeps the id unique.
            .id("text-input")
            .role(gpui::Role::TextInput)
            .when(!self.placeholder.is_empty(), |d| {
                d.aria_label(self.placeholder.clone())
                    .aria_placeholder(self.placeholder.clone())
            })
            .when(!hidden, |d| d.aria_value(self.content.clone()))
            .flex()
            .flex_1()
            .min_w_0()
            .key_context("TextInput")
            .track_focus(&self.focus_handle)
            .cursor(CursorStyle::IBeam)
            .on_action(cx.listener(Self::backspace))
            .on_action(cx.listener(Self::backspace_word))
            .on_action(cx.listener(Self::delete))
            .on_action(cx.listener(Self::left))
            .on_action(cx.listener(Self::right))
            .on_action(cx.listener(Self::word_left))
            .on_action(cx.listener(Self::word_right))
            .on_action(cx.listener(Self::select_left))
            .on_action(cx.listener(Self::select_right))
            .on_action(cx.listener(Self::select_all))
            .on_action(cx.listener(Self::home))
            .on_action(cx.listener(Self::end))
            .on_action(cx.listener(Self::paste))
            .on_action(cx.listener(Self::cut))
            .on_action(cx.listener(Self::copy))
            .on_action(cx.listener(Self::enter))
            .on_action(cx.listener(|_, _: &SubmitAlt, _, cx| cx.emit(InputEvent::SubmitAlt)))
            .on_action(cx.listener(|_, _: &Cancel, _, cx| cx.emit(InputEvent::Cancel)))
            .on_action(cx.listener(|this, _: &Up, _, cx| this.vertical(-1, cx)))
            .on_action(cx.listener(|this, _: &Down, _, cx| this.vertical(1, cx)))
            .on_action(cx.listener(|_, _: &Complete, _, cx| cx.emit(InputEvent::Complete)))
            .on_mouse_down(MouseButton::Left, cx.listener(Self::on_mouse_down))
            .on_mouse_up(MouseButton::Left, cx.listener(Self::on_mouse_up))
            .on_mouse_up_out(MouseButton::Left, cx.listener(Self::on_mouse_up))
            .on_mouse_move(cx.listener(Self::on_mouse_move))
            .when(self.mono, |d| d.font_family(theme::FONT_MONO))
            .child(TextElement { input: cx.entity() })
    }
}

impl Focusable for TextInput {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn enter_types_a_newline_only_in_multiline() {
        assert_eq!(entered(true), Some("\n"));
        // One line: Enter stays Submit, as every existing field expects.
        assert_eq!(entered(false), None);
    }

    #[test]
    fn paste_keeps_newlines_only_in_multiline() {
        assert_eq!(pasted("a\r\nb\nc", true), "a\nb\nc");
        assert_eq!(pasted("a\r\nb\nc", false), "a b c");
    }

    #[test]
    fn line_ends_around_newlines() {
        let t = "one\ntwo two\n\nend";
        assert_eq!(line_start(t, 0), 0);
        assert_eq!(line_start(t, 6), 4);
        assert_eq!(line_end(t, 6), 11);
        assert_eq!(line_start(t, 12), 12);
        assert_eq!(line_end(t, 12), 12);
        assert_eq!(line_end(t, 14), t.len());
    }

    #[test]
    fn rows_of_wrapped_lines() {
        // "hello world again" wraps after "hello " and "world "; then a
        // newline and "x"; then a trailing newline's empty line.
        let text = "hello world again\nx\n";
        let lines = vec![(0, 17, vec![6, 12]), (18, 1, vec![]), (20, 0, vec![])];
        let (rows, line_of) = row_ranges(&lines);
        assert_eq!(rows, vec![0..6, 6..12, 12..17, 18..19, 20..20]);
        assert_eq!(line_of, vec![0, 0, 0, 1, 2]);
        assert_eq!(&text[rows[1].clone()], "world ");
        assert_eq!(row_index(&rows, 0), 0);
        // Where a line wraps, the offset starts the row below.
        assert_eq!(row_index(&rows, 5), 0);
        assert_eq!(row_index(&rows, 6), 1);
        assert_eq!(row_index(&rows, 12), 2);
        // The end of a line, before its newline, stays on its row.
        assert_eq!(row_index(&rows, 17), 2);
        assert_eq!(row_index(&rows, 18), 3);
        assert_eq!(row_index(&rows, 19), 3);
        assert_eq!(row_index(&rows, 20), 4);
        assert_eq!(row_index(&rows, 99), 4);
        // End stays on a wrapped row, before the space it breaks after.
        assert_eq!(row_end(&rows, text, 0), 5);
        assert_eq!(row_end(&rows, text, 1), 11);
        assert_eq!(row_end(&rows, text, 2), 17);
        assert_eq!(row_end(&rows, text, 4), 20);
    }

    #[test]
    fn scroll_follows_the_caret_row() {
        let (h, view, content) = (px(20.), px(60.), px(200.));
        // In view: no shift.
        assert_eq!(follow(px(0.), px(20.), px(20.) + h, view, content), px(0.));
        // Below the view: shift just enough to show the row.
        assert_eq!(
            follow(px(0.), px(100.), px(100.) + h, view, content),
            px(60.)
        );
        // Above it: shift up to the row's top.
        assert_eq!(
            follow(px(60.), px(20.), px(20.) + h, view, content),
            px(20.)
        );
        // Never past the end of the content.
        assert_eq!(
            follow(px(500.), px(180.), px(200.), view, content),
            px(140.)
        );
    }

    #[test]
    fn composed_selection_counts_from_the_composed_text() {
        let before = "ñandú 日本語 ";
        let field = format!("{before}にほ");
        let at = |sel| composed_selection(before.len(), "にほ", sel);
        assert_eq!(at(Some(2..2)), field.len()..field.len());
        assert_eq!(at(Some(1..2)), before.len() + 3..field.len());
        assert_eq!(at(None), field.len()..field.len());
        // A surrogate pair is two UTF-16 units but one char.
        let field = format!("{before}😀a");
        let sel = composed_selection(before.len(), "😀a", Some(2..2));
        assert_eq!(sel, before.len() + 4..before.len() + 4);
        assert!(field.is_char_boundary(sel.start));
    }
}
