//! Arakun's colors. Every surface comes from one [`Palette`] (a tint, how
//! dark the background is, and an accent) plus alpha mixes of the content
//! color. The default is night blue (hue 226) with an amber accent; users
//! pick another palette or make their own in Settings › Appearance.

use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU32, Ordering};

use gpui::{Hsla, Rgba, hsla, rgb};

/// How added and removed lines are told apart.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DiffPalette {
    /// Green and red.
    Default,
    /// Blue and orange, which red-green colorblindness keeps apart.
    Colorblind,
    /// Brighter text and stronger line backgrounds.
    HighContrast,
}

impl DiffPalette {
    pub const ALL: [DiffPalette; 3] = [
        DiffPalette::Default,
        DiffPalette::Colorblind,
        DiffPalette::HighContrast,
    ];

    pub fn id(self) -> &'static str {
        match self {
            DiffPalette::Default => "default",
            DiffPalette::Colorblind => "colorblind",
            DiffPalette::HighContrast => "high_contrast",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            DiffPalette::Default => "Default",
            DiffPalette::Colorblind => "Colorblind",
            DiffPalette::HighContrast => "High contrast",
        }
    }

    pub fn from_id(id: &str) -> Self {
        Self::ALL
            .into_iter()
            .find(|p| p.id() == id)
            .unwrap_or(DiffPalette::Default)
    }

    /// The added and removed text colors.
    pub fn swatch(self) -> (Rgba, Rgba) {
        let (add, del, _) = self.colors();
        (rgb(add.0), rgb(del.0))
    }

    /// Text, line bar and line background alpha for (added, removed).
    fn colors(self) -> ((u32, u32), (u32, u32), f32) {
        match self {
            DiffPalette::Default => ((0x6ee7b7, 0x10b981), (0xfda4af, 0xf43f5e), 0.15),
            DiffPalette::Colorblind => ((0x7cc4ff, 0x3b82f6), (0xffb26b, 0xf97316), 0.16),
            DiffPalette::HighContrast => ((0x8dffc8, 0x22e58f), (0xffa3b1, 0xff4d6a), 0.26),
        }
    }
}

static DIFF_PALETTE: AtomicU8 = AtomicU8::new(0);
static GLASS: AtomicBool = AtomicBool::new(false);

pub fn diff_palette() -> DiffPalette {
    DiffPalette::ALL[DIFF_PALETTE.load(Ordering::Relaxed) as usize % 3]
}

pub fn set_diff_palette(p: DiffPalette) {
    let ix = DiffPalette::ALL.iter().position(|x| *x == p).unwrap_or(0);
    DIFF_PALETTE.store(ix as u8, Ordering::Relaxed);
}

/// Translucent window surfaces over the compositor's blur.
pub fn glass() -> bool {
    GLASS.load(Ordering::Relaxed)
}

pub fn set_glass(on: bool) {
    GLASS.store(on, Ordering::Relaxed);
}

pub fn window_background() -> gpui::WindowBackgroundAppearance {
    if glass() {
        gpui::WindowBackgroundAppearance::Blurred
    } else {
        gpui::WindowBackgroundAppearance::Opaque
    }
}

/// The background lightness the surface offsets were tuned on: callers of
/// [`surface`] pass lightness values relative to it.
const BASE_L: f32 = 0.085;
/// Dialogs sit this much lighter than the background.
const DIALOG_LIFT: f32 = 0.035;
const CONTENT_L: f32 = 0.92;
/// The content color carries a little of the tint, never more than this.
const CONTENT_S_MAX: f32 = 0.22;

/// What a palette is made of. Everything else is derived from these four.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Palette {
    /// Stable id: `night` for a built-in, `custom-…` for the user's.
    pub id: String,
    pub name: String,
    /// Hue of the tint, in degrees.
    pub hue: f32,
    /// How strongly surfaces carry the tint, 0 (gray) to 0.6.
    pub tint: f32,
    /// Lightness of the background, 0.03 to 0.16.
    pub bg: f32,
    /// The accent, `0xRRGGBB`.
    pub accent: u32,
}

