//! Arakun Teams, the pilot: sign in with GitHub, and the flags the Teams
//! server sets per account. The Teams tab shows only to accounts with the
//! `teams` flag. Named apart from the private `team` hub, which is something else.
//!
//! The app runs GitHub's device flow, hands the GitHub token to the server once
//! and keeps only the session the server answers with, in the system keyring.

pub mod api;
pub mod bot;
mod coord;
pub mod crypto;
pub mod decisions;
pub mod hub;
pub mod sign_in;
pub mod socket;
pub mod store;
pub mod view;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use gpui::{AppContext as _, AsyncApp, Context, SharedString, Task, WeakEntity};

use crate::platform::keyring;
use api::{ApiError, Me, Poll};

/// The session's keyring account (see `store::account`).
fn session_account() -> String {
    store::account("teams-session")
}

/// Where the sign-in stands.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Phase {
    Idle,
    /// Asking GitHub for a code.
    Starting,
    /// Waiting for the user to enter `user_code` at `verification_uri`
    /// before `expires_at`.
    Waiting {
        user_code: SharedString,
        verification_uri: SharedString,
        expires_at: Instant,
    },
    /// GitHub said yes; trading its token for a session.
    Finishing,
    /// The code ran out before GitHub heard back. Nothing was signed in.
    Expired {
        user_code: SharedString,
    },
    /// The request was turned down on GitHub.
    Denied {
        user_code: SharedString,
    },
}

impl Phase {
    /// Nothing under way: a sign-in can start.
    pub fn at_rest(&self) -> bool {
        matches!(
            self,
            Phase::Idle | Phase::Expired { .. } | Phase::Denied { .. }
        )
    }

    /// The code GitHub gave, while it means something to show.
    pub fn user_code(&self) -> Option<&SharedString> {
        match self {
            Phase::Waiting { user_code, .. }
            | Phase::Expired { user_code }
            | Phase::Denied { user_code } => Some(user_code),
            _ => None,
        }
    }
}

/// A GitHub profile picture of someone Teams shows.
enum Picture {
    /// Downloading; the old file shows meanwhile, if any.
    Pending(Option<PathBuf>),
    Ready(PathBuf),
    Failed,
}

pub struct Teams {
    base: Result<String, String>,
    session: Option<String>,
    /// Who the session belongs to; `None` until the server answered.
    me: Option<Me>,
    /// The keyring has been read.
    loaded: bool,
    phase: Phase,
    error: Option<SharedString>,
    /// Pictures by lower-case login, kept with the GitHub tab's in
    /// `github/avatars/`.
    pictures: HashMap<String, Picture>,
    pictures_busy: bool,
    _task: Option<Task<()>>,
}

impl Teams {
    pub fn new(cx: &mut Context<Self>) -> Self {
        let base = api::base_url();
        let this = Self {
            base: base.clone(),
            session: None,
            me: None,
            loaded: false,
            phase: Phase::Idle,
            error: None,
            pictures: HashMap::new(),
            pictures_busy: false,
            _task: None,
        };
        let base = match base {
            Ok(base) => base,
            Err(e) => {
                return Self {
                    loaded: true,
                    error: Some(e.into()),
                    ..this
                };
            }
        };
        cx.spawn(async move |this, cx| {
            let found = cx
                .background_spawn(async move {
                    let session = load_session()?;
                    let me = match fixture() {
                        Some(path) => read_fixture(&path),
                        None => api::me(&base, &session),
                    };
                    Some((session, me))
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                this.loaded = true;
                if let Some((session, me)) = found {
                    this.session = Some(session);
                    this.apply_me(me, cx);
                }
                cx.notify();
            });
        })
        .detach();
        this
    }

    /// The keyring has been read, so "signed out" is an answer, not a guess.
    pub fn ready(&self) -> bool {
        self.loaded
    }

    pub fn signed_in(&self) -> bool {
        self.session.is_some()
    }

    pub fn me(&self) -> Option<&Me> {
        self.me.as_ref()
    }

    /// The server and the session to call it with, once signed in to an
    /// account that has Teams.
    pub fn signed(&self) -> Option<(String, String, Me)> {
        let me = self.me.clone().filter(|m| m.has(api::TEAMS_FLAG))?;
        Some((self.base.clone().ok()?, self.session.clone()?, me))
    }

    /// The Teams server's host, as the encryption panel names it.
    pub fn server_host(&self) -> Option<String> {
        let base = self.base.as_ref().ok()?;
        let rest = base.split_once("://").map_or(base.as_str(), |(_, r)| r);
        Some(rest.split('/').next().unwrap_or(rest).to_string())
    }

