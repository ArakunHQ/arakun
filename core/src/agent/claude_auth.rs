//! How Claude Code reaches a model on this machine: a claude.ai sign-in, an
//! API key, or another endpoint such as a gateway or a local proxy
//! (CLIProxyAPI and the like). Claude Code reads the `env` block of
//! ~/.claude/settings.json over its own environment, so this does too.
//!
//! The one-shot runs (quick answers, the branch namer) start Claude Code with
//! `--setting-sources ""` so that no hooks, plugins or project files load. That
//! also drops the `env` block and `apiKeyHelper`, which is where an API key or
//! a proxy is usually set up; [`one_shot`] puts them back.

use std::collections::BTreeMap;
use std::process::Command;

use serde_json::Value;
use ureq::http::Uri;

const API_KEY: &str = "ANTHROPIC_API_KEY";
const AUTH_TOKEN: &str = "ANTHROPIC_AUTH_TOKEN";
const BASE_URL: &str = "ANTHROPIC_BASE_URL";

/// What Claude Code bills when it is not a claude.ai plan.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Access {
    /// An Anthropic API key: `ANTHROPIC_API_KEY`, `ANTHROPIC_AUTH_TOKEN` or `apiKeyHelper`.
    ApiKey,
    /// Another endpoint (`ANTHROPIC_BASE_URL`), by its host.
    Endpoint(String),
    /// Amazon Bedrock, Google Vertex AI or Microsoft Foundry.
    Cloud(&'static str),
}

impl Access {
    /// "API key", "Endpoint · localhost:8317", "Amazon Bedrock". The host is
    /// hidden in redacted mode: a company gateway names the company.
    pub fn label(&self) -> String {
        match self {
            Access::ApiKey => "API key".into(),
            Access::Endpoint(host) => format!("Endpoint · {}", crate::redact::value(host)),
            Access::Cloud(name) => (*name).into(),
        }
    }
}

/// Claude Code's own settings as it sees them: the `env` block and the
/// `apiKeyHelper` of ~/.claude/settings.json.
#[derive(Clone, Debug, Default)]
struct UserSettings {
    env: BTreeMap<String, String>,
    // Not a secret: the command that prints the key, not the key.
    api_key_helper: Option<String>,
}

fn user_settings() -> UserSettings {
    let path = crate::platform::home()
        .join(".claude")
        .join("settings.json");
    std::fs::read_to_string(path)
        .ok()
        .and_then(|s| serde_json::from_str::<Value>(&s).ok())
        .map(|j| parse_settings(&j))
        .unwrap_or_default()
}

fn parse_settings(j: &Value) -> UserSettings {
    let env = j["env"]
        .as_object()
        .map(|o| {
            o.iter()
                .filter_map(|(k, v)| {
                    // Claude Code takes numbers and booleans here too.
                    let v = match v {
                        Value::String(s) => s.clone(),
                        Value::Number(n) => n.to_string(),
                        Value::Bool(b) => b.to_string(),
                        _ => return None,
                    };
                    Some((k.clone(), v))
                })
                .collect()
        })
        .unwrap_or_default();
    UserSettings {
        env,
        api_key_helper: j["apiKeyHelper"]
            .as_str()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string),
    }
}

/// How Claude Code reaches the model when it is not on a claude.ai plan, or
/// `None` when nothing but a sign-in is set up. Reads files; keep it off the UI thread.
pub fn access() -> Option<Access> {
    let settings = user_settings();
    let access = access_from(&settings, |k| std::env::var(k).ok());
    if let Ok(mut latest) = LATEST.lock() {
        *latest = access.clone();
    }
    access
}

static LATEST: std::sync::Mutex<Option<Access>> = std::sync::Mutex::new(None);

/// The `ANTHROPIC_BASE_URL` Claude Code runs on, when it is another endpoint
/// and no cloud provider wins over it. Reads files; keep it off the UI thread.
pub fn endpoint_url() -> Option<String> {
    let settings = user_settings();
    let process = |k: &str| std::env::var(k).ok();
    matches!(access_from(&settings, process), Some(Access::Endpoint(_)))
        .then(|| setting(&settings, BASE_URL, process))
        .flatten()
}

/// One variable as Claude Code sees it: the settings' `env` over the process.
fn setting(
    settings: &UserSettings,
    key: &str,
    process: impl Fn(&str) -> Option<String>,
) -> Option<String> {
    settings
        .env
        .get(key)
        .cloned()
        .or_else(|| process(key))
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
}

