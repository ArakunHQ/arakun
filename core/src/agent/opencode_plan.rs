//! The OpenCode Go plan: how full its 5-hour, weekly and monthly windows are,
//! from the endpoint the OpenCode console reads. Same shape as `plan` (cache,
//! `latest` for the rail, `refresh` for the views) and the same `PlanUsage`,
//! with no subscription, tier or spend: Go's limits are dollar amounts per
//! model, but the endpoint only reports how full each window is.

use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context as _, Result, bail};
use serde_json::Value;

use super::plan::{self, PlanUsage, Window};
use super::{AgentKind, cli_login};

const ENDPOINT: &str = "https://opencode.ai/zen/go/v1/usage";
/// Refetch at most this often; `refresh` serves the cache in between.
const STALE: u64 = 5 * 60;
/// The provider OpenCode files the Go key under in its `auth.json`.
const PROVIDER: &str = "opencode-go";

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// OpenCode's credentials file; `ARAKUN_OPENCODE_AUTH` points elsewhere (the
/// dev nest sandboxes XDG_DATA_HOME, so it would never find the real one).
fn auth_path() -> PathBuf {
    if let Some(p) = std::env::var_os("ARAKUN_OPENCODE_AUTH")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
    {
        return p;
    }
    crate::platform::data_home()
        .join("opencode")
        .join("auth.json")
}

/// The Go API key, if `opencode auth login` stored one. Never logged or shown.
fn key() -> Option<String> {
    let json: Value = crate::durable::load_json(auth_path())?;
    json[PROVIDER]["key"]
        .as_str()
        .filter(|k| !k.is_empty())
        .map(str::to_string)
}

fn cache_path() -> PathBuf {
    crate::paths::shared(crate::paths::OPENCODE_PLAN)
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
/// `None` without a Go key (the cache never outlives the key); fetch errors
/// fall back to the stale cache so the panel never blanks out.
pub fn refresh() -> Option<PlanUsage> {
    let Some(key) = key() else {
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

/// One GET of the usage endpoint. A key without Go answers 403.
fn fetch(key: &str) -> Result<PlanUsage> {
    cli_login::check(AgentKind::OpenCode, ENDPOINT)?;
    let body = plan::http(
        ENDPOINT,
        None,
        &[("Authorization", format!("Bearer {key}"))],
    )?;
    let json: Value =
        serde_json::from_str(&body).context("OpenCode usage endpoint did not answer JSON")?;
    if let Some(msg) = json["error"]["message"]
        .as_str()
        .or_else(|| json["error"].as_str())
    {
        bail!("OpenCode usage endpoint: {msg}");
    }
    Ok(parse(&json))
}

/// `{"usage": {"rolling": {status, percent, resetsAt}, "weekly": …, "monthly": …}}`
/// into windows. `status` other than "ok" rides along as the severity, so the
/// views can say it.
fn parse(json: &Value) -> PlanUsage {
    let windows = [
        ("rolling", "5-hour limit"),
        ("weekly", "Weekly limit"),
        ("monthly", "Monthly limit"),
    ]
    .into_iter()
    .filter_map(|(key, label)| {
        let w = json["usage"].get(key).filter(|w| w.is_object())?;
        let status = w["status"].as_str().unwrap_or("ok");
        Some(Window {
            label: label.to_string(),
            percent: w["percent"]
                .as_f64()
                .filter(|p| p.is_finite() && *p >= 0.)
                .map_or(0, |p| p.round() as u64),
            severity: if status == "ok" {
                "normal".into()
            } else {
                status.to_string()
            },
            resets_at: w["resetsAt"].as_str().and_then(plan::from_iso),
            active: true,
        })
    })
    .collect();
    PlanUsage {
        at: now(),
        windows,
        ..PlanUsage::default()
    }
}

/// A status the endpoint sent that isn't "ok", for the views to show as is.
pub fn status(w: &Window) -> Option<&str> {
    (w.severity != "normal").then_some(w.severity.as_str())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_the_three_windows() {
        let json = serde_json::json!({"usage": {
            "rolling": {"status": "ok", "percent": 1, "resetsAt": "2026-11-02T22:47:51.000Z"},
            "weekly": {"status": "ok", "percent": 0, "resetsAt": "2026-11-09T00:00:00.000Z"},
            "monthly": {"status": "throttled", "percent": 99.6, "resetsAt": "2026-12-02T17:07:42.000Z"}
        }});
        let p = parse(&json);
        let rows: Vec<_> = p
            .windows
            .iter()
            .map(|w| (w.short_label(), w.percent, status(w)))
            .collect();
        assert_eq!(
            rows,
            [
                ("5h".to_string(), 1, None),
                ("Week".to_string(), 0, None),
                ("Month".to_string(), 100, Some("throttled"))
            ]
        );
        assert_eq!(
            p.windows[0].resets_at,
            plan::from_iso("2026-11-02T22:47:51Z")
        );
        assert!(!p.windows[0].hot() && p.windows[2].hot());
        assert!(p.plan_label().is_empty() && p.spend.is_none());
    }

    #[test]
    fn missing_windows_are_left_out() {
        let p = parse(&serde_json::json!({"usage": {"weekly": {"percent": 40}}}));
        assert_eq!(p.windows.len(), 1);
        assert_eq!((p.windows[0].percent, p.windows[0].resets_at), (40, None));
        assert!(parse(&serde_json::json!({})).windows.is_empty());
    }
}
