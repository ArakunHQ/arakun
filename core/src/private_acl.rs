//! Windows access rules for files and folders only their user may open.
//!
//! What Unix does with modes 0600 and 0700: a DACL with full access for the
//! current user and SYSTEM, protected, so nothing is inherited from the
//! folder above. Files and folders are created with it, so there is no moment
//! when they carry the folder's rules; a folder's entries are inherited by
//! what is made in it later.

use std::ffi::OsString;
use std::fs::File;
use std::io;
use std::os::windows::ffi::OsStrExt as _;
use std::path::{Component, Path, Prefix};
use std::sync::OnceLock;

use windows_sys::Win32::Foundation::LocalFree;
use windows_sys::Win32::Security::Authorization::{
    ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
};
use windows_sys::Win32::Security::{PSECURITY_DESCRIPTOR, SECURITY_ATTRIBUTES};

/// Creates `path` as a new, empty file to write, failing with `AlreadyExists`
/// when anything is there (like `OpenOptions::create_new`).
pub(crate) fn create_new_file(path: &Path) -> io::Result<File> {
    use std::os::windows::io::FromRawHandle as _;
    use windows_sys::Win32::Foundation::{GENERIC_WRITE, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::Storage::FileSystem::{
        CREATE_NEW, CreateFileW, FILE_ATTRIBUTE_NORMAL, FILE_SHARE_DELETE, FILE_SHARE_READ,
        FILE_SHARE_WRITE,
    };
    let sd = Descriptor::new(false)?;
    let sa = sd.attributes();
    let name = wide(path)?;
    // SAFETY: `name` is NUL-terminated and `sa` points at `sd`, both alive
    // for the call. The handle is new and handed to `File`, which closes it.
    unsafe {
        let handle = CreateFileW(
            name.as_ptr(),
            GENERIC_WRITE,
            // What std opens files with, so a rename over it still works.
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            &sa,
            CREATE_NEW,
            FILE_ATTRIBUTE_NORMAL,
            std::ptr::null_mut(),
        );
        if handle == INVALID_HANDLE_VALUE {
            return Err(io::Error::last_os_error());
        }
        Ok(File::from_raw_handle(handle))
    }
}

/// Creates `dir` and its missing parents, each private. One already there
/// keeps its rules, as on Unix.
pub(crate) fn create_dir_all(dir: &Path) -> io::Result<()> {
    if dir.as_os_str().is_empty() || dir.is_dir() {
        return Ok(());
    }
    if let Some(parent) = dir.parent() {
        create_dir_all(parent)?;
    }
    match create_dir(dir) {
        Ok(()) => Ok(()),
        // Made by someone else in the meantime, like `fs::create_dir_all`.
        Err(_) if dir.is_dir() => Ok(()),
        Err(e) => Err(e),
    }
}

fn create_dir(dir: &Path) -> io::Result<()> {
    use windows_sys::Win32::Storage::FileSystem::CreateDirectoryW;
    let sd = Descriptor::new(true)?;
    let sa = sd.attributes();
    let name = wide(dir)?;
    // SAFETY: `name` is NUL-terminated and `sa` points at `sd`, both alive
    // for the call.
    if unsafe { CreateDirectoryW(name.as_ptr(), &sa) } == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// Replaces the DACL of a file that is already there with the private one.
/// For sockets, which can't be created with it, and for logs from before
/// (`durable::open_private_log`).
pub(crate) fn set(path: &Path) -> io::Result<()> {
    use std::os::windows::fs::OpenOptionsExt as _;
    use std::os::windows::io::AsRawHandle as _;
    use windows_sys::Win32::Security::Authorization::{SE_FILE_OBJECT, SetSecurityInfo};
    use windows_sys::Win32::Security::{
        ACL, DACL_SECURITY_INFORMATION, GetSecurityDescriptorDacl,
        PROTECTED_DACL_SECURITY_INFORMATION,
    };
    use windows_sys::Win32::Storage::FileSystem::{
        FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT, READ_CONTROL, WRITE_DAC,
    };
    let sd = Descriptor::new(false)?;
    let (mut present, mut defaulted) = (0, 0);
    let mut dacl: *mut ACL = std::ptr::null_mut();
    // SAFETY: `sd` is a valid descriptor that outlives `dacl`, which points
    // into it.
    if unsafe { GetSecurityDescriptorDacl(sd.0, &mut present, &mut dacl, &mut defaulted) } == 0 {
        return Err(io::Error::last_os_error());
    }
    // A socket is a reparse point: open it, not what it would point at.
    let file = std::fs::OpenOptions::new()
        .access_mode(READ_CONTROL | WRITE_DAC)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT | FILE_FLAG_BACKUP_SEMANTICS)
        .open(path)?;
    // SAFETY: the handle is open, and `dacl` is alive with `sd`.
    match unsafe {
        SetSecurityInfo(
            file.as_raw_handle(),
            SE_FILE_OBJECT,
            DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            dacl,
            std::ptr::null(),
        )
    } {
        0 => Ok(()),
        e => Err(io::Error::from_raw_os_error(e as i32)),
    }
}

/// A security descriptor Windows allocated, freed on drop.
struct Descriptor(PSECURITY_DESCRIPTOR);

impl Descriptor {
    /// The private DACL; `folder` makes its entries inherited by the files
    /// and folders made inside.
    fn new(folder: bool) -> io::Result<Self> {
        let sddl: Vec<u16> = sddl(user_sid()?, folder)
            .encode_utf16()
            .chain(Some(0))
            .collect();
        let mut sd: PSECURITY_DESCRIPTOR = std::ptr::null_mut();
        // SAFETY: `sddl` is NUL-terminated; Windows allocates `sd`, which
        // `Drop` frees.
        if unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                sddl.as_ptr(),
                SDDL_REVISION_1,
                &mut sd,
                std::ptr::null_mut(),
            )
        } == 0
        {
            return Err(io::Error::last_os_error());
        }
        Ok(Descriptor(sd))
    }

    fn attributes(&self) -> SECURITY_ATTRIBUTES {
        SECURITY_ATTRIBUTES {
            nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: self.0,
            bInheritHandle: 0,
        }
    }
}

