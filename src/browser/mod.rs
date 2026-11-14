//! Browser tabs: Chromium (CEF 154) rendered off-screen into a GPUI view.
//!
//! - `handle_subprocess()` first thing in `main`: CEF re-executes this binary for its
//!   renderer/GPU/utility processes (`--type=...`); for the app itself it only scans argv.
//! - CEF is loaded (`dlopen`) and initialized when the first [`BrowserView`] is created,
//!   never before; all views share that one context and CDP endpoint ([`cdp_port`]).
//! - Without the `browser` cargo feature `BrowserView` is a placeholder with the same API.

// The demo and the app use different parts of the API, and the placeholder
// (no `browser` feature) little of the shared UI code.
#![allow(dead_code)]

mod chrome;
mod home;
mod text_field;
pub mod url;

#[cfg(all(
    feature = "browser",
    any(target_os = "linux", target_os = "macos", windows)
))]
mod client;
#[cfg(all(feature = "browser", target_os = "linux"))]
mod clipboard;
#[cfg(all(
    feature = "browser",
    any(target_os = "linux", target_os = "macos", windows)
))]
mod keys;
#[cfg(all(
    feature = "browser",
    any(target_os = "linux", target_os = "macos", windows)
))]
mod loader;
#[cfg(all(
    feature = "browser",
    any(target_os = "linux", target_os = "macos", windows)
))]
mod runtime;
#[cfg(all(
    feature = "browser",
    any(target_os = "linux", target_os = "macos", windows)
))]
mod view;

#[cfg(not(all(
    feature = "browser",
    any(target_os = "linux", target_os = "macos", windows)
)))]
#[path = "placeholder.rs"]
mod view;

use gpui::SharedString;

#[allow(unused_imports, reason = "the demo opens its tabs with an address")]
pub use home::{HomeInfo, Server};
pub use view::BrowserView;

#[derive(Clone, Debug)]
pub enum BrowserEvent {
    TitleChanged(SharedString),
    UrlChanged(SharedString),
    LoadingChanged(bool),
    /// `window.open`, `target=_blank`, middle/Ctrl+click, "Open link in new tab", Inspect.
    /// No native window is ever opened; the shell decides where the tab goes.
    OpenInNewTab(String),
    /// [`BrowserView::favicon`] changed.
    FaviconChanged,
    /// The page closed itself (`window.close()`); the view is now empty.
    CloseRequested,
}

/// A message the page logged (or an uncaught error), for `arakun page console`.
#[derive(Clone, Debug)]
pub struct ConsoleLine {
    /// Grows with every message a tab logs, across page loads.
    pub seq: u64,
    /// `error`, `warn`, `log` or `debug`.
    pub level: &'static str,
    pub text: String,
    /// `url:line` of the script, when Chromium knows it.
    pub source: String,
}

/// What `arakun page` needs to know before it talks to a page.
#[derive(Clone, Debug, Default)]
pub struct PageState {
    /// Chromium created the page (it may still be loading).
    pub open: bool,
    pub loading: bool,
    /// Loads the page finished so far; it grows once a navigation is done.
    pub loads: u64,
    pub error: Option<SharedString>,
    /// The message of the JavaScript dialog the page is waiting on.
    pub dialog: Option<String>,
}

/// Runs CEF's child processes (re-executions of this binary with `--type=...`) and exits;
/// returns immediately for the app process. Call it first thing in `main`.
pub fn handle_subprocess() {
    #[cfg(all(
        feature = "browser",
        any(target_os = "linux", target_os = "macos", windows)
    ))]
    if std::env::args_os()
        .skip(1)
        .any(|a| a.as_encoded_bytes().starts_with(b"--type="))
    {
        // arakun.exe is a console program: a child started by a arakun without a console
        // gets a console window of its own, which would stay open as long as the
        // renderer lives.
        crate::platform::detach_own_console();
        if let Err(e) = loader::load() {
            eprintln!("arakun: {e}");
            std::process::exit(1);
        }
        let _ = cef::api_hash(cef::sys::CEF_API_VERSION_LAST, 0);
        let args = cef::args::Args::new();
        let mut app = client::app(client::AppState {
            pump: None,
            cdp_port: 0,
        });
        let code = cef::execute_process(
            Some(args.as_main_args()),
            Some(&mut app),
            std::ptr::null_mut(),
        );
        std::process::exit(code.max(0));
    }
}

/// The Chrome DevTools Protocol port (bound to 127.0.0.1) once CEF is running: agents can
/// list tabs at `http://127.0.0.1:<port>/json/list`. `None` before the first tab.
pub fn cdp_port() -> Option<u16> {
    #[cfg(all(
        feature = "browser",
        any(target_os = "linux", target_os = "macos", windows)
    ))]
    return runtime::cdp_port();
    #[cfg(not(all(
        feature = "browser",
        any(target_os = "linux", target_os = "macos", windows)
    )))]
    None
}

/// Whether this build can show real pages.
pub const ENABLED: bool = cfg!(all(
    feature = "browser",
    any(target_os = "linux", target_os = "macos", windows)
));
