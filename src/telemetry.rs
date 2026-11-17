//! Crash reports and anonymous usage counts, sent to Arakun's PostHog project
//! only after the user says yes.
//!
//! Nothing is sent until the user answers the question Arakun asks on first
//! launch (`telemetry_dialog`), and only what they agreed to: crash reports
//! (panics with their stack trace, agent errors) and usage counts are two
//! choices, kept in `app_dir/telemetry.json` and changed in Settings › General.
//! `DO_NOT_TRACK=1` or `ARAKUN_TELEMETRY=0` turn both off and skip the
//! question; `ARAKUN_TELEMETRY=1` turns both on.
//!
//! Never a prompt, a reply, a file's contents, a URL or a project's name.
//! Error messages lose URLs, paths, emails and anything shaped like a secret
//! before they leave the machine (`scrub`). Every event carries a random id
//! kept in `app_dir/telemetry-id`, not tied to any account, and creates no
//! person profile. Debug builds, tests and `arakun dev` send nothing.
//!
//! Events go to PostHog's capture API over the `ureq` client the app already
//! has, from one background thread; a panic is sent right away from the
//! panicking thread, since the process may not outlive it.

use std::cell::Cell;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Condvar, Mutex, MutexGuard, OnceLock};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

/// The project token: write-only, so it is safe in the binary. A build can
/// point at another project with `ARAKUN_POSTHOG_KEY` (empty turns it off).
const KEY: &str = match option_env!("ARAKUN_POSTHOG_KEY") {
    Some(key) => key,
    None => "phc_kXKrLfaWumKa3mfXcFVBCRpyKa6DZiUv7hCkQW2epLQZ",
};
const HOST: &str = match option_env!("ARAKUN_POSTHOG_HOST") {
    Some(host) => host,
    None => "https://us.i.posthog.com",
};

/// Usage events wait this long to go out together.
const BATCH_EVERY: Duration = Duration::from_secs(5);
/// Past this many queued events the oldest are dropped (PostHog unreachable).
const MAX_QUEUE: usize = 500;
/// The frames nearest the panic that are kept.
const MAX_FRAMES: usize = 64;
/// How much of an error message is sent, after scrubbing.
const MAX_MESSAGE: usize = 300;

/// What the user agreed to send.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Consent {
    #[serde(default)]
    pub crashes: bool,
    #[serde(default)]
    pub usage: bool,
}

impl Consent {
    pub const ALL: Consent = Consent {
        crashes: true,
        usage: true,
    };
    pub const NONE: Consent = Consent {
        crashes: false,
        usage: false,
    };
}

/// What is sent right now; read by every capture and by the panic hook, so no
/// lock is taken there.
static CRASHES: AtomicBool = AtomicBool::new(false);
static USAGE: AtomicBool = AtomicBool::new(false);
static STARTED: OnceLock<()> = OnceLock::new();

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// What the environment says, which wins over the saved answer.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum EnvChoice {
    Off,
    On,
    Unset,
}

fn env_choice(do_not_track: Option<&str>, arakun: Option<&str>) -> EnvChoice {
    let norm = |v: &str| v.trim().to_ascii_lowercase();
    // https://consoledonottrack.com: any value but empty or 0 opts out.
    if do_not_track
        .map(norm)
        .is_some_and(|v| !v.is_empty() && v != "0")
    {
        return EnvChoice::Off;
    }
    match arakun.map(norm).as_deref() {
        Some("0" | "off" | "false" | "no") => EnvChoice::Off,
        Some("1" | "on" | "true" | "yes") => EnvChoice::On,
        _ => EnvChoice::Unset,
    }
}

fn env() -> EnvChoice {
    static ENV: OnceLock<EnvChoice> = OnceLock::new();
    *ENV.get_or_init(|| {
        env_choice(
            std::env::var("DO_NOT_TRACK").ok().as_deref(),
            std::env::var("ARAKUN_TELEMETRY").ok().as_deref(),
        )
    })
}

