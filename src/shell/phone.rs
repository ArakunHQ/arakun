//! The phone's side of the shell: turning on the server (`crate::phone`),
//! answering what phones ask, and sending them what changes. Entry changes
//! are gathered for a short while before they go out, so a streaming agent
//! costs a few messages a second, not one per token.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use futures::StreamExt as _;
use gpui::{App, AppContext as _, Context, SharedString, Task, Window};
use serde_json::json;

use super::Shell;
use super::sessions::summary;
use crate::agent::AgentKind;
use crate::agent::{Setting, Thread, ThreadEvent};
use crate::cloud;
use crate::island::AlertKind;
use crate::model::{Environment, TabKind};
use crate::phone::notify::{self, Kind, Notifier, Presence};
use crate::phone::relay::{self, RelayClient};
use crate::phone::{
    self, Access, ClientId, ClientMsg, Incoming, Reach, Server, Transport, push, tailscale, wire,
};
use crate::phone_view::{
    AccountComputer, Address, BetaState, BetaStatus, ComputerList, DeviceInfo, PairingInfo,
    PhoneEvent, PhoneState, RelayAccount, RelayInfo, SignedInDevice, SignedInList,
};
use crate::state;

/// Entry changes wait this long for more before they are sent.
const GATHER: Duration = Duration::from_millis(90);
/// The tick: counts the pairing code down, and compares the inbox with what phones
/// last saw as a safety net (changes normally reach them through the flush). It
/// hashes what the inbox shows and builds it only when that moved.
const INBOX_EVERY: Duration = Duration::from_secs(1);
/// How often Tailscale is asked again while something is in the way: quickly at
/// first, while the user is probably fixing it, then slowly, in case it stays so
/// for days; and once the way is open, in case it closes.
const TAILSCALE_RETRY: Duration = Duration::from_secs(3);
const TAILSCALE_RETRY_LATER: Duration = Duration::from_secs(15);
const TAILSCALE_QUICK_TRIES: u32 = 20;
const TAILSCALE_RECHECK: Duration = Duration::from_secs(60);

pub(super) struct PhoneLink {
    server: Transport,
    reach: Reach,
    /// The account signed in when the relay client was started, by GitHub
    /// id and session: another one needs a new client.
    account: Option<(u64, String)>,
    /// Why the relay was started signed out though there is an account.
    relay_problem: Option<String>,
    /// Signs the push notifications this computer sends.
    vapid: Arc<push::Vapid>,
    /// Phone notices waiting for the person to be away.
    notifier: Notifier,
    /// Checks the waiting notices while there are any.
    notice_timer: Option<Task<()>>,
    /// Where phones find this computer through Tailscale, as last worked out.
    remote: Address,
    _remote: Option<Task<()>>,
    /// The paired devices and the pairing code, shared with the server's threads.
    access: phone::Shared,
    /// The session each phone is looking at.
    watching: HashMap<ClientId, SharedString>,
    /// Phones whose page left the screen: they still get updates, but a push too.
    hidden: HashSet<ClientId>,
    /// Sessions whose entries changed since the last flush, with the lowest index that did.
    dirty: HashMap<SharedString, usize>,
    /// Sessions whose status, permission or usage changed since the last flush.
    state_dirty: Vec<SharedString>,
    /// Something that shows in the inbox changed since the last flush.
    inbox_dirty: bool,
    flush: Option<Task<()>>,
    last_inbox: String,
    /// What [`Shell::phone_inbox_stamp`] gave when the inbox was last built.
    last_inbox_stamp: Option<u64>,
    /// The plan limits phones last got, so a refresh that changed nothing stays quiet.
    last_usage: String,
    /// Photos phones are sending, in pieces.
    uploads: phone::Uploads,
    /// The saved transcript a device read last, for the pages after the first.
    last_read: crate::agent::saved::LastRead,
    /// When the account was last asked for its signed-in devices by the
    /// timer, and the ask under way.
    pruned_at: Option<Instant>,
    pruning: Option<Task<()>>,
    _listen: Task<()>,
    _tick: Task<()>,
}

/// The phone's private keys on their way from the keyring. It may wait for
/// the person to unlock it, so it is read on background threads.
#[derive(Default)]
pub(super) struct PhoneKeys {
    /// The keys of the link `start_phone` is starting.
    starting: Option<Task<()>>,
    /// The relay identity, made when the relay is chosen.
    making: Option<Task<()>>,
    /// A new relay identity in place of the old one.
    renewing: Option<Task<()>>,
}

/// The account's signed-in devices and computers, as last asked.
#[derive(Default)]
pub(super) struct SignedIn {
    /// The session they were asked with: an answer for another is dropped.
    asked: Option<String>,
    list: Option<Vec<cloud::SessionRow>>,
    error: Option<String>,
    machines: Option<Vec<cloud::Machine>>,
    machines_error: Option<String>,
    /// This computer's label on the relay, read when the lists are asked:
    /// reading it on every view update would parse `phone.json` and the
    /// key each time.
    own_label: Option<String>,
    _task: Option<Task<()>>,
    /// The phone app betas, asked with the session in `beta_asked` whatever
    /// the way in: signing up needs only the account.
    beta_asked: Option<String>,
    beta: Option<cloud::BetaSignups>,
    beta_error: Option<String>,
    /// The sign-up on its way, and why the last one failed, by platform.
    beta_joining: Option<cloud::BetaPlatform>,
    beta_join_error: Option<(cloud::BetaPlatform, String)>,
    _beta_task: Option<Task<()>>,
}

/// Whether the relay turned down the sign-in it was given, which the rest
/// of the app still takes for good: the account is to be asked again.
/// Started without a sign-in, the relay client says the same and means
/// only that it waits for one.
fn relay_ended_sign_in(status: &relay::Status, given: bool) -> bool {
    given && *status == relay::Status::SignedOut
}

/// The account's computers that run the app, for the Phone tab: servers
/// are the Computers tab's. This computer is marked, and not removed here.
fn your_computers(machines: &[cloud::Machine], own_label: Option<&str>) -> Vec<AccountComputer> {
    machines
        .iter()
        .filter(|m| m.kind == "app")
        .map(|m| AccountComputer {
            label: m.label.clone(),
            name: m.name.clone(),
            online: m.online,
            last_seen_at: m.last_seen_at,
            this: Some(m.label.as_str()) == own_label,
        })
        .collect()
}

/// What the Phone tab says when the old key could not be removed from the
/// account before a new one replaced it.
fn old_key_still_listed(e: &cloud::Error) -> String {
    format!(
        "The new key is in place, but this computer's old entry is still listed in your account ({e}). Remove it under Your computers."
    )
}

/// The settings a phone can pick: the model, then the reasoning (its choices depend on the model).
const PHONE_SETTINGS: [Setting; 2] = [Setting::Model, Setting::Effort];

impl Shell {
    /// The `options` answer for a phone: what a new `agent` session in the
    /// environment at `env_path` starts on, and its choices.
    pub(super) fn phone_options(&self, env_path: &Path, agent: AgentKind) -> String {
        let list = PHONE_SETTINGS
            .into_iter()
            .filter_map(|s| Some((s, self.draft_choices(env_path, agent, s)?)))
            .collect();
        json!({
            "type": "options", "env": env_path.to_string_lossy(), "agent": agent.id(),
            "settings": wire::settings(list),
        })
        .to_string()
    }

    /// Whether a phone is looking at this session, so it stays live like an open tab.
    pub(super) fn phone_watching(&self, env_path: &Path, id: &str) -> bool {
        let pid = self.phone_id(env_path, id);
        self.phone
            .as_ref()
            .is_some_and(|link| link.watching.values().any(|w| *w == pid))
    }

    /// What phones call the session `id` of the environment at `env_path`. A
    /// server's carries the server's id: two servers may give one the same.
    pub(super) fn phone_id(&self, env_path: &Path, id: &str) -> SharedString {
        match self.find_env(env_path).and_then(|e| e.remote.as_ref()) {
            Some(r) => super::servers::phone_session_id(&r.server, id).into(),
            None => SharedString::from(id.to_string()),
        }
    }

    /// What phones call each session of `env`. Taken before the sessions go,
    /// since the id of a server's session depends on its environment.
    pub(super) fn phone_ids_of(env: &Environment) -> Vec<String> {
        env.sessions
            .iter()
            .map(|s| match env.remote.as_ref() {
                Some(r) => super::servers::phone_session_id(&r.server, &s.id),
                None => s.id.to_string(),
            })
            .collect()
    }

    /// A session took a new id: phones that muted it keep it muted.
    pub(super) fn rename_phone_session(&mut self, old: &str, new: &str) {
        let Some(link) = &self.phone else {
            return;
        };
        let changed = link.access.lock().unwrap().rename_muted(old, new);
        if changed {
            self.save_devices();
        }
    }

    /// Sessions are deleted: no phone keeps them muted.
    pub(super) fn forget_phone_sessions(&mut self, pids: &[String]) {
        let Some(link) = &self.phone else {
            return;
        };
        let changed = {
            let mut access = link.access.lock().unwrap();
            pids.iter().fold(false, |c, p| access.forget_session(p) | c)
        };
        if changed {
            self.save_devices();
        }
    }

    /// The environment and the session id there of what phones call `pid`.
    fn phone_session(&self, pid: &str) -> Option<(PathBuf, SharedString)> {
        let (id, server) = super::servers::split_phone_id(pid);
        self.projects
            .iter()
            .flat_map(|p| &p.environments)
            .find(|e| {
                e.remote.as_ref().map(|r| r.server.as_str()) == server
                    && e.session_index(id).is_some()
            })
            .map(|e| (e.path.clone(), SharedString::from(id.to_string())))
    }

    /// What a session's entries are read against when they go to a phone.
    fn phone_scope(&self, env_path: &Path, cwd: &Path) -> wire::Scope {
        // A server's session names files on the server: one of the same name
        // here is not its to show.
        let remote = self.find_env(env_path).is_some_and(|e| e.remote.is_some());
        let key = self
            .phone
            .as_ref()
            .filter(|_| !remote)
            .map(|l| l.access.lock().unwrap().image_key.clone());
        wire::Scope {
            cwd: cwd.to_path_buf(),
            key,
        }
    }

    /// The sessions changed in a way the inbox shows; phones hear soon.
    pub(super) fn phone_inbox_changed(&mut self, cx: &mut Context<Self>) {
        let Some(link) = &mut self.phone else {
            return;
        };
        if link.server.client_count() == 0 {
            return;
        }
        link.inbox_dirty = true;
        self.phone_schedule_flush(cx);
    }

    fn phone_schedule_flush(&mut self, cx: &mut Context<Self>) {
        let Some(link) = &mut self.phone else {
            return;
        };
        if link.flush.is_none() {
            link.flush = Some(cx.spawn(async move |this, cx| {
                cx.background_executor().timer(GATHER).await;
                let _ = this.update(cx, |this, cx| this.phone_flush(cx));
            }));
        }
    }

