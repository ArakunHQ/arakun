//! The interactive half: focus, keyboard, IME, clipboard, mouse, cursor blink.

use std::ops::Range;
use std::time::{Duration, Instant};

use alacritty_terminal::grid::Scroll;
use alacritty_terminal::index::{Column, Direction, Line, Point as AlacPoint};
use alacritty_terminal::selection::SelectionType;
use alacritty_terminal::term::TermMode;
use alacritty_terminal::term::search::Match;
use gpui::prelude::FluentBuilder as _;
use gpui::{
    App, AppContext as _, Bounds, ClipboardItem, Context, Entity, EntityInputHandler, FocusHandle,
    Focusable, InteractiveElement as _, IntoElement, KeyDownEvent, MouseButton, MouseDownEvent,
    MouseMoveEvent, MouseUpEvent, ParentElement as _, Pixels, Render, ScrollDelta,
    ScrollWheelEvent, SharedString, StatefulInteractiveElement as _, Styled as _, Subscription,
    Task, UTF16Selection, Window, div, px,
};

use super::element::{FONT_SIZE, Geometry, TerminalElement};
use super::keys;
use super::model::Terminal;
use super::mouse::{self, Button};
use crate::input::{InputEvent, TextInput};
use crate::theme;
use crate::ui::icon;

const BLINK_INTERVAL: Duration = Duration::from_millis(600);
/// Blinking stops (cursor stays solid) this long after the last keystroke, so an idle
/// focused terminal doesn't repaint the window forever.
const BLINK_TIMEOUT: Duration = Duration::from_secs(10);
/// GPUI already reports a wheel notch as 3 lines; mouse reports want one per notch.
const LINES_PER_NOTCH: f32 = 3.;

pub struct TerminalView {
    terminal: Entity<Terminal>,
    focus: FocusHandle,
    metrics: Option<(Pixels, Pixels)>,
    geometry: Option<Geometry>,
    ime_bounds: Option<Bounds<Pixels>>,
    marked_text: Option<String>,
    focused: bool,
    cursor_on: bool,
    cursor_blinks: bool,
    blink_epoch: usize,
    last_input: Instant,
    blink_task: Option<Task<()>>,
    selecting: bool,
    reported_button: Option<MouseButton>,
    last_motion_cell: Option<mouse::Cell>,
    scroll_px: Pixels,
    search: Option<Search>,
    _subscriptions: Vec<Subscription>,
}

/// The find bar (Ctrl+Shift+F): the query and the match it is on.
struct Search {
    input: Entity<TextInput>,
    current: Option<Match>,
    missing: bool,
    _sub: Subscription,
}

impl TerminalView {
    pub fn new(terminal: Entity<Terminal>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let focus = cx.focus_handle();
        let subscriptions = vec![
            cx.observe(&terminal, |_, _, cx| cx.notify()),
            cx.on_focus_in(&focus, window, |this, window, cx| this.focus_changed(true, window, cx)),
            cx.on_focus_out(&focus, window, |this, _, window, cx| {
                this.focus_changed(false, window, cx)
            }),
            cx.observe_window_activation(window, |this, window, cx| {
                if this.focus.contains_focused(window, cx) {
                    let active = window.is_window_active();
                    this.focus_changed(active, window, cx);
                }
            }),
        ];
        Self {
            terminal,
            focus,
            metrics: None,
            geometry: None,
            ime_bounds: None,
            marked_text: None,
            focused: false,
            cursor_on: true,
            cursor_blinks: true,
            blink_epoch: 0,
            last_input: Instant::now(),
            blink_task: None,
            selecting: false,
            reported_button: None,
            last_motion_cell: None,
            scroll_px: px(0.),
            search: None,
            _subscriptions: subscriptions,
        }
    }

    pub fn terminal(&self) -> &Entity<Terminal> {
        &self.terminal
    }

    /// Cell width and line height for the terminal font, measured once.
    pub(crate) fn metrics(&self, window: &Window) -> (Pixels, Pixels) {
        if let Some(m) = self.metrics {
            return m;
        }
        measure(window)
    }

