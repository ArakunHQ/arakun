//! The network side of Teams: GitHub's device flow, then the Teams server
//! (`teams/README.md`). Every call blocks, so callers run them off the UI
//! thread. The types mirror the server's JSON.

use std::sync::OnceLock;
use std::time::Duration;

use serde::{Deserialize, Serialize};

/// Arakun's OAuth App. Public: the device flow has no client secret, and the
/// server checks with GitHub that this app issued the token.
pub const CLIENT_ID: &str = "Ov23lipgmMeATCa5aTr3";
pub const DEFAULT_URL: &str = "https://teams.arakun.dev";
/// Overrides [`DEFAULT_URL`], for a server run with `wrangler dev` or a staging one.
pub const URL_ENV: &str = "ARAKUN_TEAMS_URL";
/// The flag the server sets on accounts that may use Teams.
pub const TEAMS_FLAG: &str = "teams";

const DEVICE_CODE_URL: &str = "https://github.com/login/device/code";
const TOKEN_URL: &str = "https://github.com/login/oauth/access_token";
const DEVICE_GRANT: &str = "urn:ietf:params:oauth:grant-type:device_code";

/// The server to talk to: [`URL_ENV`] when set, else [`DEFAULT_URL`].
pub fn base_url() -> Result<String, String> {
    base_url_from(std::env::var(URL_ENV).ok())
}

/// The session goes in a header on every call, so a plain `http://` server is
/// only accepted on this machine.
fn base_url_from(var: Option<String>) -> Result<String, String> {
    let Some(url) = var
        .map(|v| v.trim().trim_end_matches('/').to_string())
        .filter(|v| !v.is_empty())
    else {
        return Ok(DEFAULT_URL.into());
    };
    if url.starts_with("https://") || crate::github::pilot::local_http(&url) {
        Ok(url)
    } else {
        Err(format!(
            "{URL_ENV} must start with https:// (http:// only for localhost)."
        ))
    }
}

/// Who is signed in, as `GET /me` answers.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Me {
    pub github_id: u64,
    pub login: String,
    #[serde(default)]
    pub flags: Vec<String>,
}

impl Me {
    pub fn has(&self, flag: &str) -> bool {
        self.flags.iter().any(|f| f == flag)
    }
}

/// What `POST /auth/github` answers. Its `expires_at` is left out: the server
/// extends a session that is used, and a 401 says when one has ended.
#[derive(Clone, Debug, Deserialize)]
pub struct SignedIn {
    pub session: String,
    #[serde(flatten)]
    pub me: Me,
}

/// GitHub's answer to the first step of the device flow.
#[derive(Clone, Debug, Deserialize)]
pub struct DeviceCode {
    pub device_code: String,
    pub user_code: String,
    pub verification_uri: String,
    pub expires_in: u64,
    /// The seconds to wait between polls.
    #[serde(default = "default_interval")]
    pub interval: u64,
}

fn default_interval() -> u64 {
    5
}

/// One answer to a poll of the token endpoint: a token or an error code.
#[derive(Clone, Debug, Default, Deserialize)]
pub struct TokenAnswer {
    pub access_token: Option<String>,
    pub error: Option<String>,
    pub error_description: Option<String>,
    /// With `slow_down`, the interval GitHub wants from now on.
    pub interval: Option<u64>,
}

/// What to do after a poll.
#[derive(Debug, PartialEq, Eq)]
pub enum Poll {
    /// Ask again after this many seconds, and keep that pace.
    Wait(u64),
    Token(String),
    /// The code ran out before the user entered it.
    Expired,
    /// The user turned the request down on GitHub.
    Denied,
    Failed(String),
}