/// The accent the brand started with, `#ffb547`.
pub const AMBER: u32 = 0xffb547;

impl Palette {
    fn new(id: &str, name: &str, hue: f32, tint: f32, bg: f32, accent: u32) -> Self {
        Palette {
            id: id.into(),
            name: name.into(),
            hue,
            tint,
            bg,
            accent,
        }
    }

    /// The palettes Arakun ships, the default first.
    pub fn built_in() -> Vec<Palette> {
        vec![
            Palette::new("night", "Arakun Night", 226., 0.28, 0.085, AMBER),
            Palette::new("graphite", "Graphite", 230., 0.06, 0.075, AMBER),
            Palette::new("midnight", "Midnight", 250., 0.45, 0.07, 0x8b9dff),
            Palette::new("forest", "Forest", 150., 0.22, 0.07, 0x5fd38d),
            Palette::new("rose", "Rosé", 340., 0.18, 0.085, 0xff8fab),
        ]
    }

    pub fn night() -> Palette {
        Palette::built_in().remove(0)
    }

    /// Kept inside what the rest of the theme was tuned for, so a typed or
    /// pasted value can't make text unreadable.
    pub fn clamped(mut self) -> Palette {
        self.hue = self.hue.rem_euclid(360.);
        self.tint = self.tint.clamp(0., 0.6);
        self.bg = self.bg.clamp(0.03, 0.16);
        self.accent &= 0xff_ffff;
        self
    }

    /// A surface at lightness `l` as for the default palette, on this
    /// palette's background made [readable](Palette::readable_bg).
    pub fn surface(&self, l: f32) -> Hsla {
        tinted(self.hue, self.tint, self.readable_bg(), l)
    }

    pub fn background(&self) -> Hsla {
        self.surface(BASE_L)
    }

    /// Text, made [readable](Palette::readable_content_l).
    pub fn content(&self) -> Hsla {
        self.content_at(self.readable_content_l())
    }

    fn content_at(&self, l: f32) -> Hsla {
        hsla(self.hue / 360., (self.tint * 0.8).min(CONTENT_S_MAX), l, 1.)
    }

    /// The accent, made [readable](Palette::readable_accent).
    pub fn accent(&self) -> Hsla {
        rgb(self.readable_accent()).into()
    }

    /// The background lightness the palette is drawn with: the one asked
    /// for, or darker when text or warnings wouldn't read on it. Text is
    /// already near white, so only a darker background can bring it up to
    /// the contrast the default was built to.
    pub fn readable_bg(&self) -> f32 {
        let text = self.content_at(CONTENT_L);
        // Both only fall as the background gets lighter.
        let reads = |bg: f32| {
            let back = tinted(self.hue, self.tint, bg, BASE_L);
            contrast(text, back) >= MIN_TEXT * MARGIN && contrast(warn(), back) >= MIN_WARN * MARGIN
        };
        if reads(self.bg) {
            return self.bg;
        }
        // Black always reads; find the lightest background that still does.
        let (mut lo, mut hi) = (0., self.bg);
        for _ in 0..16 {
            let mid = (lo + hi) / 2.;
            if reads(mid) {
                lo = mid;
            } else {
                hi = mid;
            }
        }
        lo
    }

