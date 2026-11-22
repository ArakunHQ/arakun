//! The network side of Teams: GitHub's device flow, then the Teams server
//! (`teams/README.md`). Every call blocks, so callers run them off the UI
//! thread. The types mirror the server's JSON.

use std::sync::OnceLock;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use super::crypto::{Envelope, Sealed, WireMessage};

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
    token_answer(status, &body)
}

fn token_answer(status: u16, body: &str) -> Result<TokenAnswer, ApiError> {
    match serde_json::from_str::<TokenAnswer>(body) {
        Ok(a) if status == 200 || a.error.is_some() => Ok(a),
        // A 200 that is not JSON (a proxy's page, say) is not "answered 200".
        Err(_) if status == 200 => Err(ApiError::Unreadable),
        _ => Err(ApiError::Status(status, github_error(body))),
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

/// A GitHub account, as `GET https://api.github.com/users/<login>` shows it.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub struct GithubUser {
    pub login: String,
    #[serde(default)]
    pub name: Option<String>,
}

/// Looks a login up on GitHub, without a token: `None` when there is no such
/// account. A rate limit (403 or 429) is an error, so the caller can say less.
pub fn github_user(login: &str) -> Result<Option<GithubUser>, ApiError> {
    let ok = !login.is_empty()
        && login.len() <= 39
        && login
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-');
    if !ok {
        return Ok(None);
    }
    let (status, body) = answer(
        agent()
            .get(format!("https://api.github.com/users/{login}"))
            .header("Accept", "application/vnd.github+json")
            .call(),
    )?;
    match status {
        404 => Ok(None),
        200 => read_github_user(&body),
        s => Err(ApiError::Status(s, github_error(&body))),
    }
}

/// The account in GitHub's answer, when it is a person's. The same path
/// answers for an organization, which the server refuses to invite.
fn read_github_user(body: &str) -> Result<Option<GithubUser>, ApiError> {
    let v: serde_json::Value = serde_json::from_str(body).map_err(|_| ApiError::Unreadable)?;
    if v["type"] != "User" {
        return Ok(None);
    }
    serde_json::from_value(v)
        .map(Some)
        .map_err(|_| ApiError::Unreadable)
}

// ---- teams (teams/PROTOCOL.md) ------------------------------------------------

/// A device as `GET /users/<id>/devices` and the pending list show it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeviceRow {
    pub device_id: String,
    pub sig_pub: String,
    pub enc_pub: String,
    pub name: String,
    pub created_at: u64,
    pub endorsed_by: Option<String>,
    #[serde(default)]
    pub endorsement_sig: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Endorsement {
    pub by: String,
    pub sig: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    Owner,
    Admin,
    Member,
}

impl Role {
    pub fn is_admin(self) -> bool {
        matches!(self, Role::Owner | Role::Admin)
    }

    pub fn word(self) -> &'static str {
        match self {
            Role::Owner => "Owner",
            Role::Admin => "Admin",
            Role::Member => "Member",
        }
    }
}

