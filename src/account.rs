//! The Factory account's plan usage: the same limits `droid` checks before every
//! turn (5-hour, weekly and monthly windows of Standard Usage, the Droid Core
//! pool, and the Extra Usage balance), read from `GET /api/billing/limits` with a
//! Factory API key. The key lives in the desktop keyring (Secret Service via
//! `secret-tool` on Linux, Credential Manager on Windows), or comes from `FACTORY_API_KEY`.

use std::io::Write as _;
use std::process::Stdio;
use std::time::{Duration, Instant};

use gpui::{AppContext as _, Context, EventEmitter, SharedString, Task};
use serde_json::Value;

use crate::platform::keyring;

pub fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

pub const API_BASE: &str = "https://api.factory.ai";
pub const USAGE_URL: &str = "https://app.factory.ai/settings/usage";
pub const API_KEYS_URL: &str = "https://app.factory.ai/settings/api-keys";

const KEYRING_ACCOUNT: &str = "factory-api-key";

#[derive(Clone, Copy, Debug, Default)]
pub struct Bucket {
    pub used_percent: f32,
    /// Unix seconds; a bucket whose window already ended has been reset.
    pub window_end: Option<u64>,
}

impl Bucket {
    pub fn active(&self, now: u64) -> bool {
        self.window_end.is_some_and(|end| end > now)
    }

    pub fn exhausted(&self, now: u64) -> bool {
        self.active(now) && self.used_percent >= 100.
    }

