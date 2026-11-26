//! Linear's side of a delegated issue, shared by the app and `arakun server`:
//! the GraphQL call, the issue reads, the guard over the mappings, the
//! delegated prompt, the record of runs, and the reports back to Linear.
//!
//! Nothing here keeps a key. The caller passes it in for each call, from the
//! app's keyring or the server's own file, and it goes to `api.linear.app`
//! in a header only.

pub mod delegation;
pub mod env;
pub mod events;
pub mod follow;
pub mod issues;
pub mod outbox;
pub mod prompt;
pub mod runs;
pub mod start;

use std::sync::OnceLock;
use std::time::Duration;

use serde_json::{Value, json};

#[derive(Clone, Debug, PartialEq)]
pub enum ApiError {
    /// The key was revoked or is not valid.
    SignedOut,
    /// Nothing more until this unix second.
    Limited {
        until: u64,
    },
    Failed(String),
}

impl From<crate::guard::Panicked> for ApiError {
    fn from(panicked: crate::guard::Panicked) -> Self {
        ApiError::Failed(panicked.to_string())
    }
}

impl ApiError {
    pub fn message(&self) -> String {
        match self {
            ApiError::SignedOut => "Linear refused the sign-in. Sign in again.".into(),
            ApiError::Failed(m) => m.clone(),
            ApiError::Limited { until } => {
                format!("Linear asked to wait until {}.", crate::when::clock(*until))
            }
        }
    }
}

/// Turns a status and body into the `data` object or the error to show.
/// Linear reports most failures as a 200 (or 400) with an `errors` list.
pub fn answer(status: u16, retry: Option<&str>, text: &str, now: u64) -> Result<Value, ApiError> {
    let v: Value = serde_json::from_str(text).unwrap_or(Value::Null);
    let first = &v["errors"][0];
    let code = first["extensions"]["code"].as_str();
    if status == 401 || code == Some("AUTHENTICATION_ERROR") {
        return Err(ApiError::SignedOut);
    }
    if status == 429 || code == Some("RATELIMITED") {
        return Err(ApiError::Limited {
            until: crate::when::retry_after(retry, now),
        });
    }
    if let Some(m) = first["message"].as_str() {
        return Err(ApiError::Failed(m.to_string()));
    }
    if !(200..300).contains(&status) {
        return Err(ApiError::Failed(format!("Linear answered {status}.")));
    }
    match v.get("data") {
        Some(d) if d.is_object() => Ok(d.clone()),
        _ => Err(ApiError::Failed("Linear sent no data.".into())),
    }
}

const ENDPOINT: &str = "https://api.linear.app/graphql";
/// The most bytes of an answer read: a long issue with its comments fits.
const ANSWER_LIMIT: u64 = 16 * 1024 * 1024;

fn http() -> &'static ureq::Agent {
    static AGENT: OnceLock<ureq::Agent> = OnceLock::new();
    AGENT.get_or_init(|| {
        ureq::Agent::config_builder()
            .http_status_as_error(false)
            .user_agent(concat!("arakun/", env!("ARAKUN_VERSION")))
            .timeout_global(Some(Duration::from_secs(30)))
            .build()
            .into()
    })
}

/// Runs `query` with a personal API key and returns its `data` object.
/// Linear takes the key as it is, without `Bearer`, and only in the header.
pub fn call(key: &str, query: &str, variables: Value) -> Result<Value, ApiError> {
    let now = crate::model::unix_now();
    let body = json!({ "query": query, "variables": variables }).to_string();
    let request = ureq::http::Request::builder()
        .method("POST")
        .uri(ENDPOINT)
        .header("Content-Type", "application/json")
        .header("Authorization", key)
        .body(body)
        .map_err(|e| ApiError::Failed(format!("Couldn't reach Linear: {e}")))?;
    let mut reply = http()
        .run(request)
        .map_err(|e| ApiError::Failed(format!("Couldn't reach Linear: {e}")))?;
    let status = reply.status().as_u16();
    let retry = reply
        .headers()
        .get("retry-after")
        .and_then(|h| h.to_str().ok())
        .map(str::to_string);
    let text = reply
        .body_mut()
        .with_config()
        .limit(ANSWER_LIMIT)
        .read_to_string()
        .map_err(|e| ApiError::Failed(format!("Linear's answer was cut short: {e}")))?;
    answer(status, retry.as_deref(), &text, now)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_good_reply_gives_its_data() {
        let v = answer(200, None, r#"{"data":{"viewer":{"id":"u1"}}}"#, 100).unwrap();
        assert_eq!(v["viewer"]["id"], "u1");
    }

    #[test]
    fn a_401_is_signed_out() {
        assert_eq!(answer(401, None, "{}", 100), Err(ApiError::SignedOut));
    }

    #[test]
    fn an_authentication_error_in_a_200_is_signed_out() {
        let body = r#"{"errors":[{"message":"x","extensions":{"code":"AUTHENTICATION_ERROR"}}]}"#;
        assert_eq!(answer(200, None, body, 100), Err(ApiError::SignedOut));
    }

    #[test]
    fn a_rate_limit_says_when() {
        assert_eq!(
            answer(429, Some("30"), "{}", 100),
            Err(ApiError::Limited { until: 130 })
        );
        let body = r#"{"errors":[{"message":"x","extensions":{"code":"RATELIMITED"}}]}"#;
        // No Retry-After: wait a minute.
        assert_eq!(
            answer(400, None, body, 100),
            Err(ApiError::Limited { until: 160 })
        );
    }

    #[test]
    fn another_error_carries_linears_message() {
        let body = r#"{"errors":[{"message":"Entity not found"}]}"#;
        assert_eq!(
            answer(200, None, body, 100),
            Err(ApiError::Failed("Entity not found".into()))
        );
    }
}
