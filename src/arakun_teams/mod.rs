//! Arakun Teams, the pilot: sign in with GitHub, and the flags the Teams
//! server sets per account. The Teams tab shows only to accounts with the
//! `teams` flag. Named apart from the private `team` hub, which is something else.
//!
//! The app runs GitHub's device flow, hands the GitHub token to the server once
//! and keeps only the session the server answers with, in the system keyring.

pub mod api;
pub mod crypto;
pub mod hub;
pub mod socket;
pub mod store;
pub mod view;

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
    /// Asks `/me` again after it failed for a reason other than a 401, such as
    /// starting offline, so the Teams tab can still show in this run.
    retry: Option<Task<()>>,
    retry_after: Duration,
}

/// The first wait before asking `/me` again; it doubles up to `LAST_RETRY`.
const FIRST_RETRY: Duration = Duration::from_secs(15);
const LAST_RETRY: Duration = Duration::from_secs(10 * 60);

fn next_retry(after: Duration) -> Duration {
    (after * 2).min(LAST_RETRY)
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
            retry: None,
            retry_after: FIRST_RETRY,
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
                self.stop_retrying();
            }
            // The server ended the session (it expired, or was signed out
            // elsewhere): forget it here too.
            Err(ApiError::Unauthorized) => {
                self.session = None;
                self.me = None;
                self.error = Some("The Teams session ended. Sign in again.".into());
                self.stop_retrying();
                cx.background_spawn(async move { keyring::clear(&session_account()) })
                    .detach();
            }
            Err(e) => {
                self.error = Some(format!("Teams: {e}").into());
                self.retry_later(cx);
            }
        }
    }

    fn retry_later(&mut self, cx: &mut Context<Self>) {
        let wait = self.retry_after;
        self.retry_after = next_retry(wait);
        self.retry = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(wait).await;
            let _ = this.update(cx, |this, cx| this.refresh_if_unknown(cx));
        }));
    }

    fn stop_retrying(&mut self) {
        self.retry = None;
        self.retry_after = FIRST_RETRY;
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
        self.stop_retrying();
        cx.notify();
        if fixture().is_some() {
            return;
        }
        let base = self.base.clone().ok();
        cx.background_spawn(async move {
            end_session(
                &session,
                base.as_deref(),
                || keyring::clear(&session_account()),
                api::logout,
            );
        })
        .detach();
    }
}

/// Forgets the session here before asking the server to end it. The server
/// call can take up to 20 s: quitting during it must not leave the session
/// stored, and a sign-in right after must not be cleared by it.
fn end_session(
    session: &str,
    base: Option<&str>,
    clear: impl FnOnce(),
    logout: impl FnOnce(&str, &str) -> Result<(), ApiError>,
) {
    clear();
    if let Some(base) = base
        && let Err(e) = logout(base, session)
        && e != ApiError::Unauthorized
    {
        eprintln!("arakun: Teams sign-out: {e}");
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
    let mut dropped = 0;
    let token = loop {
        cx.background_executor()
            .timer(Duration::from_secs(interval))
            .await;
        if Instant::now() >= deadline {
            return Err(EXPIRED.into());
        }
        let device = code.device_code.clone();
        let answer = match cx
            .background_spawn(async move { api::poll_token(&device) })
            .await
        {
            Ok(answer) => {
                dropped = 0;
                answer
            }
            Err(e) if poll_again(&e, dropped) => {
                dropped += 1;
                continue;
            }
            Err(e) => return Err(format!("GitHub: {e}")),
        };
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
        let saved = keyring::store(&session_account(), "arakun: Teams session", &signed.session);
        Ok((signed, saved))
    })
    .await
}

const EXPIRED: &str = "The code ran out before it was entered on GitHub. Try again.";

/// Polls in a row that may get no answer before the sign-in gives up.
const POLL_DROPS: u32 = 3;

/// The user may be halfway through approving on GitHub, so a poll that got no
/// answer is tried again at the next interval, a few times in a row at most.
fn poll_again(e: &ApiError, dropped: u32) -> bool {
    matches!(e, ApiError::Network(_)) && dropped < POLL_DROPS
}

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
    keyring::lookup(&session_account()).filter(|s| session_ok(s))
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

    #[test]
    fn a_poll_without_an_answer_is_tried_again_a_few_times() {
        let offline = ApiError::Network("timed out".into());
        assert!(poll_again(&offline, 0));
        assert!(poll_again(&offline, POLL_DROPS - 1));
        assert!(!poll_again(&offline, POLL_DROPS));
        assert!(!poll_again(&ApiError::Status(500, "down".into()), 0));
        assert!(!poll_again(&ApiError::Unreadable, 0));
    }

    #[test]
    fn me_is_asked_again_less_and_less_often() {
        let mut after = FIRST_RETRY;
        let mut waits = Vec::new();
        for _ in 0..8 {
            waits.push(after.as_secs());
            after = next_retry(after);
        }
        assert_eq!(waits, [15, 30, 60, 120, 240, 480, 600, 600]);
    }

    #[test]
    fn sign_out_forgets_the_session_before_asking_the_server() {
        let steps = std::cell::RefCell::new(Vec::new());
        end_session(
            "the-session",
            Some("https://teams.example"),
            || steps.borrow_mut().push("clear".to_string()),
            |base, session| {
                steps.borrow_mut().push(format!("logout {base} {session}"));
                Err(ApiError::Network("offline".into()))
            },
        );
        assert_eq!(
            steps.into_inner(),
            ["clear", "logout https://teams.example the-session"]
        );
    }

    #[test]
    fn sign_out_without_a_server_still_forgets_the_session() {
        let mut cleared = false;
        end_session(
            "the-session",
            None,
            || cleared = true,
            |_, _| panic!("no server to ask"),
        );
        assert!(cleared);
    }
}