/// One of the caller's teams, as `GET /teams` lists it.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub struct TeamRow {
    pub team_id: String,
    pub name: String,
    pub role: Role,
    pub epoch: u64,
    pub rotation_pending: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub struct MemberRow {
    pub github_id: u64,
    pub login: String,
    pub role: Role,
    pub joined_at: u64,
    pub first_key_epoch: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub struct ChannelRow {
    pub channel_id: String,
    pub name_ct: Sealed,
    pub created_at: u64,
}

/// `GET /teams/<team_id>`.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub struct TeamDetail {
    pub team_id: String,
    pub name: String,
    pub epoch: u64,
    #[serde(default)]
    pub keyed_epoch: Option<u64>,
    /// Absent on Workers that cannot guard a historical grant at write time.
    pub roster_epoch: Option<u64>,
    pub rotation_pending: bool,
    pub members: Vec<MemberRow>,
    pub channels: Vec<ChannelRow>,
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub struct InviteRow {
    pub invite_id: String,
    pub team_id: String,
    pub team_name: String,
    pub invited_by_login: Option<String>,
    pub created_at: u64,
    pub expires_at: u64,
    /// How many members the team has; older servers leave it out.
    #[serde(default)]
    pub member_count: Option<u64>,
}

/// An open invite of a team, as its admins see it (`GET /teams/<id>/invites`).
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub struct TeamInvite {
    pub invite_id: String,
    /// Lower case.
    pub login: String,
    #[serde(default)]
    pub invited_by_login: Option<String>,
    pub created_at: u64,
    pub expires_at: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub struct PendingRow {
    pub request_id: String,
    pub github_id: u64,
    pub login: String,
    pub requested_at: u64,
    pub devices: Vec<DeviceRow>,
    /// When they last reminded the admins.
    #[serde(default)]
    pub reminded_at: Option<u64>,
}

/// A team this user asked to join, waiting for an admin (`GET /pending`).
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub struct WaitingRow {
    pub team_id: String,
    pub team_name: String,
    pub requested_at: u64,
    #[serde(default)]
    pub invited_by_login: Option<String>,
    #[serde(default)]
    pub invited_by_id: Option<u64>,
    #[serde(default)]
    pub reminded_at: Option<u64>,
}

/// One of this device's envelopes, with when its epoch's key was made.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub struct DatedEnvelope {
    #[serde(flatten)]
    pub envelope: Envelope,
    /// Not signed; older servers leave it out.
    #[serde(default)]
    pub made_at: Option<u64>,
}

/// What `POST /teams/<id>/remind` answered.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Remind {
    Sent,
    /// Too soon: the next one may go at this time.
    TooSoon(u64),
}

/// A stored message, from history or the channel's socket.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub struct Stored {
    pub seq: u64,
    pub at: u64,
    pub message: WireMessage,
}

/// One teams call: the session, and the device it is made from when the
/// server needs to know (`X-Arakun-Device`).
pub struct Call<'a> {
    pub base: &'a str,
    pub session: &'a str,
    pub device: Option<&'a str>,
}

/// A history page holds up to 200 messages of up to 64 KiB each.
const HISTORY_LIMIT: u64 = 32 * 1024 * 1024;
const SMALL_LIMIT: u64 = 1024 * 1024;
/// The pending list comes in one answer with up to 20 devices per person
/// (`MAX_DEVICES` in the Worker), so about 10 KiB a person: room for some
/// 3,000 people waiting.
const PENDING_LIMIT: u64 = 32 * 1024 * 1024;

