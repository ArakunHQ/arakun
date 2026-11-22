//! Crash reports and anonymous usage counts, sent to Arakun's PostHog project
//! only after the user says yes.
//!
//! Nothing is sent until the user answers the question Arakun asks on first
//! launch (`telemetry_dialog`), and only what they agreed to: crash reports
//! (panics with their stack trace, agent errors) and usage counts are two
//! choices, kept in `app_dir/telemetry.json` and changed in Settings › Privacy.
//! `DO_NOT_TRACK=1` or `ARAKUN_TELEMETRY=0` turn both off and skip the
//! question; `ARAKUN_TELEMETRY=1` turns both on. While the question is open,
//! usage events wait in memory: sent if the answer is yes, dropped otherwise.
//!
//! Never a prompt, a reply or a file's contents. An agent's error is sent as
//! a fixed category, not its text (`category`). A panic's message is the one
//! text sent: it loses URLs, paths, emails and anything shaped like a secret
//! before it leaves the machine (`scrub`), and its other words go as they are. Every event carries a random id
//! kept in `app_dir/telemetry-id`, not tied to any account, and creates no
//! person profile. Debug builds, tests and `arakun dev` send nothing.
//!
//! Events go to PostHog's capture API over the `ureq` client the app already
//! has, from one background thread; a panic is sent right away from the
//! panicking thread, since the process may not outlive it. The panic hook
//! itself is `crash`'s, which also keeps a crash log on this machine whatever
//! the answer was.
//!
//! Feedback is different. A survey response the user sends on purpose (user
//! and error feedback) goes out even if usage is off, because pressing Send is
//! the consent (`feedback_allowed`). Agent feedback and popover surveys still
//! need usage. `DO_NOT_TRACK` and `ARAKUN_TELEMETRY=0` block all of it, and a
//! response never waits in `hold`.

use std::cell::Cell;
use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Condvar, Mutex, MutexGuard, OnceLock};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

/// The project token: write-only, so it is safe in the binary. A build can
/// point at another project with `ARAKUN_POSTHOG_KEY` (empty turns it off).
pub(crate) const KEY: &str = match option_env!("ARAKUN_POSTHOG_KEY") {
    Some(key) => key,
    None => "phc_kXKrLfaWumKa3mfXcFVBCRpyKa6DZiUv7hCkQW2epLQZ",
};
pub(crate) const HOST: &str = match option_env!("ARAKUN_POSTHOG_HOST") {
    Some(host) => host,
    None => "https://us.i.posthog.com",
};

/// Usage events wait this long to go out together.
const BATCH_EVERY: Duration = Duration::from_secs(5);
/// Past this many queued events the oldest are dropped (PostHog unreachable).
const MAX_QUEUE: usize = 500;
/// Usage events kept while the first-launch question is open.
const MAX_HELD: usize = 100;
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
/// Set while the first-launch question is open: usage events wait in memory
/// for the answer, so the launch and the setup that come before it still count
/// when the user says yes. A no, or quitting first, drops them unsent.
static HOLDING: AtomicBool = AtomicBool::new(false);
static LAUNCHED: OnceLock<std::time::Instant> = OnceLock::new();

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
pub(crate) fn blocked() -> bool {
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
    *lock(saved()).get_or_insert_with(|| crate::durable::load_json(consent_file()))
}

/// Whether to ask on this launch: never asked, and nothing else decides it.
pub fn should_ask() -> bool {
    !blocked() && env() == EnvChoice::Unset && consent().is_none()
}

/// What this process sends, which Settings shows.
pub fn effective() -> Consent {
    if blocked() {
        Consent::NONE
    } else if env() == EnvChoice::On {
        Consent::ALL
    } else {
        consent().unwrap_or(Consent::NONE)
    }
}

/// The variable that decides instead of the user's answer, if one does.
pub fn env_override() -> Option<&'static str> {
    match env() {
        EnvChoice::Unset => None,
        EnvChoice::Off
            if std::env::var("DO_NOT_TRACK")
                .is_ok_and(|v| !v.trim().is_empty() && v.trim() != "0") =>
        {
            Some("DO_NOT_TRACK")
        }
        _ => Some("ARAKUN_TELEMETRY"),
    }
}

