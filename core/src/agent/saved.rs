//! A saved transcript, read once and kept while its file stays as it was.
//!
//! A device reads a long session a page at a time, and every page asks for
//! the same file. Reading and parsing all of it again for each page would
//! hold up the main thread once per page, for longer the longer the session.

use std::cell::{Cell, RefCell};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use super::{Entry, SubagentRecord, Thread};

/// How long a read is kept with nobody asking for it.
const KEEP: Duration = Duration::from_secs(60);

struct Kept {
    path: PathBuf,
    /// When the file was written and how long it was: a save changes these.
    stamp: (SystemTime, u64),
    entries: Vec<Arc<Entry>>,
    cwd: PathBuf,
    /// The follow-ups that waited when it was saved.
    queue: Vec<String>,
    /// Its subagents, each with its own transcript.
    subagents: Vec<SubagentRecord>,
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

/// A transcript read by [`read`], for [`LastRead::keep`].
pub struct Read(Kept);

/// Reads the transcript at `path`. A long one takes a while to parse, so
/// call it off the main thread and hand the result to [`LastRead::keep`].
pub fn read(path: &Path) -> Option<Read> {
    load(path).map(Read)
}

fn load(path: &Path) -> Option<Kept> {
    // Taken before the read: a save that lands in between leaves an old
    // stamp on new entries, and the next call reads again.
    let stamp = stamp(path);
    let (Ok(read), Some(stamp)) = (Thread::read_transcript(path), stamp) else {
        return None;
    };
    Some(Kept {
        path: path.to_path_buf(),
        stamp,
        entries: read.entries,
        cwd: read.meta.cwd,
        queue: read.queue,
        subagents: read.subagents,
        used: Cell::new(Instant::now()),
    })
}

impl LastRead {
    /// Runs `f` over the entries and the directory saved at `path`. A file
    /// that is missing or cannot be read has no entries, and is read again
    /// the next time: only a read that worked is kept.
    pub fn with<R>(&self, path: &Path, f: impl FnOnce(&[Arc<Entry>], &Path) -> R) -> R {
        match self.kept(path) {
            Some(kept) => f(&kept.entries, &kept.cwd),
            None => f(&[], Path::new("")),
        }
    }

    /// The follow-ups that waited in the transcript saved at `path`: a
    /// restored thread holds them, paused, until someone lets them go.
    pub fn queue(&self, path: &Path) -> Vec<String> {
        self.kept(path).map(|k| k.queue.clone()).unwrap_or_default()
    }

    /// The subagents saved at `path`, with the directory, as [`LastRead::with`] reads it.
    pub fn with_subagents<R>(
        &self,
        path: &Path,
        f: impl FnOnce(&[SubagentRecord], &Path) -> R,
    ) -> R {
        match self.kept(path) {
            Some(kept) => f(&kept.subagents, &kept.cwd),
            None => f(&[], Path::new("")),
        }
    }

    fn kept(&self, path: &Path) -> Option<Rc<Kept>> {
        let kept = match self.fresh(path) {
            Some(kept) => kept,
            None => {
                #[cfg(test)]
                self.reads.set(self.reads.get() + 1);
                // A read that failed is not kept: the file may be readable
                // again with the same time and length (its folder was, for a
                // moment, not), and the empty answer would stay for good.
                let kept = load(path).map(Rc::new);
                self.kept.borrow_mut().clone_from(&kept);
                kept?
            }
        };
        kept.used.set(Instant::now());
        Some(kept)
    }

    /// The read kept for `path`, if the file has not changed since.
    fn fresh(&self, path: &Path) -> Option<Rc<Kept>> {
        let stamp = stamp(path);
        self.kept
            .borrow()
            .clone()
            .filter(|k| k.path == path && Some(k.stamp) == stamp)
    }

    /// Whether the read kept for `path` is still what the file holds, so
    /// [`LastRead::with_kept`] has it without a new [`read`].
    pub fn is_kept(&self, path: &Path) -> bool {
        self.fresh(path).is_some()
    }

    /// Keeps what [`read`] got. A read that failed lets go of the last one,
    /// as [`LastRead::with`] does.
    pub fn keep(&self, read: Option<Read>) {
        *self.kept.borrow_mut() = read.map(|r| Rc::new(r.0));
    }