    /// The lightness text is drawn at: the default's, or lighter when the
    /// gray text roles (text at an alpha over the background) wouldn't read.
    /// Those barely change with the background, since they carry some of it,
    /// so only lighter text helps them; some tints at full strength need it.
    pub fn readable_content_l(&self) -> f32 {
        let back = tinted(self.hue, self.tint, self.readable_bg(), BASE_L);
        let least = |role: fn(&Ramp) -> f32| {
            Grays::ALL
                .iter()
                .map(|g| role(&g.ramp()))
                .fold(f32::MAX, f32::min)
        };
        let (muted, subtle, faint) = (
            least(|r| r.text_muted),
            least(|r| r.text_subtle),
            least(|r| r.text_faint),
        );
        // Each only rises as the text gets lighter.
        let reads = |l: f32| {
            let on = |alpha: f32| contrast(over(self.content_at(l).opacity(alpha), back), back);
            on(muted) >= MIN_MUTED && on(subtle) >= MIN_SUBTLE && on(faint) >= MIN_FAINT
        };
        if reads(CONTENT_L) {
            return CONTENT_L;
        }
        let (mut lo, mut hi) = (CONTENT_L, 1.);
        for _ in 0..16 {
            let mid = (lo + hi) / 2.;
            if reads(mid) {
                hi = mid;
            } else {
                lo = mid;
            }
        }
        hi
    }

    /// The accent as drawn: the one asked for, made lighter (same hue)
    /// until the background reads on it, since text on an accent button is
    /// drawn in the background color.
    pub fn readable_accent(&self) -> u32 {
        let back = tinted(self.hue, self.tint, self.readable_bg(), BASE_L);
        let asked = self.accent & 0xff_ffff;
        let reads = |c: u32| contrast(back, rgb(c)) >= MIN_ACCENT * MARGIN;
        if reads(asked) {
            return asked;
        }
        let base = Hsla::from(rgb(asked));
        let at = |l: f32| to_hex(hsla(base.h, base.s, l, 1.));
        // White always reads on a background text reads on.
        let (mut lo, mut hi) = (base.l, 1.);
        for _ in 0..16 {
            let mid = (lo + hi) / 2.;
            if reads(at(mid)) {
                hi = mid;
            } else {
                lo = mid;
            }
        }
        at(hi)
    }

    /// One line that carries the palette, to share it:
    /// `arakun:palette/v1:265,32,7.5,ff7a59,Sunset`.
    pub fn share_code(&self) -> String {
        format!(
            "arakun:palette/v1:{},{},{},{:06x},{}",
            self.hue.round(),
            (self.tint * 100.).round(),
            (self.bg * 1000.).round() / 10.,
            self.accent,
            self.name.replace(',', " ")
        )
    }

    /// A palette from [`Palette::share_code`]'s line; None when it isn't one.
    pub fn from_share_code(code: &str) -> Option<Palette> {
        let rest = code.trim().strip_prefix("arakun:palette/v1:")?;
        let mut parts = rest.splitn(5, ',');
        let hue: f32 = parts.next()?.trim().parse().ok()?;
        let tint: f32 = parts.next()?.trim().parse().ok()?;
        let bg: f32 = parts.next()?.trim().parse().ok()?;
        let accent = u32::from_str_radix(parts.next()?.trim().trim_start_matches('#'), 16).ok()?;
        let name = parts
            .next()
            .map(str::trim)
            .filter(|n| !n.is_empty())
            .unwrap_or("Shared");
        if !(hue.is_finite() && tint.is_finite() && bg.is_finite()) {
            return None;
        }
        Some(Palette::new("", name, hue, tint / 100., bg / 100., accent).clamped())
    }
}

/// The contrast every palette keeps against its background: text, the
/// gray text roles at their faintest ramp, and the accent and warnings
/// (WCAG AAA for body text is 7).
const MIN_TEXT: f32 = 12.;
const MIN_MUTED: f32 = 7.;
const MIN_SUBTLE: f32 = 4.5;
const MIN_FAINT: f32 = 2.5;
const MIN_ACCENT: f32 = 7.;
const MIN_WARN: f32 = 7.;
/// Headroom over the minimums for a single color, so rounding to 8-bit
/// color can't drop it under them. Gray text is an alpha mix and is checked
/// as drawn, so it needs none, and the default's sits too close to have any.
const MARGIN: f32 = 1.02;