/// Starts sending what the user agreed to, if anything.
pub fn init() {
    let _ = LAUNCHED.set(std::time::Instant::now());
    HOLDING.store(should_ask(), Ordering::Relaxed);
    apply(effective());
}

/// Saves the user's answer (from the first-launch question or Settings) and
/// applies it at once. When it cannot be saved, this process still follows
/// it, and the old answer is deleted so it cannot come back next launch,
/// which then asks again.
pub fn set_consent(choice: Consent) -> Result<(), String> {
    *lock(saved()) = Some(Some(choice));
    let now = effective();
    apply(now);
    release_held(now.usage);
    let path = consent_file();
    let bytes = serde_json::to_vec_pretty(&choice).unwrap_or_default();
    crate::durable::write(&path, &bytes).map_err(|e| {
        let _ = std::fs::remove_file(&path);
        format!("Could not save your choice ({e}); Arakun will ask again next time.")
    })
}

fn apply(choice: Consent) {
    // The flags first, so nothing can be queued under the old choice after
    // the queue is cleaned below.
    CRASHES.store(choice.crashes, Ordering::Relaxed);
    USAGE.store(choice.usage, Ordering::Relaxed);
    // What was queued while a choice was on is not sent once it is off.
    lock(&queue().events).retain(|e| allowed(e, choice));
    if choice.crashes || choice.usage {
        start_sender();
    }
}

fn start_sender() {
    if STARTED.set(()).is_ok()
        && let Err(e) = std::thread::Builder::new()
            .name("telemetry".into())
            .spawn(send_loop)
    {
        eprintln!("arakun: telemetry not started: {e}");
    }
}

/// Where a survey response comes from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Source {
    User,
    Error,
    Agent,
    Popover,
}

impl Source {
    pub fn as_str(self) -> &'static str {
        match self {
            Source::User => "user",
            Source::Error => "error",
            Source::Agent => "agent",
            Source::Popover => "popover",
        }
    }

    /// Whether the person pressed Send themselves, which is their consent.
    fn on_purpose(self) -> bool {
        matches!(self, Source::User | Source::Error)
    }
}

/// Why a response was not sent.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Refused {
    /// `DO_NOT_TRACK` or `ARAKUN_TELEMETRY=0`.
    Blocked,
    NoUsage,
    DevBuild,
}

impl std::fmt::Display for Refused {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Refused::Blocked => "Telemetry is turned off by DO_NOT_TRACK or ARAKUN_TELEMETRY=0.",
            Refused::NoUsage => "Usage telemetry is off, so this was not sent.",
            Refused::DevBuild => "Debug builds and arakun dev send nothing.",
        })
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Blocked {
    No,
    Env,
    Dev,
}

fn blocked_by() -> Blocked {
    if env() == EnvChoice::Off {
        Blocked::Env
    } else if blocked() {
        Blocked::Dev
    } else {
        Blocked::No
    }
}

/// The consent table for survey responses, apart from the live state so a
/// test can reach every row.
fn feedback_decision(src: Source, blocked: Blocked, consent: Consent) -> Result<(), Refused> {
    match blocked {
        Blocked::Env => return Err(Refused::Blocked),
        Blocked::Dev => return Err(Refused::DevBuild),
        Blocked::No => {}
    }
    if src.on_purpose() || consent.usage {
        Ok(())
    } else {
        Err(Refused::NoUsage)
    }
}

/// Whether a response from `src` may be sent now.
pub fn feedback_allowed(src: Source) -> Result<(), Refused> {
    feedback_decision(src, blocked_by(), effective())
}

/// A top-level field on a queued event, set only by `send_feedback`. It is
/// outside `properties` so no caller of `capture` can forge it, and it is
/// stripped before the event is posted (`payload`).
const FEEDBACK_MARK: &str = "arakun_feedback_source";

