//! Logs that only grow: once one passes its limit it moves aside to
//! `<name>.old.<ext>`, replacing the copy before, and a new one starts. A log
//! and its old copy stay under about twice the limit, and the latest lines are
//! always kept.

use std::fs::File;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard, PoisonError};

/// Held from a log's size check until it is open again, so two writers that
/// both see it full don't both move it aside: the second would put the new,
/// nearly empty log over the old copy.
static ROTATING: Mutex<()> = Mutex::new(());

fn lock() -> MutexGuard<'static, ()> {
    ROTATING.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Where `path` goes when it is full: `usage.jsonl` becomes `usage.old.jsonl`.
pub fn old_path(path: &Path) -> PathBuf {
    let stem = path.file_stem().unwrap_or_default().to_string_lossy();
    let name = match path.extension() {
        Some(ext) => format!("{stem}.old.{}", ext.to_string_lossy()),
        None => format!("{stem}.old"),
    };
    path.with_file_name(name)
}

/// Moves `path` aside when it is over `max` bytes. Call it holding `lock()`.
fn rotate(path: &Path, max: u64) {
    if std::fs::metadata(path).is_ok_and(|m| m.len() > max) {
        let _ = std::fs::rename(path, old_path(path));
    }
}

/// Logs hold what agents read and print, so only this user may read them.
fn open(path: &Path) -> io::Result<File> {
    crate::durable::open_private_log(path, false)
}

/// Appends `bytes` with one write, so lines from two writers never
/// interleave, after moving a full log aside.
pub fn append(path: &Path, bytes: &[u8], max: u64) -> io::Result<()> {
    let _rotating = lock();
    rotate(path, max);
    open(path)?.write_all(bytes)
}

/// How much one writer adds between two looks at the log's size.
const CHECK_EVERY: u64 = 64 * 1024;

/// A log held open for a long time, such as an agent's stderr for as long as
/// it runs. Its size is checked as it is written, not only when it is opened,
/// so one long session can't grow it past the limit.
pub struct Writer {
    path: PathBuf,
    file: File,
    max: u64,
    since_check: u64,
}

impl Writer {
    pub fn open(path: PathBuf, max: u64) -> io::Result<Self> {
        let _rotating = lock();
        rotate(&path, max);
        let file = open(&path)?;
        Ok(Self {
            path,
            file,
            max,
            since_check: 0,
        })
    }

    fn rotate_if_full(&mut self) {
        // The size of the file this handle writes to, which another writer may
        // have moved aside already; then the path holds a new, small log.
        if !self.file.metadata().is_ok_and(|m| m.len() > self.max) {
            return;
        }
        let _rotating = lock();
        rotate(&self.path, self.max);
        if let Ok(file) = open(&self.path) {
            self.file = file;
        }
    }
}

impl Write for Writer {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.since_check += buf.len() as u64;
        if self.since_check >= CHECK_EVERY {
            self.since_check = 0;
            self.rotate_if_full();
        }
        self.file.write(buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.file.flush()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("arakun-rotate-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn old_copies_keep_the_extension() {
        assert_eq!(
            old_path(Path::new("/d/usage.jsonl")),
            Path::new("/d/usage.old.jsonl")
        );
        assert_eq!(old_path(Path::new("/d/log")), Path::new("/d/log.old"));
    }

    #[test]
    fn a_full_log_moves_aside_before_the_next_line() {
        let dir = scratch("append");
        let path = dir.join("u.jsonl");
        append(&path, b"one\n", 4).unwrap();
        append(&path, b"two\n", 4).unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"one\ntwo\n");
        append(&path, b"three\n", 4).unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"three\n");
        assert_eq!(std::fs::read(old_path(&path)).unwrap(), b"one\ntwo\n");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_writer_held_open_rotates_while_it_writes() {
        let dir = scratch("writer");
        let path = dir.join("agent.log");
        let max = 2 * CHECK_EVERY;
        let mut w = Writer::open(path.clone(), max).unwrap();
        let line = vec![b'x'; 1024];
        for _ in 0..1024 {
            w.write_all(&line).unwrap();
        }
        let len = |p: &Path| std::fs::metadata(p).unwrap().len();
        assert!(len(&path) <= max + CHECK_EVERY, "{}", len(&path));
        assert!(len(&old_path(&path)) <= max + CHECK_EVERY);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