    /// Every picture on disk, by lower-case login: a copy to draw from.
    pub fn pictures(&self) -> HashMap<String, PathBuf> {
        self.pictures
            .iter()
            .filter_map(|(k, p)| match p {
                Picture::Ready(p) | Picture::Pending(Some(p)) => Some((k.clone(), p.clone())),
                _ => None,
            })
            .collect()
    }

    /// The server ended the session in the middle of Teams' work: forget it,
    /// as a 401 to `/me` would.
    pub fn session_ended(&mut self, cx: &mut Context<Self>) {
        if self.session.is_some() {
            self.apply_me(Err(ApiError::Unauthorized), cx);
            cx.notify();
        }
    }

    /// Whether the Teams tab shows: the server set the `teams` flag on this account.
    pub fn enabled(&self) -> bool {
        self.me.as_ref().is_some_and(|m| m.has(api::TEAMS_FLAG))
    }

    pub fn phase(&self) -> &Phase {
        &self.phase
    }

    pub fn error(&self) -> Option<&SharedString> {
        self.error.as_ref()
    }

    /// Asks the server again who is signed in, when it could not say before.
    pub fn refresh_if_unknown(&mut self, cx: &mut Context<Self>) {
        let (Some(session), Ok(base)) = (self.session.clone(), self.base.clone()) else {
            return;
        };
        if self.me.is_some() || !self.phase.at_rest() || fixture().is_some() {
            return;
        }
        self._task = Some(cx.spawn(async move |this, cx| {
            let me = cx
                .background_spawn(async move { api::me(&base, &session) })
                .await;
            let _ = this.update(cx, |this, cx| {
                this.apply_me(me, cx);
                cx.notify();
            });
        }));
    }

    fn apply_me(&mut self, me: Result<Me, ApiError>, cx: &mut Context<Self>) {
        match me {
            Ok(me) => {
                self.me = Some(me);
                self.error = None;
            }
            // The server ended the session (it expired, or was signed out
            // elsewhere): forget it here too.
            Err(ApiError::Unauthorized) => {
                self.session = None;
                self.me = None;
                self.error = Some("The Teams session ended. Sign in again.".into());
                cx.background_spawn(async move { keyring::clear(&session_account()) })
                    .detach();
            }
            Err(e) => self.error = Some(format!("Teams: {e}").into()),
        }
    }

    /// Starts GitHub's device flow: shows a code, opens GitHub's page for it,
    /// and waits for the user to approve there.
    pub fn sign_in(&mut self, cx: &mut Context<Self>) {
        if self.session.is_some() || !self.phase.at_rest() {
            return;
        }
        let base = match &self.base {
            Ok(b) => b.clone(),
            Err(e) => {
                self.error = Some(e.clone().into());
                cx.notify();
                return;
            }
        };
        self.phase = Phase::Starting;
        self.error = None;
        cx.notify();
        self._task = Some(cx.spawn(async move |this, cx| {
            let result = match fixture_sign_in() {
                Some(outcome) => fixture_flow(&this, outcome, cx).await,
                None => device_flow(&this, base, cx).await,
            };
            let _ = this.update(cx, |this, cx| {
                let code = this.phase.user_code().cloned().unwrap_or_default();
                this.phase = Phase::Idle;
                match result {
                    Ok((signed, saved)) => {
                        this.session = Some(signed.session);
                        this.me = Some(signed.me);
                        this.error = saved.err().map(|e| {
                            format!(
                                "Signed in for now, but the keyring could not keep the session: {e}"
                            )
                            .into()
                        });
                    }
                    Err(Ended::Expired) => {
                        this.error = Some(EXPIRED.into());
                        this.phase = Phase::Expired { user_code: code };
                    }
                    Err(Ended::Denied) => {
                        this.error = Some(DENIED.into());
                        this.phase = Phase::Denied { user_code: code };
                    }
                    Err(Ended::Failed(e)) => this.error = Some(e.into()),
                }
                cx.notify();
            });
        }));
    }

    /// Stops a sign-in that is waiting on GitHub, or puts away one that
    /// ended without a session.
    pub fn cancel(&mut self, cx: &mut Context<Self>) {
        if self.phase == Phase::Idle {
            return;
        }
        self._task = None;
        self.phase = Phase::Idle;
        cx.notify();
    }

