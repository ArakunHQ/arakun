//! Text selection over one markdown surface (a reply, an expanded thought).
//!
//! Every piece of text the surface lays out registers a [`Seg`] in the
//! surface's [`TextMap`], numbered in reading order as if the pieces were one
//! string joined by newlines. A selection is a range in that numbering, so it
//! can start in a paragraph and end in a code block. [`SelText`] wraps each
//! piece: it records where the text landed and paints the selection and the
//! inline-code chips under the glyphs.

use std::cell::{Cell, RefCell};
use std::ops::Range;
use std::rc::Rc;

use super::search::Spot;

use gpui::{
    AnyElement, App, Bounds, Element, ElementId, GlobalElementId, Hsla, InspectorElementId,
    IntoElement, LayoutId, Pixels, Point, SharedString, TextLayout, TransformationMatrix, Window,
    fill, point, px, size,
};

pub struct Seg {
    /// Offset of the first byte in the surface's numbering.
    pub start: usize,
    pub text: SharedString,
    /// Local byte ranges that exist only for layout (chip padding); copying skips them.
    pub pads: Vec<Range<usize>>,
    pub layout: TextLayout,
    pub bounds: Rc<Cell<Option<Bounds<Pixels>>>>,
}

impl Seg {
    fn end(&self) -> usize {
        self.start + self.text.len()
    }
}

/// The text pieces of one surface, rebuilt on every render.
#[derive(Default)]
pub struct TextMap {
    segs: RefCell<Vec<Seg>>,
    /// Inline code that names a file, in the surface's numbering.
    files: RefCell<Vec<(Range<usize>, String)>>,
}

fn dist(v: Pixels, lo: Pixels, hi: Pixels) -> Pixels {
    if v < lo {
        lo - v
    } else if v > hi {
        v - hi
    } else {
        px(0.)
    }
}

impl TextMap {
    pub fn clear(&self) {
        self.segs.borrow_mut().clear();
        self.files.borrow_mut().clear();
    }

    pub fn file(&self, range: Range<usize>, path: String) {
        self.files.borrow_mut().push((range, path));
    }

    /// The file named by the inline code at `offset`.
    pub fn file_at(&self, offset: usize) -> Option<String> {
        self.files
            .borrow()
            .iter()
            .find(|(r, _)| r.start <= offset && offset < r.end)
            .map(|(_, p)| p.clone())
    }

    pub fn is_empty(&self) -> bool {
        self.segs.borrow().is_empty()
    }

    pub fn push(&self, seg: Seg) {
        self.segs.borrow_mut().push(seg);
    }

    /// The surface offset under `pos`, snapping to the nearest text when the
    /// point is in a gap or beside the text. None before anything was painted.
    pub fn hit(&self, pos: Point<Pixels>) -> Option<usize> {
        let segs = self.segs.borrow();
        let ready: Vec<(&Seg, Bounds<Pixels>)> = segs
            .iter()
            .filter_map(|s| s.bounds.get().map(|b| (s, b)))
            .collect();
        let (last, _) = *ready.last()?;
        let mut best: Option<(&Seg, Bounds<Pixels>, Pixels)> = None;
        for &(s, b) in &ready {
            if pos.y >= b.top() && pos.y < b.bottom() {
                let d = dist(pos.x, b.left(), b.right());
                if best.is_none_or(|(_, _, bd)| d < bd) {
                    best = Some((s, b, d));
                }
            }
        }
        if let Some((s, b, _)) = best {
            let x = if pos.x < b.left() {
                b.left()
            } else if pos.x > b.right() {
                b.right()
            } else {
                pos.x
            };
            let ix = s
                .layout
                .index_for_position(point(x, pos.y))
                .unwrap_or_else(|i| i)
                .min(s.text.len());
            return Some(s.start + ix);
        }
        for &(s, b) in &ready {
            if pos.y < b.top() {
                return Some(s.start);
            }
        }
        Some(last.end())
    }