/// A tinted surface at lightness `l` as for the default palette, moved by
/// how far `bg` is from the default background.
fn tinted(hue: f32, tint: f32, bg: f32, l: f32) -> Hsla {
    hsla(hue / 360., tint, (l + bg - BASE_L).clamp(0., 1.), 1.)
}

/// `top` drawn over an opaque `under`, as the screen shows it.
fn over(top: Hsla, under: Hsla) -> Rgba {
    let (t, u): (Rgba, Rgba) = (top.into(), under.into());
    let a = top.a;
    Rgba {
        r: t.r * a + u.r * (1. - a),
        g: t.g * a + u.g * (1. - a),
        b: t.b * a + u.b * (1. - a),
        a: 1.,
    }
}

fn to_hex(c: impl Into<Rgba>) -> u32 {
    let c: Rgba = c.into();
    let b = |v: f32| (v.clamp(0., 1.) * 255.).round() as u32;
    (b(c.r) << 16) | (b(c.g) << 8) | b(c.b)
}

fn luminance(c: impl Into<Rgba>) -> f32 {
    let c: Rgba = c.into();
    let lin = |v: f32| {
        if v <= 0.04045 {
            v / 12.92
        } else {
            ((v + 0.055) / 1.055).powf(2.4)
        }
    };
    0.2126 * lin(c.r) + 0.7152 * lin(c.g) + 0.0722 * lin(c.b)
}

/// WCAG contrast ratio of two colors, 1 to 21.
pub fn contrast(a: impl Into<Rgba>, b: impl Into<Rgba>) -> f32 {
    let (a, b) = (luminance(a), luminance(b));
    (a.max(b) + 0.05) / (a.min(b) + 0.05)
}

// The palette in use, as plain numbers so every color call stays a few
// loads. Background and accent are stored already made readable.
static HUE_X100: AtomicU32 = AtomicU32::new(22_600);
static TINT_X1000: AtomicU32 = AtomicU32::new(280);
// These two are f32 bits, stored exact so the colors drawn are the ones
// checked.
static BG: AtomicU32 = AtomicU32::new(BASE_L.to_bits());
static CONTENT_LIGHT: AtomicU32 = AtomicU32::new(CONTENT_L.to_bits());
static ACCENT: AtomicU32 = AtomicU32::new(AMBER);

pub fn set_palette(p: &Palette) {
    let p = p.clone().clamped();
    HUE_X100.store((p.hue * 100.).round() as u32, Ordering::Relaxed);
    TINT_X1000.store((p.tint * 1000.).round() as u32, Ordering::Relaxed);
    BG.store(p.readable_bg().to_bits(), Ordering::Relaxed);
    CONTENT_LIGHT.store(p.readable_content_l().to_bits(), Ordering::Relaxed);
    ACCENT.store(p.readable_accent(), Ordering::Relaxed);
}

/// The palette in use, without making it readable again: [`set_palette`]
/// already did, and these run on every color call.
fn current_surface(l: f32) -> Hsla {
    tinted(
        HUE_X100.load(Ordering::Relaxed) as f32 / 100.,
        TINT_X1000.load(Ordering::Relaxed) as f32 / 1000.,
        f32::from_bits(BG.load(Ordering::Relaxed)),
        l,
    )
}

fn current_content() -> Hsla {
    hsla(
        hue(),
        (TINT_X1000.load(Ordering::Relaxed) as f32 / 1000. * 0.8).min(CONTENT_S_MAX),
        f32::from_bits(CONTENT_LIGHT.load(Ordering::Relaxed)),
        1.,
    )
}

fn hue() -> f32 {
    HUE_X100.load(Ordering::Relaxed) as f32 / 36_000.
}

/// A tinted surface at the given lightness, for panels that sit between the
/// named ones. Lightness is given as for the default palette and moves with
/// the palette's background.
pub fn surface(l: f32) -> Hsla {
    current_surface(l)
}