/// The event for a response from `src`. The source comes from the parameter,
/// whatever `props` says.
fn feedback_event(event: &str, src: Source, mut props: Map<String, Value>) -> Value {
    props.insert("source".into(), src.as_str().into());
    let mut e = event_json(event, props);
    e[FEEDBACK_MARK] = src.as_str().into();
    e
}

/// Sends a survey response. It never waits in `hold`: user and error feedback
/// goes out even while the first-launch question is open.
pub fn send_feedback(event: &str, src: Source, props: Map<String, Value>) -> Result<(), Refused> {
    feedback_allowed(src)?;
    enqueue(feedback_event(event, src, props));
    start_sender();
    Ok(())
}

/// Queues a usage event. `props` must say nothing about the user's work: a
/// kind, a count, a name Arakun itself gives.
pub fn capture(event: &str, props: &[(&str, &str)]) {
    capture_with(event, props, &[]);
}

/// `capture` with counts too, sent as numbers so PostHog can sum and
/// average them.
pub fn capture_with(event: &str, props: &[(&str, &str)], counts: &[(&str, u64)]) {
    // A response must go through `send_feedback`, which checks its source.
    debug_assert!(event != events::SURVEY_SENT, "use send_feedback");
    if event == events::SURVEY_SENT {
        return;
    }
    note_event(event);
    let usage = USAGE.load(Ordering::Relaxed);
    if !usage && !HOLDING.load(Ordering::Relaxed) {
        return;
    }
    let mut map: Map<String, Value> = props
        .iter()
        .map(|(k, v)| (k.to_string(), Value::from(*v)))
        .collect();
    for (k, v) in counts {
        map.insert(k.to_string(), Value::from(*v));
    }
    let event = event_json(event, map);
    if usage {
        enqueue(event);
    } else {
        hold(event);
    }
}

/// Most event names kept; a popover survey names a handful.
const SEEN_EVENTS_MAX: usize = 256;

fn seen_names() -> &'static Mutex<std::collections::HashSet<String>> {
    static SEEN: OnceLock<Mutex<std::collections::HashSet<String>>> = OnceLock::new();
    SEEN.get_or_init(Default::default)
}

/// Remembers a name, never its properties, and before the consent check: a
/// survey can ask for an event that was not sent.
fn note_event(name: &str) {
    let mut set = seen_names().lock().unwrap_or_else(|e| e.into_inner());
    if set.len() < SEEN_EVENTS_MAX {
        set.insert(name.to_string());
    }
}

/// The names of the events captured since launch, whether or not they were
/// sent. Popover surveys use it for their event conditions.
pub(crate) fn seen_events() -> std::collections::HashSet<String> {
    seen_names()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone()
}

fn held() -> &'static Mutex<Vec<Value>> {
    static HELD: OnceLock<Mutex<Vec<Value>>> = OnceLock::new();
    HELD.get_or_init(|| Mutex::new(Vec::new()))
}

fn hold(event: Value) {
    let mut held = lock(held());
    if held.len() < MAX_HELD {
        held.push(event);
    }
}

/// The first answer is in: what waited for it is queued, or dropped.
fn release_held(send: bool) {
    HOLDING.store(false, Ordering::Relaxed);
    let held = std::mem::take(&mut *lock(held()));
    if send {
        for event in held {
            enqueue(event);
        }
    }
}

/// Counts the quit with how long Arakun was open, then sends what is queued,
/// waiting at most a couple of seconds.
pub fn quit() {
    let minutes = LAUNCHED.get().map_or(0, |t| t.elapsed().as_secs() / 60);
    capture_with(events::APP_QUIT, &[], &[("minutes_open", minutes)]);
    // Before the flush: a kill during its wait must not look like a crash.
    crate::crash::mark_clean_exit();
    flush();
}

