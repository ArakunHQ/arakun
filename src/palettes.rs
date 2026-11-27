//! The look the user chose, kept in `palettes.json` next to the state file:
//! the palette in use and their own ones (the colors live in `theme`), dark
//! or light, and how round, roomy and spread out the app is (the scales live
//! in `ui`).

use std::path::PathBuf;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

use serde::{Deserialize, Serialize};

use crate::theme::Palette;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Saved {
    /// Id of the palette in use; empty is the default.
    #[serde(default)]
    pub current: String,
    /// The user's own palettes.
    #[serde(default)]
    pub custom: Vec<Palette>,
    /// Corner roundness, 1 is the default.
    #[serde(default = "one")]
    pub radius: f32,
    /// Padding and gaps, 1 is the default.
    #[serde(default = "one")]
    pub space: f32,
    /// Pixels between the window's floating cards.
    #[serde(default = "eight")]
    pub window_gap: f32,
    /// Light mode: the palette on a near-white background. Off is dark.
    #[serde(default)]
    pub light: bool,
    /// The file could not be read in full nor moved aside, so saving would
    /// write over what it still holds. Saves are refused for this session.
    #[serde(skip)]
    read_only: bool,
}

fn one() -> f32 {
    1.
}

fn eight() -> f32 {
    8.
}

impl Default for Saved {
    fn default() -> Self {
        Saved {
            current: String::new(),
            custom: Vec::new(),
            radius: 1.,
            space: 1.,
            window_gap: 8.,
            light: false,
            read_only: false,
        }
    }
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
        self.find(&self.current).unwrap_or_else(Palette::black)
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
    crate::paths::store(crate::paths::PALETTES)
}

pub fn load() -> Saved {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    load_from(&file(), now)
}

