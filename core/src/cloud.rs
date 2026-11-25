//! The account client: GitHub's device flow, then the Arakun service
//! (`cloud/`) that turns a GitHub token into a session. The app and
//! `arakun-server` both sign in through it. Every call blocks, so callers run
//! them off any UI thread.

use std::collections::HashMap;
use std::fmt;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use serde::Deserialize;

pub const DEFAULT_URL: &str = "https://app.arakun.dev";
/// Arakun's OAuth App, which [`DEFAULT_URL`] checks tokens against. Public:
/// the device flow has no client secret, and the server checks with GitHub
/// that this app issued the token.
pub const CLIENT_ID: &str = "Ov23lipgmMeATCa5aTr3";
/// Overrides [`DEFAULT_URL`], for a server run with `wrangler dev` or a staging
/// one. Named for Teams because people already have it set.
pub const URL_ENV: &str = "ARAKUN_TEAMS_URL";

const DEVICE_CODE_URL: &str = "https://github.com/login/device/code";
const TOKEN_URL: &str = "https://github.com/login/oauth/access_token";
const DEVICE_GRANT: &str = "urn:ietf:params:oauth:grant-type:device_code";
/// The service keeps the account's primary address, so it can reach people
/// about their account.
const SCOPE: &str = "user:email";

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
    if url.starts_with("https://") || local_http(&url) {
        Ok(url)
    } else {
        Err(format!(
            "{URL_ENV} must start with https:// (http:// only for localhost)."
        ))
    }
}

/// Plain `http://` only to this machine: tokens must never cross a network
/// unencrypted.
pub fn local_http(url: &str) -> bool {
    let Some(rest) = url.strip_prefix("http://") else {
        return false;
    };
    let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
    if authority.contains('@') {
        return false;
    }
    let host = match authority.find(']') {
        Some(end) if authority.starts_with('[') => {
            let port = &authority[end + 1..];
            if !port.is_empty() && !port.starts_with(':') {
                return false;
            }
            &authority[..=end]
        }
        _ => authority.split(':').next().unwrap_or_default(),
    };
    matches!(
        host.to_ascii_lowercase().as_str(),
        "localhost" | "127.0.0.1" | "[::1]"
    )
}

/// A URL's origin as scheme, host and port, lower case, with the default port
/// filled in: `None` for one that is not `http` or `https`.
pub fn origin(url: &str) -> Option<String> {
    let url = url.trim();
    let (scheme, rest) = url.split_once("://")?;
    let scheme = scheme.to_ascii_lowercase();
    let default = match scheme.as_str() {
        "https" => 443,
        "http" => 80,
        _ => return None,
    };
    let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
    if authority.is_empty() || authority.contains('@') {
        return None;
    }
    let authority = authority.to_ascii_lowercase();
    let (host, port) = match authority.rfind(':') {
        Some(i) if !authority[i + 1..].contains(']') => {
            (&authority[..i], authority[i + 1..].parse::<u16>().ok()?)
        }
        _ => (authority.as_str(), default),
    };
    if host.is_empty() {
        return None;
    }
    Some(format!("{scheme}://{host}:{port}"))
}

/// Whether two addresses are the same server. A session goes only to the
/// server that issued it, so this is asked before one is sent.
pub fn same_origin(a: &str, b: &str) -> bool {
    matches!((origin(a), origin(b)), (Some(x), Some(y)) if x == y)
}

/// What stands in a `Debug` print where a secret is.
const HIDDEN: &str = "(hidden)";

/// GitHub's answer to the first step of the device flow. `Debug` leaves the
/// device code out: with it, a poll gets the token.
#[derive(Clone, PartialEq, Eq)]
pub struct DeviceCode {
    pub user_code: String,
    pub verification_uri: String,
    pub device_code: String,
    /// How long to wait between polls.
    pub interval: Duration,
    pub expires_in: Duration,
}

impl fmt::Debug for DeviceCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DeviceCode")
            .field("user_code", &self.user_code)
            .field("verification_uri", &self.verification_uri)
            .field("device_code", &HIDDEN)
            .field("interval", &self.interval)
            .field("expires_in", &self.expires_in)
            .finish()
    }
}

/// What `POST /auth/github` answers. Its `expires_at` is left out: the server
/// extends a session that is used, and a 401 says when one has ended.
/// `Debug` leaves the session out: it opens the account, so it must never
/// end up in a log or a failed assertion.
#[derive(Clone, PartialEq, Eq, Deserialize)]
pub struct SignedIn {
    pub session: String,
    pub session_id: String,
    pub github_id: u64,
    pub login: String,
    #[serde(default)]
    pub flags: Vec<String>,
    #[serde(default)]
    pub plan: String,
}

impl fmt::Debug for SignedIn {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SignedIn")
            .field("session", &HIDDEN)
            .field("session_id", &self.session_id)
            .field("github_id", &self.github_id)
            .field("login", &self.login)
            .field("flags", &self.flags)
            .field("plan", &self.plan)
            .finish()
    }
}

/// Who is signed in, as `GET /me` answers.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub struct Me {
    pub github_id: u64,
    pub login: String,
    #[serde(default)]
    pub flags: Vec<String>,
    #[serde(default)]
    pub plan: String,
    #[serde(default)]
    pub notices: Vec<Notice>,
}

