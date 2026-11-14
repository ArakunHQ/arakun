//! The process-wide CEF context: started on the first browser tab, shared by every tab.
//!
//! CEF runs with `external_message_pump`: it tells us through
//! `on_schedule_message_pump_work` (from any thread) when it wants
//! `do_message_loop_work` called, and a GPUI foreground task sleeps until then (see
//! [`idle_tick`] for the polling floor). With no tabs nothing wakes up.

use std::cell::RefCell;
use std::path::PathBuf;
use std::pin::pin;
use std::rc::{Rc, Weak};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU16, Ordering};
use std::time::{Duration, Instant};

use cef::ImplBrowserHost as _;
use futures::StreamExt as _;
use futures::channel::mpsc;
use gpui::{App, AsyncApp, WeakEntity};

use super::client::{self, AppState, TabState};
use super::loader;
use super::view::BrowserView;

pub static CONTEXT_READY: AtomicBool = AtomicBool::new(false);
static CDP_PORT: AtomicU16 = AtomicU16::new(0);

/// Preferred CDP port; a free one is picked when it is taken. `ARAKUN_CDP_PORT` overrides
/// it (0 disables remote debugging). The port in use is also written by Chromium to
/// `<profile>/DevToolsActivePort`.
const DEFAULT_CDP_PORT: u16 = 9339;

/// CEF never asks for more than this delay to be honoured exactly; longer requests are
/// clamped so a lost wake-up costs at most this much latency.
const MAX_DELAY: Duration = Duration::from_secs(1);

pub struct PumpSignal {
    tx: mpsc::UnboundedSender<i64>,
}

impl PumpSignal {
    pub fn schedule(&self, delay_ms: i64) {
        if debug() {
            eprintln!("arakun-browser: schedule {delay_ms} ms");
        }
        let _ = self.tx.unbounded_send(delay_ms);
    }
}

enum State {
    Uninit,
    Running(Arc<PumpSignal>),
    Failed(String),
}

struct Registered {
    tab: Weak<TabState>,
    view: WeakEntity<BrowserView>,
}

thread_local! {
    static STATE: RefCell<State> = const { RefCell::new(State::Uninit) };
    static VIEWS: RefCell<Vec<Registered>> = const { RefCell::new(Vec::new()) };
}

pub fn cdp_port() -> Option<u16> {
    match CDP_PORT.load(Ordering::Relaxed) {
        0 => None,
        p => Some(p),
    }
}

pub fn profile_dir() -> PathBuf {
    match std::env::var_os("ARAKUN_BROWSER_PROFILE") {
        Some(dir) if !dir.is_empty() => dir.into(),
        _ => loader::data_home().join("arakun").join("browser"),
    }
}

/// Who else holds `profile`, when another live process does: starting CEF on a profile
/// in use blocks `cef::initialize` for 20 s before it gives up. On Linux Chromium's
/// `SingletonLock` symlink reads `<hostname>-<pid>`.
#[cfg(target_os = "linux")]
fn profile_owner(profile: &std::path::Path) -> Option<String> {
    let target = std::fs::read_link(profile.join("SingletonLock")).ok()?;
    let (host, pid) = target.to_str()?.rsplit_once('-')?;
    let pid: u32 = pid.parse().ok()?;
    let ours = std::fs::read_to_string("/proc/sys/kernel/hostname").unwrap_or_default();
    let alive = std::path::Path::new(&format!("/proc/{pid}")).exists();
    (host == ours.trim() && alive && pid != std::process::id()).then(|| format!("pid {pid}"))
}

/// macOS has the same `SingletonLock` but no /proc; the hostname is not checked because
/// the profile lives in the home folder, which is not shared between machines there.
#[cfg(target_os = "macos")]
fn profile_owner(profile: &std::path::Path) -> Option<String> {
    let target = std::fs::read_link(profile.join("SingletonLock")).ok()?;
    let (_, pid) = target.to_str()?.rsplit_once('-')?;
    let pid: u32 = pid.parse().ok()?;
    (crate::platform::process_alive(pid) && pid != std::process::id()).then(|| format!("pid {pid}"))
}

/// On Windows Chromium keeps `<profile>\lockfile` open without delete sharing while it
/// runs (and deletes it on close), so a lockfile that cannot be removed is in use.
#[cfg(windows)]
fn profile_owner(profile: &std::path::Path) -> Option<String> {
    let lock = profile.join("lockfile");
    (lock.exists() && std::fs::remove_file(&lock).is_err()).then(|| "another process".to_string())
}

/// Set when this process runs on a throwaway profile; removed at shutdown.
static EPHEMERAL_PROFILE: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();

