//! What the Teams tab shows and does, past sign-in: this device's keys and
//! registration, the user's teams, invites and approvals, the team keys of
//! every epoch, and the channels with their live messages.
//!
//! Every call to the server runs on a background thread; this entity holds the
//! results and the keys. The server only stores and forwards: every team key
//! arrives in an envelope signed by an admin's device, and every message is
//! checked against its sender's device before it shows.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::Arc;
use std::time::{Duration, Instant};

use futures::StreamExt as _;
use futures::channel::mpsc;
use futures::future::{FutureExt as _, Shared};
use gpui::{AppContext as _, Context, Entity, Task};

use super::Teams;
use super::api::{
    ApiError, Call, DeviceRow, Endorsement, InviteRow, Me, MemberRow, PendingRow, Role, Stored,
    TeamDetail, TeamRow,
};
use super::crypto::{self, Body, Envelope, TeamKey, WireMessage};
use super::socket::{self, Dial, Link};
use super::store::{self, Kept, Local, Sealer};

/// How often a device that waits for an endorsement asks again.
const ENDORSE_POLL: Duration = Duration::from_secs(5);
/// How often the teams, invites, approvals and keys are read again while
/// the app runs: nothing else tells this device about them.
const REFRESH: Duration = Duration::from_secs(20);
/// A team's device lists are read again at most this often for an unknown sender.
const DEVICES_AGAIN: Duration = Duration::from_secs(5);
/// What a device code starts with, so a paste of something else is told apart.
const CODE_PREFIX: &str = "arakun-device";
const MAX_ENVELOPES: usize = 500;

