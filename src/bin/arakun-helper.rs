//! Windows: the program CEF starts for its renderer, GPU and utility processes
//! (`browser_subprocess_path`, set in src/browser/runtime.rs). arakun.exe is a console
//! program, so every child CEF started from it got a console window of its own that
//! flashed on screen; this one is a GUI program and gets none. It does what
//! `browser::handle_subprocess` does: the app's CEF handlers only act in the browser process.
#![cfg_attr(windows, windows_subsystem = "windows")]

#[cfg(all(windows, feature = "browser"))]
fn main() {
    // libcef.dll is delay-loaded and normally found next to this exe; with ARAKUN_CEF_DIR
    // (inherited from arakun) it lives elsewhere, so load it from there first.
    if let Some(dir) = std::env::var_os("ARAKUN_CEF_DIR") {
        use std::os::windows::ffi::OsStrExt as _;
        use windows_sys::Win32::System::LibraryLoader::{LOAD_WITH_ALTERED_SEARCH_PATH, LoadLibraryExW};
        let path = std::path::Path::new(&dir).join("libcef.dll");
        let wide: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
        // SAFETY: `wide` is a NUL-terminated path.
        unsafe { LoadLibraryExW(wide.as_ptr(), std::ptr::null_mut(), LOAD_WITH_ALTERED_SEARCH_PATH) };
    }
    let _ = cef::api_hash(cef::sys::CEF_API_VERSION_LAST, 0);
    let args = cef::args::Args::new();
    let code = cef::execute_process(Some(args.as_main_args()), None, std::ptr::null_mut());
    std::process::exit(code.max(0));
}

#[cfg(not(all(windows, feature = "browser")))]
fn main() {}