/// A message from the operator to this account, until it is read.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub struct Notice {
    pub id: String,
    pub text: String,
    #[serde(default)]
    pub url: Option<String>,
}

/// A computer of the account on the relay, as `GET /machines` lists it.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub struct Machine {
    pub label: String,
    /// Its public key, `pub` on the wire.
    #[serde(rename = "pub")]
    pub public: String,
    pub name: String,
    pub kind: String,
    pub online: bool,
    pub last_seen_at: u64,
}

/// A signed-in device of the account, as `GET /sessions` lists it.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub struct SessionRow {
    pub id: String,
    pub name: String,
    pub kind: String,
    pub created_at: u64,
    pub last_used_at: u64,
    /// The session that asked.
    pub current: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Error {
    /// 401: the session is not valid (any more).
    SignedOut,
    /// The operator blocked the account, with the reason they gave.
    Blocked(String),
    /// The request got no answer.
    Unreachable(String),
    /// Any other answer than the one expected, with what the server said.
    Refused(String),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::SignedOut => write!(f, "The sign-in is no longer valid."),
            Error::Blocked(reason) if reason.is_empty() => write!(f, "This account is blocked."),
            Error::Blocked(reason) => write!(f, "This account is blocked: {reason}."),
            Error::Unreachable(e) => write!(f, "Could not reach the server ({e})."),
            Error::Refused(m) if m.is_empty() => write!(f, "The server refused the request."),
            Error::Refused(m) => write!(f, "The server answered: {}.", m.trim_end_matches('.')),
        }
    }
}

impl std::error::Error for Error {}

/// `GET /auth/config`: the OAuth App the server checks tokens against, for the
/// device flow to ask GitHub with.
pub fn client_id(base: &str) -> Result<String, Error> {
    #[derive(Deserialize)]
    struct Config {
        github_client_id: Option<String>,
    }
    let (status, body) = send(base, "GET", "/auth/config", None, None, SMALL_LIMIT)?;
    let config: Config = json(status, &body)?;
    config
        .github_client_id
        .filter(|id| !id.is_empty())
        .ok_or_else(|| Error::Refused("sign-in is not set up on this server".into()))
}

/// The OAuth App to run the device flow with for the server at `base`:
/// Arakun's own for the default server, without asking it; a server of one's
/// own names its own app.
pub fn github_client_id(base: &str) -> Result<String, Error> {
    if base == DEFAULT_URL {
        Ok(CLIENT_ID.into())
    } else {
        client_id(base)
    }
}

/// Asks GitHub for a code the user enters at `verification_uri`.
pub fn start_device_flow(client_id: &str) -> Result<DeviceCode, Error> {
    #[derive(Deserialize)]
    struct Wire {
        device_code: String,
        user_code: String,
        verification_uri: String,
        expires_in: u64,
        #[serde(default = "default_interval")]
        interval: u64,
    }
    fn default_interval() -> u64 {
        5
    }
    let sent = http()
        .post(DEVICE_CODE_URL)
        .header("Accept", "application/json")
        .send_form([("client_id", client_id), ("scope", SCOPE)]);
    let (status, body) = read(sent, SMALL_LIMIT)?;
    if status != 200 {
        return Err(Error::Refused(github_error(status, &body)));
    }
    let wire: Wire = serde_json::from_str(&body).map_err(|_| unreadable())?;
    Ok(DeviceCode {
        user_code: wire.user_code,
        verification_uri: wire.verification_uri,
        device_code: wire.device_code,
        // A zero interval would poll as fast as the caller can.
        interval: Duration::from_secs(wire.interval.max(1)),
        expires_in: Duration::from_secs(wire.expires_in),
    })
}

/// One poll of GitHub's token endpoint. `None`: ask again after
/// `code.interval`. When GitHub asks to slow down, the extra time is
/// remembered and waited here before each later poll, so a caller that keeps
/// to `code.interval` keeps GitHub's pace.
pub fn poll_device_flow(client_id: &str, code: &DeviceCode) -> Result<Option<String>, Error> {
    // The extra wait of each flow, and when its code has run out for sure.
    static EXTRA: OnceLock<Mutex<HashMap<String, (Duration, Instant)>>> = OnceLock::new();
    let extra = EXTRA.get_or_init(Default::default);
    let lock = || extra.lock().unwrap_or_else(|e| e.into_inner());
    // A flow given up while GitHub said to slow down never gets a last
    // answer: its entry goes once its code is dead.
    forget_expired(&mut lock(), Instant::now());
    let wait = lock()
        .get(&code.device_code)
        .map(|(wait, _)| *wait)
        .unwrap_or_default();
    if !wait.is_zero() {
        std::thread::sleep(wait);
    }
    let polled = poll_once(client_id, &code.device_code, code.interval + wait);
    if !matches!(polled, Ok(Poll::Wait(_)) | Err(Error::Unreachable(_))) {
        lock().remove(&code.device_code);
    }
    match polled? {
        Poll::Wait(pace) => {
            if let Some(more) = pace.checked_sub(code.interval).filter(|d| *d > wait) {
                let dead = Instant::now() + code.expires_in;
                lock().insert(code.device_code.clone(), (more, dead));
            }
            Ok(None)
        }
        Poll::Token(token) => Ok(Some(token)),
        Poll::Expired => Err(Error::Refused(
            "the code ran out before it was entered on GitHub".into(),
        )),
        Poll::Denied => Err(Error::Refused(
            "the sign-in was turned down on GitHub".into(),
        )),
        Poll::Failed(e) => Err(Error::Refused(e)),
    }
}

