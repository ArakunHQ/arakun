//! The panic hook: every panic is written to a crash log on this machine, and
//! sent to PostHog only if the user agreed to crash reports (`telemetry`).
//!
//! Arakun mostly starts from the desktop, where nobody sees stderr, so without
//! the log a panic leaves nothing to put in a bug report. The log is
//! `crash.log` in Arakun's data folder and never leaves the machine by itself.
//! Its messages are scrubbed the way a sent report is, so it can be pasted
//! into an issue.
//!
//! A panic that a guard stops (`guard`), or that only ends a thread, is an
//! error: the app carries on. Only a panic on the main thread is a crash.

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::thread::ThreadId;

use serde_json::{Map, Value, json};

/// Past this size the older half of the log is dropped.
const MAX_LOG: u64 = 256 * 1024;
/// The frames nearest the panic that are written.
const MAX_FRAMES: usize = 40;

static MAIN: OnceLock<ThreadId> = OnceLock::new();

pub fn log_path() -> PathBuf {
    crate::platform::app_dir().join("crash.log")
}

/// Installs the hook. Call it on the main thread: a panic there is the one
/// that ends the app.
pub fn install() {
    let _ = MAIN.set(std::thread::current().id());
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let on_main = MAIN.get() == Some(&std::thread::current().id());
        let fatal = on_main && !crate::guard::active();
        let props = crate::telemetry::panic_props(info, fatal);
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_millis() as u64);
        let entry = entry(&props, &crate::telemetry::timestamp(now), fatal);
        // Best effort: a full disk must not turn one panic into two.
        let _ = append(&log_path(), &entry, MAX_LOG);
        let sent = crate::telemetry::send_panic(props);
        // Only a crash whose report went out can be linked to an answer.
        if let (true, Some(event)) = (fatal, sent)
            && enabled()
        {
            let _ = write_marker(&app_dir(), Some(&event));
        }
        previous(info);
    }));
}

fn app_dir() -> PathBuf {
    crate::platform::app_dir()
}

/// The file that exists while Arakun runs. A clean exit deletes it, so one
/// left behind means the last run ended badly.
fn marker_path(dir: &Path) -> PathBuf {
    dir.join("running")
}

/// Tests and `arakun dev` never write it: they are killed all the time. A
/// debug build writes it only when `ARAKUN_CRASH_MARKER` is set, so a plain
/// `cargo run` has no crash to ask about and the notice can be tried by hand.
fn enabled() -> bool {
    enabled_for(
        cfg!(test),
        cfg!(debug_assertions),
        crate::dev::size().is_some(),
        std::env::var_os("ARAKUN_CRASH_MARKER").is_some(),
    )
}

fn enabled_for(test: bool, debug: bool, dev: bool, marker_var: bool) -> bool {
    if test {
        return false;
    }
    if debug { marker_var } else { !dev }
}

/// `{"event": uuid}` when a fatal panic's report was sent, else empty.
fn write_marker(dir: &Path, event: Option<&str>) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    let text = event.map_or(String::new(), |e| json!({ "event": e }).to_string());
    std::fs::write(marker_path(dir), text)
}

fn clear_marker(dir: &Path) {
    let _ = std::fs::remove_file(marker_path(dir));
}

/// Outer `Some` if the marker exists; inner the crash report's event id.
fn read_marker(dir: &Path) -> Option<Option<String>> {
    let text = std::fs::read_to_string(marker_path(dir)).ok()?;
    let event = serde_json::from_str::<Value>(&text)
        .ok()
        .and_then(|v| v["event"].as_str().map(str::to_string));
    Some(event)
}

static PREVIOUS: OnceLock<Option<Option<String>>> = OnceLock::new();

/// Whether the last run ended without a clean exit, and the event id of its
/// crash report if one was sent. Read from the marker once, before
/// `mark_running` replaces it.
pub fn last_run_ended_badly() -> Option<Option<String>> {
    PREVIOUS.get_or_init(|| read_marker(&app_dir())).clone()
}

/// Marks this run as started. Call it once at startup.
pub fn mark_running() {
    let _ = last_run_ended_badly();
    if enabled() {
        let _ = write_marker(&app_dir(), None);
    }
}

/// Marks this run as ended on purpose (quit, close, restart).
pub fn mark_clean_exit() {
    if enabled() {
        clear_marker(&app_dir());
    }
}