/// Reports an error Arakun handled, such as an agent that stopped with one:
/// `kind` is its type in PostHog (`AgentError`). Only a fixed category of
/// `message` is sent (`category`), never its text: an agent's error can hold
/// a project's name or a credential no scrubbing would recognise. `props` must
/// hold only fixed labels, numbers and flags for the same reason.
///
/// Returns the event's id, or None when nothing was sent.
pub fn error(kind: &'static str, message: &str, props: &[(&str, Value)]) -> Option<String> {
    if !CRASHES.load(Ordering::Relaxed) {
        return None;
    }
    let mut map: Map<String, Value> = props
        .iter()
        .map(|(k, v)| (k.to_string(), v.clone()))
        .collect();
    map.insert("$exception_level".into(), "error".into());
    map.insert(
        "$exception_list".into(),
        json!([{
            "type": kind,
            "value": category(message),
            "mechanism": {"type": "generic", "handled": true, "synthetic": false},
        }]),
    );
    let event = event_json("$exception", map);
    let id = event["uuid"].as_str().map(str::to_string);
    enqueue(event);
    id
}

/// Sends what is queued, waiting at most a couple of seconds.
fn flush() {
    if STARTED.get().is_none() {
        return;
    }
    let batch: Vec<Value> = std::mem::take(&mut *lock(&queue().events)).into();
    send(&batch, quick_agent());
}

/// Whether `event` may go out under `choice`: crash reports and usage counts
/// are separate answers.
fn allowed(event: &Value, choice: Consent) -> bool {
    if event["event"] == "$exception" {
        choice.crashes
    } else if event["event"] == events::SURVEY_SENT {
        // Feedback the person sent on purpose is its own consent, but only
        // when `send_feedback` marked it so.
        let src = event[FEEDBACK_MARK].as_str();
        matches!(src, Some("user" | "error")) || choice.usage
    } else {
        choice.usage
    }
}

fn current() -> Consent {
    Consent {
        crashes: CRASHES.load(Ordering::Relaxed),
        usage: USAGE.load(Ordering::Relaxed),
    }
}

/// What an agent's error was, as one of a few fixed labels.
pub(crate) fn category(message: &str) -> &'static str {
    let m = message.to_ascii_lowercase();
    let has = |words: &[&str]| words.iter().any(|w| m.contains(w));
    if has(&[
        "is not signed in",
        "unauthorized",
        "401",
        "403",
        "forbidden",
    ]) {
        "not signed in"
    } else if has(&["rate limit", "429", "too many requests", "quota"]) {
        "rate limited"
    } else if has(&[
        "overloaded",
        "529",
        "503",
        "502",
        "500 ",
        "internal server error",
    ]) {
        "provider error"
    } else if has(&["timed out", "timeout"]) {
        "timed out"
    } else if has(&[
        "connection refused",
        "connection reset",
        "dns",
        "network",
        "unreachable",
    ]) {
        "network error"
    // Only Arakun's own wording for a missing CLI. A bare "not found" is also in
    // a provider's 404, a missing model, an adapter's stderr and a deleted folder.
    } else if has(&["was not found on path", "own binary is missing"]) {
        "agent not found"
    } else if has(&["npm was not found"]) {
        "npm not found"
    } else if has(&["stopped responding"]) {
        "stopped responding"
    } else if has(&[" exited ("]) {
        "process exited"
    } else if m.starts_with("initialize failed") {
        "initialize failed"
    } else if has(&["starting the session failed", "returned no sessionid"]) {
        "session start failed"
    // The CLI was found but did not run: often the session's folder is gone.
    } else if m.starts_with("starting ") {
        "failed to start"
    } else if m.starts_with("the turn failed") {
        "turn failed"
    } else {
        "other"
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
    pub const ONBOARDING_FINISHED: &str = "onboarding finished";
    pub const APP_QUIT: &str = "app quit";
    pub const UPDATE_ASKED: &str = "update asked";
    pub const SURVEY_SENT: &str = "survey sent";
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
        ("$lib_version", env!("ARAKUN_VERSION").into()),
        ("$app_name", "Arakun".into()),
        ("$app_version", env!("ARAKUN_VERSION").into()),
        ("$os", os.into()),
        ("arch", std::env::consts::ARCH.into()),
        ("build", build.into()),
        // Counted by install, never a person profile, and no location from the IP.
        ("$process_person_profile", false.into()),
        ("$geoip_disable", true.into()),
    ] {
        props.insert(k.into(), v);
    }
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64);
    json!({
        "event": name,
        "uuid": uuid_v4(),
        "distinct_id": install_id(),
        "properties": props,
        // When it happened: an event can wait for the first answer, or for
        // the next batch, before it is sent.
        "timestamp": timestamp(now),
    })
}