    pub(crate) fn frame_painted(
        &mut self,
        geometry: Geometry,
        ime_bounds: Bounds<Pixels>,
        blinking: bool,
        cx: &mut Context<Self>,
    ) {
        if self.metrics.is_none() {
            self.metrics = Some((geometry.cell_width, geometry.line_height));
        }
        self.geometry = Some(geometry);
        self.ime_bounds = Some(ime_bounds);
        if blinking != self.cursor_blinks {
            self.cursor_blinks = blinking;
            if blinking {
                self.restart_blink(cx);
            } else {
                self.cursor_on = true;
                self.blink_task = None;
            }
        }
    }

    fn is_focused(&self, window: &Window) -> bool {
        self.focus.is_focused(window) && window.is_window_active()
    }

    fn focus_changed(&mut self, focused: bool, window: &mut Window, cx: &mut Context<Self>) {
        let _ = window;
        self.focused = focused;
        let terminal = self.terminal.read(cx);
        if terminal.mode().contains(TermMode::FOCUS_IN_OUT) {
            terminal.write_input(if focused { b"\x1b[I" } else { b"\x1b[O" });
        }
        if focused {
            self.restart_blink(cx);
        } else {
            self.blink_task = None;
            self.cursor_on = true;
        }
        cx.notify();
    }