/// `#10121c` in the default palette.
pub fn background() -> Hsla {
    current_surface(BASE_L)
}

/// The window itself: see-through with glass on. Everything painted with
/// `background()` (text on accents, popovers, the terminal) stays opaque.
pub fn window_bg() -> Hsla {
    background().opacity(if glass() { 0.78 } else { 1. })
}

/// The same tone as the content area: the floating cards separate by gap
/// and border, not by a darker sidebar.
pub fn sidebar() -> Hsla {
    background().opacity(if glass() { 0.62 } else { 1. })
}

/// `#e6e8ef` in the default palette.
pub fn content() -> Hsla {
    current_content()
}

/// Content at the given strength over transparent.
pub fn mix(strength: f32) -> Hsla {
    content().opacity(strength)
}

// ---------------------------------------------------------------------------
// Roles: what a gray is for, not how strong it is. Views take these instead
// of `mix(0.45)`-style numbers, so every secondary text, border and hover in
// the app is the same gray. The ramp behind them is one choice (`Grays`).

/// How far apart the grays of the roles sit.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Grays {
    /// Even steps between text levels.
    Balanced,
    /// Brighter secondary text and firmer borders: the default, since
    /// secondary text on the night-blue background needs the extra lift.
    Crisp,
    /// Quieter secondary text and hairline borders.
    Soft,
}

impl Grays {
    pub const ALL: [Grays; 3] = [Grays::Balanced, Grays::Crisp, Grays::Soft];

    pub fn label(self) -> &'static str {
        match self {
            Grays::Balanced => "Balanced",
            Grays::Crisp => "Crisp",
            Grays::Soft => "Soft",
        }
    }

    pub fn id(self) -> &'static str {
        match self {
            Grays::Balanced => "balanced",
            Grays::Crisp => "crisp",
            Grays::Soft => "soft",
        }
    }

    pub fn from_id(id: &str) -> Self {
        Self::ALL
            .into_iter()
            .find(|g| g.id() == id)
            .unwrap_or(Grays::Crisp)
    }

    /// Content alpha for each role.
    pub fn ramp(self) -> Ramp {
        match self {
            Grays::Balanced => Ramp {
                text_muted: 0.72,
                text_subtle: 0.52,
                text_faint: 0.36,
                border_subtle: 0.06,
                border: 0.10,
                border_strong: 0.16,
                fill_raised: 0.03,
                fill_hover: 0.06,
                fill_selected: 0.10,
            },
            Grays::Crisp => Ramp {
                text_muted: 0.80,
                text_subtle: 0.60,
                text_faint: 0.42,
                border_subtle: 0.08,
                border: 0.13,
                border_strong: 0.20,
                fill_raised: 0.04,
                fill_hover: 0.07,
                fill_selected: 0.12,
            },
            Grays::Soft => Ramp {
                text_muted: 0.68,
                text_subtle: 0.50,
                text_faint: 0.34,
                border_subtle: 0.05,
                border: 0.08,
                border_strong: 0.13,
                fill_raised: 0.025,
                fill_hover: 0.05,
                fill_selected: 0.09,
            },
        }
    }
}

/// The alphas of content color each role uses.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Ramp {
    pub text_muted: f32,
    pub text_subtle: f32,
    pub text_faint: f32,
    pub border_subtle: f32,
    pub border: f32,
    pub border_strong: f32,
    pub fill_raised: f32,
    pub fill_hover: f32,
    pub fill_selected: f32,
}

/// Crisp until the saved choice is read: index 1 of `Grays::ALL`.
static GRAYS: AtomicU8 = AtomicU8::new(1);

pub fn grays() -> Grays {
    Grays::ALL[GRAYS.load(Ordering::Relaxed) as usize % Grays::ALL.len()]
}

pub fn set_grays(g: Grays) {
    let ix = Grays::ALL.iter().position(|x| *x == g).unwrap_or(0);
    GRAYS.store(ix as u8, Ordering::Relaxed);
}