impl Drop for Descriptor {
    fn drop(&mut self) {
        // SAFETY: allocated by Windows with LocalAlloc, freed once.
        unsafe { LocalFree(self.0) };
    }
}

/// The private DACL in SDDL: `P` stops inheritance from the folder above,
/// `FA` is full access, `SY` is SYSTEM.
fn sddl(user: &str, folder: bool) -> String {
    let inherit = if folder { "OICI" } else { "" };
    format!("D:P(A;{inherit};FA;;;{user})(A;{inherit};FA;;;SY)")
}

/// The current user's SID as a string, such as `S-1-5-21-…`.
pub(crate) fn user_sid() -> io::Result<&'static str> {
    static SID: OnceLock<String> = OnceLock::new();
    if let Some(sid) = SID.get() {
        return Ok(sid);
    }
    let sid = read_user_sid()?;
    Ok(SID.get_or_init(|| sid))
}

fn read_user_sid() -> io::Result<String> {
    use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};
    use windows_sys::Win32::Security::Authorization::ConvertSidToStringSidW;
    use windows_sys::Win32::Security::{GetTokenInformation, TOKEN_QUERY, TOKEN_USER, TokenUser};
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
        let sid = from_wide(wide_sid);
        LocalFree(wide_sid.cast());
        Ok(sid)
    }
}

/// # Safety
/// `s` points at a NUL-terminated UTF-16 string.
unsafe fn from_wide(s: *const u16) -> String {
    // SAFETY: the caller's promise.
    unsafe {
        let len = (0..).take_while(|&i| *s.add(i) != 0).count();
        String::from_utf16_lossy(std::slice::from_raw_parts(s, len))
    }
}

