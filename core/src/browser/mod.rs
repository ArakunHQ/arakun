//! Browser tabs: Chromium (CEF 154) rendered off-screen into a GPUI view.
//!
//! - `handle_subprocess()` first thing in `main`: CEF re-executes this binary for its
//!   renderer/GPU/utility processes (`--type=...`); for the app itself it only scans argv.
//! - CEF is loaded (`dlopen`) and initialized when the first [`BrowserView`] is created,
//!   never before; all views share that one context and CDP endpoint ([`cdp_port`]).
//! - Without the `browser` cargo feature `BrowserView` is a placeholder with the same API.

// Without CEF the placeholder draws in place of the view, and these are only the view's.
#[cfg_attr(
    not(all(
        feature = "browser",
        any(target_os = "linux", target_os = "macos", windows)
    )),
    allow(dead_code)
)]
mod chrome;
pub mod favicons;
#[cfg_attr(
    not(all(
        feature = "browser",
        any(target_os = "linux", target_os = "macos", windows)
    )),
    allow(dead_code)
)]
mod home;
#[cfg_attr(
    not(all(
        feature = "browser",
        any(target_os = "linux", target_os = "macos", windows)
    )),
    allow(dead_code)
)]
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
    /// Only the placeholder sends it: show the setting for where links open.
    OpenLinkSettings,
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

/// The argument the Linux sandbox check (`runtime::userns_sandbox_works`) runs this
/// binary with, in a child process, to see whether it starts in a nested user namespace.
pub const SANDBOX_PROBE: &str = "--arakun-sandbox-probe";

/// Whether this process is that check's child: it only has to start.
fn is_sandbox_probe(mut args: impl Iterator<Item = std::ffi::OsString>) -> bool {
    args.nth(1).is_some_and(|a| a == SANDBOX_PROBE)
}

/// Runs CEF's child processes (re-executions of this binary with `--type=...`) and exits;
/// returns immediately for the app process. Call it first thing in `main`.
pub fn handle_subprocess() {
    // Before anything else: the browser demo would take the argument for a page to open.
    if is_sandbox_probe(std::env::args_os()) {
        std::process::exit(0);
    }
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

/// Whether web links (from the chat, the GitHub view, pins…) open in an Arakun
/// browser tab rather than the desktop's browser. Set in Settings → General.
/// Without a choice, builds that can show pages keep links in Arakun, and the
/// others send them out, where they can actually load.
pub fn links_in_arakun() -> bool {
    match LINKS_CHOICE.load(std::sync::atomic::Ordering::Relaxed) {
        LINKS_ARAKUN => return true,
        LINKS_SYSTEM => return false,
        _ => {}
    }
    match std::fs::read_to_string(links_path()) {
        Ok(choice) => parse_links_choice(&choice).unwrap_or(ENABLED),
        Err(_) => ENABLED,
    }
}

pub fn set_links_in_arakun(on: bool) {
    // Kept in memory too, so the switch works for this run even when the file
    // cannot be written.
    let choice = if on { LINKS_ARAKUN } else { LINKS_SYSTEM };
    LINKS_CHOICE.store(choice, std::sync::atomic::Ordering::Relaxed);
    crate::durable::write_soon(
        links_path(),
        if on { "arakun\n" } else { "system\n" }.into(),
        "where links open will not last past a restart",
    );
}

/// The choice made in this run: [`LINKS_UNSET`] until the switch is used.
static LINKS_CHOICE: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(LINKS_UNSET);
const LINKS_UNSET: u8 = 0;
const LINKS_ARAKUN: u8 = 1;
const LINKS_SYSTEM: u8 = 2;

fn links_path() -> std::path::PathBuf {
    crate::platform::app_dir().join("open-links")
}

fn parse_links_choice(text: &str) -> Option<bool> {
    match text.trim() {
        "arakun" => Some(true),
        "system" => Some(false),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use std::ffi::OsString;

    use super::{SANDBOX_PROBE, is_sandbox_probe, parse_links_choice};

    #[test]
    fn reads_the_links_choice() {
        assert_eq!(parse_links_choice("arakun\n"), Some(true));
        assert_eq!(parse_links_choice("system"), Some(false));
        // Anything else falls back to what the build can do.
        assert_eq!(parse_links_choice(""), None);
        assert_eq!(parse_links_choice("chrome"), None);
    }

    #[test]
    fn only_the_probe_argument_makes_a_probe() {
        let args = |a: &[&str]| a.iter().map(OsString::from).collect::<Vec<_>>().into_iter();
        assert!(is_sandbox_probe(args(&["arakun", SANDBOX_PROBE])));
        assert!(!is_sandbox_probe(args(&["arakun"])));
        assert!(!is_sandbox_probe(args(&["arakun", "--version"])));
        assert!(!is_sandbox_probe(args(&[
            "arakun",
            "https://example.com",
            SANDBOX_PROBE
        ])));
    }
}