/// Whether this process may never send anything, whatever the user chose.
fn blocked() -> bool {
    cfg!(test)
        || KEY.is_empty()
        || crate::dev::size().is_some()
        || env() == EnvChoice::Off
        // Developers' own runs would swamp real use.
        || (cfg!(debug_assertions) && env() != EnvChoice::On)
}

fn consent_file() -> PathBuf {
    crate::platform::app_dir().join("telemetry.json")
}

fn saved() -> &'static Mutex<Option<Option<Consent>>> {
    static SAVED: OnceLock<Mutex<Option<Option<Consent>>>> = OnceLock::new();
    SAVED.get_or_init(|| Mutex::new(None))
}

/// The user's answer, or None while they have not given one.
pub fn consent() -> Option<Consent> {
    *lock(saved()).get_or_insert_with(|| {
        std::fs::read(consent_file())
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
    })
}

/// Whether to ask on this launch: never asked, and nothing else decides it.
pub fn should_ask() -> bool {
    !blocked() && env() == EnvChoice::Unset && consent().is_none()
}

/// What this process sends.
fn effective() -> Consent {
    if blocked() {
        Consent::NONE
    } else if env() == EnvChoice::On {
        Consent::ALL
    } else {
        consent().unwrap_or(Consent::NONE)
    }
}

/// Starts sending what the user agreed to, if anything.
pub fn init() {
    apply(effective());
}

/// Saves the user's answer (from the first-launch question or Settings) and
/// applies it at once.
pub fn set_consent(choice: Consent) {
    *lock(saved()) = Some(Some(choice));
    let path = consent_file();
    let bytes = serde_json::to_vec_pretty(&choice).unwrap_or_default();
    if let Err(e) = crate::durable::write(&path, &bytes) {
        eprintln!("arakun: could not save {}: {e}", path.display());
    }
    apply(effective());
}

fn apply(choice: Consent) {
    if !choice.usage {
        // What was queued while it was on is not sent either.
        lock(&queue().events).clear();
    }
    CRASHES.store(choice.crashes, Ordering::Relaxed);
    USAGE.store(choice.usage, Ordering::Relaxed);
    if (choice.crashes || choice.usage) && STARTED.set(()).is_ok() {
        install_panic_hook();
        if let Err(e) = std::thread::Builder::new()
            .name("telemetry".into())
            .spawn(send_loop)
        {
            eprintln!("arakun: telemetry not started: {e}");
        }
    }
}

/// Queues a usage event. `props` must say nothing about the user's work: a
/// kind, a count, a name Arakun itself gives.
pub fn capture(event: &str, props: &[(&str, &str)]) {
    if !USAGE.load(Ordering::Relaxed) {
        return;
    }
    let props = props
        .iter()
        .map(|(k, v)| (k.to_string(), Value::from(*v)))
        .collect();
    enqueue(event_json(event, props));
}

/// Reports an error Arakun handled, such as an agent that stopped with one:
/// `kind` is its type in PostHog (`AgentError`); only the scrubbed first line
/// of `message` is sent.
pub fn error(kind: &'static str, message: &str, props: &[(&str, &str)]) {
    if !CRASHES.load(Ordering::Relaxed) {
        return;
    }
    let mut map: Map<String, Value> = props
        .iter()
        .map(|(k, v)| (k.to_string(), Value::from(*v)))
        .collect();
    map.insert("$exception_level".into(), "error".into());
    map.insert(
        "$exception_list".into(),
        json!([{
            "type": kind,
            "value": scrub(message.lines().next().unwrap_or(""), &home()),
            "mechanism": {"type": "generic", "handled": true, "synthetic": false},
        }]),
    );
    enqueue(event_json("$exception", map));
}

/// Sends what is queued, waiting at most a couple of seconds; call before the
/// process ends.
pub fn flush() {
    if STARTED.get().is_none() {
        return;
    }
    let batch = std::mem::take(&mut *lock(&queue().events));
    if !batch.is_empty() {
        send(&batch, quick_agent());
    }
}