/// Drops the extra waits of the device flows whose code has run out by `now`.
fn forget_expired(extra: &mut HashMap<String, (Duration, Instant)>, now: Instant) {
    extra.retain(|_, (_, dead)| *dead > now);
}

/// Trades the GitHub token for a session. The server asks GitHub who the
/// token belongs to and does not keep it. `name` and `kind` say what the
/// session is on, for the list of signed-in devices.
pub fn sign_in(base: &str, github_token: &str, name: &str, kind: &str) -> Result<SignedIn, Error> {
    let body = serde_json::json!({ "access_token": github_token, "name": name, "kind": kind });
    let (status, body) = send(base, "POST", "/auth/github", None, Some(body), SMALL_LIMIT)?;
    match json(status, &body) {
        // No session yet, so a 401 is about the GitHub token, not a stale sign-in.
        Err(Error::SignedOut) => Err(Error::Refused(
            "GitHub did not accept the sign-in. Try again.".into(),
        )),
        answer => answer,
    }
}

pub fn me(base: &str, session: &str) -> Result<Me, Error> {
    get(base, session, "/me", SMALL_LIMIT)
}

pub fn machines(base: &str, session: &str) -> Result<Vec<Machine>, Error> {
    get(base, session, "/machines", LIST_LIMIT)
}

/// Removes one of the account's machines. One the account does not list
/// (removed already, or never connected) is removed all the same.
pub fn forget_machine(base: &str, session: &str, label: &str) -> Result<(), Error> {
    remove_machine(base, session, label).map(|_| ())
}

/// `forget_machine`, saying whether the account listed the machine: false
/// for one it did not have.
pub fn remove_machine(base: &str, session: &str, label: &str) -> Result<bool, Error> {
    let path = format!("/machines/{}", path_part(label)?);
    let (status, body) = send(base, "DELETE", &path, Some(session), None, SMALL_LIMIT)?;
    match status {
        200..=299 => Ok(true),
        404 => Ok(false),
        _ => Err(refusal(status, &body)),
    }
}

pub fn sessions(base: &str, session: &str) -> Result<Vec<SessionRow>, Error> {
    get(base, session, "/sessions", LIST_LIMIT)
}

/// Ends one of the account's sessions, this one or another.
pub fn sign_out_session(base: &str, session: &str, id: &str) -> Result<(), Error> {
    act(
        base,
        session,
        "DELETE",
        &format!("/sessions/{}", path_part(id)?),
    )
}

/// Marks a notice read, so `/me` stops listing it.
pub fn read_notice(base: &str, session: &str, id: &str) -> Result<(), Error> {
    act(
        base,
        session,
        "POST",
        &format!("/notices/{}/read", path_part(id)?),
    )
}

/// Ends the session on the server.
pub fn logout(base: &str, session: &str) -> Result<(), Error> {
    act(base, session, "POST", "/auth/logout")
}

/// One answer to a poll of the token endpoint: a token or an error code.
/// `Debug` leaves the token out.
#[derive(Clone, Default, Deserialize)]
pub struct TokenAnswer {
    pub access_token: Option<String>,
    pub error: Option<String>,
    pub error_description: Option<String>,
    /// With `slow_down`, the interval GitHub wants from now on, in seconds.
    pub interval: Option<u64>,
}

impl fmt::Debug for TokenAnswer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TokenAnswer")
            .field("access_token", &self.access_token.as_ref().map(|_| HIDDEN))
            .field("error", &self.error)
            .field("error_description", &self.error_description)
            .field("interval", &self.interval)
            .finish()
    }
}

/// What to do after a poll. `Debug` leaves the token out.
#[derive(PartialEq, Eq)]
pub enum Poll {
    /// Ask again after this long, and keep that pace.
    Wait(Duration),
    Token(String),
    /// The code ran out before the user entered it.
    Expired,
    /// The user turned the request down on GitHub.
    Denied,
    Failed(String),
}

impl fmt::Debug for Poll {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Poll::Wait(pace) => f.debug_tuple("Wait").field(pace).finish(),
            Poll::Token(_) => f.debug_tuple("Token").field(&HIDDEN).finish(),
            Poll::Expired => f.write_str("Expired"),
            Poll::Denied => f.write_str("Denied"),
            Poll::Failed(why) => f.debug_tuple("Failed").field(why).finish(),
        }
    }
}

/// One poll of GitHub's token endpoint, polling every `interval` so far.
/// For callers that tell expiry and denial apart; others use
/// [`poll_device_flow`].
pub fn poll_once(client_id: &str, device_code: &str, interval: Duration) -> Result<Poll, Error> {
    let sent = http()
        .post(TOKEN_URL)
        .header("Accept", "application/json")
        .send_form([
            ("client_id", client_id),
            ("device_code", device_code),
            ("grant_type", DEVICE_GRANT),
        ]);
    let (status, body) = read(sent, SMALL_LIMIT)?;
    poll_answer(status, &body, interval)
}