/// Reads the file one palette at a time, so one entry that doesn't parse
/// (hand edited, or written by a newer version) costs only itself. A file
/// that couldn't be read in full is moved aside first, since the next save
/// would replace it; the palettes that did read are written back so the
/// next start doesn't move it again. If it can't be moved, nothing is
/// saved over it this session.
fn load_from(path: &std::path::Path, now: u64) -> Saved {
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Saved::default(),
        Err(e) => {
            // It may still hold the user's palettes: don't save over it.
            eprintln!("arakun: could not read {}: {e}", path.display());
            return Saved {
                read_only: true,
                ..Saved::default()
            };
        }
    };
    match parse(&bytes) {
        Some((saved, 0)) => saved,
        Some((mut saved, skipped)) => {
            eprintln!(
                "arakun: skipped {skipped} palette(s) in {} that could not be read",
                path.display()
            );
            if keep_aside(path, now) {
                store_at(path, &saved);
            } else {
                saved.read_only = true;
            }
            saved
        }
        None => {
            eprintln!("arakun: could not read {}", path.display());
            Saved {
                read_only: !keep_aside(path, now),
                ..Saved::default()
            }
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
    // Anything but `true` is dark, the default.
    let light = matches!(obj.remove("light"), Some(serde_json::Value::Bool(true)));
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
    // A bad shape value falls back to its default; it doesn't cost the palettes.
    // Square corners and no window gap are choices (0); no spacing is not.
    let mut scale = |key: &str, default: f32, zero_ok: bool| match obj.remove(key) {
        Some(serde_json::Value::Number(n)) => n
            .as_f64()
            .map(|f| f as f32)
            .filter(|f| f.is_finite() && (*f > 0. || zero_ok && *f == 0.))
            .unwrap_or(default),
        _ => default,
    };
    let (radius, space, window_gap) = (
        scale("radius", one(), true),
        scale("space", one(), false),
        scale("window_gap", eight(), true),
    );
    Some((
        Saved {
            current,
            custom,
            radius,
            space,
            window_gap,
            light,
            read_only: false,
        },
        skipped,
    ))
}

/// Moves a palettes file that couldn't be read in full to
/// `palettes.json.corrupt-<unix time>`, so the user can still get it back.
/// `false` when it is still in place (a rename can fail on Windows while
/// another program holds the file open).
fn keep_aside(path: &std::path::Path, now: u64) -> bool {
    let mut name = path.as_os_str().to_owned();
    name.push(format!(".corrupt-{now}"));
    let aside = PathBuf::from(name);
    match std::fs::rename(path, &aside) {
        Ok(()) => {
            eprintln!("arakun: kept the old file as {}", aside.display());
            true
        }
        Err(e) => {
            eprintln!("arakun: could not keep {} aside: {e}", path.display());
            false
        }
    }
}

/// A number for a copy of the palettes about to be saved: a later copy gets
/// a larger one.
pub fn next_version() -> u64 {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    NEXT.fetch_add(1, Ordering::Relaxed)
}

/// The version on disk.
static WRITTEN: Mutex<u64> = Mutex::new(0);

/// Saves copy `version` of the palettes unless a later one is already on
/// disk. Saves run off the UI thread and one may still be writing when
/// Settings closes and saves its last edit, so without this an older copy
/// could land after the newer one. Returns whether it saved, so a failed
/// save can be tried again later.
pub fn store_version(saved: &Saved, version: u64) -> bool {
    store_version_at(&file(), &WRITTEN, saved, version)
}

fn store_version_at(
    path: &std::path::Path,
    written: &Mutex<u64>,
    saved: &Saved,
    version: u64,
) -> bool {
    let mut on_disk = written.lock().unwrap_or_else(|e| e.into_inner());
    if version <= *on_disk {
        return true;
    }
    let ok = store_at(path, saved);
    if ok {
        *on_disk = version;
    }
    ok
}

fn store_at(path: &std::path::Path, saved: &Saved) -> bool {
    if saved.read_only {
        eprintln!(
            "arakun: not saving {}: it holds palettes that could not be read",
            path.display()
        );
        return false;
    }
    match crate::durable::write(path, &serde_json::to_vec_pretty(saved).unwrap_or_default()) {
        Ok(()) => true,
        Err(e) => {
            eprintln!("arakun: could not save {}: {e}", path.display());
            false
        }
    }
}

/// Puts the saved palette and scales in use; called once at start.
pub fn apply_saved() {
    apply(&load());
}

pub fn apply(saved: &Saved) {
    crate::theme::set_mode(if saved.light {
        crate::theme::Mode::Light
    } else {
        crate::theme::Mode::Dark
    });
    crate::theme::set_palette(&saved.active());
    crate::ui::set_radius_scale(saved.radius);
    crate::ui::set_space_scale(saved.space);
    crate::ui::set_window_gap(saved.window_gap);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_users_palettes_get_their_own_ids_and_names() {
        let mut s = Saved::default();
        assert_eq!(s.active().id, "black");
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
        assert_eq!(s.active().id, "black");
        s.remove("custom-1");
        assert!(s.add(&night) == "custom-1", "a freed id is used again");
    }

    #[test]
    fn a_missing_palette_falls_back_to_the_default() {
        let s = Saved {
            current: "gone".into(),
            ..Saved::default()
        };
        assert_eq!(s.active().id, "black");
        let s = Saved {
            current: "forest".into(),
            ..Saved::default()
        };
        assert_eq!(s.active().name, "Forest");
        // A file from before the scales existed keeps the default look.
        let old: Saved = serde_json::from_str(r#"{"current":"rose","custom":[]}"#).unwrap();
        assert_eq!((old.radius, old.space, old.window_gap), (1., 1., 8.));
        assert!(!old.light, "and stays dark");
        let (lit, _) = parse(br#"{"current":"night","light":true}"#).unwrap();
        assert!(lit.light, "light mode survives a reload");
    }

    #[test]
    fn an_older_save_never_lands_after_a_newer_one() {
        let path = scratch("versions").join("palettes.json");
        let written = Mutex::new(0);
        let with = |current: &str| Saved {
            current: current.into(),
            ..Saved::default()
        };
        assert!(store_version_at(&path, &written, &with("rose"), 2));
        // The background save of an earlier edit finishes last.
        assert!(store_version_at(&path, &written, &with("forest"), 1));
        assert_eq!(load_from(&path, 0).current, "rose");
        assert!(store_version_at(&path, &written, &with("forest"), 3));
        assert_eq!(load_from(&path, 0).current, "forest");
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
        let s = load_from(&path, 42);
        let ids: Vec<_> = s.custom.iter().map(|p| p.id.as_str()).collect();
        assert_eq!(ids, ["custom-1", "custom-2"]);
        assert_eq!(s.active().name, "Sea");
        // The bad entry is still on disk for the user, and the good ones
        // were written back so the next start reads them without a copy.
        let kept = asides(&dir);
        assert_eq!(kept.len(), 1);
        assert_eq!(std::fs::read_to_string(&kept[0]).unwrap(), body);
        assert_eq!(load_from(&path, 42), s);
        assert_eq!(asides(&dir).len(), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_garbage_file_is_kept_aside_not_lost() {
        let dir = scratch("garbage");
        let path = dir.join("palettes.json");
        std::fs::write(&path, b"{\"current\": \"custom-1\", \"custom\": [{").unwrap();
        assert_eq!(load_from(&path, 42), Saved::default());
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

    /// A directory where `keep_aside` would move the file, so the rename fails.
    fn block_aside(path: &std::path::Path) {
        let mut name = path.as_os_str().to_owned();
        name.push(".corrupt-42");
        let dir = PathBuf::from(name);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("keep"), b"").unwrap();
    }

    #[test]
    fn a_file_that_cannot_be_moved_aside_is_not_overwritten() {
        let dir = scratch("stuck-entry");
        let path = dir.join("palettes.json");
        let body = r#"{"custom": [
            {"id": "custom-1", "name": "Sunset", "hue": 265, "tint": 0.32, "bg": 0.075, "accent": 16742745},
            {"id": "custom-2", "name": "Broken", "hue": "red"}
        ]}"#;
        std::fs::write(&path, body).unwrap();
        block_aside(&path);
        let mut s = load_from(&path, 42);
        assert_eq!(s.custom.len(), 1);
        // Neither the load nor a later save drops the entry it skipped.
        assert_eq!(std::fs::read_to_string(&path).unwrap(), body);
        s.add(&Palette::night());
        assert!(!store_at(&path, &s));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), body);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_garbage_file_that_cannot_be_moved_aside_is_not_overwritten() {
        let dir = scratch("stuck-garbage");
        let path = dir.join("palettes.json");
        std::fs::write(&path, b"not json").unwrap();
        block_aside(&path);
        let s = load_from(&path, 42);
        assert!(!store_at(&path, &s));
        assert_eq!(std::fs::read(&path).unwrap(), b"not json");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_unreadable_file_is_not_overwritten() {
        let dir = scratch("unreadable");
        // A directory can't be read as a file, and must not be replaced either.
        let path = dir.join("palettes.json");
        std::fs::create_dir_all(&path).unwrap();
        let s = load_from(&path, 42);
        assert!(!store_at(&path, &s));
        assert!(path.is_dir());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_failed_save_says_so() {
        let dir = scratch("failed-save");
        // A file where the folder should be: the write can't succeed.
        let blocker = dir.join("not-a-dir");
        std::fs::write(&blocker, b"").unwrap();
        let s = Saved::default();
        assert!(!store_at(&blocker.join("palettes.json"), &s));
        assert!(store_at(&dir.join("palettes.json"), &s));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_good_file_loads_without_copies() {
        let dir = scratch("good");
        let path = dir.join("palettes.json");
        let mut s = Saved::default();
        s.add(&Palette::night());
        std::fs::write(&path, serde_json::to_vec_pretty(&s).unwrap()).unwrap();
        assert_eq!(load_from(&path, 42), s);
        assert!(asides(&dir).is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_shape_settings_load_with_the_palettes() {
        let (saved, skipped) =
            parse(br#"{"current":"","custom":[],"radius":1.5,"space":0.75,"window_gap":4}"#)
                .unwrap();
        assert_eq!(skipped, 0);
        assert_eq!(
            (saved.radius, saved.space, saved.window_gap),
            (1.5, 0.75, 4.)
        );
        // Missing or bad values are the defaults.
        let (saved, _) = parse(br#"{"radius":"big"}"#).unwrap();
        assert_eq!((saved.radius, saved.space, saved.window_gap), (1., 1., 8.));
        // Square corners and no gap survive a save; no spacing at all doesn't.
        let zero = Saved {
            radius: 0.,
            window_gap: 0.,
            ..Saved::default()
        };
        let (back, _) = parse(&serde_json::to_vec(&zero).unwrap()).unwrap();
        assert_eq!(back, zero);
        let (saved, _) = parse(br#"{"radius":0,"space":0,"window_gap":0}"#).unwrap();
        assert_eq!((saved.radius, saved.space, saved.window_gap), (0., 1., 0.));
        let (saved, _) = parse(br#"{"radius":-1,"window_gap":-2}"#).unwrap();
        assert_eq!((saved.radius, saved.window_gap), (1., 8.));
    }
}