/// Where this device stands with the server.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DeviceState {
    /// Reading the keyring and registering.
    Starting,
    Registered,
    /// Another device of this user is registered; one of them has to vouch
    /// for this one first. `code` carries this device's public keys there.
    Unendorsed {
        code: String,
        id: String,
    },
    Failed(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Load {
    Loading,
    Ready,
    /// The server could not be reached or answered with an error.
    Unreachable(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChannelInfo {
    pub id: String,
    /// `None` while this device has no key for the epoch it is sealed with.
    pub name: Option<String>,
}

pub struct TeamState {
    pub detail: TeamDetail,
    pub role: Role,
    /// Each member's devices whose endorsement chain checks out.
    pub devices: HashMap<u64, Vec<DeviceRow>>,
    /// Members with a device whose chain does not check out.
    pub untrusted: Vec<String>,
    pub channels: Vec<ChannelInfo>,
    pub pending: Vec<PendingRow>,
    /// This device holds no key of the team yet: an admin's device has to
    /// seal one to it.
    pub keyless: bool,
    pub problem: Option<String>,
    devices_read: Instant,
}

impl TeamState {
    /// The member a device belongs to, with that device.
    fn sender(&self, device_id: &str) -> Option<(u64, &DeviceRow)> {
        self.devices.iter().find_map(|(gid, ds)| {
            ds.iter()
                .find(|d| d.device_id == device_id)
                .map(|d| (*gid, d))
        })
    }

    pub fn login(&self, github_id: u64) -> String {
        self.detail
            .members
            .iter()
            .find(|m| m.github_id == github_id)
            .map(|m| m.login.clone())
            .unwrap_or_else(|| "someone".into())
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum Delivery {
    Sent,
    Sending,
    Failed(String),
}

#[derive(Clone, Debug, PartialEq)]
pub struct Msg {
    pub id: String,
    pub seq: Option<u64>,
    pub at: u64,
    pub github_id: u64,
    pub login: String,
    pub body: Body,
    pub delivery: Delivery,
}

impl ChannelState {
    /// Messages sealed with a key this device does not hold.
    pub fn locked(&self) -> usize {
        self.locked.len()
    }
}

pub struct ChannelState {
    pub team_id: String,
    pub channel_id: String,
    pub messages: Vec<Msg>,
    history: Cursor,
    link_id: u64,
    history_round: u64,
    reading: bool,
    needs_history: bool,
    /// Messages whose signature or sender device did not check out: dropped,
    /// as PROTOCOL.md says, and counted.
    pub hidden: usize,
    /// Messages sealed with a key this device does not hold yet, kept to
    /// open once it arrives.
    locked: Vec<Stored>,
    pub status: socket::Status,
    pub loading: bool,
    /// Messages from a device not known yet, kept until the device lists
    /// are read again.
    unknown: Vec<Stored>,
    /// What was sent and not acknowledged yet, by message ID.
    outbox: BTreeMap<String, String>,
    link: Option<Link>,
    seen_ids: HashSet<String>,
}

/// Only ordered history may cross a retention gap. A later live frame
/// must not acknowledge the frames the socket missed.
#[derive(Default)]
struct Cursor {
    through: u64,
    newest: u64,
}

impl Cursor {
    fn live(&mut self, seq: u64) {
        self.newest = self.newest.max(seq);
        if seq == self.through.saturating_add(1) {
            self.through = seq;
        }
    }

    fn page(&mut self, last: u64) {
        self.through = self.through.max(last);
    }

    fn gap(&self) -> bool {
        self.through < self.newest
    }
}

/// "@rik added a new device".
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Notice {
    pub team_id: String,
    pub login: String,
    pub device: String,
    pub device_id: String,
}

/// A device code, read: what approving it registers.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeviceCode {
    pub sig_pub: String,
    pub enc_pub: String,
    pub name: String,
    pub id: String,
}

/// The server, the session and this device, to send to a thread.
#[derive(Clone)]
struct Net {
    base: String,
    session: String,
    local: Arc<Local>,
}

impl Net {
    fn call(&self) -> Call<'_> {
        Call {
            base: &self.base,
            session: &self.session,
            device: Some(self.local.device.id()),
        }
    }
}

pub struct Hub {
    teams_entity: Entity<Teams>,
    me: Option<Me>,
    net: Option<Net>,
    sealer: Option<Sealer>,
    kept: Kept,
    pub device: DeviceState,
    pub load: Load,
    pub teams: Vec<TeamRow>,
    pub invites: Vec<InviteRow>,
    pub team: HashMap<String, TeamState>,
    pub channels: HashMap<String, ChannelState>,
    pub notices: Vec<Notice>,
    /// This user's devices, for the Devices pane.
    pub my_devices: Vec<DeviceRow>,
    /// What went wrong with the last thing asked, until the next.
    pub error: Option<String>,
    /// What the last thing asked did, when it is worth a word.
    pub info: Option<String>,
    /// Actions under way, by a key such as `approve:<team>:<id>`.
    pub busy: HashSet<String>,
    events: socket::Events,
    /// Bumped when the account changes, so late answers for the last one are dropped.
    generation: u64,
    roster_version: u64,
    applied_roster: u64,
    membership: HashMap<String, u64>,
    /// Teams this device created whose `#general` is not made yet, with the
    /// channel ID to make it with. A retry reuses the ID, so a create that
    /// went through but lost its answer is not made twice.
    setup: HashMap<String, String>,
    next_link: u64,
    _events: Task<()>,
    save_task: Option<Shared<Task<()>>>,
    _poll: Option<Task<()>>,
    _tick: Option<Task<()>>,
}

pub fn channel_key(team_id: &str, channel_id: &str) -> String {
    format!("{team_id}/{channel_id}")
}

impl Hub {
    pub fn new(teams: Entity<Teams>, cx: &mut Context<Self>) -> Self {
        let (tx, mut rx) = mpsc::unbounded::<(String, socket::Event)>();
        let events = cx.spawn(async move |this, cx| {
            while let Some((key, ev)) = rx.next().await {
                if this
                    .update(cx, |hub, cx| hub.on_socket(key, ev, cx))
                    .is_err()
                {
                    break;
                }
            }
        });
        cx.observe(&teams, |hub, _, cx| hub.account_changed(cx))
            .detach();
        let mut hub = Self {
            teams_entity: teams,
            me: None,
            net: None,
            sealer: None,
            kept: Kept::default(),
            device: DeviceState::Starting,
            load: Load::Loading,
            teams: Vec::new(),
            invites: Vec::new(),
            team: HashMap::new(),
            channels: HashMap::new(),
            notices: Vec::new(),
            my_devices: Vec::new(),
            error: None,
            busy: HashSet::new(),
            events: tx,
            generation: 0,
            roster_version: 0,
            applied_roster: 0,
            membership: HashMap::new(),
            setup: HashMap::new(),
            next_link: 0,
            _events: events,
            save_task: None,
            _poll: None,
            _tick: None,
            info: None,
        };
        hub.account_changed(cx);
        hub
    }

    pub fn me(&self) -> Option<&Me> {
        self.me.as_ref()
    }

    pub fn device_id(&self) -> Option<&str> {
        self.net.as_ref().map(|n| n.local.device.id())
    }

    /// Who is signed in changed: start over for the new account, or stop.
    fn account_changed(&mut self, cx: &mut Context<Self>) {
        let signed = self.teams_entity.read(cx).signed();
        let same = match (&signed, &self.net, &self.me) {
            (Some((base, session, me)), Some(net), Some(was)) => {
                *base == net.base && *session == net.session && me.github_id == was.github_id
            }
            // `net` is still None while the device is loading. Signing out
            // then must invalidate the in-flight setup as well.
            (None, None, None) => true,
            _ => false,
        };
        if same {
            return;
        }
        self.generation += 1;
        self.net = None;
        self.sealer = None;
        self.kept = Kept::default();
        self.teams.clear();
        self.membership.clear();
        self.setup.clear();
        self.invites.clear();
        self.team.clear();
        self.channels.clear();
        self.notices.clear();
        self.my_devices.clear();
        self.error = None;
        self.info = None;
        self.busy.clear();
        self._poll = None;
        self._tick = None;
        self.info = None;
        self.device = DeviceState::Starting;
        self.load = Load::Loading;
        self.me = signed.as_ref().map(|(_, _, me)| me.clone());
        cx.notify();
        let Some((base, session, me)) = signed else {
            return;
        };
        if super::fixture().is_some() {
            self.device = DeviceState::Failed(
                "ARAKUN_TEAMS_FIXTURE is set: there is no Teams server to talk to.".into(),
            );
            return;
        }
        let generation = self.generation;
        // A second sign-in can supersede this one. Both must wait for the
        // pending write; taking the task would let the second read overtake it.
        let pending_save = self.save_task.clone();
        cx.spawn(async move |this, cx| {
            let started = cx
                .background_spawn(store::after_save(pending_save, move || {
                    let local = store::load_device(&base, me.github_id, || {
                        let call = Call {
                            base: &base,
                            session: &session,
                            device: None,
                        };
                        let rows = call.devices(me.github_id).map_err(|e| e.to_string())?;
                        let trusted = trusted_devices(me.github_id, &rows);
                        if trusted.len() != rows.len() {
                            return Err("Could not verify this account's existing devices.".into());
                        }
                        Ok(trusted)
                    })?;
                    let kept = local.read()?;
                    Ok::<_, String>((
                        Net {
                            base,
                            session,
                            local: Arc::new(local),
                        },
                        kept,
                    ))
                }))
                .await;
            let _ = this.update(cx, |hub, cx| {
                if hub.generation != generation {
                    return;
                }
                match started {
                    Ok((net, kept)) => {
                        hub.sealer = Some(net.local.sealer());
                        hub.kept = kept;
                        hub.net = Some(net);
                        hub.register(cx);
                    }
                    Err(e) => {
                        hub.device = DeviceState::Failed(e);
                        cx.notify();
                    }
                }
            });
        })
        .detach();
    }

    /// Runs `work` with the server on a thread, then `done` here, unless the
    /// account changed in between. A 401 ends the session.
    fn run<T: Send + 'static>(
        &mut self,
        cx: &mut Context<Self>,
        work: impl FnOnce(&Net) -> Result<T, ApiError> + Send + 'static,
        done: impl FnOnce(&mut Self, Result<T, ApiError>, &mut Context<Self>) + 'static,
    ) {
        let Some(net) = self.net.clone() else {
            return;
        };
        let generation = self.generation;
        cx.spawn(async move |this, cx| {
            let result = cx.background_spawn(async move { work(&net) }).await;
            let _ = this.update(cx, |hub, cx| {
                if hub.generation != generation {
                    return;
                }
                if result.as_ref().err() == Some(&ApiError::Unauthorized) {
                    hub.teams_entity.update(cx, |t, cx| t.session_ended(cx));
                    return;
                }
                done(hub, result, cx);
                cx.notify();
            });
        })
        .detach();
    }

    fn save(&mut self, cx: &mut Context<Self>) {
        if let Some(sealer) = self.sealer.clone() {
            let kept = self.kept.clone();
            let prior = self.save_task.take();
            self.save_task = Some(
                cx.background_spawn(store::after_save(prior, move || sealer.write(&kept)))
                    .shared(),
            );
        }
    }

    fn github_id(&self) -> u64 {
        self.me.as_ref().map_or(0, |m| m.github_id)
    }

    // ---- this device ----------------------------------------------------------

    /// `POST /devices` without an endorsement: the first device of a user, or
    /// one already registered (also by another device on its behalf).
    fn register(&mut self, cx: &mut Context<Self>) {
        let name = device_name();
        self.run(
            cx,
            move |net| {
                let d = &net.local.device;
                net.call()
                    .register_device(d.sig_pub(), d.enc_pub(), &name, None)
            },
            |hub, result, cx| match result {
                Ok(_) => {
                    hub.device = DeviceState::Registered;
                    hub._poll = None;
                    hub.refresh(cx);
                    hub.tick(cx);
                }
                // The server wants an endorsement: the user has a device already.
                Err(ApiError::Status(400, msg)) if msg.contains("endorsement") => {
                    let Some(net) = &hub.net else { return };
                    let d = &net.local.device;
                    hub.device = DeviceState::Unendorsed {
                        code: device_code(d.sig_pub(), d.enc_pub(), &device_name()),
                        id: d.id().to_string(),
                    };
                    hub.poll(ENDORSE_POLL, cx, |hub, cx| hub.register(cx));
                }
                Err(e) => {
                    hub.load = Load::Unreachable(e.to_string());
                    hub.device = DeviceState::Failed(e.to_string());
                }
            },
        );
    }

    /// Calls `again` once after `wait`, unless something else replaced it.
    fn poll(
        &mut self,
        wait: Duration,
        cx: &mut Context<Self>,
        again: impl FnOnce(&mut Self, &mut Context<Self>) + 'static,
    ) {
        self._poll = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(wait).await;
            let _ = this.update(cx, |hub, cx| again(hub, cx));
        }));
    }

    /// Reads everything again every [`REFRESH`], for as long as this account
    /// stays signed in.
    fn tick(&mut self, cx: &mut Context<Self>) {
        self._tick = Some(cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(REFRESH).await;
                if this.update(cx, |hub, cx| hub.refresh(cx)).is_err() {
                    return;
                }
            }
        }));
    }

    /// Tries again after the server could not be reached.
    pub fn retry(&mut self, cx: &mut Context<Self>) {
        self.error = None;
        self.info = None;
        match &self.device {
            DeviceState::Registered => self.refresh(cx),
            DeviceState::Failed(_) if self.net.is_none() => {
                self.net = None;
                self.me = None;
                self.account_changed(cx);
            }
            _ => self.register(cx),
        }
        self.load = Load::Loading;
        cx.notify();
    }

    /// This user's devices, for the Devices pane.
    pub fn load_my_devices(&mut self, cx: &mut Context<Self>) {
        let me = self.github_id();
        self.run(
            cx,
            move |net| net.call().devices(me),
            |hub, result, _| match result {
                Ok(rows) => hub.my_devices = rows,
                Err(e) => hub.error = Some(e.to_string()),
            },
        );
    }

    /// Vouches for another device of this user, from the code it shows, and
    /// registers it on its behalf: it is in from its next try.
    pub fn approve_device(&mut self, code: &DeviceCode, cx: &mut Context<Self>) {
        let busy = format!("device:{}", code.id);
        if !self.busy.insert(busy.clone()) {
            return;
        }
        self.error = None;
        self.info = None;
        let me = self.github_id();
        let code = code.clone();
        let name = code.name.clone();
        self.run(
            cx,
            move |net| {
                let d = &net.local.device;
                let endorsement = Endorsement {
                    by: d.id().to_string(),
                    sig: d.endorse(me, &code.sig_pub, &code.enc_pub),
                };
                net.call().register_device(
                    &code.sig_pub,
                    &code.enc_pub,
                    &code.name,
                    Some(endorsement),
                )?;
                net.call().devices(me)
            },
            move |hub, result, cx| {
                hub.busy.remove(&busy);
                match result {
                    Ok(rows) => {
                        hub.info = Some(format!(
                            "{name} can use Teams now. Your teams' admins give it the keys next."
                        ));
                        hub.my_devices = rows;
                        hub.refresh(cx);
                    }
                    Err(e) => hub.error = Some(e.to_string()),
                }
            },
        );
    }

    // ---- teams ----------------------------------------------------------------

    /// The user's teams and invites, then each team in full.
    pub fn refresh(&mut self, cx: &mut Context<Self>) {
        if self.device != DeviceState::Registered {
            return;
        }
        let retry: Vec<_> = self
            .channels
            .iter()
            .filter(|(_, c)| c.needs_history)
            .map(|(key, _)| key.clone())
            .collect();
        for key in retry {
            self.catch_up(&key, cx);
        }
        self.roster_version += 1;
        let version = self.roster_version;
        self.run(
            cx,
            |net| Ok((net.call().teams()?, net.call().invites()?)),
            move |hub, result, cx| match result {
                _ if version < hub.applied_roster => {}
                Ok((teams, invites)) => {
                    hub.applied_roster = version;
                    hub.load = Load::Ready;
                    let ids: HashSet<&str> = teams.iter().map(|t| t.team_id.as_str()).collect();
                    let asked: Vec<String> = hub
                        .kept
                        .asked()
                        .keys()
                        .filter(|t| ids.contains(t.as_str()))
                        .cloned()
                        .collect();
                    for t in &asked {
                        if let Some(name) = hub.kept.asked().get(t) {
                            hub.info = Some(format!("You're in {name} now."));
                        }
                        hub.kept.forget_ask(t);
                    }
                    let gone: Vec<String> = hub
                        .membership
                        .keys()
                        .filter(|t| !ids.contains(t.as_str()))
                        .cloned()
                        .collect();
                    for t in gone {
                        if let Some(was) = hub.team.get(&t) {
                            hub.info = Some(format!("You're no longer in {}.", was.detail.name));
                        }
                        hub.team.remove(&t);
                        hub.setup.remove(&t);
                        hub.notices.retain(|n| n.team_id != t);
                        hub.channels.retain(|_, c| c.team_id != t);
                        hub.kept.forget_team(&t);
                    }
                    hub.kept.retain_teams(&ids);
                    hub.save(cx);
                    update_memberships(&mut hub.membership, &hub.teams, &teams, version);
                    hub.teams = teams;
                    hub.invites = invites;
                    let ids: Vec<String> = hub.teams.iter().map(|t| t.team_id.clone()).collect();
                    for id in ids {
                        hub.load_team(&id, cx);
                    }
                }
                Err(e) => hub.load = Load::Unreachable(e.to_string()),
            },
        );
    }

    /// Teams this user asked to join and is waiting on, with their names.
    pub fn asked(&self) -> Vec<(String, String)> {
        self.kept
            .asked()
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect()
    }

    /// One team: its members and their devices, its channels, this device's
    /// keys, and the admin's duties (a pending rotation, new devices).
    pub fn load_team(&mut self, team_id: &str, cx: &mut Context<Self>) {
        let Some(row) = self.teams.iter().find(|t| t.team_id == team_id).cloned() else {
            return;
        };
        let Some(version) = self.membership.get(team_id).copied() else {
            return;
        };
        let busy = format!("load:{team_id}:{version}");
        if !self.busy.insert(busy.clone()) {
            return;
        }
        let held: HashSet<u64> = self.kept.keys(team_id).iter().map(|(e, _)| *e).collect();
        let team_id = team_id.to_string();
        let admin = row.role.is_admin();
        self.run(
            cx,
            {
                let team_id = team_id.clone();
                move |net| read_team(net, &team_id, admin, &held)
            },
            move |hub, result, cx| {
                hub.busy.remove(&busy);
                if hub.membership.get(&team_id) != Some(&version) {
                    return;
                }
                match result {
                    Ok(read) => hub.team_read(&team_id, row.role, read, cx),
                    Err(ApiError::Status(404, _)) => hub.refresh(cx),
                    Err(e) => {
                        if let Some(t) = hub.team.get_mut(&team_id) {
                            t.problem = Some(e.to_string());
                        } else {
                            hub.load = Load::Unreachable(e.to_string());
                        }
                    }
                }
            },
        );
    }

    fn team_read(&mut self, team_id: &str, role: Role, read: TeamRead, cx: &mut Context<Self>) {
        let mut changed = false;
        for (&gid, devices) in &read.devices {
            let known = self.kept.roots.contains_key(&gid);
            if !devices.is_empty() && !pin_directory(&mut self.kept, gid, devices) {
                self.team.remove(team_id);
                self.channels.retain(|_, c| c.team_id != team_id);
                self.error = Some(format!(
                    "The trusted device for account {gid} changed. Key delivery is paused."
                ));
                return;
            }
            changed |= !known && !devices.is_empty();
        }
        for (epoch, key) in &read.keys {
            changed |= self.kept.put_key(team_id, *epoch, key);
        }
        changed |= self.kept.observe_grants(&read.detail);
        // New devices of members seen before; the first look at a member
        // only learns what they have.
        let mine = self.device_id().unwrap_or_default().to_string();
        for m in &read.detail.members {
            let Some(devices) = read.devices.get(&m.github_id) else {
                continue;
            };
            if let Some(seen) = self.kept.seen(m.github_id) {
                for d in devices {
                    if !seen.contains(&d.device_id)
                        && d.device_id != mine
                        && !self.notices.iter().any(|n| n.device_id == d.device_id)
                    {
                        self.notices.push(Notice {
                            team_id: team_id.to_string(),
                            login: m.login.clone(),
                            device: d.name.clone(),
                            device_id: d.device_id.clone(),
                        });
                    }
                }
            }
            changed |= self
                .kept
                .see(m.github_id, devices.iter().map(|d| d.device_id.clone()));
        }
        if changed {
            self.save(cx);
        }
        let channels = read
            .detail
            .channels
            .iter()
            .map(|c| ChannelInfo {
                id: c.channel_id.clone(),
                name: self.kept.key(team_id, c.name_ct.epoch).and_then(|k| {
                    crypto::open_channel_name(team_id, &c.channel_id, &k, &c.name_ct).ok()
                }),
            })
            .collect();
        let keyless = self.kept.keys(team_id).is_empty();
        let problem = (read.bad_envelopes > 0).then(|| {
            format!(
                "{} team key{} for this device did not check out and {} ignored.",
                read.bad_envelopes,
                if read.bad_envelopes == 1 { "" } else { "s" },
                if read.bad_envelopes == 1 {
                    "was"
                } else {
                    "were"
                },
            )
        });
        let rotation = read.detail.rotation_pending;
        self.team.insert(
            team_id.to_string(),
            TeamState {
                detail: read.detail,
                role,
                devices: read.devices,
                untrusted: read.untrusted,
                channels,
                pending: read.pending,
                keyless,
                problem,
                devices_read: Instant::now(),
            },
        );
        if let Some(row) = self.teams.iter_mut().find(|t| t.team_id == team_id) {
            row.rotation_pending = rotation;
        }
        // Messages that waited on a device list, and any sealed with a key
        // that just arrived, get another look.
        let keys: Vec<String> = self
            .channels
            .iter()
            .filter(|(_, c)| c.team_id == team_id)
            .map(|(k, _)| k.clone())
            .collect();
        for key in keys {
            if let Some(c) = self.channels.get_mut(&key) {
                let mut waiting = std::mem::take(&mut c.unknown);
                waiting.append(&mut c.locked);
                self.ingest(&key, waiting, true, cx);
            }
        }
        if self.setup.contains_key(team_id) {
            let has_channels = self
                .team
                .get(team_id)
                .is_some_and(|t| !t.detail.channels.is_empty());
            if has_channels {
                self.setup.remove(team_id);
            } else if let Some((epoch, key)) = self.sending_key(team_id) {
                self.finish_setup(team_id, epoch, key, cx);
            }
        }
        if role.is_admin() {
            if rotation {
                self.rotate(team_id, cx);
            } else {
                self.seal_for_new_devices(team_id, cx);
                self.reseal_names(team_id, cx);
            }
        }
    }

    pub fn create_team(&mut self, name: String, cx: &mut Context<Self>) {
        if !self.busy.insert("create-team".into()) {
            return;
        }
        self.error = None;
        self.info = None;
        let me = self.github_id();
        let pinned = self.kept.roots.get(&me).cloned();
        self.run(
            cx,
            move |net| {
                let call = net.call();
                let rows = call.devices(me)?;
                let mine = trusted_devices(me, &rows);
                if mine.len() != rows.len()
                    || !mine.iter().any(|d| {
                        d.device_id == net.local.device.id()
                            && d.enc_pub == net.local.device.enc_pub()
                    })
                    || pinned
                        .as_ref()
                        .is_some_and(|p| root_of(&mine).as_ref() != Some(p))
                {
                    return Err(ApiError::Status(
                        0,
                        "The device directory does not match this device.".into(),
                    ));
                }
                Ok((call.create_team(&name)?, mine))
            },
            move |hub, result, cx| match result {
                Ok((team_id, mine)) => {
                    hub.setup.insert(team_id.clone(), crypto::ulid());
                    hub.first_key(team_id, me, mine, cx)
                }
                Err(e) => {
                    hub.busy.remove("create-team");
                    hub.error = Some(format!("Could not create the team: {e}"));
                }
            },
        );
    }

    fn first_key(&mut self, team: String, me: u64, mine: Vec<DeviceRow>, cx: &mut Context<Self>) {
        // A roster read started before POST /teams may omit the new team.
        // Don't let it erase a receipt or look like a later departure.
        self.roster_version += 1;
        let roster = self.roster_version;
        self.applied_roster = roster;
        let key = crypto::new_team_key();
        self.run(
            cx,
            {
                let team = team.clone();
                move |net| {
                    let batch = seal_all(net, &team, &[(1, key)], &mine)?;
                    net.call().put_envelopes(&team, 0, 1, &batch)?;
                    Ok(batch)
                }
            },
            move |hub, result, cx| {
                let batch = match result {
                    Ok(batch) => batch,
                    // Another upload made the key first, such as this device's
                    // rotation from a refresh that saw the new team. The
                    // refresh reads that key, and `team_read` finishes setup.
                    Err(ApiError::Status(409, _)) => {
                        hub.busy.remove("create-team");
                        hub.refresh(cx);
                        return;
                    }
                    // The team exists without a key. The next refresh rotates
                    // it, then `team_read` finishes setup.
                    Err(e) => {
                        hub.busy.remove("create-team");
                        hub.error = Some(format!("Could not make the team's first key: {e}"));
                        hub.refresh(cx);
                        return;
                    }
                };
                if hub.applied_roster != roster
                    && !hub
                        .teams
                        .iter()
                        .any(|t| t.team_id == team && t.role == Role::Owner && t.epoch == 1)
                {
                    hub.busy.remove("create-team");
                    return;
                }
                hub.kept.put_key(&team, 1, &key);
                hub.kept.admitted(&team, me, 1, &batch);
                hub.save(cx);
                // The first key is already live even if channel creation fails.
                hub.finish_setup(&team, 1, key, cx);
            },
        );
    }

    /// Makes `#general` for a team this device created, with the channel ID
    /// kept for it. A failure is tried again when the team is read next.
    fn finish_setup(&mut self, team_id: &str, epoch: u64, key: TeamKey, cx: &mut Context<Self>) {
        let Some(channel) = self.setup.get(team_id).cloned() else {
            self.busy.remove("create-team");
            return;
        };
        let busy = format!("general:{team_id}");
        if !self.busy.insert(busy.clone()) {
            return;
        }
        let team = team_id.to_string();
        self.run(
            cx,
            {
                let team = team.clone();
                move |net| {
                    let name = crypto::seal_channel_name(&team, &channel, epoch, &key, "general");
                    net.call().create_channel(&team, &channel, &name)
                }
            },
            move |hub, result, cx| {
                hub.busy.remove(&busy);
                hub.busy.remove("create-team");
                match result {
                    Ok(()) => {
                        hub.setup.remove(&team);
                        hub.refresh(cx);
                    }
                    // The ID is taken by an earlier try whose answer was
                    // lost, or the key changed. Reading the team tells which.
                    Err(ApiError::Status(409, _)) => hub.refresh(cx),
                    Err(e) => {
                        hub.error = Some(format!("Could not create the team's first channel: {e}"));
                    }
                }
            },
        );
    }

    /// A new key for the team's current epoch, sealed to every device of
    /// every member: after a removal, or the first key of a team.
    fn rotate(&mut self, team_id: &str, cx: &mut Context<Self>) {
        let Some(t) = self.team.get(team_id) else {
            return;
        };
        let Some(version) = self.membership.get(team_id).copied() else {
            return;
        };
        let busy = format!("rotate:{team_id}:{version}");
        if !t.untrusted.is_empty() {
            let who = t.untrusted.join(", @");
            if let Some(t) = self.team.get_mut(team_id) {
                t.problem = Some(format!(
                    "The team needs a new key, but a device of @{who} does not check out, so none was made."
                ));
            }
            return;
        }
        if !self.busy.insert(busy.clone()) {
            return;
        }
        let epoch = t.detail.epoch;
        let keyed_epoch = t.detail.keyed_epoch.unwrap_or(epoch.saturating_sub(1));
        let team_id = team_id.to_string();
        let key = crypto::new_team_key();
        let batch: Vec<_> = recipients(t)
            .map(|(m, d)| KeyDelivery::new(m, d, epoch, key))
            .collect();
        self.run(
            cx,
            {
                let team_id = team_id.clone();
                move |net| {
                    upload_batch(net, &team_id, batch, |envelopes| {
                        net.call()
                            .put_envelopes(&team_id, keyed_epoch, epoch, envelopes)
                    })
                }
            },
            move |hub, result, cx| {
                hub.busy.remove(&busy);
                if hub.membership.get(&team_id) != Some(&version) {
                    return;
                }
                match result {
                    Ok(batch) => {
                        hub.kept.put_key(&team_id, epoch, &key);
                        record_batch(&mut hub.kept, &team_id, &batch);
                        hub.save(cx);
                        hub.load_team(&team_id, cx);
                    }
                    Err(e) => {
                        if let Some(t) = hub.team.get_mut(&team_id) {
                            t.problem = Some(format!("Could not make the team's new key: {e}"));
                        }
                    }
                }
            },
        );
    }

    /// Devices members added after they joined have no keys until an admin
    /// seals the epochs granted to that membership, once per device and epoch.
    fn seal_for_new_devices(&mut self, team_id: &str, cx: &mut Context<Self>) {
        let Some(t) = self.team.get(team_id) else {
            return;
        };
        let Some(version) = self.membership.get(team_id).copied() else {
            return;
        };
        let keyed_epoch = t.detail.keyed_epoch.unwrap_or(t.detail.epoch);
        let mine = self.device_id().unwrap_or_default();
        let batch = next_backfill(&self.kept, t, mine);
        if t.detail.roster_epoch != Some(keyed_epoch) {
            if let Some(t) = self.team.get_mut(team_id) {
                t.problem =
                    Some("Update the Teams server to deliver historical keys safely.".into());
            }
            return;
        }
        if batch.is_empty() {
            return;
        }
        let busy = format!("seal:{team_id}:{version}");
        if !self.busy.insert(busy.clone()) {
            return;
        }
        let team_id = team_id.to_string();
        self.run(
            cx,
            {
                let team_id = team_id.clone();
                move |net| {
                    upload_batch(net, &team_id, batch, |envelopes| {
                        net.call()
                            .put_envelopes(&team_id, keyed_epoch, keyed_epoch, envelopes)
                    })
                }
            },
            move |hub, result, cx| {
                hub.busy.remove(&busy);
                if hub.membership.get(&team_id) != Some(&version) {
                    return;
                }
                match result {
                    Ok(batch) => {
                        record_batch(&mut hub.kept, &team_id, &batch);
                        hub.save(cx);
                        // Re-read membership before planning another batch.
                        if batch.len() == MAX_ENVELOPES {
                            hub.load_team(&team_id, cx);
                        }
                    }
                    Err(e) => {
                        if let Some(t) = hub.team.get_mut(&team_id) {
                            t.problem =
                                Some(format!("Could not give a new device the team's keys: {e}"));
                        }
                    }
                }
            },
        );
    }

    pub fn invite(&mut self, team_id: &str, login: String, cx: &mut Context<Self>) {
        let busy = format!("invite:{team_id}");
        if !self.busy.insert(busy.clone()) {
            return;
        }
        self.error = None;
        self.info = None;
        let team_id = team_id.to_string();
        self.run(
            cx,
            move |net| net.call().invite(&team_id, &login).map(|()| login),
            move |hub, result, _| {
                hub.busy.remove(&busy);
                match result {
                    Ok(login) => {
                        hub.info = Some(format!(
                            "Invited @{login}. They'll see it in their Teams tab."
                        ))
                    }
                    Err(ApiError::Status(409, _)) => {
                        hub.error =
                            Some("They are a member already, or waiting for approval.".into())
                    }
                    Err(e) => hub.error = Some(format!("Could not invite: {e}")),
                }
            },
        );
    }

    pub fn accept(&mut self, invite: &InviteRow, cx: &mut Context<Self>) {
        let busy = format!("accept:{}", invite.invite_id);
        if !self.busy.insert(busy.clone()) {
            return;
        }
        self.error = None;
        self.info = None;
        let invite = invite.clone();
        self.run(
            cx,
            {
                let id = invite.invite_id.clone();
                move |net| net.call().accept(&id)
            },
            move |hub, result, cx| {
                hub.busy.remove(&busy);
                match result {
                    Ok(()) => {
                        hub.info = Some(format!(
                            "You asked to join {}. An admin approves you next.",
                            invite.team_name
                        ));
                        hub.kept.ask(&invite.team_id, &invite.team_name);
                        hub.save(cx);
                        hub.refresh(cx);
                    }
                    Err(e) => hub.error = Some(format!("Could not accept: {e}")),
                }
            },
        );
    }

    /// Lets a pending member in: their devices must all check out, and get
    /// every key this device holds from the oldest it can give without a gap.
    pub fn approve(
        &mut self,
        team_id: &str,
        github_id: u64,
        request_id: &str,
        cx: &mut Context<Self>,
    ) {
        let Some(t) = self.team.get(team_id) else {
            return;
        };
        let Some(version) = self.membership.get(team_id).copied() else {
            return;
        };
        let team_epoch = t.detail.epoch;
        let Some(p) = t
            .pending
            .iter()
            .find(|p| p.github_id == github_id && p.request_id == request_id)
            .cloned()
        else {
            return;
        };
        let trusted = trusted_devices(github_id, &p.devices);
        if trusted.len() != p.devices.len() || !pin_directory(&mut self.kept, github_id, &trusted) {
            self.error = Some(format!(
                "A device of @{} does not check out, so they can't be approved from here.",
                p.login
            ));
            cx.notify();
            return;
        }
        self.save(cx);
        let keys = contiguous_tail(&self.kept.keys(team_id));
        if keys.is_empty() {
            self.error = Some("This device holds no key of the team yet.".into());
            cx.notify();
            return;
        }
        let busy = format!("approve:{team_id}:{github_id}");
        if !self.busy.insert(busy.clone()) {
            return;
        }
        self.error = None;
        self.info = None;
        let team_id = team_id.to_string();
        self.run(
            cx,
            {
                let team_id = team_id.clone();
                move |net| {
                    let envelopes = seal_all(net, &team_id, &keys, &trusted)?;
                    net.call()
                        .approve(&team_id, github_id, &p.request_id, &envelopes)?;
                    Ok(envelopes)
                }
            },
            move |hub, result, cx| {
                hub.busy.remove(&busy);
                if hub.membership.get(&team_id) != Some(&version)
                    || !hub
                        .team
                        .get(&team_id)
                        .is_some_and(|t| t.detail.epoch == team_epoch)
                {
                    return;
                }
                match result {
                    Ok(batch) => {
                        hub.kept.admitted(&team_id, github_id, team_epoch, &batch);
                        hub.save(cx);
                        hub.load_team(&team_id, cx);
                    }
                    Err(e) => hub.error = Some(format!("Could not approve: {e}")),
                }
            },
        );
    }

    pub fn reject(
        &mut self,
        team_id: &str,
        github_id: u64,
        request_id: String,
        cx: &mut Context<Self>,
    ) {
        let team_id = team_id.to_string();
        self.run(
            cx,
            {
                let team_id = team_id.clone();
                move |net| net.call().reject(&team_id, github_id, &request_id)
            },
            move |hub, result, cx| match result {
                Ok(()) => hub.load_team(&team_id, cx),
                Err(e) => hub.error = Some(format!("Could not turn the request down: {e}")),
            },
        );
    }

    /// Removes a member (an admin), or leaves (anyone, with their own ID).
    pub fn remove(&mut self, team_id: &str, github_id: u64, cx: &mut Context<Self>) {
        let team_id = team_id.to_string();
        self.run(
            cx,
            {
                let team_id = team_id.clone();
                move |net| net.call().remove_member(&team_id, github_id)
            },
            move |hub, result, cx| match result {
                Ok(()) => hub.refresh(cx),
                Err(ApiError::Status(409, _)) => {
                    hub.error = Some("The last owner can't leave the team.".into())
                }
                Err(e) => hub.error = Some(format!("Could not remove: {e}")),
            },
        );
    }

    /// Do not encrypt a new message with a key still held by a removed member.
    fn sending_key(&self, team_id: &str) -> Option<(u64, TeamKey)> {
        let t = self.team.get(team_id)?;
        let (epoch, key) = self.kept.newest(team_id)?;
        (epoch == t.detail.epoch && !t.detail.rotation_pending).then_some((epoch, key))
    }

    pub fn create_channel(&mut self, team_id: &str, name: String, cx: &mut Context<Self>) {
        let Some((epoch, key)) = self.sending_key(team_id) else {
            self.error = Some("This device does not hold the team's current key yet.".into());
            cx.notify();
            return;
        };
        let busy = format!("channel:{team_id}");
        if !self.busy.insert(busy.clone()) {
            return;
        }
        self.error = None;
        self.info = None;
        let team_id = team_id.to_string();
        self.run(
            cx,
            {
                let team_id = team_id.clone();
                move |net| {
                    let channel_id = crypto::ulid();
                    let sealed =
                        crypto::seal_channel_name(&team_id, &channel_id, epoch, &key, &name);
                    net.call().create_channel(&team_id, &channel_id, &sealed)?;
                    Ok(channel_id)
                }
            },
            move |hub, result, cx| {
                hub.busy.remove(&busy);
                match result {
                    Ok(_) => hub.load_team(&team_id, cx),
                    Err(e) => hub.error = Some(format!("Could not create the channel: {e}")),
                }
            },
        );
    }

    /// Seals the channel names still sealed with an older key again with the
    /// newest one, so a member approved without the older epochs can read
    /// them. Names this device cannot open stay as they are. A failure is
    /// tried again the next time the team is read.
    fn reseal_names(&mut self, team_id: &str, cx: &mut Context<Self>) {
        let Some(t) = self.team.get(team_id) else {
            return;
        };
        let Some((epoch, key)) = self.sending_key(team_id) else {
            return;
        };
        let stale = stale_names(&t.detail, &t.channels, epoch);
        if stale.is_empty() {
            return;
        }
        let Some(version) = self.membership.get(team_id).copied() else {
            return;
        };
        let busy = format!("names:{team_id}:{version}");
        if !self.busy.insert(busy.clone()) {
            return;
        }
        let team_id = team_id.to_string();
        self.run(
            cx,
            {
                let team_id = team_id.clone();
                move |net| {
                    for (channel, name) in &stale {
                        let sealed =
                            crypto::seal_channel_name(&team_id, channel, epoch, &key, name);
                        net.call().rename_channel(&team_id, channel, &sealed)?;
                    }
                    Ok(())
                }
            },
            move |hub, result, cx| {
                hub.busy.remove(&busy);
                if hub.membership.get(&team_id) != Some(&version) {
                    return;
                }
                match result {
                    Ok(()) => hub.load_team(&team_id, cx),
                    // A newer key: the next read seals with it.
                    Err(ApiError::Status(409, _)) => {}
                    Err(e) => {
                        if let Some(t) = hub.team.get_mut(&team_id) {
                            t.problem = Some(format!(
                                "Could not seal the channel names with the team's new key: {e}"
                            ));
                        }
                    }
                }
            },
        );
    }

    pub fn dismiss_notice(&mut self, device_id: &str, cx: &mut Context<Self>) {
        self.notices.retain(|n| n.device_id != device_id);
        cx.notify();
    }

    // ---- channels ---------------------------------------------------------------

    /// Opens a channel's socket, once; its history comes when it connects.
    pub fn open_channel(&mut self, team_id: &str, channel_id: &str, cx: &mut Context<Self>) {
        let key = channel_key(team_id, channel_id);
        if self.channels.get(&key).is_some_and(|c| c.link.is_some()) {
            return;
        }
        let Some(net) = self.net.clone() else {
            return;
        };
        if !self.membership.contains_key(team_id) {
            return;
        }
        self.next_link += 1;
        let link_id = self.next_link;
        let link = Link::open(
            format!("{link_id}:{key}"),
            Dial {
                url: super::api::socket_url(&net.base, team_id, channel_id),
                session: net.session.clone(),
                device: net.local.device.id().to_string(),
            },
            self.events.clone(),
        );
        let state = self.channels.entry(key).or_insert_with(|| ChannelState {
            team_id: team_id.to_string(),
            channel_id: channel_id.to_string(),
            messages: Vec::new(),
            history: Cursor::default(),
            link_id,
            history_round: 0,
            reading: false,
            needs_history: false,
            hidden: 0,
            locked: Vec::new(),
            status: socket::Status::Connecting,
            loading: true,
            unknown: Vec::new(),
            outbox: BTreeMap::new(),
            link: None,
            seen_ids: HashSet::new(),
        });
        state.link_id = link_id;
        state.link = Some(link);
        cx.notify();
    }

    fn on_socket(&mut self, source: String, ev: socket::Event, cx: &mut Context<Self>) {
        let Some((id, key)) = source.split_once(':') else {
            return;
        };
        let Some(c) = self.channels.get_mut(key) else {
            return;
        };
        if id.parse::<u64>().ok() != Some(c.link_id) || c.link.is_none() {
            return;
        }
        match ev {
            socket::Event::Status(status) => {
                let connected = status == socket::Status::Connected;
                let refused = matches!(status, socket::Status::Refused(_));
                c.history_round += 1;
                c.reading = false;
                c.status = status;
                if refused {
                    c.link = None;
                    c.loading = false;
                    self.refresh(cx);
                } else if connected {
                    // What was missed while away, then what was not acknowledged.
                    c.needs_history = true;
                    self.catch_up(key, cx);
                }
            }
            socket::Event::Frame(v) => match v["type"].as_str() {
                Some("message") => {
                    if let Ok(stored) = serde_json::from_value::<Stored>(v) {
                        c.history.live(stored.seq);
                        let first_gap = c.history.gap() && !c.needs_history;
                        c.needs_history |= first_gap;
                        self.ingest(key, vec![stored], false, cx);
                        if first_gap {
                            self.catch_up(key, cx);
                        }
                    }
                }
                Some("error") => {
                    let id = v["id"].as_str().unwrap_or_default().to_string();
                    let why = v["error"].as_str().unwrap_or("refused").to_string();
                    let c = self.channels.get_mut(key).expect("looked up above");
                    // The reply to a replay may arrive after catch-up finished.
                    if why.contains("exists") {
                        c.outbox.remove(&id);
                        c.needs_history = true;
                        self.catch_up(key, cx);
                    } else if let Some(m) = c.messages.iter_mut().find(|m| m.id == id) {
                        c.outbox.remove(&id);
                        m.delivery = Delivery::Failed(why.clone());
                        if why.contains("epoch") {
                            let team = c.team_id.clone();
                            self.load_team(&team, cx);
                        }
                    }
                }
                _ => {}
            },
        }
        cx.notify();
    }

    /// Reads the history after the last gap-free sequence, page by page, then
    /// sends again what is still unacknowledged.
    fn catch_up(&mut self, key: &str, cx: &mut Context<Self>) {
        let Some(c) = self.channels.get_mut(key) else {
            return;
        };
        if c.reading || c.status != socket::Status::Connected {
            return;
        }
        c.reading = true;
        let (link_id, round) = (c.link_id, c.history_round);
        let (team_id, channel_id, after) =
            (c.team_id.clone(), c.channel_id.clone(), c.history.through);
        let key = key.to_string();
        self.run(
            cx,
            move |net| history_since(after, |seq| net.call().history(&team_id, &channel_id, seq)),
            move |hub, result, cx| {
                let Some(c) = hub.channels.get_mut(&key) else {
                    return;
                };
                if c.link_id != link_id || c.history_round != round {
                    return;
                }
                c.reading = false;
                c.loading = false;
                match result {
                    Ok(stored) => {
                        if after != 0
                            && stored
                                .first()
                                .is_some_and(|s| s.seq > after.saturating_add(1))
                        {
                            hub.info = Some(
                                "Some older messages are no longer in this channel's history."
                                    .into(),
                            );
                        }
                        if let Some(last) = stored.last() {
                            c.history.page(last.seq);
                        }
                        c.needs_history = c.history.gap();
                        hub.ingest(&key, stored, false, cx);
                        let c = hub.channels.get_mut(&key).expect("looked up above");
                        if !c.needs_history
                            && let Some(link) = &c.link
                        {
                            for frame in c.outbox.values() {
                                link.send(frame.clone());
                            }
                        }
                    }
                    Err(e) => {
                        c.needs_history = true;
                        hub.error = Some(format!("Could not read the channel: {e}"));
                    }
                }
            },
        );
    }

    /// Checks and opens stored messages, and files them by `seq`. Ones from
    /// a device not known yet wait for the team's device lists to be read
    /// again; once they were just read (`fresh`), those are dropped.
    fn ingest(&mut self, key: &str, stored: Vec<Stored>, fresh: bool, cx: &mut Context<Self>) {
        let Some(c) = self.channels.get_mut(key) else {
            return;
        };
        let Some(t) = self.team.get(&c.team_id) else {
            c.unknown.extend(stored);
            return;
        };
        let mut unknown_seen = false;
        for s in stored {
            if c.seen_ids.contains(&s.message.id) {
                continue;
            }
            let Some((gid, device)) = t.sender(&s.message.device_id) else {
                if fresh {
                    c.hidden += 1;
                } else {
                    unknown_seen = true;
                    c.unknown.push(s);
                }
                continue;
            };
            let Some(sig_pub) = crypto::unb64(&device.sig_pub) else {
                c.hidden += 1;
                continue;
            };
            if !crypto::verify_message(&c.team_id, &c.channel_id, &s.message, &sig_pub) {
                c.hidden += 1;
                continue;
            }
            let Some(team_key) = self.kept.key(&c.team_id, s.message.epoch) else {
                c.locked.push(s);
                continue;
            };
            let Ok(body) = crypto::open_message(&c.team_id, &c.channel_id, &s.message, &team_key)
            else {
                c.hidden += 1;
                continue;
            };
            c.outbox.remove(&s.message.id);
            c.seen_ids.insert(s.message.id.clone());
            let msg = Msg {
                id: s.message.id.clone(),
                seq: Some(s.seq),
                at: s.at,
                github_id: gid,
                login: t.login(gid),
                body,
                delivery: Delivery::Sent,
            };
            match c.messages.iter().position(|m| m.id == msg.id) {
                Some(i) => c.messages[i] = msg,
                None => c.messages.push(msg),
            }
        }
        // Sent ones by seq; what is still on its way stays at the end.
        c.messages
            .sort_by_key(|m| (m.seq.is_none(), m.seq.unwrap_or(0), m.at));
        let team_id = c.team_id.clone();
        if unknown_seen
            && self
                .team
                .get(&team_id)
                .is_some_and(|t| t.devices_read.elapsed() >= DEVICES_AGAIN)
        {
            if let Some(t) = self.team.get_mut(&team_id) {
                t.devices_read = Instant::now();
            }
            self.load_team(&team_id, cx);
        }
    }

    /// Sends a text message, replying to `reply_to` when given.
    pub fn send(
        &mut self,
        team_id: &str,
        channel_id: &str,
        text: String,
        reply_to: Option<String>,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some((epoch, key)) = self.sending_key(team_id) else {
            self.error = Some("This device does not hold the team's current key yet.".into());
            self.load_team(team_id, cx);
            cx.notify();
            return false;
        };
        let Some(net) = self.net.clone() else {
            return false;
        };
        let ckey = channel_key(team_id, channel_id);
        let Some(c) = self.channels.get_mut(&ckey) else {
            return false;
        };
        let id = crypto::ulid();
        let body = Body::text(text, reply_to);
        let wire: WireMessage = crypto::seal_message(
            team_id,
            channel_id,
            &id,
            epoch,
            &key,
            &net.local.device,
            &body,
        );
        let frame = serde_json::json!({ "type": "send", "message": wire }).to_string();
        let me = self.me.clone();
        c.messages.push(Msg {
            id: id.clone(),
            seq: None,
            at: now(),
            github_id: me.as_ref().map_or(0, |m| m.github_id),
            login: me.map(|m| m.login).unwrap_or_default(),
            body,
            delivery: Delivery::Sending,
        });
        c.outbox.insert(id, frame.clone());
        if c.status == socket::Status::Connected
            && let Some(link) = &c.link
        {
            link.send(frame);
        }
        cx.notify();
        true
    }

    /// Seals a failed message again (with the key the team has now) and sends it.
    pub fn resend(&mut self, team_id: &str, channel_id: &str, id: &str, cx: &mut Context<Self>) {
        let ckey = channel_key(team_id, channel_id);
        let Some(c) = self.channels.get_mut(&ckey) else {
            return;
        };
        let Some(i) = c.messages.iter().position(|m| m.id == id) else {
            return;
        };
        let m = c.messages.remove(i);
        self.send(team_id, channel_id, m.body.text, m.body.reply_to, cx);
    }
}

fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// The name this device registers with: the machine's.
fn device_name() -> String {
    let name: String = crate::phone::host_name()
        .chars()
        .filter(|c| !c.is_control())
        .take(100)
        .collect();
    if name.trim().is_empty() {
        "Arakun".into()
    } else {
        name.trim().to_string()
    }
}

/// What a device shows to be approved from another: its two public keys and
/// its name, in one line to copy.
pub fn device_code(sig_pub: &str, enc_pub: &str, name: &str) -> String {
    format!("{CODE_PREFIX}.{sig_pub}.{enc_pub}.{}", crypto::b64(name))
}

pub fn read_device_code(code: &str) -> Option<DeviceCode> {
    let mut parts = code.trim().split('.');
    if parts.next()? != CODE_PREFIX {
        return None;
    }
    let (sig_pub, enc_pub, name) = (parts.next()?, parts.next()?, parts.next()?);
    if parts.next().is_some() {
        return None;
    }
    let raw = crypto::unb64(sig_pub).filter(|p| p.len() == 65 && p[0] == 4)?;
    crypto::unb64(enc_pub).filter(|p| p.len() == 65 && p[0] == 4)?;
    let name = String::from_utf8(crypto::unb64(name)?).ok()?;
    Some(DeviceCode {
        sig_pub: sig_pub.into(),
        enc_pub: enc_pub.into(),
        id: crypto::device_id_of(&raw),
        name,
    })
}