/// Decides the next step of the device flow from GitHub's answer, polling every
/// `interval` so far. Kept apart from the network so it can be tested.
pub fn next_poll(answer: &TokenAnswer, interval: Duration) -> Poll {
    if let Some(token) = answer.access_token.as_deref().filter(|t| !t.is_empty()) {
        return Poll::Token(token.to_string());
    }
    match answer.error.as_deref() {
        Some("authorization_pending") => Poll::Wait(interval),
        // GitHub adds 5 s on each slow_down, and says the new total; take
        // whichever is longer so we never poll faster than it asks.
        Some("slow_down") => Poll::Wait(
            (interval + Duration::from_secs(5))
                .max(Duration::from_secs(answer.interval.unwrap_or(0))),
        ),
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

/// Pending and the other device-flow errors come back with an `error`, for
/// [`next_poll`] to read.
fn poll_answer(status: u16, body: &str, interval: Duration) -> Result<Poll, Error> {
    // GitHub's own trouble leaves the code valid: ask again at the same pace
    // until it runs out.
    if status >= 500 {
        return Ok(Poll::Wait(interval));
    }
    match serde_json::from_str::<TokenAnswer>(body) {
        Ok(a) if status == 200 || a.error.is_some() => Ok(next_poll(&a, interval)),
        // A 200 that is not JSON (a proxy's page, say) is not an answer.
        Err(_) if status == 200 => Err(unreadable()),
        _ => Err(Error::Refused(github_error(status, body))),
    }
}

/// A label or id to put in a path. These can come from another machine, so
/// anything that could reach another route (`/`, `..`, `%`) is refused.
fn path_part(part: &str) -> Result<&str, Error> {
    let ok = (1..=128).contains(&part.len())
        && part
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_');
    if ok {
        Ok(part)
    } else {
        Err(Error::Refused(format!("{part:?} is not a valid name")))
    }
}

const SMALL_LIMIT: u64 = 64 * 1024;
/// A list of machines or signed-in devices is a few hundred bytes a row.
const LIST_LIMIT: u64 = 1024 * 1024;

fn get<T: serde::de::DeserializeOwned>(
    base: &str,
    session: &str,
    path: &str,
    limit: u64,
) -> Result<T, Error> {
    let (status, body) = send(base, "GET", path, Some(session), None, limit)?;
    json(status, &body)
}

/// A call that answers nothing but its status.
fn act(base: &str, session: &str, method: &str, path: &str) -> Result<(), Error> {
    let (status, body) = send(base, method, path, Some(session), None, SMALL_LIMIT)?;
    match status {
        200..=299 => Ok(()),
        _ => Err(refusal(status, &body)),
    }
}

/// Sends one request to the server. Sessions and GitHub tokens go in it, so
/// plain `http://` is refused unless the server is on this machine.
fn send(
    base: &str,
    method: &str,
    path: &str,
    session: Option<&str>,
    body: Option<serde_json::Value>,
    limit: u64,
) -> Result<(u16, String), Error> {
    if !(base.starts_with("https://") || local_http(base)) {
        return Err(Error::Refused(format!(
            "{base} must start with https:// (http:// only for localhost)"
        )));
    }
    let mut req = ureq::http::Request::builder()
        .method(method)
        .uri(format!("{base}{path}"));
    if let Some(session) = session {
        req = req.header("Authorization", format!("Bearer {session}"));
    }
    let built = |e: ureq::http::Error| Error::Unreachable(e.to_string());
    let sent = match body {
        Some(body) => http_for(base).run(
            req.header("Content-Type", "application/json")
                .body(body.to_string())
                .map_err(built)?,
        ),
        // ureq sends `()` on a POST as a chunked body, and a server that answers before
        // reading its end marker resets the connection. An empty slice sends
        // `Content-Length: 0` instead; GET and DELETE carry no length at all.
        None if matches!(method, "POST" | "PUT" | "PATCH") => {
            http_for(base).run(req.body(&[][..]).map_err(built)?)
        }
        None => http_for(base).run(req.body(()).map_err(built)?),
    };
    read(sent, limit)
}

fn read(
    sent: Result<ureq::http::Response<ureq::Body>, ureq::Error>,
    limit: u64,
) -> Result<(u16, String), Error> {
    let mut res = sent.map_err(|e| Error::Unreachable(e.to_string()))?;
    let status = res.status().as_u16();
    let body = res
        .body_mut()
        .with_config()
        .limit(limit)
        .read_to_string()
        .unwrap_or_default();
    Ok((status, body))
}

fn json<T: serde::de::DeserializeOwned>(status: u16, body: &str) -> Result<T, Error> {
    match status {
        200..=299 => serde_json::from_str(body).map_err(|_| unreadable()),
        _ => Err(refusal(status, body)),
    }
}

fn unreadable() -> Error {
    Error::Refused("an answer that could not be read".into())
}

/// The server's errors are `{"error": "..."}`; a blocked account's also
/// carries the operator's `reason`.
fn refusal(status: u16, body: &str) -> Error {
    let v: serde_json::Value = serde_json::from_str(body).unwrap_or_default();
    let said = v["error"].as_str().unwrap_or_default();
    match status {
        401 => Error::SignedOut,
        403 if said == "blocked" => {
            Error::Blocked(v["reason"].as_str().unwrap_or_default().to_string())
        }
        429 => Error::Refused("too many requests; try again in a minute".into()),
        _ if said.is_empty() => Error::Refused(format!("HTTP {status}")),
        _ => Error::Refused(said.to_string()),
    }
}

/// GitHub's errors are `{"error": "...", "error_description": "..."}`.
fn github_error(status: u16, body: &str) -> String {
    let v: serde_json::Value = serde_json::from_str(body).unwrap_or_default();
    v["error_description"]
        .as_str()
        .or(v["error"].as_str())
        .map_or_else(|| format!("HTTP {status}"), str::to_string)
}

/// The agent for GitHub and other hosts: the proxy from the environment, IPv4
/// first.
pub fn http() -> &'static ureq::Agent {
    static AGENT: OnceLock<ureq::Agent> = OnceLock::new();
    AGENT.get_or_init(|| build(false))
}

/// The agent for the server at `base`. Plain `http://` to this machine skips
/// the proxy from the environment (`HTTP_PROXY`, `ALL_PROXY`): it would carry
/// the session off the machine unencrypted. `https://` keeps it.
pub fn http_for(base: &str) -> &'static ureq::Agent {
    static DIRECT: OnceLock<ureq::Agent> = OnceLock::new();
    if local_http(base) {
        DIRECT.get_or_init(|| build(true))
    } else {
        http()
    }
}

