//! The dark ANSI terminal palette plus the standard xterm 256-color cube,
//! resolved once into `Hsla`.

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
        0..16 => rgb_of(ANSI[index]),
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
            dim(rgb_of(ANSI[i - NamedColor::DimBlack as usize]))
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

static PALETTE: LazyLock<Vec<Hsla>> = LazyLock::new(|| {
    (0..alacritty_terminal::term::color::COUNT)
        .map(|i| to_hsla(default_rgb(i)))
        .collect()
});

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
        PALETTE[index]
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
