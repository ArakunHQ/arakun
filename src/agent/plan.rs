//! The claude.ai plan behind Claude Code: what each usage window (5 hours,
//! weekly per model, monthly extra credits) has left, from the same OAuth
//! endpoint Claude Code's `/usage` command reads. Live sessions only report a
//! window when it gets near (`agent::thread::PlanLimit`); this shows the whole
//! picture any time.
//!
//! Two seats can be on screen at once: the machine's own claude.ai sign-in and
//! a long-lived token (`claude setup-token`) Claude Code runs on, whose windows
//! only the rate-limit headers of a one-token call report.

use std::io::Write as _;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context as _, Result, anyhow, bail};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::{AgentKind, cli_login};

/// Also read through CLIProxyAPI (`cliproxy`), with each account's own token.
pub(crate) const ENDPOINT: &str = "https://api.anthropic.com/api/oauth/usage";
/// A long-lived token can't read [`ENDPOINT`]; a one-token message can, from
/// the `anthropic-ratelimit-unified-*` headers of its answer.
const MESSAGES: &str = "https://api.anthropic.com/v1/messages";
const PROBE_MODEL: &str = "claude-haiku-5-5";
/// Refetch at most this often; `refresh` serves the cache in between.
const STALE: u64 = 5 * 60;
/// The borrowed seat's windows cost a one-token message (a few dozen tokens of
/// that seat), so they are read less often than the sign-in's.
const BORROWED_STALE: u64 = 15 * 60;

/// One fetch of the account's plan usage.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct PlanUsage {
    /// Unix seconds when this was fetched.
    pub at: u64,
    /// The plan kind, e.g. "team"; empty when unknown.
    pub subscription: String,
    /// e.g. "default_claude_max_5x"; empty when unknown.
    pub tier: String,
    pub windows: Vec<Window>,
    /// Monthly extra-usage credits, when the account has them enabled.
    pub spend: Option<Spend>,
    /// Claude Code runs on a `CLAUDE_CODE_OAUTH_TOKEN` (another seat's
    /// long-lived token), not on the claude.ai sign-in: the windows are that
    /// token's, read from the rate-limit headers of a one-token call.
    #[serde(default)]
    pub borrowed: bool,
}

/// One usage window of the plan: the 5-hour session limit, a weekly limit
/// (sometimes per model), or any other window the endpoint reports.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Window {
    /// "5-hour limit", "Weekly · Fable", ...
    pub label: String,
    /// 0-100, how full the window is.
    pub percent: u64,
    /// "normal" unless the account is near or over the limit.
    pub severity: String,
    /// Unix seconds when the window starts over.
    pub resets_at: Option<u64>,
    /// False when the window does not apply right now.
    pub active: bool,
}

/// Extra usage billed as credits beyond the plan's windows.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Spend {
    pub used: f64,
    pub limit: Option<f64>,
    /// 0-100.
    pub percent: u64,
    pub currency: String,
}

/// Both seats this machine can see at once: its own claude.ai sign-in and a
/// long-lived ("borrowed") token, the one Claude Code is running on.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Seats {
    /// Unix seconds when the sign-in's plan was fetched.
    #[serde(default)]
    pub at: u64,
    /// The claude.ai sign-in's plan; `None` when signed out or the fetch failed.
    #[serde(default)]
    pub own: Option<PlanUsage>,
    /// Unix seconds when the borrowed seat's windows were read.
    #[serde(default)]
    pub borrowed_at: u64,
    /// The borrowed seat's windows; `None` when there is no token or it failed.
    #[serde(default)]
    pub borrowed: Option<PlanUsage>,
}

impl Seats {
    /// The seat Claude Code runs on right now.
    pub fn active(&self) -> Option<&PlanUsage> {
        self.in_use(active_is_borrowed())
    }

    /// Kept apart from [`Self::active`] so tests don't depend on which account
    /// this machine happens to be signed in with.
    fn in_use(&self, borrowed: bool) -> Option<&PlanUsage> {
        if borrowed {
            self.borrowed.as_ref()
        } else {
            self.own.as_ref()
        }
    }
}