/// The event names Arakun sends, in one place so a rename shows in review.
pub mod events {
    pub const APP_LAUNCHED: &str = "app launched";
    pub const SESSION_STARTED: &str = "session started";
    pub const TURN_FINISHED: &str = "agent turn finished";
    pub const PROJECT_ADDED: &str = "project added";
    pub const ENVIRONMENT_CREATED: &str = "environment created";
    pub const VIEW_OPENED: &str = "view opened";
    pub const BROWSER_TAB_OPENED: &str = "browser tab opened";
}

// ---- events -----------------------------------------------------------------

fn event_json(name: &str, mut props: Map<String, Value>) -> Value {
    let os = match std::env::consts::OS {
        "linux" => "Linux",
        "macos" => "Mac OS X",
        "windows" => "Windows",
        other => other,
    };
    let build = if cfg!(debug_assertions) {
        "debug"
    } else {
        "release"
    };
    for (k, v) in [
        ("$lib", Value::from("arakun")),
        ("$lib_version", env!("CARGO_PKG_VERSION").into()),
        ("$app_name", "Arakun".into()),
        ("$app_version", env!("CARGO_PKG_VERSION").into()),
        ("$os", os.into()),
        ("arch", std::env::consts::ARCH.into()),
        ("build", build.into()),
        // Counted by install, never a person profile, and no location from the IP.
        ("$process_person_profile", false.into()),
        ("$geoip_disable", true.into()),
    ] {
        props.insert(k.into(), v);
    }
    json!({
        "event": name,
        "distinct_id": install_id(),
        "properties": props,
    })
}

/// A random id for this install, made on first use. Deleting the file starts
/// a new one.
fn install_id() -> String {
    static ID: OnceLock<String> = OnceLock::new();
    ID.get_or_init(|| {
        let path = crate::platform::app_dir().join("telemetry-id");
        if let Ok(id) = std::fs::read_to_string(&path) {
            let id = id.trim();
            if id.len() == 32 && id.bytes().all(|b| b.is_ascii_hexdigit()) {
                return id.to_string();
            }
        }
        let mut bytes = [0u8; 16];
        if getrandom::fill(&mut bytes).is_err() {
            return "unknown".into();
        }
        let id: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
        let _ = crate::durable::write(&path, id.as_bytes());
        id
    })
    .clone()
}

fn home() -> String {
    static HOME: OnceLock<String> = OnceLock::new();
    HOME.get_or_init(|| crate::platform::home().to_string_lossy().into_owned())
        .clone()
}

// ---- sending ----------------------------------------------------------------

struct Queue {
    events: Mutex<Vec<Value>>,
    wake: Condvar,
}

fn queue() -> &'static Queue {
    static QUEUE: OnceLock<Queue> = OnceLock::new();
    QUEUE.get_or_init(|| Queue {
        events: Mutex::new(Vec::new()),
        wake: Condvar::new(),
    })
}

fn enqueue(event: Value) {
    let q = queue();
    let mut events = lock(&q.events);
    if events.len() >= MAX_QUEUE {
        events.remove(0);
    }
    events.push(event);
    if events.len() >= 50 {
        q.wake.notify_one();
    }
}

fn send_loop() {
    let q = queue();
    loop {
        let batch = {
            let events = lock(&q.events);
            let (mut events, _) = q
                .wake
                .wait_timeout(events, BATCH_EVERY)
                .unwrap_or_else(|e| e.into_inner());
            std::mem::take(&mut *events)
        };
        if !batch.is_empty() {
            send(&batch, agent());
        }
    }
}

thread_local! {
    /// Set while this thread sends, so a panic inside the send is not sent.
    static SENDING: Cell<bool> = const { Cell::new(false) };
}

/// Posts `batch`; on failure the events are dropped, as reports are best effort.
fn send(batch: &[Value], agent: &ureq::Agent) {
    let body = json!({"api_key": KEY, "batch": batch}).to_string();
    SENDING.with(|s| s.set(true));
    let _ = agent
        .post(format!("{}/batch/", HOST.trim_end_matches('/')))
        .header("Content-Type", "application/json")
        .send(body);
    SENDING.with(|s| s.set(false));
}

