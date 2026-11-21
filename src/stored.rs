//! Loading the files Arakun keeps its state in without losing one it can't
//! read. A missing file means a fresh start. A file that can't be read or
//! parsed (from a newer Arakun, edited by hand, torn by a crash) is renamed to
//! `<name>.corrupt-<unix time>` before anything can save over it, and the
//! window says so once.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde::de::DeserializeOwned;

/// A file that could not be read, for the window to tell the user about.
#[derive(Clone, Debug, PartialEq)]
pub struct Unreadable {
    pub path: PathBuf,
    /// Where the old bytes are now; `None` when they could not be moved and
    /// the file is left alone (nothing saves over it).
    pub kept: Option<PathBuf>,
}

/// Files that could not be read and are still in the way: saves skip them.
static BLOCKED: Mutex<Option<HashSet<PathBuf>>> = Mutex::new(None);
/// Set aside since the window last asked.
static REPORTS: Mutex<Vec<Unreadable>> = Mutex::new(Vec::new());

/// The JSON in `path`; `None` when there is none yet or it could not be read
/// (then it is set aside).
pub fn load<T: DeserializeOwned>(path: &Path) -> Option<T> {
    load_with(path, |bytes| {
        serde_json::from_slice(bytes).map_err(|e| e.to_string())
    })
}

/// `load` with a parser of its own, for files that skip what they don't know.
pub fn load_with<T>(path: &Path, parse: impl FnOnce(&[u8]) -> Result<T, String>) -> Option<T> {
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return None,
        Err(e) => {
            reject(path, &e.to_string(), crate::model::unix_now());
            return None;
        }
    };
    match parse(&bytes) {
        Ok(value) => Some(value),
        Err(e) => {
            reject(path, &e, crate::model::unix_now());
            None
        }
    }
}

/// Saves `bytes` to `path` with `durable::write`, unless `path` still holds a
/// file that could not be read: that one is moved first, or kept and the save
/// fails.
pub fn write(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    if !clear(path, crate::model::unix_now()) {
        return Err(std::io::Error::other(format!(
            "not saving {}: it holds data that could not be read",
            path.display()
        )));
    }
    crate::durable::write(path, bytes)
}

/// The files set aside since the last call.
pub fn take_reports() -> Vec<Unreadable> {
    std::mem::take(&mut *REPORTS.lock().unwrap_or_else(|e| e.into_inner()))
}

fn reject(path: &Path, why: &str, now: u64) {
    eprintln!("arakun: could not read {}: {why}", path.display());
    let kept = set_aside(path, now);
    // A file that stays stuck is read again on every refresh (the memory
    // list): say so once, not each time.
    let new = kept.is_some()
        || BLOCKED
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get_or_insert_with(HashSet::new)
            .insert(path.to_path_buf());
    if new {
        report(path, kept);
    }
}

fn report(path: &Path, kept: Option<PathBuf>) {
    REPORTS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .push(Unreadable {
            path: path.to_path_buf(),
            kept,
        });
}

/// Whether `path` may be written: true unless a file that could not be read
/// is still there and can't be moved yet.
fn clear(path: &Path, now: u64) -> bool {
    let mut blocked = BLOCKED.lock().unwrap_or_else(|e| e.into_inner());
    let Some(set) = blocked.as_mut().filter(|s| s.contains(path)) else {
        return true;
    };
    let moved = match std::fs::symlink_metadata(path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => true,
        _ => match set_aside(path, now) {
            Some(kept) => {
                report(path, Some(kept));
                true
            }
            None => false,
        },
    };
    if moved {
        set.remove(path);
    }
    moved
}

/// How many `.corrupt-<time>-<n>` names `set_aside` tries in one second.
pub const ASIDE_NAMES: usize = 100;

