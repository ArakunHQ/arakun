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

    pub fn surface(&self, l: f32) -> Hsla {
        hsla(
            self.hue / 360.,
            self.tint,
            (l + self.bg - BASE_L).clamp(0., 1.),
            1.,
        )
    }

    pub fn background(&self) -> Hsla {
        self.surface(BASE_L)
    }

    pub fn dialog(&self) -> Hsla {
        self.surface(BASE_L + DIALOG_LIFT)
    }

    pub fn content(&self) -> Hsla {
        hsla(
            self.hue / 360.,
            (self.tint * 0.8).min(CONTENT_S_MAX),
            CONTENT_L,
            1.,
        )
    }

    pub fn accent(&self) -> Hsla {
        rgb(self.accent).into()
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

// The palette in use, as plain numbers so every color call stays a few loads.
static HUE_X100: AtomicU32 = AtomicU32::new(22_600);
static TINT_X1000: AtomicU32 = AtomicU32::new(280);
static BG_X1000: AtomicU32 = AtomicU32::new(85);
static ACCENT: AtomicU32 = AtomicU32::new(AMBER);

/// The palette every color comes from, until another is set.
fn current() -> Palette {
    Palette {
        id: String::new(),
        name: String::new(),
        hue: HUE_X100.load(Ordering::Relaxed) as f32 / 100.,
        tint: TINT_X1000.load(Ordering::Relaxed) as f32 / 1000.,
        bg: BG_X1000.load(Ordering::Relaxed) as f32 / 1000.,
        accent: ACCENT.load(Ordering::Relaxed),
    }
}

pub fn set_palette(p: &Palette) {
    let p = p.clone().clamped();
    HUE_X100.store((p.hue * 100.).round() as u32, Ordering::Relaxed);
    TINT_X1000.store((p.tint * 1000.).round() as u32, Ordering::Relaxed);
    BG_X1000.store((p.bg * 1000.).round() as u32, Ordering::Relaxed);
    ACCENT.store(p.accent, Ordering::Relaxed);
}

fn hue() -> f32 {
    HUE_X100.load(Ordering::Relaxed) as f32 / 36_000.
}

/// A tinted surface at the given lightness, for panels that sit between the
/// named ones. Lightness is given as for the default palette and moves with
/// the palette's background.
pub fn surface(l: f32) -> Hsla {
    current().surface(l)
}

/// `#10121c` in the default palette.
pub fn background() -> Hsla {
    current().background()
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
    current().content()
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
    current().dialog()
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
        for c in [p.background(), p.dialog(), p.content()] {
            assert!(c.s > 0.1, "{c:?} is gray");
        }
        assert!((p.accent().h * 360. - 211.).abs() > 90.);
    }

    #[test]
    fn every_built_in_palette_stays_readable() {
        for p in Palette::built_in() {
            assert!(
                contrast(p.content(), p.background()) > 12.,
                "{} text",
                p.name
            );
            assert!(
                contrast(p.background(), p.accent()) > 5.,
                "{} accent",
                p.name
            );
            assert!(contrast(warn(), p.background()) > 7., "{} warn", p.name);
        }
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

    /// `top` drawn over `under`, as the screen shows it.
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

    #[test]
    fn every_gray_ramp_keeps_text_readable() {
        for g in Grays::ALL {
            let r = g.ramp();
            let on = |alpha: f32| contrast(over(mix(alpha), background()), background());
            // WCAG AA: 4.5 for body text, 3 for large text and hints.
            assert!(on(r.text_muted) >= 7., "{g:?} muted {}", on(r.text_muted));
            assert!(
                on(r.text_subtle) >= 4.5,
                "{g:?} subtle {}",
                on(r.text_subtle)
            );
            assert!(on(r.text_faint) >= 2.5, "{g:?} faint {}", on(r.text_faint));
            assert!(r.text_muted > r.text_subtle && r.text_subtle > r.text_faint);
            assert!(r.border_subtle < r.border && r.border < r.border_strong);
            assert!(r.fill_raised < r.fill_hover && r.fill_hover < r.fill_selected);
        }
    }

    #[test]
    fn warnings_do_not_look_like_the_accent() {
        let warn_h = Hsla::from(warn()).h * 360.;
        assert!((warn_h - accent().h * 360.).abs() >= 12.);
    }
}
