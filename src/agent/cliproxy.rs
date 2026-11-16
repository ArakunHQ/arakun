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
//! and a key the proxy refused is not tried again. The key is saved with the
//! proxy it was typed for and only ever sent there, never in the clear to
//! another machine: switch Claude Code to another endpoint and the tab asks again.

use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context as _, Result, anyhow, bail};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::plan::{self, PlanUsage};

/// Refetch at most this often; `refresh` serves the cache in between.
const STALE: u64 = 5 * 60;

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

/// The management key as saved from the Usage tab, with the management base
/// it was typed for. Never logged or shown.
#[derive(Serialize, Deserialize)]
struct SavedKey {
    base: String,
    key: String,
}

fn saved_key() -> Option<SavedKey> {
    serde_json::from_slice(&std::fs::read(key_path()).ok()?).ok()
}

/// The saved key, only when it was typed for `base`.
fn key_for(saved: Option<SavedKey>, base: &str) -> Option<String> {
    saved
        .filter(|s| s.base == base)
        .map(|s| s.key.trim().to_string())
        .filter(|k| !k.is_empty())
}

/// The management base of the endpoint Claude Code runs on now.
fn current_base() -> Option<String> {
    management_base(&super::claude_auth::endpoint_url()?)
}

/// Saves the management key for `base`, the proxy the tab showed when the key
/// was typed (not whatever Claude Code runs on by the time this runs), where
/// only this user can read it.
pub fn save_key(key: &str, base: &str) -> Result<()> {
    let saved = SavedKey {
        base: base.to_string(),
        key: key.trim().to_string(),
    };
    crate::durable::write_private(&key_path(), &serde_json::to_vec(&saved)?)?;
    Ok(())
}

/// Whether the key may travel to `base`: over https, or plain http to this
/// machine. Anything else would send it in the clear.
fn safe_to_send(base: &str) -> bool {
    base.starts_with("https://") || (base.starts_with("http://") && loopback(base))
}

/// Whether `base` is on this machine: "localhost", "127.0.0.1:8317", "[::1]".
fn loopback(base: &str) -> bool {
    let rest = base.split_once("://").map_or(base, |(_, r)| r);
    let host = rest.split('/').next().unwrap_or_default();
    let host = match host.strip_prefix('[') {
        Some(v6) => v6.split(']').next().unwrap_or_default(),
        None => host.rsplit_once(':').map_or(host, |(h, _)| h),
    };
    host.eq_ignore_ascii_case("localhost")
        || host
            .parse::<std::net::IpAddr>()
            .is_ok_and(|ip| ip.is_loopback())
}

/// At most `n` characters of `s`, cut on a character boundary.
fn clip(s: &str, n: usize) -> String {
    s.chars().take(n).collect()
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
    let base = current_base()?;
    let key = key_for(saved_key(), &base);
    let cache = cached().filter(|c| c.base == base);
    let (out, read_now) = decide(base, key.as_deref(), cache, now(), force, read);
    if read_now {
        store(&out);
    }
    Some(out)
}