    /// The selected text, pieces joined by newlines, chip padding left out.
    pub fn text_in(&self, range: Range<usize>) -> String {
        let segs = self.segs.borrow();
        let mut out = String::new();
        let mut first = true;
        for s in segs.iter() {
            let a = range.start.max(s.start);
            let b = range.end.min(s.end());
            if a >= b {
                continue;
            }
            if !first {
                out.push('\n');
            }
            first = false;
            let (a, b) = (a - s.start, b - s.start);
            let mut ix = a;
            let mut pads: Vec<&Range<usize>> =
                s.pads.iter().filter(|p| p.end > a && p.start < b).collect();
            pads.sort_by_key(|p| p.start);
            for p in pads {
                if p.start > ix {
                    out.push_str(slice(&s.text, ix, p.start));
                }
                ix = ix.max(p.end);
            }
            if ix < b {
                out.push_str(slice(&s.text, ix, b));
            }
        }
        out
    }

    /// The word around `offset` (double click).
    pub fn word_at(&self, offset: usize) -> Range<usize> {
        let segs = self.segs.borrow();
        let Some(s) = segs.iter().find(|s| offset >= s.start && offset <= s.end()) else {
            return offset..offset;
        };
        let local = offset - s.start;
        let word = |c: char| c.is_alphanumeric() || c == '_';
        let text: &str = &s.text;
        let mut a = local;
        while a > 0 {
            let Some(c) = text[..a].chars().next_back() else {
                break;
            };
            if !word(c) {
                break;
            }
            a -= c.len_utf8();
        }
        let mut b = local;
        while b < text.len() {
            let Some(c) = text[b..].chars().next() else {
                break;
            };
            if !word(c) {
                break;
            }
            b += c.len_utf8();
        }
        s.start + a..s.start + b
    }

    /// The whole piece around `offset` (triple click).
    pub fn piece_at(&self, offset: usize) -> Range<usize> {
        self.segs
            .borrow()
            .iter()
            .find(|s| offset >= s.start && offset <= s.end())
            .map_or(offset..offset, |s| s.start..s.end())
    }
}

fn slice(text: &str, a: usize, b: usize) -> &str {
    let mut a = a.min(text.len());
    let mut b = b.min(text.len());
    while a > 0 && !text.is_char_boundary(a) {
        a -= 1;
    }
    while b < text.len() && !text.is_char_boundary(b) {
        b += 1;
    }
    &text[a..b]
}

/// A rounded box drawn behind a run of text: inline code, a file reference.
#[derive(Clone)]
pub struct Chip {
    pub range: Range<usize>,
    /// Bytes from the chip's start to its text: the room the icon gets.
    pub lead: usize,
    /// Icon drawn in the chip's leading padding.
    pub icon: Option<SharedString>,
}

/// Wraps a text element (StyledText or InteractiveText around `layout`) to
/// record its bounds and paint chips and the selection beneath it.
pub struct SelText {
    inner: AnyElement,
    text: SharedString,
    layout: TextLayout,
    bounds: Rc<Cell<Option<Bounds<Pixels>>>>,
    chips: Vec<Chip>,
    selection: Option<Range<usize>>,
    marks: Vec<Range<usize>>,
    /// Search matches, and which one is current with where to report it.
    hits: Vec<Range<usize>>,
    current_hit: Option<(usize, Spot)>,
    chip_color: Hsla,
    icon_color: Hsla,
}

impl SelText {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        inner: AnyElement,
        text: SharedString,
        layout: TextLayout,
        bounds: Rc<Cell<Option<Bounds<Pixels>>>>,
        chips: Vec<Chip>,
        selection: Option<Range<usize>>,
        chip_color: Hsla,
        icon_color: Hsla,
    ) -> Self {
        SelText {
            inner,
            text,
            layout,
            bounds,
            chips,
            selection,
            marks: Vec::new(),
            hits: Vec::new(),
            current_hit: None,
            chip_color,
            icon_color,
        }
    }

    /// Local ranges drawn as annotated: a tint and an underline.
    pub fn marks(mut self, marks: Vec<Range<usize>>) -> Self {
        self.marks = marks;
        self
    }

    /// Local ranges matching the search; `current` indexes them and reports
    /// where it lands in `spot`.
    pub fn hits(mut self, hits: Vec<Range<usize>>, current: Option<usize>, spot: &Spot) -> Self {
        self.current_hit = current
            .filter(|&c| c < hits.len())
            .map(|c| (c, spot.clone()));
        self.hits = hits;
        self
    }
}

pub fn hit_color() -> Hsla {
    gpui::hsla(48. / 360., 1., 0.55, 0.30)
}

