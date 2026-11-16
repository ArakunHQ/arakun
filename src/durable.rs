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
    write_in(path, bytes, true, false)
}

/// `write` into a directory that must already be there: when it is gone,
/// this fails instead of making it again.
#[allow(dead_code)] // The examples include this file and don't use it.
pub fn replace(path: &Path, bytes: &[u8]) -> io::Result<()> {
    write_in(path, bytes, false, false)
}

/// `write` for a file other users of the machine must not read (agent
/// replies, tokens): on Unix the file ends up 0600, since the temp file is
/// created that way before anything goes in, and a directory it creates is
/// 0700. Elsewhere it is `write`.
#[allow(dead_code)] // the examples that include this file keep nothing private
pub fn write_private(path: &Path, bytes: &[u8]) -> io::Result<()> {
    write_in(path, bytes, true, true)
}

fn write_in(path: &Path, bytes: &[u8], make_dir: bool, private: bool) -> io::Result<()> {
    let dir = path
        .parent()
        .filter(|d| !d.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    if make_dir {
        let mut builder = std::fs::DirBuilder::new();
        builder.recursive(true);
        #[cfg(unix)]
        if private {
            use std::os::unix::fs::DirBuilderExt as _;
            builder.mode(0o700);
        }
        builder.create(dir)?;
    }
    let name = path
        .file_name()
        .ok_or_else(|| io::Error::other(format!("{} has no file name", path.display())))?;
    let (tmp, mut file) = create_temp(dir, name, private, || SEQ.fetch_add(1, Ordering::Relaxed))?;
    let replaced = file
        .write_all(bytes)
        .and_then(|()| file.sync_all())
        .and_then(|()| {
            drop(file);
            std::fs::rename(&tmp, path)
        });
    // The temp file is ours here: it was created by this call, so removing it
    // can't delete a file someone else put there.
    if replaced.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    replaced?;
    if let Ok(d) = File::open(dir) {
        let _ = d.sync_all();
    }
    Ok(())
}

/// How many taken temp names `create_temp` skips before it gives up.
const TEMP_TRIES: u32 = 64;

/// Creates a new temp file for `name` in `dir`. The file is created
/// exclusively: a file already at that name (left by a crash, or put there by
/// another user of a shared directory to read what we write) is never opened,
/// and the next name is tried. On Unix a private file is 0600 from the start,
/// which a pre-existing file would not be.
fn create_temp(
    dir: &Path,
    name: &std::ffi::OsStr,
    private: bool,
    mut next: impl FnMut() -> u64,
) -> io::Result<(std::path::PathBuf, File)> {
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    if private {
        use std::os::unix::fs::OpenOptionsExt as _;
        opts.mode(0o600);
    }
    #[cfg(not(unix))]
    let _ = private;
    for _ in 0..TEMP_TRIES {
        let mut tmp = OsString::from(".");
        tmp.push(name);
        tmp.push(format!(".{}-{}.tmp", std::process::id(), next()));
        let tmp = dir.join(tmp);
        match opts.open(&tmp) {
            Ok(file) => return Ok((tmp, file)),
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
        }
    }
    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        format!(
            "no free temp file name for {} in {}",
            name.to_string_lossy(),
            dir.display()
        ),
    ))
}

#[cfg(test)]
mod tests {
    fn scratch(name: &str) -> std::path::PathBuf {
        let dir =
            std::env::temp_dir().join(format!("arakun-durable-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    fn entries(dir: &std::path::Path) -> Vec<String> {
        let mut names: Vec<_> = std::fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
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

    #[cfg(unix)]
    #[test]
    fn a_private_file_and_its_new_directory_are_the_owners_alone() {
        use std::os::unix::fs::PermissionsExt as _;
        let mode = |p: &std::path::Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
        let dir = scratch("private");
        let path = dir.join("deep").join("notices.json");
        // Replacing a file that was readable by all must not keep its mode.
        std::fs::create_dir_all(dir.join("deep")).unwrap();
        std::fs::write(&path, b"old").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        super::write_private(&path, b"new").unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"new");
        assert_eq!(mode(&path), 0o600);
        let fresh = dir.join("fresh").join("notices.json");
        super::write_private(&fresh, b"x").unwrap();
        assert_eq!(mode(&fresh), 0o600);
        assert_eq!(mode(&dir.join("fresh")), 0o700);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[cfg(unix)]
    #[test]
    fn a_temp_file_someone_else_made_is_left_alone() {
        use std::os::unix::fs::PermissionsExt as _;
        let mode = |p: &std::path::Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
        let dir = scratch("planted");
        std::fs::create_dir_all(&dir).unwrap();
        // Another user plants a file readable by all at the first name.
        let planted = dir.join(format!(".notices.json.{}-0.tmp", std::process::id()));
        std::fs::write(&planted, b"planted").unwrap();
        std::fs::set_permissions(&planted, std::fs::Permissions::from_mode(0o666)).unwrap();
        let mut seq = 0..;
        let (tmp, _file) =
            super::create_temp(&dir, std::ffi::OsStr::new("notices.json"), true, || {
                seq.next().unwrap()
            })
            .unwrap();
        assert_ne!(tmp, planted);
        assert_eq!(mode(&tmp), 0o600);
        assert_eq!(std::fs::read(&planted).unwrap(), b"planted");
        assert_eq!(mode(&planted), 0o666);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn gives_up_when_every_temp_name_is_taken() {
        let dir = scratch("taken-names");
        std::fs::create_dir_all(&dir).unwrap();
        let planted = dir.join(format!(".f.{}-7.tmp", std::process::id()));
        std::fs::write(&planted, b"x").unwrap();
        let err = super::create_temp(&dir, std::ffi::OsStr::new("f"), false, || 7).unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::AlreadyExists);
        assert_eq!(std::fs::read(&planted).unwrap(), b"x");
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
