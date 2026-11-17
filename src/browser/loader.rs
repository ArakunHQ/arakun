//! Loads CEF the first time a browser tab needs it.
//!
//! Linux: the `cef` crate calls the CEF C API as ordinary extern functions, which would
//! make `libcef.so` a startup dependency of arakun: measured, that alone doubles the PSS of
//! a window with no browser tab (51 → 105 MB) because the dynamic linker relocates the
//! whole 270 MB library. Instead every C entry point the crate uses is defined here as a
//! two-instruction trampoline that jumps through a slot filled in by [`load`]. At build
//! time `-lcef` resolves to the empty stub from `tools/install-cef.sh`, which `--as-needed`
//! then drops, so a symbol missing from the list below shows up as a link error rather
//! than as a silent runtime dependency.
//!
//! macOS: the `cef` crate links CEF's wrapper, whose entry points stay unresolved until
//! [`load`] hands it the framework (`cef_load_library`), so nothing extra is needed here.
//!
//! Windows: the linker delay-loads `libcef.dll` (see build.rs), so the first CEF call
//! loads it. [`load`] loads it first by full path, so that call finds this copy and a
//! missing runtime is an error message instead of a crash.

use std::path::PathBuf;

#[cfg(target_os = "linux")]
pub use linux::load;
#[cfg(target_os = "macos")]
pub use macos::load;
#[cfg(windows)]
pub use windows::load;

/// Where the CEF runtime lives: `$ARAKUN_CEF_DIR`, else on Linux and macOS
/// `${XDG_DATA_HOME:-~/.local/share}/arakun/cef` (filled by `tools/install-cef.sh`) and on
/// Windows the folder of arakun.exe (where the build copies it). Inside Arakun.app the
/// bundle's own `Contents/Frameworks` comes first (tools/install.sh puts CEF there). On
/// Linux a `cef` folder next to the binary comes first: the release archive has one, so
/// it runs where it was unpacked.
pub fn runtime_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os("ARAKUN_CEF_DIR") {
        return dir.into();
    }
    #[cfg(target_os = "linux")]
    // Canonical, so a link to the binary finds the folder next to the real one.
    if let Some(dir) = std::env::current_exe()
        .and_then(std::fs::canonicalize)
        .ok()
        .and_then(|exe| Some(exe.parent()?.join("cef")))
        .filter(|dir| dir.join("libcef.so").is_file())
    {
        return dir;
    }
    #[cfg(target_os = "macos")]
    // Canonical: started through the `arakun` link in ~/.local/bin, current_exe is the link.
    if let Some(frameworks) = std::env::current_exe()
        .and_then(std::fs::canonicalize)
        .ok()
        .and_then(|exe| Some(exe.parent()?.parent()?.join("Frameworks")))
        .filter(|dir| dir.join("Chromium Embedded Framework.framework").is_dir())
    {
        return frameworks;
    }
    #[cfg(windows)]
    return std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(PathBuf::from))
        .unwrap_or_default();
    #[cfg(not(windows))]
    data_home().join("arakun/cef")
}

/// Why the runtime at `path` can't load. A release says how to get it back: the
/// updater of a release from before the browser left it out. A build from
/// source gets `dev_hint`.
fn missing(path: &std::path::Path, dev_hint: &str) -> String {
    // As crate::update::release_build(), which the browser demo does not build.
    if option_env!("ARAKUN_RELEASE_BUILD") != Some("1") {
        return format!("CEF runtime not found at {} ({dev_hint})", path.display());
    }
    let install = if cfg!(windows) {
        "irm https://arakun.dev/install.ps1 | iex"
    } else {
        "curl -fsSL https://arakun.dev/install | sh"
    };
    format!(
        "This Arakun has no browser runtime at {}. Quit Arakun and install it again: {install}",
        path.display()
    )
}

pub fn data_home() -> PathBuf {
    crate::platform::data_home()
}

#[cfg(target_os = "linux")]
mod linux {
    use std::ffi::{CString, c_char, c_int, c_void};
    use std::sync::OnceLock;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::runtime_dir;

    #[cfg(not(target_arch = "x86_64"))]
    compile_error!("the browser loader only has x86_64 Linux trampolines");

    unsafe extern "C" {
        fn dlopen(filename: *const c_char, flag: c_int) -> *mut c_void;
        fn dlsym(handle: *mut c_void, symbol: *const c_char) -> *mut c_void;
        fn dlerror() -> *const c_char;
    }

    const RTLD_NOW: c_int = 2;

    extern "C" fn unloaded() -> ! {
        eprintln!("arakun: a CEF function was called before libcef.so was loaded");
        std::process::abort();
    }