/// A user's devices whose endorsement chain checks out, in the order given.
///
/// Each device's ID must be the one its `sig` key derives, and both keys
/// P-256 points. The first device (the oldest without an endorsement) is the
/// root; every other must carry an endorsement signed by a trusted device.
pub fn trusted_devices(gid: u64, rows: &[DeviceRow]) -> Vec<DeviceRow> {
    let mut ids = HashSet::new();
    if rows.iter().any(|d| !ids.insert(&d.device_id)) {
        return Vec::new();
    }
    let point = |s: &str| {
        crypto::unb64(s)
            .filter(|p| p.len() == 65 && p[0] == 4 && p256::PublicKey::from_sec1_bytes(p).is_ok())
    };
    let sound: Vec<&DeviceRow> = rows
        .iter()
        .filter(|d| point(&d.enc_pub).is_some())
        .filter(|d| point(&d.sig_pub).is_some_and(|p| crypto::device_id_of(&p) == d.device_id))
        .collect();
    let Some(root) = sound
        .iter()
        .filter(|d| d.endorsed_by.is_none())
        .min_by(|a, b| (a.created_at, &a.device_id).cmp(&(b.created_at, &b.device_id)))
    else {
        return Vec::new();
    };
    let mut trusted: HashSet<&str> = HashSet::from([root.device_id.as_str()]);
    loop {
        let before = trusted.len();
        for d in &sound {
            if let Some(by) = &d.endorsed_by
                && trusted.contains(by.as_str())
                && !trusted.contains(d.device_id.as_str())
                && let Some(sig) = &d.endorsement_sig
                && let Some(parent) = sound.iter().find(|p| &p.device_id == by)
                && let Some(key) = point(&parent.sig_pub)
                && crypto::verify(
                    &key,
                    &crypto::device_signed(gid, &d.sig_pub, &d.enc_pub),
                    sig,
                )
            {
                trusted.insert(d.device_id.as_str());
            }
        }
        if trusted.len() == before {
            break;
        }
    }
    rows.iter()
        .filter(|d| trusted.contains(d.device_id.as_str()))
        .cloned()
        .collect()
}