    /// Opens (or focuses) the Phone tab of the visible environment.
    pub(super) fn open_phone_tab(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.menu = None;
        if !self.has_projects() {
            self.pick_project_for("The Phone tab", Self::open_phone_tab, window, cx);
            return;
        }
        let strip = &mut self.strip;
        let index = match strip
            .tabs
            .iter()
            .position(|t| matches!(t.kind, TabKind::Phone))
        {
            Some(i) => i,
            None => strip.add(crate::model::Tab::new("Phone", TabKind::Phone)),
        };
        strip.active_tab = index;
        self.note_active_tab(cx);
        self.list_signed_in(window, cx);
        self.list_beta(window, cx);
        self.sync_phone_view(cx);
        self.views.phone.update(cx, |v, cx| v.focus(window, cx));
        cx.notify();
    }

    /// The sign-in changed (in, out, or to another account): the relay comes
    /// back with the new one, or signed out, and what shows of the account
    /// is asked again.
    pub(super) fn account_changed(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.servers_account_changed(window, cx);
        self.account_notices(cx);
        let account = self
            .teams
            .read(cx)
            .account()
            .map(|a| (a.github_id, a.session));
        self.relay_owner_changed(cx);
        if let Some(link) = &self.phone
            && link.reach == Reach::Relay
            && link.account != account
        {
            self.save_devices();
            self.restart_phone(&phone::Settings::load(), window, cx);
        }
        let session = account.map(|(_, s)| s);
        if self.signed_in.asked != session || self.signed_in.beta_asked != session {
            self.signed_in = SignedIn::default();
            self.list_signed_in(window, cx);
            self.list_beta(window, cx);
        }
        self.sync_phone_view(cx);
    }

    /// Asks the account for its signed-in devices, when the relay is the way
    /// in and there is an account to ask.
    fn list_signed_in(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if phone::Settings::load().reach != Reach::Relay {
            return;
        }
        let Some(account) = self.teams.read(cx).account() else {
            return;
        };
        let session = account.session.clone();
        self.signed_in.asked = Some(session.clone());
        self.signed_in.own_label = phone::Settings::load().relay_label();
        let asked_at = crate::model::unix_now();
        self.signed_in._task = Some(cx.spawn_in(window, async move |this, cx| {
            let base = account.base;
            let token = account.session;
            let (listed, machines) = cx
                .background_spawn(async move {
                    // The fixture's session is made up: the service would
                    // answer 401 and sign it out.
                    if crate::arakun_teams::fixture().is_some() {
                        let me = cloud::SessionRow {
                            id: "fixture".into(),
                            name: crate::arakun_teams::hub::device_name(),
                            kind: "app".into(),
                            created_at: 0,
                            last_used_at: crate::model::unix_now(),
                            current: true,
                        };
                        return (Ok(vec![me]), Ok(super::servers::fixture_machines()));
                    }
                    (
                        cloud::sessions(&base, &token),
                        cloud::machines(&base, &token),
                    )
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                if this.signed_in.asked.as_deref() != Some(session.as_str()) {
                    return;
                }
                this.signed_in._task = None;
                match listed {
                    Ok(list) => {
                        let ids: Vec<String> = list.iter().map(|s| s.id.clone()).collect();
                        this.forget_relay_devices(Some(phone::SignedInDevices {
                            ids: &ids,
                            asked_at,
                        }));
                        this.signed_in.list = Some(list);
                        this.signed_in.error = None;
                    }
                    Err(e) => this.account_call_failed(e, cx),
                }
                match machines {
                    Ok(list) => {
                        this.signed_in.machines = Some(list);
                        this.signed_in.machines_error = None;
                    }
                    Err(cloud::Error::SignedOut) => {}
                    Err(e) => this.signed_in.machines_error = Some(e.to_string()),
                }
                this.sync_phone_view(cx);
            });
        }));
    }

    /// Asks the account where it stands with the phone app betas.
    fn list_beta(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(account) = self.teams.read(cx).account() else {
            return;
        };
        let session = account.session.clone();
        self.signed_in.beta_asked = Some(session.clone());
        self.signed_in._beta_task = Some(cx.spawn_in(window, async move |this, cx| {
            let (base, token) = (account.base, account.session);
            let answer = cx
                .background_spawn(async move {
                    // The fixture's session is made up: the service would
                    // answer 401 and sign it out.
                    if crate::arakun_teams::fixture().is_some() {
                        let none = cloud::BetaSignup {
                            status: "none".into(),
                            email: None,
                            retry_at: None,
                        };
                        return Ok(cloud::BetaSignups {
                            email: Some("you@example.com".into()),
                            ios: none.clone(),
                            android: none,
                        });
                    }
                    cloud::beta(&base, &token)
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                if this.signed_in.beta_asked.as_deref() != Some(session.as_str()) {
                    return;
                }
                this.signed_in._beta_task = None;
                match answer {
                    Ok(b) => {
                        this.signed_in.beta = Some(b);
                        this.signed_in.beta_error = None;
                    }
                    Err(cloud::Error::SignedOut) => {
                        this.teams.update(cx, |t, cx| t.session_ended(cx));
                    }
                    Err(e) => this.signed_in.beta_error = Some(e.to_string()),
                }
                this.sync_phone_view(cx);
                // The rail's beta card reads the same answer.
                this.fill_beta_card_email(cx);
                cx.notify();
            });
        }));
    }

    /// Signs the account up for one beta; the operator admits by hand.
    pub(super) fn join_beta(
        &mut self,
        platform: cloud::BetaPlatform,
        email: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(account) = self.teams.read(cx).account() else {
            return;
        };
        if self.signed_in.beta_joining.is_some() {
            return;
        }
        self.signed_in.beta_joining = Some(platform);
        self.signed_in.beta_join_error = None;
        self.sync_phone_view(cx);
        cx.notify();
        let session = account.session.clone();
        cx.spawn_in(window, async move |this, cx| {
            let (base, token) = (account.base, account.session);
            let done = cx
                .background_spawn(async move {
                    if crate::arakun_teams::fixture().is_some() {
                        return Ok(cloud::BetaSignup {
                            status: "pending".into(),
                            email: Some(email),
                            retry_at: None,
                        });
                    }
                    cloud::join_beta(&base, &token, platform, &email)
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                this.signed_in.beta_joining = None;
                if this.signed_in.beta_asked.as_deref() != Some(session.as_str()) {
                    return;
                }
                match done {
                    Ok(signup) => {
                        if let Some(b) = &mut this.signed_in.beta {
                            match platform {
                                cloud::BetaPlatform::Ios => b.ios = signup,
                                cloud::BetaPlatform::Android => b.android = signup,
                            }
                        }
                    }
                    Err(cloud::Error::SignedOut) => {
                        this.teams.update(cx, |t, cx| t.session_ended(cx));
                    }
                    Err(e) => this.signed_in.beta_join_error = Some((platform, e.to_string())),
                }
                this.sync_phone_view(cx);
                cx.notify();
            });
        })
        .detach();
    }

    /// What the Phone tab says of the phone app betas, and the account's
    /// email to offer.
    pub(super) fn beta_state(&self, cx: &App) -> (BetaState, Option<String>) {
        let teams = self.teams.read(cx);
        if teams.account().is_none() {
            // A session the service has not answered about yet is not a sign-out.
            let checking = !teams.ready() || teams.session().is_some();
            return (
                if checking {
                    BetaState::Loading
                } else {
                    BetaState::SignedOut
                },
                None,
            );
        }
        let s = &self.signed_in;
        let Some(b) = &s.beta else {
            return match &s.beta_error {
                Some(e) => (BetaState::Failed(e.clone()), None),
                None => (BetaState::Loading, None),
            };
        };
        let status = |platform, signup: &cloud::BetaSignup| BetaStatus {
            status: signup.status.clone(),
            email: signup.email.clone(),
            retry_at: signup.retry_at,
            busy: s.beta_joining == Some(platform),
            error: s
                .beta_join_error
                .as_ref()
                .filter(|(p, _)| *p == platform)
                .map(|(_, e)| e.clone()),
        };
        let state = BetaState::Ready {
            ios: status(cloud::BetaPlatform::Ios, &b.ios),
            android: status(cloud::BetaPlatform::Android, &b.android),
        };
        (state, b.email.clone())
    }

    /// Removes one of the account's computers; the lists are asked again after.
    fn remove_computer(&mut self, label: String, window: &mut Window, cx: &mut Context<Self>) {
        // The fixture's session is made up: the service would sign it out.
        let Some(account) = self
            .teams
            .read(cx)
            .account()
            .filter(|_| crate::arakun_teams::fixture().is_none())
        else {
            return;
        };
        cx.spawn_in(window, async move |this, cx| {
            let (base, token) = (account.base, account.session);
            let done = cx
                .background_spawn(async move { cloud::forget_machine(&base, &token, &label) })
                .await;
            let _ = this.update_in(cx, |this, window, cx| {
                // A notice, not the tab's error line: the lists asked again
                // below would clear that at once.
                match done {
                    Ok(()) => {}
                    Err(cloud::Error::SignedOut) => {
                        this.account_call_failed(cloud::Error::SignedOut, cx)
                    }
                    Err(e) => this.notify(
                        AlertKind::Warning,
                        "Could not remove the computer",
                        &e.to_string(),
                        None,
                        cx,
                    ),
                }
                this.list_signed_in(window, cx);
                this.servers_list_machines(window, cx);
                this.sync_phone_view(cx);
            });
        })
        .detach();
    }

