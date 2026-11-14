//! Paints the alacritty grid. Each frame locks the terminal once, turns the visible
//! rows into style runs and merged background rects, unlocks, then shapes. Text
//! shaping is cached by GPUI across frames (the key is text + font runs; colors are
//! not part of it), so unchanged rows cost a hash lookup.

use alacritty_terminal::grid::Dimensions as _;
use alacritty_terminal::index::{Column, Line};
use alacritty_terminal::selection::SelectionRange;
use alacritty_terminal::term::TermMode;
use alacritty_terminal::term::cell::{Cell, Flags};
use alacritty_terminal::vte::ansi::{Color, CursorShape, NamedColor};
use gpui::{
    App, BorderStyle, Bounds, ContentMask, CursorStyle, DispatchPhase, Element, ElementId,
    ElementInputHandler, Entity, FocusHandle, Font, FontFeatures, FontStyle, FontWeight,
    GlobalElementId, Hitbox, HitboxBehavior, Hsla, InspectorElementId, IntoElement, LayoutId,
    MouseDownEvent, MouseMoveEvent, MouseUpEvent, Pixels, Point, ScrollWheelEvent, ShapedLine,
    SharedString, StrikethroughStyle, Style, TextAlign, TextRun, UnderlineStyle, Window, fill,
    outline, point, px, relative, size,
};

use super::colors::{self, Resolver};
use super::model::Terminal;
use super::view::TerminalView;
use crate::theme;

pub const FONT_SIZE: f32 = 13.;
/// Padding around the grid; full-screen TUIs (alt screen) get none.
const PAD_X: f32 = 10.;
const PAD_Y: f32 = 8.;
/// Longer gaps than this split a row into separately shaped segments.
const MAX_INLINE_GAP: usize = 4;

/// Where the grid sits on screen for the last painted frame; mouse handling and the
/// IME read it.
#[derive(Clone, Copy, Debug)]
pub struct Geometry {
    pub origin: Point<Pixels>,
    pub cell_width: Pixels,
    pub line_height: Pixels,
    pub columns: usize,
    pub lines: usize,
    pub display_offset: usize,
    pub mode: TermMode,
}

impl Geometry {
    /// Viewport cell under `position` (clamped into the grid) and which half of it.
    pub fn cell_at(&self, position: Point<Pixels>) -> (usize, usize, bool) {
        let x = (position.x - self.origin.x) / self.cell_width;
        let y = (position.y - self.origin.y) / self.line_height;
        let col = (x.max(0.) as usize).min(self.columns.saturating_sub(1));
        let line = (y.max(0.) as usize).min(self.lines.saturating_sub(1));
        let right = x >= self.columns as f32 || (x >= 0. && x.fract() > 0.5);
        (line, col, right)
    }
}

pub struct TerminalElement {
    terminal: Entity<Terminal>,
    view: Entity<TerminalView>,
    focus: FocusHandle,
    focused: bool,
    cursor_on: bool,
    marked_text: Option<SharedString>,
}

impl TerminalElement {
    pub fn new(
        terminal: Entity<Terminal>,
        view: Entity<TerminalView>,
        focus: FocusHandle,
        focused: bool,
        cursor_on: bool,
        marked_text: Option<SharedString>,
    ) -> Self {
        Self {
            terminal,
            view,
            focus,
            focused,
            cursor_on,
            marked_text,
        }
    }
}

impl IntoElement for TerminalElement {
    type Element = Self;
    fn into_element(self) -> Self {
        self
    }
}

#[derive(Clone, PartialEq)]
struct RunStyle {
    color: Hsla,
    bold: bool,
    italic: bool,
    underline: Option<UnderlineStyle>,
    strike: bool,
}

struct Segment {
    line: usize,
    col: usize,
    text: String,
    runs: Vec<(usize, RunStyle)>,
}

impl Segment {
    fn push(&mut self, text: &str, style: &RunStyle) {
        self.text.push_str(text);
        match self.runs.last_mut() {
            Some((len, last)) if last == style => *len += text.len(),
            _ => self.runs.push((text.len(), style.clone())),
        }
    }