fn history_since(
    mut after: u64,
    mut read: impl FnMut(u64) -> Result<Vec<Stored>, ApiError>,
) -> Result<Vec<Stored>, ApiError> {
    let mut all = Vec::new();
    loop {
        let page = read(after)?;
        let full = page.len() >= 200;
        for item in &page {
            if item.seq <= after {
                return Err(ApiError::Status(
                    0,
                    "Channel history did not advance.".into(),
                ));
            }
            after = item.seq;
        }
        all.extend(page);
        if !full {
            return Ok(all);
        }
    }
}

fn update_memberships(
    current: &mut HashMap<String, u64>,
    old: &[TeamRow],
    new: &[TeamRow],
    version: u64,
) {
    current.retain(|id, _| new.iter().any(|r| &r.team_id == id));
    for row in new {
        if !old.contains(row) {
            current.insert(row.team_id.clone(), version);
        }
    }
}

fn root_of(rows: &[DeviceRow]) -> Option<(String, String)> {
    let root = rows.iter().find(|d| d.endorsed_by.is_none())?;
    Some((root.device_id.clone(), root.enc_pub.clone()))
}

fn pin_directory(kept: &mut Kept, gid: u64, trusted: &[DeviceRow]) -> bool {
    let Some(root) = root_of(trusted) else {
        return false;
    };
    kept.roots.entry(gid).or_insert_with(|| root.clone()) == &root
}