    /// A new relay key. Signed in, the old label is removed from the account
    /// first, so the account does not keep listing a computer that is gone;
    /// if that fails the key is replaced all the same, and the tab says the
    /// old one is still listed.
    fn new_identity(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let was_on = self.phone_on();
        // Stopped first: the old key must not connect, and so register, again.
        self.stop_phone(cx);
        let old = phone::Settings::load().relay_label();
        let account = self
            .teams
            .read(cx)
            .account()
            .filter(|_| crate::arakun_teams::fixture().is_none());
        let (Some(label), Some(account)) = (old, account) else {
            self.renew_identity(was_on, window, cx);
            return;
        };
        cx.spawn_in(window, async move |this, cx| {
            let (base, token) = (account.base, account.session);
            let done = cx
                .background_spawn(async move { cloud::forget_machine(&base, &token, &label) })
                .await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.renew_identity(was_on, window, cx);
                if let Err(e) = done {
                    this.notify(
                        AlertKind::Warning,
                        "Old computer still listed",
                        &old_key_still_listed(&e),
                        None,
                        cx,
                    );
                    if e == cloud::Error::SignedOut {
                        this.account_call_failed(e, cx);
                    }
                }
                this.list_signed_in(window, cx);
            });
        })
        .detach();
    }

    /// Puts a new relay key in the keyring on a background thread: the
    /// keyring may wait for the person to unlock it. When it does not take
    /// the key, the old one stays, and the tab says so.
    fn renew_identity(&mut self, was_on: bool, window: &mut Window, cx: &mut Context<Self>) {
        let renew = cx.background_spawn(async move {
            let mut settings = phone::Settings::load();
            settings.renew_identity()?;
            settings.save_soon();
            Ok::<_, String>(settings)
        });
        self.phone_keys.renewing = Some(cx.spawn_in(window, async move |this, cx| {
            let renewed = renew.await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.phone_keys.renewing = None;
                let settings = renewed.unwrap_or_else(|e| {
                    this.notify(AlertKind::Warning, "Relay key not changed", &e, None, cx);
                    phone::Settings::load()
                });
                this.signed_in.own_label = settings.relay_label();
                this.servers_own_label_changed();
                // Turned on while the key was made, the link has the old key.
                if was_on || this.phone_on() {
                    this.restart_phone(&settings, window, cx);
                } else {
                    this.sync_phone_view(cx);
                }
            });
        }));
    }

    /// Whether the phone link is on, or its keys are on their way to start it.
    fn phone_on(&self) -> bool {
        self.phone.is_some() || self.phone_keys.starting.is_some()
    }

    /// Makes the identity the first time the relay is chosen, on a background
    /// thread: the keyring may wait for the person to unlock it. Starting
    /// the phone link makes it too, as phones check every handshake with it.
    fn make_identity(&mut self, cx: &mut Context<Self>) {
        if self.phone_keys.making.is_some() {
            return;
        }
        let make = cx.background_spawn(async { phone::Settings::load_keys().map(drop) });
        self.phone_keys.making = Some(cx.spawn(async move |this, cx| {
            let made = make.await;
            let _ = this.update(cx, |this, cx| {
                // After an error `making` stays set: a keyring that said no
                // is not asked again on every view update.
                if let Err(e) = made {
                    eprintln!("arakun: {e}");
                    return;
                }
                this.phone_keys.making = None;
                this.signed_in.own_label = phone::Settings::load().relay_label();
                this.servers_own_label_changed();
                this.sync_phone_view(cx);
            });
        }));
    }

    /// The devices the relay let in belong to the account signed in then:
    /// under another one, or none, they all go (`phone::relay_owner`).
    fn relay_owner_changed(&mut self, cx: &mut Context<Self>) {
        // The made-up account of a test run says nothing about real devices.
        if crate::arakun_teams::fixture().is_some() {
            return;
        }
        let teams = self.teams.read(cx);
        let now = match teams.account() {
            Some(a) => phone::AccountNow::SignedIn(a.github_id),
            None if teams.ready() && teams.session().is_none() => phone::AccountNow::SignedOut,
            None => phone::AccountNow::Unknown,
        };
        // Told on every change of Teams, pictures included: the same
        // account as last time changes nothing, so the file is not read.
        if self.relay_owner_seen == Some(now) {
            return;
        }
        self.relay_owner_seen = Some(now);
        let mut settings = phone::Settings::load();
        let (drop_all, kept) = phone::relay_owner(settings.relay_account, now);
        if kept != settings.relay_account {
            settings.relay_account = kept;
            settings.save_soon();
        }
        if drop_all {
            self.forget_relay_devices(None);
        }
    }

    /// While a relay device gets pushes, the account is asked every so
    /// often which devices are still signed in (`phone::relay_prune_due`),
    /// so one signed out while not connected stops getting this computer's
    /// pushes even with the Phone tab closed. A failed ask drops nothing.
    fn relay_prune_tick(&mut self, cx: &mut Context<Self>) {
        if crate::arakun_teams::fixture().is_some() {
            return;
        }
        let account = self.teams.read(cx).account();
        let Some(link) = &mut self.phone else {
            return;
        };
        let now = Instant::now();
        let due = phone::relay_prune_due(
            account.is_some(),
            &link.access.lock().unwrap().devices,
            link.pruned_at,
            now,
        );
        let (true, None, Some(account)) = (due, &link.pruning, account) else {
            return;
        };
        link.pruned_at = Some(now);
        let asked = account.session.clone();
        let asked_at = crate::model::unix_now();
        link.pruning = Some(cx.spawn(async move |this, cx| {
            let (base, token) = (account.base, account.session);
            let listed = cx
                .background_spawn(async move { cloud::sessions(&base, &token) })
                .await;
            let _ = this.update(cx, |this, cx| {
                if let Some(link) = &mut this.phone {
                    link.pruning = None;
                }
                // Another sign-in since: its devices are another list.
                if this.teams.read(cx).account().map(|a| a.session) != Some(asked) {
                    return;
                }
                match listed {
                    Ok(rows) => {
                        let ids: Vec<String> = rows.into_iter().map(|r| r.id).collect();
                        this.forget_relay_devices(Some(phone::SignedInDevices {
                            ids: &ids,
                            asked_at,
                        }));
                        this.sync_phone_view(cx);
                    }
                    Err(cloud::Error::SignedOut) => {
                        this.teams.update(cx, |t, cx| t.session_ended(cx));
                    }
                    Err(_) => {}
                }
            });
        }));
    }

    /// Drops the devices the relay let in that the account no longer has
    /// (`phone::prune_relay_devices`), here and in what is saved: a device
    /// signed out elsewhere must not keep getting this computer's pushes.
    fn forget_relay_devices(&mut self, listed: Option<phone::SignedInDevices<'_>>) {
        // The made-up account lists made-up ids: real devices would all go.
        if crate::arakun_teams::fixture().is_some() {
            return;
        }
        if let Some(link) = &self.phone {
            let dropped =
                phone::prune_relay_devices(&mut link.access.lock().unwrap().devices, listed);
            for id in &dropped {
                link.server.disconnect_device(id);
            }
            if !dropped.is_empty() {
                self.save_devices();
            }
            return;
        }
        let mut settings = phone::Settings::load();
        if !phone::prune_relay_devices(&mut settings.devices, listed).is_empty() {
            settings.save_soon();
        }
    }

    /// A call to the account failed. A sign-in the service no longer knows
    /// is forgotten here, and the prompt to sign in shows.
    pub(super) fn account_call_failed(&mut self, e: cloud::Error, cx: &mut Context<Self>) {
        if e == cloud::Error::SignedOut {
            self.teams.update(cx, |t, cx| t.session_ended(cx));
        } else {
            self.signed_in.error = Some(e.to_string());
        }
    }

    /// Signs one of the account's devices out; the list is asked again after.
    fn sign_out_device(&mut self, id: String, window: &mut Window, cx: &mut Context<Self>) {
        let Some(account) = self.teams.read(cx).account() else {
            return;
        };
        cx.spawn_in(window, async move |this, cx| {
            let (base, token) = (account.base, account.session);
            let done = cx
                .background_spawn(async move { cloud::sign_out_session(&base, &token, &id) })
                .await;
            let _ = this.update_in(cx, |this, window, cx| {
                if let Err(e) = done {
                    this.account_call_failed(e, cx);
                }
                this.list_signed_in(window, cx);
                this.sync_phone_view(cx);
            });
        })
        .detach();
    }

    /// What the Phone tab says of the account the relay lets devices in by.
    fn relay_account(&self, cx: &App) -> RelayAccount {
        let teams = self.teams.read(cx);
        if let Some(account) = teams.account() {
            let devices = match (&self.signed_in.error, &self.signed_in.list) {
                (Some(e), _) => SignedInList::Failed(e.clone()),
                (None, Some(list)) => SignedInList::Listed(
                    list.iter()
                        .map(|s| SignedInDevice {
                            id: s.id.clone(),
                            name: s.name.clone(),
                            kind: s.kind.clone(),
                            last_used_at: s.last_used_at,
                            current: s.current,
                        })
                        .collect(),
                ),
                (None, None) => SignedInList::Loading,
            };
            let computers = match (&self.signed_in.machines_error, &self.signed_in.machines) {
                (Some(e), _) => ComputerList::Failed(e.clone()),
                (None, Some(list)) => {
                    ComputerList::Listed(your_computers(list, self.signed_in.own_label.as_deref()))
                }
                (None, None) => ComputerList::Loading,
            };
            return RelayAccount::SignedIn {
                login: account.login,
                devices,
                computers,
            };
        }
        // A session the service has not answered about yet is not a sign-out.
        if !teams.ready() || teams.session().is_some() {
            RelayAccount::Checking
        } else {
            RelayAccount::SignedOut
        }
    }

    pub(super) fn on_phone_event(
        &mut self,
        ev: &PhoneEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match ev {
            PhoneEvent::Enable => {
                let mut settings = phone::Settings::load();
                settings.enabled = true;
                settings.save_soon();
                self.start_phone(&settings, None, window, cx);
            }
            PhoneEvent::Disable => {
                self.save_devices();
                let mut settings = phone::Settings::load();
                settings.enabled = false;
                settings.save_soon();
                self.stop_phone(cx);
                self.sync_phone_view(cx);
            }
            PhoneEvent::SetReach(reach) => {
                self.save_devices();
                let mut settings = phone::Settings::load();
                if settings.reach == *reach {
                    return;
                }
                settings.reach = *reach;
                settings.save_soon();
                self.list_signed_in(window, cx);
                if self.phone_on() {
                    self.restart_phone(&settings, window, cx);
                } else {
                    self.sync_phone_view(cx);
                }
            }
            PhoneEvent::Pair => {
                // The relay lets in the account's devices: pairing is the network's.
                if let Some(link) = &self.phone
                    && link.reach != Reach::Relay
                {
                    link.access.lock().unwrap().begin_pairing();
                }
                self.sync_phone_view(cx);
            }
            PhoneEvent::CancelPairing => {
                if let Some(link) = &self.phone {
                    link.access.lock().unwrap().cancel_pairing();
                }
                self.sync_phone_view(cx);
            }
            PhoneEvent::Unpair(id) => {
                if let Some(link) = &self.phone {
                    link.access.lock().unwrap().unpair(id);
                    link.server.disconnect_device(id);
                }
                self.save_devices();
                self.sync_phone_view(cx);
            }
            PhoneEvent::NewIdentity => self.new_identity(window, cx),
            PhoneEvent::SignIn => self.open_account_sign_in(window, cx),
            PhoneEvent::SignOutDevice(id) => self.sign_out_device(id.clone(), window, cx),
            PhoneEvent::RemoveComputer(label) => self.remove_computer(label.clone(), window, cx),
            PhoneEvent::JoinBeta { platform, email } => {
                self.join_beta(*platform, email.clone(), window, cx)
            }
            PhoneEvent::OpenHere(url) => {
                if self.has_projects() {
                    let env_path = self.env().path.clone();
                    self.push_browser_tab(&env_path, url.clone(), true, true, window, cx);
                }
            }
        }
    }

    /// The relay is this computer's way in, so the account can reach it.
    pub(super) fn relay_on(&self) -> bool {
        self.phone.as_ref().is_some_and(|l| l.reach == Reach::Relay)
    }

    /// Starts the server when the last run left it on.
    pub(super) fn restore_phone(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let settings = phone::Settings::load();
        if settings.enabled {
            self.start_phone(&settings, None, window, cx);
        }
    }

    /// Writes the devices as the server now knows them (paired, seen, unpaired).
    fn save_devices(&self) {
        let Some(link) = &self.phone else {
            return;
        };
        let mut settings = phone::Settings::load();
        settings.devices = link.access.lock().unwrap().devices.clone();
        settings.save_soon();
    }

    /// Closes the server and every phone; gives Tailscale's HTTPS port back if it was ours.
    fn stop_phone(&mut self, cx: &mut Context<Self>) {
        // A link still waiting for its keys does not start.
        self.phone_keys.starting = None;
        let Some(link) = self.phone.take() else {
            return;
        };
        self.push_linear_host(cx);
        // A phone of the next link may come with the same id.
        self.servers_phone_left(None);
        if link.reach == Reach::Tailscale {
            cx.background_executor()
                .spawn(async {
                    let _ = tailscale::stop_serving();
                })
                .detach();
        }
    }

    /// Starts the phone link again with `settings`. Notices waiting for the
    /// person to leave carry over to the new link.
    fn restart_phone(
        &mut self,
        settings: &phone::Settings,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let waiting = self
            .phone
            .as_mut()
            .map(|link| std::mem::take(&mut link.notifier));
        self.stop_phone(cx);
        self.start_phone(settings, waiting, window, cx);
    }

    /// Starts the phone link with `settings` once its private keys are read.
    /// They come from the keyring, which may wait up to minutes for the
    /// person to unlock it, so they are read on a background thread and
    /// never on this one. `waiting` are the notices a link before this one
    /// still had for when the person leaves.
    fn start_phone(
        &mut self,
        settings: &phone::Settings,
        waiting: Option<Notifier>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let mut settings = settings.clone();
        let keys = cx.background_spawn(async move { phone::Settings::load_keys() });
        self.phone_keys.starting = Some(cx.spawn_in(window, async move |this, cx| {
            let keys = keys.await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.phone_keys.starting = None;
                match keys {
                    Ok(keys) => {
                        // Devices may have come or gone while the keys were read.
                        let now = phone::Settings::load();
                        // The keys may have made the identity just now.
                        let label = now.relay_label();
                        settings.devices = now.devices;
                        if this.signed_in.own_label != label {
                            this.signed_in.own_label = label;
                            this.servers_own_label_changed();
                        }
                        this.start_phone_with(&settings, keys, window, cx);
                        if let (Some(waiting), Some(link)) = (waiting, &mut this.phone) {
                            link.notifier = waiting;
                            this.time_notices(window, cx);
                        }
                    }
                    Err(e) => this.views.phone.update(cx, |v, cx| v.set_error(e, cx)),
                }
            });
        }));
    }

    fn start_phone_with(
        &mut self,
        settings: &phone::Settings,
        keys: phone::Keys,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let vapid = Arc::new(keys.vapid);
        let access = Access::new(settings.devices.clone(), vapid.public_key());
        let teams_account = (settings.reach == Reach::Relay)
            .then(|| self.teams.read(cx).account())
            .flatten();
        let signed_in = teams_account
            .as_ref()
            .map(|a| (a.github_id, a.session.clone()));
        let (relay_account, relay_problem) = relay_sign_in(
            &settings.relay_base,
            teams_account.as_ref(),
            crate::arakun_teams::fixture().is_some(),
        );
        let identity = keys.identity;
        let (server, mut incoming) = match settings.reach {
            Reach::Lan | Reach::Tailscale => {
                let identity = Arc::new(identity);
                match Server::start(
                    settings.reach.bind(),
                    settings.port,
                    access.clone(),
                    identity,
                ) {
                    Ok((s, rx)) => (Transport::Local(s), rx),
                    Err(e) => {
                        self.views.phone.update(cx, |v, cx| {
                            v.set_error(
                                format!("Could not listen on port {}: {e}", settings.port),
                                cx,
                            )
                        });
                        return;
                    }
                }
            }
            Reach::Relay => {
                // The window's sign-in; without one the client waits signed
                // out, and a sign-in starts a new one (`account_changed`).
                let machine = relay::Machine {
                    name: phone::host_name(),
                    kind: relay::MachineKind::App,
                };
                let account = relay_account;
                let (r, rx) = RelayClient::start(
                    &settings.relay_base,
                    identity,
                    access.clone(),
                    machine,
                    account,
                );
                (Transport::Relay(r), rx)
            }
        };
        let watch = match (settings.reach, server.port()) {
            (Reach::Tailscale, Some(port)) => Some(self.watch_tailscale(port, cx)),
            _ => None,
        };
        let listen = cx.spawn_in(window, async move |this, cx| {
            while let Some(msg) = incoming.next().await {
                if this
                    .update_in(cx, |this, window, cx| this.phone_incoming(msg, window, cx))
                    .is_err()
                {
                    break;
                }
            }
        });
        let tick = crate::poll::poll_every(cx, INBOX_EVERY, INBOX_EVERY, |this, cx| {
            if let Some(link) = &this.phone {
                link.last_read.trim();
            }
            this.relay_prune_tick(cx);
            this.phone_sync_inbox(cx)
        });
        let remote = match &server {
            Transport::Relay(client) => relay_address(relay::Status::Connecting, client.page_url()),
            Transport::Local(_) => Address::Pending("Asking Tailscale…".into()),
        };
        self.phone = Some(PhoneLink {
            server,
            reach: settings.reach,
            account: signed_in,
            relay_problem,
            vapid,
            notifier: Notifier::default(),
            notice_timer: None,
            remote,
            _remote: watch,
            access,
            watching: HashMap::new(),
            hidden: HashSet::new(),
            dirty: HashMap::new(),
            state_dirty: Vec::new(),
            inbox_dirty: false,
            flush: None,
            last_inbox: String::new(),
            last_inbox_stamp: None,
            last_usage: String::new(),
            uploads: phone::Uploads::default(),
            last_read: Default::default(),
            pruned_at: None,
            pruning: None,
            _listen: listen,
            _tick: tick,
        });
        self.push_linear_host(cx);
        self.sync_phone_view(cx);
    }

    /// Keeps asking Tailscale where phones can find us, and keeps `serve` pointed at
    /// the server while it can. Each answer goes to the Phone tab as it comes.
    fn watch_tailscale(&mut self, port: u16, cx: &mut Context<Self>) -> Task<()> {
        cx.spawn(async move |this, cx| {
            let mut tries = 0u32;
            loop {
                let found = cx
                    .background_executor()
                    .spawn(async move { tailscale_address(port) })
                    .await;
                let ready = matches!(found, Address::Ready(_));
                tries = if ready { 0 } else { tries + 1 };
                let alive = this.update(cx, |this, cx| {
                    let changed = match &mut this.phone {
                        Some(link) if link.remote != found => {
                            link.remote = found;
                            true
                        }
                        _ => false,
                    };
                    if changed {
                        this.sync_phone_view(cx);
                    }
                });
                if alive.is_err() {
                    break;
                }
                let wait = match (ready, tries) {
                    (true, _) => TAILSCALE_RECHECK,
                    (false, n) if n < TAILSCALE_QUICK_TRIES => TAILSCALE_RETRY,
                    _ => TAILSCALE_RETRY_LATER,
                };
                cx.background_executor().timer(wait).await;
            }
        })
    }

    /// Tells the Phone tab what to show: the address, the code while pairing, the devices.
    pub(super) fn sync_phone_view(&mut self, cx: &mut Context<Self>) {
        let settings = phone::Settings::load();
        let reach = self.phone.as_ref().map_or(settings.reach, |l| l.reach);
        // The identity is made the first time the relay is chosen, not before.
        if reach == Reach::Relay && settings.relay_label().is_none() {
            self.make_identity(cx);
        }
        let relay = (reach == Reach::Relay).then(|| RelayInfo {
            url: settings.relay_address().trim_end_matches('/').to_string(),
            account: self.relay_account(cx),
        });
        let state = match &self.phone {
            Some(link) => {
                let port = link.server.port().unwrap_or(phone::DEFAULT_PORT);
                let address = match link.reach {
                    Reach::Lan => match phone::lan_ip() {
                        Some(ip) => Address::Ready(phone::lan_address(ip, port)),
                        None => Address::Problem {
                            what: "Could not tell this computer's address on the network.".into(),
                            link: None,
                        },
                    },
                    Reach::Relay => match &link.relay_problem {
                        Some(why) => Address::Problem {
                            what: why.clone(),
                            link: None,
                        },
                        None => link.remote.clone(),
                    },
                    Reach::Tailscale => link.remote.clone(),
                };
                let base = match (&address, &link.server) {
                    (Address::Ready(url), _) => url.clone(),
                    // The relay address stands whether or not we are connected to it right now.
                    (_, Transport::Relay(client)) => client.page_url().to_string(),
                    _ => format!("http://<this computer>:{port}/"),
                };
                let connected = link.server.connected_devices();
                let mut access = link.access.lock().unwrap();
                // On a server of our own the phone checks the handshake
                // against the key the link names.
                let label = match &link.server {
                    Transport::Local(_) => settings.relay_label(),
                    Transport::Relay(_) => None,
                };
                let pairing = access.pairing().map(|(code, left)| PairingInfo {
                    url: phone::link(&base, &code, label.as_deref()),
                    code,
                    seconds_left: left.as_secs(),
                });
                // Paired here with a code: the ones the relay let in are
                // the account's, listed under "Signed-in devices".
                let devices = access
                    .devices
                    .iter()
                    .filter(|d| phone::Door::Local.admits(d))
                    .map(|d| DeviceInfo {
                        id: d.id.clone(),
                        name: d.name.clone(),
                        paired_at: d.paired_at,
                        last_seen: d.last_seen,
                        connected: connected.contains(&d.id),
                    })
                    .collect();
                PhoneState::On {
                    reach: link.reach,
                    relay,
                    address,
                    pairing,
                    devices,
                    phones: link.server.client_count(),
                }
            }
            None => PhoneState::Off { reach, relay },
        };
        let (beta, email) = self.beta_state(cx);
        self.views.phone.update(cx, |v, cx| {
            v.set_state(state, cx);
            v.set_beta(beta, email.as_deref(), cx);
        });
    }

    fn phone_incoming(&mut self, msg: Incoming, window: &mut Window, cx: &mut Context<Self>) {
        match msg {
            Incoming::Connected(id, _device) => {
                let inbox = self.phone_inbox(cx);
                if let Some(link) = &mut self.phone {
                    link.server.send(id, inbox.clone());
                    link.last_inbox = inbox;
                }
                // The device's last_seen moved.
                self.save_devices();
                self.sync_phone_view(cx);
            }
            Incoming::Disconnected(id) => {
                self.phone_stop_watching(id, cx);
                self.servers_phone_left(Some(id));
                if let Some(link) = &mut self.phone {
                    link.hidden.remove(&id);
                }
                self.sync_phone_view(cx);
            }
            // The first time is worth a word: the account let it in alone.
            Incoming::NewDevice { name, .. } => {
                self.notify(
                    AlertKind::Info,
                    &format!("{name} connected for the first time"),
                    "",
                    None,
                    cx,
                );
                self.save_devices();
                self.sync_phone_view(cx);
            }
            // The device's sockets close on their own; `Disconnected` tidies the rest.
            Incoming::Paired | Incoming::Renamed | Incoming::Subscribed | Incoming::Unpaired => {
                self.save_devices();
                self.sync_phone_view(cx);
            }
            Incoming::Relay(status) => {
                // A Linear sync that failed, and reports Linear did not take,
                // while the relay was away go now.
                let connected = status == relay::Status::Connected;
                let mut ended = false;
                if let Some(link) = &mut self.phone
                    && let Transport::Relay(client) = &link.server
                {
                    let given = link.account.is_some() && link.relay_problem.is_none();
                    ended = relay_ended_sign_in(&status, given);
                    link.remote = relay_address(status, client.page_url());
                }
                // The relay ended the sign-in (signed out from another
                // device, or run out) and nothing told Teams: without this
                // the tab says "Signed in" over "Waiting for your sign-in".
                if ended {
                    self.teams.update(cx, |t, cx| t.recheck(cx));
                }
                self.sync_phone_view(cx);
                self.push_linear_host(cx);
                if connected {
                    self.sync_linear(window, cx);
                    self.linear_flush(cx);
                    // The relay label may be what the gate waited for.
                    self.linear_replay_kept(window, cx);
                    self.linear_start_waiting(window, cx);
                }
            }
            Incoming::Message(id, msg) => self.phone_message(id, msg, window, cx),
            Incoming::Linear(delivery) => self.linear_delivery(delivery, window, cx),
        }
    }

    fn phone_message(
        &mut self,
        client: ClientId,
        msg: ClientMsg,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match msg {
            ClientMsg::Open { session, .. } => {
                let Some((env_path, sid)) = self.phone_session(&session) else {
                    self.phone_send(client, wire::error("that session is gone"));
                    return;
                };
                let pid: SharedString = session.into();
                if let Some(link) = &mut self.phone {
                    link.watching.insert(client, pid.clone());
                }
                // A server's session: its server sends it here as it goes.
                if self.find_env(&env_path).is_some_and(|e| e.remote.is_some()) {
                    self.ensure_live(&env_path, &sid, window, cx);
                    let who = super::servers::Watcher::Phone(client);
                    self.servers_follow(&env_path, &sid, who, cx);
                } else {
                    // A server's session it looked at before is let go, as
                    // when it opens another of a server's.
                    self.servers_look_away(super::servers::Watcher::Phone(client));
                }
                let path = env_path.clone();
                self.phone_when_read(&env_path, &sid.clone(), cx, move |this, cx| {
                    let reply = this.phone_session_json(&path, &sid, &pid, cx);
                    this.phone_send(client, reply);
                });
            }
            ClientMsg::Close { .. } => self.phone_stop_watching(client, cx),
            ClientMsg::Entries { session, from, to } => {
                let Some((env_path, sid)) = self.phone_session(&session) else {
                    return;
                };
                let path = env_path.clone();
                self.phone_when_read(&env_path, &sid.clone(), cx, move |this, cx| {
                    let (entries, count) = this.phone_entries(&path, &sid, from, to.max(from), cx);
                    let reply = json!({ "type": "entries", "id": session, "from": from, "count": count, "entries": entries, "older": true });
                    this.phone_send(client, reply.to_string());
                });
            }
            ClientMsg::Send {
                session,
                text,
                uploads,
                now,
            } => {
                let Some((env_path, sid)) = self.phone_session(&session) else {
                    self.phone_send(client, wire::error("that session is gone"));
                    return;
                };
                // The prompt carries photos by the id they finished uploading
                // under: their files become `@` mentions in the text, the same
                // way pasted images attach on the desktop.
                let text = match &mut self.phone {
                    Some(link) => link.uploads.attach(text, &uploads),
                    None => text,
                };
                self.ensure_live(&env_path, &sid, window, cx);
                let server = self
                    .find_env(&env_path)
                    .and_then(|e| e.remote.as_ref())
                    .map(|r| r.server.clone());
                // A server's session: it goes there with who sent it, so one
                // that does not get there is said to this phone, not put in
                // the desktop's composer.
                if let Some(server) = server {
                    self.servers_phone_send(&server, &sid, &text, now, client, cx);
                    if let Some(s) = self.session_mut(&env_path, &sid) {
                        s.updated = crate::model::unix_now();
                    }
                    cx.notify();
                } else if let Some(thread) = self.phone_thread(&env_path, &sid) {
                    thread.update(cx, |t, cx| {
                        if now {
                            t.interrupt_with(text, cx)
                        } else {
                            t.send(text, cx)
                        }
                    });
                    if let Some(s) = self.session_mut(&env_path, &sid) {
                        s.updated = crate::model::unix_now();
                    }
                    cx.notify();
                }
            }
            // A piece of a photo: base64, kept by upload id until the last one
            // lands. Then it is saved as a pasted image, and the id a prompt
            // names is worth that file.
            ClientMsg::Upload {
                session: _,
                upload,
                name,
                part,
                of,
                data,
            } => {
                let Some(link) = &mut self.phone else { return };
                let saved =
                    link.uploads
                        .piece(&upload, &name, part, of, &data, super::composer::save_image);
                if let Some(path) = saved {
                    let name = path
                        .file_name()
                        .map(|n| n.to_string_lossy())
                        .unwrap_or_default();
                    self.phone_send(
                        client,
                        json!({ "type": "uploaded", "upload": upload, "name": name }).to_string(),
                    );
                }
            }
            ClientMsg::Start {
                env,
                agent,
                text,
                settings,
                ..
            } => {
                let path = PathBuf::from(env);
                let Some(found) = self.find_env(&path) else {
                    self.phone_send(client, wire::error("that project is gone"));
                    return;
                };
                let agent = agent
                    .as_deref()
                    .map(AgentKind::from_id)
                    .unwrap_or(found.agent);
                let text = text.trim().to_string();
                if text.is_empty() {
                    return;
                }
                // On a server: it starts there, with the model and effort
                // picked (the server takes no other setting from a device),
                // and says its id when it has one.
                if found.remote.is_some() {
                    let by = super::servers::Asker::Phone(client);
                    let Some(reference) =
                        self.servers_start(&path, agent, &text, &settings, by, None)
                    else {
                        self.phone_send(client, wire::error("that server is not connected"));
                        return;
                    };
                    // The branch is named after the first prompt once it
                    // started, as from the desktop.
                    let first = super::servers::FirstPrompt {
                        env: path,
                        text,
                        agent,
                        note: None,
                        mentions_from: None,
                        desktop: false,
                    };
                    self.servers_first_prompt(reference, first);
                    return;
                }
                for (key, value) in &settings {
                    if let Some(s) = Setting::from_key(key) {
                        self.remember_setting(&path, agent, s, value, cx);
                    }
                }
                // Not shown in a tab: the desktop keeps what it had on screen.
                let Some(id) =
                    self.start_session_in(&path, agent, summary(&text), text, false, window, cx)
                else {
                    self.phone_send(client, wire::error("could not start the session"));
                    return;
                };
                self.phone_sync_inbox(cx);
                self.phone_send(client, json!({ "type": "started", "id": id }).to_string());
            }
            ClientMsg::Options { env, agent } => {
                let path = PathBuf::from(&env);
                let agent = AgentKind::from_id(&agent);
                // A server's folder: what that server offers, as last said,
                // and again once it answers.
                self.servers_ask_options_for(client, &path, agent);
                let reply = self.phone_options(&path, agent);
                self.phone_send(client, reply);
            }
            ClientMsg::Settings { session } => {
                let Some((env_path, sid)) = self.phone_session(&session) else {
                    return;
                };
                self.ensure_live(&env_path, &sid, window, cx);
                let list = match self.phone_thread(&env_path, &sid) {
                    Some(thread) => {
                        let t = thread.read(cx);
                        PHONE_SETTINGS
                            .into_iter()
                            .filter_map(|s| Some((s, t.setting(s).filter(|(_, o)| !o.is_empty())?)))
                            .collect()
                    }
                    None => Vec::new(),
                };
                let reply =
                    json!({ "type": "settings", "id": session, "settings": wire::settings(list) });
                self.phone_send(client, reply.to_string());
            }
            ClientMsg::Setting {
                session,
                key,
                value,
            } => {
                let (Some(s), Some((env_path, sid))) =
                    (Setting::from_key(&key), self.phone_session(&session))
                else {
                    return;
                };
                let Some(thread) = self.phone_thread(&env_path, &sid) else {
                    self.phone_send(client, wire::error("that session is not running"));
                    return;
                };
                thread.update(cx, |t, cx| t.set_setting(s, &value, cx));
                // The environment remembers it too, as when picked on the desktop.
                let agent = self
                    .find_env(&env_path)
                    .and_then(|e| e.sessions.iter().find(|x| *x.id == *sid))
                    .map(|x| x.agent);
                if let Some(agent) = agent {
                    self.remember_setting(&env_path, agent, s, &value, cx);
                }
                let t = thread.read(cx);
                let list = PHONE_SETTINGS
                    .into_iter()
                    .filter_map(|s| Some((s, t.setting(s).filter(|(_, o)| !o.is_empty())?)))
                    .collect();
                let reply =
                    json!({ "type": "settings", "id": session, "settings": wire::settings(list) });
                self.phone_send(client, reply.to_string());
            }
            // Esc in this app pauses the queue, for a phone through it too.
            ClientMsg::Cancel { session, .. } => {
                if let Some((env_path, sid)) = self.phone_session(&session)
                    && let Some(thread) = self.phone_thread(&env_path, &sid)
                {
                    thread.update(cx, |t, cx| t.cancel(cx));
                }
            }
            ClientMsg::Unqueue {
                session,
                index,
                text,
                now,
            } => {
                if let Some((env_path, sid)) = self.phone_session(&session)
                    && let Some(thread) = self.phone_thread(&env_path, &sid)
                {
                    thread.update(cx, |t, cx| t.unqueue(index, &text, now, cx));
                }
            }
            ClientMsg::ResumeQueue { session } => {
                if let Some((env_path, sid)) = self.phone_session(&session)
                    && let Some(thread) = self.phone_thread(&env_path, &sid)
                {
                    thread.update(cx, |t, cx| t.resume_queue(cx));
                }
            }
            ClientMsg::Permission { session, option } => {
                if let Some((env_path, sid)) = self.phone_session(&session)
                    && let Some(thread) = self.phone_thread(&env_path, &sid)
                {
                    thread.update(cx, |t, cx| t.respond_permission(&option, cx));
                }
            }
            ClientMsg::Answer { session, answers } => {
                if let Some((env_path, sid)) = self.phone_session(&session)
                    && let Some(thread) = self.phone_thread(&env_path, &sid)
                {
                    let fits = thread
                        .read(cx)
                        .pending_question()
                        .is_some_and(|q| q.questions.len() == answers.len());
                    if fits {
                        thread.update(cx, |t, cx| t.answer_question(answers, cx));
                    } else {
                        self.phone_send(client, wire::error("that question is gone"));
                    }
                }
            }
            ClientMsg::SkipQuestion { session } => {
                if let Some((env_path, sid)) = self.phone_session(&session)
                    && let Some(thread) = self.phone_thread(&env_path, &sid)
                {
                    thread.update(cx, |t, cx| t.skip_question(cx));
                }
            }
            ClientMsg::Secret {
                session,
                id,
                values,
            } => {
                let private = self.phone.as_ref().is_some_and(|l| l.server.private());
                let answered = match self
                    .phone_session(&session)
                    .and_then(|(env_path, sid)| self.phone_thread(&env_path, &sid))
                {
                    Some(thread) => {
                        thread.update(cx, |t, cx| t.answer_secret(id, values, private, cx))
                    }
                    None => Err("that session is not running".into()),
                };
                if let Err(e) = answered {
                    self.phone_send(client, wire::error(&e));
                }
            }
            ClientMsg::Changes { session } => {
                let Some((env_path, sid)) = self.phone_session(&session) else {
                    self.phone_send(client, wire::error("that session is gone"));
                    return;
                };
                let path = env_path.clone();
                self.phone_when_read(&env_path, &sid.clone(), cx, move |this, cx| {
                    let mut msg = this.with_session_entries(&path, &sid, cx, wire::changes);
                    msg["id"] = json!(session);
                    this.phone_send(client, msg.to_string());
                });
            }
            ClientMsg::Diff {
                session,
                entry,
                index,
            } => {
                let Some((env_path, sid)) = self.phone_session(&session) else {
                    self.phone_send(client, wire::error("that session is gone"));
                    return;
                };
                let path = env_path.clone();
                self.phone_when_read(&env_path, &sid.clone(), cx, move |this, cx| {
                    let diff = this.with_session_entries(&path, &sid, cx, |all, cwd| {
                        wire::diff_message(all, entry, index, cwd)
                    });
                    let reply = match diff {
                        Some(mut msg) => {
                            msg["id"] = json!(session);
                            msg.to_string()
                        }
                        None => wire::error("that change is gone"),
                    };
                    this.phone_send(client, reply);
                });
            }
            ClientMsg::Transcript { session, from } => {
                let Some((env_path, sid)) = self.phone_session(&session) else {
                    self.phone_send(client, wire::error("that session is gone"));
                    return;
                };
                let path = env_path.clone();
                self.phone_when_read(&env_path, &sid.clone(), cx, move |this, cx| {
                    let agent = this
                        .find_env(&path)
                        .and_then(|e| e.sessions.iter().find(|s| *s.id == *sid))
                        .map_or("", |s| s.agent.id());
                    let frames = this.with_session_entries(&path, &sid, cx, |all, cwd| {
                        wire::transcript_message(&session, agent, cwd, all, from)
                    });
                    for frame in frames {
                        this.phone_send(client, frame);
                    }
                });
            }
            ClientMsg::Usage => {
                // Fresh numbers follow through `phone_usage_changed` once the read lands.
                self.account
                    .update(cx, |a, cx| a.refresh_if_stale(Duration::from_secs(60), cx));
                let usage = self.phone_usage_json(cx);
                if let Some(link) = &mut self.phone {
                    link.last_usage = usage.clone();
                }
                self.phone_send(client, usage);
            }
            ClientMsg::Visible { visible } => {
                if let Some(link) = &mut self.phone {
                    if visible {
                        link.hidden.remove(&client);
                    } else {
                        link.hidden.insert(client);
                    }
                }
            }
            // Said by an app to the servers it follows. This app reads its
            // own idle time, and a phone's would not be about this computer.
            ClientMsg::Idle { .. } => {}
            // What the app asks of a server, to install there. A phone has
            // the Marketplace tab of this computer for that.
            ClientMsg::Market
            | ClientMsg::InstallPlugin { .. }
            | ClientMsg::InstallServer { .. }
            | ClientMsg::Mirror(_)
            | ClientMsg::ForgetSignIns => {
                self.phone_send(
                    client,
                    wire::error("Install from the Marketplace tab of this computer."),
                );
            }
            // What the app asks of a server. A phone loads the signed links
            // its entries carry instead.
            ClientMsg::Image { .. } => {
                self.phone_send(client, wire::error("images come through their links"));
            }
            // The desktop's projects are added in its own window, not from a device.
            ClientMsg::AddPlace { .. }
            | ClientMsg::Clone { .. }
            | ClientMsg::NewEnv { .. }
            | ClientMsg::RenameBranch { .. } => self.phone_send(
                client,
                wire::error("add projects to this computer in Arakun itself"),
            ),
            // The app's Linear settings come from its own tab, not a phone.
            ClientMsg::LinearSettings(_) => self.phone_send(
                client,
                wire::error("set up Linear in Arakun's Linear tab"),
            ),
            ClientMsg::Fork { .. }
            | ClientMsg::Compact { .. }
            | ClientMsg::Rewind { .. }
            | ClientMsg::Stage { .. }
            | ClientMsg::ContinueWith { .. } => self.phone_send(
                client,
                wire::error(
                    "fork, compact, rewind, stage or hand over this computer's sessions in Arakun itself",
                ),
            ),
            // What the app asks of a server for its terminals.
            ClientMsg::TermOpen { .. }
            | ClientMsg::TermAttach { .. }
            | ClientMsg::TermInput { .. }
            | ClientMsg::TermResize { .. }
            | ClientMsg::TermSeen { .. }
            | ClientMsg::TermClose { .. } => {
                self.phone_send(client, wire::error("terminals open on servers only"));
            }
            // What the app asks of a server for the files of its environments.
            ClientMsg::ListDir { .. } | ClientMsg::ReadFile { .. } | ClientMsg::ListFiles { .. } => {
                self.phone_send(client, wire::error("files are browsed on servers only"));
            }
            ClientMsg::SettleEnv { .. } | ClientMsg::DeleteEnv { .. } => self.phone_send(
                client,
                wire::error("settle or delete this computer's environments in Arakun itself"),
            ),
            // Answered with its `ref`, so whoever waits on it stops waiting.
            ClientMsg::Git { reference, .. } => {
                let why = anyhow::anyhow!("commit this computer's changes in Arakun itself");
                let reply = crate::git_call::reply(reference, Err(why));
                self.phone_send(client, reply.to_string());
            }
            ClientMsg::Ping => {}
        }
    }

    /// Both agents' plan limits here, as `wire::plan_usage` puts them.
    pub(super) fn local_usage(&self, cx: &App) -> serde_json::Value {
        let plan = crate::agent::plan::latest();
        let account = self.account.read(cx);
        let droid = wire::DroidPlan {
            connected: account.connected(),
            limits: account.limits(),
            error: account.error().map(|e| e.as_ref()),
            fetched_at: account.fetched_at(),
        };
        let access = crate::agent::claude_auth::latest().map(|a| a.label());
        wire::plan_usage(
            plan.as_ref(),
            access.as_deref(),
            &droid,
            crate::account::unix_now(),
        )
    }

    fn phone_usage_json(&self, cx: &Context<Self>) -> String {
        self.local_usage(cx).to_string()
    }

    /// The plan limits may have moved (a fetch landed): phones hear if they did.
    pub(super) fn phone_usage_changed(&mut self, cx: &mut Context<Self>) {
        let Some(link) = &self.phone else {
            return;
        };
        if link.server.client_count() == 0 {
            return;
        }
        let usage = self.phone_usage_json(cx);
        if let Some(link) = &mut self.phone
            && link.last_usage != usage
        {
            link.server.broadcast(&usage);
            link.last_usage = usage;
        }
    }

    /// A phone looked away from its session: the agent may stop if no one else is looking.
    fn phone_stop_watching(&mut self, client: ClientId, cx: &mut Context<Self>) {
        let Some(link) = &mut self.phone else {
            return;
        };
        let Some(id) = link.watching.remove(&client) else {
            return;
        };
        self.servers_look_away(super::servers::Watcher::Phone(client));
        if let Some((env_path, sid)) = self.phone_session(&id) {
            self.retire_if_hidden(&env_path, &sid, cx);
        }
    }

    /// Whether phone `client` looks at what phones call `pid`: a phone that
    /// came later with the same id does not.
    pub(super) fn phone_looks_at(&self, client: ClientId, pid: &str) -> bool {
        self.phone
            .as_ref()
            .is_some_and(|link| link.watching.get(&client).is_some_and(|w| w == pid))
    }

    pub(super) fn phone_send(&self, client: ClientId, text: String) {
        if let Some(link) = &self.phone {
            link.server.send(client, text);
        }
    }

    pub(super) fn phone_thread(&self, env_path: &Path, id: &str) -> Option<gpui::Entity<Thread>> {
        let env = self.find_env(env_path)?;
        env.sessions[env.session_index(id)?]
            .live
            .as_ref()
            .map(|l| l.thread.clone())
    }

    /// Every session of every project, newest first, with what a waiting one asks.
    fn phone_inbox(&self, cx: &App) -> String {
        let mut sessions: Vec<serde_json::Value> = Vec::new();
        // Where the phone can start a session, with the agent each place picks.
        let mut places: Vec<serde_json::Value> = Vec::new();
        self.phone_inbox_walk(
            cx,
            |p| places.push(wire::inbox_place(p)),
            |s| sessions.push(wire::inbox_session(s)),
        );
        wire::inbox(&phone::host_name(), sessions, places)
    }

    /// A hash of what the inbox shows. The tick compares it with the last one
    /// and builds the inbox only when it moved: building it for every
    /// session each second costs more than hashing the same fields.
    fn phone_inbox_stamp(&self, cx: &App) -> u64 {
        use std::hash::{Hash, Hasher};
        // Both walkers write to the one hasher, in the order the inbox lists.
        let h = std::cell::RefCell::new(std::collections::hash_map::DefaultHasher::new());
        self.phone_inbox_walk(
            cx,
            |p| (0u8, p).hash(&mut *h.borrow_mut()),
            |s| (1u8, s).hash(&mut *h.borrow_mut()),
        );
        h.into_inner().finish()
    }

    /// Hands each place, then each of its sessions, as the inbox shows them.
    fn phone_inbox_walk(
        &self,
        cx: &App,
        mut place: impl FnMut(wire::InboxPlace),
        mut session: impl FnMut(wire::InboxSession),
    ) {
        for p in &self.projects {
            // A server's folders too: the phone works there through this app.
            for e in &p.environments {
                place(wire::InboxPlace {
                    path: &e.path,
                    project: &p.name,
                    env: &e.name,
                    branch: Some(e.branch.as_ref()).filter(|b| !b.is_empty()),
                    agent: e.agent.id(),
                    repo: Some(&p.path),
                    // The page a phone opens has no use for it.
                    origin: None,
                });
                for s in &e.sessions {
                    let id = match &e.remote {
                        Some(r) => super::servers::phone_session_id(&r.server, &s.id),
                        None => s.id.to_string(),
                    };
                    session(wire::InboxSession {
                        id: &id,
                        title: &s.title,
                        agent: s.agent.label(),
                        status: s.status,
                        updated: s.updated,
                        added: s.added,
                        removed: s.removed,
                        project: &p.name,
                        env: &e.name,
                        place: &e.path,
                        pr: s.meta.as_ref().and_then(|m| m.pr.as_deref()),
                        asks: s.live.as_ref().and_then(|l| wire::asks(l.thread.read(cx))),
                    });
                }
            }
        }
    }

    /// Sends the inbox to every phone when it differs from what they last got.
    fn phone_sync_inbox(&mut self, cx: &mut Context<Self>) {
        let Some(link) = &self.phone else {
            return;
        };
        // The tab counts the code down and drops it when it expires.
        if link.access.lock().unwrap().pairing().is_some()
            || self.views.phone.read(cx).pairing_shown()
        {
            self.sync_phone_view(cx);
        }
        let Some(link) = &self.phone else {
            return;
        };
        if link.server.client_count() == 0 {
            return;
        }
        let phones = link.server.client_count();
        let stamp = self.phone_inbox_stamp(cx);
        if link.last_inbox_stamp != Some(stamp) {
            let inbox = self.phone_inbox(cx);
            if let Some(link) = &mut self.phone {
                link.last_inbox_stamp = Some(stamp);
                if link.last_inbox != inbox {
                    link.server.broadcast(&inbox);
                    link.last_inbox = inbox;
                }
            }
        }
        self.views
            .phone
            .update(cx, |v, cx| v.set_phones(phones, cx));
    }

    /// A session as a phone opens it: its state and its last entries.
    /// Session `id` of the environment at `env_path`, as phones call it `pid`.
    fn phone_session_json(
        &mut self,
        env_path: &Path,
        id: &SharedString,
        pid: &SharedString,
        cx: &mut Context<Self>,
    ) -> String {
        let Some(env) = self.find_env(env_path) else {
            return wire::error("that session is gone");
        };
        let Some(i) = env.session_index(id) else {
            return wire::error("that session is gone");
        };
        let s = &env.sessions[i];
        let (title, agent, status) = (s.title.clone(), s.agent.label(), s.status);
        let place = self.env_label(env_path);
        let head = wire::SessionHead {
            id: pid,
            title: &title,
            agent,
            status,
            place: &place,
        };
        match self.phone_thread(env_path, id) {
            Some(thread) => {
                let t = thread.read(cx);
                let status = super::sessions::status_of(t);
                let private = self.phone.as_ref().is_some_and(|l| l.server.private());
                let scope = self.phone_scope(env_path, t.cwd());
                wire::session_live(&head, t, status, &scope, private)
            }
            None => {
                let Some(link) = &self.phone else {
                    return wire::error("that session is gone");
                };
                let path = state::transcript_path(id);
                let queue = link.last_read.queue_kept(&path);
                link.last_read.with_kept(&path, |all, cwd| {
                    wire::session_saved(&head, all, &queue, &self.phone_scope(env_path, cwd))
                })
            }
        }
    }

    /// Entries `from..to` of a session and how many it has.
    fn phone_entries(
        &self,
        env_path: &Path,
        id: &str,
        from: usize,
        to: usize,
        cx: &Context<Self>,
    ) -> (Vec<serde_json::Value>, usize) {
        self.with_session_entries(env_path, id, cx, |all, cwd| {
            let to = to.min(all.len());
            let from = from.min(to);
            let scope = self.phone_scope(env_path, cwd);
            (
                all[from..to]
                    .iter()
                    .map(|e| wire::entry(e, &scope))
                    .collect(),
                all.len(),
            )
        })
    }

    /// Runs `f` over a session's entries and directory: the live thread's, or
    /// the transcript's when the session is not running. The transcript is
    /// the one [`Shell::phone_when_read`] read; this never reads it.
    fn with_session_entries<R>(
        &self,
        env_path: &Path,
        id: &str,
        cx: &Context<Self>,
        f: impl FnOnce(&[crate::agent::Entry], &Path) -> R,
    ) -> R {
        match self.phone_thread(env_path, id) {
            Some(thread) => {
                let t = thread.read(cx);
                f(t.entries(), t.cwd())
            }
            None => match &self.phone {
                Some(link) => link.last_read.with_kept(&state::transcript_path(id), f),
                None => f(&[], Path::new("")),
            },
        }
    }

    /// Calls `f` once a session's entries are at hand: right away for a
    /// running session or a transcript read before, else after its
    /// transcript is read and parsed on the background executor, so a long
    /// one does not hold up the window.
    fn phone_when_read(
        &mut self,
        env_path: &Path,
        id: &str,
        cx: &mut Context<Self>,
        f: impl FnOnce(&mut Self, &mut Context<Self>) + 'static,
    ) {
        let path = state::transcript_path(id);
        let ready = self.phone_thread(env_path, id).is_some()
            || self
                .phone
                .as_ref()
                .is_none_or(|l| l.last_read.is_kept(&path));
        if ready {
            f(self, cx);
            return;
        }
        let read = cx.background_spawn(async move { crate::agent::saved::read(&path) });
        cx.spawn(async move |this, cx| {
            let read = read.await;
            let _ = this.update(cx, |this, cx| {
                if let Some(link) = &this.phone {
                    link.last_read.keep(read);
                }
                f(this, cx);
            });
        })
        .detach();
    }

    /// Called for every event of a live thread; notes what phones should hear about.
    pub(super) fn phone_thread_event(
        &mut self,
        id: &SharedString,
        ev: &ThreadEvent,
        cx: &mut Context<Self>,
    ) {
        let Some(link) = &mut self.phone else {
            return;
        };
        // The inbox shows every session's status and title, watched or not.
        if matches!(
            ev,
            ThreadEvent::StatusChanged
                | ThreadEvent::NeedsPermission
                | ThreadEvent::TitleChanged
                | ThreadEvent::Finished { .. }
        ) && link.server.client_count() > 0
        {
            link.inbox_dirty = true;
        }
        if !link.watching.values().any(|w| w == id) {
            if link.inbox_dirty {
                self.phone_schedule_flush(cx);
            }
            return;
        }
        match ev {
            ThreadEvent::EntriesChanged { from } => {
                let e = link.dirty.entry(id.clone()).or_insert(*from);
                *e = (*e).min(*from);
            }
            ThreadEvent::Reset => {
                link.dirty.insert(id.clone(), 0);
            }
            // Shell changes land a moment after the turn ends; the changes chip follows them.
            ThreadEvent::StatusChanged
            | ThreadEvent::NeedsPermission
            | ThreadEvent::TitleChanged
            | ThreadEvent::Changed
            | ThreadEvent::ShellChanges => {
                if !link.state_dirty.contains(id) {
                    link.state_dirty.push(id.clone());
                }
            }
            // The shell may retire the thread right after a turn ends; send
            // what is pending while it is still there.
            ThreadEvent::Finished { .. } => {
                if !link.state_dirty.contains(id) {
                    link.state_dirty.push(id.clone());
                }
                self.phone_flush(cx);
                return;
            }
            _ => return,
        }
        self.phone_schedule_flush(cx);
    }

    fn phone_flush(&mut self, cx: &mut Context<Self>) {
        let Some(link) = &mut self.phone else {
            return;
        };
        link.flush = None;
        let dirty: Vec<(SharedString, usize)> = link.dirty.drain().collect();
        let states: Vec<SharedString> = std::mem::take(&mut link.state_dirty);
        if std::mem::take(&mut link.inbox_dirty) {
            self.phone_sync_inbox(cx);
        }
        for (id, from) in dirty {
            let Some((env_path, sid)) = self.phone_session(&id) else {
                continue;
            };
            let Some(thread) = self.phone_thread(&env_path, &sid) else {
                continue;
            };
            let t = thread.read(cx);
            let msg = wire::entries_message(
                &id,
                t.entries(),
                from,
                &self.phone_scope(&env_path, t.cwd()),
            );
            self.phone_send_watchers(&id, msg);
        }
        for id in states {
            let Some((env_path, sid)) = self.phone_session(&id) else {
                continue;
            };
            let Some(thread) = self.phone_thread(&env_path, &sid) else {
                continue;
            };
            let t = thread.read(cx);
            let private = self.phone.as_ref().is_some_and(|l| l.server.private());
            let msg = wire::state_message(&id, t, super::sessions::status_of(t), private);
            self.phone_send_watchers(&id, msg);
        }
    }

    fn phone_send_watchers(&self, id: &SharedString, msg: String) {
        let Some(link) = &self.phone else {
            return;
        };
        for (client, watched) in &link.watching {
            if watched == id {
                link.server.send(*client, msg.clone());
            }
        }
    }

    /// Offers a notice about a session to every phone that asked for them,
    /// except the ones looking at that very session right now. It goes now,
    /// or once the person is away from the computer.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn notify_phones(
        &mut self,
        id: &SharedString,
        kind: Kind,
        title: &str,
        body: &str,
        env: &str,
        secs: Option<u64>,
        window: &Window,
        cx: &mut Context<Self>,
    ) {
        let Some(link) = &mut self.phone else {
            return;
        };
        // Phones with this session on screen see the change already.
        let looking: HashSet<String> = link
            .watching
            .iter()
            .filter(|(client, watched)| *watched == id && !link.hidden.contains(client))
            .filter_map(|(client, _)| link.server.device_of(*client))
            .collect();
        let c = notify::Candidate {
            session: id.to_string(),
            kind,
            turn_secs: secs,
            payload: push::notice(kind.as_str(), id, title, body, env, secs),
        };
        let now = link.notifier.offer(
            c,
            &link.access.lock().unwrap().devices,
            &looking,
            presence(window),
            Instant::now(),
        );
        self.push_notices(now, cx);
        self.time_notices(window, cx);
    }

    /// The person opened the session on the computer: its waiting notices
    /// are moot.
    pub(super) fn phone_seen(&mut self, env_path: &Path, id: &str) {
        let pid = self.phone_id(env_path, id);
        if let Some(link) = &mut self.phone {
            link.notifier.seen(&pid);
        }
    }

    /// Checks the waiting notices about every 15 s while there are any.
    fn time_notices(&mut self, window: &Window, cx: &mut Context<Self>) {
        let Some(link) = &self.phone else {
            return;
        };
        if link.notice_timer.is_some() || !link.notifier.has_pending() {
            return;
        }
        let timer = cx.spawn_in(window, async move |this, cx| {
            loop {
                cx.background_executor().timer(notify::CHECK_EVERY).await;
                let more = this.update_in(cx, |this, window, cx| this.tick_notices(window, cx));
                if !matches!(more, Ok(true)) {
                    break;
                }
            }
        });
        if let Some(link) = &mut self.phone {
            link.notice_timer = Some(timer);
        }
    }

    /// Sends the waiting notices that may go now. False once none wait.
    fn tick_notices(&mut self, window: &Window, cx: &mut Context<Self>) -> bool {
        let Some(link) = &mut self.phone else {
            return false;
        };
        let mut notifier = std::mem::take(&mut link.notifier);
        let access = link.access.clone();
        let due = {
            let app: &App = cx;
            let still = |pid: &str, kind| self.phone_still(pid, kind, app);
            // A phone looking at the session drops it through `still`.
            let devices = &access.lock().unwrap().devices;
            notifier.tick(
                devices,
                &HashSet::new(),
                presence(window),
                still,
                Instant::now(),
            )
        };
        let more = notifier.has_pending();
        if let Some(link) = &mut self.phone {
            link.notifier = notifier;
            if !more {
                link.notice_timer = None;
            }
        }
        self.push_notices(due, cx);
        more
    }

    /// Whether the session phones call `pid` is still in the state its
    /// notice is about: still waiting for an answer, or not at work again.
    /// One a phone has on screen is seen.
    fn phone_still(&self, pid: &str, kind: Kind, cx: &App) -> bool {
        let Some(link) = &self.phone else {
            return false;
        };
        let looked_at = link
            .watching
            .iter()
            .any(|(client, watched)| watched == pid && !link.hidden.contains(client));
        let Some((env_path, id)) = self.phone_session(pid).filter(|_| !looked_at) else {
            return false;
        };
        // A retired session is neither working nor asking anything.
        let Some(thread) = self.phone_thread(&env_path, &id) else {
            return kind != Kind::Input;
        };
        thread.read(cx).still(kind)
    }

    /// Pushes notices in the background. Subscriptions the push service says
    /// are gone are forgotten.
    fn push_notices(&self, notices: Vec<notify::Delivery>, cx: &mut Context<Self>) {
        let Some(link) = &self.phone else {
            return;
        };
        if notices.is_empty() {
            return;
        }
        let (vapid, access) = (link.vapid.clone(), link.access.clone());
        cx.spawn(async move |this, cx| {
            let gone = cx
                .background_executor()
                .spawn(async move {
                    let now = crate::model::unix_now();
                    let mut gone = Vec::new();
                    for n in notices {
                        match push::send(&vapid, &n.sub, n.payload.as_bytes(), now) {
                            Ok(()) => {}
                            Err(push::Fail::Gone) => gone.push((n.device, n.sub)),
                            Err(e) => eprintln!("arakun: could not notify a phone: {e}"),
                        }
                    }
                    if !gone.is_empty() {
                        access.lock().unwrap().drop_subscriptions(&gone);
                    }
                    gone
                })
                .await;
            if !gone.is_empty() {
                let _ = this.update(cx, |this, cx| {
                    this.save_devices();
                    this.sync_phone_view(cx);
                });
            }
        })
        .detach();
    }
}

