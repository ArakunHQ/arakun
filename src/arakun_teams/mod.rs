//! Arakun Teams, the pilot: sign in with GitHub, and the flags the Teams
//! server sets per account. The Teams tab shows only to accounts with the
//! `teams` flag. Named apart from the private `team` hub, which is something else.
//!
//! The app runs GitHub's device flow, hands the GitHub token to the server once
//! and keeps only the session the server answers with, in the system keyring.

pub mod api;
#[allow(dead_code)] // the Teams tab uses it from the next commit on
pub mod crypto;
pub mod view;

use std::time::{Duration, Instant};

use gpui::{AppContext as _, AsyncApp, Context, SharedString, Task, WeakEntity};

use crate::platform::keyring;
use api::{ApiError, Me, Poll};

const KEYRING_ACCOUNT: &str = "teams-session";

/// Where the sign-in stands.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Phase {
    Idle,
    /// Asking GitHub for a code.
    Starting,
    /// Waiting for the user to enter `user_code` at `verification_uri`.
    Waiting {
        user_code: SharedString,
        verification_uri: SharedString,
    },
    /// GitHub said yes; trading its token for a session.
    Finishing,
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
        if self.me.is_some() || self.phase != Phase::Idle || fixture().is_some() {
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
                cx.background_spawn(async move { keyring::clear(KEYRING_ACCOUNT) })
                    .detach();
            }
            Err(e) => self.error = Some(format!("Teams: {e}").into()),
        }
    }

    /// Starts GitHub's device flow: shows a code, opens GitHub's page for it,
    /// and waits for the user to approve there.
    pub fn sign_in(&mut self, cx: &mut Context<Self>) {
        if self.session.is_some() || self.phase != Phase::Idle {
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
            let result = device_flow(&this, base, cx).await;
            let _ = this.update(cx, |this, cx| {
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
                    Err(e) => this.error = Some(e.into()),
                }
                cx.notify();
            });
        }));
    }

    /// Stops a sign-in that is waiting on GitHub.
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
            keyring::clear(KEYRING_ACCOUNT);
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
) -> Result<(api::SignedIn, Result<(), String>), String> {
    let code = cx
        .background_spawn(async move { api::device_code() })
        .await
        .map_err(|e| format!("GitHub: {e}"))?;
    let deadline = Instant::now() + Duration::from_secs(code.expires_in);
    let uri = code.verification_uri.clone();
    this.update(cx, |this, cx| {
        this.phase = Phase::Waiting {
            user_code: code.user_code.clone().into(),
            verification_uri: uri.clone().into(),
        };
        cx.open_url(&uri);
        cx.notify();
    })
    .map_err(|e| e.to_string())?;

    let mut interval = code.interval.max(1);
    let token = loop {
        cx.background_executor()
            .timer(Duration::from_secs(interval))
            .await;
        if Instant::now() >= deadline {
            return Err(EXPIRED.into());
        }
        let device = code.device_code.clone();
        let answer = cx
            .background_spawn(async move { api::poll_token(&device) })
            .await
            .map_err(|e| format!("GitHub: {e}"))?;
        match api::next_poll(&answer, interval) {
            Poll::Wait(secs) => interval = secs,
            Poll::Token(token) => break token,
            Poll::Expired => return Err(EXPIRED.into()),
            Poll::Denied => return Err("The sign-in was turned down on GitHub.".into()),
            Poll::Failed(e) => return Err(format!("GitHub: {e}")),
        }
    };

    this.update(cx, |this, cx| {
        this.phase = Phase::Finishing;
        cx.notify();
    })
    .map_err(|e| e.to_string())?;
    cx.background_spawn(async move {
        let signed = api::sign_in(&base, &token).map_err(|e| format!("Teams: {e}"));
        drop(token);
        let signed = signed?;
        let saved = keyring::store(KEYRING_ACCOUNT, "arakun: Teams session", &signed.session);
        Ok((signed, saved))
    })
    .await
}

const EXPIRED: &str = "The code ran out before it was entered on GitHub. Try again.";

/// The server's sessions are 32 random bytes in base64url.
fn session_ok(s: &str) -> bool {
    (16..=256).contains(&s.len())
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

fn load_session() -> Option<String> {
    if fixture().is_some() {
        return Some("fixture-session-0000000000".into());
    }
    keyring::lookup(KEYRING_ACCOUNT).filter(|s| session_ok(s))
}

/// `ARAKUN_TEAMS_FIXTURE=<me.json>` stands in for the session and `GET /me`,
/// to look at the UI offline (in `arakun dev`, say). The file holds what
/// `/me` would answer.
fn fixture() -> Option<std::path::PathBuf> {
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
