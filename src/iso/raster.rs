//! A frame rasterized on the CPU with tiny-skia, for the Isometric tab.
//!
//! GPUI fills paths with at most 4× MSAA, which leaves 1 px lines at 30°
//! visibly stepped. tiny-skia computes exact pixel coverage, like a browser,
//! and draws arcs as cubic Béziers, so edges come out smooth at any scale.

use tiny_skia::{
    Color, FillRule, GradientStop, LineCap, LinearGradient, LineJoin, Paint as SkPaint, PathBuilder, Pixmap, PixmapPaint, Point,
    RadialGradient, SpreadMode, Stroke, StrokeDash, Transform,
};

use super::svg::{Palette, Rgb};
use super::{Item, Paint, Rect, Seg, arc_cubics};

/// Where a frame lands in a pixmap: screen units to device pixels.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Fit {
    pub k: f32,
    pub ox: f32,
    pub oy: f32,
}

impl Fit {
    /// Centers `extent` in a `w`×`h` px area with `pad` px around it.
    pub fn new(extent: Rect, w: f32, h: f32, pad: f32) -> Option<Fit> {
        let (ew, eh) = (extent[2] - extent[0], extent[3] - extent[1]);
        let (bw, bh) = (w - 2. * pad, h - 2. * pad);
        if ew <= 0. || eh <= 0. || bw <= 0. || bh <= 0. {
            return None;
        }
        let k = (bw / ew).min(bh / eh);
        Some(Fit {
            k,
            ox: pad + (bw - ew * k) / 2. - extent[0] * k,
            oy: pad + (bh - eh * k) / 2. - extent[1] * k,
        })
    }
}

fn color(c: Rgb, alpha: f32) -> Color {
    Color::from_rgba(c[0].clamp(0., 1.), c[1].clamp(0., 1.), c[2].clamp(0., 1.), alpha.clamp(0., 1.))
        .unwrap_or(Color::BLACK)
}

/// Paints `items` on the palette's ground into a `w`×`h` pixmap. `line` is
/// the stroke width in pixels (1.25 points times the display scale).
///
/// A run of items on one layer that is fading is drawn opaque into a pixmap
/// of its own and laid down once at the layer's opacity, the way a browser
/// fades a `<g opacity>`.
pub fn frame(items: &[Item], palette: &Palette, w: u32, h: u32, fit: Fit, line: f32) -> Option<Pixmap> {
    let mut pixmap = Pixmap::new(w.max(1), h.max(1))?;
    pixmap.fill(color(palette.ground, 1.));
    let mut rest = items;
    while let Some(first) = rest.first() {
        let n = rest.iter().take_while(|i| i.layer == first.layer).count();
        let (run, tail) = rest.split_at(n);
        rest = tail;
        if first.alpha <= 0. {
            continue;
        }
        if first.alpha >= 1. {
            for item in run {
                draw(&mut pixmap, item, palette, fit, line, (0., 0.));
            }
            continue;
        }
        let Some(b) = super::bounds(run) else { continue };
        let pad = line * 2.;
        let x0 = ((fit.ox + b[0] * fit.k - pad).floor()).max(0.);
        let y0 = ((fit.oy + b[1] * fit.k - pad).floor()).max(0.);
        let x1 = ((fit.ox + b[2] * fit.k + pad).ceil()).min(w as f32);
        let y1 = ((fit.oy + b[3] * fit.k + pad).ceil()).min(h as f32);
        if x1 <= x0 || y1 <= y0 {
            continue;
        }
        let Some(mut layer) = Pixmap::new((x1 - x0) as u32, (y1 - y0) as u32) else { continue };
        for item in run {
            draw(&mut layer, item, palette, fit, line, (x0, y0));
        }
        let paint = PixmapPaint { opacity: first.alpha, ..PixmapPaint::default() };
        pixmap.draw_pixmap(x0 as i32, y0 as i32, layer.as_ref(), &paint, Transform::identity(), None);
    }
    Some(pixmap)
}