/// The channels whose name is sealed with an epoch older than `epoch`, with
/// the name this device read. One it could not open is left out.
fn stale_names(detail: &TeamDetail, read: &[ChannelInfo], epoch: u64) -> Vec<(String, String)> {
    detail
        .channels
        .iter()
        .filter(|c| c.name_ct.epoch < epoch)
        .filter_map(|c| {
            let name = read.iter().find(|r| r.id == c.channel_id)?.name.clone()?;
            Some((c.channel_id.clone(), name))
        })
        .collect()
}

/// The keys from the newest back to the first gap: a skipped epoch has no
/// envelopes, and an approval must cover every epoch from its first.
fn contiguous_tail(keys: &[(u64, TeamKey)]) -> Vec<(u64, TeamKey)> {
    let mut out: Vec<(u64, TeamKey)> = Vec::new();
    for (epoch, key) in keys.iter().rev() {
        if out.last().is_some_and(|(e, _)| *e != epoch + 1) {
            break;
        }
        out.push((*epoch, *key));
    }
    out.reverse();
    out
}

fn recipients(t: &TeamState) -> impl Iterator<Item = (&MemberRow, &DeviceRow)> {
    t.detail.members.iter().flat_map(|m| {
        t.devices
            .get(&m.github_id)
            .into_iter()
            .flatten()
            .map(move |d| (m, d))
    })
}

struct KeyDelivery {
    github_id: u64,
    joined_at: u64,
    device: DeviceRow,
    epoch: u64,
    key: TeamKey,
}

impl KeyDelivery {
    fn new(m: &MemberRow, d: &DeviceRow, epoch: u64, key: TeamKey) -> Self {
        Self {
            github_id: m.github_id,
            joined_at: m.joined_at,
            device: d.clone(),
            epoch,
            key,
        }
    }
}

fn next_backfill(kept: &Kept, t: &TeamState, mine: &str) -> Vec<KeyDelivery> {
    let team = &t.detail.team_id;
    let keys = kept.keys(team);
    let mut batch = Vec::new();
    for (m, d) in recipients(t) {
        if d.device_id == mine || d.created_at < m.joined_at {
            continue;
        }
        for &(epoch, key) in &keys {
            if epoch <= t.detail.epoch
                && m.first_key_epoch.is_some_and(|first| epoch >= first)
                && kept.needs_delivery(team, m.github_id, m.joined_at, &d.device_id, epoch)
            {
                batch.push(KeyDelivery::new(m, d, epoch, key));
                if batch.len() == MAX_ENVELOPES {
                    return batch;
                }
            }
        }
    }
    batch
}

fn upload_batch(
    net: &Net,
    team_id: &str,
    batch: Vec<KeyDelivery>,
    put: impl FnOnce(&[Envelope]) -> Result<bool, ApiError>,
) -> Result<Vec<KeyDelivery>, ApiError> {
    let envelopes = batch
        .iter()
        .map(|s| seal_one(net, team_id, s.epoch, &s.key, &s.device))
        .collect::<Result<Vec<_>, _>>()?;
    if put(&envelopes)? {
        return Err(ApiError::Status(
            409,
            "The team changed during key delivery.".into(),
        ));
    }
    Ok(batch)
}

fn record_batch(kept: &mut Kept, team: &str, batch: &[KeyDelivery]) {
    for s in batch {
        kept.delivered(team, s.github_id, s.joined_at, &s.device.device_id, s.epoch);
    }
}

fn seal_one(
    net: &Net,
    team_id: &str,
    epoch: u64,
    key: &TeamKey,
    d: &DeviceRow,
) -> Result<Envelope, ApiError> {
    crypto::seal_envelope(
        team_id,
        epoch,
        key,
        &d.device_id,
        &d.enc_pub,
        &net.local.device,
    )
    .map_err(|e| ApiError::Status(0, format!("device {}: {e}", d.device_id)))
}

/// One envelope per device and key.
fn seal_all(
    net: &Net,
    team_id: &str,
    keys: &[(u64, TeamKey)],
    devices: &[DeviceRow],
) -> Result<Vec<Envelope>, ApiError> {
    let mut out = Vec::with_capacity(keys.len() * devices.len());
    for d in devices {
        for (epoch, key) in keys {
            out.push(seal_one(net, team_id, *epoch, key, d)?);
        }
    }
    Ok(out)
}

/// What reading one team gave.
struct TeamRead {
    detail: TeamDetail,
    devices: HashMap<u64, Vec<DeviceRow>>,
    untrusted: Vec<String>,
    /// Keys from envelopes this device did not hold yet, checked.
    keys: Vec<(u64, TeamKey)>,
    bad_envelopes: usize,
    pending: Vec<PendingRow>,
}