pub fn current_hit_color() -> Hsla {
    gpui::hsla(30. / 360., 1., 0.55, 0.65)
}

pub fn mark_color() -> Hsla {
    gpui::hsla(43. / 360., 0.96, 0.56, 1.)
}

pub fn selection_color() -> Hsla {
    crate::theme::text_selection()
}

/// Rectangles covering `range` of a laid-out text, one per visual line.
fn range_rects(
    layout: &TextLayout,
    text: &str,
    bounds: Bounds<Pixels>,
    range: Range<usize>,
) -> Vec<Bounds<Pixels>> {
    let lh = layout.line_height();
    let (Some(mut a), Some(b)) = (
        layout.position_for_index(range.start),
        layout.position_for_index(range.end),
    ) else {
        return Vec::new();
    };
    // An index where a line wraps reads as the end of the upper line; a range
    // starting there begins at the start of the next one.
    if b.y - a.y > px(1.)
        && let Some(c) = text.get(range.start..).and_then(|t| t.chars().next())
        && let Some(next) = layout.position_for_index(range.start + c.len_utf8())
        && next.y - a.y > px(1.)
    {
        a = point(bounds.left(), next.y);
    }
    if (a.y - b.y).abs() < px(1.) {
        return vec![Bounds::new(a, size(b.x - a.x, lh))];
    }
    let mut out = vec![Bounds::new(a, size(bounds.right() - a.x, lh))];
    let mut y = a.y + lh;
    while y + px(1.) < b.y {
        out.push(Bounds::new(
            point(bounds.left(), y),
            size(bounds.size.width, lh),
        ));
        y += lh;
    }
    out.push(Bounds::new(
        point(bounds.left(), b.y),
        size(b.x - bounds.left(), lh),
    ));
    out
}

/// The boxes of a chip, one per line it spans, each as wide as its own text.
/// `range_rects` runs a wrapped range to the edge of the text, which drew a long
/// empty tail after the part left on the upper line.
fn chip_rects(
    layout: &TextLayout,
    text: &str,
    bounds: Bounds<Pixels>,
    range: Range<usize>,
) -> Vec<Bounds<Pixels>> {
    let lh = layout.line_height();
    let Some(chip) = text.get(range.clone()) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    // A line holding only the chip's NBSP padding (gpui breaks there) gets no box:
    // it would show as an empty chip at the end of the line.
    let mut push = |y: Pixels, x0: Pixels, x1: Pixels, from: usize, to: usize| {
        if !text[from..to].chars().all(|c| c == '\u{a0}') {
            out.push(Bounds::new(point(x0, y), size(x1 - x0, lh)));
        }
    };
    let mut line: Option<(Pixels, Pixels, Pixels, usize)> = None;
    let mut last = range.start;
    for i in chip
        .char_indices()
        .map(|(i, c)| range.start + i + c.len_utf8())
    {
        let Some(at) = layout.position_for_index(i) else {
            continue;
        };
        let prev = i - text[..i].chars().next_back().map_or(1, char::len_utf8);
        line = match line {
            Some((y, x0, _, from)) if (at.y - y).abs() < px(1.) => Some((y, x0, at.x, from)),
            Some((y, x0, x1, from)) => {
                push(y, x0, x1, from, prev);
                Some((at.y, bounds.left(), at.x, prev))
            }
            None => {
                let start = layout.position_for_index(prev).unwrap_or(at);
                // The first character opens the next line when the chip starts at a wrap.
                if (at.y - start.y).abs() < px(1.) {
                    Some((at.y, start.x, at.x, prev))
                } else {
                    Some((at.y, bounds.left(), at.x, prev))
                }
            }
        };
        last = i;
    }
    if let Some((y, x0, x1, from)) = line {
        push(y, x0, x1, from, last);
    }
    out
}