/// Decides the next step of the device flow from GitHub's answer, polling every
/// `interval` seconds so far. Kept apart from the network so it can be tested.
pub fn next_poll(answer: &TokenAnswer, interval: u64) -> Poll {
    if let Some(token) = answer.access_token.as_deref().filter(|t| !t.is_empty()) {
        return Poll::Token(token.to_string());
    }
    match answer.error.as_deref() {
        Some("authorization_pending") => Poll::Wait(interval),
        // GitHub adds 5 s on each slow_down, and says the new total; take
        // whichever is longer so we never poll faster than it asks.
        Some("slow_down") => Poll::Wait((interval + 5).max(answer.interval.unwrap_or(0))),
        Some("expired_token") => Poll::Expired,
        Some("access_denied") => Poll::Denied,
        Some(code) => Poll::Failed(
            answer
                .error_description
                .clone()
                .filter(|d| !d.is_empty())
                .unwrap_or_else(|| format!("GitHub answered {code}.")),
        ),
        None => Poll::Failed("GitHub sent neither a token nor an error.".into()),
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum ApiError {
    /// 401: the session or token is not valid (any more).
    Unauthorized,
    /// Any other answer than the one expected, with the server's message.
    Status(u16, String),
    /// The request did not get an answer.
    Network(String),
    /// An answer that is not the JSON expected.
    Unreadable,
}

impl std::fmt::Display for ApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ApiError::Unauthorized => write!(f, "The sign-in is no longer valid."),
            ApiError::Status(429, _) => write!(f, "Too many sign-ins; try again in a minute."),
            ApiError::Status(code, msg) if msg.is_empty() => {
                write!(f, "The server answered {code}.")
            }
            ApiError::Status(code, msg) => write!(f, "The server answered {code}: {msg}."),
            ApiError::Network(e) => write!(f, "Could not reach the server ({e})."),
            ApiError::Unreadable => write!(f, "The server's answer could not be read."),
        }
    }
}

/// Asks GitHub for a code the user enters at `verification_uri`. No scopes:
/// the server only needs to know who the user is.
pub fn device_code() -> Result<DeviceCode, ApiError> {
    let (status, body) = answer(
        agent()
            .post(DEVICE_CODE_URL)
            .header("Accept", "application/json")
            .send_form([("client_id", CLIENT_ID), ("scope", "")]),
    )?;
    if status != 200 {
        return Err(ApiError::Status(status, github_error(&body)));
    }
    serde_json::from_str(&body).map_err(|_| ApiError::Unreadable)
}

/// One poll of GitHub's token endpoint. Pending and the other device-flow
/// errors come back as a 200 with an `error`, for [`next_poll`] to read.
pub fn poll_token(device_code: &str) -> Result<TokenAnswer, ApiError> {
    let (status, body) = answer(
        agent()
            .post(TOKEN_URL)
            .header("Accept", "application/json")
            .send_form([
                ("client_id", CLIENT_ID),
                ("device_code", device_code),
                ("grant_type", DEVICE_GRANT),
            ]),
    )?;
    match serde_json::from_str::<TokenAnswer>(&body) {
        Ok(a) if status == 200 || a.error.is_some() => Ok(a),
        _ => Err(ApiError::Status(status, github_error(&body))),
    }
}

/// Trades the GitHub token for a session of the Teams server. The server asks
/// GitHub who the token belongs to and does not keep it.
pub fn sign_in(base: &str, github_token: &str) -> Result<SignedIn, ApiError> {
    let body = serde_json::json!({ "access_token": github_token }).to_string();
    let (status, body) = answer(
        server(base)
            .post(format!("{base}/auth/github"))
            .header("Content-Type", "application/json")
            .send(body),
    )?;
    json(status, &body)
}

pub fn me(base: &str, session: &str) -> Result<Me, ApiError> {
    let (status, body) = answer(
        server(base)
            .get(format!("{base}/me"))
            .header("Authorization", format!("Bearer {session}"))
            .call(),
    )?;
    json(status, &body)
}