impl Window {
    /// "5h", "Fable", "Week": the window's name where rows are narrow.
    pub fn short_label(&self) -> String {
        if self.label == "5-hour limit" {
            "5h".into()
        } else if self.label == "Weekly limit" {
            "Week".into()
        } else if self.label == "Monthly limit" {
            "Month".into()
        } else {
            self.label
                .strip_prefix("Weekly · ")
                .unwrap_or(&self.label)
                .to_string()
        }
    }

    /// Near or over the limit: worth drawing in red.
    pub fn hot(&self) -> bool {
        self.severity != "normal" || self.percent >= 80
    }

    /// "resets in 2h 14m", "resets Tue 21:00", "resets Dec 2" a week or more
    /// out; "resetting…" once past due.
    pub fn reset_text(&self, now: u64) -> Option<String> {
        let at = self.resets_at?;
        if at <= now {
            return Some("resetting…".into());
        }
        let delta = at - now;
        if delta < 90 * 60 {
            return Some(format!("resets in {}m", delta.div_ceil(60)));
        }
        if delta < 36 * 3600 {
            return Some(format!(
                "resets in {}h {:02}m",
                delta / 3600,
                delta % 3600 / 60
            ));
        }
        let t = crate::platform::local_time(at)?;
        if delta >= 7 * 86_400 {
            return Some(format!(
                "resets {} {}",
                MONTH[t.month.rem_euclid(12) as usize],
                t.day
            ));
        }
        Some(format!(
            "resets {} {:02}:{:02}",
            WEEKDAY[t.wday.rem_euclid(7) as usize],
            t.hour,
            t.minute
        ))
    }
}

const WEEKDAY: [&str; 7] = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
const MONTH: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];

impl PlanUsage {
    /// "Team · Max 5x" for the settings list; empty when nothing is known.
    pub fn plan_label(&self) -> String {
        if self.borrowed {
            return "Borrowed token".into();
        }
        let sub = match self.subscription.as_str() {
            "" => String::new(),
            s => {
                let mut c = s.chars();
                c.next()
                    .map_or_else(String::new, |f| f.to_uppercase().collect())
                    + c.as_str()
            }
        };
        let tier = self
            .tier
            .strip_prefix("default_claude_")
            .unwrap_or(&self.tier)
            .replace('_', " ");
        let tier = if tier.is_empty() {
            tier
        } else {
            let mut c = tier.chars();
            c.next()
                .map_or_else(String::new, |f| f.to_uppercase().collect())
                + c.as_str()
        };
        [sub, tier]
            .into_iter()
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
            .join(" · ")
    }