fn read_team(
    net: &Net,
    team_id: &str,
    admin: bool,
    held: &HashSet<u64>,
) -> Result<TeamRead, ApiError> {
    let call = net.call();
    let detail = call.team(team_id)?;
    let mut devices = HashMap::new();
    let mut untrusted = Vec::new();
    for m in &detail.members {
        let rows = call.devices(m.github_id)?;
        let trusted = trusted_devices(m.github_id, &rows);
        if trusted.len() != rows.len() {
            untrusted.push(m.login.clone());
        }
        devices.insert(m.github_id, trusted);
    }
    // Only an admin's device may hand out a team key.
    let admins: HashMap<&str, &DeviceRow> = detail
        .members
        .iter()
        .filter(|m| m.role.is_admin())
        .filter_map(|m| devices.get(&m.github_id))
        .flatten()
        .map(|d| (d.device_id.as_str(), d))
        .collect();
    let mut keys = Vec::new();
    let mut bad = 0;
    for e in call.envelopes(team_id)? {
        if held.contains(&e.epoch) {
            continue;
        }
        let opened = admins
            .get(e.from.as_str())
            .and_then(|d| crypto::unb64(&d.sig_pub))
            .ok_or(crypto::CryptoError::BadSignature)
            .and_then(|sig_pub| crypto::open_envelope(&e, team_id, &net.local.device, &sig_pub));
        match opened {
            Ok(key) => keys.push((e.epoch, key)),
            Err(_) => bad += 1,
        }
    }
    let pending = if admin {
        call.pending(team_id)?
    } else {
        Vec::new()
    };
    Ok(TeamRead {
        detail,
        devices,
        untrusted,
        keys,
        bad_envelopes: bad,
        pending,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crypto::Device;

    fn row(d: &Device, created_at: u64, endorsed_by: Option<&Device>) -> DeviceRow {
        DeviceRow {
            device_id: d.id().into(),
            sig_pub: d.sig_pub().into(),
            enc_pub: d.enc_pub().into(),
            name: "laptop".into(),
            created_at,
            endorsed_by: endorsed_by.map(|e| e.id().to_string()),
            endorsement_sig: endorsed_by.map(|e| e.endorse(21, d.sig_pub(), d.enc_pub())),
        }
    }

    #[test]
    fn trusts_the_chain_from_the_first_device() {
        let (a, b, c) = (Device::generate(), Device::generate(), Device::generate());
        let rows = vec![row(&a, 1, None), row(&b, 2, Some(&a)), row(&c, 3, Some(&b))];
        assert_eq!(trusted_devices(21, &rows).len(), 3);
        assert_eq!(trusted_devices(22, &rows).len(), 1);
        // An endorser nobody trusts, and a second root, are left out.
        let (x, y) = (Device::generate(), Device::generate());
        let rows = vec![row(&a, 1, None), row(&x, 2, Some(&y)), row(&y, 3, None)];
        let ids: Vec<String> = trusted_devices(21, &rows)
            .into_iter()
            .map(|d| d.device_id)
            .collect();
        assert_eq!(ids, vec![a.id().to_string()]);
        // A row whose ID is not its key's.
        let mut forged = row(&b, 2, Some(&a));
        forged.sig_pub = c.sig_pub().into();
        assert_eq!(trusted_devices(21, &[row(&a, 1, None), forged]).len(), 1);
        assert!(trusted_devices(21, &[]).is_empty());
    }

    #[test]
    fn known_root_binds_both_keys_across_refreshes() {
        let (a, b) = (Device::generate(), Device::generate());
        let mut kept = Kept::default();
        let original = row(&a, 1, None);
        assert!(pin_directory(
            &mut kept,
            21,
            std::slice::from_ref(&original)
        ));
        let mut kept: Kept = serde_json::from_slice(&serde_json::to_vec(&kept).unwrap()).unwrap();
        assert!(!pin_directory(&mut kept, 21, &[row(&b, 2, None)]));
        let changed = DeviceRow {
            enc_pub: b.enc_pub().into(),
            ..original.clone()
        };
        assert!(!pin_directory(&mut kept, 21, &[changed]));
        assert!(pin_directory(&mut kept, 21, &[original]));
    }

    #[test]
    fn late_read_loses_its_membership_even_if_the_user_rejoins() {
        let rows = [TeamRow {
            team_id: "t".into(),
            name: "t".into(),
            role: Role::Admin,
            epoch: 1,
            rotation_pending: false,
        }];
        let mut membership = HashMap::new();
        update_memberships(&mut membership, &[], &rows, 1);
        let pending = membership["t"];
        update_memberships(&mut membership, &rows, &rows, 2);
        assert_eq!(membership.get("t"), Some(&pending));
        update_memberships(&mut membership, &rows, &[], 3);
        assert!(!membership.contains_key("t"));
        update_memberships(&mut membership, &[], &rows, 4);
        assert_ne!(membership.get("t"), Some(&pending));
    }

    #[test]
    fn live_message_does_not_acknowledge_missing_history() {
        let mut c = Cursor::default();
        c.page(40);
        c.live(43);
        assert!(c.gap());
        assert_eq!(c.through, 40);
        c.page(42);
        assert!(c.gap());
        c.page(43);
        c.live(44);
        assert_eq!(c.through, 44);
        assert!(!c.gap());
    }

    #[test]
    fn device_codes_round_trip() {
        let d = Device::generate();
        let code = device_code(d.sig_pub(), d.enc_pub(), "karlo's laptop");
        let read = read_device_code(&format!("  {code}\n")).unwrap();
        assert_eq!(read.id, d.id());
        assert_eq!(read.name, "karlo's laptop");
        assert!(read_device_code("arakun-device.x.y.z").is_none());
        assert!(read_device_code(&code.replace("arakun-device", "other")).is_none());
        assert!(read_device_code(&format!("{code}.more")).is_none());
    }

    #[test]
    fn approvals_give_keys_back_to_the_first_gap() {
        let k = |e: u64| (e, [e as u8; 32]);
        assert_eq!(contiguous_tail(&[k(1), k(2), k(3)]), vec![k(1), k(2), k(3)]);
        assert_eq!(contiguous_tail(&[k(1), k(2), k(4), k(5)]), vec![k(4), k(5)]);
        assert!(contiguous_tail(&[]).is_empty());
    }

    #[test]
    fn only_readable_names_under_an_older_key_are_sealed_again() {
        let channel = |id: &str, epoch: u64| super::super::api::ChannelRow {
            channel_id: id.into(),
            name_ct: crypto::seal_channel_name("T", id, epoch, &[epoch as u8; 32], "x"),
            created_at: 1,
        };
        let mut t = delivery_team(vec![], 3);
        t.detail.channels = vec![channel("A", 1), channel("B", 2), channel("C", 3)];
        let read = [
            ChannelInfo {
                id: "A".into(),
                name: Some("general".into()),
            },
            // Sealed with a key this device never got.
            ChannelInfo {
                id: "B".into(),
                name: None,
            },
            ChannelInfo {
                id: "C".into(),
                name: Some("ideas".into()),
            },
        ];
        assert_eq!(
            stale_names(&t.detail, &read, 3),
            vec![("A".to_string(), "general".to_string())]
        );
        assert!(stale_names(&t.detail, &read, 1).is_empty());
    }

    fn delivery_team(devices: Vec<DeviceRow>, epoch: u64) -> TeamState {
        TeamState {
            detail: TeamDetail {
                team_id: "T".into(),
                name: "test".into(),
                epoch,
                keyed_epoch: Some(epoch),
                roster_epoch: Some(epoch),
                rotation_pending: false,
                members: vec![MemberRow {
                    github_id: 21,
                    login: "member".into(),
                    role: Role::Member,
                    joined_at: 10,
                    first_key_epoch: Some(epoch),
                }],
                channels: vec![],
            },
            role: Role::Owner,
            devices: HashMap::from([(21, devices)]),
            untrusted: vec![],
            channels: vec![],
            pending: vec![],
            keyless: false,
            problem: None,
            devices_read: Instant::now(),
        }
    }

    fn offline_net() -> Net {
        Net {
            base: String::new(),
            session: String::new(),
            local: Arc::new(Local::for_test(Device::generate())),
        }
    }

    #[test]
    fn an_unconfirmed_approval_cannot_attach_to_a_later_membership() {
        let net = offline_net();
        let device = row(&Device::generate(), 1, None);
        let approved = seal_all(&net, "T", &[(1, [1; 32]), (2, [2; 32])], &[device]).unwrap();
        let mut pending = Kept::default();
        pending.admitted("T", 21, 2, &approved);
        let mut t = delivery_team(vec![], 2).detail;
        let members = std::mem::take(&mut t.members);
        pending.observe_grants(&t); // A read started before the approval.
        t.members = members;
        let mut same = pending.clone();
        same.observe_grants(&t);
        assert!(same.needs_delivery("T", 21, 10, "new", 1));
        t.epoch = 3;
        t.keyed_epoch = Some(3);
        pending.observe_grants(&t);
        assert!(!pending.needs_delivery("T", 21, 10, "new", 1));
        assert!(pending.needs_delivery("T", 21, 10, "new", 3));
    }

    #[test]
    fn rotation_leaves_only_the_approved_history_to_backfill() {
        let net = offline_net();
        let root = Device::generate();
        let old = row(&root, 10, None);
        // Device and membership timestamps have only second precision.
        let new = row(&Device::generate(), 10, Some(&root));
        let mut t = delivery_team(vec![old.clone()], 4);
        t.detail.members[0].first_key_epoch = Some(3);
        let mut kept = Kept::default();
        let held = [(1, [1; 32]), (3, [3; 32]), (4, [4; 32])];
        let approved = seal_all(&net, "T", &contiguous_tail(&held), &[old]).unwrap();
        kept.admitted("T", 21, 4, &approved);
        kept.observe_grants(&t.detail);
        assert!(!kept.needs_delivery("T", 21, 10, root.id(), 3));
        assert!(!kept.needs_delivery("T", 21, 10, root.id(), 4));
        for (e, key) in held {
            kept.put_key("T", e, &key);
        }
        assert!(next_backfill(&kept, &t, "").is_empty());
        t.devices.get_mut(&21).unwrap().push(new.clone());
        t.detail.epoch = 5;
        t.detail.rotation_pending = true;
        kept.observe_grants(&t.detail);
        let rotation = recipients(&t)
            .map(|(m, d)| KeyDelivery::new(m, d, 5, [5; 32]))
            .collect();
        let sent = upload_batch(&net, "T", rotation, |envs| {
            assert_eq!(envs.len(), 2);
            assert!(envs.iter().all(|e| e.epoch == 5));
            Ok(false)
        })
        .unwrap();
        record_batch(&mut kept, "T", &sent);
        kept.put_key("T", 5, &[5; 32]);
        // Learning an omitted key later must not broaden this approval.
        kept.put_key("T", 2, &[2; 32]);
        t.detail.keyed_epoch = Some(5);
        t.detail.rotation_pending = false;
        let missing = next_backfill(&kept, &t, "");
        assert_eq!(
            missing.iter().map(|s| s.epoch).collect::<Vec<_>>(),
            vec![3, 4]
        );
        assert!(missing.iter().all(|s| s.device.device_id == new.device_id));
        assert!(next_backfill(&kept, &t, &new.device_id).is_empty());
        record_batch(&mut kept, "T", &missing);
        assert!(next_backfill(&kept, &t, "").is_empty());

        // This user was removed and approved again while we were offline.
        t.detail.members[0].first_key_epoch = Some(6);
        t.detail.members[0].joined_at = 30;
        t.detail.epoch = 6;
        t.detail.keyed_epoch = Some(6);
        kept.observe_grants(&t.detail);
        let latest = row(&Device::generate(), 40, Some(&root));
        t.devices.get_mut(&21).unwrap().push(latest.clone());
        kept.put_key("T", 6, &[6; 32]);
        kept.delivered("T", 21, 10, &latest.device_id, 6);
        let missing = next_backfill(&kept, &t, "");
        assert_eq!(missing.len(), 1);
        assert_eq!(
            (missing[0].epoch, &missing[0].device.device_id),
            (6, &latest.device_id)
        );
    }

    #[test]
    fn a_failed_backfill_resumes_after_the_last_accepted_batch() {
        let net = offline_net();
        let root = Device::generate();
        let creator = row(&root, 10, None);
        let first = seal_all(&net, "T", &[(1, [1; 32])], std::slice::from_ref(&creator)).unwrap();
        let mut kept = Kept::default();
        kept.admitted("T", 21, 1, &first);
        let mut t = delivery_team(vec![creator], 1);
        kept.observe_grants(&t.detail);
        kept.put_key("T", 1, &[1; 32]);
        assert!(!kept.needs_delivery("T", 21, 10, root.id(), 1));
        assert!(next_backfill(&kept, &t, "").is_empty());

        let last = MAX_ENVELOPES as u64 + 1;
        t.detail.epoch = last;
        t.detail.keyed_epoch = Some(last);
        t.devices
            .get_mut(&21)
            .unwrap()
            .push(row(&Device::generate(), 20, Some(&root)));
        kept.observe_grants(&t.detail);
        for e in 2..=last {
            kept.put_key("T", e, &[0; 32]);
        }
        let batch = next_backfill(&kept, &t, root.id());
        assert_eq!(batch.len(), MAX_ENVELOPES);
        let accepted = upload_batch(&net, "T", batch, |envs| {
            assert_eq!(envs.len(), MAX_ENVELOPES);
            Ok(false)
        })
        .unwrap();
        record_batch(&mut kept, "T", &accepted);
        let saved = serde_json::to_vec(&kept).unwrap();
        let batch = next_backfill(&kept, &t, root.id());
        assert!(
            upload_batch(&net, "T", batch, |envs| {
                assert_eq!(envs.len(), 1);
                assert_eq!(envs[0].epoch, last);
                Err(ApiError::Status(503, "unavailable".into()))
            })
            .is_err()
        );
        let mut restarted = serde_json::from_slice(&saved).unwrap();
        let batch = next_backfill(&restarted, &t, root.id());
        assert_eq!(batch.len(), 1);
        assert_eq!(batch[0].epoch, last);
        let accepted = upload_batch(&net, "T", batch, |_| Ok(false)).unwrap();
        record_batch(&mut restarted, "T", &accepted);
        assert!(next_backfill(&restarted, &t, root.id()).is_empty());
    }

    /// Two members against a real Teams Worker, through the same calls and
    /// crypto the hub makes: create a team with its first key and a channel,
    /// invite, accept, approve with sealed keys, chat both ways over the
    /// sockets with a reply, read the history, then remove and rotate.
    ///
    /// Needs a server and two fresh users with the `teams` flag and no
    /// devices yet (see the Teams section of teams/README.md for running one
    /// locally):
    ///
    /// ```sh
    /// ARAKUN_TEAMS_E2E="http://localhost:8787 <session A> <session B> <login B>" \
    ///   cargo test --no-default-features against_a_server -- --ignored
    /// ```
    #[test]
    #[ignore]
    fn plays_two_members_against_a_server() {
        let env = std::env::var("ARAKUN_TEAMS_E2E").expect("set ARAKUN_TEAMS_E2E");
        let parts: Vec<&str> = env.split_whitespace().collect();
        let [base, sa, sb, login_b] = parts[..] else {
            panic!("ARAKUN_TEAMS_E2E is \"<url> <session A> <session B> <login B>\"");
        };
        let net = |session: &str| Net {
            base: base.into(),
            session: session.into(),
            local: Arc::new(Local::for_test(Device::generate())),
        };
        let (a, b) = (net(sa), net(sb));
        let me = |n: &Net| super::super::api::me(base, &n.session).unwrap();
        let (ma, mb) = (me(&a), me(&b));
        for n in [&a, &b] {
            let d = &n.local.device;
            let id = n
                .call()
                .register_device(d.sig_pub(), d.enc_pub(), "e2e", None)
                .unwrap();
            assert_eq!(id, d.id());
        }

        // A makes the team, its first key and a channel, as create_team does.
        let team = a.call().create_team("E2E").unwrap();
        let mine = trusted_devices(ma.github_id, &a.call().devices(ma.github_id).unwrap());
        let key = crypto::new_team_key();
        let envelopes = seal_all(&a, &team, &[(1, key)], &mine).unwrap();
        assert!(!a.call().put_envelopes(&team, 0, 1, &envelopes).unwrap());
        let channel = crypto::ulid();
        let sealed = crypto::seal_channel_name(&team, &channel, 1, &key, "general");
        a.call().create_channel(&team, &channel, &sealed).unwrap();

        // Invite, accept, approve.
        a.call().invite(&team, login_b).unwrap();
        let invite = b
            .call()
            .invites()
            .unwrap()
            .into_iter()
            .find(|i| i.team_id == team)
            .unwrap();
        b.call().accept(&invite.invite_id).unwrap();
        assert!(matches!(
            b.call().team(&team),
            Err(ApiError::Status(404, _))
        ));
        let pending = a.call().pending(&team).unwrap();
        let p = pending
            .iter()
            .find(|p| p.github_id == mb.github_id)
            .unwrap();
        let theirs = trusted_devices(mb.github_id, &p.devices);
        assert_eq!(theirs.len(), 1);
        let envelopes = seal_all(&a, &team, &contiguous_tail(&[(1, key)]), &theirs).unwrap();
        a.call()
            .approve(&team, mb.github_id, &p.request_id, &envelopes)
            .unwrap();

        // B reads the team as load_team does: the key, checked, and the channel.
        let read = read_team(&b, &team, false, &HashSet::new()).unwrap();
        assert_eq!(read.keys, vec![(1, key)]);
        assert_eq!(read.bad_envelopes, 0);
        assert!(read.untrusted.is_empty());
        let ch = &read.detail.channels[0];
        assert_eq!(
            crypto::open_channel_name(&team, &ch.channel_id, &key, &ch.name_ct).unwrap(),
            "general"
        );

        // Both on the channel's socket.
        let (tx, mut rx) = mpsc::unbounded();
        let dial = |n: &Net| Dial {
            url: super::super::api::socket_url(base, &team, &channel),
            session: n.session.clone(),
            device: n.local.device.id().to_string(),
        };
        let la = Link::open("a".into(), dial(&a), tx.clone());
        let lb = Link::open("b".into(), dial(&b), tx);
        let mut connected = HashSet::new();
        let mut next = || {
            let deadline = Instant::now() + Duration::from_secs(10);
            loop {
                match rx.try_recv() {
                    Ok(got) => return got,
                    Err(mpsc::TryRecvError::Closed) => panic!("the sockets hung up"),
                    Err(mpsc::TryRecvError::Empty) => {
                        assert!(Instant::now() < deadline, "nothing came in time");
                        std::thread::sleep(Duration::from_millis(20));
                    }
                }
            }
        };
        while connected.len() < 2 {
            if let (who, socket::Event::Status(socket::Status::Connected)) = next() {
                connected.insert(who);
            }
        }
        let send = |n: &Net, link: &Link, body: &Body| {
            let id = crypto::ulid();
            let m = crypto::seal_message(&team, &channel, &id, 1, &key, &n.local.device, body);
            link.send(serde_json::json!({ "type": "send", "message": m }).to_string());
            id
        };
        let mut heard = |want: &str| -> (String, Stored) {
            loop {
                if let (who, socket::Event::Frame(v)) = next() {
                    assert_ne!(v["type"], "error", "{v}");
                    if v["type"] != "message" {
                        continue;
                    }
                    let s: Stored = serde_json::from_value(v).unwrap();
                    if s.message.id == want {
                        return (who, s);
                    }
                }
            }
        };
        let first = send(&a, &la, &Body::text("hello from A", None));
        let (_, got) = heard(&first);
        let sender = read.devices[&ma.github_id]
            .iter()
            .find(|d| d.device_id == got.message.device_id)
            .unwrap();
        assert!(crypto::verify_message(
            &team,
            &channel,
            &got.message,
            &crypto::unb64(&sender.sig_pub).unwrap()
        ));
        assert_eq!(
            crypto::open_message(&team, &channel, &got.message, &key)
                .unwrap()
                .text,
            "hello from A"
        );
        let reply = send(&b, &lb, &Body::text("hi A", Some(first.clone())));
        let (_, got) = heard(&reply);
        let body = crypto::open_message(&team, &channel, &got.message, &key).unwrap();
        assert_eq!(body.reply_to.as_deref(), Some(first.as_str()));

        let history = b.call().history(&team, &channel, 0).unwrap();
        let ids: Vec<&str> = history.iter().map(|s| s.message.id.as_str()).collect();
        assert_eq!(ids, vec![first.as_str(), reply.as_str()]);
        // A late live frame and a failed history request must leave the
        // earlier review available for the next retry on this same socket.
        let mut cursor = Cursor::default();
        cursor.live(history[1].seq);
        assert!(cursor.gap());
        assert!(
            history_since(cursor.through, |_| Err(ApiError::Status(
                503,
                "offline".into()
            )))
            .is_err()
        );
        let caught = history_since(cursor.through, |after| {
            b.call().history(&team, &channel, after)
        })
        .unwrap();
        assert_eq!(caught, history);
        cursor.page(caught.last().unwrap().seq);
        assert!(!cursor.gap());

        // A second device of A, vouched for by the first from its code, as
        // approve_device does; it then registers as itself and gets the key
        // from an admin's device, as seal_for_new_devices does.
        let second = Net {
            base: base.into(),
            session: sa.into(),
            local: Arc::new(Local::for_test(Device::generate())),
        };
        let d2 = &second.local.device;
        assert!(matches!(
            second.call().register_device(d2.sig_pub(), d2.enc_pub(), "second", None),
            Err(ApiError::Status(400, m)) if m.contains("endorsement")
        ));
        let code = read_device_code(&device_code(d2.sig_pub(), d2.enc_pub(), "second")).unwrap();
        let endorsement = Endorsement {
            by: a.local.device.id().into(),
            sig: a
                .local
                .device
                .endorse(ma.github_id, &code.sig_pub, &code.enc_pub),
        };
        a.call()
            .register_device(&code.sig_pub, &code.enc_pub, &code.name, Some(endorsement))
            .unwrap();
        assert_eq!(
            second
                .call()
                .register_device(d2.sig_pub(), d2.enc_pub(), "second", None)
                .unwrap(),
            d2.id()
        );
        let both = trusted_devices(ma.github_id, &a.call().devices(ma.github_id).unwrap());
        assert_eq!(both.len(), 2);
        let fresh: Vec<DeviceRow> = both
            .into_iter()
            .filter(|d| d.device_id == d2.id())
            .collect();
        a.call()
            .put_envelopes(
                &team,
                1,
                1,
                &seal_all(&a, &team, &[(1, key)], &fresh).unwrap(),
            )
            .unwrap();
        let read2 = read_team(&second, &team, true, &HashSet::new()).unwrap();
        assert_eq!(read2.keys, vec![(1, key)]);

        // A removes B: a new key for A's devices only, and B is out.
        a.call().remove_member(&team, mb.github_id).unwrap();
        let read = read_team(&a, &team, true, &HashSet::from([1])).unwrap();
        assert!(read.detail.rotation_pending);
        assert_eq!(read.detail.epoch, 2);
        let devices: Vec<DeviceRow> = read.devices.values().flatten().cloned().collect();
        let rotated = crypto::new_team_key();
        let envelopes = seal_all(&a, &team, &[(2, rotated)], &devices).unwrap();
        assert!(!a.call().put_envelopes(&team, 1, 2, &envelopes).unwrap());
        let id = crypto::ulid();
        let message = crypto::seal_message(
            &team,
            &channel,
            &id,
            2,
            &rotated,
            &a.local.device,
            &Body::text("after rotation", None),
        );
        la.send(serde_json::json!({ "type": "send", "message": message }).to_string());
        let mut closed = false;
        let mut received = false;
        while !(closed && received) {
            match next() {
                (who, socket::Event::Status(socket::Status::Refused(_))) if who == "b" => {
                    closed = true
                }
                (who, socket::Event::Frame(v)) if v["message"]["id"] == id => {
                    assert_eq!(who, "a");
                    let stored: Stored = serde_json::from_value(v).unwrap();
                    assert!(crypto::open_message(&team, &channel, &stored.message, &key).is_err());
                    assert_eq!(
                        crypto::open_message(&team, &channel, &stored.message, &rotated)
                            .unwrap()
                            .text,
                        "after rotation"
                    );
                    received = true;
                }
                _ => {}
            }
        }
        assert!(matches!(
            b.call().team(&team),
            Err(ApiError::Status(404, _))
        ));
        assert!(matches!(
            b.call().history(&team, &channel, 0),
            Err(ApiError::Status(404, _))
        ));
    }
}