fn agent_with(timeout: Duration) -> ureq::Agent {
    ureq::Agent::config_builder()
        .timeout_global(Some(timeout))
        .user_agent(concat!("arakun/", env!("CARGO_PKG_VERSION")))
        .build()
        .into()
}

fn agent() -> &'static ureq::Agent {
    static AGENT: OnceLock<ureq::Agent> = OnceLock::new();
    AGENT.get_or_init(|| agent_with(Duration::from_secs(10)))
}

/// For a dying process or one quitting: it must not hang there.
fn quick_agent() -> &'static ureq::Agent {
    static AGENT: OnceLock<ureq::Agent> = OnceLock::new();
    AGENT.get_or_init(|| agent_with(Duration::from_secs(2)))
}

// ---- panics -----------------------------------------------------------------

fn install_panic_hook() {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        if CRASHES.load(Ordering::Relaxed) && !SENDING.with(Cell::get) {
            let event = panic_event(info);
            send(&[event], quick_agent());
        }
        previous(info);
    }));
}

#[allow(deprecated)] // PanicHookInfo is only a rename of PanicInfo.
fn panic_event(info: &std::panic::PanicInfo<'_>) -> Value {
    let message = info
        .payload()
        .downcast_ref::<&str>()
        .map(|s| s.to_string())
        .or_else(|| info.payload().downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "panic occurred".into());
    let home = home();
    let mut props = Map::new();
    if let Some(at) = info.location() {
        props.insert("$exception_panic_file".into(), at.file().into());
        props.insert("$exception_panic_line".into(), at.line().into());
        props.insert("$exception_panic_column".into(), at.column().into());
    }
    if let Some(name) = std::thread::current().name() {
        props.insert("thread".into(), name.into());
    }
    props.insert("$exception_level".into(), "fatal".into());
    props.insert(
        "$exception_list".into(),
        json!([{
            "type": "Panic",
            "value": scrub(&message, &home),
            "mechanism": {"type": "panic", "handled": false, "synthetic": false},
            "stacktrace": {"type": "raw", "frames": frames(&home)},
        }]),
    );
    event_json("$exception", props)
}

/// The panicking thread's stack, outermost first, without this hook and the
/// standard library's panic machinery. Resolved here from the binary's line
/// tables, the way PostHog's own Rust SDK sends frames it resolved.
fn frames(home: &str) -> Vec<Value> {
    // (function, file, line), innermost first.
    let mut found: Vec<(Option<String>, Option<String>, Option<u32>)> = Vec::new();
    backtrace::trace(|frame| {
        // Inlined calls resolve to several symbols, innermost first.
        backtrace::resolve_frame(frame, |symbol| {
            let function = symbol.name().map(|n| format!("{n:#}"));
            let file = symbol.filename().map(|p| p.to_string_lossy().into_owned());
            if function.is_some() || file.is_some() {
                found.push((function, file, symbol.lineno()));
            }
        });
        true
    });
    let functions: Vec<&str> = found
        .iter()
        .map(|(f, _, _)| f.as_deref().unwrap_or(""))
        .collect();
    let skip = machinery_end(&functions);
    found.drain(..skip);
    found.truncate(MAX_FRAMES);
    found.reverse();
    found
        .into_iter()
        .map(|(function, file, line)| {
            let function = function.unwrap_or_default();
            let mut frame = json!({
                "function": function,
                "lang": "rust",
                "in_app": in_app(&function),
                "synthetic": false,
                "platform": "native",
                "client_resolved": true,
            });
            if let Some(file) = file {
                frame["filename"] = file.replacen(home, "~", 1).into();
            }
            if let Some(line) = line {
                frame["lineno"] = line.into();
            }
            frame
        })
        .collect()
}