/// One panic as the log shows it, ending in a blank line.
fn entry(props: &Map<String, Value>, when: &str, fatal: bool) -> String {
    let text = |key: &str| props.get(key).and_then(Value::as_str);
    let number = |key: &str| props.get(key).and_then(Value::as_u64);
    let build = if cfg!(debug_assertions) {
        "debug"
    } else {
        "release"
    };
    let mut out = format!(
        "{when} Arakun {} ({build}, {} {})\n",
        env!("ARAKUN_VERSION"),
        std::env::consts::OS,
        std::env::consts::ARCH,
    );
    let thread = text("thread").unwrap_or("unnamed");
    let at = match text("$exception_panic_file") {
        Some(file) => format!(
            " at {file}:{}:{}",
            number("$exception_panic_line").unwrap_or(0),
            number("$exception_panic_column").unwrap_or(0),
        ),
        None => String::new(),
    };
    let after = if fatal {
        "Arakun closed"
    } else {
        "Arakun carried on"
    };
    out.push_str(&format!("Thread {thread} panicked{at}. {after}.\n"));
    let exception = &props["$exception_list"][0];
    if let Some(message) = exception["value"].as_str() {
        out.push_str(&format!("  {message}\n"));
    }
    // Sent outermost first; the reader wants the panic's own frame on top.
    if let Some(frames) = exception["stacktrace"]["frames"].as_array() {
        for frame in frames.iter().rev().take(MAX_FRAMES) {
            let function = frame["function"].as_str().unwrap_or("?");
            match (frame["filename"].as_str(), frame["lineno"].as_u64()) {
                (Some(file), Some(line)) => {
                    out.push_str(&format!("    {function} ({file}:{line})\n"))
                }
                (Some(file), None) => out.push_str(&format!("    {function} ({file})\n")),
                _ => out.push_str(&format!("    {function}\n")),
            }
        }
    }
    out.push('\n');
    out
}

/// Adds `entry` to the log at `path`. A log over `max` bytes first loses its
/// older half, cut where an entry starts.
fn append(path: &Path, entry: &str, max: u64) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    if std::fs::metadata(path).is_ok_and(|m| m.len() > max) {
        let old = std::fs::read(path)?;
        let from = old.len().saturating_sub(max as usize / 2);
        let cut = old[from..]
            .windows(2)
            .position(|w| w == b"\n\n")
            .map_or(old.len(), |i| from + i + 2);
        std::fs::write(path, &old[cut..])?;
    }
    std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?
        .write_all(entry.as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn props() -> Map<String, Value> {
        let value = json!({
            "$exception_panic_file": "src/agent/workers_ai.rs",
            "$exception_panic_line": 2010,
            "$exception_panic_column": 5,
            "thread": "workers-ai-turn",
            "$exception_list": [{
                "value": "index out of bounds",
                "stacktrace": {"frames": [
                    {"function": "std::thread::spawn"},
                    {"function": "arakun::agent::turn", "filename": "src/agent/workers_ai.rs", "lineno": 2010},
                ]},
            }],
        });
        value.as_object().unwrap().clone()
    }

    #[test]
    fn an_entry_says_where_and_what_happened_next() {
        let text = entry(&props(), "2026-11-18T10:00:00.000Z", false);
        let lines: Vec<&str> = text.lines().collect();
        assert!(lines[0].starts_with("2026-11-18T10:00:00.000Z Arakun "));
        assert_eq!(
            lines[1],
            "Thread workers-ai-turn panicked at src/agent/workers_ai.rs:2010:5. Arakun carried on."
        );
        assert_eq!(lines[2], "  index out of bounds");
        assert_eq!(
            lines[3],
            "    arakun::agent::turn (src/agent/workers_ai.rs:2010)"
        );
        assert_eq!(lines[4], "    std::thread::spawn");
        assert!(text.ends_with("\n\n"));
        assert!(entry(&props(), "now", true).contains(". Arakun closed.\n"));
    }

    fn temp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("arakun-marker-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn marker_absent_after_clean_exit() {
        let dir = temp("clean");
        write_marker(&dir, None).unwrap();
        clear_marker(&dir);
        assert_eq!(read_marker(&dir), None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn marker_present_after_kill() {
        let dir = temp("kill");
        write_marker(&dir, None).unwrap();
        assert_eq!(read_marker(&dir), Some(None));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn marker_carries_panic_uuid() {
        let dir = temp("uuid");
        write_marker(&dir, Some("0190-abc")).unwrap();
        assert_eq!(read_marker(&dir), Some(Some("0190-abc".to_string())));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn tests_never_write_the_real_marker() {
        assert!(!enabled());
    }

    #[test]
    fn enabled_logic() {
        // Tests never write it, whatever else is set.
        assert!(!enabled_for(true, false, false, true));
        // A release build writes it, except under `arakun dev`.
        assert!(enabled_for(false, false, false, false));
        assert!(!enabled_for(false, false, true, false));
        // A plain debug run does not; the variable turns it on.
        assert!(!enabled_for(false, true, false, false));
        assert!(enabled_for(false, true, false, true));
    }

    #[test]
    fn a_long_log_loses_its_oldest_entries() {
        let dir = std::env::temp_dir().join(format!("arakun-crash-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("crash.log");
        for n in 0..10 {
            append(&path, &format!("entry {n}\n  what\n\n"), 60).unwrap();
        }
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.starts_with("entry "), "{text}");
        assert!(text.ends_with("entry 9\n  what\n\n"), "{text}");
        assert!(!text.contains("entry 0"), "{text}");
        assert!(text.len() < 200, "{text}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
