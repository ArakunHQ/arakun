//! Durable file replacement for state, transcripts and caches.

use std::ffi::OsString;
use std::fs::File;
use std::io::{self, Write as _};
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};

static SEQ: AtomicU64 = AtomicU64::new(0);

/// Replaces `path` with `bytes` so that a crash or a power cut leaves either
/// the old file or the new one, never an empty or torn one: the bytes go to a
/// temp file next to it that is flushed to disk before it is renamed over, and
/// the directory is flushed after so the rename itself survives. Each call
/// gets its own temp file, so two saves of the same file can't interleave.
/// Creates the parent directory.
pub fn write(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let dir = path.parent().filter(|d| !d.as_os_str().is_empty()).unwrap_or(Path::new("."));
    std::fs::create_dir_all(dir)?;
    let name = path.file_name().ok_or_else(|| io::Error::other(format!("{} has no file name", path.display())))?;
    let mut tmp = OsString::from(".");
    tmp.push(name);
    tmp.push(format!(".{}-{}.tmp", std::process::id(), SEQ.fetch_add(1, Ordering::Relaxed)));
    let tmp = dir.join(tmp);
    let replaced = File::create(&tmp)
        .and_then(|mut f| {
            f.write_all(bytes)?;
            f.sync_all()
        })
        .and_then(|()| std::fs::rename(&tmp, path));
    if replaced.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    replaced?;
    if let Ok(d) = File::open(dir) {
        let _ = d.sync_all();
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    fn scratch(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("arakun-durable-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    fn entries(dir: &std::path::Path) -> Vec<String> {
        let mut names: Vec<_> =
            std::fs::read_dir(dir).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect();
        names.sort();
        names
    }

    #[test]
    fn replaces_the_file_and_leaves_nothing_behind() {
        let dir = scratch("ok");
        let path = dir.join("deep").join("state.json");
        super::write(&path, b"one").unwrap();
        super::write(&path, b"two").unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"two");
        assert_eq!(entries(&dir.join("deep")), ["state.json"]);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_failed_write_keeps_no_temp_file() {
        let dir = scratch("fail");
        let path = dir.join("taken");
        std::fs::create_dir_all(path.join("inside")).unwrap();
        assert!(super::write(&path, b"x").is_err());
        assert_eq!(entries(&dir), ["taken"]);
        let _ = std::fs::remove_dir_all(dir);
    }
}