    /// "5h 12% · Weekly 0%", one percentage per window, for compact rows.
    pub fn windows_summary(&self) -> String {
        self.windows
            .iter()
            .map(|w| {
                let short = w
                    .label
                    .replace("5-hour limit", "5h")
                    .replace("Weekly · ", "")
                    .replace("Weekly limit", "Weekly");
                format!("{short} {}%", w.percent)
            })
            .collect::<Vec<_>>()
            .join(" · ")
    }
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

fn credentials_path() -> PathBuf {
    crate::platform::home()
        .join(".claude")
        .join(".credentials.json")
}

fn seats_path() -> PathBuf {
    crate::platform::app_dir().join("claude-seats.json")
}

/// The last fetch of both seats, however old.
fn cached_seats() -> Option<Seats> {
    serde_json::from_str(&std::fs::read_to_string(seats_path()).ok()?).ok()
}

fn store_seats(seats: &Seats) {
    if let Ok(json) = serde_json::to_vec_pretty(seats) {
        let _ = crate::durable::write(&seats_path(), &json);
    }
}

/// The last usage any view fetched, for surfaces that only render (the rail).
pub fn latest() -> Option<PlanUsage> {
    LATEST.lock().ok()?.clone()
}

static LATEST: std::sync::Mutex<Option<PlanUsage>> = std::sync::Mutex::new(None);
static LATEST_SEATS: std::sync::Mutex<Option<Seats>> = std::sync::Mutex::new(None);

/// The last fetch of both seats, for surfaces that only render (the rail).
pub fn latest_seats() -> Option<Seats> {
    LATEST_SEATS.lock().ok()?.clone()
}

/// Whether Claude Code is running on a borrowed token right now.
pub fn active_is_borrowed() -> bool {
    oauth_token().is_some()
}

/// What the views show: the cache while it is fresh, a fetch when it is not.
/// `None` when there is no Claude sign-in on this machine; fetch errors fall
/// back to the stale cache so the panel never blanks out.
pub fn refresh() -> Option<PlanUsage> {
    seats();
    latest()
}

/// Both seats, the sign-in at most every [`STALE`] seconds and the borrowed
/// one every [`BORROWED_STALE`] (reading it costs a few tokens of that seat).
/// A seat that fails to answer keeps the cache's windows, so one dead token
/// never blanks the other.
pub fn seats() -> Seats {
    let at = now();
    let mut out = cached_seats().unwrap_or_default();
    let token = borrowed_token();
    let own_due = out.own.is_none() || at.saturating_sub(out.at) >= STALE;
    let borrowed_due = token.is_some()
        && (out.borrowed.is_none() || at.saturating_sub(out.borrowed_at) >= BORROWED_STALE);
    if !own_due && !borrowed_due && (token.is_some() || out.borrowed.is_none()) {
        return remember_seats(out);
    }
    let mut changed = false;
    if own_due && let Ok(own) = fetch_own() {
        out.own = own;
        out.at = at;
        changed = true;
    }
    match &token {
        Some(token) if borrowed_due => {
            out.borrowed = fetch_borrowed(token).ok().or(out.borrowed.take());
            out.borrowed_at = at;
            changed = true;
        }
        // The token is gone (no file, not in use): its windows go with it.
        None if out.borrowed.is_some() => {
            out.borrowed = None;
            changed = true;
        }
        _ => {}
    }
    if changed {
        store_seats(&out);
    }
    remember_seats(out)
}

fn remember_seats(s: Seats) -> Seats {
    if let Ok(mut latest) = LATEST_SEATS.lock() {
        *latest = Some(s.clone());
    }
    // The seat Claude Code runs on is what the rail, the phone and the
    // alerts read; keep it with the pair so one fetch feeds every surface.
    if let Ok(mut latest) = LATEST.lock() {
        *latest = s.active().cloned();
    }
    s
}

/// One GET of the usage endpoint for the claude.ai sign-in.
fn fetch_own() -> Result<Option<PlanUsage>> {
    let Some(oauth) = credentials() else {
        return Ok(None);
    };
    // Claude Code renews its own sign-in when it runs. A renewal here could
    // rotate the refresh token and sign Claude Code out, so an expired token
    // waits for that and the cached windows stay on screen.
    if oauth.expires_at <= now() + 60 {
        bail!("Claude Code's sign-in has expired; it renews it the next time it runs");
    }
    cli_login::check(AgentKind::Claude, ENDPOINT)?;
    let body = http(
        ENDPOINT,
        None,
        &[
            ("Authorization", format!("Bearer {}", oauth.access_token)),
            ("anthropic-beta", "oauth-2025-04-20".into()),
        ],
    )?;
    let json: Value = serde_json::from_str(&body).context("usage endpoint did not answer JSON")?;
    if let Some(msg) = json["error"]["message"]
        .as_str()
        .or_else(|| json["error"].as_str())
    {
        bail!("usage endpoint: {msg}");
    }
    Ok(Some(parse(&json, &oauth)))
}

struct Oauth {
    access_token: String,
    /// Unix seconds.
    expires_at: u64,
    subscription: String,
    tier: String,
}

/// Claude Code's sign-in, shared with every tool that reads ~/.claude. Read
/// only: see `cli_login`.
fn credentials() -> Option<Oauth> {
    let json: Value =
        serde_json::from_str(&std::fs::read_to_string(credentials_path()).ok()?).ok()?;
    let o = json.get("claudeAiOauth")?;
    Some(Oauth {
        access_token: o["accessToken"].as_str()?.to_string(),
        expires_at: (o["expiresAt"].as_f64().unwrap_or(0.) / 1000.) as u64,
        subscription: o["subscriptionType"]
            .as_str()
            .unwrap_or_default()
            .to_string(),
        tier: o["rateLimitTier"].as_str().unwrap_or_default().to_string(),
    })
}

/// Whether Claude Code has a claude.ai sign-in or a long-lived token. On macOS
/// the sign-in sits in the keychain, so the account in ~/.claude.json counts too.
pub fn has_sign_in() -> bool {
    credentials().is_some()
        || oauth_token().is_some()
        || std::fs::read_to_string(crate::platform::home().join(".claude.json"))
            .ok()
            .and_then(|s| serde_json::from_str::<Value>(&s).ok())
            .is_some_and(|j| j["oauthAccount"].is_object())
}

/// The `CLAUDE_CODE_OAUTH_TOKEN` Claude Code will run on instead of the
/// claude.ai sign-in: from `env` in ~/.claude/settings.json (it wins, Claude
/// Code applies it over its own environment) or from ours.
fn oauth_token() -> Option<String> {
    let settings = crate::platform::home()
        .join(".claude")
        .join("settings.json");
    let from_settings = std::fs::read_to_string(settings)
        .ok()
        .and_then(|s| serde_json::from_str::<Value>(&s).ok())
        .and_then(|j| {
            j["env"]["CLAUDE_CODE_OAUTH_TOKEN"]
                .as_str()
                .map(str::to_string)
        });
    from_settings
        .or_else(|| std::env::var("CLAUDE_CODE_OAUTH_TOKEN").ok())
        .filter(|t| !t.trim().is_empty())
}

/// Another seat's long-lived token (`claude setup-token`), one line in a file
/// of the user's own: the lab's seat, a teammate's, a spare.
#[cfg(feature = "coderlabs")]
pub fn seat_token_path() -> PathBuf {
    std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .unwrap_or_else(|| crate::platform::home().join(".config"))
        .join("arakun")
        .join("claude-seat-token")
}

/// The long-lived token Claude Code is running on right now, if any.
#[cfg(not(feature = "coderlabs"))]
fn borrowed_token() -> Option<String> {
    oauth_token()
}

/// The borrowed seat's token: the one Claude Code is running on right now
/// (`env` in ~/.claude/settings.json, which wins), `ARAKUN_CLAUDE_SEAT_TOKEN`,
/// or the file at [`seat_token_path`]. Read only, for the usage card.
#[cfg(feature = "coderlabs")]
fn borrowed_token() -> Option<String> {
    let env = |k: &str| {
        std::env::var(k)
            .ok()
            .map(|v| v.trim().to_string())
            .filter(|v| !v.is_empty())
    };
    oauth_token()
        .or_else(|| env("ARAKUN_CLAUDE_SEAT_TOKEN"))
        .or_else(|| {
            std::fs::read_to_string(seat_token_path())
                .ok()
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
        })
}

/// The token's 5-hour and weekly windows, from the headers of a one-token
/// Haiku message (about 35 tokens of the seat per fetch).
fn fetch_borrowed(token: &str) -> Result<PlanUsage> {
    cli_login::check(AgentKind::Claude, MESSAGES)?;
    let body = serde_json::json!({
        "model": PROBE_MODEL,
        "max_tokens": 1,
        "system": "You are Claude Code, Anthropic's official CLI for Claude.",
        "messages": [{"role": "user", "content": "hi"}],
    })
    .to_string();
    let (status, headers) = http_headers(
        MESSAGES,
        Some(&body),
        &[
            ("Authorization", format!("Bearer {token}")),
            ("anthropic-beta", "oauth-2025-04-20".into()),
            ("anthropic-version", "2023-06-01".into()),
            ("Content-Type", "application/json".into()),
        ],
    )?;
    let plan = parse_headers(&headers);
    if plan.windows.is_empty() {
        bail!("HTTP {status}: the token's answer carried no rate-limit headers");
    }
    Ok(plan)
}

/// `anthropic-ratelimit-unified-{5h,7d}-{utilization,reset,status}` (curl's
/// `%{header_json}`: lowercase names, each an array of values).
fn parse_headers(headers: &Value) -> PlanUsage {
    let get = |name: &str| {
        headers[format!("anthropic-ratelimit-unified-{name}")][0]
            .as_str()
            .map(str::to_string)
    };
    let windows = [("5h", "5-hour limit"), ("7d", "Weekly limit")]
        .into_iter()
        .filter_map(|(key, label)| {
            let used: f64 = get(&format!("{key}-utilization"))?.parse().ok()?;
            let status = get(&format!("{key}-status")).unwrap_or_default();
            Some(Window {
                label: label.to_string(),
                percent: percent(&Value::from(used)),
                severity: match status.as_str() {
                    "" | "allowed" => "normal",
                    "rejected" => "critical",
                    _ => "warning",
                }
                .to_string(),
                resets_at: get(&format!("{key}-reset")).and_then(|r| r.parse().ok()),
                active: true,
            })
        })
        .collect();
    PlanUsage {
        at: now(),
        windows,
        borrowed: true,
        ..PlanUsage::default()
    }
}

/// curl with the flags and headers in a stdin config, so tokens never sit in
/// the process list. Fails on a non-2xx status.
pub(crate) fn http(url: &str, post: Option<&str>, headers: &[(&str, String)]) -> Result<String> {
    let (body, status) = http_with(url, post, headers, "")?;
    if !(200..300).contains(&status) {
        return Err(status_error(status, &body));
    }
    Ok(body)
}

/// A non-2xx answer as an error: the status and the start of the body.
fn status_error(status: u32, body: &str) -> anyhow::Error {
    // Cut at a char: an error page from a proxy or portal can be in any language.
    anyhow!("HTTP {status}: {}", &body[..body.floor_char_boundary(200)])
}

/// curl with `extra` (write-out variables) printed after the body: the
/// output and the HTTP status, whatever it is.
fn http_with(
    url: &str,
    post: Option<&str>,
    headers: &[(&str, String)],
    extra: &str,
) -> Result<(String, u32)> {
    curl(&curl_config(url, post, headers, extra, false))
}

/// The body and the HTTP status, whatever it is. `direct` goes past any
/// proxy in the environment (`http_proxy`): for a server on this machine,
/// whose headers no proxy should see.
pub(crate) fn http_status(
    url: &str,
    post: Option<&str>,
    headers: &[(&str, String)],
    direct: bool,
) -> Result<(String, u32)> {
    curl(&curl_config(url, post, headers, "", direct))
}

/// curl's config: the flags and headers it reads from stdin.
fn curl_config(
    url: &str,
    post: Option<&str>,
    headers: &[(&str, String)],
    extra: &str,
    direct: bool,
) -> String {
    let mut config = format!(
        "url = \"{url}\"\nsilent\nshow-error\nmax-time = 20\nwrite-out = \"{}\\n%{{http_code}}\"\n",
        extra.replace('"', "\\\"")
    );
    for (k, v) in headers {
        config.push_str(&format!(
            "header = \"{}: {}\"\n",
            k,
            v.replace('\\', "\\\\").replace('"', "\\\"")
        ));
    }
    if let Some(body) = post {
        config.push_str("request = \"POST\"\n");
        config.push_str(&format!(
            "data = \"{}\"\n",
            body.replace('\\', "\\\\").replace('"', "\\\"")
        ));
    }
    if direct {
        config.push_str("noproxy = \"*\"\n");
    }
    config
}

/// Runs curl with `config` on stdin, so tokens never sit in the process list.
fn curl(config: &str) -> Result<(String, u32)> {
    let mut child = crate::platform::command("curl")
        .args(["--config", "-"])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .context("curl is needed to read plan usage")?;
    child
        .stdin
        .take()
        .expect("piped")
        .write_all(config.as_bytes())?;
    let out = child.wait_with_output()?;
    if !out.status.success() {
        bail!("curl: {}", String::from_utf8_lossy(&out.stderr).trim());
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let (body, status) = text.rsplit_once('\n').unwrap_or((&text, "0"));
    Ok((body.to_string(), status.trim().parse().unwrap_or(0)))
}

/// The answer's status and headers (curl's `%{header_json}` object), even
/// for a refusal: a 429 still says which window is full.
fn http_headers(url: &str, post: Option<&str>, headers: &[(&str, String)]) -> Result<(u32, Value)> {
    // curl turns the config's `\n` into a newline before the marker.
    let (out, status) = http_with(url, post, headers, "\\n@@arakun-headers@@%{header_json}")?;
    let (_, json) = out
        .rsplit_once("\n@@arakun-headers@@")
        .ok_or_else(|| anyhow!("curl printed no headers"))?;
    Ok((
        status,
        serde_json::from_str(json).context("curl's header_json")?,
    ))
}

fn parse(json: &Value, oauth: &Oauth) -> PlanUsage {
    parse_usage(json, &oauth.subscription, &oauth.tier)
}

/// The endpoint's JSON into our shape. `limits[]` is the canonical list; the
/// five_hour/seven_day fields cover accounts the list does not serve.
pub(crate) fn parse_usage(json: &Value, subscription: &str, tier: &str) -> PlanUsage {
    let mut windows = Vec::new();
    if let Some(limits) = json["limits"].as_array().filter(|l| !l.is_empty()) {
        for l in limits {
            let kind = l["kind"].as_str().unwrap_or_default();
            let model = l["scope"]["model"]["display_name"]
                .as_str()
                .unwrap_or_default();
            let label = match kind {
                "session" => "5-hour limit".to_string(),
                "weekly_scoped" if !model.is_empty() => format!("Weekly · {model}"),
                "weekly_scoped" | "weekly" => "Weekly limit".to_string(),
                other => {
                    let mut c = other.replace('_', " ");
                    if let Some(f) = c.get(..1) {
                        c.replace_range(..1, &f.to_uppercase());
                    }
                    c
                }
            };
            windows.push(Window {
                label,
                percent: whole(&l["percent"]),
                severity: l["severity"].as_str().unwrap_or("normal").to_string(),
                resets_at: l["resets_at"].as_str().and_then(from_iso),
                active: l["is_active"].as_bool().unwrap_or(true),
            });
        }
    } else {
        for (key, label) in [
            ("five_hour", "5-hour limit"),
            ("seven_day", "Weekly limit"),
            ("seven_day_opus", "Weekly · Opus"),
            ("seven_day_sonnet", "Weekly · Sonnet"),
        ] {
            if let Some(w) = json.get(key).filter(|w| w.is_object()) {
                windows.push(Window {
                    label: label.to_string(),
                    percent: percent(&w["utilization"]),
                    severity: w["locked_reason"]
                        .as_str()
                        .map_or("normal", |_| "warning")
                        .to_string(),
                    resets_at: w["resets_at"].as_str().and_then(from_iso),
                    active: true,
                });
            }
        }
    }
    let spend = spend(json);
    PlanUsage {
        at: now(),
        subscription: subscription.to_string(),
        tier: tier.to_string(),
        windows,
        spend,
        borrowed: false,
    }
}

/// `limits[].percent` and `spend.percent` are always 0-100: a 1 there is 1%, not a whole fraction.
fn whole(v: &Value) -> u64 {
    v.as_f64()
        .filter(|p| p.is_finite() && *p >= 0.)
        .map_or(0, |p| p.round() as u64)
}

/// A fraction (0-1) or an already-percent number, as 0-100 (legacy `utilization` fields).
fn percent(v: &Value) -> u64 {
    v.as_f64()
        .filter(|p| p.is_finite() && *p >= 0.)
        .map_or(0, |p| if p <= 1. { p * 100. } else { p }.round() as u64)
}

fn spend(json: &Value) -> Option<Spend> {
    let s = &json["spend"];
    if s["enabled"].as_bool() == Some(true) {
        let exp = s["used"]["exponent"].as_f64().unwrap_or(2.);
        let unit = 10f64.powf(exp);
        let money = |v: &Value| v["amount_minor"].as_f64().map(|a| a / unit);
        return Some(Spend {
            used: money(&s["used"]).unwrap_or(0.),
            limit: s.get("limit").and_then(money),
            percent: whole(&s["percent"]),
            currency: s["used"]["currency"].as_str().unwrap_or("USD").to_string(),
        });
    }
    let e = &json["extra_usage"];
    if e["is_enabled"].as_bool() == Some(true) {
        return Some(Spend {
            used: e["used_credits"].as_f64().unwrap_or(0.),
            limit: e["monthly_limit"].as_f64(),
            percent: percent(&e["utilization"]),
            currency: e["currency"].as_str().unwrap_or("USD").to_string(),
        });
    }
    None
}

/// "2026-10-26T02:59:59.778354+00:00" (or "Z") to unix seconds.
pub(crate) fn from_iso(s: &str) -> Option<u64> {
    let (date, time) = s.split_once('T')?;
    let mut d = date.split('-');
    let (mut y, mo, da): (i64, i64, i64) = (
        d.next()?.parse().ok()?,
        d.next()?.parse().ok()?,
        d.next()?.parse().ok()?,
    );
    let (clock, offset) = time
        .split_once(['+', '-'])
        .unwrap_or((time.trim_end_matches('Z'), ""));
    let sign: i64 = if time.contains('-') && !time.ends_with('Z') {
        -1
    } else {
        1
    };
    let mut t = clock.split(':');
    let secs = |s: &str| s.split('.').next().unwrap_or("0").parse().ok();
    let (h, mi, se): (i64, i64, i64) = (
        t.next()?.parse().ok()?,
        t.next().and_then(secs)?,
        t.next().and_then(secs).unwrap_or(0),
    );
    let mut off = offset.split(':');
    let offset_secs = off.next().unwrap_or("0").parse::<i64>().unwrap_or(0) * 3600
        + off.next().unwrap_or("0").parse::<i64>().unwrap_or(0) * 60;
    // Days since the epoch, Howard Hinnant's days-from-civil.
    if mo <= 2 {
        y -= 1;
    }
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = (mo + 9) % 12;
    let doy = (153 * mp + 2) / 5 + da - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146097 + doe - 719468;
    Some((days * 86400 + h * 3600 + mi * 60 + se - sign * offset_secs) as u64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_limits_list_and_spend() {
        let json = serde_json::json!({
            "five_hour": {"utilization": 0.4, "resets_at": "2026-10-26T02:59:59.778354+00:00"},
            "limits": [
                {"kind": "session", "percent": 42.4, "severity": "normal", "resets_at": "2026-10-26T02:59:59.778354+00:00", "is_active": true},
                {"kind": "weekly_scoped", "percent": 80, "severity": "warning", "resets_at": null, "scope": {"model": {"display_name": "Fable"}}, "is_active": false}
            ],
            "spend": {"enabled": true, "used": {"amount_minor": 1234, "currency": "USD", "exponent": 2}, "limit": {"amount_minor": 5000, "exponent": 2}, "percent": 24.68}
        });
        let oauth = Oauth {
            access_token: String::new(),
            expires_at: 0,
            subscription: "team".into(),
            tier: "default_claude_max_5x".into(),
        };
        let p = parse(&json, &oauth);
        assert_eq!(p.plan_label(), "Team · Max 5x");
        assert_eq!(p.windows.len(), 2);
        assert_eq!(p.windows[0].label, "5-hour limit");
        assert_eq!(p.windows[0].percent, 42);
        assert_eq!(p.windows[1].label, "Weekly · Fable");
        assert!(!p.windows[1].active);
        assert!(p.windows[1].hot());
        let spend = p.spend.unwrap();
        assert_eq!(
            (spend.used, spend.limit, spend.percent),
            (12.34, Some(50.), 25)
        );
    }

    #[test]
    fn falls_back_to_window_fields() {
        let json = serde_json::json!({
            "five_hour": {"utilization": 0.04, "resets_at": "2026-10-26T02:59:59+00:00"},
            "seven_day_opus": {"utilization": 12.0, "resets_at": null}
        });
        let oauth = Oauth {
            access_token: String::new(),
            expires_at: 0,
            subscription: "max".into(),
            tier: String::new(),
        };
        let p = parse(&json, &oauth);
        assert_eq!(
            p.windows
                .iter()
                .map(|w| (w.label.as_str(), w.percent))
                .collect::<Vec<_>>(),
            [("5-hour limit", 4), ("Weekly · Opus", 12)]
        );
        assert!(p.spend.is_none());
    }

    #[test]
    fn borrowed_token_windows_come_from_headers() {
        let headers = serde_json::json!({
            "anthropic-ratelimit-unified-5h-utilization": ["0.0"],
            "anthropic-ratelimit-unified-5h-reset": ["1794254400"],
            "anthropic-ratelimit-unified-5h-status": ["allowed"],
            "anthropic-ratelimit-unified-7d-utilization": ["0.86"],
            "anthropic-ratelimit-unified-7d-reset": ["1794337200"],
            "anthropic-ratelimit-unified-7d-status": ["allowed_warning"]
        });
        let p = parse_headers(&headers);
        assert!(p.borrowed);
        assert_eq!(p.plan_label(), "Borrowed token");
        assert_eq!(
            p.windows
                .iter()
                .map(|w| (w.label.as_str(), w.percent))
                .collect::<Vec<_>>(),
            [("5-hour limit", 0), ("Weekly limit", 86)]
        );
        assert_eq!(p.windows[1].resets_at, Some(1_794_337_200));
        assert!(p.windows[1].hot() && !p.windows[0].hot());
    }

    #[test]
    #[ignore = "live: spends ~35 tokens of the borrowed seat's token"]
    fn live_borrowed_fetch() {
        let p = fetch_borrowed(&borrowed_token().expect("no token")).unwrap();
        eprintln!("{p:?}");
    }

    #[test]
    #[ignore = "live: reads the sign-in's plan and the borrowed seat's"]
    fn live_seats_fetch() {
        let s = seats();
        eprintln!("{s:?}");
        assert!(s.own.is_some() || s.borrowed.is_some());
    }

    #[test]
    fn seats_keep_both_and_pick_the_active_one() {
        let own = PlanUsage {
            at: 1,
            subscription: "team".into(),
            tier: "default_claude_max_5x".into(),
            ..PlanUsage::default()
        };
        let borrowed = parse_headers(&serde_json::json!({
            "anthropic-ratelimit-unified-5h-utilization": ["1.0"],
            "anthropic-ratelimit-unified-5h-status": ["rejected"]
        }));
        let seats = Seats {
            at: 1,
            own: Some(own.clone()),
            borrowed_at: 1,
            borrowed: Some(borrowed.clone()),
        };
        // Signed in on the machine's own account, that seat is in use; with a
        // borrowed token, the borrowed one.
        assert_eq!(seats.in_use(false), Some(&own));
        assert_eq!(seats.in_use(true), Some(&borrowed));
        // Both survive a round trip through the cache file's JSON.
        let json = serde_json::to_string(&seats).unwrap();
        let back: Seats = serde_json::from_str(&json).unwrap();
        assert_eq!(back, seats);
        assert_eq!(back.borrowed.as_ref().unwrap().windows[0].percent, 100);
    }

    #[test]
    fn a_direct_call_skips_the_environments_proxy() {
        let headers = [("Authorization", "Bearer k".to_string())];
        let direct = curl_config("http://127.0.0.1:8317/x", None, &headers, "", true);
        assert!(direct.contains("noproxy = \"*\"\n"));
        let normal = curl_config(ENDPOINT, None, &headers, "", false);
        assert!(!normal.contains("noproxy"));
    }

    #[test]
    fn iso_dates_become_unix_seconds() {
        assert_eq!(from_iso("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(from_iso("1970-01-01T02:30:00+02:30"), Some(0));
        assert_eq!(
            from_iso("2026-10-26T02:59:59.778354+00:00"),
            Some(1_792_983_599)
        );
        assert_eq!(from_iso("nonsense"), None);
    }

    #[test]
    fn reset_text_is_relative_soon_and_absolute_later() {
        let w = Window {
            label: "x".into(),
            percent: 0,
            severity: "normal".into(),
            resets_at: Some(1_000_900),
            active: true,
        };
        assert_eq!(w.reset_text(1_000_000).as_deref(), Some("resets in 15m"));
        let w = Window {
            resets_at: Some(1_000_000 + 5_500),
            ..w
        };
        assert_eq!(w.reset_text(1_000_000).as_deref(), Some("resets in 1h 31m"));
        assert_eq!(w.reset_text(1_000_001).as_deref(), Some("resets in 1h 31m"));
        let past = Window {
            resets_at: Some(10),
            ..w
        };
        assert_eq!(past.reset_text(1_000_000).as_deref(), Some("resetting…"));
    }

    #[test]
    fn status_error_cuts_the_body_at_a_char() {
        // 199 ASCII bytes, then "é" across byte 200: an error page in another language.
        let body = format!("{}é and more", "x".repeat(199));
        let err = status_error(502, &body).to_string();
        assert_eq!(err, format!("HTTP 502: {}", "x".repeat(199)));
        assert_eq!(status_error(404, "nö").to_string(), "HTTP 404: nö");
    }
}