    /// Spaces only need a font; reuse the previous run unless it draws decorations.
    fn push_spaces(&mut self, count: usize) {
        let Some((len, last)) = self.runs.last_mut() else {
            return;
        };
        for _ in 0..count {
            self.text.push(' ');
        }
        if last.underline.is_none() && !last.strike {
            *len += count;
        } else {
            let style = RunStyle {
                underline: None,
                strike: false,
                ..last.clone()
            };
            self.runs.push((count, style));
        }
    }
}

struct Rect {
    line: usize,
    start: usize,
    end: usize,
    color: Hsla,
}

/// Part of a cell in eighths: x in absolute eighth-columns, y in eighths of a line.
struct Block {
    line: usize,
    x0: usize,
    x1: usize,
    y0: u8,
    y1: u8,
    color: Hsla,
}

/// Block elements (U+2580..U+259F) as (x0, x1, y0, y1) boxes in eighths of a cell,
/// plus an alpha for the shades. Painting them as quads instead of glyphs keeps
/// pixel art and progress bars free of seams between cells.
/// A block-element quad in eighths of a cell: x0, x1, y0, y1.
type Quad = (u8, u8, u8, u8);

fn block_parts(c: char) -> Option<(&'static [Quad], f32)> {
    const UL: (u8, u8, u8, u8) = (0, 4, 0, 4);
    const UR: (u8, u8, u8, u8) = (4, 8, 0, 4);
    const LL: (u8, u8, u8, u8) = (0, 4, 4, 8);
    const LR: (u8, u8, u8, u8) = (4, 8, 4, 8);
    const FULL: &[(u8, u8, u8, u8)] = &[(0, 8, 0, 8)];
    Some(match c {
        '▀' => (&[(0, 8, 0, 4)], 1.),
        '▁' => (&[(0, 8, 7, 8)], 1.),
        '▂' => (&[(0, 8, 6, 8)], 1.),
        '▃' => (&[(0, 8, 5, 8)], 1.),
        '▄' => (&[(0, 8, 4, 8)], 1.),
        '▅' => (&[(0, 8, 3, 8)], 1.),
        '▆' => (&[(0, 8, 2, 8)], 1.),
        '▇' => (&[(0, 8, 1, 8)], 1.),
        '█' => (FULL, 1.),
        '▉' => (&[(0, 7, 0, 8)], 1.),
        '▊' => (&[(0, 6, 0, 8)], 1.),
        '▋' => (&[(0, 5, 0, 8)], 1.),
        '▌' => (&[(0, 4, 0, 8)], 1.),
        '▍' => (&[(0, 3, 0, 8)], 1.),
        '▎' => (&[(0, 2, 0, 8)], 1.),
        '▏' => (&[(0, 1, 0, 8)], 1.),
        '▐' => (&[(4, 8, 0, 8)], 1.),
        '░' => (FULL, 0.25),
        '▒' => (FULL, 0.5),
        '▓' => (FULL, 0.75),
        '▔' => (&[(0, 8, 0, 1)], 1.),
        '▕' => (&[(7, 8, 0, 8)], 1.),
        '▖' => (&[LL], 1.),
        '▗' => (&[LR], 1.),
        '▘' => (&[UL], 1.),
        '▙' => (&[UL, LL, LR], 1.),
        '▚' => (&[UL, LR], 1.),
        '▛' => (&[UL, UR, LL], 1.),
        '▜' => (&[UL, UR, LR], 1.),
        '▝' => (&[UR], 1.),
        '▞' => (&[UR, LL], 1.),
        '▟' => (&[UR, LL, LR], 1.),
        _ => return None,
    })
}

fn push_block(blocks: &mut Vec<Block>, line: usize, col: usize, c: char, color: Hsla) -> bool {
    let Some((parts, alpha)) = block_parts(c) else {
        return false;
    };
    let mut color = color;
    color.a *= alpha;
    for &(x0, x1, y0, y1) in parts {
        let (x0, x1) = (col * 8 + x0 as usize, col * 8 + x1 as usize);
        match blocks.last_mut() {
            Some(b)
                if b.line == line && b.x1 == x0 && b.y0 == y0 && b.y1 == y1 && b.color == color =>
            {
                b.x1 = x1
            }
            _ => blocks.push(Block {
                line,
                x0,
                x1,
                y0,
                y1,
                color,
            }),
        }
    }
    true
}

struct CursorInfo {
    line: usize,
    col: usize,
    shape: CursorShape,
    wide: bool,
    text: Option<(String, Hsla, bool, bool)>,
}

