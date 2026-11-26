//! Binds a local socket only its user can open, from the moment it has a name.
//!
//! `bind` then `chmod` leaves a moment when another user on the machine can
//! connect. On Unix the socket is bound inside a new 0700 folder, made 0600,
//! and only then linked to its real name. On Windows it gets a DACL that lets
//! in only the current user.

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

/// Binds `path` and lets in only this user. The socket inherits its folder's
/// ACL until then; arakun's folders are in the user's profile, which other
/// users cannot open.
#[cfg(windows)]
pub fn bind(path: &Path) -> io::Result<UnixListener> {
    let listener = UnixListener::bind(path)?;
    if let Err(e) = owner_only(path) {
        drop(listener);
        let _ = std::fs::remove_file(path);
        return Err(e);
    }
    Ok(listener)
}

/// Replaces the file's DACL with one entry, full access for the current user,
/// and stops it inheriting from its folder.
#[cfg(windows)]
fn owner_only(path: &Path) -> io::Result<()> {
    use std::os::windows::fs::OpenOptionsExt as _;
    use std::os::windows::io::AsRawHandle as _;
    use windows_sys::Win32::Foundation::{CloseHandle, HANDLE, LocalFree};
    use windows_sys::Win32::Security::Authorization::{
        ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW,
        SDDL_REVISION_1, SE_FILE_OBJECT, SetSecurityInfo,
    };
    use windows_sys::Win32::Security::{
        ACL, DACL_SECURITY_INFORMATION, GetSecurityDescriptorDacl, GetTokenInformation,
        PROTECTED_DACL_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR, TOKEN_QUERY, TOKEN_USER,
        TokenUser,
    };
    use windows_sys::Win32::Storage::FileSystem::{
        FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT, READ_CONTROL, WRITE_DAC,
    };
    use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

    let last = io::Error::last_os_error;
    // SAFETY: each pointer handed to Windows points at a live local or at
    // memory Windows allocated, which is freed once, after its last use.
    unsafe {
        let mut token: HANDLE = std::ptr::null_mut();
        if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) == 0 {
            return Err(last());
        }
        // u64s keep TOKEN_USER's pointers aligned.
        let mut buf = vec![0u64; 64];
        let mut len = 0u32;
        let ok = GetTokenInformation(
            token,
            TokenUser,
            buf.as_mut_ptr().cast(),
            (buf.len() * 8) as u32,
            &mut len,
        );
        let err = last();
        CloseHandle(token);
        if ok == 0 {
            return Err(err);
        }
        let user = &*(buf.as_ptr() as *const TOKEN_USER);
        let mut wide_sid = std::ptr::null_mut();
        if ConvertSidToStringSidW(user.User.Sid, &mut wide_sid) == 0 {
            return Err(last());
        }
        let sid = String::from_utf16_lossy(std::slice::from_raw_parts(
            wide_sid,
            (0..).take_while(|&i| *wide_sid.add(i) != 0).count(),
        ));
        LocalFree(wide_sid.cast());

        let sddl: Vec<u16> = format!("D:P(A;;FA;;;{sid})")
            .encode_utf16()
            .chain(Some(0))
            .collect();
        let mut sd: PSECURITY_DESCRIPTOR = std::ptr::null_mut();
        if ConvertStringSecurityDescriptorToSecurityDescriptorW(
            sddl.as_ptr(),
            SDDL_REVISION_1,
            &mut sd,
            std::ptr::null_mut(),
        ) == 0
        {
            return Err(last());
        }
        let (mut present, mut defaulted) = (0, 0);
        let mut dacl: *mut ACL = std::ptr::null_mut();
        if GetSecurityDescriptorDacl(sd, &mut present, &mut dacl, &mut defaulted) == 0 {
            let err = last();
            LocalFree(sd);
            return Err(err);
        }
        // The socket is a reparse point: open it, not what it would point at.
        let file = std::fs::OpenOptions::new()
            .access_mode(READ_CONTROL | WRITE_DAC)
            .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT | FILE_FLAG_BACKUP_SEMANTICS)
            .open(path);
        let result = file.and_then(|file| {
            match SetSecurityInfo(
                file.as_raw_handle(),
                SE_FILE_OBJECT,
                DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                dacl,
                std::ptr::null(),
            ) {
                0 => Ok(()),
                e => Err(io::Error::from_raw_os_error(e as i32)),
            }
        });
        LocalFree(sd);
        result
    }
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
