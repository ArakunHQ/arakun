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
/// Unread counts of channels not open here are read again at most this often
/// while the tab shows, and at most every [`UNREAD_FOCUS`] when it gets focus.
const UNREAD_AGAIN: Duration = Duration::from_secs(60);
const UNREAD_FOCUS: Duration = Duration::from_secs(5);
/// What a device code starts with, so a paste of something else is told apart.
const CODE_PREFIX: &str = "arakun-device";

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
    last_seq: u64,
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
    /// Who has this channel open now, by GitHub ID, as the server last said.
    /// Empty while the socket is down: nobody is known to be here then.
    pub here: Vec<u64>,
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
    /// Invites sent from here in this run, by team: the login and when. The
    /// server has no list of a team's open invites to read back.
    pub sent: Vec<(String, String, u64)>,
    /// Who invited this user to a team, from invites accepted in this run.
    pub inviters: HashMap<String, String>,
    /// Teams an admin let this user into during this run: their chat says so.
    pub joined: HashSet<String>,
    /// @bot's runs and what this user did with its answers, on this device.
    pub bot: super::bot::Bots,
    /// The channel the tab shows, by channel key: what arrives there is read.
    viewing: Option<String>,
    /// Unread counts of channels with no socket here, from their history.
    unread_read: HashMap<String, usize>,
    /// When those were last read.
    unread_at: Option<Instant>,
    events: socket::Events,
    /// Bumped when the account changes, so late answers for the last one are dropped.
    generation: u64,
    _events: Task<()>,
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
            bot: super::bot::Bots::default(),
            viewing: None,
            unread_read: HashMap::new(),
            unread_at: None,
            events: tx,
            generation: 0,
            _events: events,
            _poll: None,
            _tick: None,
            info: None,
            sent: Vec::new(),
            inviters: HashMap::new(),
            joined: HashSet::new(),
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

    /// This device's public signing key, for comparing devices.
    pub fn sig_pub(&self) -> Option<&str> {
        self.net.as_ref().map(|n| n.local.device.sig_pub())
    }

    /// The user checked this device's compare numbers with its owner.
    pub fn verified(&self, device_id: &str) -> bool {
        self.kept.verified(device_id)
    }

    /// Marks a device checked ("They match"), or not ("They don't").
    pub fn set_verified(&mut self, device_id: &str, yes: bool, cx: &mut Context<Self>) {
        if self.kept.set_verified(device_id, yes) {
            self.save(cx);
        }
        cx.notify();
    }

    /// Who is signed in changed: start over for the new account, or stop.
    fn account_changed(&mut self, cx: &mut Context<Self>) {
        let signed = self.teams_entity.read(cx).signed();
        let same = match (&signed, &self.net, &self.me) {
            (Some((base, session, me)), Some(net), Some(was)) => {
                *base == net.base && *session == net.session && me.github_id == was.github_id
            }
            (None, None, _) => true,
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
        self.invites.clear();
        self.team.clear();
        self.channels.clear();
        self.notices.clear();
        self.my_devices.clear();
        self.error = None;
        self.info = None;
        self.busy.clear();
        self.sent.clear();
        self.inviters.clear();
        self.joined.clear();
        self.bot.clear();
        self.viewing = None;
        self.unread_read.clear();
        self.unread_at = None;
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
        cx.spawn(async move |this, cx| {
            let started = cx
                .background_spawn(async move {
                    let local = Arc::new(store::load_device()?);
                    let kept = local.read();
                    Ok::<_, String>((local, kept))
                })
                .await;
            let _ = this.update(cx, |hub, cx| {
                if hub.generation != generation {
                    return;
                }
                match started {
                    Ok((local, kept)) => {
                        hub.sealer = Some(local.sealer());
                        hub.kept = kept;
                        hub.net = Some(Net {
                            base,
                            session,
                            local,
                        });
                        let _ = me;
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

    fn save(&self, cx: &mut Context<Self>) {
        if let Some(sealer) = self.sealer.clone() {
            let kept = self.kept.clone();
            cx.background_spawn(async move { sealer.write(&kept) })
                .detach();
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
        self.run(
            cx,
            |net| Ok((net.call().teams()?, net.call().invites()?)),
            |hub, result, cx| match result {
                Ok((teams, invites)) => {
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
                        hub.joined.insert(t.clone());
                        hub.kept.forget_ask(t);
                    }
                    let gone: Vec<String> = hub
                        .team
                        .keys()
                        .filter(|t| !ids.contains(t.as_str()))
                        .cloned()
                        .collect();
                    for t in gone {
                        if let Some(was) = hub.team.get(&t) {
                            hub.info = Some(format!("You're no longer in {}.", was.detail.name));
                        }
                        hub.team.remove(&t);
                        hub.channels.retain(|_, c| c.team_id != t);
                        hub.kept.forget_team(&t);
                    }
                    hub.save(cx);
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
        let busy = format!("load:{team_id}");
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
        for (epoch, key) in &read.keys {
            changed |= self.kept.put_key(team_id, *epoch, key);
        }
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
        if role.is_admin() {
            if rotation {
                self.rotate(team_id, cx);
            } else {
                self.seal_for_new_devices(team_id, cx);
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
        self.run(
            cx,
            move |net| {
                let call = net.call();
                let team_id = call.create_team(&name)?;
                // The first key, for every device of this user, then a channel.
                let mine = trusted_devices(&call.devices(me)?);
                let key = crypto::new_team_key();
                let envelopes = seal_all(net, &team_id, &[(1, key)], &mine)?;
                call.put_envelopes(&team_id, &envelopes)?;
                let channel_id = crypto::ulid();
                let name_ct = crypto::seal_channel_name(&team_id, &channel_id, 1, &key, "general");
                call.create_channel(&team_id, &channel_id, &name_ct)?;
                let sealed: Vec<String> = mine.into_iter().map(|d| d.device_id).collect();
                Ok((team_id, key, sealed, channel_id))
            },
            |hub, result, cx| {
                hub.busy.remove("create-team");
                match result {
                    Ok((team_id, key, sealed, _)) => {
                        hub.kept.put_key(&team_id, 1, &key);
                        hub.kept.mark_sealed(&team_id, sealed);
                        hub.save(cx);
                        hub.refresh(cx);
                    }
                    Err(e) => hub.error = Some(format!("Could not create the team: {e}")),
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
        let busy = format!("rotate:{team_id}");
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
        let devices: Vec<DeviceRow> = t.devices.values().flatten().cloned().collect();
        let team_id = team_id.to_string();
        let key = crypto::new_team_key();
        self.run(
            cx,
            {
                let team_id = team_id.clone();
                move |net| {
                    let envelopes = seal_all(net, &team_id, &[(epoch, key)], &devices)?;
                    net.call().put_envelopes(&team_id, &envelopes)?;
                    Ok(devices.into_iter().map(|d| d.device_id).collect::<Vec<_>>())
                }
            },
            move |hub, result, cx| {
                hub.busy.remove(&busy);
                match result {
                    Ok(sealed) => {
                        hub.kept.put_key(&team_id, epoch, &key);
                        hub.kept.mark_sealed(&team_id, sealed);
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
    /// seals them: every key this device holds, once per device.
    fn seal_for_new_devices(&mut self, team_id: &str, cx: &mut Context<Self>) {
        let Some(t) = self.team.get(team_id) else {
            return;
        };
        let keys = self.kept.keys(team_id);
        let mine = self.device_id().unwrap_or_default();
        if keys.is_empty() {
            return;
        }
        let joined: HashMap<u64, u64> = t
            .detail
            .members
            .iter()
            .map(|m| (m.github_id, m.joined_at))
            .collect();
        let fresh: Vec<DeviceRow> = t
            .devices
            .iter()
            .flat_map(|(gid, ds)| {
                let since = joined.get(gid).copied().unwrap_or(0);
                ds.iter()
                    .filter(move |d| d.created_at > since)
                    .cloned()
                    .collect::<Vec<_>>()
            })
            .filter(|d| d.device_id != mine && !self.kept.sealed_for(team_id, &d.device_id))
            .collect();
        if fresh.is_empty() {
            return;
        }
        let busy = format!("seal:{team_id}");
        if !self.busy.insert(busy.clone()) {
            return;
        }
        let team_id = team_id.to_string();
        self.run(
            cx,
            {
                let team_id = team_id.clone();
                move |net| {
                    let envelopes = seal_all(net, &team_id, &keys, &fresh)?;
                    for chunk in envelopes.chunks(500) {
                        net.call().put_envelopes(&team_id, chunk)?;
                    }
                    Ok(fresh.into_iter().map(|d| d.device_id).collect::<Vec<_>>())
                }
            },
            move |hub, result, cx| {
                hub.busy.remove(&busy);
                match result {
                    Ok(sealed) => {
                        hub.kept.mark_sealed(&team_id, sealed);
                        hub.save(cx);
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
            {
                let team_id = team_id.clone();
                move |net| net.call().invite(&team_id, &login).map(|()| login)
            },
            move |hub, result, _| {
                hub.busy.remove(&busy);
                match result {
                    Ok(login) => {
                        hub.sent
                            .retain(|(t, l, _)| !(*t == team_id && l.eq_ignore_ascii_case(&login)));
                        hub.sent.push((team_id, login, now()));
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
                    // The Team tab shows the wait itself.
                    Ok(()) => {
                        hub.kept.ask(&invite.team_id, &invite.team_name);
                        if let Some(by) = &invite.invited_by_login {
                            hub.inviters.insert(invite.team_id.clone(), by.clone());
                        }
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
    pub fn approve(&mut self, team_id: &str, github_id: u64, cx: &mut Context<Self>) {
        let Some(t) = self.team.get(team_id) else {
            return;
        };
        let Some(p) = t.pending.iter().find(|p| p.github_id == github_id).cloned() else {
            return;
        };
        let trusted = trusted_devices(&p.devices);
        if trusted.len() != p.devices.len() || trusted.is_empty() {
            self.error = Some(format!(
                "A device of @{} does not check out, so they can't be approved from here.",
                p.login
            ));
            cx.notify();
            return;
        }
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
                    net.call().approve(&team_id, github_id, &envelopes)?;
                    Ok(trusted.into_iter().map(|d| d.device_id).collect::<Vec<_>>())
                }
            },
            move |hub, result, cx| {
                hub.busy.remove(&busy);
                match result {
                    Ok(sealed) => {
                        hub.kept.mark_sealed(&team_id, sealed);
                        hub.save(cx);
                        hub.load_team(&team_id, cx);
                    }
                    Err(e) => hub.error = Some(format!("Could not approve: {e}")),
                }
            },
        );
    }

    pub fn reject(&mut self, team_id: &str, github_id: u64, cx: &mut Context<Self>) {
        let team_id = team_id.to_string();
        self.run(
            cx,
            {
                let team_id = team_id.clone();
                move |net| net.call().reject(&team_id, github_id)
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

    /// The epoch and key a message or channel name is sealed with now: the
    /// current epoch, or while a rotation is pending the newest one held.
    fn sending_key(&self, team_id: &str) -> Option<(u64, TeamKey)> {
        let t = self.team.get(team_id)?;
        let (epoch, key) = self.kept.newest(team_id)?;
        (epoch == t.detail.epoch || (t.detail.rotation_pending && epoch < t.detail.epoch))
            .then_some((epoch, key))
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
        let link = Link::open(
            key.clone(),
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
            last_seq: 0,
            hidden: 0,
            locked: Vec::new(),
            status: socket::Status::Connecting,
            loading: true,
            unknown: Vec::new(),
            outbox: BTreeMap::new(),
            link: None,
            seen_ids: HashSet::new(),
            here: Vec::new(),
        });
        state.link = Some(link);
        cx.notify();
    }

    fn on_socket(&mut self, key: String, ev: socket::Event, cx: &mut Context<Self>) {
        let Some(c) = self.channels.get_mut(&key) else {
            return;
        };
        match ev {
            socket::Event::Status(status) => {
                let connected = status == socket::Status::Connected;
                let refused = matches!(status, socket::Status::Refused(_));
                c.status = status;
                // A new socket hears who is here right away; until then
                // the last list may be stale.
                if !connected {
                    c.here.clear();
                }
                if refused {
                    c.link = None;
                    c.loading = false;
                    self.refresh(cx);
                }
                if connected {
                    // What was missed while away, then what was not acknowledged.
                    self.catch_up(&key, cx);
                }
            }
            socket::Event::Frame(v) => match v["type"].as_str() {
                Some("message") => {
                    if let Ok(stored) = serde_json::from_value::<Stored>(v) {
                        self.ingest(&key, vec![stored], false, cx);
                    }
                }
                Some("presence") => {
                    if let Some(here) = presence(&v) {
                        let c = self.channels.get_mut(&key).expect("looked up above");
                        c.here = here;
                    }
                }
                Some("error") => {
                    let id = v["id"].as_str().unwrap_or_default().to_string();
                    let why = v["error"].as_str().unwrap_or("refused").to_string();
                    let c = self.channels.get_mut(&key).expect("looked up above");
                    // Stored already: the history read on connect brings it.
                    if why.contains("exists") {
                        c.outbox.remove(&id);
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

    /// Reads the history after the newest message held, page by page, then
    /// sends again what is still unacknowledged.
    fn catch_up(&mut self, key: &str, cx: &mut Context<Self>) {
        let Some(c) = self.channels.get(key) else {
            return;
        };
        let (team_id, channel_id, after) = (c.team_id.clone(), c.channel_id.clone(), c.last_seq);
        let key = key.to_string();
        self.run(
            cx,
            move |net| {
                let mut all = Vec::new();
                let mut after = after;
                loop {
                    let page = net.call().history(&team_id, &channel_id, after)?;
                    let full = page.len() >= 200;
                    if let Some(last) = page.last() {
                        after = last.seq;
                    }
                    all.extend(page);
                    if !full {
                        return Ok(all);
                    }
                }
            },
            move |hub, result, cx| {
                let Some(c) = hub.channels.get_mut(&key) else {
                    return;
                };
                c.loading = false;
                match result {
                    Ok(stored) => {
                        hub.ingest(&key, stored, false, cx);
                        let c = hub.channels.get_mut(&key).expect("looked up above");
                        if let Some(link) = &c.link {
                            for frame in c.outbox.values() {
                                link.send(frame.clone());
                            }
                        }
                    }
                    Err(e) => hub.error = Some(format!("Could not read the channel: {e}")),
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
        // This device's @bot questions the server just stored: the bot runs now.
        let mut asked = Vec::new();
        for s in stored {
            if c.seen_ids.contains(&s.message.id) {
                continue;
            }
            c.last_seq = c.last_seq.max(s.seq);
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
            if c.outbox.remove(&s.message.id).is_some()
                && body.kind == "text"
                && body.mentions.iter().any(|m| m == "bot")
            {
                asked.push(s.message.id.clone());
            }
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
        let channel_id = c.channel_id.clone();
        for id in asked {
            self.start_bot(&team_id, &channel_id, &id, cx);
        }
        if self.viewing.as_deref() == Some(key) {
            self.mark_read(key, cx);
        }
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

    // ---- unread ---------------------------------------------------------------

    /// The channel the tab shows now, if any: its messages count as read.
    pub fn set_viewing(&mut self, key: Option<String>, cx: &mut Context<Self>) {
        if self.viewing == key {
            return;
        }
        self.viewing = key.clone();
        if let Some(key) = key {
            self.unread_read.remove(&key);
            self.mark_read(&key, cx);
        }
        cx.notify();
    }

    fn mark_read(&mut self, key: &str, cx: &mut Context<Self>) {
        let Some(c) = self.channels.get(key) else {
            return;
        };
        if self.kept.mark_read(key, c.last_seq) {
            self.save(cx);
        }
    }

    /// Messages from others after the last one this device showed. Channels
    /// with a socket count what they hold; the rest what history said.
    pub fn unread(&self, team_id: &str, channel_id: &str) -> usize {
        let key = channel_key(team_id, channel_id);
        if self.viewing.as_deref() == Some(key.as_str()) {
            return 0;
        }
        let me = self.github_id();
        match self.channels.get(&key) {
            Some(c) if !c.loading => {
                let after = self.kept.last_read(&key).unwrap_or(0);
                c.messages
                    .iter()
                    .filter(|m| m.seq.is_some_and(|s| s > after))
                    .filter(|m| m.github_id != me && counts_as_unread(&m.body.kind))
                    .count()
            }
            _ => self.unread_read.get(&key).copied().unwrap_or(0),
        }
    }

    /// Reads the unread counts of channels with no socket here, from one
    /// history page each: on focus (`soon`), else at most every
    /// [`UNREAD_AGAIN`].
    pub fn refresh_unread(&mut self, soon: bool, cx: &mut Context<Self>) {
        let wait = if soon { UNREAD_FOCUS } else { UNREAD_AGAIN };
        if self.unread_at.is_some_and(|at| at.elapsed() < wait) || self.team.is_empty() {
            return;
        }
        self.unread_at = Some(Instant::now());
        let me = self.github_id();
        let mut asks = Vec::new();
        for (team_id, t) in &self.team {
            let mine: HashSet<String> = t
                .devices
                .get(&me)
                .into_iter()
                .flatten()
                .map(|d| d.device_id.clone())
                .collect();
            let keys: HashMap<u64, TeamKey> = self.kept.keys(team_id).into_iter().collect();
            for ch in &t.channels {
                let key = channel_key(team_id, &ch.id);
                if self.channels.contains_key(&key) {
                    continue;
                }
                let after = self.kept.last_read(&key).unwrap_or(0);
                asks.push((
                    key,
                    team_id.clone(),
                    ch.id.clone(),
                    after,
                    mine.clone(),
                    keys.clone(),
                ));
            }
        }
        if asks.is_empty() {
            return;
        }
        self.run(
            cx,
            move |net| {
                let mut out = Vec::new();
                for (key, team_id, channel_id, after, mine, keys) in asks {
                    // One channel failing (an old server, a removed channel)
                    // leaves its count as it was.
                    let Ok(page) = net.call().history(&team_id, &channel_id, after) else {
                        continue;
                    };
                    let n = page
                        .iter()
                        .filter(|s| !mine.contains(&s.message.device_id))
                        .filter(|s| {
                            keys.get(&s.message.epoch)
                                .and_then(|k| {
                                    crypto::open_message(&team_id, &channel_id, &s.message, k).ok()
                                })
                                .is_none_or(|b| counts_as_unread(&b.kind))
                        })
                        .count();
                    out.push((key, n));
                }
                Ok(out)
            },
            |hub, result, _| {
                if let Ok(counts) = result {
                    for (key, n) in counts {
                        hub.unread_read.insert(key, n);
                    }
                }
            },
        );
    }

    /// Sends a text message, replying to `reply_to` when given. One that
    /// mentions @bot runs it once the server has stored it.
    pub fn send(
        &mut self,
        team_id: &str,
        channel_id: &str,
        text: String,
        reply_to: Option<String>,
        cx: &mut Context<Self>,
    ) -> bool {
        let mut body = Body::text(text, reply_to);
        if super::bot::mentions_bot(&body.text) {
            body.mentions = vec!["bot".into()];
        }
        self.send_body(team_id, channel_id, body, cx)
    }

    /// Seals and sends any message body.
    pub fn send_body(
        &mut self,
        team_id: &str,
        channel_id: &str,
        body: Body,
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
        self.send_body(team_id, channel_id, m.body, cx);
    }
}

/// Kinds that show as a message of their own, and so count as unread.
/// Acceptances show under their answer.
fn counts_as_unread(kind: &str) -> bool {
    matches!(kind, "text" | "bot" | "decision")
}

/// An unread count as the channel list shows it.
pub fn unread_label(n: usize) -> Option<String> {
    match n {
        0 => None,
        1..=99 => Some(n.to_string()),
        _ => Some("99+".into()),
    }
}

/// The GitHub IDs of a `presence` frame, each once.
fn presence(v: &serde_json::Value) -> Option<Vec<u64>> {
    let mut here: Vec<u64> = v["here"]
        .as_array()?
        .iter()
        .filter_map(serde_json::Value::as_u64)
        .collect();
    here.sort_unstable();
    here.dedup();
    Some(here)
}

/// A team's members for the "Here now" list: who has the channel open
/// first, each group in the team's order, with whether they are here.
pub fn here_now<'a>(members: &'a [MemberRow], here: &[u64]) -> Vec<(&'a MemberRow, bool)> {
    let mut out: Vec<(&MemberRow, bool)> = members
        .iter()
        .map(|m| (m, here.contains(&m.github_id)))
        .collect();
    out.sort_by_key(|(_, on)| !on);
    out
}

fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// The name this device registers with: the machine's.
pub fn device_name() -> String {
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
/// root; every other must be endorsed by one already trusted. The server
/// checked each endorsement's signature when the device registered; the list
/// does not carry it, so a client checks the chain's shape.
pub fn trusted_devices(rows: &[DeviceRow]) -> Vec<DeviceRow> {
    let point = |s: &str| crypto::unb64(s).filter(|p| p.len() == 65 && p[0] == 4);
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
            let e = crypto::seal_envelope(
                team_id,
                *epoch,
                key,
                &d.device_id,
                &d.enc_pub,
                &net.local.device,
            )
            .map_err(|e| ApiError::Status(0, format!("device {}: {e}", d.device_id)))?;
            out.push(e);
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
        let trusted = trusted_devices(&rows);
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
        }
    }

    #[test]
    fn trusts_the_chain_from_the_first_device() {
        let (a, b, c) = (Device::generate(), Device::generate(), Device::generate());
        let rows = vec![row(&a, 1, None), row(&b, 2, Some(&a)), row(&c, 3, Some(&b))];
        assert_eq!(trusted_devices(&rows).len(), 3);
        // An endorser nobody trusts, and a second root, are left out.
        let (x, y) = (Device::generate(), Device::generate());
        let rows = vec![row(&a, 1, None), row(&x, 2, Some(&y)), row(&y, 3, None)];
        let ids: Vec<String> = trusted_devices(&rows)
            .into_iter()
            .map(|d| d.device_id)
            .collect();
        assert_eq!(ids, vec![a.id().to_string()]);
        // A row whose ID is not its key's.
        let mut forged = row(&b, 2, Some(&a));
        forged.sig_pub = c.sig_pub().into();
        assert_eq!(trusted_devices(&[row(&a, 1, None), forged]).len(), 1);
        assert!(trusted_devices(&[]).is_empty());
    }

    #[test]
    fn reads_presence_and_puts_who_is_here_first() {
        let v = serde_json::json!({ "type": "presence", "here": [9, 3, 9, "x"] });
        assert_eq!(presence(&v), Some(vec![3, 9]));
        assert_eq!(presence(&serde_json::json!({ "type": "presence" })), None);
        let m = |id: u64, login: &str| MemberRow {
            github_id: id,
            login: login.into(),
            role: Role::Member,
            joined_at: 0,
        };
        let members = vec![m(1, "karlo"), m(2, "rik"), m(3, "dani")];
        let got: Vec<(&str, bool)> = here_now(&members, &[3, 1])
            .into_iter()
            .map(|(m, on)| (m.login.as_str(), on))
            .collect();
        assert_eq!(got, vec![("karlo", true), ("dani", true), ("rik", false)]);
    }

    #[test]
    fn unread_counts_cap_at_99() {
        assert_eq!(unread_label(0), None);
        assert_eq!(unread_label(7).as_deref(), Some("7"));
        assert_eq!(unread_label(99).as_deref(), Some("99"));
        assert_eq!(unread_label(200).as_deref(), Some("99+"));
        assert!(counts_as_unread("text") && counts_as_unread("bot"));
        assert!(!counts_as_unread("decision_accept"));
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
        let mine = trusted_devices(&a.call().devices(ma.github_id).unwrap());
        let key = crypto::new_team_key();
        let envelopes = seal_all(&a, &team, &[(1, key)], &mine).unwrap();
        assert!(!a.call().put_envelopes(&team, &envelopes).unwrap());
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
        let theirs = trusted_devices(&p.devices);
        assert_eq!(theirs.len(), 1);
        let envelopes = seal_all(&a, &team, &contiguous_tail(&[(1, key)]), &theirs).unwrap();
        a.call().approve(&team, mb.github_id, &envelopes).unwrap();

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
        drop((la, lb));

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
        let both = trusted_devices(&a.call().devices(ma.github_id).unwrap());
        assert_eq!(both.len(), 2);
        let fresh: Vec<DeviceRow> = both
            .into_iter()
            .filter(|d| d.device_id == d2.id())
            .collect();
        a.call()
            .put_envelopes(&team, &seal_all(&a, &team, &[(1, key)], &fresh).unwrap())
            .unwrap();
        let read2 = read_team(&second, &team, true, &HashSet::new()).unwrap();
        assert_eq!(read2.keys, vec![(1, key)]);

        // A removes B: a new key for A's devices only, and B is out.
        a.call().remove_member(&team, mb.github_id).unwrap();
        let read = read_team(&a, &team, true, &HashSet::from([1])).unwrap();
        assert!(read.detail.rotation_pending);
        assert_eq!(read.detail.epoch, 2);
        let devices: Vec<DeviceRow> = read.devices.values().flatten().cloned().collect();
        let envelopes = seal_all(&a, &team, &[(2, crypto::new_team_key())], &devices).unwrap();
        assert!(!a.call().put_envelopes(&team, &envelopes).unwrap());
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