fn ephemeral_profile() -> PathBuf {
    let base = std::env::var_os("XDG_RUNTIME_DIR").map_or_else(std::env::temp_dir, PathBuf::from);
    // Leftovers of instances that were killed before their shutdown ran.
    for entry in std::fs::read_dir(&base).into_iter().flatten().flatten() {
        let name = entry.file_name();
        let pid = name.to_str().and_then(|n| n.strip_prefix("arakun-browser-")).and_then(|p| p.parse::<u32>().ok());
        if pid.is_some_and(|pid| !crate::platform::process_alive(pid)) {
            let _ = std::fs::remove_dir_all(entry.path());
        }
    }
    base.join(format!("arakun-browser-{}", std::process::id()))
}

/// How long the pump may sleep with nothing scheduled. CEF does not announce every
/// delayed task through `on_schedule_message_pump_work` (cefclient polls at 30 Hz for
/// the same reason), so a visible page is polled at `ARAKUN_BROWSER_IDLE_MS` (33 ms),
/// hidden pages once a second, and with no pages at all the pump only wakes on request.
fn idle_tick() -> Option<Duration> {
    static TICK: std::sync::OnceLock<Duration> = std::sync::OnceLock::new();
    let visible = *TICK.get_or_init(|| {
        let ms = std::env::var("ARAKUN_BROWSER_IDLE_MS").ok().and_then(|v| v.parse().ok()).unwrap_or(33);
        Duration::from_millis(ms)
    });
    let ready = CONTEXT_READY.load(Ordering::Relaxed);
    VIEWS.with(|v| {
        let mut tick = None;
        for tab in v.borrow().iter().filter_map(|r| r.tab.upgrade()) {
            let live = tab.browser.borrow().is_some();
            if tab.visible.get() && (live || !ready) {
                return Some(visible);
            }
            if live {
                tick = Some(MAX_DELAY);
            }
        }
        tick
    })
}

fn debug() -> bool {
    static DEBUG: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *DEBUG.get_or_init(|| std::env::var_os("ARAKUN_BROWSER_DEBUG").is_some())
}

pub fn timing() -> bool {
    std::env::var_os("ARAKUN_TIMING").is_some()
}

/// Starts CEF if it is not running yet. Must run on the main thread.
pub fn ensure(cx: &mut App) -> Result<(), String> {
    let state = STATE.with(|s| match &*s.borrow() {
        State::Running(_) => Some(Ok(())),
        State::Failed(e) => Some(Err(e.clone())),
        State::Uninit => None,
    });
    if let Some(result) = state {
        return result;
    }
    let result = start(cx);
    STATE.with(|s| {
        *s.borrow_mut() = match &result {
            Ok(signal) => State::Running(signal.clone()),
            Err(e) => State::Failed(e.clone()),
        }
    });
    result.map(|_| ())
}

/// Asks the pump to run as soon as possible.
pub fn kick() {
    STATE.with(|s| {
        if let State::Running(signal) = &*s.borrow() {
            signal.schedule(0);
        }
    });
}

pub fn register(tab: &Rc<TabState>, view: WeakEntity<BrowserView>) {
    VIEWS.with(|v| v.borrow_mut().push(Registered { tab: Rc::downgrade(tab), view }));
    kick();
}

fn pick_cdp_port() -> u16 {
    if let Some(p) = std::env::var("ARAKUN_CDP_PORT").ok().and_then(|v| v.parse::<u16>().ok()) {
        return p;
    }
    let bind = |port| std::net::TcpListener::bind(("127.0.0.1", port));
    if bind(DEFAULT_CDP_PORT).is_ok() {
        return DEFAULT_CDP_PORT;
    }
    bind(0).ok().and_then(|l| l.local_addr().ok()).map_or(0, |a| a.port())
}

/// Windows: `arakun-helper.exe` next to arakun.exe (src/bin/arakun-helper.rs), so CEF's
/// child processes open no console window. Without it CEF re-runs arakun.exe itself.
#[cfg(windows)]
fn subprocess_path() -> Option<PathBuf> {
    let helper = std::env::current_exe().ok()?.with_file_name("arakun-helper.exe");
    helper.exists().then_some(helper)
}
/// macOS: CEF would look for helper apps inside an app bundle; arakun has none, so its
/// children are re-runs of this binary (see `handle_subprocess`).
#[cfg(target_os = "macos")]
fn subprocess_path() -> Option<PathBuf> {
    std::env::current_exe().ok()
}
#[cfg(target_os = "linux")]
fn subprocess_path() -> Option<PathBuf> {
    None
}