    macro_rules! trampolines {
        ($($name:ident),* $(,)?) => {
            #[allow(non_upper_case_globals)]
            mod slots {
                $(pub static $name: super::AtomicUsize = super::AtomicUsize::new(0);)*
            }
            $(
                core::arch::global_asm!(
                    concat!(".globl ", stringify!($name)),
                    concat!(".type ", stringify!($name), ",@function"),
                    concat!(stringify!($name), ":"),
                    "mov r11, qword ptr [rip + {slot}]",
                    "test r11, r11",
                    "jz {unloaded}",
                    "jmp r11",
                    concat!(".size ", stringify!($name), ", . - ", stringify!($name)),
                    slot = sym slots::$name,
                    unloaded = sym unloaded,
                );
            )*
            const SYMBOLS: &[(&str, &AtomicUsize)] = &[$((stringify!($name), &slots::$name)),*];
        };
    }

    trampolines!(
        cef_api_hash,
        cef_browser_host_create_browser_sync,
        cef_command_line_create,
        cef_do_message_loop_work,
        cef_execute_process,
        cef_initialize,
        cef_shutdown,
        cef_string_list_alloc,
        cef_string_list_append,
        cef_string_list_free,
        cef_string_list_size,
        cef_string_list_value,
        cef_string_userfree_utf16_free,
        cef_string_utf16_clear,
        cef_string_utf16_set,
        cef_string_utf16_to_utf8,
        cef_string_utf8_clear,
        cef_string_utf8_to_utf16,
    );

    /// Loads libcef.so once per process; later calls return the first result.
    pub fn load() -> Result<(), String> {
        static LOADED: OnceLock<Result<(), String>> = OnceLock::new();
        LOADED
            .get_or_init(|| {
                let path = runtime_dir().join("libcef.so");
                if !path.exists() {
                    return Err(super::missing(&path, "run tools/install-cef.sh"));
                }
                let c_path =
                    CString::new(path.as_os_str().as_encoded_bytes()).map_err(|e| e.to_string())?;
                let handle = unsafe { dlopen(c_path.as_ptr(), RTLD_NOW) };
                if handle.is_null() {
                    return Err(format!("dlopen {}: {}", path.display(), last_error()));
                }
                for (name, slot) in SYMBOLS {
                    let c_name = CString::new(*name).expect("symbol names have no NUL");
                    let addr = unsafe { dlsym(handle, c_name.as_ptr()) };
                    if addr.is_null() {
                        return Err(format!("{} has no {name}", path.display()));
                    }
                    slot.store(addr as usize, Ordering::Release);
                }
                Ok(())
            })
            .clone()
    }

    fn last_error() -> String {
        let err = unsafe { dlerror() };
        if err.is_null() {
            return "unknown error".into();
        }
        unsafe { std::ffi::CStr::from_ptr(err) }
            .to_string_lossy()
            .into_owned()
    }
}

/// The framework CEF loads on macOS, inside [`runtime_dir`].
#[cfg(target_os = "macos")]
pub fn framework_dir() -> PathBuf {
    runtime_dir().join("Chromium Embedded Framework.framework")
}

#[cfg(target_os = "macos")]
mod macos {
    use std::sync::OnceLock;

    use super::framework_dir;

    /// Loads the framework once per process; later calls return the first result.
    pub fn load() -> Result<(), String> {
        static LOADED: OnceLock<Result<(), String>> = OnceLock::new();
        LOADED
            .get_or_init(|| {
                let path = framework_dir().join("Chromium Embedded Framework");
                if !path.exists() {
                    return Err(super::missing(&path, "run tools/install-cef.sh"));
                }
                let c_path = std::ffi::CString::new(path.as_os_str().as_encoded_bytes())
                    .map_err(|e| e.to_string())?;
                // SAFETY: a NUL-terminated path that outlives the call.
                if unsafe { cef::load_library(Some(&*c_path.as_ptr().cast())) } != 1 {
                    return Err(format!("loading {} failed", path.display()));
                }
                Ok(())
            })
            .clone()
    }
}

#[cfg(windows)]
mod windows {
    use std::os::windows::ffi::OsStrExt as _;
    use std::sync::OnceLock;

    use windows_sys::Win32::System::LibraryLoader::{
        LOAD_WITH_ALTERED_SEARCH_PATH, LoadLibraryExW,
    };

    use super::runtime_dir;

    /// Loads libcef.dll once per process; later calls return the first result.
    pub fn load() -> Result<(), String> {
        static LOADED: OnceLock<Result<(), String>> = OnceLock::new();
        LOADED
            .get_or_init(|| {
                let path = runtime_dir().join("libcef.dll");
                if !path.exists() {
                    return Err(super::missing(
                        &path,
                        "`cargo build` copies it next to arakun.exe; \
                         or point ARAKUN_CEF_DIR at a folder that has it",
                    ));
                }
                let wide: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
                // The altered search path finds chrome_elf.dll and the other DLLs it
                // imports next to it, wherever that is.
                let handle = unsafe {
                    LoadLibraryExW(
                        wide.as_ptr(),
                        std::ptr::null_mut(),
                        LOAD_WITH_ALTERED_SEARCH_PATH,
                    )
                };
                if handle.is_null() {
                    return Err(format!(
                        "loading {}: {}",
                        path.display(),
                        std::io::Error::last_os_error()
                    ));
                }
                Ok(())
            })
            .clone()
    }
}