/// How many innermost frames belong to the hook and the panic machinery: up
/// to and through the first run of `std::panicking`-like frames.
fn machinery_end(functions: &[&str]) -> usize {
    let machinery = |f: &str| {
        f.starts_with("std::panicking::")
            || f.starts_with("core::panicking::")
            || f.starts_with("std::sys::backtrace::")
            || f.starts_with("std::rt::begin_panic")
            || f.starts_with("__rust")
            || f == "rust_begin_unwind"
    };
    let Some(start) = functions.iter().position(|f| machinery(f)) else {
        return 0;
    };
    functions[start..]
        .iter()
        .position(|f| !machinery(f))
        .map_or(functions.len(), |n| start + n)
}

/// Arakun's own code, as opposed to the standard library and dependencies.
fn in_app(function: &str) -> bool {
    let path = function.trim_start_matches('<');
    path.split("::").next() == Some("arakun")
}

// ---- scrubbing --------------------------------------------------------------

/// `text` without what could identify the user or leak a secret: the home
/// folder, URLs, paths, emails and values shaped like tokens. The words
/// around them stay, so the message still says what went wrong.
pub fn scrub(text: &str, home: &str) -> String {
    // `/` alone would turn every path into `~`.
    let text = if home.len() >= 2 {
        text.replace(home, "~")
    } else {
        text.to_string()
    };
    let mut out = String::with_capacity(text.len());
    let mut after_scheme = false;
    for (i, word) in text.split(' ').enumerate() {
        if i > 0 {
            out.push(' ');
        }
        let lead = word.len() - word.trim_start_matches(|c| "([{<\"'`".contains(c)).len();
        let body = &word[lead..];
        let core = body.trim_end_matches(|c| ")]}>\"'`,.;:!?".contains(c));
        let tail = &body[core.len()..];
        let replaced = if after_scheme && !core.is_empty() {
            Some("<secret>".to_string())
        } else {
            redact(core)
        };
        after_scheme = matches!(core.to_ascii_lowercase().as_str(), "bearer" | "basic");
        out.push_str(&word[..lead]);
        out.push_str(replaced.as_deref().unwrap_or(core));
        out.push_str(tail);
    }
    match out.char_indices().nth(MAX_MESSAGE) {
        Some((i, _)) => format!("{}…", &out[..i]),
        None => out,
    }
}

/// What replaces `word`, if it needs replacing.
fn redact(word: &str) -> Option<String> {
    if word.is_empty() {
        return None;
    }
    if word.contains("://") {
        return Some("<url>".into());
    }
    // A Windows path, before `C:` reads as `key:value`.
    let b = word.as_bytes();
    if b.len() >= 3 && b[0].is_ascii_alphabetic() && b[1] == b':' && matches!(b[2], b'\\' | b'/') {
        return Some("<path>".into());
    }
    // `key=value`, `key:value`: the name says what failed, the value may be secret.
    if let Some((name, value)) = word.split_once(['=', ':'])
        && !name.is_empty()
        && !value.is_empty()
        && !name.contains(['/', '\\'])
    {
        let sep = &word[name.len()..name.len() + 1];
        let value = redact(value)?;
        return Some(format!("{name}{sep}{value}"));
    }
    if word.contains('@') && word.contains('.') && !word.starts_with('@') {
        return Some("<email>".into());
    }
    if word.contains(['/', '\\']) || word.starts_with('~') {
        return Some("<path>".into());
    }
    secret_shaped(word).then(|| "<secret>".into())
}

fn secret_shaped(word: &str) -> bool {
    const PREFIXES: [&str; 18] = [
        "sk-",
        "sk_",
        "pk_",
        "rk_",
        "ghp_",
        "gho_",
        "ghu_",
        "ghs_",
        "ghr_",
        "github_pat_",
        "xox",
        "phc_",
        "phx_",
        "phs_",
        "AKIA",
        "AIza",
        "glpat-",
        "eyJ",
    ];
    if word.len() >= 12 && PREFIXES.iter().any(|p| word.starts_with(p)) {
        return true;
    }
    let tokenish = word
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || "-_+=.".contains(c));
    let digits = word.chars().filter(char::is_ascii_digit).count();
    let letters = word.chars().filter(char::is_ascii_alphabetic).count();
    let hex = word.len() >= 16 && word.chars().all(|c| c.is_ascii_hexdigit());
    hex || (tokenish && word.len() >= 20 && digits >= 2 && letters >= 2)
}