/// The last [`access`] any view read, for surfaces that only render (the phone).
pub fn latest() -> Option<Access> {
    LATEST.lock().ok()?.clone()
}

/// Claude Code's order: a cloud provider, then any other endpoint, then a key.
fn access_from(
    settings: &UserSettings,
    process: impl Fn(&str) -> Option<String>,
) -> Option<Access> {
    let get = |k: &str| setting(settings, k, &process);
    let on = |k: &str| get(k).is_some_and(|v| !matches!(v.as_str(), "0" | "false"));
    for (flag, name) in [
        ("CLAUDE_CODE_USE_BEDROCK", "Amazon Bedrock"),
        ("CLAUDE_CODE_USE_VERTEX", "Google Vertex AI"),
        ("CLAUDE_CODE_USE_FOUNDRY", "Microsoft Foundry"),
    ] {
        if on(flag) {
            return Some(Access::Cloud(name));
        }
    }
    if let Some(host) = get(BASE_URL).and_then(|u| custom_host(&u)) {
        return Some(Access::Endpoint(host));
    }
    (get(AUTH_TOKEN).is_some() || get(API_KEY).is_some() || settings.api_key_helper.is_some())
        .then_some(Access::ApiKey)
}

/// The host of a base URL that is not Anthropic's own API.
fn custom_host(url: &str) -> Option<String> {
    if url.is_empty() {
        return None;
    }
    let uri = url.parse::<Uri>().ok();
    let Some(authority) = uri.as_ref().and_then(Uri::authority) else {
        // An unrecognized destination is not permission to use a saved seat.
        return Some("unrecognized endpoint".into());
    };
    let raw = authority.as_str();
    let name = raw.strip_suffix(":443").unwrap_or(raw);
    if uri.as_ref().and_then(Uri::scheme_str) == Some("https")
        && name
            .strip_suffix('.')
            .unwrap_or(name)
            .eq_ignore_ascii_case("api.anthropic.com")
    {
        return None;
    }
    // The authority can contain credentials. Never put them in the label.
    Some(raw.rsplit('@').next().unwrap_or(raw).to_ascii_lowercase())
}

/// Sets up `cmd`, a `claude -p --setting-sources ""` run, to reach the model
/// the way the user's Claude Code does. Call it before setting any variable of
/// the run's own: the settings' `env` would override it.
///
/// A key in arakun's own environment is often there for other tools. With a
/// claude.ai sign-in and no other endpoint, it is dropped so the run counts
/// against the plan and doesn't bill the key. Keys and endpoints set in
/// Claude Code's own settings always apply.
pub fn one_shot(cmd: &mut Command) -> std::io::Result<()> {
    let settings = user_settings();
    let signed_in = super::plan::has_sign_in();
    let plan = drops_process_key(&settings, signed_in, |k| std::env::var(k).ok());
    if plan {
        cmd.env_remove(API_KEY).env_remove(AUTH_TOKEN);
    }
    cmd.envs(&settings.env);
    let dir = crate::platform::app_dir();
    match &settings.api_key_helper {
        // Only flag settings load next to `--setting-sources ""`.
        Some(helper) => {
            cmd.arg("--settings").arg(helper_settings(&dir, helper)?);
        }
        None => {
            let _ = std::fs::remove_file(dir.join(HELPER_SETTINGS));
        }
    }
    Ok(())
}

/// The file that hands a one-shot run the user's `apiKeyHelper`.
const HELPER_SETTINGS: &str = crate::paths::CLAUDE_KEY_HELPER.name();

/// Writes `{"apiKeyHelper": helper}` to a file in `dir` only this user can
/// read, and returns its path. A helper can hold a key (`echo sk-ant-…`), and
/// other local users can read a process's arguments, so it never goes there.
fn helper_settings(dir: &std::path::Path, helper: &str) -> std::io::Result<std::path::PathBuf> {
    let path = dir.join(HELPER_SETTINGS);
    let json = serde_json::json!({ "apiKeyHelper": helper }).to_string();
    crate::durable::write_private(&path, json.as_bytes())?;
    Ok(path)
}

