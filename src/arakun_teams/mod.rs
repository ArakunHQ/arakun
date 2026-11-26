//! Arakun Teams, the pilot: sign in with GitHub, and the flags the Teams
//! server sets per account. The Teams tab shows only to accounts with the
//! `teams` flag. Named apart from the private `team` hub, which is something else.
//!
//! The app runs GitHub's device flow, hands the GitHub token to the server once
//! and keeps only the session the server answers with, in the system keyring.

pub mod api;
pub mod bot;
pub mod bot_guard;
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

use crate::cloud::{self, Poll};
use crate::platform::keyring;
use api::{ApiError, Me};

/// The keyring entry of the session. A server other than the default keeps
/// its own: a `wrangler dev` server answers 401 to the real session, and that
/// 401 would clear it while the real server still holds it.
fn keyring_account() -> String {
    keyring_account_for(std::env::var(api::URL_ENV).ok().as_deref())
}

/// Keyed by the address as given, not as checked, so a session stored for a
/// server whose address is now wrong can still be found and cleared.
fn keyring_account_for(url: Option<&str>) -> String {
    let url = url.map_or("", |u| u.trim().trim_end_matches('/'));
    if url.is_empty() || url == api::DEFAULT_URL {
        "teams-session".into()
    } else {
        format!("teams-session@{url}")
    }
}

/// The session's keyring account (see `store::account`).
fn session_account() -> String {
    store::account(&keyring_account())
}

/// The account as the rest of the app reaches the Arakun service with it:
/// the relay, the account's servers and its signed-in devices. No `Debug`:
/// the session opens the account, so it must never end up in a log.
#[derive(Clone, PartialEq, Eq)]
pub struct Account {
    /// The service's address.
    pub base: String,
    pub session: String,
    pub github_id: u64,
    pub login: String,
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
    /// The download failed at this time; asked for again after `PICTURE_RETRY`.
    Failed(Instant),
}

/// How long a failed picture waits before a later request tries it again.
/// Not at once: each failure notifies, and the views ask again on every
/// notification, so going offline would download in a loop.
const PICTURE_RETRY: Duration = Duration::from_secs(5 * 60);