    /// Ends the session on the server and forgets it here, even when the
    /// server can't be reached.
    pub fn sign_out(&mut self, cx: &mut Context<Self>) {
        let Some(session) = self.session.take() else {
            return;
        };
        self.me = None;
        self.error = None;
        self.phase = Phase::Idle;
        self._task = None;
        cx.notify();
        if fixture().is_some() {
            return;
        }
        let base = self.base.clone().ok();
        cx.background_spawn(async move {
            if let Some(base) = base
                && let Err(e) = api::logout(&base, &session)
                && e != ApiError::Unauthorized
            {
                eprintln!("arakun: Teams sign-out: {e}");
            }
            keyring::clear(&session_account());
        })
        .detach();
    }
}

impl Teams {
    /// The GitHub picture of `login` on disk, while there is one. Ask for it
    /// with [`Teams::want_pictures`].
    pub fn picture(&self, login: &str) -> Option<&Path> {
        match self.pictures.get(&login.to_ascii_lowercase())? {
            Picture::Ready(p) | Picture::Pending(Some(p)) => Some(p),
            _ => None,
        }
    }

    /// Downloads the pictures of `logins` not on disk yet or older than a
    /// week, a batch at a time, as the GitHub tab does. What is on disk
    /// shows meanwhile.
    pub fn want_pictures(
        &mut self,
        logins: impl IntoIterator<Item = String>,
        cx: &mut Context<Self>,
    ) {
        use crate::github::avatars;
        let dir = avatars::dir();
        let mut want: Vec<String> = Vec::new();
        let mut found = false;
        for login in logins {
            let key = login.to_ascii_lowercase();
            if key.is_empty() || self.pictures.contains_key(&key) || want.contains(&key) {
                continue;
            }
            match avatars::cached(&dir, &key) {
                Some((path, false)) => {
                    self.pictures.insert(key, Picture::Ready(path));
                    found = true;
                }
                Some((path, true)) => {
                    self.pictures
                        .insert(key.clone(), Picture::Pending(Some(path)));
                    want.push(key);
                    found = true;
                }
                None => {
                    self.pictures.insert(key.clone(), Picture::Pending(None));
                    want.push(key);
                }
            }
        }
        if found {
            cx.notify();
        }
        if want.is_empty() || self.pictures_busy {
            return;
        }
        self.pictures_busy = true;
        cx.spawn(async move |this, cx| {
            let got = cx
                .background_spawn(async move {
                    want.iter()
                        .map(|l| (l.clone(), avatars::download(&dir, l).ok()))
                        .collect::<Vec<_>>()
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                this.pictures_busy = false;
                for (login, path) in got {
                    let state = match (path, this.pictures.remove(&login)) {
                        (Some(p), _) => Picture::Ready(p),
                        // A failed refresh keeps the old picture.
                        (None, Some(Picture::Pending(Some(old)))) => Picture::Ready(old),
                        (None, _) => Picture::Failed,
                    };
                    this.pictures.insert(login, state);
                }
                // People asked for while this batch ran.
                let waiting: Vec<String> = this
                    .pictures
                    .iter()
                    .filter(|(_, p)| matches!(p, Picture::Pending(_)))
                    .map(|(k, _)| k.clone())
                    .collect();
                for k in &waiting {
                    this.pictures.remove(k);
                }
                cx.notify();
                if !waiting.is_empty() {
                    this.want_pictures(waiting, cx);
                }
            });
        })
        .detach();
    }
}

/// The device flow from the first request to a saved session. The GitHub
/// token lives only inside it, and is dropped once the server has it.
async fn device_flow(
    this: &WeakEntity<Teams>,
    base: String,
    cx: &mut AsyncApp,
) -> Result<(api::SignedIn, Result<(), String>), Ended> {
    let code = cx
        .background_spawn(async move { api::device_code() })
        .await
        .map_err(|e| Ended::Failed(format!("GitHub: {e}")))?;
    let deadline = Instant::now() + Duration::from_secs(code.expires_in);
    let uri = code.verification_uri.clone();
    this.update(cx, |this, cx| {
        this.phase = Phase::Waiting {
            user_code: code.user_code.clone().into(),
            verification_uri: uri.clone().into(),
            expires_at: deadline,
        };
        cx.open_url(&uri);
        cx.notify();
    })
    .map_err(|e| Ended::Failed(e.to_string()))?;

    let mut interval = code.interval.max(1);
    let token = loop {
        cx.background_executor()
            .timer(Duration::from_secs(interval))
            .await;
        if Instant::now() >= deadline {
            return Err(Ended::Expired);
        }
        let device = code.device_code.clone();
        let answer = cx
            .background_spawn(async move { api::poll_token(&device) })
            .await
            .map_err(|e| Ended::Failed(format!("GitHub: {e}")))?;
        match api::next_poll(&answer, interval) {
            Poll::Wait(secs) => interval = secs,
            Poll::Token(token) => break token,
            Poll::Expired => return Err(Ended::Expired),
            Poll::Denied => return Err(Ended::Denied),
            Poll::Failed(e) => return Err(Ended::Failed(format!("GitHub: {e}"))),
        }
    };

    this.update(cx, |this, cx| {
        this.phase = Phase::Finishing;
        cx.notify();
    })
    .map_err(|e| Ended::Failed(e.to_string()))?;
    cx.background_spawn(async move {
        let signed = api::sign_in(&base, &token).map_err(|e| Ended::Failed(format!("Teams: {e}")));
        drop(token);
        let signed = signed?;
        let saved = keyring::store(&session_account(), "arakun: Teams session", &signed.session);
        Ok((signed, saved))
    })
    .await
}