impl Call<'_> {
    fn send(
        &self,
        method: &str,
        path: &str,
        body: Option<serde_json::Value>,
        limit: u64,
    ) -> Result<(u16, String), ApiError> {
        let mut req = ureq::http::Request::builder()
            .method(method)
            .uri(format!("{}{path}", self.base))
            .header("Authorization", format!("Bearer {}", self.session));
        if let Some(device) = self.device {
            req = req.header(DEVICE_HEADER, device);
        }
        let sent = match body {
            Some(body) => req
                .header("Content-Type", "application/json")
                .body(body.to_string())
                .map_err(|e| ApiError::Network(e.to_string()))
                .and_then(|r| {
                    server(self.base)
                        .run(r)
                        .map_err(|e| ApiError::Network(e.to_string()))
                }),
            None => req
                .body(())
                .map_err(|e| ApiError::Network(e.to_string()))
                .and_then(|r| {
                    server(self.base)
                        .run(r)
                        .map_err(|e| ApiError::Network(e.to_string()))
                }),
        };
        read(sent, limit)
    }

    fn get<T: serde::de::DeserializeOwned>(&self, path: &str) -> Result<T, ApiError> {
        let (status, body) = self.send("GET", path, None, SMALL_LIMIT)?;
        json(status, &body)
    }

    fn post<T: serde::de::DeserializeOwned>(
        &self,
        method: &str,
        path: &str,
        body: serde_json::Value,
    ) -> Result<T, ApiError> {
        let (status, text) = self.send(method, path, Some(body), SMALL_LIMIT)?;
        // 201 Created answers the same JSON as a 200.
        json(if status == 201 { 200 } else { status }, &text)
    }

    /// `POST /devices`: the device's ID, for a new device or one known already.
    pub fn register_device(
        &self,
        sig_pub: &str,
        enc_pub: &str,
        name: &str,
        endorsement: Option<Endorsement>,
    ) -> Result<String, ApiError> {
        #[derive(Deserialize)]
        struct Registered {
            device_id: String,
        }
        let body = serde_json::json!({
            "sig_pub": sig_pub, "enc_pub": enc_pub, "name": name, "endorsement": endorsement,
        });
        self.post::<Registered>("POST", "/devices", body)
            .map(|r| r.device_id)
    }

    pub fn devices(&self, github_id: u64) -> Result<Vec<DeviceRow>, ApiError> {
        self.get(&format!("/users/{github_id}/devices"))
    }

    pub fn create_team(&self, name: &str) -> Result<String, ApiError> {
        #[derive(Deserialize)]
        struct Created {
            team_id: String,
        }
        self.post::<Created>("POST", "/teams", serde_json::json!({ "name": name }))
            .map(|c| c.team_id)
    }

    pub fn teams(&self) -> Result<Vec<TeamRow>, ApiError> {
        self.get("/teams")
    }

    pub fn team(&self, team_id: &str) -> Result<TeamDetail, ApiError> {
        self.get(&format!("/teams/{team_id}"))
    }

    pub fn invite(&self, team_id: &str, login: &str) -> Result<(), ApiError> {
        let body = serde_json::json!({ "github_login": login });
        self.post::<serde_json::Value>("POST", &format!("/teams/{team_id}/invites"), body)
            .map(|_| ())
    }

    pub fn invites(&self) -> Result<Vec<InviteRow>, ApiError> {
        self.get("/invites")
    }

    pub fn decline(&self, invite_id: &str) -> Result<(), ApiError> {
        let path = format!("/invites/{invite_id}/decline");
        let (status, body) = self.send("POST", &path, Some(serde_json::json!({})), SMALL_LIMIT)?;
        nothing(status, &body)
    }

    pub fn team_invites(&self, team_id: &str) -> Result<Vec<TeamInvite>, ApiError> {
        self.get(&format!("/teams/{team_id}/invites"))
    }

    pub fn withdraw(&self, team_id: &str, invite_id: &str) -> Result<(), ApiError> {
        let path = format!("/teams/{team_id}/invites/{invite_id}");
        let (status, body) = self.send("DELETE", &path, None, SMALL_LIMIT)?;
        nothing(status, &body)
    }

    /// The teams this user asked to join, oldest first.
    pub fn waiting(&self) -> Result<Vec<WaitingRow>, ApiError> {
        self.get("/pending")
    }

    pub fn remind(&self, team_id: &str) -> Result<Remind, ApiError> {
        let path = format!("/teams/{team_id}/remind");
        let (status, body) = self.send("POST", &path, Some(serde_json::json!({})), SMALL_LIMIT)?;
        remind_answer(status, &body)
    }

    pub fn accept(&self, invite_id: &str) -> Result<(), ApiError> {
        let path = format!("/invites/{invite_id}/accept");
        self.post::<serde_json::Value>("POST", &path, serde_json::json!({}))
            .map(|_| ())
    }

    pub fn pending(&self, team_id: &str) -> Result<Vec<PendingRow>, ApiError> {
        let path = format!("/teams/{team_id}/pending");
        let (status, body) = self.send("GET", &path, None, PENDING_LIMIT)?;
        json(status, &body)
    }

    /// `devices` and `keyed_epoch` are what the admin reviewed, so a device
    /// added or a rotation ended since then answers `409`, not `400`.
    pub fn approve(
        &self,
        team_id: &str,
        github_id: u64,
        request_id: &str,
        devices: &[String],
        keyed_epoch: Option<u64>,
        envelopes: &[Envelope],
    ) -> Result<(), ApiError> {
        let path = format!("/teams/{team_id}/pending/{github_id}/approve");
        let body = approval(request_id, devices, keyed_epoch, envelopes);
        self.post::<serde_json::Value>("POST", &path, body)
            .map(|_| ())
    }

    pub fn reject(&self, team_id: &str, github_id: u64, request_id: &str) -> Result<(), ApiError> {
        let path = format!("/teams/{team_id}/pending/{github_id}/reject");
        let reviewed = serde_json::json!({ "request_id": request_id });
        let (status, body) = self.send("POST", &path, Some(reviewed), SMALL_LIMIT)?;
        nothing(status, &body)
    }

    /// `PUT /teams/<id>/envelopes`: whether a rotation is still pending after.
    pub fn put_envelopes(
        &self,
        team_id: &str,
        keyed_epoch: u64,
        roster_epoch: u64,
        envelopes: &[Envelope],
    ) -> Result<bool, ApiError> {
        #[derive(Deserialize)]
        struct Put {
            rotation_pending: bool,
        }
        let path = format!("/teams/{team_id}/envelopes");
        let body = serde_json::json!({ "keyed_epoch": keyed_epoch, "roster_epoch": roster_epoch, "envelopes": envelopes });
        self.post::<Put>("PUT", &path, body)
            .map(|p| p.rotation_pending)
    }

    /// This device's envelopes, oldest epoch first.
    pub fn dated_envelopes(&self, team_id: &str) -> Result<Vec<DatedEnvelope>, ApiError> {
        self.get(&format!("/teams/{team_id}/envelopes"))
    }

    pub fn remove_member(&self, team_id: &str, github_id: u64) -> Result<(), ApiError> {
        let path = format!("/teams/{team_id}/members/{github_id}");
        let (status, body) = self.send("DELETE", &path, None, SMALL_LIMIT)?;
        nothing(status, &body)
    }

    pub fn create_channel(
        &self,
        team_id: &str,
        channel_id: &str,
        name_ct: &Sealed,
    ) -> Result<(), ApiError> {
        let body = serde_json::json!({ "channel_id": channel_id, "name_ct": name_ct });
        self.post::<serde_json::Value>("POST", &format!("/teams/{team_id}/channels"), body)
            .map(|_| ())
    }

    /// `PUT /teams/<id>/channels/<id>`: the channel's name, sealed again.
    pub fn rename_channel(
        &self,
        team_id: &str,
        channel_id: &str,
        name_ct: &Sealed,
    ) -> Result<(), ApiError> {
        let body = serde_json::json!({ "name_ct": name_ct });
        let path = format!("/teams/{team_id}/channels/{channel_id}");
        self.post::<serde_json::Value>("PUT", &path, body)
            .map(|_| ())
    }

    /// Up to 200 messages after `after`, oldest first.
    pub fn history(
        &self,
        team_id: &str,
        channel_id: &str,
        after: u64,
    ) -> Result<Vec<Stored>, ApiError> {
        let path =
            format!("/teams/{team_id}/channels/{channel_id}/messages?after={after}&limit=200");
        let (status, body) = self.send("GET", &path, None, HISTORY_LIMIT)?;
        json(status, &body)
    }
}

