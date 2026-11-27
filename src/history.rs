//! Prompts sent from the composer, oldest first, for ↑ and ↓ in an empty
//! composer. Kept in `history.json` next to the state, across environments.

use std::path::{Path, PathBuf};

const KEEP: usize = 500;

#[derive(Default)]
pub struct History {
    entries: Vec<String>,
}

fn file() -> PathBuf {
    crate::paths::store(crate::paths::HISTORY)
}

impl History {
    pub fn load() -> Self {
        Self::load_from(&file())
    }

    fn load_from(path: &Path) -> Self {
        Self {
            entries: crate::stored::load(path).unwrap_or_default(),
        }
    }

    /// Adds a sent prompt as the newest entry; an earlier copy of it moves up
    /// instead of showing twice. `false` when there was nothing to keep.
    pub fn push(&mut self, text: &str) -> bool {
        let text = text.trim();
        if text.is_empty() {
            return false;
        }
        self.entries.retain(|e| e != text);
        self.entries.push(text.to_string());
        if self.entries.len() > KEEP {
            self.entries.drain(..self.entries.len() - KEEP);
        }
        true
    }

    pub fn get(&self, ix: usize) -> Option<&str> {
        self.entries.get(ix).map(String::as_str)
    }

    /// The entry before `from`, or the newest one when not browsing yet.
    pub fn older(&self, from: Option<usize>) -> Option<usize> {
        match from {
            None => self.entries.len().checked_sub(1),
            Some(ix) => ix.checked_sub(1),
        }
    }

    /// The entry after `from`; `None` past the newest.
    pub fn newer(&self, from: usize) -> Option<usize> {
        (from + 1 < self.entries.len()).then_some(from + 1)
    }

    pub fn bytes(&self) -> Vec<u8> {
        serde_json::to_vec(&self.entries).unwrap_or_default()
    }
}

pub fn write(bytes: &[u8]) {
    write_to(&file(), bytes);
}

pub(crate) fn write_to(path: &Path, bytes: &[u8]) {
    if let Err(e) = crate::stored::write(path, bytes) {
        eprintln!("arakun: could not save {}: {e}", path.display());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn newest_last_without_repeats() {
        let mut h = History::default();
        assert!(!h.push("  "));
        h.push("one");
        h.push("two\n");
        h.push("one");
        assert_eq!(h.entries, ["two", "one"]);
        assert_eq!(h.older(None), Some(1));
        assert_eq!(h.older(Some(1)), Some(0));
        assert_eq!(h.older(Some(0)), None);
        assert_eq!(h.newer(0), Some(1));
        assert_eq!(h.newer(1), None);
        assert_eq!(History::default().older(None), None);
    }

    #[test]
    fn keeps_the_latest_entries() {
        let mut h = History::default();
        for i in 0..KEEP + 3 {
            h.push(&i.to_string());
        }
        assert_eq!(h.entries.len(), KEEP);
        assert_eq!(h.get(0), Some("3"));
    }

    #[test]
    fn an_unreadable_history_is_kept() {
        let dir = crate::stored::tests::scratch("history");
        let path = dir.join("history.json");
        std::fs::write(&path, r#"["one", 2]"#).unwrap();
        let mut h = History::load_from(&path);
        assert_eq!(h.get(0), None);
        h.push("three");
        write_to(&path, &h.bytes());
        assert_eq!(History::load_from(&path).get(0), Some("three"));
        assert_eq!(
            crate::stored::tests::kept(&path).as_deref(),
            Some(r#"["one", 2]"#)
        );
        let _ = std::fs::remove_dir_all(dir);
    }
}