/// How a device flow ended without a session.
enum Ended {
    Expired,
    Denied,
    Failed(String),
}

const EXPIRED: &str = "The code ran out before it was entered on GitHub. Try again.";
const DENIED: &str = "The sign-in was turned down on GitHub.";

/// The server's sessions are 32 random bytes in base64url.
fn session_ok(s: &str) -> bool {
    (16..=256).contains(&s.len())
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

fn load_session() -> Option<String> {
    if fixture().is_some() {
        // A sign-in to play starts signed out.
        return fixture_sign_in()
            .is_none()
            .then(|| "fixture-session-0000000000".into());
    }
    keyring::lookup(&session_account()).filter(|s| session_ok(s))
}

/// With the fixture, `ARAKUN_TEAMS_FIXTURE_SIGN_IN` starts signed out and
/// plays the sign-in dialog without GitHub: a made-up code that opens nothing,
/// then after a few seconds `ok` (signed in as the fixture), `expired` or
/// `denied`; anything else keeps waiting.
fn fixture_sign_in() -> Option<String> {
    fixture()?;
    std::env::var("ARAKUN_TEAMS_FIXTURE_SIGN_IN").ok()
}

/// The fixture's sign-in, as `device_flow` would run it.
async fn fixture_flow(
    this: &WeakEntity<Teams>,
    outcome: String,
    cx: &mut AsyncApp,
) -> Result<(api::SignedIn, Result<(), String>), Ended> {
    const CODE_LIFE: Duration = Duration::from_secs(15 * 60);
    this.update(cx, |this, cx| {
        this.phase = Phase::Waiting {
            user_code: "WDJB-MJHT".into(),
            verification_uri: "https://github.com/login/device".into(),
            expires_at: Instant::now() + CODE_LIFE - Duration::from_secs(28),
        };
        cx.notify();
    })
    .map_err(|e| Ended::Failed(e.to_string()))?;
    let wait = if matches!(outcome.as_str(), "ok" | "expired" | "denied") {
        Duration::from_secs(4)
    } else {
        CODE_LIFE
    };
    cx.background_executor().timer(wait).await;
    match outcome.as_str() {
        "expired" => Err(Ended::Expired),
        "denied" => Err(Ended::Denied),
        _ => {
            let path = fixture().ok_or_else(|| Ended::Failed("no fixture".into()))?;
            let me = read_fixture(&path).map_err(|e| Ended::Failed(e.to_string()))?;
            Ok((
                api::SignedIn {
                    session: "fixture-session-0000000000".into(),
                    me,
                },
                Ok(()),
            ))
        }
    }
}

/// `ARAKUN_TEAMS_FIXTURE=<me.json>` stands in for the session and `GET /me`,
/// to look at the UI offline (in `arakun dev`, say). The file holds what
/// `/me` would answer.
pub(crate) fn fixture() -> Option<std::path::PathBuf> {
    std::env::var_os("ARAKUN_TEAMS_FIXTURE").map(Into::into)
}

fn read_fixture(path: &std::path::Path) -> Result<Me, ApiError> {
    let text = std::fs::read_to_string(path).map_err(|e| ApiError::Network(e.to_string()))?;
    serde_json::from_str(&text).map_err(|_| ApiError::Unreadable)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sessions_look_like_the_servers() {
        assert!(session_ok("Qm9vbXNoYWthbGFrYS1ib29tc2hha2FsYWthLWJvb20"));
        assert!(!session_ok(""));
        assert!(!session_ok("short"));
        assert!(!session_ok("has spaces in it, which no session has"));
        assert!(!session_ok("Qm9vbXNoYWthbGFrYS1ib29tc2hha2FsYWthLWJvb20\n"));
    }
}