/// One item, opaque, with the pixmap's origin at `origin` of the frame.
fn draw(pixmap: &mut Pixmap, item: &Item, palette: &Palette, fit: Fit, line: f32, origin: (f32, f32)) {
    let pt = |[x, y]: [f32; 2]| (fit.ox + x * fit.k - origin.0, fit.oy + y * fit.k - origin.1);
    let mut pb = PathBuilder::new();
    let mut at = [0., 0.];
    for seg in &item.path {
        match *seg {
            Seg::M(p) => {
                let (x, y) = pt(p);
                pb.move_to(x, y);
                at = p;
            }
            Seg::L(p) => {
                let (x, y) = pt(p);
                pb.line_to(x, y);
                at = p;
            }
            Seg::A { r, rot, large, sweep, to } => {
                for [c1, c2, end] in arc_cubics(at, r, rot, large, sweep, to) {
                    let ((x1, y1), (x2, y2), (x, y)) = (pt(c1), pt(c2), pt(end));
                    pb.cubic_to(x1, y1, x2, y2, x, y);
                }
                at = to;
            }
            Seg::C(c1, c2, to) => {
                let ((x1, y1), (x2, y2), (x, y)) = (pt(c1), pt(c2), pt(to));
                pb.cubic_to(x1, y1, x2, y2, x, y);
                at = to;
            }
            Seg::Z => pb.close(),
        }
    }
    let Some(path) = pb.finish() else { return };
    let mut paint = SkPaint { anti_alias: true, ..SkPaint::default() };
    // A frame `[ox, oy, ux, uy, vx, vy]` in screen units, as a pixel transform.
    let frame_tf = |f: [f32; 6]| {
        let (ox, oy) = pt([f[0], f[1]]);
        Transform::from_row(f[2] * fit.k, f[3] * fit.k, f[4] * fit.k, f[5] * fit.k, ox, oy)
    };
    match item.paint {
        Paint::Fill(tone) => {
            paint.set_color(color(palette.fill(tone), 1.));
            pixmap.fill_path(&path, &paint, FillRule::EvenOdd, Transform::identity(), None);
        }
        Paint::LineOnly(tone) => {
            if !palette.soft {
                paint.set_color(color(palette.fill(tone), 1.));
                pixmap.fill_path(&path, &paint, FillRule::EvenOdd, Transform::identity(), None);
            }
        }
        Paint::Shade { frame, hi } => {
            if !palette.soft {
                return;
            }
            // Light over a curved surface: a sheen where it faces the light,
            // falling off through the middle, darkening toward the rim.
            let white = |a: f32| Color::from_rgba(1., 1., 1., a).unwrap_or(Color::WHITE);
            let shade = |a: f32| {
                let k = if palette.dark { 0. } else { 0.05 };
                Color::from_rgba(k, k, k + 0.03, a).unwrap_or(Color::BLACK)
            };
            let stops = vec![
                GradientStop::new(0., white(if palette.dark { 0.32 } else { 0.62 })),
                GradientStop::new(0.18, white(if palette.dark { 0.12 } else { 0.22 })),
                GradientStop::new(0.5, white(0.)),
                GradientStop::new(0.8, shade(if palette.dark { 0.25 } else { 0.1 })),
                GradientStop::new(1., shade(if palette.dark { 0.55 } else { 0.32 })),
            ];
            let focal = Point::from_xy(hi[0] * 0.62, hi[1] * 0.62);
            if let Some(shader) =
                RadialGradient::new(focal, Point::from_xy(0., 0.), 1., stops, SpreadMode::Pad, frame_tf(frame))
            {
                paint.shader = shader;
                pixmap.fill_path(&path, &paint, FillRule::EvenOdd, Transform::identity(), None);
            }
        }
        Paint::Fade { a, b } => {
            if !palette.soft {
                return;
            }
            let (ax, ay) = pt(a);
            let (bx, by) = pt(b);
            let k = |al: f32| Color::from_rgba(0.02, 0.02, 0.06, al).unwrap_or(Color::BLACK);
            let deep = if palette.dark { 0.35 } else { 0.16 };
            let stops = vec![GradientStop::new(0., k(0.)), GradientStop::new(0.6, k(deep * 0.25)), GradientStop::new(1., k(deep))];
            if let Some(shader) =
                LinearGradient::new(Point::from_xy(ax, ay), Point::from_xy(bx, by), stops, SpreadMode::Pad, Transform::identity())
            {
                paint.shader = shader;
                pixmap.fill_path(&path, &paint, FillRule::EvenOdd, Transform::identity(), None);
            }
        }
        Paint::Soft { frame, alpha } => {
            if !palette.soft {
                return;
            }
            let ink = |a: f32| Color::from_rgba(0.02, 0.02, 0.05, a).unwrap_or(Color::BLACK);
            let stops = vec![GradientStop::new(0., ink(alpha)), GradientStop::new(0.55, ink(alpha * 0.55)), GradientStop::new(1., ink(0.))];
            if let Some(shader) =
                RadialGradient::new(Point::from_xy(0., 0.), Point::from_xy(0., 0.), 1., stops, SpreadMode::Pad, frame_tf(frame))
            {
                paint.shader = shader;
                pixmap.fill_path(&path, &paint, FillRule::EvenOdd, Transform::identity(), None);
            }
        }
        Paint::Stroke { ink, dashed, flow, wide } => {
            paint.set_color(color(palette.stroke(ink), 1.));
            let unit = line / 1.25;
            let stroke = Stroke {
                width: if wide { line * 2. } else { line },
                line_cap: LineCap::Round,
                line_join: LineJoin::Round,
                dash: if dashed { StrokeDash::new(vec![2. * unit, 3. * unit], (1. - flow.rem_euclid(1.)) * 5. * unit) } else { None },
                ..Stroke::default()
            };
            pixmap.stroke_path(&path, &paint, &stroke, Transform::identity(), None);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cubics_stay_on_the_ellipse() {
        // A quarter of a 30×10 ellipse turned 20°, in both directions.
        for sweep in [true, false] {
            let (from, to) = ([0., 0.], [24., 18.]);
            let pts = crate::iso::arc_points(from, [30., 10.], 20., false, sweep, to, 64);
            let cubics = arc_cubics(from, [30., 10.], 20., false, sweep, to);
            assert_eq!(cubics.last().unwrap()[2], to);
            // Midpoint of the first cubic against the densely sampled arc.
            let [c1, c2, e] = cubics[0];
            let mid = [
                0.125 * from[0] + 0.375 * c1[0] + 0.375 * c2[0] + 0.125 * e[0],
                0.125 * from[1] + 0.375 * c1[1] + 0.375 * c2[1] + 0.125 * e[1],
            ];
            let off = pts.iter().map(|p| (p[0] - mid[0]).hypot(p[1] - mid[1])).fold(f32::MAX, f32::min);
            assert!(off < 0.05, "sweep {sweep}: {off}");
        }
    }

    #[test]
    fn paints_a_scene() {
        let items = crate::iso::render(&(crate::iso::scenes::ALL[0].build)(0.3));
        let ext = crate::iso::bounds(&items).unwrap();
        let pal = Palette { ink: [0.16; 3], ground: [1.; 3], accent: [0.11, 0.69, 0.61], dark: false, soft: false };
        let fit = Fit::new(ext, 400., 300., 10.).unwrap();
        let pm = frame(&items, &pal, 400, 300, fit, 1.25).unwrap();
        assert!(pm.pixels().iter().any(|p| p.red() < 128), "some ink was drawn");
    }
}

/// `cargo test --release --bin arakun raster_speed -- --ignored --nocapture`
#[test]
#[ignore]
fn raster_speed() {
    let pal = Palette { ink: [0.9; 3], ground: [0.06, 0.07, 0.11], accent: [1., 0.7, 0.3], dark: true, soft: false };
    let (w, h) = (1720, 1200);
    let start = std::time::Instant::now();
    for i in 0..120 {
        let items = crate::iso::render(&(crate::iso::scenes::ALL[0].build)(i as f32 / 120.));
        let ext = crate::iso::bounds(&items).unwrap();
        let fit = Fit::new(ext, w as f32, h as f32, 64.).unwrap();
        let pm = frame(&items, &pal, w, h, fit, 2.5).unwrap();
        std::hint::black_box(pm);
    }
    eprintln!("{:.2} ms per {w}x{h} frame", start.elapsed().as_secs_f64() * 1000. / 120.);
}

/// `ISO_DUMP=dir cargo test --bin arakun dump_pngs -- --ignored`: frames of
/// every scene as PNGs, rendered the way the tab renders them.
#[test]
#[ignore]
fn dump_pngs() {
    let dir = std::env::var("ISO_DUMP").unwrap_or_else(|_| "/tmp/iso".into());
    std::fs::create_dir_all(&dir).unwrap();
    let pal = Palette { ink: [0.9, 0.91, 0.94], ground: [0.063, 0.071, 0.11], accent: [1., 0.71, 0.28], dark: true, soft: false };
    let steps: usize = std::env::var("ISO_STEPS").ok().and_then(|s| s.parse().ok()).unwrap_or(24);
    for (i, s) in crate::iso::scenes::ALL.iter().enumerate() {
        let ext = (0..=48)
            .filter_map(|j| crate::iso::bounds(&crate::iso::render(&(s.build)(j as f32 / 48.))))
            .reduce(|a, b| [a[0].min(b[0]), a[1].min(b[1]), a[2].max(b[2]), a[3].max(b[3])])
            .unwrap();
        for j in 0..steps {
            let t = j as f32 / steps as f32;
            let items = crate::iso::render(&(s.build)(t));
            let fit = Fit::new(ext, 1600., 1100., 40.).unwrap();
            let pm = frame(&items, &pal, 1600, 1100, fit, 2.5).unwrap();
            pm.save_png(format!("{dir}/png-{i}-{j:03}.png")).unwrap();
        }
    }
}
