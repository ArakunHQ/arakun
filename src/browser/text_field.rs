//! A single-line text input: the address bar and the `prompt()` dialog field.
//! Keys are matched directly in `on_key_down` instead of through actions, so the module
//! works without key bindings registered by the app.

use std::ops::Range;

use gpui::{
    App, Bounds, ClipboardItem, ContentMask, Context, CursorStyle, Element, ElementId,
    ElementInputHandler, Entity, EntityInputHandler, EventEmitter, FocusHandle, Focusable,
    GlobalElementId, Hsla, InteractiveElement as _, IntoElement, KeyDownEvent, LayoutId,
    MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, ParentElement as _, Pixels, Point,
    Render, ShapedLine, SharedString, Style, Styled as _, TextRun, UTF16Selection, UnderlineStyle,
    Window, div, fill, point, px, relative, size,
};
use unicode_segmentation::UnicodeSegmentation as _;

use crate::theme;

pub enum TextFieldEvent {
    Submit,
    Cancel,
}

pub struct TextField {
    focus: FocusHandle,
    content: String,
    placeholder: SharedString,
    selected: Range<usize>,
    reversed: bool,
    marked: Option<Range<usize>>,
    last_layout: Option<ShapedLine>,
    last_bounds: Option<Bounds<Pixels>>,
    selecting: bool,
    scroll_x: Pixels,
    /// The user changed the text since the last `set_text`.
    edited: bool,
}

impl EventEmitter<TextFieldEvent> for TextField {}

impl Focusable for TextField {
    fn focus_handle(&self, _: &App) -> gpui::FocusHandle {
        self.focus.clone()
    }
}

impl TextField {
    pub fn new(placeholder: impl Into<SharedString>, cx: &mut Context<Self>) -> Self {
        Self {
            focus: cx.focus_handle(),
            content: String::new(),
            placeholder: placeholder.into(),
            selected: 0..0,
            reversed: false,
            marked: None,
            last_layout: None,
            last_bounds: None,
            selecting: false,
            scroll_x: px(0.),
            edited: false,
        }
    }

    pub fn text(&self) -> &str {
        &self.content
    }

    pub fn set_text(&mut self, text: &str, cx: &mut Context<Self>) {
        self.edited = false;
        if self.content == text {
            return;
        }
        self.content = text.replace(['\n', '\r'], "");
        self.selected = self.content.len()..self.content.len();
        self.reversed = false;
        self.marked = None;
        self.scroll_x = px(0.);
        cx.notify();
    }

    pub fn select_all(&mut self, cx: &mut Context<Self>) {
        self.selected = 0..self.content.len();
        self.reversed = false;
        cx.notify();
    }

    pub fn is_edited(&self) -> bool {
        self.edited
    }

    pub fn is_focused(&self, window: &Window) -> bool {
        self.focus.is_focused(window)
    }

    fn cursor(&self) -> usize {
        if self.reversed {
            self.selected.start
        } else {
            self.selected.end
        }
    }

    fn move_to(&mut self, offset: usize, cx: &mut Context<Self>) {
        self.selected = offset..offset;
        self.reversed = false;
        cx.notify();
    }

    fn select_to(&mut self, offset: usize, cx: &mut Context<Self>) {
        if self.reversed {
            self.selected.start = offset;
        } else {
            self.selected.end = offset;
        }
        if self.selected.end < self.selected.start {
            self.reversed = !self.reversed;
            self.selected = self.selected.end..self.selected.start;
        }
        cx.notify();
    }

    fn prev_boundary(&self, offset: usize) -> usize {
        self.content
            .grapheme_indices(true)
            .rev()
            .find_map(|(i, _)| (i < offset).then_some(i))
            .unwrap_or(0)
    }

    fn next_boundary(&self, offset: usize) -> usize {
        self.content
            .grapheme_indices(true)
            .find_map(|(i, _)| (i > offset).then_some(i))
            .unwrap_or(self.content.len())
    }

    fn prev_word(&self, offset: usize) -> usize {
        self.content
            .split_word_bound_indices()
            .rev()
            .find_map(|(i, w)| (i < offset && !w.trim().is_empty()).then_some(i))
            .unwrap_or(0)
    }

