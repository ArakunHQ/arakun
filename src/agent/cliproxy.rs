//! Plan limits through CLIProxyAPI: when Claude Code runs on that proxy, the
//! claude.ai accounts behind it are the ones that hit their 5-hour and weekly
//! limits, not this machine's sign-in. The proxy's management API lists them
//! (`GET /v0/management/auth-files`), and its `api-call` reads the same usage
//! endpoint as `plan` with each account's own token, so the token never leaves
//! the proxy.
//!
//! The management API needs the proxy's management key. Every wrong or missing
//! key counts toward a 30-minute ban of this address (5 tries), which would
//! also lock out the proxy's own web panel, so nothing is sent without a key
//! and a key the proxy refused is not tried again.

use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context as _, Result, anyhow, bail};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::plan::{self, PlanUsage};

/// Refetch at most this often; `refresh` serves the cache in between.
const STALE: u64 = 5 * 60;
/// Overrides the key saved from the Usage tab.
const KEY_ENV: &str = "ARAKUN_CLIPROXY_KEY";

/// What the proxy's Claude accounts have left.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Proxy {
    /// The management API's base, e.g. "http://127.0.0.1:8317/v0/management".
    pub base: String,
    /// Unix seconds of the last read.
    pub at: u64,
    pub accounts: Vec<Account>,
    /// Why the last read failed as a whole: a refused key, the proxy is down.
    pub error: Option<String>,
    /// Whether a management key is set; without one nothing is read.
    #[serde(skip)]
    pub has_key: bool,
    /// The fingerprint of a key the proxy refused, so it is not sent again.
    #[serde(default)]
    refused: Option<u64>,
}

/// One claude.ai account the proxy routes to.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Account {
    /// The account's email, or its auth file's name.
    pub name: String,
    /// Its windows; `None` when the read failed.
    pub usage: Option<PlanUsage>,
    pub error: Option<String>,
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

fn key_path() -> PathBuf {
    crate::platform::app_dir().join("cliproxy-management-key")
}

fn cache_path() -> PathBuf {
    crate::platform::app_dir().join("cliproxy-plan.json")
}

/// The management key: `ARAKUN_CLIPROXY_KEY`, else the one saved from the
/// Usage tab. Never logged or shown.
fn key() -> Option<String> {
    std::env::var(KEY_ENV)
        .ok()
        .or_else(|| std::fs::read_to_string(key_path()).ok())
        .map(|k| k.trim().to_string())
        .filter(|k| !k.is_empty())
}

/// Saves the management key where only this user can read it; an empty key
/// removes it.
pub fn save_key(key: &str) -> std::io::Result<()> {
    let key = key.trim();
    if key.is_empty() {
        return match std::fs::remove_file(key_path()) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e),
            _ => Ok(()),
        };
    }
    crate::durable::write_private(&key_path(), key.as_bytes())
}

/// The management API's base for Claude Code's `ANTHROPIC_BASE_URL`: the
/// proxy serves both from one origin.
fn management_base(base_url: &str) -> Option<String> {
    let (scheme, rest) = base_url.trim().split_once("://")?;
    let host = rest.split(['/', '?', '#']).next()?.trim();
    let host = host.rsplit_once('@').map_or(host, |(_, h)| h);
    (!host.is_empty()).then(|| format!("{}://{host}/v0/management", scheme.to_lowercase()))
}

/// A stable fingerprint of the key (FNV-1a), so the cache can say "this key
/// was refused" without holding the key.
fn fingerprint(key: &str) -> u64 {
    key.bytes().fold(0xcbf2_9ce4_8422_2325, |h, b| {
        (h ^ u64::from(b)).wrapping_mul(0x100_0000_01b3)
    })
}

fn cached() -> Option<Proxy> {
    serde_json::from_str(&std::fs::read_to_string(cache_path()).ok()?).ok()
}

fn store(p: &Proxy) {
    if let Ok(json) = serde_json::to_string_pretty(p) {
        let _ = crate::durable::write_private(&cache_path(), json.as_bytes());
    }
}