    /// Like [`LastRead::with`], but never reads the file: the caller read it
    /// off the main thread with [`read`]. The read kept for `path` answers
    /// even if a save landed since; with none, there are no entries.
    pub fn with_kept<R>(&self, path: &Path, f: impl FnOnce(&[Arc<Entry>], &Path) -> R) -> R {
        match self.peek(path) {
            Some(kept) => f(&kept.entries, &kept.cwd),
            None => f(&[], Path::new("")),
        }
    }

    /// Like [`LastRead::queue`], but never reads the file.
    pub fn queue_kept(&self, path: &Path) -> Vec<String> {
        self.peek(path).map(|k| k.queue.clone()).unwrap_or_default()
    }

    /// Like [`LastRead::with_subagents`], but never reads the file.
    pub fn with_kept_subagents<R>(
        &self,
        path: &Path,
        f: impl FnOnce(&[SubagentRecord], &Path) -> R,
    ) -> R {
        match self.peek(path) {
            Some(kept) => f(&kept.subagents, &kept.cwd),
            None => f(&[], Path::new("")),
        }
    }

    fn peek(&self, path: &Path) -> Option<Rc<Kept>> {
        let kept = self.kept.borrow().clone().filter(|k| k.path == path)?;
        kept.used.set(Instant::now());
        Some(kept)
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

    #[test]
    fn the_queue_saved_comes_with_the_read() {
        let dir = std::env::temp_dir().join(format!("arakun-last-queue-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("q.json");
        save(&path, &["one"]);
        let last = LastRead::default();
        // None saved: none to show.
        assert!(last.queue(&path).is_empty());
        let mut t: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        t["queue"] = serde_json::json!(["next", "after"]);
        std::fs::write(&path, t.to_string()).unwrap();
        assert_eq!(last.queue(&path), ["next", "after"]);
        // The entries come from the same read.
        let before = last.reads.get();
        assert_eq!(last.with(&path, |all, _| all.len()), 1);
        assert_eq!(last.reads.get(), before);
        // A file gone has none.
        std::fs::remove_dir_all(&dir).unwrap();
        assert!(last.queue(&path).is_empty());
    }

    #[test]
    fn the_subagents_saved_come_with_the_read() {
        let dir = std::env::temp_dir().join(format!("arakun-last-subs-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("s.json");
        save(&path, &["one"]);
        let mut t: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        t["subagents"] = serde_json::json!([{
            "id": "a", "name": "Explore", "task": "look", "state": "completed", "started": 1,
            "call": "toolu_1", "entries": [{ "type": "agent", "text": "found it" }],
        }]);
        std::fs::write(&path, t.to_string()).unwrap();
        let last = LastRead::default();
        let found = |sub: &str| {
            last.with_subagents(&path, |all, cwd| {
                let r = all.iter().find(|r| r.answers_to(sub))?;
                Some((r.id.clone(), r.entries.len(), cwd.to_path_buf()))
            })
        };
        // By its own id or by the call that started it, from one read.
        assert_eq!(found("a"), Some(("a".into(), 1, PathBuf::from("/work"))));
        assert_eq!(found("toolu_1").map(|f| f.0), Some("a".into()));
        assert_eq!(found("b"), None);
        assert_eq!(last.reads.get(), 1);
        assert_eq!(last.with_kept_subagents(&path, |all, _| all.len()), 1);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_read_made_elsewhere_is_kept_and_answers_without_reading() {
        let dir = std::env::temp_dir().join(format!("arakun-last-keep-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("k.json");
        save(&path, &["one", "two"]);
        let last = LastRead::default();
        assert!(!last.is_kept(&path));
        // Nothing kept: no entries, and no read on this thread.
        assert_eq!(last.with_kept(&path, |all, _| all.len()), 0);
        let got = std::thread::scope(|s| s.spawn(|| read(&path)).join().unwrap());
        last.keep(got);
        assert!(last.is_kept(&path));
        assert_eq!(last.with_kept(&path, |all, _| all.len()), 2);
        assert_eq!(last.with(&path, |all, _| all.len()), 2);
        assert_eq!(last.reads.get(), 0);
        // A save since: no longer current, but what was read still answers.
        save(&path, &["one", "two", "three"]);
        assert!(!last.is_kept(&path));
        assert_eq!(last.with_kept(&path, |all, _| all.len()), 2);
        // A read that failed lets go of it.
        last.keep(read(&dir.join("missing.json")));
        assert_eq!(last.with_kept(&path, |all, _| all.len()), 0);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