    fn next_word(&self, offset: usize) -> usize {
        self.content
            .split_word_bound_indices()
            .find_map(|(i, w)| {
                (i + w.len() > offset && !w.trim().is_empty()).then_some(i + w.len())
            })
            .unwrap_or(self.content.len())
    }

    fn replace(&mut self, range: Range<usize>, text: &str, cx: &mut Context<Self>) {
        let text = text.replace(['\n', '\r'], " ");
        self.content.replace_range(range.clone(), &text);
        self.edited = true;
        let end = range.start + text.len();
        self.selected = end..end;
        self.reversed = false;
        self.marked = None;
        cx.notify();
    }

    fn key_down(&mut self, ev: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let ks = &ev.keystroke;
        let m = &ks.modifiers;
        let ctrl = m.control || m.platform;
        let cursor = self.cursor();
        match ks.key.as_str() {
            "enter" => cx.emit(TextFieldEvent::Submit),
            "escape" => cx.emit(TextFieldEvent::Cancel),
            "backspace" => {
                if self.selected.is_empty() {
                    let to = if ctrl {
                        self.prev_word(cursor)
                    } else {
                        self.prev_boundary(cursor)
                    };
                    self.replace(to..cursor, "", cx);
                } else {
                    self.replace(self.selected.clone(), "", cx);
                }
            }
            "delete" => {
                if self.selected.is_empty() {
                    let to = if ctrl {
                        self.next_word(cursor)
                    } else {
                        self.next_boundary(cursor)
                    };
                    self.replace(cursor..to, "", cx);
                } else {
                    self.replace(self.selected.clone(), "", cx);
                }
            }
            "left" | "right" | "home" | "end" => {
                let to = match ks.key.as_str() {
                    "left" if ctrl => self.prev_word(cursor),
                    "left" if !m.shift && !self.selected.is_empty() => self.selected.start,
                    "left" => self.prev_boundary(cursor),
                    "right" if ctrl => self.next_word(cursor),
                    "right" if !m.shift && !self.selected.is_empty() => self.selected.end,
                    "right" => self.next_boundary(cursor),
                    "home" => 0,
                    _ => self.content.len(),
                };
                if m.shift {
                    self.select_to(to, cx)
                } else {
                    self.move_to(to, cx)
                }
            }
            "a" if ctrl => self.select_all(cx),
            "c" | "x" if ctrl => {
                if !self.selected.is_empty() {
                    cx.write_to_clipboard(ClipboardItem::new_string(
                        self.content[self.selected.clone()].to_string(),
                    ));
                    if ks.key == "x" {
                        self.replace(self.selected.clone(), "", cx);
                    }
                }
            }
            "v" if ctrl => {
                if let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) {
                    self.replace(self.selected.clone(), text.trim(), cx);
                }
            }
            _ => return,
        }
        let _ = window;
        cx.stop_propagation();
    }

    fn mouse_down(&mut self, ev: &MouseDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        if !self.focus.is_focused(window) {
            // Like a browser omnibox: the first click selects the whole address.
            window.focus(&self.focus, cx);
            self.select_all(cx);
            return;
        }
        if ev.click_count >= 2 {
            self.select_all(cx);
            return;
        }
        self.selecting = true;
        let index = self.index_for_position(ev.position);
        if ev.modifiers.shift {
            self.select_to(index, cx)
        } else {
            self.move_to(index, cx)
        }
    }

    fn mouse_up(&mut self, _: &MouseUpEvent, _: &mut Window, _: &mut Context<Self>) {
        self.selecting = false;
    }

    fn mouse_move(&mut self, ev: &MouseMoveEvent, _: &mut Window, cx: &mut Context<Self>) {
        if self.selecting {
            let index = self.index_for_position(ev.position);
            self.select_to(index, cx);
        }
    }

    fn index_for_position(&self, position: Point<Pixels>) -> usize {
        let (Some(bounds), Some(line)) = (self.last_bounds, self.last_layout.as_ref()) else {
            return 0;
        };
        if self.content.is_empty() {
            return 0;
        }
        line.closest_index_for_x(position.x - bounds.left() + self.scroll_x)
    }

    fn offset_from_utf16(&self, offset: usize) -> usize {
        let mut utf8 = 0;
        let mut utf16 = 0;
        for ch in self.content.chars() {
            if utf16 >= offset {
                break;
            }
            utf16 += ch.len_utf16();
            utf8 += ch.len_utf8();
        }
        utf8
    }

    fn offset_to_utf16(&self, offset: usize) -> usize {
        self.content[..offset.min(self.content.len())]
            .encode_utf16()
            .count()
    }

    fn range_from_utf16(&self, r: &Range<usize>) -> Range<usize> {
        self.offset_from_utf16(r.start)..self.offset_from_utf16(r.end)
    }

    fn range_to_utf16(&self, r: &Range<usize>) -> Range<usize> {
        self.offset_to_utf16(r.start)..self.offset_to_utf16(r.end)
    }
}