/// Chromium's message pump asks `NSApp` whether it is inside `sendEvent:` through
/// `CrAppProtocol`, which GPUI's application class does not implement; without it CEF
/// aborts. The flag is never set because GPUI, not Chromium, dispatches the events.
#[cfg(target_os = "macos")]
fn adopt_ns_app() {
    use std::ffi::{c_char, c_void};

    #[link(name = "objc")]
    unsafe extern "C" {
        fn objc_getClass(name: *const c_char) -> *mut c_void;
        fn object_getClass(obj: *mut c_void) -> *mut c_void;
        fn sel_registerName(name: *const c_char) -> *mut c_void;
        fn class_addMethod(cls: *mut c_void, sel: *mut c_void, imp: *const c_void, types: *const c_char) -> bool;
        fn objc_getProtocol(name: *const c_char) -> *mut c_void;
        fn class_addProtocol(cls: *mut c_void, protocol: *mut c_void) -> bool;
        fn objc_msgSend();
    }
    static HANDLING: AtomicBool = AtomicBool::new(false);
    extern "C" fn is_handling(_: *mut c_void, _: *mut c_void) -> bool {
        HANDLING.load(Ordering::Relaxed)
    }
    extern "C" fn set_handling(_: *mut c_void, _: *mut c_void, value: bool) {
        HANDLING.store(value, Ordering::Relaxed);
    }
    // SAFETY: Objective-C runtime calls with NUL-terminated names; the methods added match
    // the type encodings given.
    unsafe {
        let ns_app_class = objc_getClass(c"NSApplication".as_ptr());
        let shared: extern "C" fn(*mut c_void, *mut c_void) -> *mut c_void =
            std::mem::transmute(objc_msgSend as *const c_void);
        let app = shared(ns_app_class, sel_registerName(c"sharedApplication".as_ptr()));
        if app.is_null() {
            return;
        }
        let class = object_getClass(app);
        class_addMethod(class, sel_registerName(c"isHandlingSendEvent".as_ptr()), is_handling as *const c_void, c"B@:".as_ptr());
        class_addMethod(class, sel_registerName(c"setHandlingSendEvent:".as_ptr()), set_handling as *const c_void, c"v@:B".as_ptr());
        for name in [c"CrAppProtocol", c"CrAppControlProtocol", c"CefAppProtocol"] {
            let protocol = objc_getProtocol(name.as_ptr());
            if !protocol.is_null() {
                class_addProtocol(class, protocol);
            }
        }
    }
}

fn start(cx: &mut App) -> Result<Arc<PumpSignal>, String> {
    let started = Instant::now();
    loader::load()?;
    let _ = cef::api_hash(cef::sys::CEF_API_VERSION_LAST, 0);
    #[cfg(target_os = "macos")]
    adopt_ns_app();

    let (tx, rx) = mpsc::unbounded();
    let signal = Arc::new(PumpSignal { tx });
    let port = pick_cdp_port();
    let mut app = client::app(AppState { pump: Some(signal.clone()), cdp_port: port });

    let runtime = loader::runtime_dir();
    let mut profile = profile_dir();
    if let Some(owner) = profile_owner(&profile) {
        let fallback = ephemeral_profile();
        eprintln!(
            "arakun-browser: {} is in use by {owner}; this window gets a temporary profile at {}",
            profile.display(),
            fallback.display()
        );
        let _ = EPHEMERAL_PROFILE.set(fallback.clone());
        profile = fallback;
    }
    std::fs::create_dir_all(&profile).map_err(|e| format!("{}: {e}", profile.display()))?;
    // CEF compares the resolved root_cache_path with cache_path as given: under a link
    // (macOS' /tmp is one to /private/tmp) they differ and the profile is dropped for memory.
    let profile = crate::platform::canonicalize(&profile).unwrap_or(profile);
    let spanish = std::env::var("LANG").is_ok_and(|l| l.starts_with("es"));
    let path = |p: PathBuf| cef::CefString::from(p.to_string_lossy().as_ref());
    // macOS reads the resources and locales from inside the framework.
    #[cfg(target_os = "macos")]
    let (resources, locales) = {
        let _ = runtime;
        (cef::CefString::default(), cef::CefString::default())
    };
    #[cfg(not(target_os = "macos"))]
    let (resources, locales) = (path(runtime.clone()), path(runtime.join("locales")));
    let settings = cef::Settings {
        #[cfg(target_os = "macos")]
        framework_dir_path: path(loader::framework_dir()),
        windowless_rendering_enabled: 1,
        external_message_pump: 1,
        // Linux: renderers run in Chromium's user-namespace sandbox, so a page that exploits
        // a renderer bug stays boxed in. Windows and macOS still lack the sandbox bootstrap
        // CEF needs (on macOS, cef_sandbox linked into the child and its seatbelt context
        // set up before the framework loads): without it every child dies on start, and
        // after the GPU process fails six times Chromium takes the whole app down.
        no_sandbox: cfg!(any(windows, target_os = "macos")) as i32,
        remote_debugging_port: port as i32,
        root_cache_path: path(profile.clone()),
        cache_path: path(profile.join("Default")),
        persist_session_cookies: 1,
        resources_dir_path: resources,
        locales_dir_path: locales,
        locale: cef::CefString::from(if spanish { "es" } else { "en-US" }),
        accept_language_list: cef::CefString::from(if spanish { "es-ES,es,en-US,en" } else { "en-US,en" }),
        log_file: path(profile.join("cef.log")),
        log_severity: cef::LogSeverity::WARNING,
        background_color: 0xFFFF_FFFF,
        browser_subprocess_path: subprocess_path().map(path).unwrap_or_default(),
        ..Default::default()
    };
    // CEF keeps pointers into argv, so the arguments live for the whole process.
    let args: &'static cef::args::Args = Box::leak(Box::new(cef::args::Args::new()));
    if cef::initialize(Some(args.as_main_args()), Some(&settings), Some(&mut app), std::ptr::null_mut()) != 1 {
        return Err(format!(
            "CEF failed to start (is another arakun using {}? see cef.log there)",
            profile.display()
        ));
    }
    CDP_PORT.store(port, Ordering::Relaxed);
    if timing() {
        eprintln!("arakun-browser: cef::initialize took {:?} (CDP port {port})", started.elapsed());
    }

    cx.spawn(async move |cx: &mut AsyncApp| pump(rx, cx).await).detach();
    cx.on_app_quit(|_| {
        shutdown();
        async {}
    })
    .detach();
    Ok(signal)
}

