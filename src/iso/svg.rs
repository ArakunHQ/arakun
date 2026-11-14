//! Colors of the line-iso style and the SVG a frame exports to.
//!
//! The exported file themes like illos': every fill is a `color-mix` of
//! three CSS variables (`--iso-ink`, `--iso-ground`, `--iso-accent`) plus
//! `--iso-dark` (0 or 1), so a page can restyle it without touching paths.

use std::fmt::Write as _;

use super::{Ink, Item, Paint, Seg, Tone, bounds};

/// An sRGB color, 0..1 per channel.
pub type Rgb = [f32; 3];

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Palette {
    pub ink: Rgb,
    pub ground: Rgb,
    pub accent: Rgb,
    pub dark: bool,
    /// The soft style: gradients, contact shadows and lighter outlines.
    pub soft: bool,
}

impl Palette {
    /// How much of the ink (or accent) each fill mixes into the ground,
    /// in light and in dark: light shades the walls, dark lifts the top.
    fn share(tone: Tone) -> (f32, f32) {
        match tone {
            Tone::Custom(..) => (1., 1.),
            Tone::Top => (0., 0.10),
            Tone::Left => (0.04, 0.05),
            Tone::Right => (0.09, 0.02),
            Tone::Recess => (0.06, 0.04),
            Tone::AccentTop => (0.10, 0.30),
            Tone::AccentLeft => (0.18, 0.22),
            Tone::AccentRight => (0.26, 0.16),
            // Dark parts: deep in light, a step above the ground in dark.
            Tone::MidTop => (0.2, 0.32),
            Tone::MidRight => (0.3, 0.24),
            Tone::DarkTop => (0.62, 0.2),
            Tone::DarkLeft => (0.72, 0.15),
            Tone::DarkRight => (0.8, 0.11),
            Tone::Ink | Tone::Accent => (1., 1.),
            Tone::Mute => (0.45, 0.45),
            Tone::Ground => (0., 0.),
        }
    }

    fn accented(tone: Tone) -> bool {
        matches!(tone, Tone::AccentTop | Tone::AccentLeft | Tone::AccentRight | Tone::Accent)
    }

    pub fn fill(&self, tone: Tone) -> Rgb {
        if let Tone::Custom(c, face) = tone {
            let c = c.map(|v| v as f32 / 255.);
            // Lit top a touch lighter, the shaded wall darker; in the dark
            // look the whole color sinks toward the night.
            let k = [1.08, 0.92, 0.76][face.min(2) as usize] * if self.dark { 0.82 } else { 1. };
            return c.map(|v| (v * k).min(1.));
        }
        let (light, dark) = Self::share(tone);
        let over = if Self::accented(tone) { self.accent } else { self.ink };
        mix(over, self.ground, if self.dark { dark } else { light })
    }

    pub fn stroke(&self, ink: Ink) -> Rgb {
        let c = self.stroke_full(ink);
        // Soft: outlines settle back so the shading carries the form.
        if self.soft { mix(c, self.ground, 0.55) } else { c }
    }

    fn stroke_full(&self, ink: Ink) -> Rgb {
        match ink {
            Ink::Edge => self.ink,
            Ink::Mute => mix(self.ink, self.ground, 0.45),
            Ink::Accent => self.accent,
            Ink::AccentMute => mix(self.accent, self.ground, 0.45),
        }
    }
}

/// `color-mix(in srgb, a p, b)`.
pub fn mix(a: Rgb, b: Rgb, p: f32) -> Rgb {
    [0, 1, 2].map(|i| a[i] * p + b[i] * (1. - p))
}

pub fn hex(c: Rgb) -> String {
    let b = |v: f32| (v.clamp(0., 1.) * 255.).round() as u8;
    format!("#{:02x}{:02x}{:02x}", b(c[0]), b(c[1]), b(c[2]))
}

pub fn num(v: f32) -> String {
    let s = format!("{:.2}", v);
    let s = s.trim_end_matches('0').trim_end_matches('.');
    if s == "-0" { "0".into() } else { s.into() }
}