/// A random version 4 UUID, which PostHog keeps as the event's id.
pub(crate) fn uuid_v4() -> String {
    let mut b = [0u8; 16];
    // A failed read leaves zeros: still well formed, and PostHog drops a
    // repeat, so nothing worse than a lost event.
    let _ = getrandom::fill(&mut b);
    b[6] = (b[6] & 0x0f) | 0x40;
    b[8] = (b[8] & 0x3f) | 0x80;
    let h: String = b.iter().map(|x| format!("{x:02x}")).collect();
    format!(
        "{}-{}-{}-{}-{}",
        &h[..8],
        &h[8..12],
        &h[12..16],
        &h[16..20],
        &h[20..]
    )
}

/// `unix_ms` as RFC 3339 in UTC, as PostHog's capture API takes it.
pub fn timestamp(unix_ms: u64) -> String {
    let secs = (unix_ms / 1000) as i64;
    let (y, m, d) = crate::platform::civil_from_days(secs.div_euclid(86_400));
    let s = secs.rem_euclid(86_400);
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}.{:03}Z",
        s / 3600,
        s / 60 % 60,
        s % 60,
        unix_ms % 1000
    )
}

/// A random id for this install, made on first use. Deleting the file starts
/// a new one.
pub(crate) fn install_id() -> String {
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

/// Who `flags` asks for: the install's id when the user agreed to usage
/// counts, else a random one that lives as long as this process, so a
/// feature flag check can't tie this machine to anything.
pub(crate) fn flags_id() -> String {
    if effective().usage {
        return install_id();
    }
    static ID: OnceLock<String> = OnceLock::new();
    ID.get_or_init(|| {
        let mut bytes = [0u8; 16];
        let _ = getrandom::fill(&mut bytes);
        bytes.iter().map(|b| format!("{b:02x}")).collect()
    })
    .clone()
}

pub(crate) fn home() -> String {
    static HOME: OnceLock<String> = OnceLock::new();
    HOME.get_or_init(|| crate::platform::home().to_string_lossy().into_owned())
        .clone()
}

// ---- sending ----------------------------------------------------------------

struct Queue {
    // A deque, so dropping the oldest event when full does not shift the rest.
    events: Mutex<VecDeque<Value>>,
    wake: Condvar,
}

fn queue() -> &'static Queue {
    static QUEUE: OnceLock<Queue> = OnceLock::new();
    QUEUE.get_or_init(|| Queue {
        events: Mutex::new(VecDeque::new()),
        wake: Condvar::new(),
    })
}

fn enqueue(event: Value) {
    let q = queue();
    let mut events = lock(&q.events);
    push_capped(&mut events, event);
    if events.len() >= 50 {
        q.wake.notify_one();
    }
}

/// Adds `event`, dropping the oldest first when the queue is full.
fn push_capped(events: &mut VecDeque<Value>, event: Value) {
    if events.len() >= MAX_QUEUE {
        events.pop_front();
    }
    events.push_back(event);
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
            Vec::from(std::mem::take(&mut *events))
        };
        send(&batch, agent());
    }
}

thread_local! {
    /// Set while this thread sends, so a panic inside the send is not sent.
    static SENDING: Cell<bool> = const { Cell::new(false) };
}

/// The request body: the events without the marker, which is ours alone.
fn payload(batch: &[&Value]) -> Value {
    let batch: Vec<Value> = batch
        .iter()
        .map(|e| {
            let mut e = (*e).clone();
            if let Some(o) = e.as_object_mut() {
                o.remove(FEEDBACK_MARK);
            }
            e
        })
        .collect();
    json!({"api_key": KEY, "batch": batch})
}