fn build(direct: bool) -> ureq::Agent {
    let mut config = ureq::Agent::config_builder();
    if direct {
        config = config.proxy(None);
    }
    let config = config
        .timeout_global(Some(Duration::from_secs(20)))
        .http_status_as_error(false)
        .user_agent(concat!("arakun/", env!("ARAKUN_VERSION")))
        .build();
    ureq::Agent::with_parts(
        config,
        ureq::unversioned::transport::DefaultConnector::new(),
        Ipv4First::default(),
    )
}

/// The system resolver, with IPv4 addresses tried first. ureq dials one address
/// after another; Cloudflare lists IPv6 first, and on a network whose IPv6 is
/// broken each IPv6 attempt hangs until the request's time is gone.
#[derive(Debug, Default)]
struct Ipv4First(ureq::unversioned::resolver::DefaultResolver);

impl ureq::unversioned::resolver::Resolver for Ipv4First {
    fn resolve(
        &self,
        uri: &ureq::http::Uri,
        config: &ureq::config::Config,
        timeout: ureq::unversioned::transport::NextTimeout,
    ) -> Result<ureq::unversioned::resolver::ResolvedSocketAddrs, ureq::Error> {
        let mut addrs = self.0.resolve(uri, config, timeout)?;
        ipv4_first(&mut addrs);
        Ok(addrs)
    }
}

