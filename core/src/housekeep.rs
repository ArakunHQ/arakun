//! Removing files Arakun made and no longer needs: photos sent to a server,
//! transcripts of sessions nothing lists, temp files a crash left behind.
//! The server (`server::store`) and the app each say which folders to look in.
//!
//! A file goes only once it has not changed for a while, by its modified
//! time, so one still being written or read is never taken. Only plain files
//! go: a folder or a link is left alone.

use std::path::Path;
use std::time::{Duration, SystemTime};

pub const DAY: Duration = Duration::from_secs(86_400);

/// Removes the files in `dir` (not below it) whose name `pick` accepts and
/// that have not changed for `age`. Returns how many went. A folder that is
/// not there holds nothing to remove.
pub fn remove_old(dir: &Path, age: Duration, pick: impl Fn(&str) -> bool) -> usize {
    let now = SystemTime::now();
    let Ok(entries) = std::fs::read_dir(dir) else {
        return 0;
    };
    let mut removed = 0;
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        if !pick(name) {
            continue;
        }
        // `DirEntry::metadata` does not follow a link, so a link is not a file here.
        let Ok(meta) = entry.metadata() else { continue };
        if !meta.is_file() {
            continue;
        }
        let old = meta
            .modified()
            .ok()
            .and_then(|m| now.duration_since(m).ok())
            .is_some_and(|since| since >= age);
        if old && std::fs::remove_file(entry.path()).is_ok() {
            removed += 1;
        }
    }
    removed
}

/// Whether `name` is the temp file of a `durable` write
/// (`.<name>.<pid>-<n>.tmp`). One that is still there a day later belongs to
/// a write that died before its rename.
pub fn is_write_leftover(name: &str) -> bool {
    let Some(rest) = name.strip_prefix('.').and_then(|n| n.strip_suffix(".tmp")) else {
        return false;
    };
    let Some((file, stamp)) = rest.rsplit_once('.') else {
        return false;
    };
    let digits = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
    !file.is_empty()
        && stamp
            .split_once('-')
            .is_some_and(|(pid, n)| digits(pid) && digits(n))
}

/// Removes the temp files of `durable` writes in `dir` that died a day or
/// more ago.
pub fn remove_write_leftovers(dir: &Path) -> usize {
    remove_old(dir, DAY, is_write_leftover)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn temp(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("arakun-housekeep-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn file_aged(dir: &Path, name: &str, age: Duration) -> PathBuf {
        let path = dir.join(name);
        let f = std::fs::File::create(&path).unwrap();
        f.set_modified(SystemTime::now() - age).unwrap();
        path
    }

    #[test]
    fn only_old_picked_files_go() {
        let dir = temp("old");
        let old = file_aged(&dir, "photo-1.png", 3 * DAY);
        let new = file_aged(&dir, "photo-2.png", Duration::from_secs(60));
        let other = file_aged(&dir, "keep.png", 3 * DAY);
        std::fs::create_dir(dir.join("photo-dir")).unwrap();
        let n = remove_old(&dir, 2 * DAY, |n| n.starts_with("photo-"));
        assert_eq!(n, 1);
        assert!(!old.exists());
        assert!(new.exists());
        assert!(other.exists());
        assert!(dir.join("photo-dir").is_dir());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_missing_folder_holds_nothing() {
        let dir = std::env::temp_dir().join("arakun-housekeep-not-there");
        assert_eq!(remove_old(&dir, DAY, |_| true), 0);
    }

    #[test]
    fn write_leftovers_are_told_by_their_name() {
        assert!(is_write_leftover(".state.json.123-4.tmp"));
        assert!(is_write_leftover(".s18f-0.json.99-0.tmp"));
        assert!(!is_write_leftover("state.json.123-4.tmp"));
        assert!(!is_write_leftover(".state.json.tmp"));
        assert!(!is_write_leftover(".state.json.12a-4.tmp"));
        assert!(!is_write_leftover("..1-2.tmp"));
        assert!(!is_write_leftover(".notes.tmp"));
    }

    #[test]
    fn a_fresh_write_leftover_stays() {
        let dir = temp("leftover");
        let dead = file_aged(&dir, ".state.json.1-0.tmp", 2 * DAY);
        let live = file_aged(&dir, ".state.json.2-0.tmp", Duration::ZERO);
        assert_eq!(remove_write_leftovers(&dir), 1);
        assert!(!dead.exists());
        assert!(live.exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_link_is_not_followed_or_removed() {
        #[cfg(unix)]
        {
            let dir = temp("link");
            let target = file_aged(&dir, "target", 3 * DAY);
            let link = dir.join("photo-link");
            std::os::unix::fs::symlink(&target, &link).unwrap();
            assert_eq!(remove_old(&dir, DAY, |n| n.starts_with("photo-")), 0);
            assert!(link.symlink_metadata().is_ok());
            assert!(target.exists());
            let _ = std::fs::remove_dir_all(&dir);
        }
    }
}