/// Whether [`one_shot`] drops the key in arakun's environment: only with a
/// claude.ai sign-in and no other endpoint, where the key would win over the plan.
fn drops_process_key(
    settings: &UserSettings,
    signed_in: bool,
    process: impl Fn(&str) -> Option<String>,
) -> bool {
    signed_in
        && !matches!(
            access_from(settings, process),
            Some(Access::Endpoint(_) | Access::Cloud(_))
        )
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn none(_: &str) -> Option<String> {
        None
    }

    #[test]
    fn a_proxy_in_the_settings_is_an_endpoint() {
        let s = parse_settings(&json!({"env": {
            "ANTHROPIC_BASE_URL": "http://127.0.0.1:8317/v1",
            "ANTHROPIC_AUTH_TOKEN": "sk-dummy",
        }}));
        assert_eq!(
            access_from(&s, none),
            Some(Access::Endpoint("127.0.0.1:8317".into()))
        );
    }

    #[test]
    fn a_key_alone_is_an_api_key() {
        let s = UserSettings::default();
        let env = |k: &str| (k == API_KEY).then(|| "sk-ant-x".to_string());
        assert_eq!(access_from(&s, env), Some(Access::ApiKey));
        let helper = parse_settings(&json!({"apiKeyHelper": "~/bin/key.sh"}));
        assert_eq!(access_from(&helper, none), Some(Access::ApiKey));
    }

    #[test]
    fn anthropics_own_url_is_not_another_endpoint() {
        let s =
            parse_settings(&json!({"env": {"ANTHROPIC_BASE_URL": "https://api.anthropic.com"}}));
        assert_eq!(access_from(&s, none), None);
    }

    #[test]
    fn a_saved_seat_requires_the_official_https_origin() {
        for url in [
            "http://api.anthropic.com",
            "http://api.anthropic.com:443/v1",
            "api.anthropic.com",
            "https://api.anthropic.com:8443",
            "https://api.anthropic.com.example.test",
            "https://user@api.anthropic.com",
            "https:///v1",
        ] {
            let settings = parse_settings(&json!({"env": {BASE_URL: url}}));
            assert!(
                matches!(access_from(&settings, none), Some(Access::Endpoint(_))),
                "{url}"
            );
            assert!(!drops_process_key(&settings, true, none), "{url}");
        }
    }

    #[test]
    fn the_settings_win_over_the_process() {
        let s = parse_settings(&json!({"env": {"CLAUDE_CODE_USE_BEDROCK": "0"}}));
        let env = |k: &str| (k == "CLAUDE_CODE_USE_BEDROCK").then(|| "1".to_string());
        assert_eq!(access_from(&s, env), None);
        let s = parse_settings(&json!({"env": {"CLAUDE_CODE_USE_VERTEX": 1}}));
        assert_eq!(
            access_from(&s, none),
            Some(Access::Cloud("Google Vertex AI"))
        );
    }

    #[test]
    fn the_host_leaves_out_credentials_and_paths() {
        assert_eq!(
            custom_host("https://user:pw@gw.example.com/anthropic?x=1").as_deref(),
            Some("gw.example.com")
        );
        assert_eq!(custom_host("").as_deref(), None);
        assert_eq!(
            custom_host("https://API.Anthropic.com:443/").as_deref(),
            None
        );
        assert_eq!(
            custom_host("https://api.anthropic.com./v1").as_deref(),
            None
        );
        assert_eq!(
            custom_host("http://LocalHost:8317").as_deref(),
            Some("localhost:8317")
        );
    }

    #[test]
    fn the_key_helper_goes_in_a_private_file() {
        let dir = std::env::temp_dir().join(format!("arakun-helper-{}", std::process::id()));
        let path = helper_settings(&dir, "echo sk-ant-x").unwrap();
        let written: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(written, json!({"apiKeyHelper": "echo sk-ant-x"}));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600, "other users must not read the helper");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_stray_key_is_dropped_only_for_a_plan_on_anthropics_api() {
        let key = |k: &str| (k == API_KEY).then(|| "sk-ant-x".to_string());
        let s = UserSettings::default();
        assert!(drops_process_key(&s, true, key));
        assert!(!drops_process_key(&s, false, key));
        let proxy = |k: &str| match k {
            BASE_URL => Some("http://localhost:8317".to_string()),
            AUTH_TOKEN => Some("dummy".to_string()),
            _ => None,
        };
        assert!(!drops_process_key(&s, true, proxy));
    }
}
