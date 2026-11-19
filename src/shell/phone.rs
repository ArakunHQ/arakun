//! The phone's side of the shell: turning on the server (`crate::phone`),
//! answering what phones ask, and sending them what changes. Entry changes
//! are gathered for a short while before they go out, so a streaming agent
//! costs a few messages a second, not one per token.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use futures::StreamExt as _;
use gpui::{App, Context, SharedString, Task, Window};
use serde_json::json;

use super::Shell;
use super::sessions::{Notice, summary};
use crate::agent::{Setting, Thread, ThreadEvent};
use crate::model::TabKind;
use crate::phone::relay::{self, RelayClient};
use crate::phone::{
    self, Access, ClientId, ClientMsg, Incoming, Reach, Server, Transport, push, tailscale, wire,
};
use crate::phone_view::{Address, DeviceInfo, PairingInfo, PhoneEvent, PhoneState, RelayInfo};
use crate::state;

/// Entry changes wait this long for more before they are sent.
const GATHER: Duration = Duration::from_millis(90);
/// The tick: counts the pairing code down, and compares the inbox with what phones
/// last saw as a safety net (changes normally reach them through the flush).
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
    /// Signs the push notifications this computer sends.
    vapid: Arc<push::Vapid>,
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
    /// The plan limits phones last got, so a refresh that changed nothing stays quiet.
    last_usage: String,
    /// Photos phones are sending, in pieces.
    uploads: phone::Uploads,
    /// The saved transcript a device read last, for the pages after the first.
    last_read: crate::agent::saved::LastRead,
    _listen: Task<()>,
    _tick: Task<()>,
}

/// The settings a phone can pick: the model, then the reasoning (its choices depend on the model).
const PHONE_SETTINGS: [Setting; 2] = [Setting::Model, Setting::Effort];

impl Shell {
    /// Whether a phone is looking at this session, so it stays live like an open tab.
    pub(super) fn phone_watching(&self, id: &str) -> bool {
        self.phone
            .as_ref()
            .is_some_and(|link| link.watching.values().any(|w| w.as_ref() == id))
    }