/// What the views show: `None` unless Claude Code runs on another endpoint.
/// The cache while it is fresh, a read when it is not, or now with `force`
/// (a key was just saved). Reads files and the network; keep it off the UI thread.
pub fn refresh(force: bool) -> Option<Proxy> {
    let base = management_base(&super::claude_auth::endpoint_url()?)?;
    let key = key();
    let at = now();
    let mut out = cached()
        .filter(|c| c.base == base)
        .unwrap_or_else(|| Proxy {
            base: base.clone(),
            ..Proxy::default()
        });
    out.has_key = key.is_some();
    let Some(key) = key else {
        return Some(out);
    };
    let print = fingerprint(&key);
    if out.refused == Some(print) {
        return Some(out);
    }
    let fresh = out.refused.is_none() && at.saturating_sub(out.at) < STALE && out.at > 0;
    if fresh && !force {
        return Some(out);
    }
    out.at = at;
    out.refused = None;
    match read(&base, &key) {
        Ok(accounts) => {
            out.accounts = accounts;
            out.error = None;
        }
        Err(Failure::Refused(msg)) => {
            out.refused = Some(print);
            out.error = Some(msg);
            out.accounts.clear();
        }
        // The proxy is down or restarting: keep the last windows on screen.
        Err(Failure::Other(e)) => out.error = Some(format!("{e:#}")),
    }
    store(&out);
    Some(out)
}

enum Failure {
    /// The proxy said no to the key; trying it again only brings the ban nearer.
    Refused(String),
    Other(anyhow::Error),
}

impl From<anyhow::Error> for Failure {
    fn from(e: anyhow::Error) -> Self {
        Failure::Other(e)
    }
}

/// The proxy's Claude accounts and each one's windows.
fn read(base: &str, key: &str) -> Result<Vec<Account>, Failure> {
    let files = management(base, key, "/auth-files", None)?;
    let mut accounts = Vec::new();
    for (index, name) in claude_accounts(&files) {
        let call = serde_json::json!({
            "auth_index": index,
            "method": "GET",
            "url": plan::ENDPOINT,
            "header": {
                "Authorization": "Bearer $TOKEN$",
                "anthropic-beta": "oauth-2025-04-20",
            },
        })
        .to_string();
        let got = management(base, key, "/api-call", Some(&call))
            .map_err(|f| match f {
                Failure::Refused(m) => anyhow!(m),
                Failure::Other(e) => e,
            })
            .and_then(|answer| account_usage(&answer));
        let (usage, error) = match got {
            Ok(u) => (Some(u), None),
            Err(e) => (None, Some(format!("{e:#}"))),
        };
        accounts.push(Account { name, usage, error });
    }
    Ok(accounts)
}

/// One management call with the key in a header (curl reads it from stdin,
/// so it stays out of the process list).
/// A body makes it a POST.
fn management(base: &str, key: &str, path: &str, body: Option<&str>) -> Result<Value, Failure> {
    let mut headers = vec![("Authorization", format!("Bearer {key}"))];
    if body.is_some() {
        headers.push(("Content-Type", "application/json".into()));
    }
    let (out, status) = plan::http_with(&format!("{base}{path}"), body, &headers, "")?;
    let json: Value = serde_json::from_str(&out).unwrap_or(Value::Null);
    let msg = json["error"].as_str().unwrap_or_default();
    match status {
        200..=299 => Ok(json),
        401 => Err(Failure::Refused(format!(
            "CLIProxyAPI refused the management key ({msg})"
        ))),
        403 => Err(Failure::Other(anyhow!("CLIProxyAPI: {msg}"))),
        404 => Err(Failure::Other(anyhow!(
            "no CLIProxyAPI management API at {base} (is a management key set in its config?)"
        ))),
        0 => Err(Failure::Other(anyhow!(
            "CLIProxyAPI did not answer at {base}"
        ))),
        s => Err(Failure::Other(anyhow!(
            "CLIProxyAPI: HTTP {s} {}",
            if msg.is_empty() {
                &out[..out.len().min(200)]
            } else {
                msg
            }
        ))),
    }
}

