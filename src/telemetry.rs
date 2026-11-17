//! Anonymous usage counts and crash reports, sent to Arakun's PostHog project.
//!
//! What is sent: panics with their stack trace, the first line of an agent's
//! error, and a few events such as "session started" with the agent's name.
//! Never a prompt, a reply, a file's contents or a project's name. Your home
//! folder in any text is replaced with `~` before it leaves the machine.
//!
//! Every event carries a random id kept in `app_dir/telemetry-id`, not tied to
//! any account. Reports are off in tests and in `arakun dev`'s test Arakun, and
//! in debug builds unless `ARAKUN_TELEMETRY=1`. Turn them off in Settings ›
//! General, or with `DO_NOT_TRACK=1` or `ARAKUN_TELEMETRY=0`.

use std::path::PathBuf;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};

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

/// Turned off from Settings while running. The SDK's panic hook stays
/// installed, so `before_send` drops what it would still send.
static OFF: AtomicBool = AtomicBool::new(false);
static STARTED: OnceLock<()> = OnceLock::new();

/// Whether the user lets Arakun send reports, from the environment and the
/// Settings switch.
pub fn allowed() -> bool {
    let env = |name| {
        std::env::var(name)
            .ok()
            .map(|v| v.trim().to_ascii_lowercase())
    };
    // https://consoledonottrack.com: any value but empty or 0 opts out.
    if env("DO_NOT_TRACK").is_some_and(|v| !v.is_empty() && v != "0") {
        return false;
    }
    match env("ARAKUN_TELEMETRY").as_deref() {
        Some("0" | "off" | "false" | "no") => return false,
        Some("1" | "on" | "true" | "yes") => {}
        // Developers' own runs would swamp real use.
        _ if cfg!(debug_assertions) => return false,
        _ => {}
    }
    switched_on()
}

/// Starts the client and the panic hook once, if reports are allowed.
pub fn init() {
    if cfg!(test) || KEY.is_empty() || crate::dev::size().is_some() || !allowed() {
        return;
    }
    start();
}

fn start() {
    if STARTED.set(()).is_err() {
        OFF.store(false, Ordering::Relaxed);
        return;
    }
    let id = install_id();
    let home = crate::platform::home().to_string_lossy().into_owned();
    let options = posthog_rs::ClientOptionsBuilder::default()
        .api_key(KEY.to_string())
        .host(HOST)
        // A desktop app: PostHog may read the OS as the user's.
        .is_server(false)
        .request_timeout_seconds(10)
        // Quitting waits at most this long for what is queued.
        .shutdown_timeout_ms(2_000)
        .error_tracking(
            posthog_rs::ErrorTrackingOptionsBuilder::default()
                .capture_panics(true)
                .build()
                .expect("error tracking options are complete"),
        )
        .before_send(move |event| prepare(event, &id, &home))
        .build();
    let result = options
        .map_err(|e| e.to_string())
        .and_then(|o| posthog_rs::init_global(o).map_err(|e| e.to_string()));
    if let Err(e) = result {
        eprintln!("arakun: telemetry not started: {e}");
    }
}

/// The Settings switch alone, whatever the environment says.
pub fn switched_on() -> bool {
    !off_marker().exists()
}

/// Turns reports on or off from Settings; remembered across runs.
pub fn set_allowed(on: bool) {
    let marker = off_marker();
    if on {
        let _ = std::fs::remove_file(&marker);
        OFF.store(false, Ordering::Relaxed);
        init();
    } else {
        // Also drops what is queued: `prepare` runs when the worker sends.
        OFF.store(true, Ordering::Relaxed);
        if let Some(dir) = marker.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let _ = std::fs::write(&marker, "no usage or crash reports\n");
    }
}

/// Queues `event` with `props`. Values must say nothing about the user's
/// work: a kind, a count, a name Arakun itself gives.
pub fn capture(event: &str, props: &[(&str, &str)]) {
    if STARTED.get().is_none() || OFF.load(Ordering::Relaxed) {
        return;
    }
    let mut ev = posthog_rs::Event::new(event.to_string(), install_id());
    for (k, v) in props {
        let _ = ev.insert_prop(*k, *v);
    }
    posthog_rs::capture(ev);
}

