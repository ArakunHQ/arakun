//! Arakun Teams, the pilot: sign in with GitHub, and the flags the Teams
//! server sets per account. The Teams tab shows only to accounts with the
//! `teams` flag. Named apart from the private `team` hub, which is something else.
//!
//! The app runs GitHub's device flow, hands the GitHub token to the server once
//! and keeps only the session the server answers with, in the system keyring.

pub mod api;
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
    /// A `/me` request is on its way, so a second one would only double it.
    asking: bool,
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

/// A request is on its way, or a retry waits for its turn (`retry` stays set
/// from the first failure until an answer). Asking now as well would send a
/// second request, and a second failure would double the wait again.
fn already_asking(asking: bool, retry: bool) -> bool {
    asking || retry
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
            error: base.err().map(Into::into),
            _task: None,
            asking: false,
            retry: None,
            retry_after: FIRST_RETRY,
        };
        // The keyring is read even when the server's address is wrong, so a
        // stored session still shows and Sign out can clear it.
        cx.spawn(async move |this, cx| {
            let found = cx.background_spawn(async move { load_session() }).await;
            let _ = this.update(cx, |this, cx| {
                // Signed in is known from here on; the server's answer comes later.
                this.loaded = true;
                if this.session.is_none() {
                    this.session = found;
                }
                this.ask_me(cx);
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
        if self.me.is_some() || self.phase != Phase::Idle || fixture().is_some() {
            return;
        }
        if already_asking(self.asking, self.retry.is_some()) {
            return;
        }
        self.ask_me(cx);
    }

    /// Asks the server who the session belongs to. Does nothing signed out, or
    /// when the server's address is wrong.
    fn ask_me(&mut self, cx: &mut Context<Self>) {
        let (Some(session), Ok(base)) = (self.session.clone(), self.base.clone()) else {
            return;
        };
        self.asking = true;
        self._task = Some(cx.spawn(async move |this, cx| {
            let me = cx
                .background_spawn(async move {
                    match fixture() {
                        Some(path) => read_fixture(&path),
                        None => api::me(&base, &session),
                    }
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                this.asking = false;
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
                cx.background_spawn(async move { keyring::clear(KEYRING_ACCOUNT) })
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
            // Not `refresh_if_unknown`: that one waits for this very retry.
            let _ = this.update(cx, |this, cx| this.ask_me(cx));
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
        if !can_cancel(&self.phase) {
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
        self.asking = false;
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
                || keyring::clear(KEYRING_ACCOUNT),
                api::logout,
            );
        })
        .detach();
    }
}

/// Once GitHub said yes, the last step can't be stopped: dropping its task
/// would not keep it from making the session and storing it, and the UI would
/// say signed out with a session in the keyring. So it runs to its end.
fn can_cancel(phase: &Phase) -> bool {
    !matches!(phase, Phase::Idle | Phase::Finishing)
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
        let saved = keyring::store(KEYRING_ACCOUNT, "arakun: Teams session", &signed.session);
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
    fn me_is_not_asked_twice_at_once() {
        assert!(!already_asking(false, false));
        // The first request after start, with Settings opening during it.
        assert!(already_asking(true, false));
        // A retry waits, or its request is on its way.
        assert!(already_asking(false, true));
    }

    #[test]
    fn a_sign_in_that_github_approved_is_not_cancelled() {
        assert!(can_cancel(&Phase::Starting));
        assert!(can_cancel(&Phase::Waiting {
            user_code: "ABCD-1234".into(),
            verification_uri: "https://github.com/login/device".into(),
        }));
        assert!(!can_cancel(&Phase::Finishing));
        assert!(!can_cancel(&Phase::Idle));
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