fn ramp() -> Ramp {
    grays().ramp()
}

/// Primary text: titles, values, what is selected.
pub fn text() -> Hsla {
    content()
}
/// Body text that is not the point of the row: names, descriptions.
pub fn text_muted() -> Hsla {
    mix(ramp().text_muted)
}
/// Labels, units, hints, column headers.
pub fn text_subtle() -> Hsla {
    mix(ramp().text_subtle)
}
/// Placeholders, disabled text, axis ticks.
pub fn text_faint() -> Hsla {
    mix(ramp().text_faint)
}
/// Rules between rows and inside cards.
pub fn border_subtle() -> Hsla {
    mix(ramp().border_subtle)
}
/// Card and control outlines.
pub fn border() -> Hsla {
    mix(ramp().border)
}
/// Inputs and outlines that must be seen at a glance.
pub fn border_strong() -> Hsla {
    mix(ramp().border_strong)
}
/// A card or well a step above the page.
pub fn fill_raised() -> Hsla {
    mix(ramp().fill_raised)
}
/// What the pointer is over.
pub fn fill_hover() -> Hsla {
    mix(ramp().fill_hover)
}
/// The current item, the pressed segment.
pub fn fill_selected() -> Hsla {
    mix(ramp().fill_selected)
}

pub fn stroke() -> Hsla {
    border_subtle()
}
pub fn hover() -> Hsla {
    fill_hover()
}
pub fn selection() -> Hsla {
    fill_selected()
}
pub fn selection_strong() -> Hsla {
    mix(0.12)
}
pub fn selection_hover() -> Hsla {
    mix(0.15)
}

/// Dialogs, menus and popovers.
pub fn dialog() -> Hsla {
    current_surface(BASE_L + DIALOG_LIFT)
}

/// Lemon yellow, apart from the amber accent. Always shown with an icon.
pub fn warn() -> Rgba {
    rgb(0xfde047)
}

/// The palette's accent; `#ffb547` in the default one.
pub fn accent() -> Hsla {
    rgb(ACCENT.load(Ordering::Relaxed)).into()
}

/// Selected text: a light night blue, so amber stays for the accent,
/// search hits and notes.
pub fn text_selection() -> Hsla {
    hsla(hue(), 0.6, 0.62, 0.32)
}
pub fn diff_add() -> Rgba {
    rgb(diff_palette().colors().0.0)
}
pub fn diff_del() -> Rgba {
    rgb(diff_palette().colors().1.0)
}
/// The bar at the left edge of an added line.
pub fn diff_add_bar() -> Rgba {
    rgb(diff_palette().colors().0.1)
}
pub fn diff_del_bar() -> Rgba {
    rgb(diff_palette().colors().1.1)
}
pub fn diff_add_bg() -> Hsla {
    Hsla::from(diff_add_bar()).opacity(diff_palette().colors().2)
}
pub fn diff_del_bg() -> Hsla {
    Hsla::from(diff_del_bar()).opacity(diff_palette().colors().2)
}
pub fn danger() -> Rgba {
    rgb(0xe81123)
}

// States in GitHub's own dark-mode colors, so a run or a pull request reads
// the same here as on github.com. Links and work in progress take the blue of
// the logo's stars instead of GitHub's.
pub const SUCCESS: u32 = 0x3fb950;
pub const ERROR: u32 = 0xf85149;
/// Running, queued, flaky, needs a look.
pub const ATTENTION: u32 = 0xd29922;
pub const LINK: u32 = 0x86a6f2;

pub fn success() -> Rgba {
    rgb(SUCCESS)
}
pub fn error() -> Rgba {
    rgb(ERROR)
}
pub fn attention() -> Rgba {
    rgb(ATTENTION)
}
pub fn link() -> Rgba {
    rgb(LINK)
}