/// IPv4 before IPv6, each family in the order the resolver gave.
pub fn ipv4_first(addrs: &mut [std::net::SocketAddr]) {
    addrs.sort_by_key(std::net::SocketAddr::is_ipv6);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_address_written_another_way_is_the_same_server() {
        assert!(same_origin(
            "https://app.arakun.dev",
            "HTTPS://App.Arakun.dev/"
        ));
        assert!(same_origin(
            "https://app.arakun.dev:443/",
            " https://app.arakun.dev"
        ));
        assert!(same_origin(
            "http://localhost:8787",
            "http://LOCALHOST:8787/x"
        ));
        assert!(same_origin("http://[::1]:8787", "http://[::1]:8787/"));
        assert!(!same_origin(
            "https://app.arakun.dev",
            "http://app.arakun.dev"
        ));
        assert!(!same_origin(
            "https://app.arakun.dev",
            "https://relay.example"
        ));
        assert!(!same_origin(
            "http://localhost:8787",
            "http://localhost:8788"
        ));
        assert!(!same_origin(
            "https://evil@app.arakun.dev",
            "https://app.arakun.dev"
        ));
        assert!(!same_origin("app.arakun.dev", "app.arakun.dev"));
    }
    use std::io::{Read as _, Write as _};
    use std::sync::mpsc::Receiver;

    /// A server of our own for one request: answers `status` with `body`, and
    /// hands back the request it got, head and body.
    fn serve(status: u16, body: &'static str) -> (String, Receiver<String>) {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let (mut s, _) = listener.accept().unwrap();
            let mut got = Vec::new();
            let mut buf = [0u8; 4096];
            let head_end = loop {
                let n = s.read(&mut buf).unwrap();
                got.extend_from_slice(&buf[..n]);
                if let Some(i) = got.windows(4).position(|w| w == b"\r\n\r\n") {
                    break i + 4;
                }
            };
            let head = String::from_utf8_lossy(&got[..head_end]).to_string();
            let want: usize = head
                .lines()
                .find_map(|l| {
                    l.to_ascii_lowercase()
                        .strip_prefix("content-length:")
                        .map(|v| v.trim().parse().unwrap())
                })
                .unwrap_or(0);
            while got.len() < head_end + want {
                let n = s.read(&mut buf).unwrap();
                got.extend_from_slice(&buf[..n]);
            }
            tx.send(String::from_utf8_lossy(&got).to_string()).unwrap();
            write!(
                s,
                "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            )
            .unwrap();
        });
        (base, rx)
    }

    #[test]
    fn the_default_server_signs_in_with_arakuns_own_app() {
        // No request: nothing listens at the default address in a test.
        assert_eq!(github_client_id(DEFAULT_URL), Ok(CLIENT_ID.to_string()));
        let (base, _rx) = serve(200, r#"{"github_client_id": "Iv1.own"}"#);
        assert_eq!(github_client_id(&base), Ok("Iv1.own".to_string()));
    }

    #[test]
    fn me_reads_the_plan_and_notices() {
        let (base, rx) = serve(
            200,
            r#"{"github_id": 7, "login": "rik", "flags": ["teams"], "plan": "free",
                "notices": [{"id": "n1", "text": "Maintenance tonight", "url": "https://arakun.dev/status"},
                            {"id": "n2", "text": "Welcome", "url": null}]}"#,
        );
        let me = me(&base, "s3ss10n").unwrap();
        assert_eq!(me.github_id, 7);
        assert_eq!(me.login, "rik");
        assert_eq!(me.flags, ["teams"]);
        assert_eq!(me.plan, "free");
        assert_eq!(
            me.notices,
            [
                Notice {
                    id: "n1".into(),
                    text: "Maintenance tonight".into(),
                    url: Some("https://arakun.dev/status".into()),
                },
                Notice {
                    id: "n2".into(),
                    text: "Welcome".into(),
                    url: None,
                },
            ]
        );
        let req = rx.recv().unwrap();
        assert!(req.starts_with("GET /me HTTP/1.1\r\n"), "{req}");
        assert!(
            req.to_ascii_lowercase()
                .contains("authorization: bearer s3ss10n\r\n"),
            "{req}"
        );
    }

    #[test]
    fn a_401_is_signed_out() {
        let (base, _rx) = serve(401, r#"{"error": "not signed in"}"#);
        assert_eq!(me(&base, "old"), Err(Error::SignedOut));
        let (base, _rx) = serve(401, "");
        assert_eq!(machines(&base, "old"), Err(Error::SignedOut));
    }

    #[test]
    fn a_blocked_account_says_why() {
        let (base, _rx) = serve(403, r#"{"error": "blocked", "reason": "abuse"}"#);
        assert_eq!(machines(&base, "s"), Err(Error::Blocked("abuse".into())));
        let (base, _rx) = serve(403, r#"{"error": "forbidden"}"#);
        assert_eq!(
            machines(&base, "s"),
            Err(Error::Refused("forbidden".into()))
        );
    }

    #[test]
    fn other_answers_are_refused_with_the_servers_words() {
        let (base, _rx) = serve(404, r#"{"error": "not found"}"#);
        assert_eq!(
            sign_out_session(&base, "s", "abc"),
            Err(Error::Refused("not found".into()))
        );
        let (base, _rx) = serve(200, "<html>");
        assert!(matches!(me(&base, "s"), Err(Error::Refused(_))));
    }

    #[test]
    fn a_server_that_does_not_answer_is_unreachable() {
        // Bound, then closed: nothing listens there.
        let port = std::net::TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        let base = format!("http://127.0.0.1:{port}");
        assert!(matches!(me(&base, "s"), Err(Error::Unreachable(_))));
    }

    #[test]
    fn machines_reads_a_row() {
        let (base, rx) = serve(
            200,
            r#"[{"label": "k3x9", "pub": "AAAA", "name": "studio", "kind": "server",
                 "online": true, "last_seen_at": 1800000000}]"#,
        );
        assert_eq!(
            machines(&base, "s").unwrap(),
            [Machine {
                label: "k3x9".into(),
                public: "AAAA".into(),
                name: "studio".into(),
                kind: "server".into(),
                online: true,
                last_seen_at: 1_800_000_000,
            }]
        );
        assert!(rx.recv().unwrap().starts_with("GET /machines HTTP/1.1\r\n"));
    }

    #[test]
    fn sessions_reads_a_row() {
        let (base, _rx) = serve(
            200,
            r#"[{"id": "0123", "name": "Pixel", "kind": "android", "created_at": 1,
                 "last_used_at": 2, "current": false}]"#,
        );
        assert_eq!(
            sessions(&base, "s").unwrap(),
            [SessionRow {
                id: "0123".into(),
                name: "Pixel".into(),
                kind: "android".into(),
                created_at: 1,
                last_used_at: 2,
                current: false,
            }]
        );
    }

    #[test]
    fn the_no_content_calls_use_their_routes() {
        let (base, rx) = serve(204, "");
        forget_machine(&base, "s", "k3x9").unwrap();
        assert!(
            rx.recv()
                .unwrap()
                .starts_with("DELETE /machines/k3x9 HTTP/1.1\r\n")
        );
        let (base, rx) = serve(204, "");
        sign_out_session(&base, "s", "0123").unwrap();
        assert!(
            rx.recv()
                .unwrap()
                .starts_with("DELETE /sessions/0123 HTTP/1.1\r\n")
        );
        let (base, rx) = serve(204, "");
        read_notice(&base, "s", "n1").unwrap();
        assert!(
            rx.recv()
                .unwrap()
                .starts_with("POST /notices/n1/read HTTP/1.1\r\n")
        );
        let (base, rx) = serve(204, "");
        logout(&base, "s").unwrap();
        let got = rx.recv().unwrap();
        assert!(got.starts_with("POST /auth/logout HTTP/1.1\r\n"));
        // A chunked empty body leaves its end marker unread by a server that answers
        // at once, and closing on it resets the connection.
        assert!(got.contains("content-length: 0\r\n"), "{got}");
        assert!(!got.contains("transfer-encoding"), "{got}");
    }

    #[test]
    fn a_machine_the_account_does_not_list_is_removed_all_the_same() {
        let (base, _rx) = serve(404, r#"{"error": "no such machine"}"#);
        assert_eq!(forget_machine(&base, "s", "k3x9"), Ok(()));
        let (base, _rx) = serve(404, r#"{"error": "no such machine"}"#);
        assert_eq!(remove_machine(&base, "s", "k3x9"), Ok(false));
        let (base, _rx) = serve(204, "");
        assert_eq!(remove_machine(&base, "s", "k3x9"), Ok(true));
        let (base, _rx) = serve(401, r#"{"error": "not signed in"}"#);
        assert_eq!(forget_machine(&base, "s", "k3x9"), Err(Error::SignedOut));
    }

    #[test]
    fn sign_in_sends_the_name_and_kind() {
        let (base, rx) = serve(
            200,
            r#"{"session": "tok", "session_id": "0123", "expires_at": 1800000000,
                "github_id": 2, "login": "b", "flags": ["teams"], "plan": "free"}"#,
        );
        let signed = sign_in(&base, "gho_x", "studio", "app").unwrap();
        assert_eq!(
            signed,
            SignedIn {
                session: "tok".into(),
                session_id: "0123".into(),
                github_id: 2,
                login: "b".into(),
                flags: vec!["teams".into()],
                plan: "free".into(),
            }
        );
        let req = rx.recv().unwrap();
        assert!(req.starts_with("POST /auth/github HTTP/1.1\r\n"), "{req}");
        let body: serde_json::Value =
            serde_json::from_str(req.split("\r\n\r\n").nth(1).unwrap()).unwrap();
        assert_eq!(
            body,
            serde_json::json!({ "access_token": "gho_x", "name": "studio", "kind": "app" })
        );
    }

    #[test]
    fn a_label_or_id_that_could_leave_its_path_is_refused_unsent() {
        let (base, rx) = serve(204, "");
        for bad in [
            "../sessions/x",
            "",
            "a/b",
            "a b",
            "%2e%2e",
            &"a".repeat(129),
        ] {
            for call in [
                forget_machine(&base, "s", bad),
                sign_out_session(&base, "s", bad),
                read_notice(&base, "s", bad),
            ] {
                assert!(matches!(call, Err(Error::Refused(_))), "{bad}: {call:?}");
            }
        }
        // Nothing reached the server; the next request is the first it sees.
        forget_machine(&base, "s", &format!("A-z_0{}", "9".repeat(123))).unwrap();
        assert!(rx.recv().unwrap().starts_with("DELETE /machines/A-z_09"));
    }

    #[test]
    fn a_token_github_did_not_accept_is_not_a_stale_session() {
        let (base, _rx) = serve(401, r#"{"error": "invalid token"}"#);
        let refused = sign_in(&base, "gho_x", "studio", "app");
        assert_eq!(
            refused,
            Err(Error::Refused(
                "GitHub did not accept the sign-in. Try again.".into()
            ))
        );
    }

    #[test]
    fn the_client_id_comes_from_the_server() {
        let (base, rx) = serve(200, r#"{"github_client_id": "Ov23abc"}"#);
        assert_eq!(client_id(&base).unwrap(), "Ov23abc");
        assert!(
            rx.recv()
                .unwrap()
                .starts_with("GET /auth/config HTTP/1.1\r\n")
        );
        let (base, _rx) = serve(200, r#"{"github_client_id": null}"#);
        assert!(matches!(client_id(&base), Err(Error::Refused(_))));
    }

    #[test]
    fn a_session_never_goes_over_plain_http_to_another_host() {
        // Refused before any request: nothing listens there.
        for call in [
            me("http://arakun.example", "s").map(|_| ()),
            logout("http://arakun.example", "s"),
            sign_in("http://arakun.example", "gho_x", "n", "app").map(|_| ()),
        ] {
            assert!(matches!(call, Err(Error::Refused(_))), "{call:?}");
        }
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

    #[test]
    fn only_this_machine_is_local_http() {
        assert!(local_http("http://localhost:8787"));
        assert!(local_http("http://127.0.0.1"));
        assert!(local_http("http://[::1]:8787/x"));
        assert!(!local_http("https://localhost"));
        assert!(!local_http("http://localhost.example.com"));
        assert!(!local_http("http://user@localhost"));
        assert!(!local_http("http://[::1]x"));
    }

    #[test]
    fn only_a_local_http_server_skips_the_proxy() {
        let direct = http_for("http://localhost:8787");
        assert!(std::ptr::eq(direct, http_for("http://127.0.0.1:8787")));
        assert!(!std::ptr::eq(direct, http()));
        assert!(std::ptr::eq(http_for(DEFAULT_URL), http()));
    }

    #[test]
    fn tries_ipv4_before_ipv6_keeping_each_order() {
        let mut addrs: Vec<std::net::SocketAddr> = [
            "[2606:4700::1]:443",
            "104.18.22.18:443",
            "[2606:4700::2]:443",
            "104.18.23.18:443",
        ]
        .iter()
        .map(|a| a.parse().unwrap())
        .collect();
        ipv4_first(&mut addrs);
        let got: Vec<String> = addrs.iter().map(ToString::to_string).collect();
        assert_eq!(
            got,
            [
                "104.18.22.18:443",
                "104.18.23.18:443",
                "[2606:4700::1]:443",
                "[2606:4700::2]:443"
            ]
        );
    }

    fn answer(json: &str) -> TokenAnswer {
        serde_json::from_str(json).unwrap()
    }

    const FIVE: Duration = Duration::from_secs(5);

    #[test]
    fn a_debug_print_never_shows_a_secret() {
        let signed: SignedIn = serde_json::from_str(
            r#"{"session":"sess_secret","session_id":"sid","github_id":7,"login":"ann"}"#,
        )
        .unwrap();
        let code = DeviceCode {
            user_code: "ABCD-1234".into(),
            verification_uri: "https://github.com/login/device".into(),
            device_code: "dev_secret".into(),
            interval: FIVE,
            expires_in: Duration::from_secs(900),
        };
        let token = answer(r#"{"access_token": "gho_secret"}"#);
        let printed = format!(
            "{signed:?} {code:?} {token:?} {:?} {:?}",
            Poll::Token("gho_secret".into()),
            Ok::<_, Error>(signed.clone()),
        );
        assert!(!printed.contains("secret"), "{printed}");
        // What is not secret still shows, so a failed assertion says something.
        assert!(printed.contains("ann") && printed.contains("ABCD-1234"));
        assert_eq!(format!("{:?}", Poll::Wait(FIVE)), "Wait(5s)");
    }

    #[test]
    fn a_device_flow_given_up_is_forgotten_once_its_code_ran_out() {
        let now = Instant::now();
        let mut extra = HashMap::new();
        extra.insert("gone".to_string(), (FIVE, now));
        extra.insert("live".to_string(), (FIVE, now + FIVE));
        forget_expired(&mut extra, now);
        assert_eq!(extra.keys().collect::<Vec<_>>(), ["live"]);
    }

    #[test]
    fn pending_waits_at_the_same_pace() {
        let a = answer(r#"{"error": "authorization_pending"}"#);
        assert_eq!(next_poll(&a, FIVE), Poll::Wait(FIVE));
    }

    #[test]
    fn slow_down_adds_five_seconds() {
        let secs = Duration::from_secs;
        let a = answer(r#"{"error": "slow_down"}"#);
        assert_eq!(next_poll(&a, FIVE), Poll::Wait(secs(10)));
        // GitHub's own interval wins when it asks for more.
        let a = answer(r#"{"error": "slow_down", "interval": 15}"#);
        assert_eq!(next_poll(&a, FIVE), Poll::Wait(secs(15)));
        let a = answer(r#"{"error": "slow_down", "interval": 3}"#);
        assert_eq!(next_poll(&a, secs(10)), Poll::Wait(secs(15)));
    }

    #[test]
    fn a_token_ends_the_flow() {
        let a = answer(r#"{"access_token": "gho_x", "token_type": "bearer", "scope": ""}"#);
        assert_eq!(next_poll(&a, FIVE), Poll::Token("gho_x".into()));
    }

    #[test]
    fn expired_and_denied_stop() {
        assert_eq!(
            next_poll(&answer(r#"{"error": "expired_token"}"#), FIVE),
            Poll::Expired
        );
        assert_eq!(
            next_poll(&answer(r#"{"error": "access_denied"}"#), FIVE),
            Poll::Denied
        );
    }

    #[test]
    fn other_errors_fail_with_githubs_words() {
        let a = answer(
            r#"{"error": "incorrect_device_code", "error_description": "The device_code provided is not valid."}"#,
        );
        assert_eq!(
            next_poll(&a, FIVE),
            Poll::Failed("The device_code provided is not valid.".into())
        );
        let a = answer(r#"{"error": "unsupported_grant_type"}"#);
        assert_eq!(
            next_poll(&a, FIVE),
            Poll::Failed("GitHub answered unsupported_grant_type.".into())
        );
        assert!(matches!(next_poll(&answer("{}"), FIVE), Poll::Failed(_)));
        // An empty token is no token.
        assert!(matches!(
            next_poll(&answer(r#"{"access_token": ""}"#), FIVE),
            Poll::Failed(_)
        ));
    }

    #[test]
    fn a_poll_github_answered_with_a_5xx_waits_at_the_same_pace() {
        // GitHub's own trouble leaves the code valid.
        assert_eq!(poll_answer(502, "<html>", FIVE), Ok(Poll::Wait(FIVE)));
        assert_eq!(poll_answer(503, "", FIVE), Ok(Poll::Wait(FIVE)));
    }

    #[test]
    fn a_token_poll_that_is_not_json_is_refused() {
        assert!(matches!(
            poll_answer(200, "<html>", FIVE),
            Err(Error::Refused(_))
        ));
        assert!(matches!(
            poll_answer(400, "<html>", FIVE),
            Err(Error::Refused(_))
        ));
        assert_eq!(
            poll_answer(200, r#"{"error": "authorization_pending"}"#, FIVE),
            Ok(Poll::Wait(FIVE))
        );
        // GitHub answers device-flow errors with a 400 too.
        assert_eq!(
            poll_answer(400, r#"{"error": "expired_token"}"#, FIVE),
            Ok(Poll::Expired)
        );
    }
}
