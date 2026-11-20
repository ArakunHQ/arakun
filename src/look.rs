//! A look in one line to share: the colors, the shape and the sidebar, each
//! one optional, so whoever pastes it chooses again what to take. The line
//! is `arakun:look/v1:` and the parts as JSON in URL-safe base64.

use base64::Engine as _;
use serde::{Deserialize, Serialize};

use crate::theme::{DiffPalette, Grays, Palette};
use crate::ui::TypeScale;

const PREFIX: &str = "arakun:look/v1:";

/// How the app looks, in the parts a line can carry.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Look {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub colors: Option<Colors>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shape: Option<Shape>,
    /// The sidebar's layout as `sidebar.json` holds it, read back with
    /// `Sidebar::parse` so an unknown piece is dropped, not an error.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sidebar: Option<serde_json::Value>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Colors {
    pub palette: Palette,
    #[serde(default)]
    pub light: bool,
    /// `Grays::id`.
    #[serde(default)]
    pub grays: String,
    /// `DiffPalette::id`.
    #[serde(default)]
    pub diff: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Shape {
    pub radius: f32,
    pub space: f32,
    pub window_gap: f32,
    /// `TypeScale::id`.
    #[serde(default)]
    pub text: String,
    #[serde(default)]
    pub glass: bool,
}

impl Colors {
    pub fn grays(&self) -> Grays {
        Grays::from_id(&self.grays)
    }

    pub fn diff(&self) -> DiffPalette {
        DiffPalette::from_id(&self.diff)
    }
}

impl Shape {
    pub fn text(&self) -> TypeScale {
        TypeScale::from_id(&self.text)
    }

    /// The values a slider can reach, so a hand-edited line cannot set a
    /// corner radius of a thousand.
    pub fn clamped(mut self) -> Shape {
        let fix =
            |v: f32, lo: f32, hi: f32, or: f32| if v.is_finite() { v.clamp(lo, hi) } else { or };
        self.radius = fix(self.radius, 0., 2., 1.);
        self.space = fix(self.space, 0.6, 1.6, 1.);
        self.window_gap = fix(self.window_gap, 0., 20., 8.);
        self
    }
}

impl Look {
    pub fn is_empty(&self) -> bool {
        self.colors.is_none() && self.shape.is_none() && self.sidebar.is_none()
    }

    /// The line to share.
    pub fn encode(&self) -> String {
        let json = serde_json::to_vec(self).unwrap_or_default();
        format!(
            "{PREFIX}{}",
            base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(json)
        )
    }

    /// A look from [`Look::encode`]'s line; None when it isn't one or holds
    /// nothing.
    pub fn decode(line: &str) -> Option<Look> {
        let rest = line.trim().strip_prefix(PREFIX)?;
        let json = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(rest.trim())
            .ok()?;
        let mut look: Look = serde_json::from_slice(&json).ok()?;
        look.shape = look.shape.map(Shape::clamped);
        if let Some(c) = &mut look.colors {
            c.palette = c.palette.clone().clamped();
        }
        (!look.is_empty()).then_some(look)
    }
}

/// Uses a shared palette: the one the user already has with the same colors,
/// or a copy of it among theirs. Returns the id now in use.
pub fn adopt_palette(saved: &mut crate::palettes::Saved, p: &Palette) -> String {
    let same = |a: &Palette| {
        (a.hue - p.hue).abs() < 0.5
            && (a.tint - p.tint).abs() < 0.005
            && (a.bg - p.bg).abs() < 0.0005
            && a.accent == p.accent
    };
    match saved.all().into_iter().find(|a| same(a)) {
        Some(found) => {
            saved.current = found.id.clone();
            found.id
        }
        None => saved.add(p),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Look {
        Look {
            colors: Some(Colors {
                palette: Palette::built_in()[0].clone(),
                light: true,
                grays: Grays::Soft.id().into(),
                diff: DiffPalette::Colorblind.id().into(),
            }),
            shape: Some(Shape {
                radius: 1.5,
                space: 0.8,
                window_gap: 14.,
                text: TypeScale::Comfortable.id().into(),
                glass: true,
            }),
            sidebar: Some(serde_json::json!({"sections": []})),
        }
    }

    #[test]
    fn a_look_survives_its_line() {
        let look = sample();
        let line = look.encode();
        assert!(line.starts_with("arakun:look/v1:"));
        assert!(!line.contains(char::is_whitespace));
        assert_eq!(Look::decode(&line), Some(look.clone()));
        assert_eq!(Look::decode(&format!("  {line}\n")), Some(look));
    }

    #[test]
    fn only_the_parts_chosen_travel() {
        let mut look = sample();
        look.sidebar = None;
        look.shape = None;
        let back = Look::decode(&look.encode()).unwrap();
        assert!(back.colors.is_some() && back.shape.is_none() && back.sidebar.is_none());
        assert_eq!(back.colors.unwrap().grays(), Grays::Soft);
    }

    #[test]
    fn what_is_not_a_look_is_refused() {
        assert_eq!(Look::decode("arakun:palette/v1:220,10,5,ffb347,Mine"), None);
        assert_eq!(Look::decode("arakun:look/v1:not base64 at all"), None);
        assert_eq!(Look::decode(&Look::default().encode()), None);
        assert_eq!(Look::decode(""), None);
    }

    #[test]
    fn a_hand_made_shape_is_kept_in_range() {
        let mut look = sample();
        look.shape.as_mut().unwrap().radius = 1000.;
        look.shape.as_mut().unwrap().window_gap = -3.;
        let shape = Look::decode(&look.encode()).unwrap().shape.unwrap();
        assert_eq!(shape.radius, 2.);
        assert_eq!(shape.window_gap, 0.);
    }

    #[test]
    fn a_shared_palette_reuses_one_with_the_same_colors() {
        let mut saved = crate::palettes::Saved::default();
        let night = Palette::built_in()
            .into_iter()
            .find(|p| p.id != saved.active().id)
            .unwrap();
        let id = adopt_palette(&mut saved, &night);
        assert_eq!(id, night.id);
        assert!(saved.custom.is_empty());

        let mine = Palette {
            accent: 0x123456,
            ..night.clone()
        };
        let id = adopt_palette(&mut saved, &mine);
        assert!(id.starts_with("custom-"));
        assert_eq!(saved.custom.len(), 1);
        // Pasting it again picks the copy instead of making another.
        assert_eq!(adopt_palette(&mut saved, &mine), id);
        assert_eq!(saved.custom.len(), 1);
    }
}