impl EntityInputHandler for TextField {
    fn text_for_range(
        &mut self,
        range: Range<usize>,
        actual: &mut Option<Range<usize>>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<String> {
        let range = self.range_from_utf16(&range);
        actual.replace(self.range_to_utf16(&range));
        Some(self.content[range].to_string())
    }

    fn selected_text_range(
        &mut self,
        _: bool,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<UTF16Selection> {
        Some(UTF16Selection {
            range: self.range_to_utf16(&self.selected),
            reversed: self.reversed,
        })
    }

    fn marked_text_range(&self, _: &mut Window, _: &mut Context<Self>) -> Option<Range<usize>> {
        self.marked.as_ref().map(|r| self.range_to_utf16(r))
    }

    fn unmark_text(&mut self, _: &mut Window, _: &mut Context<Self>) {
        self.marked = None;
    }

    fn replace_text_in_range(
        &mut self,
        range: Option<Range<usize>>,
        text: &str,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let range = range
            .map(|r| self.range_from_utf16(&r))
            .or(self.marked.clone())
            .unwrap_or(self.selected.clone());
        self.replace(range, text, cx);
    }

    fn replace_and_mark_text_in_range(
        &mut self,
        range: Option<Range<usize>>,
        text: &str,
        new_selected: Option<Range<usize>>,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let range = range
            .map(|r| self.range_from_utf16(&r))
            .or(self.marked.clone())
            .unwrap_or(self.selected.clone());
        self.content.replace_range(range.clone(), text);
        self.edited = true;
        self.marked = (!text.is_empty()).then(|| range.start..range.start + text.len());
        self.selected = new_selected
            .map(|r| self.range_from_utf16(&r))
            .map(|r| range.start + r.start..range.start + r.end)
            .unwrap_or(range.start + text.len()..range.start + text.len());
        cx.notify();
    }

    fn bounds_for_range(
        &mut self,
        range: Range<usize>,
        bounds: Bounds<Pixels>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        let line = self.last_layout.as_ref()?;
        let range = self.range_from_utf16(&range);
        let x = bounds.left() - self.scroll_x;
        Some(Bounds::from_corners(
            point(x + line.x_for_index(range.start), bounds.top()),
            point(x + line.x_for_index(range.end), bounds.bottom()),
        ))
    }

    fn character_index_for_point(
        &mut self,
        p: Point<Pixels>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<usize> {
        let bounds = self.last_bounds?;
        let line = self.last_layout.as_ref()?;
        let index = line.index_for_x(p.x - bounds.left() + self.scroll_x)?;
        Some(self.offset_to_utf16(index))
    }
}

impl Render for TextField {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .id("text-field")
            .flex()
            .flex_1()
            .min_w_0()
            .h_full()
            .items_center()
            .track_focus(&self.focus)
            .cursor(CursorStyle::IBeam)
            .on_key_down(cx.listener(Self::key_down))
            .on_mouse_down(MouseButton::Left, cx.listener(Self::mouse_down))
            .on_mouse_up(MouseButton::Left, cx.listener(Self::mouse_up))
            .on_mouse_up_out(MouseButton::Left, cx.listener(Self::mouse_up))
            .on_mouse_move(cx.listener(Self::mouse_move))
            .child(TextLine { field: cx.entity() })
    }
}

struct TextLine {
    field: Entity<TextField>,
}

struct LinePrepaint {
    line: ShapedLine,
    scroll_x: Pixels,
    cursor: Option<Bounds<Pixels>>,
    selection: Option<Bounds<Pixels>>,
}

impl IntoElement for TextLine {
    type Element = Self;
    fn into_element(self) -> Self {
        self
    }
}

impl Element for TextLine {
    type RequestLayoutState = ();
    type PrepaintState = LinePrepaint;

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
    ) -> (LayoutId, ()) {
        let mut style = Style::default();
        style.size.width = relative(1.).into();
        style.size.height = window.line_height().into();
        (window.request_layout(style, [], cx), ())
    }

    fn prepaint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&gpui::InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) -> LinePrepaint {
        let field = self.field.read(cx);
        let focused = field.focus.is_focused(window);
        let style = window.text_style();
        let (text, color): (SharedString, Hsla) = if field.content.is_empty() {
            (field.placeholder.clone(), theme::mix(0.35))
        } else {
            (
                field.content.clone().into(),
                if focused {
                    theme::content()
                } else {
                    theme::mix(0.7)
                },
            )
        };
        let run = TextRun {
            len: text.len(),
            font: style.font(),
            color,
            background_color: None,
            underline: None,
            strikethrough: None,
        };
        let runs = match field.marked.as_ref().filter(|_| !field.content.is_empty()) {
            Some(m) => [
                TextRun {
                    len: m.start,
                    ..run.clone()
                },
                TextRun {
                    len: m.end - m.start,
                    underline: Some(UnderlineStyle {
                        color: Some(color),
                        thickness: px(1.),
                        wavy: false,
                    }),
                    ..run.clone()
                },
                TextRun {
                    len: text.len() - m.end,
                    ..run
                },
            ]
            .into_iter()
            .filter(|r| r.len > 0)
            .collect(),
            None => vec![run],
        };
        let font_size = style.font_size.to_pixels(window.rem_size());
        let line = window
            .text_system()
            .shape_line(text, font_size, &runs, None);

        let content_len = field.content.len();
        let cursor_x = if content_len == 0 {
            px(0.)
        } else {
            line.x_for_index(field.cursor())
        };
        let width = bounds.size.width;
        let mut scroll_x = if focused { field.scroll_x } else { px(0.) };
        if focused {
            if cursor_x - scroll_x > width - px(2.) {
                scroll_x = cursor_x - width + px(2.);
            } else if cursor_x < scroll_x {
                scroll_x = cursor_x;
            }
            scroll_x = scroll_x
                .min((line.width - width + px(2.)).max(px(0.)))
                .max(px(0.));
        }
        let x0 = bounds.left() - scroll_x;
        let (cursor, selection) = if !focused {
            (None, None)
        } else if field.selected.is_empty() {
            (
                Some(Bounds::new(
                    point(x0 + cursor_x, bounds.top()),
                    size(px(1.5), bounds.size.height),
                )),
                None,
            )
        } else if content_len > 0 {
            let a = line.x_for_index(field.selected.start);
            let b = line.x_for_index(field.selected.end);
            (
                None,
                Some(Bounds::from_corners(
                    point(x0 + a, bounds.top()),
                    point(x0 + b, bounds.bottom()),
                )),
            )
        } else {
            (None, None)
        };
        LinePrepaint {
            line,
            scroll_x,
            cursor,
            selection,
        }
    }

    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&gpui::InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut (),
        p: &mut LinePrepaint,
        window: &mut Window,
        cx: &mut App,
    ) {
        let focus = self.field.read(cx).focus.clone();
        window.handle_input(
            &focus,
            ElementInputHandler::new(bounds, self.field.clone()),
            cx,
        );
        window.with_content_mask(Some(ContentMask { bounds }), |window| {
            if let Some(sel) = p.selection {
                window.paint_quad(fill(sel, theme::text_selection()));
            }
            let origin = point(bounds.left() - p.scroll_x, bounds.top());
            let _ = p.line.paint(
                origin,
                window.line_height(),
                gpui::TextAlign::Left,
                None,
                window,
                cx,
            );
            if let Some(cursor) = p.cursor {
                window.paint_quad(fill(cursor, theme::content()));
            }
        });
        let line = p.line.clone();
        let scroll_x = p.scroll_x;
        self.field.update(cx, |field, _| {
            field.last_layout = Some(line);
            field.last_bounds = Some(bounds);
            field.scroll_x = scroll_x;
        });
    }
}