impl Element for SelText {
    type RequestLayoutState = ();
    type PrepaintState = ();

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
        (self.inner.request_layout(window, cx), ())
    }

    fn prepaint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) {
        self.inner.prepaint(window, cx);
        self.bounds.set(Some(bounds));
        if let Some((i, spot)) = &self.current_hit
            && let Some(r) = self.hits.get(*i)
            && let Some(first) = range_rects(&self.layout, &self.text, bounds, r.clone()).first()
        {
            spot.set(Some(*first));
        }
    }

    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut (),
        _: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) {
        let lh = self.layout.line_height();
        // Chips sit 2px inside the line box: a 20px chip on a 24px line.
        let inset = ((lh - px(20.)) / 2.).max(px(1.));
        for chip in &self.chips {
            let rects: Vec<_> = chip_rects(&self.layout, &self.text, bounds, chip.range.clone())
                .into_iter()
                .filter(|r| r.size.width > px(1.))
                .collect();
            let last = rects.len().saturating_sub(1);
            for (i, r) in rects.iter().enumerate() {
                // A pixel past the padding on each side, so the text breathes. Where a
                // chip wraps, its padding stays at its two ends; the cut gets some too.
                let (left, right) = (
                    if i > 0 { px(4.) } else { px(1.) },
                    if i < last { px(4.) } else { px(1.) },
                );
                let r = Bounds::new(
                    point(r.origin.x - left, r.origin.y + inset),
                    size(r.size.width + left + right, lh - inset * 2.),
                );
                window.paint_quad(fill(r, self.chip_color).corner_radii(px(5.)));
                if i == 0
                    && let Some(icon) = &chip.icon
                {
                    // Sized to the text and centred in the room its padding leaves,
                    // which follows the font size; a fixed icon ran into small text.
                    let lead = chip.range.start..(chip.range.start + chip.lead).min(chip.range.end);
                    let room = range_rects(&self.layout, &self.text, bounds, lead)
                        .first()
                        .map_or(px(18.), |l| l.size.width + px(1.));
                    let s = (lh * 0.6)
                        .clamp(px(11.), px(16.))
                        .min(room - px(4.))
                        .max(px(8.));
                    let icon_bounds = Bounds::new(
                        point(
                            r.origin.x + (room - s) / 2.,
                            r.origin.y + (r.size.height - s) / 2.,
                        ),
                        size(s, s),
                    );
                    let _ = window.paint_svg(
                        icon_bounds,
                        icon.clone(),
                        None,
                        TransformationMatrix::unit(),
                        self.icon_color,
                        cx,
                    );
                }
            }
        }
        for m in self.marks.iter().filter(|m| m.start < m.end) {
            for r in range_rects(&self.layout, &self.text, bounds, m.clone()) {
                window.paint_quad(fill(r, mark_color().opacity(0.14)));
                let underline = Bounds::new(
                    point(r.origin.x, r.origin.y + r.size.height - px(2.)),
                    size(r.size.width, px(1.5)),
                );
                window.paint_quad(fill(underline, mark_color().opacity(0.7)));
            }
        }
        let current = self.current_hit.as_ref().map(|(i, _)| *i);
        for (i, h) in self
            .hits
            .iter()
            .enumerate()
            .filter(|(_, h)| h.start < h.end)
        {
            let color = if Some(i) == current {
                current_hit_color()
            } else {
                hit_color()
            };
            for r in range_rects(&self.layout, &self.text, bounds, h.clone()) {
                window.paint_quad(fill(r, color).corner_radii(px(3.)));
            }
        }
        if let Some(sel) = self.selection.clone().filter(|s| s.start < s.end) {
            for r in range_rects(&self.layout, &self.text, bounds, sel) {
                window.paint_quad(fill(r, selection_color()));
            }
        }
        self.inner.paint(window, cx);
    }
}

impl IntoElement for SelText {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seg(start: usize, text: &str, pads: Vec<Range<usize>>) -> Seg {
        Seg {
            start,
            text: SharedString::from(text.to_string()),
            pads,
            layout: TextLayout::default(),
            bounds: Rc::new(Cell::new(None)),
        }
    }

    #[test]
    fn copies_across_pieces_without_padding() {
        let map = TextMap::default();
        map.push(seg(0, "hello world", vec![]));
        // "\u{2002}x\u{2002}": an inline-code chip with its padding.
        let chip = "see \u{2002}x\u{2002} here";
        let pad = "\u{2002}".len();
        map.push(seg(12, chip, vec![4..4 + pad, 5 + pad..5 + 2 * pad]));
        assert_eq!(map.text_in(6..11), "world");
        assert_eq!(map.text_in(6..12 + chip.len()), "world\nsee x here");
        assert_eq!(map.word_at(2), 0..5);
        assert_eq!(map.piece_at(13), 12..12 + chip.len());
    }
}