/// Posts `batch`; on failure the events are dropped, as reports are best effort.
fn send(batch: &[Value], agent: &ureq::Agent) {
    // The answer may have changed since these were queued.
    let now = current();
    let batch: Vec<&Value> = batch.iter().filter(|e| allowed(e, now)).collect();
    if batch.is_empty() {
        return;
    }
    let body = payload(&batch).to_string();
    SENDING.with(|s| s.set(true));
    let url = format!("{}/batch/", HOST.trim_end_matches('/'));
    let _ = crate::http::json_call(agent, "POST", &url, None, Some(body), 64 * 1024);
    SENDING.with(|s| s.set(false));
}

fn agent() -> &'static ureq::Agent {
    crate::http::agent(Duration::from_secs(10))
}

/// For a dying process or one quitting: it must not hang there.
fn quick_agent() -> &'static ureq::Agent {
    crate::http::agent(Duration::from_secs(2))
}

// ---- panics -----------------------------------------------------------------

/// Sends a panic right away, from the panicking thread, if the user agreed
/// to crash reports. `props` come from `panic_props`. Returns the event's id
/// when it was sent.
pub fn send_panic(props: Map<String, Value>) -> Option<String> {
    if CRASHES.load(Ordering::Relaxed) && !SENDING.with(Cell::get) {
        let event = event_json("$exception", props);
        let id = event["uuid"].as_str().map(str::to_string);
        send(&[event], quick_agent());
        return id;
    }
    None
}