/// Whether a request for a picture in state `p` should download it.
fn wants_download(p: Option<&Picture>, now: Instant) -> bool {
    match p {
        None => true,
        Some(Picture::Failed(at)) => now.duration_since(*at) >= PICTURE_RETRY,
        Some(Picture::Ready(_) | Picture::Pending(_)) => false,
    }
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
    /// A `/me` request is on its way, so a second one would only double it.
    asking: bool,
    /// When `/me` was last asked, to ask again once the answer is old.
    asked: Option<Instant>,
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

/// How old the server's answer may get before opening Settings asks again.
const STALE: Duration = Duration::from_secs(5 * 60);

fn stale(asked: Option<Instant>, now: Instant) -> bool {
    asked.is_none_or(|at| now.duration_since(at) >= STALE)
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
            pictures: HashMap::new(),
            pictures_busy: false,
            _task: None,
            asking: false,
            asked: None,
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

    /// The server and the session to call it with, once signed in to an
    /// account that has Teams.
    pub fn signed(&self) -> Option<(String, String, Me)> {
        let me = self.me.clone().filter(|m| m.has(api::TEAMS_FLAG))?;
        Some((self.base.clone().ok()?, self.session.clone()?, me))
    }

    /// The signed-in account, once the server has said who it is.
    pub fn account(&self) -> Option<Account> {
        let me = self.me.as_ref()?;
        Some(Account {
            base: self.base.clone().ok()?,
            session: self.session.clone()?,
            github_id: me.github_id,
            login: me.login.clone(),
        })
    }

    /// The service and the session to call it with, as soon as there is a
    /// session, before the server has said who it is.
    pub fn session(&self) -> Option<(String, String)> {
        Some((self.base.clone().ok()?, self.session.clone()?))
    }

    /// The operator's messages to this account not read yet.
    pub fn notices(&self) -> &[cloud::Notice] {
        self.me.as_ref().map_or(&[], |m| m.notices.as_slice())
    }

    /// Marks the operator's message `id` read, here and on the server, so
    /// `/me` stops listing it.
    pub fn read_notice(&mut self, id: &str, cx: &mut Context<Self>) {
        if let Some(me) = &mut self.me {
            me.notices.retain(|n| n.id != id);
        }
        let Some((base, session)) = self.session() else {
            return;
        };
        if fixture().is_some() {
            return;
        }
        let id = id.to_string();
        cx.spawn(async move |this, cx| {
            let read = cx
                .background_spawn(async move { cloud::read_notice(&base, &session, &id) })
                .await;
            if read == Err(cloud::Error::SignedOut) {
                let _ = this.update(cx, |this, cx| this.session_ended(cx));
            }
        })
        .detach();
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

    /// Whether Linear may start sessions here: the server set the `linear`
    /// flag on this account.
    pub fn linear_enabled(&self) -> bool {
        self.me.as_ref().is_some_and(|m| m.has(api::LINEAR_FLAG))
    }

    /// What the Teams tab can say about this account.
    pub fn access(&self) -> Access {
        access(
            self.signed_in(),
            self.me.as_ref().map(|m| m.has(api::TEAMS_FLAG)),
        )
    }

    pub fn phase(&self) -> &Phase {
        &self.phase
    }

    pub fn error(&self) -> Option<&SharedString> {
        self.error.as_ref()
    }

    /// Asks the server again who is signed in, when it could not say before.
    pub fn refresh_if_unknown(&mut self, cx: &mut Context<Self>) {
        if self.me.is_some() || !self.phase.at_rest() || fixture().is_some() {
            return;
        }
        if already_asking(self.asking, self.retry.is_some()) {
            return;
        }
        self.ask_me(cx);
    }

    /// Asks the server again who is signed in, because something else said
    /// the session ended (the relay turned it down). A 401 forgets it; any
    /// other answer leaves things as they are.
    pub fn recheck(&mut self, cx: &mut Context<Self>) {
        if self.session.is_none() || !self.phase.at_rest() || fixture().is_some() || self.asking {
            return;
        }
        self.ask_me(cx);
    }

    /// Asks the server again once its answer is old, so a flag it turned on
    /// or off since shows without a restart.
    pub fn refresh_if_stale(&mut self, cx: &mut Context<Self>) {
        if self.me.is_none() || !self.phase.at_rest() || fixture().is_some() {
            return;
        }
        if self.asking || !stale(self.asked, Instant::now()) {
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
        self.asked = Some(Instant::now());
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
                cx.background_spawn(async move { keyring::clear(&session_account()) })
                    .detach();
            }
            // Asking again failed: what the server said before still stands.
            Err(_) if self.me.is_some() => {}
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
                        this.asked = Some(Instant::now());
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
                || keyring::clear(&session_account()),
                api::logout,
            );
        })
        .detach();
    }
}

/// Whether the account may use Teams. Signed out, not answered yet and turned
/// off are told apart, so a signed-in account is never told to sign in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Access {
    SignedOut,
    /// Signed in, but the server has not said who it is.
    Unknown,
    /// Signed in, and the server has not turned Teams on for the account.
    Off,
    On,
}

/// `flag` is the account's `teams` flag, once the server answered.
fn access(signed_in: bool, flag: Option<bool>) -> Access {
    match (signed_in, flag) {
        (false, _) => Access::SignedOut,
        (true, None) => Access::Unknown,
        (true, Some(false)) => Access::Off,
        (true, Some(true)) => Access::On,
    }
}

