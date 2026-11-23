//! The network side of Teams: the Teams server (`cloud/README.md`). The sign-in
//! itself (GitHub's device flow, then a session) is the account client's, in
//! `cloud`. Every call blocks, so callers run them off the UI thread. The types
//! mirror the server's JSON.

use serde::{Deserialize, Serialize};

use super::crypto::{Envelope, Sealed, WireMessage};

/// Arakun's OAuth App. Public: the device flow has no client secret, and the
/// server checks with GitHub that this app issued the token.
pub const CLIENT_ID: &str = "Ov23lipgmMeATCa5aTr3";
pub use crate::cloud::{DEFAULT_URL, URL_ENV, base_url};
/// The flag the server sets on accounts that may use Teams.
pub const TEAMS_FLAG: &str = "teams";

/// Who is signed in, as `GET /me` answers.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Me {
    pub github_id: u64,
    pub login: String,
    #[serde(default)]
    pub flags: Vec<String>,
    /// The operator's messages to this account not read yet. Not kept in a
    /// fixture: they come from the server each time.
    #[serde(default, skip_serializing)]
    pub notices: Vec<crate::cloud::Notice>,
}

impl Me {
    pub fn has(&self, flag: &str) -> bool {
        self.flags.iter().any(|f| f == flag)
    }
}

/// What `POST /auth/github` answers, as Teams keeps it.
#[derive(Clone, Debug, Deserialize)]
pub struct SignedIn {
    pub session: String,
    #[serde(flatten)]
    pub me: Me,
}

impl From<crate::cloud::SignedIn> for SignedIn {
    fn from(s: crate::cloud::SignedIn) -> Self {
        Self {
            session: s.session,
            me: Me {
                github_id: s.github_id,
                login: s.login,
                flags: s.flags,
                notices: Vec::new(),
            },
        }
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

/// Trades the GitHub token for a session, named after this machine.
pub fn sign_in(base: &str, github_token: &str) -> Result<SignedIn, crate::cloud::Error> {
    let name = super::hub::device_name();
    crate::cloud::sign_in(base, github_token, &name, "app").map(SignedIn::from)
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

// ---- teams (cloud/PROTOCOL.md) ------------------------------------------------

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

/// Who wrote with a device, as `GET .../senders/<device_id>` answers: the
/// user and all their devices, to check like a member's.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub struct SenderRow {
    pub github_id: u64,
    pub login: String,
    pub devices: Vec<DeviceRow>,
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

/// What `POST /teams/<id>/invites` answers. Each field may be missing on an
/// older server.
#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize)]
pub struct Invited {
    #[serde(default)]
    pub invite_id: String,
    /// As GitHub spells it now.
    #[serde(default)]
    pub login: String,
    #[serde(default)]
    pub expires_at: u64,
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
    /// The login the invite was sent to. GitHub logins can be renamed and
    /// taken by someone else, so a different `login` is worth a second look.
    #[serde(default)]
    pub invited_login: Option<String>,
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
/// `GET /devices` answers for everyone in up to 50 teams of 25, with up to 20
/// devices each: about 12 MiB at the most.
const PEOPLE_LIMIT: u64 = 32 * 1024 * 1024;

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

    /// `GET /devices`: the devices of the caller and everyone they share a
    /// team with, or with `users` only of those of them. A user out of reach
    /// is not in the map. A server from before this call answers 404.
    pub fn people_devices(
        &self,
        users: Option<&[u64]>,
    ) -> Result<std::collections::HashMap<u64, Vec<DeviceRow>>, ApiError> {
        #[derive(Deserialize)]
        struct Person {
            github_id: u64,
            devices: Vec<DeviceRow>,
        }
        let path = people_path(users);
        let (status, body) = self.send("GET", &path, None, PEOPLE_LIMIT)?;
        let people: Vec<Person> = json(status, &body)?;
        Ok(people
            .into_iter()
            .map(|p| (p.github_id, p.devices))
            .collect())
    }

    /// Who wrote in a channel with `device_id`, for a sender who is no
    /// longer a member. `None` when the server has no message of that
    /// device there, and from an older server, which has no such lookup.
    pub fn sender(
        &self,
        team_id: &str,
        channel_id: &str,
        device_id: &str,
    ) -> Result<Option<SenderRow>, ApiError> {
        let path = format!("/teams/{team_id}/channels/{channel_id}/senders/{device_id}");
        let (status, body) = self.send("GET", &path, None, SMALL_LIMIT)?;
        sender_answer(status, &body)
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

    pub fn invite(&self, team_id: &str, login: &str) -> Result<Invited, ApiError> {
        let body = serde_json::json!({ "github_login": login });
        self.post::<Invited>("POST", &format!("/teams/{team_id}/invites"), body)
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

    /// The `seq` of a channel's newest message, 0 when it has none: one
    /// small read that tells whether anything came since.
    pub fn newest(&self, team_id: &str, channel_id: &str) -> Result<u64, ApiError> {
        let path = format!("/teams/{team_id}/channels/{channel_id}/messages?before=latest&limit=1");
        let (status, body) = self.send("GET", &path, None, HISTORY_LIMIT)?;
        let page: Vec<Stored> = json(status, &body)?;
        Ok(page.last().map_or(0, |s| s.seq))
    }
}

/// The path of `GET /devices`, for everyone or for the users named.
fn people_path(users: Option<&[u64]>) -> String {
    match users {
        None => "/devices".to_string(),
        Some(ids) => {
            let ids: Vec<String> = ids.iter().map(u64::to_string).collect();
            format!("/devices?users={}", ids.join(","))
        }
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

fn sender_answer(status: u16, body: &str) -> Result<Option<SenderRow>, ApiError> {
    if status == 404 {
        return Ok(None);
    }
    json(status, body).map(Some)
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
    crate::cloud::http()
}

/// The agent for the Teams server at `base` (see `cloud::http_for`).
fn server(base: &str) -> &'static ureq::Agent {
    crate::cloud::http_for(base)
}

pub(super) use crate::cloud::ipv4_first;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn asks_for_everyone_or_the_users_named() {
        assert_eq!(people_path(None), "/devices");
        assert_eq!(people_path(Some(&[7, 21])), "/devices?users=7,21");
        assert_eq!(people_path(Some(&[])), "/devices?users=");
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
    fn keeps_the_login_a_pending_member_was_invited_as() {
        let row = |extra: serde_json::Value| -> PendingRow {
            let mut v = serde_json::json!({
                "request_id": "r", "github_id": 7, "login": "new-login",
                "requested_at": 1, "devices": []
            });
            v.as_object_mut()
                .unwrap()
                .extend(extra.as_object().unwrap().clone());
            serde_json::from_value(v).unwrap()
        };
        let renamed = row(serde_json::json!({ "invited_login": "old-login" }));
        assert_eq!(renamed.invited_login.as_deref(), Some("old-login"));
        // Older servers leave it out.
        assert_eq!(row(serde_json::json!({})).invited_login, None);
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
    fn a_sender_the_server_does_not_know_is_none() {
        let found = r#"{"github_id": 7, "login": "gone", "devices": []}"#;
        assert_eq!(
            sender_answer(200, found)
                .unwrap()
                .map(|r| (r.github_id, r.login)),
            Some((7, "gone".into()))
        );
        // A device with no message in the channel, or an older server.
        assert_eq!(
            sender_answer(404, r#"{"error":"not found"}"#).unwrap(),
            None
        );
        assert!(matches!(
            sender_answer(401, ""),
            Err(ApiError::Unauthorized)
        ));
        assert!(matches!(
            sender_answer(500, ""),
            Err(ApiError::Status(500, _))
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
}