/// Whether the person is at the computer: how long since they last used it,
/// when the desktop can tell, and whether this window has focus.
fn presence(window: &Window) -> Presence {
    Presence {
        idle: (arakun_core::host::get().idle_for)(),
        focused: window.is_window_active(),
    }
}

/// What the relay client is started with: the account's sign-in only when
/// the relay is the service that issued it, and never the made-up one of
/// `ARAKUN_TEAMS_FIXTURE`. Otherwise it starts signed out, and why when
/// there was an account.
fn relay_sign_in(
    relay_base: &str,
    account: Option<&crate::arakun_teams::Account>,
    fixture: bool,
) -> (relay::Account, Option<String>) {
    let signed_out = relay::Account {
        github_id: 0,
        session: None,
    };
    let Some(account) = account.filter(|_| !fixture) else {
        return (signed_out, None);
    };
    if !relay::takes_session_from(relay_base, &account.base) {
        return (
            signed_out,
            Some(format!(
                "The relay ({}) is not the server you signed in to ({}). Set `relay_base` in phone.json and ARAKUN_TEAMS_URL to the same server.",
                relay_base.trim().trim_end_matches('/'),
                account.base.trim_end_matches('/')
            )),
        );
    }
    (
        relay::Account {
            github_id: account.github_id,
            session: Some(account.session.clone()),
        },
        None,
    )
}