/// Once GitHub said yes, the last step can't be stopped: dropping its task
/// would not keep it from making the session and storing it, and the UI would
/// say signed out with a session in the keyring. So it runs to its end.
fn can_cancel(phase: &Phase) -> bool {
    !matches!(phase, Phase::Idle | Phase::Finishing)
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
        let now = Instant::now();
        for login in logins {
            let key = login.to_ascii_lowercase();
            if key.is_empty()
                || !wants_download(self.pictures.get(&key), now)
                || want.contains(&key)
            {
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
                        (None, _) => Picture::Failed(Instant::now()),
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
) -> Result<(api::SignedIn, Result<(), String>), Ended> {
    // A server of one's own checks tokens against an OAuth App of its own.
    // Arakun's own for the default server, which is not asked.
    let asked = base.clone();
    let client_id: String = cx
        .background_spawn(async move { cloud::github_client_id(&asked) })
        .await
        .map_err(|e| Ended::Failed(format!("Sign-in: {e}")))?;
    let id = client_id.clone();
    let code = cx
        .background_spawn(async move { cloud::start_device_flow(&id) })
        .await
        .map_err(|e| Ended::Failed(format!("GitHub: {e}")))?;
    let deadline = Instant::now() + code.expires_in;
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

    let mut interval = code.interval;
    let mut dropped = 0;
    let token = loop {
        cx.background_executor().timer(interval).await;
        if Instant::now() >= deadline {
            return Err(Ended::Expired);
        }
        let device = code.device_code.clone();
        let id = client_id.clone();
        let answer = match cx
            .background_spawn(async move { cloud::poll_once(&id, &device, interval) })
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
            Err(e) => return Err(Ended::Failed(format!("GitHub: {e}"))),
        };
        match answer {
            Poll::Wait(pace) => interval = pace,
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
        let saved = keyring::store(&session_account(), "arakun: Teams session", &signed.session)
            .map_err(String::from);
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

/// Polls in a row that may get no answer before the sign-in gives up.
const POLL_DROPS: u32 = 3;

/// The user may be halfway through approving on GitHub, so a poll that got no
/// answer is tried again at the next interval, a few times in a row at most.
/// A 5xx from GitHub is not an error here: `cloud::poll_once` waits through it
/// until the code expires.
fn poll_again(e: &cloud::Error, dropped: u32) -> bool {
    matches!(e, cloud::Error::Unreachable(_)) && dropped < POLL_DROPS
}

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

    #[test]
    fn a_poll_without_an_answer_is_tried_again_a_few_times() {
        let offline = cloud::Error::Unreachable("timed out".into());
        assert!(poll_again(&offline, 0));
        assert!(poll_again(&offline, POLL_DROPS - 1));
        assert!(!poll_again(&offline, POLL_DROPS));
        assert!(!poll_again(&cloud::Error::Refused("bad".into()), 0));
        assert!(!poll_again(&cloud::Error::SignedOut, 0));
    }

    #[test]
    fn another_server_keeps_its_own_session() {
        assert_eq!(keyring_account_for(None), "teams-session");
        assert_eq!(keyring_account_for(Some(" ")), "teams-session");
        assert_eq!(
            keyring_account_for(Some("https://app.arakun.dev/")),
            "teams-session"
        );
        assert_eq!(
            keyring_account_for(Some("http://localhost:8787/")),
            "teams-session@http://localhost:8787"
        );
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
    fn a_failed_picture_is_tried_again_after_a_wait() {
        let now = Instant::now();
        assert!(wants_download(None, now));
        assert!(!wants_download(Some(&Picture::Pending(None)), now));
        assert!(!wants_download(Some(&Picture::Ready(PathBuf::new())), now));
        let failed = Picture::Failed(now);
        assert!(!wants_download(Some(&failed), now));
        assert!(!wants_download(
            Some(&failed),
            now + PICTURE_RETRY - Duration::from_secs(1)
        ));
        assert!(wants_download(Some(&failed), now + PICTURE_RETRY));
    }

    #[test]
    fn me_is_asked_again_once_the_answer_is_old() {
        let now = Instant::now();
        assert!(stale(None, now));
        assert!(!stale(Some(now), now));
        assert!(!stale(Some(now), now + STALE - Duration::from_secs(1)));
        assert!(stale(Some(now), now + STALE));
    }

    #[test]
    fn a_signed_in_account_is_not_taken_for_signed_out() {
        assert_eq!(access(false, None), Access::SignedOut);
        // Offline at start, or `/me` still on its way.
        assert_eq!(access(true, None), Access::Unknown);
        assert_eq!(access(true, Some(false)), Access::Off);
        assert_eq!(access(true, Some(true)), Access::On);
    }

    #[test]
    fn a_sign_in_that_github_approved_is_not_cancelled() {
        assert!(can_cancel(&Phase::Starting));
        assert!(can_cancel(&Phase::Waiting {
            user_code: "ABCD-1234".into(),
            verification_uri: "https://github.com/login/device".into(),
            expires_at: Instant::now(),
        }));
        // A sign-in that ended without a session is put away the same way.
        assert!(can_cancel(&Phase::Expired {
            user_code: "ABCD-1234".into(),
        }));
        assert!(can_cancel(&Phase::Denied {
            user_code: "ABCD-1234".into(),
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