/// Ends the session on the server.
pub fn logout(base: &str, session: &str) -> Result<(), ApiError> {
    let (status, body) = answer(
        server(base)
            .post(format!("{base}/auth/logout"))
            .header("Authorization", format!("Bearer {session}"))
            .send_empty(),
    )?;
    match status {
        200..=299 => Ok(()),
        401 => Err(ApiError::Unauthorized),
        s => Err(ApiError::Status(s, server_error(&body))),
    }
}

fn answer(
    sent: Result<ureq::http::Response<ureq::Body>, ureq::Error>,
) -> Result<(u16, String), ApiError> {
    let mut res = sent.map_err(|e| ApiError::Network(e.to_string()))?;
    let status = res.status().as_u16();
    let body = res
        .body_mut()
        .with_config()
        .limit(64 * 1024)
        .read_to_string()
        .unwrap_or_default();
    Ok((status, body))
}

fn json<T: serde::de::DeserializeOwned>(status: u16, body: &str) -> Result<T, ApiError> {
    match status {
        200 => serde_json::from_str(body).map_err(|_| ApiError::Unreadable),
        401 => Err(ApiError::Unauthorized),
        s => Err(ApiError::Status(s, server_error(body))),
    }
}

/// The server's errors are `{"error": "..."}`.
fn server_error(body: &str) -> String {
    serde_json::from_str::<serde_json::Value>(body)
        .ok()
        .and_then(|v| v["error"].as_str().map(str::to_string))
        .unwrap_or_default()
}

/// GitHub's are `{"error": "...", "error_description": "..."}`.
fn github_error(body: &str) -> String {
    serde_json::from_str::<serde_json::Value>(body)
        .ok()
        .and_then(|v| {
            v["error_description"]
                .as_str()
                .or(v["error"].as_str())
                .map(str::to_string)
        })
        .unwrap_or_default()
}

fn agent() -> &'static ureq::Agent {
    static AGENT: OnceLock<ureq::Agent> = OnceLock::new();
    AGENT.get_or_init(|| build(false))
}

/// The agent for the Teams server at `base`. Plain `http://` to this machine
/// skips the proxy from the environment (`HTTP_PROXY`, `ALL_PROXY`): it would
/// carry the session off the machine unencrypted. `https://` keeps it.
fn server(base: &str) -> &'static ureq::Agent {
    static DIRECT: OnceLock<ureq::Agent> = OnceLock::new();
    if crate::github::pilot::local_http(base) {
        DIRECT.get_or_init(|| build(true))
    } else {
        agent()
    }
}