    /// What the bucket reads right now: a window that ended is back to zero.
    pub fn current(&self, now: u64) -> f32 {
        if self.active(now) {
            self.used_percent.max(0.)
        } else {
            0.
        }
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct Pool {
    pub five_hour: Bucket,
    pub weekly: Bucket,
    pub monthly: Bucket,
}

impl Pool {
    pub fn buckets(&self) -> [(&'static str, Bucket); 3] {
        [
            ("5 hours", self.five_hour),
            ("Week", self.weekly),
            ("Month", self.monthly),
        ]
    }

    pub fn exhausted(&self, now: u64) -> bool {
        self.buckets().iter().any(|(_, b)| b.exhausted(now))
    }

    /// The fullest bucket that is still running.
    pub fn highest(&self, now: u64) -> Option<(&'static str, f32)> {
        self.buckets()
            .into_iter()
            .filter(|(_, b)| b.active(now))
            .map(|(l, b)| (l, b.used_percent))
            .max_by(|a, b| a.1.total_cmp(&b.1))
    }

    /// When the earliest exhausted window reopens.
    pub fn reset_at(&self, now: u64) -> Option<u64> {
        self.buckets()
            .iter()
            .filter(|(_, b)| b.exhausted(now))
            .filter_map(|(_, b)| b.window_end)
            .min()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UsageMode {
    Standard,
    /// Standard is used up and the account fell back to Droid Core models.
    DroidCore,
    /// Standard is used up and turns are paid from the Extra Usage balance.
    ExtraUsage,
    /// Standard is used up and no fallback is chosen: droid will refuse turns.
    Blocked,
}

#[derive(Clone, Debug, Default)]
pub struct Limits {
    pub standard: Option<Pool>,
    pub core: Option<Pool>,
    pub extra_usage_cents: i64,
    pub overage_preference: Option<String>,
}

impl Limits {
    fn parse(v: &Value) -> Self {
        let bucket = |b: &Value| Bucket {
            used_percent: b["usedPercent"].as_f64().unwrap_or(0.) as f32,
            window_end: b["windowEnd"].as_str().and_then(parse_rfc3339),
        };
        let pool = |p: &Value| {
            p.is_object().then(|| Pool {
                five_hour: bucket(&p["fiveHour"]),
                weekly: bucket(&p["weekly"]),
                monthly: bucket(&p["monthly"]),
            })
        };
        Self {
            standard: pool(&v["limits"]["standard"]),
            core: pool(&v["limits"]["core"]),
            extra_usage_cents: v["extraUsageBalanceCents"].as_i64().unwrap_or(0),
            overage_preference: v["overagePreference"].as_str().map(str::to_string),
        }
    }

    pub fn mode(&self, now: u64) -> UsageMode {
        if !self.standard.is_some_and(|p| p.exhausted(now)) {
            return UsageMode::Standard;
        }
        match self.overage_preference.as_deref() {
            Some("extraUsage") => UsageMode::ExtraUsage,
            Some("droidCore") => UsageMode::DroidCore,
            _ => UsageMode::Blocked,
        }
    }

    /// The headline: the fullest running window of the pool in use.
    pub fn headline(&self, now: u64) -> Option<(&'static str, f32)> {
        match self.mode(now) {
            UsageMode::DroidCore => self.core.and_then(|p| p.highest(now)),
            UsageMode::ExtraUsage => None,
            _ => self.standard.and_then(|p| p.highest(now)),
        }
    }

    /// One sentence, worded like droid's own usage notice.
    pub fn summary(&self, now: u64) -> String {
        let resets = self
            .standard
            .and_then(|p| p.reset_at(now))
            .map(|t| format!(" Standard resets {}.", when(t, now)))
            .unwrap_or_default();
        match self.mode(now) {
            UsageMode::Blocked => {
                format!(
                    "Standard Usage limit reached; droid will stop until you pick Droid Core or Extra Usage.{resets}"
                )
            }
            UsageMode::Standard => match self.headline(now) {
                Some((label, pct)) if pct >= 70. => {
                    format!(
                        "You are using Standard Usage, but the {} window is at {:.0}%.",
                        label.to_lowercase(),
                        pct.ceil()
                    )
                }
                _ => "You are using Standard Usage.".into(),
            },
            UsageMode::DroidCore => format!("You are using Droid Core.{resets}"),
            UsageMode::ExtraUsage => format!(
                "You are using Extra Usage: ${:.2} remaining.{resets}",
                self.extra_usage_cents as f64 / 100.
            ),
        }
    }

    /// The next moment the numbers change on their own (a window rolling over).
    fn next_rollover(&self, now: u64) -> Option<u64> {
        [self.standard, self.core]
            .into_iter()
            .flatten()
            .flat_map(|p| p.buckets())
            .filter_map(|(_, b)| b.window_end)
            .filter(|&end| end > now)
            .min()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KeySource {
    Env,
    Keyring,
    /// droid's own login, read from `~/.factory`; droid itself keeps it fresh.
    Droid,
}

pub enum AccountEvent {
    Connected,
    ConnectFailed,
}

pub struct Account {
    key: Option<(String, KeySource)>,
    key_loaded: bool,
    limits: Option<Limits>,
    fetched: Option<Instant>,
    fetched_unix: u64,
    error: Option<SharedString>,
    busy: bool,
    _fetch: Option<Task<()>>,
    _rollover: Option<Task<()>>,
}

impl EventEmitter<AccountEvent> for Account {}

impl Account {
    pub fn new(cx: &mut Context<Self>) -> Self {
        cx.spawn(async move |this, cx| {
            let key = cx.background_spawn(async move { load_key() }).await;
            let _ = this.update(cx, |this, cx| {
                this.key = key;
                this.key_loaded = true;
                this.refresh(cx);
                cx.notify();
            });
        })
        .detach();
        Self {
            key: None,
            key_loaded: false,
            limits: None,
            fetched: None,
            fetched_unix: 0,
            error: None,
            busy: false,
            _fetch: None,
            _rollover: None,
        }
    }

    pub fn ready(&self) -> bool {
        self.key_loaded
    }

    pub fn connected(&self) -> bool {
        self.key.is_some()
    }

    pub fn key_source(&self) -> Option<KeySource> {
        self.key.as_ref().map(|(_, s)| *s)
    }

    pub fn limits(&self) -> Option<&Limits> {
        self.limits.as_ref()
    }

    pub fn error(&self) -> Option<&SharedString> {
        self.error.as_ref()
    }

    pub fn busy(&self) -> bool {
        self.busy
    }

    /// Unix time of the last successful read.
    pub fn fetched_at(&self) -> Option<u64> {
        self.fetched.map(|_| self.fetched_unix)
    }

    pub fn refresh(&mut self, cx: &mut Context<Self>) {
        let Some((key, source)) = self.key.clone() else {
            return;
        };
        if self.busy {
            return;
        }
        self.busy = true;
        cx.notify();
        self._fetch = Some(cx.spawn(async move |this, cx| {
            let result = cx
                .background_spawn(async move {
                    if source != KeySource::Droid {
                        return fetch_limits(&key, source);
                    }
                    // droid rotates its token every few hours; re-read it each fetch.
                    let key = droid_session().unwrap_or(key);
                    let result = if jwt_expired(&key) {
                        Err(String::new())
                    } else {
                        fetch_limits(&key, source)
                    };
                    match result {
                        Err(e) if (e.is_empty() || e == DROID_REJECTED) && droid_renew() => {
                            fetch_limits(&droid_session().unwrap_or(key), source)
                        }
                        Err(e) if e.is_empty() => Err(DROID_REJECTED.into()),
                        other => other,
                    }
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                this.busy = false;
                this.apply(result, cx);
                cx.notify();
            });
        }));
    }

    /// Re-reads unless the numbers are younger than `max_age`; cheap enough to
    /// call after every agent turn.
    pub fn refresh_if_stale(&mut self, max_age: Duration, cx: &mut Context<Self>) {
        if self.fetched.is_some_and(|t| t.elapsed() < max_age) {
            return;
        }
        self.refresh(cx);
    }

    /// Checks the key against the API first, so a typo never replaces a working key.
    pub fn connect(&mut self, key: String, cx: &mut Context<Self>) {
        let key = key.trim().to_string();
        if !key_ok(&key) {
            self.error = Some("That doesn't look like a Factory API key.".into());
            cx.emit(AccountEvent::ConnectFailed);
            cx.notify();
            return;
        }
        self.busy = true;
        self.error = None;
        cx.notify();
        self._fetch = Some(cx.spawn(async move |this, cx| {
            let k = key.clone();
            let result = cx
                .background_spawn(async move {
                    let limits = fetch_limits(&k, KeySource::Keyring)?;
                    keyring_store(&k)
                        .map_err(|e| format!("Could not save the key in the keyring: {e}"))?;
                    Ok(limits)
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                this.busy = false;
                let ok = result.is_ok();
                if ok {
                    this.key = Some((key, KeySource::Keyring));
                }
                this.apply(result, cx);
                cx.emit(if ok {
                    AccountEvent::Connected
                } else {
                    AccountEvent::ConnectFailed
                });
                cx.notify();
            });
        }));
    }

    pub fn disconnect(&mut self, cx: &mut Context<Self>) {
        if self.key_source() != Some(KeySource::Keyring) {
            return;
        }
        cx.background_spawn(async move { keyring_clear() }).detach();
        self.key = None;
        self.limits = None;
        self.fetched = None;
        self.error = None;
        self._fetch = None;
        self._rollover = None;
        cx.notify();
    }

    fn apply(&mut self, result: Result<Limits, String>, cx: &mut Context<Self>) {
        match result {
            Ok(limits) => {
                self.error = None;
                self.fetched = Some(Instant::now());
                self.fetched_unix = unix_now();
                self.schedule_rollover(&limits, cx);
                self.limits = Some(limits);
            }
            Err(e) => self.error = Some(e.into()),
        }
    }

    /// One timer for the next window reset, so a bar that just emptied shows it
    /// without polling.
    fn schedule_rollover(&mut self, limits: &Limits, cx: &mut Context<Self>) {
        let now = unix_now();
        self._rollover = limits.next_rollover(now).map(|end| {
            let wait = Duration::from_secs(end.saturating_sub(now) + 5);
            cx.spawn(async move |this, cx| {
                cx.background_executor().timer(wait).await;
                let _ = this.update(cx, |this, cx| this.refresh(cx));
            })
        });
    }
}

fn key_ok(key: &str) -> bool {
    key.len() >= 16
        && key.len() <= 512
        && key
            .bytes()
            .all(|b| b.is_ascii_graphic() && b != b'"' && b != b'\\')
}

/// `ARAKUN_USAGE_FIXTURE=<file.json>` stands in for the API, to look at the UI offline.
fn fixture() -> Option<std::path::PathBuf> {
    std::env::var_os("ARAKUN_USAGE_FIXTURE").map(Into::into)
}

fn load_key() -> Option<(String, KeySource)> {
    if fixture().is_some() {
        return Some(("fixture-key-0000000000".into(), KeySource::Env));
    }
    if let Ok(k) = std::env::var("FACTORY_API_KEY") {
        let k = k.trim().to_string();
        if key_ok(&k) {
            return Some((k, KeySource::Env));
        }
    }
    if let Some(k) = keyring::lookup(KEYRING_ACCOUNT)
        && key_ok(&k)
    {
        return Some((k, KeySource::Keyring));
    }
    droid_session().map(|token| (token, KeySource::Droid))
}

/// droid's own login token, so plan limits work without a separate API key.
/// `~/.factory/auth.v2.keyring` is AES-256-GCM (`iv:tag:ciphertext`, base64);
/// the key sits in the desktop keyring under "Factory CLI". With the keyring
/// disabled, droid uses `auth.v2.file` + `auth.v2.key` instead. Strictly
/// read-only: droid owns the file and renews the token itself.
fn droid_session() -> Option<String> {
    let home = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE"))?;
    let dir = std::path::PathBuf::from(home).join(".factory");
    let read = |name: &str| std::fs::read_to_string(dir.join(name)).ok();
    let json = read("auth.v2.keyring")
        .and_then(|raw| {
            droid_decrypt(
                &raw,
                &keyring::lookup_in("Factory CLI", "auth-encryption-key")?,
            )
        })
        .or_else(|| droid_decrypt(&read("auth.v2.file")?, &read("auth.v2.key")?));
    let creds: Value = serde_json::from_str(&json?).ok()?;
    let token = creds["access_token"].as_str()?.to_string();
    (token.split('.').count() == 3).then_some(token)
}

const DROID_REJECTED: &str = "Droid's login was rejected; open droid once to renew it.";

/// True once the JWT's `exp` is within a minute (or unreadable).
fn jwt_expired(token: &str) -> bool {
    use base64::Engine as _;
    let exp = token
        .split('.')
        .nth(1)
        .and_then(|p| {
            base64::engine::general_purpose::URL_SAFE_NO_PAD
                .decode(p.trim_end_matches('='))
                .ok()
        })
        .and_then(|b| serde_json::from_slice::<Value>(&b).ok())
        .and_then(|v| v["exp"].as_u64());
    exp.is_none_or(|exp| exp <= unix_now() + 60)
}

/// Has droid renew its own login: `droid exec --list-tools` refreshes the token
/// during startup and exits before any model call. Never refresh it ourselves:
/// that rotates the refresh token and logs droid out. At most once per 10 min,
/// so a dead login doesn't spawn droid after every turn. True if the token changed.
fn droid_renew() -> bool {
    use std::sync::atomic::{AtomicU64, Ordering};
    static LAST: AtomicU64 = AtomicU64::new(0);
    let now = unix_now();
    if now.saturating_sub(LAST.load(Ordering::Relaxed)) < 600 {
        return false;
    }
    LAST.store(now, Ordering::Relaxed);
    let Ok(bin) = crate::agent::droid_bin() else {
        return false;
    };
    let before = droid_session();
    let ran = crate::platform::command(bin)
        .args(["exec", "--list-tools"])
        .current_dir(std::env::temp_dir())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success());
    ran && droid_session().is_some_and(|t| Some(&t) != before.as_ref() && !jwt_expired(&t))
}

fn droid_decrypt(raw: &str, key_b64: &str) -> Option<String> {
    use aes_gcm::aead::{Aead as _, KeyInit as _, consts::U16};
    use base64::Engine as _;
    // droid encrypts with a 16-byte IV, not GCM's usual 12.
    type Aes256Gcm16 = aes_gcm::AesGcm<aes_gcm::aes::Aes256, U16>;
    let b64 = base64::engine::general_purpose::STANDARD;
    let key = b64.decode(key_b64.trim()).ok()?;
    let mut parts = raw.trim().split(':');
    let nonce = b64.decode(parts.next()?).ok()?;
    let tag = b64.decode(parts.next()?).ok()?;
    let mut data = b64.decode(parts.next()?).ok()?;
    let nonce: [u8; 16] = nonce.try_into().ok()?;
    if tag.len() != 16 {
        return None;
    }
    // Rust's aes-gcm wants the tag appended to the ciphertext.
    data.extend_from_slice(&tag);
    let cipher = Aes256Gcm16::new_from_slice(&key).ok()?;
    let plain = cipher
        .decrypt(&aes_gcm::Nonce::<U16>::from(nonce), data.as_ref())
        .ok()?;
    String::from_utf8(plain).ok()
}

fn keyring_store(key: &str) -> Result<(), String> {
    keyring::store(KEYRING_ACCOUNT, "arakun: Factory API key", key)
}

fn keyring_clear() {
    keyring::clear(KEYRING_ACCOUNT);
}

/// The key goes to curl through a config file on stdin, never on its command line.
fn fetch_limits(key: &str, source: KeySource) -> Result<Limits, String> {
    if let Some(path) = fixture() {
        let text = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
        let v: Value = serde_json::from_str(&text).map_err(|e| e.to_string())?;
        return Ok(Limits::parse(&v));
    }
    let url = format!("{API_BASE}/api/billing/limits");
    crate::agent::cli_login::check(crate::agent::AgentKind::Droid, &url)
        .map_err(|e| e.to_string())?;
    let mut child = crate::platform::command("curl")
        .args(["-sS", "--max-time", "15", "-K", "-", "-w", "\n%{http_code}"])
        .arg(url)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("curl: {e}"))?;
    let config = format!(
        "header = \"Authorization: Bearer {key}\"\nheader = \"Accept: application/json\"\n"
    );
    child
        .stdin
        .take()
        .ok_or("curl: no stdin")?
        .write_all(config.as_bytes())
        .map_err(|e| e.to_string())?;
    let out = child.wait_with_output().map_err(|e| e.to_string())?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        return Err(format!(
            "Factory is unreachable ({})",
            err.trim().trim_start_matches("curl: ")
        ));
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let (body, code) = text.rsplit_once('\n').unwrap_or(("", &text));
    match code.trim() {
        "200" => {}
        "401" | "403" => {
            return Err(match source {
                KeySource::Droid => DROID_REJECTED.into(),
                _ => "Factory rejected the API key.".into(),
            });
        }
        c => return Err(format!("Factory answered HTTP {c}.")),
    }
    let v: Value =
        serde_json::from_str(body).map_err(|_| "Unexpected answer from Factory.".to_string())?;
    let limits = Limits::parse(&v);
    if limits.standard.is_none() {
        return Err("Factory sent no plan limits for this account.".into());
    }
    Ok(limits)
}

/// `2026-10-13T22:00:00.000Z` (or with a `±hh:mm` offset) → unix seconds.
pub(crate) fn parse_rfc3339(s: &str) -> Option<u64> {
    let b = s.as_bytes();
    if b.len() < 19 || b[4] != b'-' || b[7] != b'-' || !matches!(b[10], b'T' | b't' | b' ') {
        return None;
    }
    let num = |r: std::ops::Range<usize>| s.get(r)?.parse::<i64>().ok();
    let (y, mo, d) = (num(0..4)?, num(5..7)?, num(8..10)?);
    let (h, mi, sec) = (num(11..13)?, num(14..16)?, num(17..19)?);
    let mut rest = &s[19..];
    if let Some(r) = rest.strip_prefix('.') {
        rest = r.trim_start_matches(|c: char| c.is_ascii_digit());
    }
    let offset = match rest.as_bytes().first() {
        None | Some(b'Z' | b'z') => 0,
        Some(&sign @ (b'+' | b'-')) => {
            let oh = rest.get(1..3)?.parse::<i64>().ok()?;
            let om = rest.get(4..6)?.parse::<i64>().ok()?;
            let o = oh * 3600 + om * 60;
            if sign == b'+' { o } else { -o }
        }
        _ => return None,
    };
    let days = days_from_civil(y, mo, d);
    let t = days * 86400 + h * 3600 + mi * 60 + sec - offset;
    u64::try_from(t).ok()
}

/// Howard Hinnant's `days_from_civil`.
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * (m + if m > 2 { -3 } else { 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe - 719468
}

/// "in 3h 12m" within a day, otherwise the local date ("Oct 20").
pub fn when(t: u64, now: u64) -> String {
    let left = t.saturating_sub(now);
    if left < 86400 {
        let (h, m) = (left / 3600, left % 3600 / 60);
        return if h > 0 {
            format!("in {h}h {m}m")
        } else {
            format!("in {}m", m.max(1))
        };
    }
    const MONTHS: [&str; 12] = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    let Some(tm) = crate::schedule::local(t) else {
        return format!("in {}d", left / 86400);
    };
    format!("{} {}", MONTHS[tm.month.clamp(0, 11) as usize], tm.day)
}

/// "just now", "4m ago", "2h ago".
pub fn ago(t: u64, now: u64) -> String {
    let d = now.saturating_sub(t);
    match d {
        0..60 => "just now".into(),
        60..3600 => format!("{}m ago", d / 60),
        _ => format!("{}h ago", d / 3600),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn jwt_expiry() {
        use base64::Engine as _;
        let jwt = |exp: u64| {
            let p = base64::engine::general_purpose::URL_SAFE_NO_PAD
                .encode(format!("{{\"exp\":{exp}}}"));
            format!("h.{p}.s")
        };
        assert!(!jwt_expired(&jwt(unix_now() + 3600)));
        assert!(jwt_expired(&jwt(unix_now() - 1)));
        assert!(jwt_expired("not-a-jwt"));
    }

    #[test]
    fn droid_decrypt_roundtrip() {
        use aes_gcm::aead::{Aead as _, KeyInit as _};
        use base64::Engine as _;
        let b64 = base64::engine::general_purpose::STANDARD;
        use aes_gcm::aead::consts::U16;
        let key = [7u8; 32];
        let nonce = [3u8; 16];
        let cipher = aes_gcm::AesGcm::<aes_gcm::aes::Aes256, U16>::new_from_slice(&key).unwrap();
        let sealed = cipher
            .encrypt(
                &aes_gcm::Nonce::<U16>::from(nonce),
                br#"{"access_token":"a.b.c"}"#.as_ref(),
            )
            .unwrap();
        // droid writes iv:tag:ciphertext, each base64; aes-gcm returns ct||tag.
        let (ct, tag) = sealed.split_at(sealed.len() - 16);
        let raw = format!(
            "{}:{}:{}",
            b64.encode(nonce),
            b64.encode(tag),
            b64.encode(ct)
        );
        let json = droid_decrypt(&raw, &b64.encode(key)).unwrap();
        assert!(json.contains("a.b.c"));
        assert!(droid_decrypt(&raw, "bad key").is_none());
    }

    /// The real thing: this machine's droid login. Run with -- --ignored.
    #[test]
    #[ignore]
    fn droid_session_live() {
        let token = droid_session().expect("droid login unreadable");
        assert!(token.split('.').count() == 3);
        let limits = fetch_limits(&token, KeySource::Droid).expect("limits fetch failed");
        assert!(limits.standard.is_some());
    }

    #[test]
    fn rfc3339() {
        assert_eq!(parse_rfc3339("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(parse_rfc3339("2026-10-13T22:00:00.000Z"), Some(1791928800));
        assert_eq!(parse_rfc3339("2026-10-14T00:00:00+02:00"), Some(1791928800));
        assert_eq!(parse_rfc3339("nope"), None);
    }

    #[test]
    fn modes() {
        let v: Value = serde_json::json!({
            "limits": {
                "standard": {
                    "fiveHour": {"usedPercent": 100.0, "windowEnd": "2999-01-01T00:00:00Z"},
                    "weekly": {"usedPercent": 40.0, "windowEnd": "2999-01-02T00:00:00Z"},
                    "monthly": {"usedPercent": 10.0, "windowEnd": "1999-01-01T00:00:00Z"}
                }
            },
            "extraUsageBalanceCents": 1234,
            "overagePreference": "extraUsage"
        });
        let l = Limits::parse(&v);
        let now = 1_800_000_000;
        assert_eq!(l.mode(now), UsageMode::ExtraUsage);
        let p = l.standard.unwrap();
        assert_eq!(p.monthly.current(now), 0.);
        assert_eq!(p.highest(now), Some(("5 hours", 100.)));
        assert!(l.summary(now).contains("$12.34"));
    }
}