/// How the relay connection stands, as the address row of the Phone tab tells it.
fn relay_address(status: relay::Status, page: &str) -> Address {
    match status {
        relay::Status::Connecting => Address::Pending("Connecting to the relay…".into()),
        relay::Status::Connected => Address::Ready(page.to_string()),
        relay::Status::Reconnecting { why, retry_in } => Address::Pending(format!(
            "{why} Trying again in {}.",
            relay::wait_text(retry_in)
        )),
        relay::Status::Refused { why, retry_in } => Address::Problem {
            what: format!("{why} Trying again in {}.", relay::wait_text(retry_in)),
            link: None,
        },
        relay::Status::Rejected(why) => Address::Problem {
            what: why,
            link: None,
        },
        // The account section above says what to do.
        relay::Status::SignedOut => Address::Pending("Waiting for your sign-in.".into()),
    }
}

/// Where phones find the server at `port` through Tailscale right now, with
/// `serve` pointed at it when Tailscale can; otherwise what stands in the way.
/// Runs processes: for a background thread.
fn tailscale_address(port: u16) -> Address {
    match tailscale::status() {
        tailscale::Status::Ready { name } => match tailscale::serve(port) {
            Ok(()) => Address::Ready(tailscale::url(&name)),
            Err(e) => Address::Problem { what: format!("Tailscale could not serve the phone: {e}"), link: None },
        },
        tailscale::Status::Missing => Address::Problem {
            what: "Tailscale is not installed on this computer.".into(),
            link: Some(("Get Tailscale", "https://tailscale.com/download".into())),
        },
        tailscale::Status::Down => Address::Problem {
            what: "Tailscale is not connected. Connect it from its icon in the tray; this picks it up on its own.".into(),
            link: None,
        },
        tailscale::Status::NoHttps => Address::Problem {
            what: "Your tailnet does not issue HTTPS certificates yet. Turn on HTTPS Certificates in its DNS settings.".into(),
            link: Some(("Open DNS settings", tailscale::ADMIN_DNS_URL.into())),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_sign_in_the_relay_turned_down_is_asked_about_again() {
        assert!(relay_ended_sign_in(&relay::Status::SignedOut, true));
        // Started without one, it only waits for a sign-in.
        assert!(!relay_ended_sign_in(&relay::Status::SignedOut, false));
        assert!(!relay_ended_sign_in(&relay::Status::Connected, true));
        assert!(!relay_ended_sign_in(
            &relay::Status::Rejected("no".into()),
            true
        ));
    }

    fn machine(label: &str, kind: &str) -> cloud::Machine {
        cloud::Machine {
            label: label.into(),
            public: String::new(),
            name: format!("{label} box"),
            kind: kind.into(),
            online: false,
            last_seen_at: 9,
        }
    }

    #[test]
    fn your_computers_are_the_accounts_apps_with_this_one_marked() {
        let list = [
            machine("aaaaaaaaaaaaaaaa", "app"),
            machine("bbbbbbbbbbbbbbbb", "server"),
            machine("cccccccccccccccc", "app"),
        ];
        let shown: Vec<(String, bool)> = your_computers(&list, Some("cccccccccccccccc"))
            .into_iter()
            .map(|c| (c.label, c.this))
            .collect();
        assert_eq!(
            shown,
            [
                ("aaaaaaaaaaaaaaaa".to_string(), false),
                ("cccccccccccccccc".to_string(), true)
            ]
        );
        assert!(your_computers(&list, None).iter().all(|c| !c.this));
    }

    #[test]
    fn a_new_key_whose_old_entry_stays_says_where_to_remove_it() {
        let text = old_key_still_listed(&cloud::Error::Unreachable("timed out".into()));
        assert!(
            text.contains("still listed") && text.contains("Your computers"),
            "{text}"
        );
        assert!(text.contains("timed out"), "{text}");
    }

    fn account(base: &str) -> crate::arakun_teams::Account {
        crate::arakun_teams::Account {
            base: base.into(),
            session: "tok".into(),
            github_id: 42,
            login: "octo".into(),
        }
    }

    #[test]
    fn the_relay_gets_the_session_only_from_the_server_that_issued_it() {
        // An address without a scheme is the https server.
        let (a, why) = relay_sign_in(
            "app.arakun.dev",
            Some(&account("https://app.arakun.dev")),
            false,
        );
        assert_eq!((a.github_id, a.session.as_deref()), (42, Some("tok")));
        assert!(why.is_none());

        let (a, why) = relay_sign_in(
            "https://APP.arakun.dev/",
            Some(&account("https://app.arakun.dev")),
            false,
        );
        assert_eq!((a.github_id, a.session.as_deref()), (42, Some("tok")));
        assert!(why.is_none());

        let (a, why) = relay_sign_in(
            "https://relay.example",
            Some(&account("https://app.arakun.dev")),
            false,
        );
        assert_eq!((a.github_id, a.session), (0, None));
        let why = why.unwrap();
        assert!(
            why.contains("https://relay.example") && why.contains("https://app.arakun.dev"),
            "{why}"
        );

        // A test run's made-up account never reaches a real relay.
        let (a, why) = relay_sign_in(
            "https://app.arakun.dev",
            Some(&account("https://app.arakun.dev")),
            true,
        );
        assert_eq!((a.github_id, a.session), (0, None));
        assert!(why.is_none());
        let (a, _) = relay_sign_in("https://app.arakun.dev", None, false);
        assert_eq!((a.github_id, a.session), (0, None));
    }
}
