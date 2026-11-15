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
    load_from(&file())
}

/// Reads the file one palette at a time, so one entry that doesn't parse
/// (hand edited, or written by a newer version) costs only itself. A file
/// that couldn't be read in full is moved aside first, since the next save
/// would replace it; the palettes that did read are written back so the
/// next start doesn't move it again.
fn load_from(path: &std::path::Path) -> Saved {
    let Ok(bytes) = std::fs::read(path) else {
        return Saved::default();
    };
    match parse(&bytes) {
        Some((saved, 0)) => saved,
        Some((saved, skipped)) => {
            eprintln!(
                "arakun: skipped {skipped} palette(s) in {} that could not be read",
                path.display()
            );
            keep_aside(path);
            store_at(path, &saved);
            saved
        }
        None => {
            eprintln!("arakun: could not read {}", path.display());
            keep_aside(path);
            Saved::default()
        }
    }
}

/// The saved palettes and how many entries were skipped; None when the file
/// isn't a palettes file at all.
fn parse(bytes: &[u8]) -> Option<(Saved, usize)> {
    let serde_json::Value::Object(mut obj) = serde_json::from_slice(bytes).ok()? else {
        return None;
    };
    let current = match obj.remove("current") {
        Some(serde_json::Value::String(s)) => s,
        _ => String::new(),
    };
    let entries = match obj.remove("custom") {
        Some(serde_json::Value::Array(a)) => a,
        Some(serde_json::Value::Null) | None => Vec::new(),
        Some(_) => return None,
    };
    let total = entries.len();
    let custom: Vec<Palette> = entries
        .into_iter()
        .filter_map(|v| serde_json::from_value::<Palette>(v).ok())
        .filter(|p| p.hue.is_finite() && p.tint.is_finite() && p.bg.is_finite())
        .map(Palette::clamped)
        .collect();
    let skipped = total - custom.len();
    Some((Saved { current, custom }, skipped))
}

/// Moves a palettes file that couldn't be read in full to
/// `palettes.json.corrupt-<unix time>`, so the user can still get it back.
fn keep_aside(path: &std::path::Path) {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    let mut name = path.as_os_str().to_owned();
    name.push(format!(".corrupt-{secs}"));
    let aside = PathBuf::from(name);
    match std::fs::rename(path, &aside) {
        Ok(()) => eprintln!("arakun: kept the old file as {}", aside.display()),
        Err(e) => eprintln!("arakun: could not keep {} aside: {e}", path.display()),
    }
}

pub fn store(saved: &Saved) {
    store_at(&file(), saved);
}

fn store_at(path: &std::path::Path, saved: &Saved) {
    if let Err(e) =
        crate::durable::write(path, &serde_json::to_vec_pretty(saved).unwrap_or_default())
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

    fn scratch(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("arakun-palettes-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn asides(dir: &std::path::Path) -> Vec<PathBuf> {
        std::fs::read_dir(dir)
            .unwrap()
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.to_string_lossy().contains(".corrupt-"))
            .collect()
    }

    #[test]
    fn one_bad_entry_keeps_the_other_palettes() {
        let dir = scratch("one-bad");
        let path = dir.join("palettes.json");
        let body = r#"{
            "current": "custom-2",
            "custom": [
                {"id": "custom-1", "name": "Sunset", "hue": 265, "tint": 0.32, "bg": 0.075, "accent": 16742745},
                {"id": "custom-3", "name": "Broken", "hue": "red"},
                {"id": "custom-2", "name": "Sea", "hue": 190, "tint": 0.3, "bg": 0.08, "accent": 6280388}
            ]
        }"#;
        std::fs::write(&path, body).unwrap();
        let s = load_from(&path);
        let ids: Vec<_> = s.custom.iter().map(|p| p.id.as_str()).collect();
        assert_eq!(ids, ["custom-1", "custom-2"]);
        assert_eq!(s.active().name, "Sea");
        // The bad entry is still on disk for the user, and the good ones
        // were written back so the next start reads them without a copy.
        let kept = asides(&dir);
        assert_eq!(kept.len(), 1);
        assert_eq!(std::fs::read_to_string(&kept[0]).unwrap(), body);
        assert_eq!(load_from(&path), s);
        assert_eq!(asides(&dir).len(), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_garbage_file_is_kept_aside_not_lost() {
        let dir = scratch("garbage");
        let path = dir.join("palettes.json");
        std::fs::write(&path, b"{\"current\": \"custom-1\", \"custom\": [{").unwrap();
        assert_eq!(load_from(&path), Saved::default());
        assert!(!path.exists(), "the next save can't overwrite it");
        let kept = asides(&dir);
        assert_eq!(kept.len(), 1);
        assert_eq!(
            std::fs::read(&kept[0]).unwrap(),
            b"{\"current\": \"custom-1\", \"custom\": [{"
        );
        assert_eq!(parse(b"[1, 2]"), None);
        assert_eq!(parse(b"{\"custom\": 3}"), None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_good_file_loads_without_copies() {
        let dir = scratch("good");
        let path = dir.join("palettes.json");
        let mut s = Saved::default();
        s.add(&Palette::night());
        std::fs::write(&path, serde_json::to_vec_pretty(&s).unwrap()).unwrap();
        assert_eq!(load_from(&path), s);
        assert!(asides(&dir).is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