/// `path` for a Win32 call: made absolute and verbatim (`\\?\`), as std does,
/// so a path past 260 characters still works.
fn wide(path: &Path) -> io::Result<Vec<u16>> {
    let path = std::path::absolute(path)?;
    let mut name = OsString::new();
    match path.components().next() {
        Some(Component::Prefix(p)) if matches!(p.kind(), Prefix::Disk(_)) => {
            name.push(r"\\?\");
            name.push(&path);
        }
        Some(Component::Prefix(p)) if matches!(p.kind(), Prefix::UNC(..)) => {
            // `\\server\share\…` becomes `\\?\UNC\server\share\…`.
            name.push(r"\\?\UNC");
            let rest: Vec<u16> = path.as_os_str().encode_wide().skip(1).collect();
            name.push(String::from_utf16_lossy(&rest));
        }
        _ => name.push(&path),
    }
    Ok(name.encode_wide().chain(Some(0)).collect())
}

/// The DACL of `path` in SDDL, for tests.
#[cfg(test)]
fn dacl_sddl(path: &Path) -> io::Result<String> {
    use windows_sys::Win32::Security::Authorization::{GetNamedSecurityInfoW, SE_FILE_OBJECT};
    use windows_sys::Win32::Security::DACL_SECURITY_INFORMATION;
    let name = wide(path)?;
    let mut sd: PSECURITY_DESCRIPTOR = std::ptr::null_mut();
    // SAFETY: `name` is NUL-terminated; Windows allocates `sd`, which
    // `Descriptor` frees.
    let e = unsafe {
        GetNamedSecurityInfoW(
            name.as_ptr(),
            SE_FILE_OBJECT,
            DACL_SECURITY_INFORMATION,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            &mut sd,
        )
    };
    if e != 0 {
        return Err(io::Error::from_raw_os_error(e as i32));
    }
    Descriptor(sd).dacl_sddl()
}

#[cfg(test)]
impl Descriptor {
    /// The DACL in SDDL as Windows writes it, with its short names for
    /// well-known accounts.
    fn dacl_sddl(&self) -> io::Result<String> {
        use windows_sys::Win32::Security::Authorization::ConvertSecurityDescriptorToStringSecurityDescriptorW;
        use windows_sys::Win32::Security::DACL_SECURITY_INFORMATION;
        let mut s = std::ptr::null_mut();
        // SAFETY: `self.0` is a valid descriptor; Windows allocates the
        // string, freed once after it is copied.
        unsafe {
            if ConvertSecurityDescriptorToStringSecurityDescriptorW(
                self.0,
                SDDL_REVISION_1,
                DACL_SECURITY_INFORMATION,
                &mut s,
                std::ptr::null_mut(),
            ) == 0
            {
                return Err(io::Error::last_os_error());
            }
            let out = from_wide(s);
            LocalFree(s.cast());
            Ok(out)
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    /// Asserts that `path` lets in only this user and SYSTEM, with nothing
    /// inherited from above. `folder`: the entries are passed on to what is
    /// made inside.
    pub(crate) fn assert_private(path: &std::path::Path, folder: bool) {
        // The DACL control flags, then its entries in a set order.
        fn parts(sddl: &str) -> (String, Vec<String>) {
            let rest = sddl.strip_prefix("D:").unwrap_or_else(|| panic!("{sddl}"));
            let (flags, aces) = rest.split_at(rest.find('(').unwrap_or(rest.len()));
            let mut aces: Vec<String> = aces
                .split(['(', ')'])
                .filter(|a| !a.is_empty())
                .map(str::to_owned)
                .collect();
            aces.sort();
            (flags.to_owned(), aces)
        }
        let got = super::dacl_sddl(path).unwrap();
        let want = super::Descriptor::new(folder).unwrap().dacl_sddl().unwrap();
        let (flags, aces) = parts(&got);
        assert!(flags.contains('P'), "{}: inherits: {got}", path.display());
        assert_eq!(aces, parts(&want).1, "{}: {got}", path.display());
        assert_eq!(aces.len(), 2, "{want}");
    }

    #[test]
    fn a_new_file_and_folder_are_private() {
        let dir = std::env::temp_dir().join(format!("arakun-private-acl-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let deep = dir.join("a").join("b");
        super::create_dir_all(&deep).unwrap();
        assert_private(&dir, true);
        assert_private(&deep, true);
        let file = deep.join("f");
        drop(super::create_new_file(&file).unwrap());
        assert_private(&file, false);
        let e = super::create_new_file(&file).unwrap_err();
        assert_eq!(e.kind(), std::io::ErrorKind::AlreadyExists);
        std::fs::write(deep.join("plain"), b"x").unwrap();
        super::set(&deep.join("plain")).unwrap();
        assert_private(&deep.join("plain"), false);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
