//! Binds a local socket only its user can open, from the moment it has a name.
//!
//! `bind` then `chmod` leaves a moment when another user on the machine can
//! connect. On Unix the socket is bound inside a new 0700 folder, made 0600,
//! and only then linked to its real name. On Windows it gets a DACL that lets
//! in only the current user and SYSTEM.

use std::io;
#[cfg(unix)]
use std::os::unix::net::UnixListener;
use std::path::Path;

#[cfg(windows)]
use uds_windows::UnixListener;

/// Binds `path`, open only to this user. Fails with `AddrInUse` when something
/// is already at `path`, like `UnixListener::bind`, so the caller can check
/// whether it still answers.
#[cfg(unix)]
pub fn bind(path: &Path) -> io::Result<UnixListener> {
    use std::os::unix::fs::{DirBuilderExt as _, PermissionsExt as _};
    use std::sync::atomic::{AtomicU32, Ordering};

    static NEXT: AtomicU32 = AtomicU32::new(0);
    let parent = match path.parent() {
        Some(p) if !p.as_os_str().is_empty() => p,
        _ => Path::new("."),
    };
    // Next to `path`, so the link stays on one file system. A short name, as a
    // socket path has a length limit of about 100 bytes.
    let mut tries = 0;
    let dir = loop {
        let dir = parent.join(format!(
            ".arakun-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        match std::fs::DirBuilder::new().mode(0o700).create(&dir) {
            Ok(()) => break Gone(dir),
            // Left by a crashed process that had our pid, or made by someone
            // else: either way not ours to use.
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists && tries < 16 => tries += 1,
            Err(e) => return Err(e),
        }
    };
    let tmp = dir.0.join("s");
    let listener = UnixListener::bind(&tmp)?;
    std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600))?;
    // Unlike a rename, a link does not replace what is there: two instances
    // that start at once cannot both take the name.
    match std::fs::hard_link(&tmp, path) {
        Ok(()) => Ok(listener),
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => Err(io::Error::new(
            io::ErrorKind::AddrInUse,
            format!("{} is in use", path.display()),
        )),
        Err(e) => Err(e),
    }
}

/// Removes the folder the socket was bound in; the socket lives on under its
/// real name.
#[cfg(unix)]
struct Gone(std::path::PathBuf);

#[cfg(unix)]
impl Drop for Gone {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Binds `path` and lets in only this user (and SYSTEM). The socket inherits its folder's
/// ACL until then; arakun's folders are in the user's profile, which other
/// users cannot open.
#[cfg(windows)]
pub fn bind(path: &Path) -> io::Result<UnixListener> {
    let listener = UnixListener::bind(path)?;
    if let Err(e) = crate::private_acl::set(path) {
        drop(listener);
        let _ = std::fs::remove_file(path);
        return Err(e);
    }
    Ok(listener)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(name: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!(
            "arakun-private-socket-{name}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn answers_under_its_name_and_leaves_nothing_else() {
        let d = temp("answers");
        let sock = d.join("c.sock");
        let listener = bind(&sock).unwrap();
        #[cfg(unix)]
        let _client = std::os::unix::net::UnixStream::connect(&sock).unwrap();
        #[cfg(windows)]
        let _client = uds_windows::UnixStream::connect(&sock).unwrap();
        listener.accept().unwrap();
        let names: Vec<_> = std::fs::read_dir(&d)
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(names, ["c.sock"]);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[cfg(unix)]
    #[test]
    fn only_its_user_can_open_it() {
        use std::os::unix::fs::PermissionsExt as _;
        let d = temp("mode");
        let sock = d.join("c.sock");
        let _listener = bind(&sock).unwrap();
        let mode = std::fs::metadata(&sock).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn a_taken_name_is_in_use() {
        let d = temp("taken");
        let sock = d.join("c.sock");
        let _first = bind(&sock).unwrap();
        let e = bind(&sock).unwrap_err();
        assert_eq!(e.kind(), io::ErrorKind::AddrInUse);
        let _ = std::fs::remove_dir_all(&d);
    }
}
