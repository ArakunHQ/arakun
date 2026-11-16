//! A saved transcript, read once and kept while its file stays as it was.
//!
//! A device reads a long session a page at a time, and every page asks for
//! the same file. Reading and parsing all of it again for each page would
//! hold up the main thread once per page, for longer the longer the session.

use std::cell::{Cell, RefCell};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::{Duration, Instant, SystemTime};

use super::{Entry, Thread};

/// How long a read is kept with nobody asking for it.
const KEEP: Duration = Duration::from_secs(60);

struct Kept {
    path: PathBuf,
    /// When the file was written and how long it was: a save changes these.
    stamp: (SystemTime, u64),
    entries: Vec<Entry>,
    cwd: PathBuf,
    used: Cell<Instant>,
}

/// The transcript read last. One is enough: pages of one session follow each other.
#[derive(Default)]
pub struct LastRead {
    kept: RefCell<Option<Rc<Kept>>>,
    #[cfg(test)]
    reads: Cell<usize>,
}

fn stamp(path: &Path) -> Option<(SystemTime, u64)> {
    let meta = std::fs::metadata(path).ok()?;
    Some((meta.modified().ok()?, meta.len()))
}

impl LastRead {
    /// Runs `f` over the entries and the directory saved at `path`. A file
    /// that is missing or cannot be read has no entries, and is read again
    /// the next time: only a read that worked is kept.
    pub fn with<R>(&self, path: &Path, f: impl FnOnce(&[Entry], &Path) -> R) -> R {
        // Taken before the read: a save that lands in between leaves an old
        // stamp on new entries, and the next call reads again.
        let stamp = stamp(path);
        let kept = self
            .kept
            .borrow()
            .clone()
            .filter(|k| k.path == path && Some(k.stamp) == stamp);
        let kept = match kept {
            Some(kept) => kept,
            None => {
                #[cfg(test)]
                self.reads.set(self.reads.get() + 1);
                // A read that failed is not kept: the file may be readable
                // again with the same time and length (its folder was, for a
                // moment, not), and the empty answer would stay for good.
                let (Ok(read), Some(stamp)) = (Thread::read_transcript(path), stamp) else {
                    *self.kept.borrow_mut() = None;
                    return f(&[], Path::new(""));
                };
                let (entries, cwd) = (read.entries, read.meta.cwd);
                let kept = Rc::new(Kept {
                    path: path.to_path_buf(),
                    stamp,
                    entries,
                    cwd,
                    used: Cell::new(Instant::now()),
                });
                *self.kept.borrow_mut() = Some(kept.clone());
                kept
            }
        };
        kept.used.set(Instant::now());
        f(&kept.entries, &kept.cwd)
    }

    /// Lets go of a read nobody asked for in a while. Call it now and then.
    pub fn trim(&self) {
        let mut kept = self.kept.borrow_mut();
        if kept.as_ref().is_some_and(|k| k.used.get().elapsed() > KEEP) {
            *kept = None;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn save(path: &Path, texts: &[&str]) {
        let entries: Vec<_> = texts
            .iter()
            .map(|t| serde_json::json!({ "type": "agent", "text": t }))
            .collect();
        let transcript = serde_json::json!({
            "version": 1,
            "meta": {
                "agent": "claude", "session_id": null, "title": "t", "cwd": "/work",
                "created": 0, "updated": 0,
            },
            "entries": entries,
        });
        std::fs::write(path, transcript.to_string()).unwrap();
    }

    #[test]
    fn a_file_is_read_once_until_it_changes() {
        let dir = std::env::temp_dir().join(format!("arakun-last-read-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let (a, b) = (dir.join("a.json"), dir.join("b.json"));
        save(&a, &["one", "two"]);
        save(&b, &["other"]);
        let last = LastRead::default();
        let count = |path: &Path| last.with(path, |all, cwd| (all.len(), cwd.to_path_buf()));

        // Page after page of one session: one read.
        assert_eq!(count(&a), (2, PathBuf::from("/work")));
        assert_eq!(count(&a).0, 2);
        assert_eq!(last.reads.get(), 1);
        // A save since: read again.
        save(&a, &["one", "two", "three"]);
        assert_eq!(count(&a).0, 3);
        assert_eq!(last.reads.get(), 2);
        // Another session takes its place, and the first is read again after.
        assert_eq!(count(&b).0, 1);
        assert_eq!(count(&a).0, 3);
        assert_eq!(last.reads.get(), 4);
        // A file that is gone has nothing, and nothing stale is kept for it.
        std::fs::remove_file(&a).unwrap();
        assert_eq!(count(&a), (0, PathBuf::new()));
        // A file that cannot be read has nothing either, and is tried again
        // each time: it may be whole the next time with the same stamp.
        std::fs::write(&a, "{ not a transcript").unwrap();
        let before = last.reads.get();
        assert_eq!(count(&a), (0, PathBuf::new()));
        assert_eq!(count(&a), (0, PathBuf::new()));
        assert_eq!(last.reads.get(), before + 2);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