/// Renames the file out of the way so the next save can't overwrite it, and
/// says where it went. `None` when it is still there.
pub fn set_aside(path: &Path, now: u64) -> Option<PathBuf> {
    // A rename replaces a file already at the new name: take a free one, so
    // a file set aside earlier in the same second is not lost either.
    let kept = (0..ASIDE_NAMES)
        .map(|n| {
            let mut name = path.as_os_str().to_owned();
            name.push(format!(".corrupt-{now}"));
            if n > 0 {
                name.push(format!("-{n}"));
            }
            PathBuf::from(name)
        })
        .find(|p| std::fs::symlink_metadata(p).is_err());
    let Some(kept) = kept else {
        eprintln!(
            "arakun: could not set {} aside: no free name",
            path.display()
        );
        return None;
    };
    match std::fs::rename(path, &kept) {
        Ok(()) => {
            eprintln!("arakun: kept the old file as {}", kept.display());
            Some(kept)
        }
        Err(e) => {
            eprintln!("arakun: could not set {} aside: {e}", path.display());
            None
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// An empty directory of its own, also for the stores' tests.
    pub(crate) fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("arakun-stored-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// What the one copy of `path` set aside holds; `None` without one.
    pub(crate) fn kept(path: &Path) -> Option<String> {
        let name = format!("{}.corrupt-", path.file_name()?.to_string_lossy());
        let copies: Vec<PathBuf> = std::fs::read_dir(path.parent()?)
            .ok()?
            .flatten()
            .map(|e| e.path())
            .filter(|p| {
                p.file_name()
                    .is_some_and(|n| n.to_string_lossy().starts_with(&name))
            })
            .collect();
        match copies.as_slice() {
            [one] => std::fs::read_to_string(one).ok(),
            _ => None,
        }
    }

    fn reports_for(path: &Path) -> Vec<Unreadable> {
        // Other tests report their own files in parallel: keep this one's.
        let mut all = REPORTS.lock().unwrap_or_else(|e| e.into_inner());
        let (mine, rest) = std::mem::take(&mut *all)
            .into_iter()
            .partition(|r| r.path == path);
        *all = rest;
        mine
    }

    #[test]
    fn a_missing_file_starts_fresh_and_is_not_reported() {
        let dir = scratch("missing");
        let path = dir.join("state.json");
        assert_eq!(load::<Vec<u32>>(&path), None);
        assert!(reports_for(&path).is_empty());
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 0);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_file_that_parses_is_read() {
        let dir = scratch("good");
        let path = dir.join("state.json");
        std::fs::write(&path, "[1,2]").unwrap();
        assert_eq!(load::<Vec<u32>>(&path), Some(vec![1, 2]));
        assert!(reports_for(&path).is_empty());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn an_unreadable_file_is_set_aside_and_survives_the_next_save() {
        let dir = scratch("bad");
        let path = dir.join("state.json");
        std::fs::write(&path, "[1,\"two\"]").unwrap();
        assert_eq!(load::<Vec<u32>>(&path), None);
        write(&path, b"[]").unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "[]");
        let reports = reports_for(&path);
        assert_eq!(reports.len(), 1);
        let kept = reports[0].kept.clone().unwrap();
        assert!(kept.to_string_lossy().contains("state.json.corrupt-"));
        assert_eq!(std::fs::read_to_string(kept).unwrap(), "[1,\"two\"]");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn setting_aside_keeps_an_earlier_copy() {
        let dir = scratch("twice");
        let path = dir.join("state.json");
        std::fs::write(dir.join("state.json.corrupt-42"), "first").unwrap();
        std::fs::write(&path, "second").unwrap();
        assert_eq!(
            set_aside(&path, 42),
            Some(dir.join("state.json.corrupt-42-1"))
        );
        let read = |name: &str| std::fs::read_to_string(dir.join(name)).unwrap();
        assert_eq!(read("state.json.corrupt-42"), "first");
        assert_eq!(read("state.json.corrupt-42-1"), "second");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_file_that_could_not_be_moved_is_not_overwritten() {
        let dir = scratch("stuck");
        let path = dir.join("state.json");
        std::fs::write(&path, "torn").unwrap();
        // Every name it could be moved to is taken.
        let aside = |n: usize| match n {
            0 => dir.join("state.json.corrupt-42"),
            n => dir.join(format!("state.json.corrupt-42-{n}")),
        };
        for n in 0..ASIDE_NAMES {
            std::fs::write(aside(n), "older").unwrap();
        }
        reject(&path, "test", 42);
        // Read again while still stuck: told once.
        reject(&path, "test", 42);
        assert!(!clear(&path, 42));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "torn");
        assert_eq!(
            reports_for(&path),
            vec![Unreadable {
                path: path.clone(),
                kept: None
            }]
        );
        // Once a name is free, the next save moves it and then writes.
        std::fs::remove_file(aside(7)).unwrap();
        assert!(clear(&path, 42));
        assert_eq!(std::fs::read_to_string(aside(7)).unwrap(), "torn");
        assert_eq!(reports_for(&path)[0].kept, Some(aside(7)));
        assert!(clear(&path, 42));
        let _ = std::fs::remove_dir_all(dir);
    }
}