struct Frame {
    segments: Vec<Segment>,
    rects: Vec<Rect>,
    blocks: Vec<Block>,
    cursor: Option<CursorInfo>,
    selection: Option<SelectionRange>,
    display_offset: usize,
    mode: TermMode,
    blinking: bool,
}

pub struct Prepaint {
    hitbox: Hitbox,
    geometry: Geometry,
    lines: Vec<(Point<Pixels>, ShapedLine)>,
    rects: Vec<Bounds<Pixels>>,
    rect_colors: Vec<Hsla>,
    selection: Vec<Bounds<Pixels>>,
    cursor: Option<(Bounds<Pixels>, CursorShape, Option<ShapedLine>)>,
    ime_bounds: Bounds<Pixels>,
    marked: Option<ShapedLine>,
}

fn fonts() -> [Font; 4] {
    let base = Font {
        family: theme::FONT_MONO.into(),
        features: FontFeatures::disable_ligatures(),
        fallbacks: None,
        weight: FontWeight::NORMAL,
        style: FontStyle::Normal,
    };
    let bold = Font {
        weight: FontWeight::BOLD,
        ..base.clone()
    };
    let italic = Font {
        style: FontStyle::Italic,
        ..base.clone()
    };
    let bold_italic = Font {
        style: FontStyle::Italic,
        ..bold.clone()
    };
    [base, bold, italic, bold_italic]
}

fn font_index(bold: bool, italic: bool) -> usize {
    bold as usize | ((italic as usize) << 1)
}

fn is_default_bg(color: Color) -> bool {
    matches!(color, Color::Named(NamedColor::Background))
}

/// Builds runs and rects for the visible rows. Called with the terminal locked, so it
/// only does cheap work: no shaping, no allocation per cell.
fn build_frame(term: &super::model::AlacTerm, focused: bool) -> Frame {
    let grid = term.grid();
    let lines = grid.screen_lines();
    let columns = grid.columns();
    let offset = grid.display_offset();
    let resolver = Resolver::new(term.colors());
    let mut segments: Vec<Segment> = Vec::with_capacity(lines * 2);
    let mut rects: Vec<Rect> = Vec::new();
    let mut blocks: Vec<Block> = Vec::new();
    let mut buf = [0u8; 4];

    for vline in 0..lines {
        let row = &grid[Line(vline as i32 - offset as i32)];
        let mut seg: Option<Segment> = None;
        let mut gap = 0usize;
        let mut col = 0usize;
        while col < columns {
            let cell: &Cell = &row[Column(col)];
            let flags = cell.flags;
            if flags.contains(Flags::WIDE_CHAR_SPACER) {
                col += 1;
                continue;
            }
            let wide = flags.contains(Flags::WIDE_CHAR);
            let width = if wide { 2 } else { 1 };

            let (fg, bg) = if flags.contains(Flags::INVERSE) {
                (cell.bg, cell.fg)
            } else {
                (cell.fg, cell.bg)
            };
            if !is_default_bg(bg) {
                let color = resolver.resolve(bg);
                match rects.last_mut() {
                    Some(r) if r.line == vline && r.end == col && r.color == color => {
                        r.end = col + width
                    }
                    _ => rects.push(Rect {
                        line: vline,
                        start: col,
                        end: col + width,
                        color,
                    }),
                }
            }

            let decorated = flags.intersects(Flags::ALL_UNDERLINES | Flags::STRIKEOUT);
            let blank = flags.contains(Flags::HIDDEN)
                || ((cell.c == ' ' || cell.c == '\t' || cell.c == '\0') && !decorated)
                || flags.contains(Flags::LEADING_WIDE_CHAR_SPACER);
            if blank {
                if seg.is_some() {
                    gap += width;
                }
                col += width;
                continue;
            }

            let mut color = if is_default_bg(fg) && flags.contains(Flags::INVERSE) {
                theme::background()
            } else {
                resolver.resolve(fg)
            };
            if flags.contains(Flags::DIM) {
                color.a *= 0.66;
            }
            let underline = flags
                .intersects(Flags::ALL_UNDERLINES)
                .then(|| UnderlineStyle {
                    thickness: px(1.),
                    color: Some(
                        cell.underline_color()
                            .map(|c| resolver.resolve(c))
                            .unwrap_or(color),
                    ),
                    wavy: flags.contains(Flags::UNDERCURL),
                });
            if !wide && push_block(&mut blocks, vline, col, cell.c, color) {
                if seg.is_some() {
                    gap += 1;
                }
                col += 1;
                continue;
            }
            let style = RunStyle {
                color,
                bold: flags.contains(Flags::BOLD),
                italic: flags.contains(Flags::ITALIC),
                underline,
                strike: flags.contains(Flags::STRIKEOUT),
            };

            // A wide glyph gets its own segment: shaping forces one cell per glyph.
            let contiguous = !wide && gap <= MAX_INLINE_GAP;
            match seg.as_mut() {
                Some(s) if contiguous => s.push_spaces(gap),
                _ => {
                    if let Some(s) = seg.take() {
                        segments.push(s);
                    }
                    seg = Some(Segment {
                        line: vline,
                        col,
                        text: String::new(),
                        runs: Vec::new(),
                    });
                }
            }
            gap = 0;
            let s = seg.as_mut().unwrap();
            let c = if cell.c == '\t' { ' ' } else { cell.c };
            s.push(c.encode_utf8(&mut buf), &style);
            if let Some(extra) = cell.zerowidth() {
                for z in extra {
                    s.push(z.encode_utf8(&mut buf), &style);
                }
            }
            if wide {
                segments.push(seg.take().unwrap());
            }
            col += width;
        }
        if let Some(s) = seg.take() {
            segments.push(s);
        }
    }

    let content = term.renderable_content();
    let cursor_point = content.cursor.point;
    let cursor_line = cursor_point.line.0 + offset as i32;
    let cursor = (content.cursor.shape != CursorShape::Hidden
        && cursor_line >= 0
        && (cursor_line as usize) < lines)
        .then(|| {
            let cell = &grid[cursor_point];
            let wide = cell.flags.contains(Flags::WIDE_CHAR);
            let text = (cell.c != ' ' && cell.c != '\0' && !cell.flags.contains(Flags::HIDDEN))
                .then(|| {
                    let mut s = cell.c.to_string();
                    if let Some(extra) = cell.zerowidth() {
                        s.extend(extra);
                    }
                    (
                        s,
                        theme::background(),
                        cell.flags.contains(Flags::BOLD),
                        cell.flags.contains(Flags::ITALIC),
                    )
                });
            CursorInfo {
                line: cursor_line as usize,
                col: cursor_point.column.0,
                shape: if focused {
                    content.cursor.shape
                } else {
                    CursorShape::HollowBlock
                },
                wide,
                text,
            }
        });

    Frame {
        segments,
        rects,
        blocks,
        cursor,
        selection: content.selection,
        display_offset: offset,
        mode: content.mode,
        blinking: term.cursor_style().blinking,
    }
}

