//! The Devin plan's quota: how much of the daily and weekly allowance is
//! used, read the way the devin CLI's `/usage` reads it. Same shape as
//! `opencode_plan` (cache, `latest` for the rail, `refresh` for the views).
//!
//! This is not a documented API. Devin's consumption API answers only
//! Enterprise accounts, and self-serve plans (Pro, Max) see their quota only in
//! the CLI, Devin Desktop and app.devin.ai. So Arakun calls the same Connect
//! endpoint the CLI calls, with the key `devin auth login` saved. A failure of
//! any kind leaves the plan out; nothing is guessed.

use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context as _, Result, bail};
use serde_json::{Value, json};

use super::plan::{self, PlanUsage, Window};
use super::{AgentKind, cli_login, launch};

/// The host `credentials.toml` names as `api_server_url` for a Devin sign-in.
const HOST: &str = "https://server.codeium.com";
const METHOD: &str = "/exa.seat_management_pb.SeatManagementService/GetUserStatus";
/// Refetch at most this often; `refresh` serves the cache in between.
const STALE: u64 = 5 * 60;

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// A top-level `key = "value"` of `credentials.toml`. The file is flat; a
/// value with escapes is not a key we need.
fn toml_string(text: &str, key: &str) -> Option<String> {
    text.lines().find_map(|line| {
        let (k, v) = line.split_once('=')?;
        if k.trim() != key {
            return None;
        }
        let v = v.trim().strip_prefix('"')?.strip_suffix('"')?;
        (!v.is_empty() && !v.contains('\\')).then(|| v.to_string())
    })
}

/// The sign-in's key, when it is for Devin's own server. Never logged or shown.
fn key(path: &Path) -> Option<String> {
    let text = std::fs::read_to_string(path).ok()?;
    let server = toml_string(&text, "api_server_url")?;
    (server.trim_end_matches('/') == HOST)
        .then(|| toml_string(&text, "windsurf_api_key"))
        .flatten()
}

fn cache_path() -> std::path::PathBuf {
    crate::paths::shared(crate::paths::DEVIN_PLAN)
}

/// The last fetch, however old; `None` when never fetched.
pub fn cached() -> Option<PlanUsage> {
    crate::durable::load_json(cache_path())
}

fn store(usage: &PlanUsage) {
    crate::durable::cache_json(&cache_path(), usage);
}

/// The last usage any view fetched, for surfaces that only render (the rail).
pub fn latest() -> Option<PlanUsage> {
    LATEST.lock().ok()?.clone()
}

static LATEST: std::sync::Mutex<Option<PlanUsage>> = std::sync::Mutex::new(None);

/// What the views show: the cache while it is fresh, a fetch when it is not.
/// `None` when Devin is signed out (the cache never outlives the sign-in);
/// fetch errors fall back to the stale cache so the panel never blanks out.
pub fn refresh() -> Option<PlanUsage> {
    let Some(key) = key(&launch::devin_credentials_path()) else {
        return remember(None);
    };
    let cache = cached();
    if let Some(p) = &cache
        && now().saturating_sub(p.at) < STALE
    {
        return remember(cache);
    }
    let out = match fetch(&key) {
        Ok(p) => {
            store(&p);
            Some(p)
        }
        Err(_) => cache,
    };
    remember(out)
}

fn remember(p: Option<PlanUsage>) -> Option<PlanUsage> {
    if let Ok(mut latest) = LATEST.lock() {
        *latest = p.clone();
    }
    p
}

/// One unary Connect call in its JSON encoding.
fn fetch(key: &str) -> Result<PlanUsage> {
    let url = format!("{HOST}{METHOD}");
    cli_login::check(AgentKind::Devin, &url)?;
    let body = json!({"metadata": {
        "apiKey": key,
        "ideName": "arakun",
        "ideVersion": env!("CARGO_PKG_VERSION"),
        "extensionName": "arakun",
        "extensionVersion": env!("CARGO_PKG_VERSION"),
    }})
    .to_string();
    let answer = plan::http(
        &url,
        Some(&body),
        &[
            ("Content-Type", "application/json".into()),
            ("Connect-Protocol-Version", "1".into()),
        ],
    )?;
    let json: Value =
        serde_json::from_str(&answer).context("Devin's usage endpoint did not answer JSON")?;
    if let Some(msg) = json["message"]
        .as_str()
        .filter(|_| json["code"].is_string())
    {
        bail!("Devin's usage endpoint: {msg}");
    }
    parse(&json).context("Devin's answer has no quota")
}