async fn pump(mut rx: mpsc::UnboundedReceiver<i64>, cx: &mut AsyncApp) {
    if debug() {
        eprintln!("arakun-browser: pump started");
    }
    let mut deadline: Option<Instant> = Some(Instant::now());
    let merge = |deadline: &mut Option<Instant>, delay_ms: i64| {
        let delay = Duration::from_millis(delay_ms.max(0) as u64).min(MAX_DELAY);
        let at = Instant::now() + delay;
        *deadline = Some(deadline.map_or(at, |d| d.min(at)));
    };
    loop {
        while let Ok(delay) = rx.try_recv() {
            merge(&mut deadline, delay);
        }
        let now = Instant::now();
        match deadline {
            Some(at) if at <= now => {
                deadline = None;
                if debug() {
                    eprintln!("arakun-browser: pump work");
                }
                cef::do_message_loop_work();
                after_work(cx);
                while let Ok(delay) = rx.try_recv() {
                    merge(&mut deadline, delay);
                }
                if deadline.is_none() {
                    deadline = idle_tick().map(|tick| Instant::now() + tick);
                }
            }
            Some(at) => {
                let timer = cx.background_executor().timer(at - now);
                match futures::future::select(rx.next(), pin!(timer)).await {
                    futures::future::Either::Left((Some(delay), _)) => merge(&mut deadline, delay),
                    futures::future::Either::Left((None, _)) => return,
                    futures::future::Either::Right(_) => {}
                }
            }
            None => match rx.next().await {
                Some(delay) => merge(&mut deadline, delay),
                None => return,
            },
        }
    }
}

/// Hands whatever the callbacks recorded to the views that own those tabs.
fn after_work(cx: &mut AsyncApp) {
    let ready = CONTEXT_READY.load(Ordering::Relaxed);
    let dirty: Vec<WeakEntity<BrowserView>> = VIEWS.with(|v| {
        let mut v = v.borrow_mut();
        v.retain(|r| r.tab.strong_count() > 0);
        v.iter()
            .filter(|r| {
                r.tab.upgrade().is_some_and(|t| t.dirty.get() || (ready && t.browser.borrow().is_none()))
            })
            .map(|r| r.view.clone())
            .collect()
    });
    for view in dirty {
        let _ = view.update(cx, |view, cx| view.sync(cx));
    }
}

/// Closes every browser and shuts CEF down so cookies and the cache are flushed.
fn shutdown() {
    let tabs: Vec<Rc<TabState>> =
        VIEWS.with(|v| v.borrow().iter().filter_map(|r| r.tab.upgrade()).collect());
    for tab in &tabs {
        if let Some(host) = tab.host() {
            host.close_browser(1);
        }
    }
    let until = Instant::now() + Duration::from_millis(500);
    while Instant::now() < until && tabs.iter().any(|t| !closed(t)) {
        cef::do_message_loop_work();
        std::thread::sleep(Duration::from_millis(5));
    }
    for tab in &tabs {
        tab.browser.borrow_mut().take();
    }
    cef::shutdown();
    if let Some(dir) = EPHEMERAL_PROFILE.get() {
        let _ = std::fs::remove_dir_all(dir);
    }
}

fn closed(tab: &TabState) -> bool {
    tab.updates.borrow().iter().any(|u| matches!(u, client::Update::Closed)) || tab.browser.borrow().is_none()
}