/// The header that says which of the user's devices makes a call.
pub const DEVICE_HEADER: &str = "X-Arakun-Device";

/// The channel's socket, at the server's address with `ws` for `http`.
pub fn socket_url(base: &str, team_id: &str, channel_id: &str) -> String {
    let ws = if let Some(rest) = base.strip_prefix("https://") {
        format!("wss://{rest}")
    } else if let Some(rest) = base.strip_prefix("http://") {
        format!("ws://{rest}")
    } else {
        base.to_string()
    };
    format!("{ws}/teams/{team_id}/channels/{channel_id}/ws")
}

fn remind_answer(status: u16, body: &str) -> Result<Remind, ApiError> {
    if status == 429 {
        let at = serde_json::from_str::<serde_json::Value>(body)
            .ok()
            .and_then(|v| v["retry_at"].as_u64());
        if let Some(at) = at {
            return Ok(Remind::TooSoon(at));
        }
    }
    nothing(status, body).map(|()| Remind::Sent)
}

/// An answer that carries nothing but its status.
fn nothing(status: u16, body: &str) -> Result<(), ApiError> {
    match status {
        200..=299 => Ok(()),
        401 => Err(ApiError::Unauthorized),
        s => Err(ApiError::Status(s, server_error(body))),
    }
}

fn answer(
    sent: Result<ureq::http::Response<ureq::Body>, ureq::Error>,
) -> Result<(u16, String), ApiError> {
    read(
        sent.map_err(|e| ApiError::Network(e.to_string())),
        64 * 1024,
    )
}