impl Element for TerminalElement {
    type RequestLayoutState = ();
    type PrepaintState = Prepaint;

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, ()) {
        let mut style = Style::default();
        style.size.width = relative(1.).into();
        style.size.height = relative(1.).into();
        (window.request_layout(style, [], cx), ())
    }

    fn prepaint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) -> Prepaint {
        let font_size = px(FONT_SIZE);
        let fonts = fonts();
        let (cell_width, line_height) = self.view.read(cx).metrics(window);
        let scale = window.scale_factor();
        let snap = |v: Pixels| px((f32::from(v) * scale).round() / scale);

        let focused = self.focused;
        let (frame, origin, columns, lines) = self.terminal.update(cx, |terminal, _| {
            terminal.begin_frame();
            let term = terminal.term().clone();
            let mut term = term.lock();
            let alt = term.mode().contains(TermMode::ALT_SCREEN);
            let (pad_x, pad_y) = if alt { (0., 0.) } else { (PAD_X, PAD_Y) };
            let avail_w = f32::from(bounds.size.width) - 2. * pad_x;
            let avail_h = f32::from(bounds.size.height) - 2. * pad_y;
            let columns = ((avail_w / f32::from(cell_width)).floor() as usize).max(2);
            let lines = ((avail_h / f32::from(line_height)).floor() as usize).max(1);
            terminal.resize_locked(
                &mut term,
                columns,
                lines,
                f32::from(cell_width),
                f32::from(line_height),
            );
            let origin = point(
                snap(bounds.origin.x + px(pad_x)),
                snap(bounds.origin.y + px(pad_y)),
            );
            (build_frame(&term, focused), origin, columns, lines)
        });

        let geometry = Geometry {
            origin,
            cell_width,
            line_height,
            columns,
            lines,
            display_offset: frame.display_offset,
            mode: frame.mode,
        };
        let cell_origin = |line: usize, col: usize| {
            point(
                origin.x + cell_width * col as f32,
                origin.y + line_height * line as f32,
            )
        };

        let text_system = window.text_system().clone();
        let shaped_lines = frame
            .segments
            .into_iter()
            .map(|seg| {
                let runs: Vec<TextRun> = seg
                    .runs
                    .into_iter()
                    .map(|(len, style)| TextRun {
                        len,
                        font: fonts[font_index(style.bold, style.italic)].clone(),
                        color: style.color,
                        background_color: None,
                        underline: style.underline,
                        strikethrough: style.strike.then(|| StrikethroughStyle {
                            thickness: px(1.),
                            color: Some(style.color),
                        }),
                    })
                    .collect();
                let shaped =
                    text_system.shape_line(seg.text.into(), font_size, &runs, Some(cell_width));
                (cell_origin(seg.line, seg.col), shaped)
            })
            .collect();

        let mut rects = Vec::with_capacity(frame.rects.len());
        let mut rect_colors = Vec::with_capacity(frame.rects.len());
        // Edges snap to device pixels from the same formula, so neighbours share an
        // edge exactly: no seams, no overlap.
        let x_at = |eighths: usize| snap(origin.x + cell_width * (eighths as f32 / 8.));
        let y_at = |line: usize, eighths: u8| {
            snap(origin.y + line_height * line as f32 + line_height * (eighths as f32 / 8.))
        };
        for r in &frame.rects {
            let (left, right) = (x_at(r.start * 8), x_at(r.end * 8));
            let (top, bottom) = (y_at(r.line, 0), y_at(r.line, 8));
            rects.push(Bounds::new(
                point(left, top),
                size(right - left, bottom - top),
            ));
            rect_colors.push(r.color);
        }
        for b in &frame.blocks {
            let (left, right) = (x_at(b.x0), x_at(b.x1));
            let (top, bottom) = (y_at(b.line, b.y0), y_at(b.line, b.y1));
            rects.push(Bounds::new(
                point(left, top),
                size(right - left, bottom - top),
            ));
            rect_colors.push(b.color);
        }

        let mut selection = Vec::new();
        if let Some(range) = frame.selection {
            let offset = frame.display_offset as i32;
            let first = range.start.line.0 + offset;
            let last = range.end.line.0 + offset;
            for line in first.max(0)..=last.min(lines as i32 - 1) {
                let (start, end) = if range.is_block {
                    (range.start.column.0, range.end.column.0 + 1)
                } else {
                    let start = if line == first {
                        range.start.column.0
                    } else {
                        0
                    };
                    let end = if line == last {
                        range.end.column.0 + 1
                    } else {
                        columns
                    };
                    (start, end)
                };
                let o = cell_origin(line as usize, start);
                selection.push(Bounds::new(
                    o,
                    size(cell_width * end.saturating_sub(start) as f32, line_height),
                ));
            }
        }

        let mut ime_bounds = Bounds::new(cell_origin(0, 0), size(cell_width, line_height));
        let cursor = frame.cursor.map(|c| {
            let o = cell_origin(c.line, c.col);
            let width = cell_width * if c.wide { 2. } else { 1. };
            ime_bounds = Bounds::new(o, size(width, line_height));
            let text = (c.shape == CursorShape::Block)
                .then_some(c.text)
                .flatten()
                .map(|(text, color, bold, italic)| {
                    let len = text.len();
                    text_system.shape_line(
                        text.into(),
                        font_size,
                        &[TextRun {
                            len,
                            font: fonts[font_index(bold, italic)].clone(),
                            color,
                            background_color: None,
                            underline: None,
                            strikethrough: None,
                        }],
                        Some(cell_width),
                    )
                });
            let bounds = match c.shape {
                CursorShape::Beam => Bounds::new(o, size(px(2.), line_height)),
                CursorShape::Underline => {
                    Bounds::new(point(o.x, o.y + line_height - px(2.)), size(width, px(2.)))
                }
                _ => Bounds::new(o, size(width, line_height)),
            };
            (bounds, c.shape, text)
        });

        let marked = self.marked_text.as_ref().map(|text| {
            text_system.shape_line(
                text.clone(),
                font_size,
                &[TextRun {
                    len: text.len(),
                    font: fonts[0].clone(),
                    color: theme::content(),
                    background_color: None,
                    underline: Some(UnderlineStyle {
                        thickness: px(1.),
                        color: Some(theme::content()),
                        wavy: false,
                    }),
                    strikethrough: None,
                }],
                Some(cell_width),
            )
        });

        let blinking = frame.blinking;
        self.view.update(cx, |view, cx| {
            view.frame_painted(geometry, ime_bounds, blinking, cx)
        });

        let hitbox = window.insert_hitbox(bounds, HitboxBehavior::Normal);
        Prepaint {
            hitbox,
            geometry,
            lines: shaped_lines,
            rects,
            rect_colors,
            selection,
            cursor,
            ime_bounds,
            marked,
        }
    }

    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut (),
        p: &mut Prepaint,
        window: &mut Window,
        cx: &mut App,
    ) {
        window.handle_input(
            &self.focus,
            ElementInputHandler::new(bounds, self.view.clone()),
            cx,
        );
        let mouse_mode = p.geometry.mode.intersects(TermMode::MOUSE_MODE);
        window.set_cursor_style(
            if mouse_mode {
                CursorStyle::Arrow
            } else {
                CursorStyle::IBeam
            },
            &p.hitbox,
        );
        self.register_mouse(&p.hitbox, window);

        let line_height = p.geometry.line_height;
        window.with_content_mask(Some(ContentMask { bounds }), |window| {
            for (rect, color) in p.rects.iter().zip(&p.rect_colors) {
                window.paint_quad(fill(*rect, *color));
            }
            let selection = colors::selection(self.focused);
            for rect in &p.selection {
                window.paint_quad(fill(*rect, selection));
            }
            for (origin, line) in &p.lines {
                let _ = line.paint(*origin, line_height, TextAlign::Left, None, window, cx);
            }

            if let Some(marked) = &p.marked {
                let o = p.ime_bounds.origin;
                window.paint_quad(fill(
                    Bounds::new(o, size(marked.width, line_height)),
                    theme::background(),
                ));
                let _ = marked.paint(o, line_height, TextAlign::Left, None, window, cx);
            } else if let Some((rect, shape, text)) = &p.cursor {
                let accent = theme::accent();
                match shape {
                    CursorShape::HollowBlock => {
                        window.paint_quad(outline(*rect, accent, BorderStyle::Solid))
                    }
                    _ if !self.cursor_on => {}
                    CursorShape::Block => {
                        window.paint_quad(fill(*rect, accent));
                        if let Some(text) = text {
                            let _ = text.paint(
                                rect.origin,
                                line_height,
                                TextAlign::Left,
                                None,
                                window,
                                cx,
                            );
                        }
                    }
                    _ => window.paint_quad(fill(*rect, accent)),
                }
            }
        });
    }
}