/// [`refresh`] without the files: whether to call `read` and what to show.
/// The second value says `read` ran and the cache should be written.
fn decide(
    base: String,
    key: Option<&str>,
    cache: Option<Proxy>,
    at: u64,
    force: bool,
    read: impl FnOnce(&str, &str) -> Result<Vec<Account>, Failure>,
) -> (Proxy, bool) {
    let mut out = cache.unwrap_or_else(|| Proxy {
        base: base.clone(),
        ..Proxy::default()
    });
    out.has_key = key.is_some();
    let Some(key) = key else {
        return (out, false);
    };
    if !safe_to_send(&base) {
        out.error = Some(
            "The management key is only sent over https or to this machine; use an https address for the proxy."
                .into(),
        );
        return (out, false);
    }
    let print = fingerprint(key);
    if out.refused == Some(print) {
        return (out, false);
    }
    let fresh = out.refused.is_none() && at.saturating_sub(out.at) < STALE && out.at > 0;
    if fresh && !force {
        return (out, false);
    }
    out.at = at;
    out.refused = None;
    match read(&base, key) {
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
    (out, true)
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
    read_with(|path, body| management(base, key, path, body))
}

/// [`read`] with the management calls passed in: `call(path, body)`.
fn read_with(
    mut call: impl FnMut(&str, Option<&str>) -> Result<Value, Failure>,
) -> Result<Vec<Account>, Failure> {
    let files = call("/auth-files", None)?;
    let mut accounts = Vec::new();
    for (index, name) in claude_accounts(&files) {
        let body = serde_json::json!({
            "auth_index": index,
            "method": "GET",
            "url": plan::ENDPOINT,
            "header": {
                "Authorization": "Bearer $TOKEN$",
                "anthropic-beta": "oauth-2025-04-20",
            },
        })
        .to_string();
        let got = match call("/api-call", Some(&body)) {
            Ok(answer) => account_usage(&answer),
            // The key stopped working: stop, rather than send it once per account.
            Err(refused @ Failure::Refused(_)) => return Err(refused),
            Err(Failure::Other(e)) => Err(e),
        };
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
    let url = format!("{base}{path}");
    // A proxy in the environment would see the key in the clear on its way here.
    let (out, status) = plan::http_status(&url, body, &headers, loopback(base))?;
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
                clip(&out, 200)
            } else {
                msg.to_string()
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

    const LOCAL: &str = "http://127.0.0.1:8317/v0/management";

    fn never(_: &str, _: &str) -> Result<Vec<Account>, Failure> {
        panic!("nothing may reach the proxy here")
    }

    fn refused(_: &str, _: &str) -> Result<Vec<Account>, Failure> {
        Err(Failure::Refused(
            "CLIProxyAPI refused the management key".into(),
        ))
    }

    fn one_account(_: &str, _: &str) -> Result<Vec<Account>, Failure> {
        Ok(vec![Account {
            name: "amy@example.com".into(),
            usage: None,
            error: None,
        }])
    }

    #[test]
    fn nothing_is_sent_without_a_key() {
        let (p, read) = decide(LOCAL.into(), None, None, 1_000, true, never);
        assert!(!read && !p.has_key && p.error.is_none());
    }

    #[test]
    fn a_refused_key_is_sent_once() {
        let (p, read) = decide(LOCAL.into(), Some("bad"), None, 1_000, false, refused);
        assert!(read && p.refused.is_some() && p.error.is_some());
        // Not again, even when forced or long after.
        let (p, read) = decide(LOCAL.into(), Some("bad"), Some(p), 99_000, true, never);
        assert!(!read && p.error.is_some());
        // A new key is tried.
        let (p, read) = decide(
            LOCAL.into(),
            Some("good"),
            Some(p),
            99_000,
            false,
            one_account,
        );
        assert!(read && p.refused.is_none() && p.error.is_none());
        assert_eq!(p.accounts.len(), 1);
    }

    #[test]
    fn a_fresh_read_is_served_from_the_cache() {
        let (p, _) = decide(LOCAL.into(), Some("k"), None, 1_000, false, one_account);
        let (p, read) = decide(
            LOCAL.into(),
            Some("k"),
            Some(p),
            1_000 + STALE - 1,
            false,
            never,
        );
        assert!(!read);
        let (_, read) = decide(LOCAL.into(), Some("k"), Some(p), 1_001, true, one_account);
        assert!(read, "a saved key reads at once");
    }

    #[test]
    fn the_key_only_goes_to_the_proxy_it_was_typed_for() {
        let saved = || {
            Some(SavedKey {
                base: LOCAL.into(),
                key: "k".into(),
            })
        };
        assert_eq!(key_for(saved(), LOCAL).as_deref(), Some("k"));
        assert_eq!(
            key_for(saved(), "https://gw.example.com/v0/management"),
            None
        );
        assert_eq!(key_for(None, LOCAL), None);
    }

    #[test]
    fn the_key_never_travels_in_the_clear() {
        for base in [
            LOCAL,
            "http://localhost:8317/v0/management",
            "http://[::1]:8317/v0/management",
            "https://proxy.example.com/v0/management",
        ] {
            assert!(safe_to_send(base), "{base}");
        }
        for base in [
            "http://proxy.example.com:8317/v0/management",
            "http://192.168.1.5:8317/v0/management",
            "http://127.0.0.1.example.com/v0/management",
        ] {
            assert!(!safe_to_send(base), "{base}");
            let (p, read) = decide(base.into(), Some("k"), None, 1_000, true, never);
            assert!(!read && p.error.is_some(), "{base}");
        }
    }

    #[test]
    fn a_key_refused_midway_is_not_sent_to_the_other_accounts() {
        let mut calls = Vec::new();
        let got = read_with(|path, _| {
            calls.push(path.to_string());
            match path {
                "/auth-files" => Ok(json!({"files": [
                    {"auth_index": "a", "provider": "claude", "email": "a@example.com"},
                    {"auth_index": "b", "provider": "claude", "email": "b@example.com"},
                ]})),
                _ => Err(Failure::Refused("refused".into())),
            }
        });
        assert!(matches!(got, Err(Failure::Refused(_))));
        assert_eq!(calls, ["/auth-files", "/api-call"]);
    }

    #[test]
    fn only_this_machine_skips_the_environments_proxy() {
        assert!(loopback(LOCAL));
        assert!(loopback("http://localhost:8317/v0/management"));
        assert!(loopback("http://[::1]:8317/v0/management"));
        assert!(!loopback("https://proxy.example.com/v0/management"));
        assert!(!loopback("http://127.0.0.1.example.com/v0/management"));
    }

    #[test]
    fn long_error_bodies_are_cut_on_a_character() {
        let body = "é".repeat(150);
        assert_eq!(clip(&body, 200).chars().count(), 150);
        assert_eq!(clip(&"€".repeat(300), 200).chars().count(), 200);
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