/// `(auth_index, name)` of every enabled claude.ai account in `auth-files`.
/// API keys have no plan windows, so they are left out.
fn claude_accounts(files: &Value) -> Vec<(String, String)> {
    let Some(files) = files["files"].as_array() else {
        return Vec::new();
    };
    let mut out: Vec<(String, String)> = files
        .iter()
        .filter(|f| {
            let provider = f["provider"].as_str().or_else(|| f["type"].as_str());
            provider.is_some_and(|p| p.eq_ignore_ascii_case("claude"))
                && f["disabled"].as_bool() != Some(true)
                && f["account_type"].as_str() != Some("api_key")
        })
        .filter_map(|f| {
            let index = f["auth_index"].as_str()?.to_string();
            let name = ["email", "label", "name"]
                .iter()
                .find_map(|k| f[*k].as_str().filter(|s| !s.is_empty()))
                .unwrap_or(&index)
                .to_string();
            Some((index, name))
        })
        .collect();
    out.sort_by_key(|a| a.1.to_lowercase());
    out
}

/// `api-call`'s answer (`status_code`, `body`) into the account's windows.
fn account_usage(answer: &Value) -> Result<PlanUsage> {
    let status = answer["status_code"].as_u64().unwrap_or(0);
    let body = answer["body"].as_str().unwrap_or_default();
    let json: Value =
        serde_json::from_str(body).context("the usage endpoint did not answer JSON")?;
    if let Some(msg) = json["error"]["message"]
        .as_str()
        .or_else(|| json["error"].as_str())
    {
        bail!("HTTP {status}: {msg}");
    }
    if !(200..300).contains(&status) {
        bail!("HTTP {status}");
    }
    Ok(plan::parse_usage(&json, "", ""))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn the_management_api_shares_the_proxys_origin() {
        assert_eq!(
            management_base("http://127.0.0.1:8317/v1").as_deref(),
            Some("http://127.0.0.1:8317/v0/management")
        );
        assert_eq!(
            management_base("HTTPS://user:pw@proxy.example.com/anthropic?x=1").as_deref(),
            Some("https://proxy.example.com/v0/management")
        );
        assert_eq!(management_base("localhost:8317"), None);
    }

    #[test]
    fn only_enabled_claude_sign_ins_are_read() {
        let files = json!({"files": [
            {"auth_index": "b", "provider": "claude", "email": "zoe@example.com"},
            {"auth_index": "a", "provider": "claude", "email": "amy@example.com"},
            {"auth_index": "c", "provider": "claude", "email": "off@example.com", "disabled": true},
            {"auth_index": "d", "provider": "claude", "account_type": "api_key", "label": "key"},
            {"auth_index": "e", "provider": "codex", "email": "gpt@example.com"},
            {"auth_index": "f", "type": "claude", "name": "claude-team.json"},
        ]});
        assert_eq!(
            claude_accounts(&files),
            [
                ("a".to_string(), "amy@example.com".to_string()),
                ("f".to_string(), "claude-team.json".to_string()),
                ("b".to_string(), "zoe@example.com".to_string()),
            ]
        );
        assert!(claude_accounts(&json!({"error": "x"})).is_empty());
    }

    #[test]
    fn an_api_call_answer_becomes_windows() {
        let body = json!({"limits": [
            {"kind": "session", "percent": 37, "severity": "normal", "is_active": true},
            {"kind": "weekly", "percent": 91, "severity": "warning", "is_active": true}
        ]})
        .to_string();
        let u = account_usage(&json!({"status_code": 200, "body": body})).unwrap();
        assert_eq!(
            u.windows
                .iter()
                .map(|w| (w.label.as_str(), w.percent))
                .collect::<Vec<_>>(),
            [("5-hour limit", 37), ("Weekly limit", 91)]
        );
        let expired = json!({"status_code": 401, "body": r#"{"type":"error","error":{"type":"authentication_error","message":"OAuth token has expired."}}"#});
        let err = account_usage(&expired).unwrap_err().to_string();
        assert_eq!(err, "HTTP 401: OAuth token has expired.");
    }

    #[test]
    fn the_cache_keeps_a_refused_keys_fingerprint_not_the_key() {
        let p = Proxy {
            base: "http://127.0.0.1:8317/v0/management".into(),
            refused: Some(fingerprint("secret")),
            has_key: true,
            ..Proxy::default()
        };
        let json = serde_json::to_string(&p).unwrap();
        assert!(!json.contains("secret"));
        let back: Proxy = serde_json::from_str(&json).unwrap();
        assert_eq!(back.refused, Some(fingerprint("secret")));
        assert_ne!(fingerprint("secret"), fingerprint("secret2"));
    }
}
