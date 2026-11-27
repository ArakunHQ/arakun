//! Browser tabs: Chromium (CEF 154) rendered off-screen into a GPUI view.
//!
//! - `handle_subprocess()` first thing in `main`: CEF re-executes this binary for its
//!   renderer/GPU/utility processes (`--type=...`); for the app itself it only scans argv.
//! - CEF is loaded (`dlopen`) and initialized when the first [`BrowserView`] is created,
//!   never before; all views share that one context. It opens no remote debugging port
//!   unless `ARAKUN_CDP_PORT` asks for one ([`cdp_port`]).
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
    /// `window.open`, `target=_blank`, middle/Ctrl+click, "Open link in new tab". Pages
    /// never open a native window; the shell decides where the tab goes.
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

/// The pages' main-world JavaScript contexts, kept from DevTools `Runtime` events once
/// `Runtime.enable` was sent. A navigation replaces a frame's context, so a script sent
/// to one of these ids can only run in the document that id was looked up for.
#[derive(Debug, Default)]
pub struct ScriptContexts {
    /// `(id, uniqueId, frame id)`, oldest first.
    live: Vec<(i64, String, String)>,
}

/// More are stale: a page that lives long and makes many frames.
const MAX_CONTEXTS: usize = 64;

impl ScriptContexts {
    /// Takes in a DevTools event; ignores everything but the `Runtime` context events.
    pub fn event(&mut self, method: &str, params: &serde_json::Value) {
        match method {
            "Runtime.executionContextCreated" => {
                let c = &params["context"];
                if c["auxData"]["isDefault"] != true {
                    return;
                }
                let (Some(id), Some(unique), Some(frame)) = (
                    c["id"].as_i64(),
                    c["uniqueId"].as_str(),
                    c["auxData"]["frameId"].as_str(),
                ) else {
                    return;
                };
                if self.live.len() >= MAX_CONTEXTS {
                    self.live.remove(0);
                }
                self.live.push((id, unique.to_string(), frame.to_string()));
            }
            // Ids repeat across renderer processes; the unique id does not.
            "Runtime.executionContextDestroyed" => {
                match params["executionContextUniqueId"].as_str() {
                    Some(unique) => self.live.retain(|(_, u, _)| u != unique),
                    None => {
                        let id = params["executionContextId"].as_i64();
                        self.live.retain(|(i, _, _)| Some(*i) != id);
                    }
                }
            }
            "Runtime.executionContextsCleared" => self.live.clear(),
            _ => {}
        }
    }

    /// The unique id of the newest main-world context of a frame.
    pub fn main_world(&self, frame: &str) -> Option<String> {
        self.live
            .iter()
            .rev()
            .find(|(_, _, f)| f == frame)
            .map(|(_, unique, _)| unique.clone())
    }
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
        let mut app = client::app(client::AppState { pump: None });
        let code = cef::execute_process(
            Some(args.as_main_args()),
            Some(&mut app),
            std::ptr::null_mut(),
        );
        std::process::exit(code.max(0));
    }
}

/// The Chrome DevTools Protocol port (bound to 127.0.0.1) once CEF is running. `None`
/// unless `ARAKUN_CDP_PORT` turned it on, and before the first tab. It has no token: any
/// local process, a sandboxed agent's included, can drive the browser through it.
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

/// The port `ARAKUN_CDP_PORT` asks for; 0, or anything not a port, leaves it off.
#[cfg_attr(
    not(all(
        feature = "browser",
        any(target_os = "linux", target_os = "macos", windows)
    )),
    allow(dead_code)
)]
fn cdp_port_from(value: Option<&str>) -> u16 {
    value.and_then(|v| v.trim().parse().ok()).unwrap_or(0)
}

/// Whether `url` points at the browser's own debugging port. Agents may not open or drive
/// such a page: a script there could talk to the port and get past every check on pages.
/// Any host counts, since `127.1`, `0.0.0.0` and the like all reach it.
pub fn is_debug_endpoint(url: &str) -> bool {
    cdp_port().is_some_and(|port| url_port(url) == Some(port))
}

