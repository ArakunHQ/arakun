//! Feature flags from Arakun's PostHog project, the one `telemetry` sends to.
//!
//! A check posts this build's version, channel and platform to PostHog's
//! `/flags` endpoint and gets back the flags that match them. Flags are
//! targeted by those properties (`$app_version` with PostHog's semver
//! operators), never by a person: no person profile is made, the IP gives no
//! location, and the id sent is the install's only when the user agreed to
//! usage counts (`telemetry::flags_id`). The check carries nothing a GitHub
//! update check doesn't already tell GitHub.
//!
//! Only release builds ask; debug builds, tests and `arakun dev` see no flags.

use std::collections::HashMap;
use std::time::Duration;

use serde::Deserialize;
use serde_json::{Value, json};

/// One flag as PostHog evaluated it for this build.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Flag {
    pub enabled: bool,
    /// The variant of a multivariate flag.
    pub variant: Option<String>,
    /// The JSON payload set on the flag (or on its variant), if any.
    pub payload: Option<Value>,
}

/// Whether this process may ask for flags at all.
pub fn available() -> bool {
    !cfg!(test)
        && !crate::telemetry::KEY.is_empty()
        && crate::update::release_build()
        && crate::dev::size().is_none()
}

/// The flags that match this build. Blocking: call it off the UI thread.
pub fn fetch() -> Result<HashMap<String, Flag>, String> {
    if !available() {
        return Ok(HashMap::new());
    }
    let body = request_body(&crate::telemetry::flags_id());
    let url = format!(
        "{}/flags/?v=2",
        crate::telemetry::HOST.trim_end_matches('/')
    );
    let reply = crate::http::json_call(
        crate::http::agent(Duration::from_secs(15)),
        "POST",
        &url,
        None,
        Some(body.to_string()),
        1024 * 1024,
    )
    .map_err(|e| format!("could not reach PostHog for feature flags: {e}"))?;
    if !reply.ok() {
        return Err(format!(
            "PostHog answered {} to the feature flags",
            reply.status
        ));
    }
    let text = reply.text();
    parse(&text)
}

/// The properties flags can be targeted by.
fn request_body(distinct_id: &str) -> Value {
    let os = match std::env::consts::OS {
        "macos" => "Mac OS X",
        "linux" => "Linux",
        "windows" => "Windows",
        other => other,
    };
    let channel = match crate::update::channel() {
        crate::update::Channel::Stable => "stable",
        crate::update::Channel::Nightly => "nightly",
    };
    json!({
        "api_key": crate::telemetry::KEY,
        "distinct_id": distinct_id,
        "person_properties": {
            "$app_version": env!("ARAKUN_VERSION"),
            "$os": os,
            "arch": std::env::consts::ARCH,
            "channel": channel,
        },
        "geoip_disable": true,
    })
}

/// Reads a `/flags?v=2` answer. PostHog hands the payload over as a JSON
/// string; one that isn't JSON is kept as a plain string.
fn parse(text: &str) -> Result<HashMap<String, Flag>, String> {
    #[derive(Deserialize)]
    struct Answer {
        #[serde(default)]
        flags: HashMap<String, Raw>,
        #[serde(default, rename = "errorsWhileComputingFlags")]
        errors: bool,
    }
    #[derive(Deserialize)]
    struct Raw {
        #[serde(default)]
        enabled: bool,
        #[serde(default)]
        variant: Option<String>,
        #[serde(default)]
        metadata: Option<Meta>,
    }
    #[derive(Deserialize)]
    struct Meta {
        #[serde(default)]
        payload: Option<Value>,
    }
    let answer: Answer = serde_json::from_str(text)
        .map_err(|e| format!("PostHog's feature flags can't be read: {e}"))?;
    if answer.errors && answer.flags.is_empty() {
        return Err("PostHog could not work out the feature flags".into());
    }
    Ok(answer
        .flags
        .into_iter()
        .map(|(key, raw)| {
            let payload = raw.metadata.and_then(|m| m.payload).map(|p| match p {
                Value::String(s) => serde_json::from_str(&s).unwrap_or(Value::String(s)),
                other => other,
            });
            let flag = Flag {
                enabled: raw.enabled,
                variant: raw.variant,
                payload,
            };
            (key, flag)
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_variants_and_payloads() {
        let text = r#"{
            "flags": {
                "update-policy": {
                    "key": "update-policy",
                    "enabled": true,
                    "variant": "force",
                    "metadata": {"id": 1, "payload": "{\"message\":\"A fix for a data loss bug.\"}"}
                },
                "plain": {"key": "plain", "enabled": false, "variant": null, "metadata": {"payload": "hello"}},
                "bare": {"enabled": true}
            },
            "errorsWhileComputingFlags": false
        }"#;
        let flags = parse(text).unwrap();
        let policy = &flags["update-policy"];
        assert!(policy.enabled);
        assert_eq!(policy.variant.as_deref(), Some("force"));
        assert_eq!(
            policy.payload.as_ref().unwrap()["message"],
            "A fix for a data loss bug."
        );
        assert_eq!(flags["plain"].payload, Some(Value::from("hello")));
        assert!(!flags["plain"].enabled);
        assert_eq!(
            flags["bare"],
            Flag {
                enabled: true,
                ..Flag::default()
            }
        );
    }

    #[test]
    fn an_answer_with_only_errors_is_an_error() {
        assert!(parse(r#"{"flags": {}, "errorsWhileComputingFlags": true}"#).is_err());
        assert!(parse("not json").is_err());
        assert!(parse("{}").unwrap().is_empty());
    }

    #[test]
    fn the_request_targets_the_build_not_a_person() {
        let body = request_body("abc");
        assert_eq!(body["distinct_id"], "abc");
        assert_eq!(body["geoip_disable"], true);
        assert_eq!(
            body["person_properties"]["$app_version"],
            env!("ARAKUN_VERSION")
        );
    }
}