#[cfg(test)]
mod tests {
    use super::*;

    const HOME: &str = "/home/ann";

    #[test]
    fn scrub_keeps_plain_words() {
        let s = "initialize failed: connection refused (os error 111)";
        assert_eq!(scrub(s, HOME), s);
    }

    #[test]
    fn scrub_removes_paths_inside_and_outside_home() {
        assert_eq!(
            scrub("cannot open /home/ann/work/plan.md: denied", HOME),
            "cannot open <path>: denied"
        );
        assert_eq!(
            scrub("no such file: '/srv/acme/repo/main.rs'", HOME),
            "no such file: '<path>'"
        );
        assert_eq!(
            scrub(r"failed in C:\Users\ann\acme", HOME),
            "failed in <path>"
        );
        assert_eq!(
            scrub("repo acme/payroll not found", HOME),
            "repo <path> not found"
        );
    }

    #[test]
    fn scrub_removes_tokens_urls_and_emails() {
        assert_eq!(
            scrub(
                "401 Unauthorized: invalid x-api-key sk-ant-api03-AbCdEf123456789xyz",
                HOME
            ),
            "401 Unauthorized: invalid x-api-key <secret>"
        );
        assert_eq!(
            scrub("Authorization: Bearer abc.def.ghi rejected", HOME),
            "Authorization: Bearer <secret> rejected"
        );
        assert_eq!(
            scrub("token=ghp_0123456789abcdefghij expired", HOME),
            "token=<secret> expired"
        );
        assert_eq!(
            scrub("GET https://api.acme.internal/v1?key=1 failed", HOME),
            "GET <url> failed"
        );
        assert_eq!(
            scrub("signed in as ann@acme.com", HOME),
            "signed in as <email>"
        );
        assert_eq!(
            scrub("session 3f2a9c1e4b5d6f708192a3b4c5d6e7f8 gone", HOME),
            "session <secret> gone"
        );
    }

    #[test]
    fn scrub_caps_the_length() {
        let long = "word ".repeat(200);
        assert!(scrub(&long, HOME).chars().count() <= MAX_MESSAGE + 1);
    }

    #[test]
    fn env_decides_before_the_saved_answer() {
        assert_eq!(env_choice(Some("1"), Some("1")), EnvChoice::Off);
        assert_eq!(env_choice(Some("0"), None), EnvChoice::Unset);
        assert_eq!(env_choice(Some(""), Some("on")), EnvChoice::On);
        assert_eq!(env_choice(None, Some("false")), EnvChoice::Off);
        assert_eq!(env_choice(None, None), EnvChoice::Unset);
    }

    #[test]
    fn panic_machinery_is_cut_up_to_the_panic_site() {
        let stack = [
            "backtrace::backtrace::trace",
            "arakun::telemetry::frames",
            "arakun::telemetry::install_panic_hook::{{closure}}",
            "std::panicking::rust_panic_with_hook",
            "std::sys::backtrace::__rust_end_short_backtrace",
            "rust_begin_unwind",
            "core::panicking::panic_bounds_check",
            "arakun::main::{{closure}}",
            "std::panicking::try",
        ];
        assert_eq!(machinery_end(&stack), 7);
        assert_eq!(machinery_end(&["arakun::main"]), 0);
    }

    #[test]
    fn only_arakun_frames_are_in_app() {
        assert!(in_app("arakun::main::{{closure}}"));
        assert!(in_app("<arakun::shell::Shell as gpui::Render>::render"));
        assert!(!in_app("core::slice::index::slice_index_fail"));
        assert!(!in_app("gpui::app::App::update"));
    }

    #[test]
    fn consent_reads_partial_files() {
        let c: Consent = serde_json::from_str(r#"{"crashes":true}"#).unwrap();
        assert_eq!(
            c,
            Consent {
                crashes: true,
                usage: false
            }
        );
    }
}