fn build(direct: bool) -> ureq::Agent {
    let mut config = ureq::Agent::config_builder();
    if direct {
        config = config.proxy(None);
    }
    config
        .timeout_global(Some(Duration::from_secs(20)))
        .http_status_as_error(false)
        .user_agent(concat!("arakun/", env!("CARGO_PKG_VERSION")))
        .build()
        .into()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn answer(json: &str) -> TokenAnswer {
        serde_json::from_str(json).unwrap()
    }

    #[test]
    fn only_a_local_http_server_skips_the_proxy() {
        let direct = server("http://localhost:8787");
        assert!(std::ptr::eq(direct, server("http://127.0.0.1:8787")));
        assert!(!std::ptr::eq(direct, agent()));
        assert!(std::ptr::eq(server(DEFAULT_URL), agent()));
    }

    #[test]
    fn pending_waits_at_the_same_pace() {
        let a = answer(r#"{"error": "authorization_pending"}"#);
        assert_eq!(next_poll(&a, 5), Poll::Wait(5));
    }

    #[test]
    fn slow_down_adds_five_seconds() {
        let a = answer(r#"{"error": "slow_down"}"#);
        assert_eq!(next_poll(&a, 5), Poll::Wait(10));
        // GitHub's own interval wins when it asks for more.
        let a = answer(r#"{"error": "slow_down", "interval": 15}"#);
        assert_eq!(next_poll(&a, 5), Poll::Wait(15));
        let a = answer(r#"{"error": "slow_down", "interval": 3}"#);
        assert_eq!(next_poll(&a, 10), Poll::Wait(15));
    }

    #[test]
    fn a_token_ends_the_flow() {
        let a = answer(r#"{"access_token": "gho_x", "token_type": "bearer", "scope": ""}"#);
        assert_eq!(next_poll(&a, 5), Poll::Token("gho_x".into()));
    }

    #[test]
    fn expired_and_denied_stop() {
        assert_eq!(
            next_poll(&answer(r#"{"error": "expired_token"}"#), 5),
            Poll::Expired
        );
        assert_eq!(
            next_poll(&answer(r#"{"error": "access_denied"}"#), 5),
            Poll::Denied
        );
    }

    #[test]
    fn other_errors_fail_with_githubs_words() {
        let a = answer(
            r#"{"error": "incorrect_device_code", "error_description": "The device_code provided is not valid."}"#,
        );
        assert_eq!(
            next_poll(&a, 5),
            Poll::Failed("The device_code provided is not valid.".into())
        );
        let a = answer(r#"{"error": "unsupported_grant_type"}"#);
        assert_eq!(
            next_poll(&a, 5),
            Poll::Failed("GitHub answered unsupported_grant_type.".into())
        );
        assert!(matches!(next_poll(&answer("{}"), 5), Poll::Failed(_)));
        // An empty token is no token.
        assert!(matches!(
            next_poll(&answer(r#"{"access_token": ""}"#), 5),
            Poll::Failed(_)
        ));
    }

    #[test]
    fn reads_me() {
        let me: Me = serde_json::from_str(
            r#"{"github_id": 583231, "login": "octocat", "flags": ["teams", "beta"]}"#,
        )
        .unwrap();
        assert_eq!(me.github_id, 583231);
        assert_eq!(me.login, "octocat");
        assert!(me.has(TEAMS_FLAG));
        assert!(!me.has("team"));

        let me: Me =
            serde_json::from_str(r#"{"github_id": 1, "login": "a", "flags": []}"#).unwrap();
        assert!(!me.has(TEAMS_FLAG));
        // A server that leaves flags out means none.
        let me: Me = serde_json::from_str(r#"{"github_id": 1, "login": "a"}"#).unwrap();
        assert!(me.flags.is_empty());
        assert!(serde_json::from_str::<Me>(r#"{"login": "a"}"#).is_err());
    }

    #[test]
    fn reads_a_sign_in() {
        let s: SignedIn = serde_json::from_str(
            r#"{"session": "abc", "expires_at": 1800000000, "github_id": 2, "login": "b", "flags": ["teams"]}"#,
        )
        .unwrap();
        assert_eq!(s.session, "abc");
        assert_eq!(s.me.login, "b");
        assert!(s.me.has(TEAMS_FLAG));
    }

    #[test]
    fn reads_errors() {
        assert_eq!(json::<Me>(401, "{}"), Err(ApiError::Unauthorized));
        assert_eq!(
            json::<Me>(429, r#"{"error": "too many sign-ins"}"#),
            Err(ApiError::Status(429, "too many sign-ins".into()))
        );
        assert_eq!(json::<Me>(200, "<html>"), Err(ApiError::Unreadable));
    }

    #[test]
    fn url_defaults_and_overrides() {
        assert_eq!(base_url_from(None).unwrap(), DEFAULT_URL);
        assert_eq!(base_url_from(Some("  ".into())).unwrap(), DEFAULT_URL);
        assert_eq!(
            base_url_from(Some("https://teams.example.dev/".into())).unwrap(),
            "https://teams.example.dev"
        );
        assert_eq!(
            base_url_from(Some("http://localhost:8787".into())).unwrap(),
            "http://localhost:8787"
        );
        assert!(base_url_from(Some("http://teams.example.dev".into())).is_err());
        assert!(base_url_from(Some("teams.example.dev".into())).is_err());
    }
}