fn read(
    sent: Result<ureq::http::Response<ureq::Body>, ApiError>,
    limit: u64,
) -> Result<(u16, String), ApiError> {
    let mut res = sent?;
    let status = res.status().as_u16();
    let body = res
        .body_mut()
        .with_config()
        .limit(limit)
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

/// The body of an approval. A Worker without the team's `keyed_epoch` gave
/// the client none to send back, so it is left out then.
fn approval(
    request_id: &str,
    devices: &[String],
    keyed_epoch: Option<u64>,
    envelopes: &[Envelope],
) -> serde_json::Value {
    let mut body = serde_json::json!({
        "request_id": request_id,
        "devices": devices,
        "envelopes": envelopes,
    });
    if let Some(epoch) = keyed_epoch {
        body["keyed_epoch"] = epoch.into();
    }
    body
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
pub(super) fn ipv4_first(addrs: &mut [std::net::SocketAddr]) {
    addrs.sort_by_key(std::net::SocketAddr::is_ipv6);
}

#[cfg(test)]
mod tests {
    use super::*;

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

    #[test]
    fn only_a_persons_account_is_a_github_user() {
        let user = r#"{"login":"rik","name":"Rik","type":"User"}"#;
        assert_eq!(
            read_github_user(user).unwrap().unwrap(),
            GithubUser {
                login: "rik".into(),
                name: Some("Rik".into()),
            }
        );
        // The server invites only what GitHub calls a user.
        for other in [
            r#"{"login":"ArakunHQ","name":null,"type":"Organization"}"#,
            r#"{"login":"some-bot","type":"Bot"}"#,
            r#"{"login":"rik"}"#,
        ] {
            assert_eq!(read_github_user(other).unwrap(), None, "{other}");
        }
        assert!(matches!(
            read_github_user("<html>"),
            Err(ApiError::Unreadable)
        ));
    }

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
    fn reads_the_new_invite_and_waiting_fields() {
        let old: InviteRow = serde_json::from_str(
            r#"{"invite_id":"I","team_id":"T","team_name":"Arakun","invited_by_login":"karlo","created_at":1,"expires_at":2}"#,
        )
        .unwrap();
        assert_eq!(old.member_count, None);
        let new: InviteRow = serde_json::from_str(
            r#"{"invite_id":"I","team_id":"T","team_name":"Arakun","invited_by_login":null,"created_at":1,"expires_at":2,"member_count":3}"#,
        )
        .unwrap();
        assert_eq!(new.member_count, Some(3));
        let w: Vec<WaitingRow> = serde_json::from_str(
            r#"[{"team_id":"T","team_name":"Arakun","requested_at":5,"invited_by_login":"karlo","invited_by_id":7,"reminded_at":null}]"#,
        )
        .unwrap();
        assert_eq!(w[0].invited_by_id, Some(7));
        assert_eq!(w[0].reminded_at, None);
        let d: DatedEnvelope = serde_json::from_str(
            r#"{"v":1,"team_id":"T","epoch":3,"device_id":"d","from":"f","eph_pub":"e","nonce":"n","ct":"c","sig":"s","made_at":99}"#,
        )
        .unwrap();
        assert_eq!((d.envelope.epoch, d.made_at), (3, Some(99)));
    }

    #[test]
    fn a_reminder_too_soon_says_when() {
        assert_eq!(remind_answer(204, ""), Ok(Remind::Sent));
        assert_eq!(
            remind_answer(429, r#"{"error":"too soon","retry_at":1800000000}"#),
            Ok(Remind::TooSoon(1_800_000_000))
        );
        assert!(matches!(
            remind_answer(429, "{}"),
            Err(ApiError::Status(429, _))
        ));
        assert!(matches!(
            remind_answer(404, "{}"),
            Err(ApiError::Status(404, _))
        ));
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
    fn a_token_poll_that_is_not_json_is_unreadable() {
        assert!(matches!(
            token_answer(200, "<html>"),
            Err(ApiError::Unreadable)
        ));
        assert!(matches!(
            token_answer(502, "<html>"),
            Err(ApiError::Status(502, _))
        ));
        let pending = token_answer(200, r#"{"error": "authorization_pending"}"#).unwrap();
        assert_eq!(pending.error.as_deref(), Some("authorization_pending"));
    }

    #[test]
    fn an_approval_names_what_the_admin_reviewed() {
        let devices = ["d1".to_string(), "d2".to_string()];
        let body = approval("r", &devices, Some(4), &[]);
        assert_eq!(body["request_id"], "r");
        assert_eq!(body["devices"], serde_json::json!(["d1", "d2"]));
        assert_eq!(body["keyed_epoch"], 4);
        assert_eq!(body["envelopes"], serde_json::json!([]));
        let older = approval("r", &devices, None, &[]);
        assert!(older.get("keyed_epoch").is_none());
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