pub fn path_data(path: &[Seg], dx: f32, dy: f32) -> String {
    let mut d = String::new();
    for seg in path {
        match *seg {
            Seg::M([x, y]) => {
                let _ = write!(d, "M{} {}", num(x + dx), num(y + dy));
            }
            Seg::L([x, y]) => {
                let _ = write!(d, "L{} {}", num(x + dx), num(y + dy));
            }
            Seg::A { r, rot, large, sweep, to: [x, y] } => {
                let _ = write!(
                    d,
                    "A{} {} {} {} {} {} {}",
                    num(r[0]),
                    num(r[1]),
                    num(rot),
                    large as u8,
                    sweep as u8,
                    num(x + dx),
                    num(y + dy)
                );
            }
            Seg::C(c1, c2, [x, y]) => {
                let _ = write!(
                    d,
                    "C{} {} {} {} {} {}",
                    num(c1[0] + dx),
                    num(c1[1] + dy),
                    num(c2[0] + dx),
                    num(c2[1] + dy),
                    num(x + dx),
                    num(y + dy)
                );
            }
            Seg::Z => d.push('Z'),
        }
    }
    d
}

fn tone_class(tone: Tone) -> &'static str {
    match tone {
        Tone::Custom(..) => "custom",
        Tone::Top => "top",
        Tone::Left => "left",
        Tone::Right => "right",
        Tone::Recess => "recess",
        Tone::AccentTop => "atop",
        Tone::AccentLeft => "aleft",
        Tone::AccentRight => "aright",
        Tone::MidTop => "mtop",
        Tone::MidRight => "mright",
        Tone::DarkTop => "dtop",
        Tone::DarkLeft => "dleft",
        Tone::DarkRight => "dright",
        Tone::Ink => "dot",
        Tone::Mute => "mdot",
        Tone::Ground => "ground",
        Tone::Accent => "adot",
    }
}

fn ink_class(ink: Ink) -> &'static str {
    match ink {
        Ink::Edge => "e",
        Ink::Mute => "m",
        Ink::Accent => "ae",
        Ink::AccentMute => "am",
    }
}

/// The `<style>` body every exported SVG shares: the theme variables and a
/// class per tone and ink.
pub fn style(id: &str, palette: &Palette) -> String {
    let d = if palette.dark { 1 } else { 0 };
    let mut css = format!(
        "@scope (svg.iso-{id}) {{\n:scope{{--d:var(--iso-dark,{d});--g:var(--iso-ground,{g});--ink:var(--iso-ink,{i});--accent:var(--iso-accent,{a})}}\n\
         path{{vector-effect:non-scaling-stroke;stroke-linecap:round;stroke-linejoin:round;fill-rule:evenodd}}\n",
        g = hex(palette.ground),
        i = hex(palette.ink),
        a = hex(palette.accent),
    );
    for tone in [
        Tone::Top,
        Tone::Left,
        Tone::Right,
        Tone::Recess,
        Tone::AccentTop,
        Tone::AccentLeft,
        Tone::AccentRight,
        Tone::MidTop,
        Tone::MidRight,
        Tone::DarkTop,
        Tone::DarkLeft,
        Tone::DarkRight,
        Tone::Ink,
        Tone::Mute,
        Tone::Ground,
        Tone::Accent,
    ] {
        let (light, dark) = Palette::share(tone);
        let over = if Palette::accented(tone) { "--accent" } else { "--ink" };
        let _ = writeln!(
            css,
            ".{c}{{fill:color-mix(in srgb,var({over}) calc(var(--d) * {dk}% + (1 - var(--d)) * {lt}%),var(--g))}}",
            c = tone_class(tone),
            dk = num(dark * 100.),
            lt = num(light * 100.),
        );
    }
    css.push_str(
        ".e,.m,.ae,.am{fill:none;stroke-width:1px}.e{stroke:var(--ink)}.m{stroke:color-mix(in srgb,var(--ink) 45%,var(--g))}\
         .ae{stroke:var(--accent)}.am{stroke:color-mix(in srgb,var(--accent) 45%,var(--g))}.d{stroke-dasharray:2 3}.w{stroke-width:2px}\n}",
    );
    css
}

pub fn svg_id(title: &str) -> String {
    title.to_lowercase().replace(|c: char| !c.is_ascii_alphanumeric(), "-")
}

/// How an item is painted in SVG: its classes, an inline fill for custom
/// colors, and the dash phase (0..1) of a flowing dashed line. `None` for
/// the soft look's raster-only paints.
pub fn paint_of(item: &Item, palette: &Palette) -> Option<(String, String, Option<f32>)> {
    Some(match item.paint {
        Paint::Fill(tone) | Paint::LineOnly(tone) => match tone {
            Tone::Custom(..) => (String::new(), format!("fill:{}", hex(palette.fill(tone))), None),
            _ => (tone_class(tone).to_string(), String::new(), None),
        },
        Paint::Shade { .. } | Paint::Soft { .. } | Paint::Fade { .. } => return None,
        Paint::Stroke { ink, dashed, flow, wide } => (
            format!("{}{}{}", ink_class(ink), if dashed { " d" } else { "" }, if wide { " w" } else { "" }),
            String::new(),
            (dashed && flow != 0.).then_some(flow),
        ),
    })
}