pub fn pr_open() -> Rgba {
    success()
}
pub fn pr_merged() -> Rgba {
    rgb(0xa371f7)
}
pub fn pr_closed() -> Rgba {
    error()
}
pub fn pr_draft() -> Rgba {
    rgb(0x9198a1)
}

/// Hues for things told apart only by color (chart series, project logos,
/// memory kinds, file types), at even lightness so none stands out: the stars'
/// blue, the accent's amber, then the rest of the night sky.
pub const CATEGORY: [u32; 8] = [
    0x86a6f2, // blue
    0xffb547, // amber
    0x6fd3a0, // green
    0xb394f5, // violet
    0xf5826f, // coral
    0x5cc8db, // cyan
    0xee8fc0, // rose
    0xd9c27e, // sand
];

/// What WebKitGTK resolves `system-ui` to here (GTK 4 default).
#[cfg(target_os = "linux")]
pub const FONT_SANS: &str = "Adwaita Sans";
#[cfg(target_os = "linux")]
pub const FONT_MONO: &str = "JetBrainsMono NFM";
/// Elsewhere neither Linux font is installed, and the fallback is proportional:
/// terminal cells (sized from 'm') spread its letters apart, and the no-break
/// spaces that make room for a file chip's icon get too narrow. Geist ships in assets/fonts.
#[cfg(not(target_os = "linux"))]
pub const FONT_SANS: &str = "Geist";
#[cfg(not(target_os = "linux"))]
pub const FONT_MONO: &str = "Geist Mono";

#[cfg(test)]
mod tests {
    use super::*;

    fn hex(c: impl Into<Rgba>) -> u32 {
        let c: Rgba = c.into();
        let ch = |v: f32| (v.clamp(0., 1.) * 255.).round() as u32;
        ch(c.r) << 16 | ch(c.g) << 8 | ch(c.b)
    }

    /// Each channel within 1 of `want`, since HSL to RGB rounds.
    fn near(c: impl Into<Rgba>, want: u32) -> bool {
        let got = hex(c);
        (0..3).all(|i| ((got >> (i * 8)) & 0xff).abs_diff((want >> (i * 8)) & 0xff) <= 1)
    }

    #[test]
    fn palette_matches_the_brand() {
        // The default palette, read directly: tests run in parallel and the
        // current one is global.
        let p = Palette::night();
        assert!(
            near(p.background(), 0x10121c),
            "background {:06x}",
            hex(p.background())
        );
        assert!(
            near(p.content(), 0xe6e8ef),
            "content {:06x}",
            hex(p.content())
        );
        assert!(near(p.accent(), 0xffb547), "accent {:06x}", hex(p.accent()));
        assert_eq!(hex(warn()), 0xfde047);
    }

    #[test]
    fn surfaces_are_tinted_and_the_accent_is_not_blue() {
        let p = Palette::night();
        for c in [p.background(), p.surface(BASE_L + DIALOG_LIFT), p.content()] {
            assert!(c.s > 0.1, "{c:?} is gray");
        }
        assert!((p.accent().h * 360. - 211.).abs() > 90.);
    }

    /// The contrast the single night palette was held to before palettes
    /// could change: text 12, accent and warnings 7, and for every gray
    /// ramp muted 7, subtle 4.5 and faint 2.5 (WCAG AA is 4.5 for body
    /// text, 3 for large text and hints).
    fn assert_readable(p: &Palette) {
        let (bg, text) = (p.background(), p.content());
        let what = format!(
            "{} ({}, {}, {}, {:06x})",
            p.name, p.hue, p.tint, p.bg, p.accent
        );
        assert!(contrast(text, bg) > 12., "{what} text");
        assert!(contrast(bg, p.accent()) > 7., "{what} accent");
        assert!(contrast(warn(), bg) > 7., "{what} warn");
        for g in Grays::ALL {
            let r = g.ramp();
            let on = |alpha: f32| contrast(over(text.opacity(alpha), bg), bg);
            assert!(on(r.text_muted) >= 7., "{what} {g:?} muted");
            assert!(on(r.text_subtle) >= 4.5, "{what} {g:?} subtle");
            assert!(on(r.text_faint) >= 2.5, "{what} {g:?} faint");
        }
    }

