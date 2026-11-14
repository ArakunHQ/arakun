//! Arakun's dark palette: night-blue surfaces (hue 226) and an amber accent.
//! Every surface comes from a few lightness values plus alpha mixes of the
//! content color.

use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};

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

const HUE: f32 = 226. / 360.;
const SURFACE_S: f32 = 0.28;
const CONTENT_S: f32 = 0.22;
const BG_L: f32 = 0.085;
const SIDEBAR_L: f32 = 0.085;
const DIALOG_L: f32 = 0.12;
const CONTENT_L: f32 = 0.92;

/// A night-blue surface at the given lightness, for panels that sit between
/// the named ones.
pub fn surface(l: f32) -> Hsla {
    hsla(HUE, SURFACE_S, l, 1.)
}

/// `#10121c`
pub fn background() -> Hsla {
    surface(BG_L)
}

/// The window itself: see-through with glass on. Everything painted with
/// `background()` (text on accents, popovers, the terminal) stays opaque.
pub fn window_bg() -> Hsla {
    hsla(HUE, SURFACE_S, BG_L, if glass() { 0.78 } else { 1. })
}

/// `#10121c`, the same tone as the content area: the floating cards
/// separate by gap and border, not by a darker sidebar.
pub fn sidebar() -> Hsla {
    hsla(HUE, SURFACE_S, SIDEBAR_L, if glass() { 0.62 } else { 1. })
}

/// `#e6e8ef`
pub fn content() -> Hsla {
    hsla(HUE, CONTENT_S, CONTENT_L, 1.)
}

/// Content at the given strength over transparent.
pub fn mix(strength: f32) -> Hsla {
    hsla(HUE, CONTENT_S, CONTENT_L, strength)
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
    surface(DIALOG_L)
}

/// Lemon yellow, apart from the amber accent. Always shown with an icon.
pub fn warn() -> Rgba {
    rgb(0xfde047)
}

/// `#ffb547`
pub fn accent() -> Hsla {
    hsla(36. / 360., 1., 0.64, 1.)
}

/// Selected text: a light night blue, so amber stays for the accent,
/// search hits and notes.
pub fn text_selection() -> Hsla {
    hsla(HUE, 0.6, 0.62, 0.32)
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

    fn contrast(a: impl Into<Rgba>, b: impl Into<Rgba>) -> f32 {
        let (a, b) = (luminance(a), luminance(b));
        (a.max(b) + 0.05) / (a.min(b) + 0.05)
    }

    #[test]
    fn palette_matches_the_brand() {
        assert!(
            near(background(), 0x10121c),
            "background {:06x}",
            hex(background())
        );
        assert!(
            near(surface(SIDEBAR_L), 0x10121c),
            "sidebar {:06x}",
            hex(surface(SIDEBAR_L))
        );
        assert!(near(content(), 0xe6e8ef), "content {:06x}", hex(content()));
        assert!(near(accent(), 0xffb547), "accent {:06x}", hex(accent()));
        assert_eq!(hex(warn()), 0xfde047);
    }

    #[test]
    fn surfaces_are_tinted_and_the_accent_is_not_blue() {
        for c in [background(), surface(SIDEBAR_L), dialog(), content()] {
            assert!(c.s > 0.1, "{c:?} is gray");
        }
        assert!((accent().h * 360. - 211.).abs() > 90.);
    }

    #[test]
    fn text_stays_readable() {
        assert!(contrast(content(), background()) > 12.);
        assert!(contrast(background(), accent()) > 7.);
        assert!(contrast(warn(), background()) > 7.);
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
