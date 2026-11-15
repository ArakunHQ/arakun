//! The palette in use and the ones the user made, kept in `palettes.json`
//! next to the state file. The colors themselves live in `theme`.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::theme::Palette;

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Saved {
    /// Id of the palette in use; empty is the default.
    #[serde(default)]
    pub current: String,
    /// The user's own palettes.
    #[serde(default)]
    pub custom: Vec<Palette>,
}

impl Saved {
    /// Built-in palettes first, then the user's.
    pub fn all(&self) -> Vec<Palette> {
        let mut out = Palette::built_in();
        out.extend(self.custom.iter().cloned());
        out
    }

    pub fn find(&self, id: &str) -> Option<Palette> {
        self.all().into_iter().find(|p| p.id == id)
    }

    /// The palette in use; the default when the saved one is gone.
    pub fn active(&self) -> Palette {
        self.find(&self.current).unwrap_or_else(Palette::night)
    }

    pub fn is_custom(&self, id: &str) -> bool {
        self.custom.iter().any(|p| p.id == id)
    }

    /// A new palette of the user's, from `from`, named so it doesn't clash;
    /// it becomes the one in use. Returns its id.
    pub fn add(&mut self, from: &Palette) -> String {
        let n = (1..)
            .find(|n| !self.custom.iter().any(|p| p.id == format!("custom-{n}")))
            .unwrap_or(1);
        let mut p = from.clone().clamped();
        p.id = format!("custom-{n}");
        let base = if from.id.starts_with("custom-") || from.id.is_empty() {
            from.name.clone()
        } else {
            format!("{} copy", from.name)
        };
        p.name = self.unique_name(&base);
        self.current = p.id.clone();
        self.custom.push(p);
        self.current.clone()
    }

    fn unique_name(&self, base: &str) -> String {
        let taken = |n: &str| self.all().iter().any(|p| p.name == n);
        if !taken(base) {
            return base.to_string();
        }
        (2..)
            .map(|i| format!("{base} {i}"))
            .find(|n| !taken(n))
            .unwrap_or_default()
    }

    /// Replaces one of the user's palettes, keeping its id.
    pub fn update(&mut self, p: Palette) {
        if let Some(slot) = self.custom.iter_mut().find(|c| c.id == p.id) {
            *slot = p.clamped();
        }
    }

    /// Removes one of the user's palettes; the default takes its place if it
    /// was in use.
    pub fn remove(&mut self, id: &str) {
        self.custom.retain(|p| p.id != id);
        if self.current == id {
            self.current.clear();
        }
    }
}

fn file() -> PathBuf {
    match std::env::var_os("ARAKUN_STATE").map(PathBuf::from) {
        Some(state) => state.with_file_name("palettes.json"),
        None => crate::state::data_dir().join("palettes.json"),
    }
}

pub fn load() -> Saved {
    std::fs::read(file())
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
}

pub fn store(saved: &Saved) {
    let path = file();
    if let Err(e) =
        crate::durable::write(&path, &serde_json::to_vec_pretty(saved).unwrap_or_default())
    {
        eprintln!("arakun: could not save {}: {e}", path.display());
    }
}

/// Puts the saved palette in use; called once at start.
pub fn apply_saved() {
    crate::theme::set_palette(&load().active());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_users_palettes_get_their_own_ids_and_names() {
        let mut s = Saved::default();
        assert_eq!(s.active().id, "night");
        let night = Palette::night();
        let a = s.add(&night);
        assert_eq!((a.as_str(), s.current.as_str()), ("custom-1", "custom-1"));
        assert_eq!(s.custom[0].name, "Arakun Night copy");
        let b = s.add(&night);
        assert_eq!(b, "custom-2");
        assert_eq!(s.custom[1].name, "Arakun Night copy 2");
        let mut edited = s.find("custom-1").unwrap();
        edited.hue = 10.;
        s.update(edited);
        assert_eq!(s.find("custom-1").unwrap().hue, 10.);
        s.remove("custom-2");
        assert_eq!(s.active().id, "night");
        s.remove("custom-1");
        assert!(s.add(&night) == "custom-1", "a freed id is used again");
    }

    #[test]
    fn a_missing_palette_falls_back_to_the_default() {
        let s = Saved {
            current: "gone".into(),
            custom: vec![],
        };
        assert_eq!(s.active().id, "night");
        let s = Saved {
            current: "forest".into(),
            custom: vec![],
        };
        assert_eq!(s.active().name, "Forest");
    }
}