/// The `stroke-dashoffset` that puts a `2 3` dash at phase `flow`.
pub fn dash_offset(flow: f32) -> f32 {
    (1. - flow.rem_euclid(1.)) * 5.
}

/// One frame as a standalone SVG, `pad` units of margin around it.
pub fn frame(title: &str, items: &[Item], palette: &Palette, pad: f32) -> String {
    let b = bounds(items).unwrap_or([0., 0., 1., 1.]);
    let (w, h) = (b[2] - b[0] + 2. * pad, b[3] - b[1] + 2. * pad);
    let (dx, dy) = (pad - b[0], pad - b[1]);
    let id = svg_id(title);
    let mut s = String::new();
    let _ = writeln!(
        s,
        r#"<svg class="iso-{id}" xmlns="http://www.w3.org/2000/svg" width="{w}" height="{h}" viewBox="0 0 {w} {h}" fill="none" role="img"><title>{title}</title>"#,
        w = num(w),
        h = num(h),
    );
    let _ = writeln!(s, "<style>\n{}\n</style>", style(&id, palette));
    // Items of a fading layer go in one <g opacity>, so the layer fades as
    // a whole instead of showing its own back edges through its faces.
    let mut rest = items;
    while let Some(first) = rest.first() {
        let n = rest.iter().take_while(|i| i.layer == first.layer).count();
        let (run, tail) = rest.split_at(n);
        rest = tail;
        if first.alpha <= 0. {
            continue;
        }
        let fading = first.alpha < 1.;
        if fading {
            let _ = writeln!(s, r#"<g opacity="{}">"#, num(first.alpha));
        }
        for item in run {
            let Some((class, fill, flow)) = paint_of(item, palette) else { continue };
            let style = match flow {
                Some(f) => format!(r#" style="stroke-dashoffset:{}""#, num(dash_offset(f))),
                None if !fill.is_empty() => format!(r#" style="{fill}""#),
                None => String::new(),
            };
            let _ = writeln!(s, r#"<path class="{class}"{style} d="{}"/>"#, path_data(&item.path, dx, dy));
        }
        if fading {
            s.push_str("</g>\n");
        }
    }
    s.push_str("</svg>\n");
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exports_arcs_not_polylines() {
        let items = crate::iso::render(&(crate::iso::scenes::ALL[0].build)(0.));
        let pal = Palette { ink: [0.16; 3], ground: [1.; 3], accent: [0.11, 0.69, 0.61], dark: false, soft: false };
        let svg = frame("Usage billing", &items, &pal, 16.);
        assert!(svg.starts_with("<svg class=\"iso-usage-billing\""));
        assert!(svg.contains('A'), "rounded corners are written as arcs");
        assert!(svg.trim_end().ends_with("</svg>"));
    }
}

/// `ISO_DUMP=dir cargo test --bin arakun dump_frames -- --ignored` writes
/// a few frames of every scene, in both looks, to look at without the app.
#[test]
#[ignore]
fn dump_frames() {
    let dir = std::env::var("ISO_DUMP").unwrap_or_else(|_| "/tmp/iso".into());
    std::fs::create_dir_all(&dir).unwrap();
    let paper = Palette { ink: [0.165; 3], ground: [1.; 3], accent: [0.114, 0.686, 0.612], dark: false, soft: false };
    let night = Palette { ink: [0.9, 0.91, 0.94], ground: [0.063, 0.071, 0.11], accent: [1., 0.71, 0.28], dark: true, soft: false };
    for (i, s) in crate::iso::scenes::ALL.iter().enumerate() {
        for t in [0., 0.2, 0.45, 0.62, 0.75, 0.9] {
            let items = crate::iso::render(&(s.build)(t));
            for (look, pal) in [("paper", &paper), ("night", &night)] {
                let path = format!("{dir}/{i}-{look}-{:03}.svg", (t * 100.) as u32);
                std::fs::write(path, frame(s.name, &items, pal, 16.)).unwrap();
            }
        }
    }
}