/// The port `url` connects to, given or implied by its scheme. Chromium drops tabs and
/// line breaks from a URL before parsing it, so they are dropped here too.
fn url_port(url: &str) -> Option<u16> {
    let url: String = url
        .trim()
        .chars()
        .filter(|c| !matches!(c, '\t' | '\n' | '\r'))
        .collect();
    let (scheme, rest) = url.split_once(':')?;
    let rest = rest.trim_start_matches(['/', '\\']);
    let authority = rest.split(['/', '\\', '?', '#']).next()?;
    let host_port = authority.rsplit_once('@').map_or(authority, |(_, h)| h);
    let port = match host_port.rsplit_once(']') {
        Some((_, after)) => after.strip_prefix(':'),
        None => host_port.rsplit_once(':').map(|(_, p)| p),
    };
    match port.filter(|p| !p.is_empty()) {
        Some(p) => p.parse().ok(),
        None => match scheme.to_ascii_lowercase().as_str() {
            "http" | "ws" => Some(80),
            "https" | "wss" => Some(443),
            _ => None,
        },
    }
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
    match crate::settings::read(|s| s.general.open_links) {
        Some(crate::settings::OpenLinks::Arakun) => true,
        Some(crate::settings::OpenLinks::System) => false,
        None => ENABLED,
    }
}

pub fn set_links_in_arakun(on: bool) {
    let choice = if on {
        crate::settings::OpenLinks::Arakun
    } else {
        crate::settings::OpenLinks::System
    };
    crate::settings::change(|s| s.general.open_links = Some(choice));
}

#[cfg(test)]
mod tests {
    use std::ffi::OsString;

    use serde_json::json;

    use super::{SANDBOX_PROBE, ScriptContexts, cdp_port_from, is_sandbox_probe, url_port};

    #[test]
    fn the_debugging_port_is_off_unless_asked_for() {
        assert_eq!(cdp_port_from(None), 0);
        assert_eq!(cdp_port_from(Some("")), 0);
        assert_eq!(cdp_port_from(Some("0")), 0);
        assert_eq!(cdp_port_from(Some("yes")), 0);
        assert_eq!(cdp_port_from(Some("70000")), 0);
        assert_eq!(cdp_port_from(Some(" 9222\n")), 9222);
    }

    #[test]
    fn finds_the_port_a_url_reaches() {
        for (url, port) in [
            ("http://127.0.0.1:9339/json/version", Some(9339)),
            ("http://127.1:9339", Some(9339)),
            ("http://0.0.0.0:9339/", Some(9339)),
            ("http://[::1]:9339/json", Some(9339)),
            ("http://user:pw@localhost:9339/", Some(9339)),
            ("http://localhost:09339", Some(9339)),
            ("HTTP://localhost:9339", Some(9339)),
            ("http://127.0.0.1:93\t39/", Some(9339)),
            ("http:\\\\127.0.0.1:9339\\json", Some(9339)),
            ("ws://127.0.0.1:9339/devtools/browser/x", Some(9339)),
            ("http://localhost/", Some(80)),
            ("https://example.com?q=:9339", Some(443)),
            ("http://[::1]/", Some(80)),
            ("file:///etc/passwd", None),
            ("about:blank", None),
            ("http://localhost:port/", None),
        ] {
            assert_eq!(url_port(url), port, "{url}");
        }
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

    #[test]
    fn a_navigation_replaces_the_script_context() {
        let created = |id: i64, unique: &str, frame: &str, default: bool| {
            json!({ "context": { "id": id, "uniqueId": unique,
                "auxData": { "isDefault": default, "frameId": frame } } })
        };
        let mut c = ScriptContexts::default();
        c.event(
            "Runtime.executionContextCreated",
            &created(1, "a", "main", true),
        );
        c.event(
            "Runtime.executionContextCreated",
            &created(2, "b", "main", false),
        );
        c.event(
            "Runtime.executionContextCreated",
            &created(3, "c", "child", true),
        );
        assert_eq!(c.main_world("main").as_deref(), Some("a"));
        // The next document, maybe in another process that reuses the id.
        c.event(
            "Runtime.executionContextCreated",
            &created(1, "d", "main", true),
        );
        assert_eq!(c.main_world("main").as_deref(), Some("d"));
        c.event(
            "Runtime.executionContextDestroyed",
            &json!({ "executionContextId": 1, "executionContextUniqueId": "d" }),
        );
        assert_eq!(c.main_world("main").as_deref(), Some("a"));
        c.event(
            "Runtime.executionContextDestroyed",
            &json!({ "executionContextId": 1 }),
        );
        assert_eq!(c.main_world("main"), None);
        assert_eq!(c.main_world("child").as_deref(), Some("c"));
        c.event("Runtime.executionContextsCleared", &json!({}));
        assert_eq!(c.main_world("child"), None);
    }
}