    fn restart_blink(&mut self, cx: &mut Context<Self>) {
        self.cursor_on = true;
        self.last_input = Instant::now();
        self.blink_epoch += 1;
        if !self.cursor_blinks || !self.focused {
            self.blink_task = None;
            return;
        }
        let epoch = self.blink_epoch;
        self.blink_task = Some(cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(BLINK_INTERVAL).await;
                let go = this
                    .update(cx, |view, cx| {
                        if view.blink_epoch != epoch || !view.cursor_blinks {
                            return false;
                        }
                        if view.last_input.elapsed() > BLINK_TIMEOUT {
                            if !view.cursor_on {
                                view.cursor_on = true;
                                cx.notify();
                            }
                            return false;
                        }
                        view.cursor_on = !view.cursor_on;
                        cx.notify();
                        true
                    })
                    .unwrap_or(false);
                if !go {
                    break;
                }
            }
        }));
    }

    fn input_happened(&mut self, cx: &mut Context<Self>) {
        self.terminal.read(cx).scroll_to_bottom();
        if self.blink_task.is_none() || !self.cursor_on {
            self.restart_blink(cx);
        } else {
            self.last_input = Instant::now();
        }
    }

    fn write(&mut self, bytes: &[u8], cx: &mut Context<Self>) {
        self.input_happened(cx);
        self.terminal.read(cx).write_input(bytes);
        cx.notify();
    }

    pub fn copy(&mut self, cx: &mut Context<Self>) {
        if let Some(text) = self.terminal.read(cx).selection_text() {
            cx.write_to_clipboard(ClipboardItem::new_string(text));
        }
    }

    pub fn paste(&mut self, cx: &mut Context<Self>) {
        if let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) {
            self.paste_text(&text, cx);
        }
    }

    fn paste_text(&mut self, text: &str, cx: &mut Context<Self>) {
        self.input_happened(cx);
        let terminal = self.terminal.read(cx);
        terminal.clear_selection();
        terminal.paste(text);
        cx.notify();
    }

    /// Opens the find bar, or focuses it with its text selected.
    pub fn open_search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(search) = &self.search {
            let focus = search.input.focus_handle(cx);
            search.input.update(cx, |i, cx| i.select_all_text(cx));
            window.focus(&focus, cx);
            return;
        }
        let input = cx.new(|cx| TextInput::new("Find", cx));
        let sub = cx.subscribe_in(&input, window, |this, _, ev, window, cx| match ev {
            InputEvent::Changed => this.find(None, true, cx),
            InputEvent::Submit | InputEvent::Up => this.find_step(true, cx),
            InputEvent::Down | InputEvent::SubmitAlt => this.find_step(false, cx),
            InputEvent::Cancel => this.close_search(window, cx),
            _ => {}
        });
        window.focus(&input.focus_handle(cx), cx);
        self.search = Some(Search {
            input,
            current: None,
            missing: false,
            _sub: sub,
        });
        cx.notify();
    }

    fn close_search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.search.take().is_some() {
            self.terminal.read(cx).clear_selection();
            window.focus(&self.focus, cx);
            cx.notify();
        }
    }

    fn find_step(&mut self, older: bool, cx: &mut Context<Self>) {
        let current = self.search.as_ref().and_then(|s| s.current.clone());
        self.find(current, older, cx);
    }

    fn find(&mut self, after: Option<Match>, older: bool, cx: &mut Context<Self>) {
        let Some(search) = &self.search else {
            return;
        };
        let query = search.input.read(cx).text().to_string();
        let found = self.terminal.read(cx).find(&query, after.as_ref(), older);
        if found.is_none() {
            self.terminal.read(cx).clear_selection();
        }
        if let Some(search) = &mut self.search {
            search.missing = found.is_none() && !query.is_empty();
            search.current = found;
        }
        cx.notify();
    }

    fn render_search(&self, search: &Search, cx: &mut Context<Self>) -> gpui::AnyElement {
        let nav = |id: &'static str, name: &'static str, older: bool| {
            div()
                .id(id)
                .flex()
                .items_center()
                .justify_center()
                .size(px(24.))
                .rounded(px(8.))
                .cursor_pointer()
                .hover(|s| s.bg(theme::fill_selected()))
                .child(icon(name, 12.).text_color(theme::text_subtle()))
                .on_click(cx.listener(move |this, _, _, cx| this.find_step(older, cx)))
        };
        div()
            .absolute()
            .top(px(6.))
            .right(px(12.))
            .flex()
            .items_center()
            .gap(px(4.))
            .w(px(300.))
            .pl(px(10.))
            .pr(px(4.))
            .py(px(4.))
            .rounded(px(12.))
            .border_1()
            .border_color(theme::border_strong())
            .bg(theme::dialog())
            .shadow_lg()
            .text_size(px(crate::ui::TEXT_SM))
            .child(div().flex_1().min_w_0().child(search.input.clone()))
            .when(search.missing, |d| {
                d.child(div().text_color(theme::text_subtle()).child("No results"))
            })
            .child(nav("term-find-up", "arrow-up01", true))
            .child(nav("term-find-down", "arrow-down01", false))
            .child(
                div()
                    .id("term-find-close")
                    .flex()
                    .items_center()
                    .justify_center()
                    .size(px(24.))
                    .rounded(px(8.))
                    .cursor_pointer()
                    .hover(|s| s.bg(theme::fill_selected()))
                    .child(icon("cancel01", 12.).text_color(theme::text_subtle()))
                    .on_click(cx.listener(|this, _, window, cx| this.close_search(window, cx))),
            )
            .into_any_element()
    }

    fn key_down(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let ks = &event.keystroke;
        let m = ks.modifiers;
        let key = ks.key.to_ascii_lowercase();
        let mode = self.terminal.read(cx).mode();

        // Ctrl+Shift, and on macOS Cmd alone, as in Terminal and iTerm.
        let mac_cmd = cfg!(target_os = "macos") && m.platform && !m.control && !m.alt && !m.shift;
        if m.control && m.shift && !m.alt && !m.platform || mac_cmd {
            match key.as_str() {
                "f" => {
                    self.open_search(window, cx);
                    cx.stop_propagation();
                    return;
                }
                "c" => {
                    self.copy(cx);
                    cx.stop_propagation();
                    return;
                }
                "v" => {
                    self.paste(cx);
                    cx.stop_propagation();
                    return;
                }
                _ => {}
            }
        }
        if m.shift && !m.control && !m.alt && !m.platform {
            if key == "insert" {
                self.paste(cx);
                cx.stop_propagation();
                return;
            }
            if !mode.contains(TermMode::ALT_SCREEN) {
                let scroll = match key.as_str() {
                    "pageup" => Some(Scroll::PageUp),
                    "pagedown" => Some(Scroll::PageDown),
                    "home" => Some(Scroll::Top),
                    "end" => Some(Scroll::Bottom),
                    _ => None,
                };
                if let Some(scroll) = scroll {
                    self.terminal.read(cx).scroll(scroll);
                    cx.notify();
                    cx.stop_propagation();
                    return;
                }
            }
        }

        if event.prefer_character_input && ks.key_char.is_some() {
            return;
        }
        if let Some(esc) = keys::to_esc_str(ks, mode) {
            self.write(esc.as_bytes(), cx);
            cx.stop_propagation();
        }
    }

    fn grid_point(&self, line: usize, col: usize, g: &Geometry) -> AlacPoint {
        AlacPoint::new(
            Line(line as i32 - g.display_offset as i32),
            Column(col),
        )
    }

    fn mouse_reporting(&self, shift: bool) -> Option<Geometry> {
        self.geometry
            .filter(|g| g.mode.intersects(TermMode::MOUSE_MODE) && !shift)
    }

    pub(crate) fn mouse_down(
        &mut self,
        e: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        window.focus(&self.focus, cx);
        let Some(g) = self.geometry else {
            return;
        };
        let (line, col, right) = g.cell_at(e.position);
        let cell = mouse::Cell { line, column: col };

        if let Some(g) = self.mouse_reporting(e.modifiers.shift) {
            if let Some(button) = Button::press(e.button)
                && let Some(report) = mouse::report(cell, button, true, e.modifiers, g.mode)
            {
                self.terminal.read(cx).write_input(&report);
                self.reported_button = Some(e.button);
                self.last_motion_cell = Some(cell);
            }
            return;
        }

        let point = self.grid_point(line, col, &g);
        let side = if right { Direction::Right } else { Direction::Left };
        match e.button {
            MouseButton::Left => {
                if (e.modifiers.control || cfg!(target_os = "macos") && e.modifiers.platform)
                    && let Some(url) = self.url_at(point, cx)
                {
                    cx.open_url(&url);
                    return;
                }
                let ty = match e.click_count {
                    0 | 1 => SelectionType::Simple,
                    2 => SelectionType::Semantic,
                    _ => SelectionType::Lines,
                };
                self.terminal.read(cx).start_selection(ty, point, side);
                self.selecting = true;
                cx.notify();
            }
            MouseButton::Middle => {
                if let Some(text) = crate::platform::read_primary(cx).and_then(|item| item.text()) {
                    self.paste_text(&text, cx);
                }
            }
            _ => {}
        }
    }

    pub(crate) fn mouse_move(&mut self, e: &MouseMoveEvent, hovered: bool, cx: &mut Context<Self>) {
        let Some(g) = self.geometry else {
            return;
        };
        if self.reported_button.is_some() || (hovered && self.mouse_reporting(e.modifiers.shift).is_some()) {
            if !mouse::wants_motion(g.mode, e.pressed_button) {
                return;
            }
            let (line, col, _) = g.cell_at(e.position);
            let cell = mouse::Cell { line, column: col };
            if self.last_motion_cell == Some(cell) {
                return;
            }
            self.last_motion_cell = Some(cell);
            if let Some(button) = Button::motion(e.pressed_button)
                && let Some(report) = mouse::report(cell, button, true, e.modifiers, g.mode)
            {
                self.terminal.read(cx).write_input(&report);
            }
            return;
        }

        if !self.selecting || e.pressed_button != Some(MouseButton::Left) {
            return;
        }
        let terminal = self.terminal.read(cx);
        if e.position.y < g.origin.y {
            terminal.scroll(Scroll::Delta(1));
        } else if e.position.y > g.origin.y + g.line_height * g.lines as f32 {
            terminal.scroll(Scroll::Delta(-1));
        }
        let offset = terminal.display_offset();
        let (line, col, right) = g.cell_at(e.position);
        let point = AlacPoint::new(Line(line as i32 - offset as i32), Column(col));
        let side = if right { Direction::Right } else { Direction::Left };
        terminal.update_selection(point, side);
        cx.notify();
    }

    pub(crate) fn mouse_up(&mut self, e: &MouseUpEvent, _window: &mut Window, cx: &mut Context<Self>) {
        if let Some(button) = self.reported_button
            && button == e.button
        {
            self.reported_button = None;
            if let Some(g) = self.geometry
                && let Some(b) = Button::press(button)
            {
                let (line, col, _) = g.cell_at(e.position);
                let cell = mouse::Cell { line, column: col };
                if let Some(report) = mouse::report(cell, b, false, e.modifiers, g.mode) {
                    self.terminal.read(cx).write_input(&report);
                }
            }
            return;
        }
        if self.selecting && e.button == MouseButton::Left {
            self.selecting = false;
            if let Some(text) = self.terminal.read(cx).selection_text() {
                crate::platform::write_primary(cx, ClipboardItem::new_string(text));
            }
        }
    }

    pub(crate) fn scroll_wheel(&mut self, e: &ScrollWheelEvent, cx: &mut Context<Self>) {
        let Some(g) = self.geometry else {
            return;
        };
        let reporting = self.mouse_reporting(e.modifiers.shift).is_some();
        let alt_scroll = !reporting
            && !e.modifiers.shift
            && g.mode.contains(TermMode::ALT_SCREEN | TermMode::ALTERNATE_SCROLL);
        let multiplier = match e.delta {
            ScrollDelta::Lines(_) if reporting => 1. / LINES_PER_NOTCH,
            _ => 1.,
        };
        self.scroll_px += e.delta.pixel_delta(g.line_height).y * multiplier;
        let lines = (self.scroll_px / g.line_height) as i32;
        if lines == 0 {
            return;
        }
        self.scroll_px -= g.line_height * lines as f32;

        let terminal = self.terminal.read(cx);
        if reporting {
            let (line, col, _) = g.cell_at(e.position);
            let cell = mouse::Cell { line, column: col };
            let button = if lines > 0 {
                Button::ScrollUp
            } else {
                Button::ScrollDown
            };
            if let Some(report) = mouse::report(cell, button, true, e.modifiers, g.mode) {
                for _ in 0..lines.unsigned_abs() {
                    terminal.write_input(&report);
                }
            }
        } else if alt_scroll {
            terminal.write_input(&mouse::alt_scroll(lines, g.mode.contains(TermMode::APP_CURSOR)));
        } else {
            terminal.scroll(Scroll::Delta(lines));
            cx.notify();
        }
    }

    /// OSC 8 link under `point`, else a plain http(s)/file URL in the text around it.
    fn url_at(&self, point: AlacPoint, cx: &App) -> Option<String> {
        let terminal = self.terminal.read(cx);
        if let Some(uri) = terminal.hyperlink_at(point) {
            return Some(uri);
        }
        let (text, index) = terminal.line_around(point);
        find_url(&text, index)
    }
}