/// `userStatus.planStatus` into windows. Proto3 JSON leaves out zeros, so a
/// window counts when its reset time is there, and a missing remaining percent
/// is 0. Plans billed in credits or ACUs have no quota windows: `None`.
fn parse(json: &Value) -> Option<PlanUsage> {
    let status = &json["userStatus"]["planStatus"];
    let info = Some(&status["planInfo"])
        .filter(|i| i.is_object())
        .unwrap_or(&json["planInfo"]);
    if info["billingStrategy"].as_str() != Some("BILLING_STRATEGY_QUOTA") {
        return None;
    }
    let int = |v: &Value| {
        v.as_u64()
            .or_else(|| v.as_str().and_then(|s| s.parse().ok()))
    };
    let windows = [
        ("daily", "Daily limit", "hideDailyQuota"),
        ("weekly", "Weekly limit", "hideWeeklyQuota"),
    ]
    .into_iter()
    .filter(|(_, _, hide)| !info[*hide].as_bool().unwrap_or(false))
    .filter_map(|(key, label, _)| {
        let resets_at = int(&status[format!("{key}QuotaResetAtUnix")]).filter(|t| *t > 0)?;
        let left = int(&status[format!("{key}QuotaRemainingPercent")])
            .unwrap_or(0)
            .min(100);
        Some(Window {
            label: label.into(),
            percent: 100 - left,
            severity: "normal".into(),
            resets_at: Some(resets_at),
            active: true,
        })
    })
    .collect::<Vec<_>>();
    if windows.is_empty() {
        return None;
    }
    Some(PlanUsage {
        at: now(),
        subscription: info["planName"].as_str().unwrap_or_default().to_string(),
        windows,
        ..PlanUsage::default()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_max_plan_shows_only_its_week() {
        let json = serde_json::json!({"userStatus": {"planStatus": {
            "dailyQuotaRemainingPercent": 100,
            "weeklyQuotaRemainingPercent": 97,
            "dailyQuotaResetAtUnix": "1795161600",
            "weeklyQuotaResetAtUnix": "1795334400",
            "planInfo": {"planName": "Max", "hideDailyQuota": true,
                "billingStrategy": "BILLING_STRATEGY_QUOTA"}
        }}});
        let p = parse(&json).unwrap();
        let rows: Vec<_> = p
            .windows
            .iter()
            .map(|w| (w.short_label(), w.percent, w.resets_at))
            .collect();
        assert_eq!(rows, [("Week".to_string(), 3, Some(1_795_334_400))]);
        assert_eq!(p.subscription, "Max");
    }

    #[test]
    fn a_used_up_day_is_full_even_though_proto_json_drops_the_zero() {
        let json = serde_json::json!({
            "userStatus": {"planStatus": {
                "weeklyQuotaRemainingPercent": 40,
                "dailyQuotaResetAtUnix": "100",
                "weeklyQuotaResetAtUnix": "200",
            }},
            "planInfo": {"planName": "Pro", "billingStrategy": "BILLING_STRATEGY_QUOTA"}
        });
        let p = parse(&json).unwrap();
        let rows: Vec<_> = p
            .windows
            .iter()
            .map(|w| (w.short_label(), w.percent))
            .collect();
        assert_eq!(rows, [("Day".to_string(), 100), ("Week".to_string(), 60)]);
        assert!(p.windows[0].hot());
    }

    #[test]
    fn plans_without_a_quota_show_nothing() {
        let credits = serde_json::json!({"userStatus": {"planStatus": {
            "weeklyQuotaResetAtUnix": "200",
            "planInfo": {"billingStrategy": "BILLING_STRATEGY_CREDITS"}
        }}});
        assert!(parse(&credits).is_none());
        let no_windows = serde_json::json!({"userStatus": {"planStatus": {
            "planInfo": {"billingStrategy": "BILLING_STRATEGY_QUOTA"}
        }}});
        assert!(parse(&no_windows).is_none());
        assert!(parse(&serde_json::json!({})).is_none());
    }

    /// Needs a `devin auth login`: `cargo test live_devin_plan -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn live_devin_plan() {
        let key = key(&launch::devin_credentials_path()).expect("signed in to Devin");
        let p = fetch(&key).unwrap();
        for w in &p.windows {
            println!(
                "{} {}: {}% used, resets {:?}",
                p.subscription, w.label, w.percent, w.resets_at
            );
        }
        assert!(!p.windows.is_empty());
    }

    #[test]
    fn the_key_is_read_only_for_devins_own_server() {
        let dir = std::env::temp_dir().join(format!("arakun-devin-plan-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("credentials.toml");
        let write = |server: &str| {
            std::fs::write(
                &path,
                format!("windsurf_api_key = \"k1\"\napi_server_url = \"{server}\"\n"),
            )
            .unwrap()
        };
        write("https://server.codeium.com");
        assert_eq!(key(&path).as_deref(), Some("k1"));
        write("https://server.codeium.com/");
        assert_eq!(key(&path).as_deref(), Some("k1"));
        write("https://devin.example.com");
        assert_eq!(key(&path), None);
        std::fs::write(&path, "api_server_url = \"https://server.codeium.com\"\n").unwrap();
        assert_eq!(key(&path), None);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
