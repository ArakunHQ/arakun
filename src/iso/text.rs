//! Text as outlines: Geist Mono glyphs turned into paths, so labels are as
//! sharp as the lines around them and export into the SVG as shapes.

use std::sync::OnceLock;

use ttf_parser::{Face, OutlineBuilder};

use super::{P2, Seg};

static FONT: &[u8] = include_bytes!("../../assets/fonts/GeistMono-Regular.ttf");

fn font() -> Option<&'static Face<'static>> {
    static FACE: OnceLock<Option<Face<'static>>> = OnceLock::new();
    FACE.get_or_init(|| Face::parse(FONT, 0).ok()).as_ref()
}

/// Collects a glyph's outline in screen units, y down.
struct Outline {
    path: Vec<Seg>,
    origin: P2,
    scale: f32,
    at: P2,
}

impl Outline {
    fn p(&self, x: f32, y: f32) -> P2 {
        [self.origin[0] + x * self.scale, self.origin[1] - y * self.scale]
    }
}

impl OutlineBuilder for Outline {
    fn move_to(&mut self, x: f32, y: f32) {
        let p = self.p(x, y);
        self.path.push(Seg::M(p));
        self.at = p;
    }
    fn line_to(&mut self, x: f32, y: f32) {
        let p = self.p(x, y);
        self.path.push(Seg::L(p));
        self.at = p;
    }
    fn quad_to(&mut self, x1: f32, y1: f32, x: f32, y: f32) {
        // A quadratic is the cubic with controls 2/3 of the way to its one.
        let (q, p, a) = (self.p(x1, y1), self.p(x, y), self.at);
        let c1 = [a[0] + 2. / 3. * (q[0] - a[0]), a[1] + 2. / 3. * (q[1] - a[1])];
        let c2 = [p[0] + 2. / 3. * (q[0] - p[0]), p[1] + 2. / 3. * (q[1] - p[1])];
        self.path.push(Seg::C(c1, c2, p));
        self.at = p;
    }
    fn curve_to(&mut self, x1: f32, y1: f32, x2: f32, y2: f32, x: f32, y: f32) {
        let p = self.p(x, y);
        self.path.push(Seg::C(self.p(x1, y1), self.p(x2, y2), p));
        self.at = p;
    }
    fn close(&mut self) {
        self.path.push(Seg::Z);
    }
}

/// `text` set at `size` (cap height and spacing scale with it), with
/// `tracking` extra units between letters. `at` is the left, middle or right
/// end of the line by `align` (0, 0.5, 1), on the line's vertical middle.
pub fn outline(text: &str, at: P2, size: f32, tracking: f32, align: f32) -> Vec<Seg> {
    let Some(face) = font() else { return Vec::new() };
    let scale = size / face.units_per_em() as f32;
    let glyphs: Vec<_> = text.chars().map(|c| face.glyph_index(c)).collect();
    let advance = |g: Option<ttf_parser::GlyphId>| {
        g.and_then(|g| face.glyph_hor_advance(g)).unwrap_or(face.units_per_em() / 2) as f32 * scale
    };
    let width: f32 = glyphs.iter().map(|&g| advance(g)).sum::<f32>() + tracking * glyphs.len().saturating_sub(1) as f32;
    let cap = face.capital_height().map_or(face.units_per_em() as f32 * 0.7, f32::from) * scale;
    let mut x = at[0] - width * align;
    let baseline = at[1] + cap / 2.;
    let mut out = Outline { path: Vec::new(), origin: [0., 0.], scale, at: [0., 0.] };
    for g in glyphs {
        if let Some(id) = g {
            out.origin = [x, baseline];
            face.outline_glyph(id, &mut out);
        }
        x += advance(g) + tracking;
    }
    out.path
}

/// The letter spacing for `text` at `size`: capitals get air between them,
/// running text none.
pub fn tracking(text: &str, size: f32) -> f32 {
    if text.chars().any(char::is_lowercase) { 0. } else { size * 0.12 }
}

/// How wide `text` sets, for laying out around it.
pub fn width(text: &str, size: f32, tracking: f32) -> f32 {
    let Some(face) = font() else { return 0. };
    let scale = size / face.units_per_em() as f32;
    let n = text.chars().count();
    text.chars()
        .map(|c| face.glyph_index(c).and_then(|g| face.glyph_hor_advance(g)).unwrap_or(0) as f32 * scale)
        .sum::<f32>()
        + tracking * n.saturating_sub(1) as f32
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sets_glyphs_as_closed_curves() {
        let path = outline("3.4 GHZ", [0., 0.], 10., 1., 0.);
        assert!(path.iter().any(|s| matches!(s, Seg::C(..))), "curved glyphs come out as curves");
        assert!(path.iter().filter(|s| matches!(s, Seg::Z)).count() >= 6);
        assert!(width("GHZ", 10., 1.) > width("GH", 10., 1.));
    }
}
