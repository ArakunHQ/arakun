//! The ANSI terminal palettes (one for dark mode, one for light) plus the
//! standard xterm 256-color cube, resolved once into `Hsla`. The default
//! foreground, background and cursor follow the theme live.

use std::sync::LazyLock;

use alacritty_terminal::term::color::Colors;
use alacritty_terminal::vte::ansi::{Color, NamedColor, Rgb};
use gpui::{Hsla, Rgba};

use crate::theme;

const ANSI: [u32; 16] = [
    0x1d2428, // black
    0xf87171, // red
    0x4ade80, // green
    0xfbbf24, // yellow
    0x60a5fa, // blue
    0xc084fc, // magenta
    0x22d3ee, // cyan
    0xe8eef2, // white
    0x64748b, // bright black
    0xfca5a5, // bright red
    0x86efac, // bright green
    0xfde68a, // bright yellow
    0x93c5fd, // bright blue
    0xd8b4fe, // bright magenta
    0x67e8f9, // bright cyan
    0xf8fafc, // bright white
];

/// GitHub's light terminal colors: the dark set's pastels vanish on white.
const ANSI_LIGHT: [u32; 16] = [
    0x24292f, // black
    0xcf222e, // red
    0x116329, // green
    0x4d2d00, // yellow
    0x0969da, // blue
    0x8250df, // magenta
    0x1b7c83, // cyan
    0x6e7781, // white
    0x57606a, // bright black
    0xa40e26, // bright red
    0x1a7f37, // bright green
    0x633c01, // bright yellow
    0x218bff, // bright blue
    0xa475f9, // bright magenta
    0x3192aa, // bright cyan
    0x8c959f, // bright white
];

fn ansi() -> &'static [u32; 16] {
    if theme::is_light() {
        &ANSI_LIGHT
    } else {
        &ANSI
    }
}

/// Alacritty's factor for SGR 2 (faint) on colors that have no explicit dim variant.
const DIM_FACTOR: f32 = 0.66;

fn rgb_of(hex: u32) -> Rgb {
    Rgb {
        r: (hex >> 16) as u8,
        g: (hex >> 8) as u8,
        b: hex as u8,
    }
}

fn hsla_to_rgb(c: Hsla) -> Rgb {
    let c: Rgba = c.into();
    Rgb {
        r: (c.r * 255.).round() as u8,
        g: (c.g * 255.).round() as u8,
        b: (c.b * 255.).round() as u8,
    }
}

fn dim(c: Rgb) -> Rgb {
    let f = |v: u8| (v as f32 * DIM_FACTOR).round() as u8;
    Rgb {
        r: f(c.r),
        g: f(c.g),
        b: f(c.b),
    }
}

/// Default RGB for every alacritty color index (0..269), used when the program hasn't
/// overridden it with OSC 4/10/11/12.
pub fn default_rgb(index: usize) -> Rgb {
    match index {
        0..16 => rgb_of(ansi()[index]),
        16..232 => {
            let i = index - 16;
            let level = |v: usize| if v == 0 { 0 } else { (v * 40 + 55) as u8 };
            Rgb {
                r: level(i / 36),
                g: level((i / 6) % 6),
                b: level(i % 6),
            }
        }
        232..256 => {
            let v = (8 + (index - 232) * 10) as u8;
            Rgb { r: v, g: v, b: v }
        }
        i if i == NamedColor::Foreground as usize || i == NamedColor::BrightForeground as usize => {
            hsla_to_rgb(theme::content())
        }
        i if i == NamedColor::Background as usize => hsla_to_rgb(theme::background()),
        i if i == NamedColor::Cursor as usize => hsla_to_rgb(theme::active()),
        i if (NamedColor::DimBlack as usize..=NamedColor::DimWhite as usize).contains(&i) => {
            dim(rgb_of(ansi()[i - NamedColor::DimBlack as usize]))
        }
        i if i == NamedColor::DimForeground as usize => dim(hsla_to_rgb(theme::content())),
        _ => hsla_to_rgb(theme::content()),
    }
}

pub fn to_hsla(c: Rgb) -> Hsla {
    Rgba {
        r: c.r as f32 / 255.,
        g: c.g as f32 / 255.,
        b: c.b as f32 / 255.,
        a: 1.,
    }
    .into()
}

/// The 256 indexed colors per mode; the named ones after them (foreground,
/// background, cursor, dims) are read from the theme on each use, so a
/// palette or mode change shows in open terminals.
fn indexed(light: bool) -> Vec<Hsla> {
    let table = if light { &ANSI_LIGHT } else { &ANSI };
    (0..256)
        .map(|i| match i {
            0..16 => to_hsla(rgb_of(table[i])),
            _ => to_hsla(default_rgb(i)),
        })
        .collect()
}

static DARK: LazyLock<Vec<Hsla>> = LazyLock::new(|| indexed(false));
static LIGHT: LazyLock<Vec<Hsla>> = LazyLock::new(|| indexed(true));

/// Resolves colors for one frame. `overrides` is false in the common case where no
/// program has touched the palette, which keeps lookups to a single array index.
pub struct Resolver<'a> {
    colors: &'a Colors,
    overrides: bool,
}

impl<'a> Resolver<'a> {
    pub fn new(colors: &'a Colors) -> Self {
        let overrides = (0..alacritty_terminal::term::color::COUNT).any(|i| colors[i].is_some());
        Self { colors, overrides }
    }

    pub fn index(&self, index: usize) -> Hsla {
        if self.overrides
            && let Some(rgb) = self.colors[index]
        {
            return to_hsla(rgb);
        }
        match index {
            0..256 if theme::is_light() => LIGHT[index],
            0..256 => DARK[index],
            _ => to_hsla(default_rgb(index)),
        }
    }

    pub fn resolve(&self, color: Color) -> Hsla {
        match color {
            Color::Spec(rgb) => to_hsla(rgb),
            Color::Indexed(i) => self.index(i as usize),
            Color::Named(n) => self.index(n as usize),
        }
    }
}

pub fn selection(focused: bool) -> Hsla {
    theme::mix(if focused { 0.18 } else { 0.08 })
}