    /// What a session's entries are read against when they go to a phone.
    fn phone_scope(&self, cwd: &Path) -> wire::Scope {
        let key = self
            .phone
            .as_ref()
            .map(|l| l.access.lock().unwrap().image_key.clone())
            .unwrap_or_default();
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
            self.open_picker(window, cx);
            return;
        }
        let env = self.env_mut();
        let index = match env
            .tabs
            .iter()
            .position(|t| matches!(t.kind, TabKind::Phone))
        {
            Some(i) => i,
            None => {
                env.tabs
                    .push(crate::model::Tab::new("Phone", TabKind::Phone));
                env.tabs.len() - 1
            }
        };
        env.active_tab = index;
        self.note_active_tab(cx);
        self.sync_phone_view(cx);
        self.phone_view.update(cx, |v, cx| v.focus(window, cx));
        cx.notify();
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
                settings.save();
                self.start_phone(&settings, window, cx);
            }
            PhoneEvent::Disable => {
                self.save_devices();
                let mut settings = phone::Settings::load();
                settings.enabled = false;
                settings.save();
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
                settings.save();
                if self.phone.is_some() {
                    self.stop_phone(cx);
                    self.start_phone(&settings, window, cx);
                } else {
                    self.sync_phone_view(cx);
                }
            }
            PhoneEvent::Pair => {
                if let Some(link) = &self.phone {
                    let mut access = link.access.lock().unwrap();
                    match link.reach {
                        Reach::Relay => {
                            access.begin_pairing_relay();
                        }
                        Reach::Lan | Reach::Tailscale => {
                            access.begin_pairing();
                        }
                    }
                    drop(access);
                    link.server.sync_pairing(&link.access);
                }
                self.sync_phone_view(cx);
            }
            PhoneEvent::CancelPairing => {
                if let Some(link) = &self.phone {
                    link.access.lock().unwrap().cancel_pairing();
                    link.server.sync_pairing(&link.access);
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
            PhoneEvent::NewIdentity => {
                let was_on = self.phone.is_some();
                self.stop_phone(cx);
                let mut settings = phone::Settings::load();
                settings.renew_identity();
                if was_on {
                    self.start_phone(&settings, window, cx);
                } else {
                    self.sync_phone_view(cx);
                }
            }
            PhoneEvent::OpenHere(url) => {
                if self.has_projects() {
                    let env_path = self.env().path.clone();
                    self.push_browser_tab(&env_path, url.clone(), true, true, window, cx);
                }
            }
        }
    }

    /// Starts the server when the last run left it on.
    pub(super) fn restore_phone(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let settings = phone::Settings::load();
        if settings.enabled {
            self.start_phone(&settings, window, cx);
        }
    }

    /// Writes the devices as the server now knows them (paired, seen, unpaired).
    fn save_devices(&self) {
        let Some(link) = &self.phone else {
            return;
        };
        let mut settings = phone::Settings::load();
        settings.devices = link.access.lock().unwrap().devices.clone();
        settings.save();
        link.server.sync_tickets(&link.access);
    }

    /// Closes the server and every phone; gives Tailscale's HTTPS port back if it was ours.
    fn stop_phone(&mut self, cx: &mut Context<Self>) {
        let Some(link) = self.phone.take() else {
            return;
        };
        if link.reach == Reach::Tailscale {
            cx.background_executor()
                .spawn(async {
                    let _ = tailscale::stop_serving();
                })
                .detach();
        }
    }

    fn start_phone(
        &mut self,
        settings: &phone::Settings,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let vapid = Arc::new(settings.clone().vapid());
        let access = Access::new(settings.devices.clone(), vapid.public_key());
        let (server, mut incoming) = match settings.reach {
            Reach::Lan | Reach::Tailscale => {
                match Server::start(settings.reach.bind(), settings.port, access.clone()) {
                    Ok((s, rx)) => (Transport::Local(s), rx),
                    Err(e) => {
                        self.phone_view.update(cx, |v, cx| {
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
                let (r, rx) = RelayClient::start(
                    &settings.relay_base,
                    settings.clone().identity(),
                    access.clone(),
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
        let tick = cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(INBOX_EVERY).await;
                if this
                    .update(cx, |this, cx| {
                        if let Some(link) = &this.phone {
                            link.last_read.trim();
                        }
                        this.phone_sync_inbox(cx)
                    })
                    .is_err()
                {
                    break;
                }
            }
        });
        let remote = match &server {
            Transport::Relay(client) => relay_address(relay::Status::Connecting, client.page_url()),
            Transport::Local(_) => Address::Pending("Asking Tailscale…".into()),
        };
        self.phone = Some(PhoneLink {
            server,
            reach: settings.reach,
            vapid,
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
            last_usage: String::new(),
            uploads: phone::Uploads::default(),
            last_read: Default::default(),
            _listen: listen,
            _tick: tick,
        });
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
        let mut settings = phone::Settings::load();
        let reach = self.phone.as_ref().map_or(settings.reach, |l| l.reach);
        // The identity is made the first time the relay is chosen, not before.
        let relay = (reach == Reach::Relay).then(|| {
            let label = settings.identity().label();
            RelayInfo {
                url: settings.relay_address(&label),
            }
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
                    Reach::Tailscale | Reach::Relay => link.remote.clone(),
                };
                let base = match (&address, &link.server) {
                    (Address::Ready(url), _) => url.clone(),
                    // The relay address stands whether or not we are connected to it right now.
                    (_, Transport::Relay(client)) => client.page_url().to_string(),
                    _ => format!("http://<this computer>:{port}/"),
                };
                let connected = link.server.connected_devices();
                let mut access = link.access.lock().unwrap();
                let pairing = access
                    .pairing()
                    .map(|(code, left)| match access.pairing_secret() {
                        // Through the relay the link carries everything; there is no code to read out.
                        Some(secret) => PairingInfo {
                            url: phone::relay_link(&base, &code, &secret),
                            code: String::new(),
                            seconds_left: left.as_secs(),
                        },
                        None => PairingInfo {
                            url: phone::link(&base, &code),
                            code,
                            seconds_left: left.as_secs(),
                        },
                    });
                let devices = access
                    .devices
                    .iter()
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
        self.phone_view.update(cx, |v, cx| v.set_state(state, cx));
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
                if let Some(link) = &mut self.phone {
                    link.hidden.remove(&id);
                }
                self.sync_phone_view(cx);
            }
            // The device's sockets close on their own; `Disconnected` tidies the rest.
            Incoming::Paired | Incoming::Subscribed | Incoming::Unpaired => {
                self.save_devices();
                self.sync_phone_view(cx);
            }
            Incoming::Relay(status) => {
                if let Some(link) = &mut self.phone
                    && let Transport::Relay(client) = &link.server
                {
                    link.remote = relay_address(status, client.page_url());
                }
                self.sync_phone_view(cx);
            }
            Incoming::Message(id, msg) => self.phone_message(id, msg, window, cx),
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
            ClientMsg::Open { session } => {
                let sid: SharedString = session.clone().into();
                let Some(env_path) = self.env_of_session(&session) else {
                    self.phone_send(client, wire::error("that session is gone"));
                    return;
                };
                if let Some(link) = &mut self.phone {
                    link.watching.insert(client, sid.clone());
                }
                let reply = self.phone_session_json(&env_path, &sid, cx);
                self.phone_send(client, reply);
            }
            ClientMsg::Close => self.phone_stop_watching(client, cx),
            ClientMsg::Entries { session, from, to } => {
                let sid: SharedString = session.clone().into();
                let Some(env_path) = self.env_of_session(&session) else {
                    return;
                };
                let (entries, count) = self.phone_entries(&env_path, &sid, from, to.max(from), cx);
                let reply = json!({ "type": "entries", "id": session, "from": from, "count": count, "entries": entries, "older": true });
                self.phone_send(client, reply.to_string());
            }
            ClientMsg::Send {
                session,
                text,
                uploads,
            } => {
                let Some(env_path) = self.env_of_session(&session) else {
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
                self.ensure_live(&env_path, &session, window, cx);
                if let Some(thread) = self.phone_thread(&env_path, &session) {
                    thread.update(cx, |t, cx| t.send(text, cx));
                    if let Some(s) = self.session_mut(&env_path, &session) {
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
                        .piece(&upload, &name, part, of, &data, super::paste::save_image);
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
            } => {
                let path = PathBuf::from(env);
                let Some(found) = self.find_env(&path) else {
                    self.phone_send(client, wire::error("that project is gone"));
                    return;
                };
                let agent = agent
                    .as_deref()
                    .map(crate::model::Agent::from_key)
                    .unwrap_or(found.agent);
                let text = text.trim().to_string();
                if text.is_empty() {
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
                let agent = crate::model::Agent::from_key(&agent);
                let list = PHONE_SETTINGS
                    .into_iter()
                    .filter_map(|s| Some((s, self.draft_choices(&path, agent, s)?)))
                    .collect();
                let reply = json!({ "type": "options", "env": env, "agent": agent.key(), "settings": wire::settings(list) });
                self.phone_send(client, reply.to_string());
            }
            ClientMsg::Settings { session } => {
                let Some(env_path) = self.env_of_session(&session) else {
                    return;
                };
                self.ensure_live(&env_path, &session, window, cx);
                let list = match self.phone_thread(&env_path, &session) {
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
                let (Some(s), Some(env_path)) =
                    (Setting::from_key(&key), self.env_of_session(&session))
                else {
                    return;
                };
                let Some(thread) = self.phone_thread(&env_path, &session) else {
                    self.phone_send(client, wire::error("that session is not running"));
                    return;
                };
                thread.update(cx, |t, cx| t.set_setting(s, &value, cx));
                // The environment remembers it too, as when picked on the desktop.
                let agent = self
                    .find_env(&env_path)
                    .and_then(|e| e.sessions.iter().find(|x| *x.id == *session))
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
            ClientMsg::Cancel { session } => {
                if let Some(env_path) = self.env_of_session(&session)
                    && let Some(thread) = self.phone_thread(&env_path, &session)
                {
                    thread.update(cx, |t, cx| t.cancel(cx));
                }
            }
            ClientMsg::Permission { session, option } => {
                if let Some(env_path) = self.env_of_session(&session)
                    && let Some(thread) = self.phone_thread(&env_path, &session)
                {
                    thread.update(cx, |t, cx| t.respond_permission(&option, cx));
                }
            }
            ClientMsg::Answer { session, answers } => {
                if let Some(env_path) = self.env_of_session(&session)
                    && let Some(thread) = self.phone_thread(&env_path, &session)
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
                if let Some(env_path) = self.env_of_session(&session)
                    && let Some(thread) = self.phone_thread(&env_path, &session)
                {
                    thread.update(cx, |t, cx| t.skip_question(cx));
                }
            }
            ClientMsg::Changes { session } => {
                let Some(env_path) = self.env_of_session(&session) else {
                    self.phone_send(client, wire::error("that session is gone"));
                    return;
                };
                let mut msg = self.with_session_entries(&env_path, &session, cx, wire::changes);
                msg["id"] = json!(session);
                self.phone_send(client, msg.to_string());
            }
            ClientMsg::Diff {
                session,
                entry,
                index,
            } => {
                let Some(env_path) = self.env_of_session(&session) else {
                    self.phone_send(client, wire::error("that session is gone"));
                    return;
                };
                let diff = self.with_session_entries(&env_path, &session, cx, |all, cwd| {
                    wire::diff_message(all, entry, index, cwd)
                });
                let reply = match diff {
                    Some(mut msg) => {
                        msg["id"] = json!(session);
                        msg.to_string()
                    }
                    None => wire::error("that change is gone"),
                };
                self.phone_send(client, reply);
            }
            ClientMsg::Transcript { session, from } => {
                let Some(env_path) = self.env_of_session(&session) else {
                    self.phone_send(client, wire::error("that session is gone"));
                    return;
                };
                let agent = self
                    .find_env(&env_path)
                    .and_then(|e| e.sessions.iter().find(|s| *s.id == *session))
                    .map_or("", |s| s.agent.key());
                let frames = self.with_session_entries(&env_path, &session, cx, |all, cwd| {
                    wire::transcript_message(&session, agent, cwd, all, from)
                });
                for frame in frames {
                    self.phone_send(client, frame);
                }
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
            // The desktop's projects are added in its own window, not from a device.
            ClientMsg::AddPlace { .. } | ClientMsg::Clone { .. } => self.phone_send(
                client,
                wire::error("add projects to this computer in Arakun itself"),
            ),
            ClientMsg::Ping => {}
        }
    }

    /// Both agents' plan limits, as `wire::plan_usage` puts them.
    fn phone_usage_json(&self, cx: &Context<Self>) -> String {
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
        .to_string()
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
        if let Some(env_path) = self.env_of_session(&id) {
            self.retire_if_hidden(&env_path, &id, cx);
        }
    }

    fn phone_send(&self, client: ClientId, text: String) {
        if let Some(link) = &self.phone {
            link.server.send(client, text);
        }
    }

    fn env_of_session(&self, id: &str) -> Option<PathBuf> {
        local_env_of_session(&self.projects, id)
    }

    fn phone_thread(&self, env_path: &Path, id: &str) -> Option<gpui::Entity<Thread>> {
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
        for p in &self.projects {
            // A phone pairs with each server itself; this app speaks for this machine.
            for e in p.environments.iter().filter(|e| e.remote.is_none()) {
                places.push(wire::inbox_place(wire::InboxPlace {
                    path: &e.path,
                    project: &p.name,
                    env: &e.name,
                    branch: Some(&e.branch),
                    agent: e.agent.key(),
                    repo: Some(&p.path),
                    // The page a phone opens has no use for it.
                    origin: None,
                }));
                for s in &e.sessions {
                    sessions.push(wire::inbox_session(wire::InboxSession {
                        id: &s.id,
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
                    }));
                }
            }
        }
        wire::inbox(&phone::host_name(), sessions, places)
    }

    /// Sends the inbox to every phone when it differs from what they last got.
    fn phone_sync_inbox(&mut self, cx: &mut Context<Self>) {
        let Some(link) = &self.phone else {
            return;
        };
        // The tab counts the code down and drops it when it expires.
        if link.access.lock().unwrap().pairing().is_some()
            || self.phone_view.read(cx).pairing_shown()
        {
            self.sync_phone_view(cx);
        }
        let Some(link) = &self.phone else {
            return;
        };
        if link.server.client_count() == 0 {
            return;
        }
        let inbox = self.phone_inbox(cx);
        let phones = link.server.client_count();
        if let Some(link) = &mut self.phone
            && link.last_inbox != inbox
        {
            link.server.broadcast(&inbox);
            link.last_inbox = inbox;
        }
        self.phone_view.update(cx, |v, cx| v.set_phones(phones, cx));
    }

    /// A session as a phone opens it: its state and its last entries.
    fn phone_session_json(
        &mut self,
        env_path: &Path,
        id: &SharedString,
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
            id,
            title: &title,
            agent,
            status,
            place: &place,
        };
        match self.phone_thread(env_path, id) {
            Some(thread) => {
                let t = thread.read(cx);
                let status = super::sessions::status_of(t);
                wire::session_live(&head, t, status, &self.phone_scope(t.cwd()))
            }
            None => {
                let (all, cwd) = Thread::read_transcript(&state::transcript_path(id))
                    .map(|t| (t.entries, t.meta.cwd))
                    .unwrap_or_default();
                wire::session_saved(&head, &all, &self.phone_scope(&cwd))
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
            let scope = self.phone_scope(cwd);
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
    /// the transcript's when the session is not running.
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
            None => {
                let path = state::transcript_path(id);
                match &self.phone {
                    Some(link) => link.last_read.with(&path, f),
                    None => {
                        let (all, cwd) = Thread::read_transcript(&path)
                            .map(|t| (t.entries, t.meta.cwd))
                            .unwrap_or_default();
                        f(&all, &cwd)
                    }
                }
            }
        }
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
            let Some(env_path) = self.env_of_session(&id) else {
                continue;
            };
            let Some(thread) = self.phone_thread(&env_path, &id) else {
                continue;
            };
            let t = thread.read(cx);
            let msg = wire::entries_message(&id, t.entries(), from, &self.phone_scope(t.cwd()));
            self.phone_send_watchers(&id, msg);
        }
        for id in states {
            let Some(env_path) = self.env_of_session(&id) else {
                continue;
            };
            let Some(thread) = self.phone_thread(&env_path, &id) else {
                continue;
            };
            let t = thread.read(cx);
            let msg = wire::state_message(&id, t, super::sessions::status_of(t));
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

    /// Pushes a notice about a session to every phone that asked for them,
    /// except the ones looking at that very session right now. Subscriptions
    /// the push service says are gone are forgotten.
    pub(super) fn notify_phones(
        &mut self,
        id: &SharedString,
        kind: Notice,
        title: &str,
        body: &str,
        cx: &mut Context<Self>,
    ) {
        let Some(link) = &self.phone else {
            return;
        };
        // Phones with this session on screen see the change already.
        let looking: HashSet<String> = link
            .watching
            .iter()
            .filter(|(client, watched)| *watched == id && !link.hidden.contains(client))
            .filter_map(|(client, _)| link.server.device_of(*client))
            .collect();
        let wanted = |d: &phone::Device| match kind {
            Notice::Finished => d.notices.done,
            Notice::Input => d.notices.input,
            Notice::Error => d.notices.error,
        };
        let targets: Vec<(String, push::Subscription)> = link
            .access
            .lock()
            .unwrap()
            .devices
            .iter()
            .filter(|d| !looking.contains(&d.id) && wanted(d))
            .filter_map(|d| d.push.clone().map(|s| (d.id.clone(), s)))
            .collect();
        if targets.is_empty() {
            return;
        }
        let kind = match kind {
            Notice::Finished => "done",
            Notice::Input => "input",
            Notice::Error => "error",
        };
        let payload =
            json!({ "kind": kind, "session": id, "title": title, "body": body }).to_string();
        let (vapid, access) = (link.vapid.clone(), link.access.clone());
        cx.spawn(async move |this, cx| {
            let gone = cx
                .background_executor()
                .spawn(async move {
                    let now = crate::model::unix_now();
                    let mut gone = Vec::new();
                    for (device, sub) in targets {
                        match push::send(&vapid, &sub, payload.as_bytes(), now) {
                            Ok(()) => {}
                            Err(push::Fail::Gone) => gone.push((device, sub)),
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

/// How the relay connection stands, as the address row of the Phone tab tells it.
fn relay_address(status: relay::Status, page: &str) -> Address {
    match status {
        relay::Status::Connecting => Address::Pending("Connecting to the relay…".into()),
        relay::Status::Connected => Address::Ready(page.to_string()),
        relay::Status::Reconnecting { why, retry_in } => Address::Pending(format!(
            "{why} Trying again in {} s.",
            retry_in.as_secs().max(1)
        )),
        relay::Status::Rejected(why) => Address::Problem {
            what: why,
            link: None,
        },
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

/// The environment a phone's session is in. Only this desktop's own: a
/// server's sessions belong to that server, and a phone paired with the
/// desktop has no right to read or drive them through it.
fn local_env_of_session(projects: &[crate::model::Project], id: &str) -> Option<PathBuf> {
    projects
        .iter()
        .flat_map(|p| p.environments.iter())
        .filter(|e| e.remote.is_none())
        .find(|e| e.session_index(id).is_some())
        .map(|e| e.path.clone())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Agent, Environment, Remote, Session};

    #[test]
    fn a_phone_finds_no_session_of_a_server() {
        let mut project = state::new_project(Path::new("/code/app"));
        let mut local = Environment::new(PathBuf::from("/code/app"), None, true);
        local
            .sessions
            .push(Session::new("mine".into(), "".into(), Agent::ALL[0]));
        let mut remote = Environment::new(Remote::env_path("srv", "/srv/app"), None, true);
        remote.remote = Some(Remote {
            server: "srv".into(),
            machine: "box".into(),
            place: "/srv/app".into(),
        });
        remote
            .sessions
            .push(Session::new("theirs".into(), "".into(), Agent::ALL[0]));
        project.environments = vec![local, remote];
        let projects = [project];
        assert_eq!(
            local_env_of_session(&projects, "mine"),
            Some(PathBuf::from("/code/app"))
        );
        assert_eq!(local_env_of_session(&projects, "theirs"), None);
    }
}