fn measure(window: &Window) -> (Pixels, Pixels) {
    let font_size = px(FONT_SIZE);
    let text_system = window.text_system();
    let font = gpui::Font {
        family: crate::theme::FONT_MONO.into(),
        features: gpui::FontFeatures::disable_ligatures(),
        fallbacks: None,
        weight: gpui::FontWeight::NORMAL,
        style: gpui::FontStyle::Normal,
    };
    let id = text_system.resolve_font(&font);
    let cell_width = text_system
        .advance(id, font_size, 'm')
        .map(|s| s.width)
        .unwrap_or(px(FONT_SIZE * 0.6));
    let line_height =
        (text_system.ascent(id, font_size) + text_system.descent(id, font_size).abs()).round();
    (cell_width, line_height.max(px(FONT_SIZE)))
}

fn find_url(text: &str, index: usize) -> Option<String> {
    let chars: Vec<char> = text.chars().collect();
    if index >= chars.len() {
        return None;
    }
    let is_url_char = |c: char| {
        !c.is_whitespace() && !matches!(c, '"' | '\'' | '<' | '>' | '`' | '{' | '}' | '|' | '\\' | '^')
    };
    if !is_url_char(chars[index]) {
        return None;
    }
    let mut start = index;
    while start > 0 && is_url_char(chars[start - 1]) {
        start -= 1;
    }
    let mut end = index + 1;
    while end < chars.len() && is_url_char(chars[end]) {
        end += 1;
    }
    let word: String = chars[start..end].iter().collect();
    let begin = ["https://", "http://", "file://"]
        .iter()
        .filter_map(|scheme| word.find(scheme))
        .min()?;
    let url = word[begin..].trim_end_matches(['.', ',', ';', ':', ')', ']', '!', '?']);
    // Keep a closing paren that belongs to the URL, e.g. wikipedia links.
    let url = if word[begin..].len() > url.len()
        && word[begin + url.len()..].starts_with(')')
        && url.contains('(')
    {
        &word[begin..begin + url.len() + 1]
    } else {
        url
    };
    (url.len() > "https://".len()).then(|| url.to_string())
}