/// What is known about the panic on this thread, for the crash log and for
/// PostHog. `fatal` is false when only a thread or a task died and the app
/// carried on (`guard`): that is an error, not a crash.
pub fn panic_props(info: &std::panic::PanicHookInfo<'_>, fatal: bool) -> Map<String, Value> {
    let message = info
        .payload()
        .downcast_ref::<&str>()
        .map(|s| s.to_string())
        .or_else(|| info.payload().downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "panic occurred".into());
    let home = home();
    let mut props = Map::new();
    if let Some(at) = info.location() {
        // A dependency's file is under the cargo home, in the user's home.
        let file = at.file().replacen(&home, "~", 1);
        props.insert("$exception_panic_file".into(), file.into());
        props.insert("$exception_panic_line".into(), at.line().into());
        props.insert("$exception_panic_column".into(), at.column().into());
    }
    if let Some(name) = std::thread::current().name() {
        props.insert("thread".into(), name.into());
    }
    let level = if fatal { "fatal" } else { "error" };
    props.insert("$exception_level".into(), level.into());
    props.insert(
        "$exception_list".into(),
        json!([{
            "type": "Panic",
            "value": scrub(&message, &home),
            "mechanism": {"type": "panic", "handled": !fatal, "synthetic": false},
            "stacktrace": {"type": "raw", "frames": frames(&home)},
        }]),
    );
    props
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
        // `key="value"`, as in JSON bodies: the quotes stay, the value goes.
        let inner = value.trim_start_matches(|c| "([{<\"'`".contains(c));
        let quotes = &value[..value.len() - inner.len()];
        let value = redact(inner)?;
        return Some(format!("{name}{sep}{quotes}{value}"));
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
    fn seen_events_keeps_names_even_when_nothing_is_sent() {
        capture_with("seen-events-probe", &[("secret", "x")], &[]);
        assert!(seen_events().contains("seen-events-probe"));
    }

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
            scrub(r#"token="ghp_0123456789abcdefghij" expired"#, HOME),
            r#"token="<secret>" expired"#
        );
        assert_eq!(
            scrub(
                r#"body {"api_key":"sk-ant-api03-AbCdEf123456789xyz"}"#,
                HOME
            ),
            r#"body {"api_key":"<secret>"}"#
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
    fn agent_errors_become_fixed_categories() {
        let cases = [
            (
                "Claude Code is not signed in. Run claude login",
                "not signed in",
            ),
            ("The turn failed: 429 Too Many Requests", "rate limited"),
            ("The turn failed: overloaded_error", "provider error"),
            ("initialize failed: request timed out", "timed out"),
            (
                "`droid` was not found on PATH or in ~/.local/bin",
                "agent not found",
            ),
            (
                "Claude Code's own binary is missing: /x is not there",
                "agent not found",
            ),
            (
                "npm was not found; it is needed once to install x",
                "npm not found",
            ),
            (
                "Droid exited (exit status: 1).\nat /srv/acme/x.rs",
                "process exited",
            ),
            (
                "Claude Code exited (exit status: 127).\nenv: node: No such file or directory",
                "process exited",
            ),
            (
                "starting /usr/bin/droid exec: No such file or directory (os error 2)",
                "failed to start",
            ),
            ("The turn failed: model gpt-9 not found", "turn failed"),
            (
                "Starting the session failed: Session not found",
                "session start failed",
            ),
            ("Codex stopped responding.", "stopped responding"),
            ("initialize failed: bad version", "initialize failed"),
            (
                "Starting the session failed: Project Falcon is archived",
                "session start failed",
            ),
            ("The turn failed: model said no", "turn failed"),
            ("SecretProject broke", "other"),
        ];
        for (message, want) in cases {
            assert_eq!(category(message), want, "{message}");
        }
    }

    #[test]
    fn events_follow_their_own_choice() {
        let crash = json!({"event": "$exception"});
        let usage = json!({"event": "app launched"});
        let only_crashes = Consent {
            crashes: true,
            usage: false,
        };
        assert!(allowed(&crash, only_crashes));
        assert!(!allowed(&usage, only_crashes));
        assert!(!allowed(&crash, Consent::NONE));
    }

    #[test]
    fn user_sends_without_usage() {
        for src in [Source::User, Source::Error] {
            for consent in [Consent::NONE, Consent::ALL] {
                assert_eq!(feedback_decision(src, Blocked::No, consent), Ok(()));
            }
        }
    }

    #[test]
    fn agent_needs_usage() {
        for src in [Source::Agent, Source::Popover] {
            let crashes_only = Consent {
                crashes: true,
                usage: false,
            };
            assert_eq!(
                feedback_decision(src, Blocked::No, Consent::NONE),
                Err(Refused::NoUsage)
            );
            assert_eq!(
                feedback_decision(src, Blocked::No, crashes_only),
                Err(Refused::NoUsage)
            );
            assert_eq!(feedback_decision(src, Blocked::No, Consent::ALL), Ok(()));
        }
    }

    #[test]
    fn blocked_refuses_all() {
        for src in [Source::User, Source::Error, Source::Agent, Source::Popover] {
            assert_eq!(
                feedback_decision(src, Blocked::Env, Consent::ALL),
                Err(Refused::Blocked)
            );
            assert_eq!(
                feedback_decision(src, Blocked::Dev, Consent::ALL),
                Err(Refused::DevBuild)
            );
        }
    }

    #[test]
    fn feedback_is_refused_in_tests() {
        assert_eq!(feedback_allowed(Source::User), Err(Refused::DevBuild));
    }

    fn marked(src: Source) -> Value {
        let mut e = event_json(events::SURVEY_SENT, Map::new());
        e[FEEDBACK_MARK] = src.as_str().into();
        e
    }

    #[test]
    fn feedback_event_passes_send_filter() {
        for src in [Source::User, Source::Error] {
            assert!(allowed(&marked(src), Consent::NONE), "{src:?}");
        }
        for src in [Source::Agent, Source::Popover] {
            assert!(!allowed(&marked(src), Consent::NONE), "{src:?}");
            assert!(allowed(&marked(src), Consent::ALL), "{src:?}");
        }
    }

    #[test]
    fn unmarked_user_response_is_filtered_without_usage() {
        let props = json!({"source": "user"}).as_object().unwrap().clone();
        let e = event_json(events::SURVEY_SENT, props);
        assert!(!allowed(&e, Consent::NONE));
    }

    #[test]
    fn capture_cannot_send_survey_sent() {
        USAGE.store(true, Ordering::Relaxed);
        let r = std::panic::catch_unwind(|| {
            capture(
                events::SURVEY_SENT,
                &[("source", "user"), ("tag", "cap-test")],
            );
        });
        USAGE.store(false, Ordering::Relaxed);
        let _ = r;
        let found = lock(&queue().events)
            .iter()
            .any(|e| e["properties"]["tag"] == "cap-test");
        assert!(!found);
    }

    #[test]
    fn send_feedback_source_comes_from_parameter() {
        let props = json!({"source": "user"}).as_object().unwrap().clone();
        // In tests everything is refused as a dev build, so decide directly.
        let e = feedback_event(events::SURVEY_SENT, Source::Agent, props);
        assert_eq!(e["properties"]["source"], "agent");
        assert_eq!(e[FEEDBACK_MARK], "agent");
        assert!(!allowed(&e, Consent::NONE));
    }

    #[test]
    fn marker_never_reaches_the_payload() {
        let batch = [marked(Source::User)];
        let refs: Vec<&Value> = batch.iter().collect();
        let body = payload(&refs);
        assert!(!body.to_string().contains(FEEDBACK_MARK));
        assert!(batch[0].get(FEEDBACK_MARK).is_some());
    }

    #[test]
    fn events_have_a_v4_uuid() {
        let id = uuid_v4();
        let b = id.as_bytes();
        assert_eq!(id.len(), 36);
        assert_eq!(b[14], b'4');
        assert!(matches!(b[19], b'8' | b'9' | b'a' | b'b'));
        assert_ne!(id, uuid_v4());
        assert_eq!(
            event_json("x", Map::new())["uuid"].as_str().map(str::len),
            Some(36)
        );
    }

    #[test]
    fn error_returns_event_uuid() {
        assert_eq!(error("AgentError", "boom", &[]), None);
        CRASHES.store(true, Ordering::Relaxed);
        let id = error("AgentError", "boom", &[]);
        CRASHES.store(false, Ordering::Relaxed);
        let id = id.expect("sent");
        assert_eq!(id.len(), 36);
        // Take it back out: the queue is shared with other tests.
        let mut events = lock(&queue().events);
        let before = events.len();
        events.retain(|e| e["uuid"] != id.as_str());
        assert_eq!(events.len(), before - 1);
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
            "arakun::crash::install::{{closure}}",
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
    fn timestamps_are_rfc3339_utc() {
        assert_eq!(timestamp(0), "1970-01-01T00:00:00.000Z");
        assert_eq!(timestamp(1_763_390_096_789), "2025-11-17T14:34:56.789Z");
        assert_eq!(timestamp(951_782_400_000), "2000-02-29T00:00:00.000Z");
    }

    #[test]
    fn held_events_go_out_only_after_a_yes() {
        // The only test that touches the held list and the queue.
        let event = |n: u64| json!({"event": "app launched", "n": n});
        hold(event(1));
        release_held(false);
        assert!(lock(held()).is_empty());
        assert!(lock(&queue().events).is_empty());

        hold(event(2));
        release_held(true);
        assert!(lock(held()).is_empty());
        let queued = Vec::from(std::mem::take(&mut *lock(&queue().events)));
        assert_eq!(queued, vec![event(2)]);
        assert!(!HOLDING.load(Ordering::Relaxed));
    }

    #[test]
    fn a_full_queue_drops_the_oldest_event() {
        let event = |n: usize| json!({"event": "app launched", "n": n});
        let mut events = VecDeque::new();
        for n in 0..MAX_QUEUE + 2 {
            push_capped(&mut events, event(n));
        }
        assert_eq!(events.len(), MAX_QUEUE);
        assert_eq!(events.front(), Some(&event(2)));
        assert_eq!(events.back(), Some(&event(MAX_QUEUE + 1)));
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