/// Reports an error Arakun handled, such as an agent that stopped with one.
/// `kind` says where it happened; only the first line of `message` is sent.
pub fn error(kind: &'static str, message: &str, props: &[(&str, &str)]) {
    if STARTED.get().is_none() || OFF.load(Ordering::Relaxed) {
        return;
    }
    let line = message.lines().next().unwrap_or("").trim();
    let line = match line.char_indices().nth(300) {
        Some((i, _)) => &line[..i],
        None => line,
    };
    let mut options = posthog_rs::CaptureExceptionOptions::new().distinct_id(install_id());
    for (k, v) in props {
        options = options.clone().property(*k, *v).unwrap_or(options);
    }
    let err = Handled {
        kind,
        message: line.to_string(),
    };
    let _ = posthog_rs::capture_exception_with(&err, options);
}

/// Sends what is queued; call before the process ends.
pub fn flush() {
    if STARTED.get().is_some() {
        posthog_rs::shutdown();
    }
}

#[derive(Debug)]
struct Handled {
    kind: &'static str,
    message: String,
}

impl std::fmt::Display for Handled {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.kind, self.message)
    }
}

impl std::error::Error for Handled {}

/// Adds what every event shares and takes the home folder out of all text.
fn prepare(mut event: posthog_rs::Event, id: &str, home: &str) -> Option<posthog_rs::Event> {
    if OFF.load(Ordering::Relaxed) {
        return None;
    }
    let keys: Vec<String> = event.properties().keys().cloned().collect();
    for key in keys {
        if let Some(mut value) = event.remove_prop(&key) {
            scrub(&mut value, home);
            let _ = event.insert_prop(key, value);
        }
    }
    let _ = event.insert_prop("$app_version", env!("CARGO_PKG_VERSION"));
    let _ = event.insert_prop("$app_name", "Arakun");
    let _ = event.insert_prop("arch", std::env::consts::ARCH);
    let _ = event.insert_prop(
        "build",
        if cfg!(debug_assertions) {
            "debug"
        } else {
            "release"
        },
    );
    // Panics are captured without a person; this still counts the people they hit.
    let _ = event.insert_prop("install_id", id);
    Some(event)
}

/// Replaces `home` with `~` in every string inside `value`.
fn scrub(value: &mut serde_json::Value, home: &str) {
    // `/` alone would turn every path into `~`.
    if home.len() < 2 {
        return;
    }
    match value {
        serde_json::Value::String(s) if s.contains(home) => *s = s.replace(home, "~"),
        serde_json::Value::Array(items) => items.iter_mut().for_each(|v| scrub(v, home)),
        serde_json::Value::Object(map) => map.values_mut().for_each(|v| scrub(v, home)),
        _ => {}
    }
}

fn off_marker() -> PathBuf {
    crate::platform::app_dir().join("telemetry-off")
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

/// The event names and property values Arakun sends, kept in one place so a
/// rename shows up in review.
pub mod events {
    pub const APP_LAUNCHED: &str = "app launched";
    pub const SESSION_STARTED: &str = "session started";
    pub const TURN_FINISHED: &str = "agent turn finished";
    pub const PROJECT_ADDED: &str = "project added";
    pub const ENVIRONMENT_CREATED: &str = "environment created";
    pub const VIEW_OPENED: &str = "view opened";
    pub const BROWSER_TAB_OPENED: &str = "browser tab opened";
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn scrub_replaces_home_everywhere() {
        let mut v = json!({
            "$exception_list": [{"value": "open /home/ann/x.rs: denied"}],
            "n": 3,
            "s": "/home/ann",
        });
        scrub(&mut v, "/home/ann");
        assert_eq!(
            v,
            json!({
                "$exception_list": [{"value": "open ~/x.rs: denied"}],
                "n": 3,
                "s": "~",
            })
        );
    }

    #[test]
    fn scrub_leaves_root_home_alone() {
        let mut v = json!("/usr/bin");
        scrub(&mut v, "/");
        assert_eq!(v, json!("/usr/bin"));
    }

    #[test]
    fn handled_error_reads_kind_then_message() {
        let e = Handled {
            kind: "agent error",
            message: "initialize failed".into(),
        };
        assert_eq!(e.to_string(), "agent error: initialize failed");
    }
}