impl Focusable for TerminalView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for TerminalView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let focused = self.is_focused(window);
        let search = self.search.as_ref().map(|s| self.render_search(s, cx));
        div()
            .relative()
            .size_full()
            .child(
                div()
                    .id("terminal")
                    .key_context("Terminal")
                    .track_focus(&self.focus)
                    .size_full()
                    .overflow_hidden()
                    .on_key_down(cx.listener(Self::key_down))
                    .child(TerminalElement::new(
                        self.terminal.clone(),
                        cx.entity(),
                        self.focus.clone(),
                        focused,
                        self.cursor_on || !focused,
                        self.marked_text.clone().map(SharedString::from),
                    )),
            )
            .when_some(search, |d, s| d.child(s))
    }
}

impl EntityInputHandler for TerminalView {
    fn text_for_range(
        &mut self,
        _: Range<usize>,
        _: &mut Option<Range<usize>>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<String> {
        None
    }

    fn selected_text_range(
        &mut self,
        _: bool,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<UTF16Selection> {
        Some(UTF16Selection {
            range: 0..0,
            reversed: false,
        })
    }

    fn marked_text_range(&self, _: &mut Window, _: &mut Context<Self>) -> Option<Range<usize>> {
        self.marked_text
            .as_ref()
            .map(|text| 0..text.encode_utf16().count())
    }

    fn unmark_text(&mut self, _: &mut Window, cx: &mut Context<Self>) {
        if self.marked_text.take().is_some() {
            cx.notify();
        }
    }

    fn replace_text_in_range(
        &mut self,
        _: Option<Range<usize>>,
        text: &str,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.marked_text = None;
        if !text.is_empty() {
            self.write(text.as_bytes(), cx);
        }
        cx.notify();
    }

    fn replace_and_mark_text_in_range(
        &mut self,
        _: Option<Range<usize>>,
        new_text: &str,
        _: Option<Range<usize>>,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.marked_text = (!new_text.is_empty()).then(|| new_text.to_string());
        cx.notify();
    }

    fn bounds_for_range(
        &mut self,
        range_utf16: Range<usize>,
        _: Bounds<Pixels>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        let mut bounds = self.ime_bounds?;
        if let Some(g) = self.geometry {
            bounds.origin.x += g.cell_width * range_utf16.start as f32;
        }
        Some(bounds)
    }

    fn character_index_for_point(
        &mut self,
        _: gpui::Point<Pixels>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<usize> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::find_url;

    #[test]
    fn urls() {
        let t = "see https://example.com/a(b) now";
        assert_eq!(find_url(t, 8).as_deref(), Some("https://example.com/a(b)"));
        assert_eq!(find_url("x http://a.io/x.", 4).as_deref(), Some("http://a.io/x"));
        assert_eq!(find_url("nothing here", 2), None);
    }
}