impl TerminalElement {
    fn register_mouse(&self, hitbox: &Hitbox, window: &mut Window) {
        let view = self.view.clone();
        let hb = hitbox.clone();
        window.on_mouse_event(move |e: &MouseDownEvent, phase, window, cx| {
            if phase == DispatchPhase::Bubble && hb.is_hovered(window) {
                view.update(cx, |view, cx| view.mouse_down(e, window, cx));
            }
        });
        let view = self.view.clone();
        window.on_mouse_event(move |e: &MouseUpEvent, phase, window, cx| {
            if phase == DispatchPhase::Bubble {
                view.update(cx, |view, cx| view.mouse_up(e, window, cx));
            }
        });
        let view = self.view.clone();
        let hb = hitbox.clone();
        window.on_mouse_event(move |e: &MouseMoveEvent, phase, window, cx| {
            if phase == DispatchPhase::Bubble {
                let hovered = hb.is_hovered(window);
                view.update(cx, |view, cx| view.mouse_move(e, hovered, cx));
            }
        });
        let view = self.view.clone();
        let hb = hitbox.clone();
        window.on_mouse_event(move |e: &ScrollWheelEvent, phase, window, cx| {
            if phase == DispatchPhase::Bubble && hb.should_handle_scroll(window) {
                view.update(cx, |view, cx| view.scroll_wheel(e, cx));
                cx.stop_propagation();
            }
        });
    }
}