    #[test]
    fn every_built_in_palette_stays_readable() {
        for p in Palette::built_in() {
            assert_readable(&p);
            // They read as designed, without being darkened or lightened.
            assert_eq!(p.readable_bg(), p.bg, "{} background", p.name);
            assert_eq!(p.readable_accent(), p.accent, "{} accent", p.name);
        }
    }

    #[test]
    fn every_palette_the_editor_can_make_stays_readable() {
        // The editor's accents, and ones it can't offer but a share line can.
        let accents = [
            AMBER, 0xff7a59, 0xff8fab, 0xb394f5, 0x8b9dff, 0x86a6f2, 0x5fd4c4, 0x5fd38d, 0x000000,
            0x0000ff, 0x7a1f1f, 0x404040,
        ];
        for hue in (0..360).step_by(30) {
            for tint in [0., 0.15, 0.3, 0.45, 0.6] {
                for bg in [0.03, 0.06, 0.085, 0.1, 0.12, 0.14, 0.16] {
                    for accent in accents {
                        let p = Palette::new("", "", hue as f32, tint, bg, accent);
                        assert_readable(&p);
                    }
                }
            }
        }
    }

    #[test]
    fn readable_colors_change_only_what_they_must() {
        // Light enough to need a darker background, with an accent too dark.
        let p = Palette::new("", "", 60., 0.6, 0.16, 0x0000ff);
        assert!(p.readable_bg() < p.bg);
        let lifted = Hsla::from(rgb(p.readable_accent()));
        assert!((lifted.h * 360. - 240.).abs() < 1., "keeps its hue");
        // A palette that reads already is drawn as asked.
        let p = Palette::new("", "", 200., 0.2, 0.06, 0x5fd4c4);
        assert_eq!((p.readable_bg(), p.readable_accent()), (p.bg, p.accent));
    }

    #[test]
    fn a_palette_survives_its_share_code() {
        let p = Palette::new("custom-1", "Sunset", 265., 0.32, 0.075, 0xff7a59);
        let code = p.share_code();
        assert_eq!(code, "arakun:palette/v1:265,32,7.5,ff7a59,Sunset");
        let back = Palette::from_share_code(&code).unwrap();
        assert_eq!(
            (
                back.hue,
                back.tint,
                back.bg,
                back.accent,
                back.name.as_str()
            ),
            (265., 0.32, 0.075, 0xff7a59, "Sunset")
        );
        assert!(Palette::from_share_code("palette:265,32").is_none());
        assert!(Palette::from_share_code("arakun:palette/v1:x,32,7.5,ff7a59").is_none());
        // Out of range values are pulled back in, not refused.
        let wild = Palette::from_share_code("arakun:palette/v1:-30,99,90,1ff7a59").unwrap();
        assert_eq!(
            (wild.hue, wild.tint, wild.bg, wild.accent),
            (330., 0.6, 0.16, 0xff7a59)
        );
    }

    #[test]
    fn every_gray_ramp_is_ordered() {
        for g in Grays::ALL {
            let r = g.ramp();
            assert!(r.text_muted > r.text_subtle && r.text_subtle > r.text_faint);
            assert!(r.border_subtle < r.border && r.border < r.border_strong);
            assert!(r.fill_raised < r.fill_hover && r.fill_hover < r.fill_selected);
        }
    }

    #[test]
    fn warnings_do_not_look_like_the_accent() {
        let warn_h = Hsla::from(warn()).h * 360.;
        for p in Palette::built_in() {
            assert!(
                (warn_h - p.accent().h * 360.).abs() >= 12.,
                "{} accent",
                p.name
            );
        }
    }
}
