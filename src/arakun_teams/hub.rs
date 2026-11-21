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
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use futures::StreamExt as _;
use futures::channel::mpsc;
use futures::future::{FutureExt as _, Shared};
use gpui::{AppContext as _, Context, Entity, Task};

use super::Teams;
use super::api::{
    ApiError, Call, DeviceRow, Endorsement, InviteRow, Me, MemberRow, PendingRow, Remind, Role,
    Stored, TeamDetail, TeamInvite, TeamRow, WaitingRow,
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
const MAX_ENVELOPES: usize = 500;
/// The Worker's limits on a socket frame and on a message's `ct`, decoded.
/// It refuses a frame over the first without reading the message's ID, so
/// such a message could never be marked failed: both are checked first.
const MAX_FRAME: usize = 96 * 1024;
const MAX_MESSAGE_CT: usize = 64 * 1024;

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
    /// The team's open invites, for its admins; `None` when the server
    /// could not list them.
    pub invites: Option<Vec<TeamInvite>>,
    /// When each epoch's key was made, as far as the server says.
    pub epoch_made: HashMap<u64, u64>,
    /// Who sent this device its first key of the team: who let it in.
    pub approved_by: Option<String>,
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

/// Marks the message `id` as refused by the server, with why. Only one still
/// on its way: a replay of a stored message may be refused for another
/// reason (a rate limit, say), and that must not offer to send it twice.
fn refuse(messages: &mut [Msg], id: &str, why: &str) -> bool {
    let Some(m) = messages
        .iter_mut()
        .find(|m| m.id == id && m.delivery == Delivery::Sending)
    else {
        return false;
    };
    m.delivery = Delivery::Failed(why.to_string());
    true
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
    /// The newest edit its sender made, once one applies: `body.text`
    /// then holds the edited text.
    pub edited: Option<Edited>,
}

/// An edit applied to a text message.
#[derive(Clone, Debug, PartialEq)]
pub struct Edited {
    /// The `edit` message.
    pub id: String,
    pub seq: Option<u64>,
    pub at: u64,
    /// The text as first sent.
    pub original: String,
}

/// Applies every `edit` to the text message it names: only one by the same
/// sender, of a `text` message, the newest by `seq` winning. One still on
/// its way shows at once; one that failed does not.
pub fn apply_edits(messages: &mut [Msg]) {
    for m in messages.iter_mut() {
        if let Some(e) = m.edited.take() {
            m.body.text = e.original;
        }
    }
    let mut edits: Vec<(Option<u64>, usize)> = messages
        .iter()
        .enumerate()
        .filter(|(_, m)| {
            m.body.kind == "edit"
                && m.body.reply_to.is_some()
                && !matches!(m.delivery, Delivery::Failed(_))
        })
        .map(|(i, m)| (m.seq, i))
        .collect();
    edits.sort_by_key(|(seq, i)| (seq.is_none(), seq.unwrap_or(0), *i));
    for (_, i) in edits {
        let e = &messages[i];
        let (id, seq, at, gid) = (e.id.clone(), e.seq, e.at, e.github_id);
        let (target, text) = (
            e.body.reply_to.clone().unwrap_or_default(),
            e.body.text.clone(),
        );
        let Some(t) = messages.iter_mut().find(|m| m.id == target) else {
            continue;
        };
        if t.body.kind != "text" || t.github_id != gid {
            continue;
        }
        let original = t
            .edited
            .take()
            .map_or_else(|| t.body.text.clone(), |e| e.original);
        t.body.text = text;
        t.edited = Some(Edited {
            id,
            seq,
            at,
            original,
        });
    }
}

impl ChannelState {
    /// Messages sealed with a key this device does not hold.
    pub fn locked(&self) -> usize {
        self.locked.len()
    }

    /// Whether the channel has a socket, connected or still dialing.
    pub fn linked(&self) -> bool {
        self.link.is_some()
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
    /// This device's @bot questions that arrived past a gap. The range they
    /// review is taken once history fills the gap, so it misses nothing.
    bot_waiting: Vec<String>,
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
    /// When the channel was last asked for, to pick the sockets to close.
    shown: Instant,
    seen_ids: HashSet<String>,
    /// Who has this channel open now, by GitHub ID, as the server last said.
    /// Empty while the socket is down: nobody is known to be here then.
    pub here: Vec<u64>,
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
    /// The account the state below belongs to. `Teams` also notifies when
    /// only a picture arrived; comparing this, not `net`, keeps such a
    /// notification from restarting a device setup still under way.
    account: Option<Account>,
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
    /// Teams this user asked to join, as the server lists them.
    pub waiting: Vec<WaitingRow>,
    /// A reminder to a team's admins may go again from this time, after the
    /// server said it was too soon.
    pub remind_after: HashMap<String, u64>,
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
    /// The team the last "Start a team" made, once the server named it, even
    /// when setting it up failed after: the Team tab opens that one.
    pub created: Option<String>,
    /// Bumped by every "They don't". A key upload planned before it stops
    /// before its next request, so a failed check reaches work in flight.
    doubts: Arc<AtomicU64>,
    events: socket::Events,
    /// Bumped when the account changes, so late answers for the last one are dropped.
    generation: u64,
    /// Bumped whenever a channel's messages change, so what is built from
    /// them (the decision log) is built again only then.
    pub messages_rev: u64,
    roster_version: u64,
    applied_roster: u64,
    membership: HashMap<String, u64>,
    /// Teams this device created whose `#general` is not made yet, with the
    /// channel ID to make it with. A retry reuses the ID, so a create that
    /// went through but lost its answer is not made twice.
    setup: HashMap<String, String>,
    /// Teams asked to be read while a read of the same version was on its
    /// way. That read may have started before the change that asked (an
    /// approval, a rotation), so its answer is dropped and the team read again.
    reread: HashSet<String>,
    next_link: u64,
    _events: Task<()>,
    save_task: Option<Shared<Task<Result<(), String>>>>,
    _poll: Option<Task<()>>,
    _tick: Option<Task<()>>,
}

/// How many channels keep their socket besides the one in view. Each socket
/// is a thread and a TLS connection, and a team can have hundreds of channels.
pub const SPARE_LINKS: usize = 8;

/// The channels to hang up, of those with a socket (key, when it was last
/// asked for, busy): all but the one in view and the `spare` asked for last.
/// A busy one, still reading history or with messages not yet acknowledged,
/// keeps its socket however old it is.
fn spare_links<T: Ord>(
    mut open: Vec<(String, T, bool)>,
    in_view: &str,
    spare: usize,
) -> Vec<String> {
    open.retain(|(key, ..)| key != in_view);
    open.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    open.into_iter()
        .skip(spare)
        .filter(|(_, _, busy)| !busy)
        .map(|(key, ..)| key)
        .collect()
}

/// Who is signed in, and where: the server, the session and the GitHub ID.
type Account = (String, String, u64);

fn account_of((base, session, me): &(String, String, Me)) -> Account {
    (base.clone(), session.clone(), me.github_id)
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
            account: None,
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
            messages_rev: 0,
            roster_version: 0,
            applied_roster: 0,
            membership: HashMap::new(),
            setup: HashMap::new(),
            reread: HashSet::new(),
            next_link: 0,
            _events: events,
            save_task: None,
            _poll: None,
            _tick: None,
            info: None,
            waiting: Vec::new(),
            remind_after: HashMap::new(),
            inviters: HashMap::new(),
            joined: HashSet::new(),
            created: None,
            doubts: Arc::default(),
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

    /// This device's public keys, `(sig_pub, enc_pub)`, for comparing devices.
    pub fn public_keys(&self) -> Option<(&str, &str)> {
        self.net
            .as_ref()
            .map(|n| (n.local.device.sig_pub(), n.local.device.enc_pub()))
    }

    /// The user checked this device's compare numbers with its owner.
    pub fn verified(&self, device_id: &str) -> bool {
        self.kept.verified(device_id)
    }

    /// The user found this device's compare numbers different.
    pub fn mismatched(&self, device_id: &str) -> bool {
        self.kept.mismatched(device_id)
    }

    /// Marks a device checked ("They match"), or not ("They don't").
    pub fn set_verified(&mut self, device_id: &str, yes: bool, cx: &mut Context<Self>) {
        if !yes {
            self.doubts.fetch_add(1, Ordering::SeqCst);
        }
        // "They don't" saves even when nothing changed, so pressing it again
        // retries a save that failed.
        if self.kept.set_verified(device_id, yes) || !yes {
            self.save(cx);
        }
        cx.notify();
    }

    /// Who is signed in changed: start over for the new account, or stop.
    fn account_changed(&mut self, cx: &mut Context<Self>) {
        let signed = self.teams_entity.read(cx).signed();
        let account = signed.as_ref().map(account_of);
        let same = account == self.account;
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
        self.reread.clear();
        self.invites.clear();
        self.team.clear();
        self.channels.clear();
        self.notices.clear();
        self.my_devices.clear();
        self.error = None;
        self.info = None;
        self.busy.clear();
        self.waiting.clear();
        self.remind_after.clear();
        self.inviters.clear();
        self.joined.clear();
        self.bot.clear();
        self.viewing = None;
        self.unread_read.clear();
        self.unread_at = None;
        self.created = None;
        self._poll = None;
        self._tick = None;
        self.info = None;
        self.device = DeviceState::Starting;
        self.load = Load::Loading;
        self.me = signed.as_ref().map(|(_, _, me)| me.clone());
        self.account = account;
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
        let Some(sealer) = self.sealer.clone() else {
            return;
        };
        let kept = self.kept.clone();
        let prior = self.save_task.take();
        let task = cx
            .background_spawn(store::after_save(prior, move || sealer.write(&kept)))
            .shared();
        self.save_task = Some(task.clone());
        // What only this device knows, such as a request to join, would be
        // gone after a restart: say so rather than only log it.
        let generation = self.generation;
        cx.spawn(async move |this, cx| {
            if let Err(e) = task.await {
                let _ = this.update(cx, |hub, cx| {
                    if hub.generation == generation {
                        hub.error = Some(format!(
                            "Could not save this device's Teams records: {e}. The next change tries again."
                        ));
                        cx.notify();
                    }
                });
            }
        })
        .detach();
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
                // A blip while the code is on show (Wi-Fi, sleep) keeps the
                // code and the wait: the other device may approve meanwhile.
                Err(ApiError::Network(_) | ApiError::Status(500..=599, _))
                    if matches!(hub.device, DeviceState::Unendorsed { .. }) =>
                {
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
                self.account = None;
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
            |net| {
                let teams = net.call().teams()?;
                let invites = net.call().invites()?;
                // An older server has no list of requests; the teams file
                // still remembers this device's.
                let waiting = net.call().waiting().ok();
                Ok((teams, invites, waiting))
            },
            move |hub, result, cx| match result {
                _ if version < hub.applied_roster => {}
                Ok((teams, invites, waiting)) => {
                    hub.applied_roster = version;
                    hub.load = Load::Ready;
                    let ids: HashSet<&str> = teams.iter().map(|t| t.team_id.as_str()).collect();
                    if let Some(waiting) = waiting {
                        // A request the server no longer has, of a team this
                        // user is not in, was turned down.
                        let open: HashSet<&str> =
                            waiting.iter().map(|w| w.team_id.as_str()).collect();
                        let dropped: Vec<String> = hub
                            .kept
                            .asked()
                            .keys()
                            .filter(|t| !open.contains(t.as_str()) && !ids.contains(t.as_str()))
                            .cloned()
                            .collect();
                        for t in dropped {
                            hub.kept.forget_ask(&t);
                        }
                        hub.waiting = waiting;
                    }
                    let asked: Vec<String> = hub
                        .kept
                        .asked()
                        .keys()
                        .filter(|t| ids.contains(t.as_str()))
                        .cloned()
                        .collect();
                    // The keys file is sealed and written durably: only
                    // when this changed something, not on every refresh.
                    let mut changed = false;
                    for t in &asked {
                        // The chat says so too, but only to someone who
                        // opens it; this reaches a user on another team.
                        if let Some(name) = hub.kept.asked().get(t) {
                            hub.info = Some(format!("You're in {name} now."));
                        }
                        hub.joined.insert(t.clone());
                        changed |= hub.kept.forget_ask(t);
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
                        changed |= hub.kept.forget_team(&t);
                    }
                    changed |= hub.kept.retain_teams(&ids);
                    if changed {
                        hub.save(cx);
                    }
                    update_memberships(&mut hub.membership, &hub.teams, &teams, version);
                    hub.teams = teams;
                    hub.invites = invites;
                    let ids: Vec<String> = hub.teams.iter().map(|t| t.team_id.clone()).collect();
                    for id in ids {
                        hub.reload_team(&id, false, cx);
                    }
                }
                Err(e) => hub.load = Load::Unreachable(e.to_string()),
            },
        );
    }

    /// Teams this user asked to join and is waiting on, with their names:
    /// the server's list, oldest first, then any only this device remembers.
    pub fn asked(&self) -> Vec<(String, String)> {
        let mut out: Vec<(String, String)> = self
            .waiting
            .iter()
            .map(|w| (w.team_id.clone(), w.team_name.clone()))
            .collect();
        for (k, v) in self.kept.asked() {
            if !out.iter().any(|(t, _)| t == k) {
                out.push((k.clone(), v.clone()));
            }
        }
        out
    }

    /// The server's word on a request to join, when it has one.
    pub fn waiting_for(&self, team_id: &str) -> Option<&WaitingRow> {
        self.waiting.iter().find(|w| w.team_id == team_id)
    }

    /// Reminds a team's admins of this user's request. Too soon, and the
    /// button waits for the time the server gives.
    pub fn remind(&mut self, team_id: &str, cx: &mut Context<Self>) {
        let busy = format!("remind:{team_id}");
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
                move |net| net.call().remind(&team_id)
            },
            move |hub, result, _| {
                hub.busy.remove(&busy);
                match result {
                    Ok(Remind::Sent) => {
                        if let Some(w) = hub.waiting.iter_mut().find(|w| w.team_id == team_id) {
                            w.reminded_at = Some(now());
                        }
                    }
                    Ok(Remind::TooSoon(at)) => {
                        hub.remind_after.insert(team_id, at);
                    }
                    Err(e) => hub.error = Some(format!("Could not send the reminder: {e}")),
                }
            },
        );
    }

    /// One team: its members and their devices, its channels, this device's
    /// keys, and the admin's duties (a pending rotation, new devices).
    pub fn load_team(&mut self, team_id: &str, cx: &mut Context<Self>) {
        self.reload_team(team_id, true, cx);
    }

    /// `after_change`: asked for after something changed the team, so a read
    /// on its way may be out of date. The 20 s refresh only reads again; were
    /// it to drop a read on its way too, one slower than that would never land.
    fn reload_team(&mut self, team_id: &str, after_change: bool, cx: &mut Context<Self>) {
        let Some(row) = self.teams.iter().find(|t| t.team_id == team_id).cloned() else {
            return;
        };
        let Some(version) = self.membership.get(team_id).copied() else {
            return;
        };
        let busy = format!("load:{team_id}:{version}");
        if !self.busy.insert(busy.clone()) {
            if after_change {
                self.reread.insert(team_id.to_string());
            }
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
                if hub.reread.remove(&team_id) {
                    hub.load_team(&team_id, cx);
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
        changed |= self.kept.observe_skips(&read.detail);
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
                invites: read.invites,
                epoch_made: read.epoch_made,
                approved_by: read.approved_by,
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
        self.created = None;
        let me = self.github_id();
        let pinned = self.kept.roots.get(&me).cloned();
        let doubted: HashSet<String> = self.kept.mismatched_ids().cloned().collect();
        self.run(
            cx,
            move |net| {
                let call = net.call();
                let rows = call.devices(me)?;
                let mine = trusted_devices(me, &rows);
                if mine.iter().any(|d| doubted.contains(&d.device_id)) {
                    return Err(ApiError::Status(
                        0,
                        "One of your devices is marked as not matching. Compare it again first."
                            .into(),
                    ));
                }
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
                    hub.created = Some(team_id.clone());
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
        if mine.iter().any(|d| self.kept.mismatched(&d.device_id)) {
            // The team exists; it gets no key until the check is cleared.
            self.busy.remove("create-team");
            self.error = Some(
                "One of your devices is marked as not matching, so the team got no key.".into(),
            );
            self.refresh(cx);
            return;
        }
        let doubts = self.doubts.clone();
        let planned = doubts.load(Ordering::SeqCst);
        self.run(
            cx,
            {
                let team = team.clone();
                move |net| {
                    let batch = seal_all(net, &team, &[(1, key)], &mine)?;
                    still_trusted(&doubts, planned)?;
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
        let doubted = doubted(&self.kept, t);
        if !doubted.is_empty() {
            // A failed comparison is the user's call to undo; a chain that
            // doesn't check out is not.
            let compared: Vec<&str> = doubted
                .iter()
                .filter(|l| !t.untrusted.contains(l))
                .map(String::as_str)
                .collect();
            let problem = if compared.is_empty() {
                format!(
                    "The team needs a new key, but a device of @{} does not check out, so none was made.",
                    doubted.join(", @")
                )
            } else {
                format!(
                    "The team needs a new key, but a device of @{} did not match when you compared it, so none was made. Compare it again, or remove them.",
                    compared.join(", @")
                )
            };
            if let Some(t) = self.team.get_mut(team_id) {
                t.problem = Some(problem);
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
        let doubts = self.doubts.clone();
        let planned = doubts.load(Ordering::SeqCst);
        self.run(
            cx,
            {
                let team_id = team_id.clone();
                move |net| {
                    upload_batch(net, &team_id, batch, |envelopes| {
                        still_trusted(&doubts, planned)?;
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
        let doubts = self.doubts.clone();
        let planned = doubts.load(Ordering::SeqCst);
        self.run(
            cx,
            {
                let team_id = team_id.clone();
                move |net| {
                    upload_batch(net, &team_id, batch, |envelopes| {
                        still_trusted(&doubts, planned)?;
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
            {
                let team_id = team_id.clone();
                move |net| net.call().invite(&team_id, &login).map(|()| login)
            },
            move |hub, result, cx| {
                hub.busy.remove(&busy);
                match result {
                    // The invited list comes back from the server.
                    Ok(_) => hub.load_team(&team_id, cx),
                    Err(ApiError::Status(409, _)) => {
                        hub.error =
                            Some("They are a member already, or waiting for approval.".into())
                    }
                    Err(e) => hub.error = Some(format!("Could not invite: {e}")),
                }
            },
        );
    }

    /// Takes back an invite nobody answered yet.
    pub fn withdraw(&mut self, team_id: &str, invite_id: &str, cx: &mut Context<Self>) {
        let busy = format!("withdraw:{invite_id}");
        if !self.busy.insert(busy.clone()) {
            return;
        }
        self.error = None;
        self.info = None;
        let (team_id, invite_id) = (team_id.to_string(), invite_id.to_string());
        self.run(
            cx,
            {
                let team_id = team_id.clone();
                move |net| net.call().withdraw(&team_id, &invite_id)
            },
            move |hub, result, cx| {
                hub.busy.remove(&busy);
                match result {
                    Ok(()) => hub.load_team(&team_id, cx),
                    Err(e) => hub.error = Some(format!("Could not withdraw the invite: {e}")),
                }
            },
        );
    }

    /// Turns an invite down: it goes away for good.
    pub fn decline(&mut self, invite: &InviteRow, cx: &mut Context<Self>) {
        let busy = format!("decline:{}", invite.invite_id);
        if !self.busy.insert(busy.clone()) {
            return;
        }
        self.error = None;
        self.info = None;
        let id = invite.invite_id.clone();
        self.run(
            cx,
            {
                let id = id.clone();
                move |net| net.call().decline(&id)
            },
            move |hub, result, cx| {
                hub.busy.remove(&busy);
                match result {
                    Ok(()) => {
                        hub.invites.retain(|i| i.invite_id != id);
                        hub.refresh(cx);
                    }
                    Err(e) => hub.error = Some(format!("Could not decline: {e}")),
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
        let reviewed_epoch = t.detail.keyed_epoch;
        let keyed_epoch = reviewed_epoch.unwrap_or(team_epoch);
        let Some(p) = t
            .pending
            .iter()
            .find(|p| p.github_id == github_id && p.request_id == request_id)
            .cloned()
        else {
            return;
        };
        let trusted = trusted_devices(github_id, &p.devices);
        if trusted.len() != p.devices.len()
            || trusted.iter().any(|d| self.kept.mismatched(&d.device_id))
            || !pin_directory(&mut self.kept, github_id, &trusted)
        {
            self.error = Some(format!(
                "A device of @{} does not check out, so they can't be approved from here.",
                p.login
            ));
            cx.notify();
            return;
        }
        self.save(cx);
        let keys = contiguous_tail(&self.kept.keys(team_id), |older, newer| {
            self.kept.skipped_between(team_id, older, newer)
        });
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
        let login = p.login.clone();
        let doubts = self.doubts.clone();
        let planned = doubts.load(Ordering::SeqCst);
        // The same snapshot the keys are sealed for, so a change since
        // answers 409 and the admin looks again.
        let reviewed: Vec<String> = p.devices.iter().map(|d| d.device_id.clone()).collect();
        self.run(
            cx,
            {
                let team_id = team_id.clone();
                move |net| {
                    // A request holds at most MAX_ENVELOPES: approve with the
                    // newest keys, then give the older ones newest first, so
                    // what arrived stays one run back from the newest key.
                    let parts = grant_parts(&keys, trusted.len());
                    let (newest, older) = parts.split_first().expect("keys is not empty");
                    let mut given = seal_all(net, &team_id, newest, &trusted)?;
                    still_trusted(&doubts, planned)?;
                    net.call().approve(
                        &team_id,
                        github_id,
                        &p.request_id,
                        &reviewed,
                        reviewed_epoch,
                        &given,
                    )?;
                    for part in older {
                        let batch = seal_all(net, &team_id, part, &trusted)?;
                        if let Err(e) = still_trusted(&doubts, planned).and_then(|()| {
                            net.call()
                                .put_envelopes(&team_id, keyed_epoch, team_epoch, &batch)
                        }) {
                            return Ok((given, Some(e)));
                        }
                        given.extend(batch);
                    }
                    Ok((given, None))
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
                    Ok((batch, older)) => {
                        hub.kept.admitted(&team_id, github_id, team_epoch, &batch);
                        hub.save(cx);
                        hub.load_team(&team_id, cx);
                        if let Some(e) = older {
                            hub.error = Some(format!(
                                "Approved @{login}, but their oldest team keys could not be given: {e}"
                            ));
                        }
                    }
                    Err(ApiError::Status(409, e)) => {
                        hub.error = Some(format!("Could not approve @{login}: {e}"));
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

    /// Why nothing can be sealed for this team now, or None when it can. The
    /// view keeps Send and Add channel off on it, since a pending rotation
    /// stops both while the device still holds keys.
    pub fn seal_blocked(&self, team_id: &str) -> Option<String> {
        self.sending_key(team_id)
            .is_none()
            .then(|| self.no_sending_key(team_id))
    }

    /// Why [`Self::sending_key`] gave nothing.
    fn no_sending_key(&self, team_id: &str) -> String {
        self.team
            .get(team_id)
            .map_or(NO_KEY, |t| no_key_reason(&t.detail))
            .into()
    }

    pub fn create_channel(&mut self, team_id: &str, name: String, cx: &mut Context<Self>) {
        let Some((epoch, key)) = self.sending_key(team_id) else {
            self.error = Some(self.no_sending_key(team_id));
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
        if let Some(c) = self.channels.get_mut(&key)
            && c.link.is_some()
        {
            c.shown = Instant::now();
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
        let in_view = key.clone();
        let state = self.channels.entry(key).or_insert_with(|| ChannelState {
            team_id: team_id.to_string(),
            channel_id: channel_id.to_string(),
            messages: Vec::new(),
            history: Cursor::default(),
            link_id,
            history_round: 0,
            reading: false,
            needs_history: false,
            bot_waiting: Vec::new(),
            hidden: 0,
            locked: Vec::new(),
            status: socket::Status::Connecting,
            loading: true,
            unknown: Vec::new(),
            outbox: BTreeMap::new(),
            link: None,
            shown: Instant::now(),
            seen_ids: HashSet::new(),
            here: Vec::new(),
        });
        state.link_id = link_id;
        state.link = Some(link);
        state.shown = Instant::now();
        self.close_spare_links(&in_view);
        cx.notify();
    }

    /// Hangs up the channels asked for longest ago, beyond [`SPARE_LINKS`].
    /// What was read stays, and `open_channel` dials again and reads what
    /// the channel missed.
    pub fn close_spare_links(&mut self, in_view: &str) {
        let open = self
            .channels
            .iter()
            .filter(|(_, c)| c.link.is_some())
            .map(|(key, c)| {
                let busy =
                    c.loading || c.reading || !c.outbox.is_empty() || !c.bot_waiting.is_empty();
                (key.clone(), c.shown, busy)
            })
            .collect();
        for key in spare_links(open, in_view, SPARE_LINKS) {
            if let Some(c) = self.channels.get_mut(&key) {
                // Dropping the link stops its thread, so nothing dials again,
                // and `on_socket` ignores what it still sends.
                c.link = None;
                c.status = socket::Status::Connecting;
            }
        }
    }

    /// Hangs up a channel's socket and keeps what was read: opening it again
    /// reads on from there. A channel with something still to send, or an
    /// @bot question waiting for its history, keeps its link.
    pub fn close_channel(&mut self, team_id: &str, channel_id: &str, cx: &mut Context<Self>) {
        let Some(c) = self.channels.get_mut(&channel_key(team_id, channel_id)) else {
            return;
        };
        if c.link.is_none() || !c.outbox.is_empty() || !c.bot_waiting.is_empty() {
            return;
        }
        c.link = None;
        // A history page still on its way belongs to the closed link.
        c.history_round += 1;
        c.reading = false;
        c.status = socket::Status::Connecting;
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
                // A new socket hears who is here right away; until then
                // the last list may be stale.
                if !connected {
                    c.here.clear();
                }
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
                Some("presence") => {
                    if let Some(here) = presence(&v) {
                        let c = self.channels.get_mut(key).expect("looked up above");
                        c.here = here;
                    }
                }
                Some("error") => {
                    let id = v["id"].as_str().unwrap_or_default().to_string();
                    let why = v["error"].as_str().unwrap_or("refused").to_string();
                    let c = self.channels.get_mut(key).expect("looked up above");
                    // The reply to a replay may arrive after catch-up finished.
                    if why.contains("exists") {
                        stored_already(&mut c.outbox, &c.messages, &mut c.bot_waiting, &id);
                        c.needs_history = true;
                        self.catch_up(key, cx);
                    } else if refuse(&mut c.messages, &id, &why) {
                        c.outbox.remove(&id);
                        // Send again asks with the project open then.
                        self.bot.asked.remove(&id);
                        self.messages_rev += 1;
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
                        let progress = !stored.is_empty();
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
                        } else if c.needs_history && progress {
                            // A live message came during the read, past where
                            // it ended: read on rather than wait for the
                            // refresh with the outbox held back.
                            hub.catch_up(&key, cx);
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
        // This device's @bot questions the server just stored: the bot runs now.
        let mut asked = Vec::new();
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
                edited: None,
            };
            match c.messages.iter().position(|m| m.id == msg.id) {
                Some(i) => c.messages[i] = msg,
                None => c.messages.push(msg),
            }
        }
        // Sent ones by seq; what is still on its way stays at the end.
        c.messages
            .sort_by_key(|m| (m.seq.is_none(), m.seq.unwrap_or(0), m.at));
        apply_edits(&mut c.messages);
        self.messages_rev += 1;
        let team_id = c.team_id.clone();
        let channel_id = c.channel_id.clone();
        c.bot_waiting.extend(asked);
        let asked = if c.needs_history {
            Vec::new()
        } else {
            std::mem::take(&mut c.bot_waiting)
        };
        for id in asked {
            let project = self
                .bot
                .asked
                .remove(&id)
                .unwrap_or_else(|| self.bot.project.clone());
            self.start_bot(&team_id, &channel_id, &id, project, cx);
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
        // A channel read only through history has no live frame yet.
        if self
            .kept
            .mark_read(key, c.history.newest.max(c.history.through))
        {
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

    /// Edits one of this user's text messages: a new `edit` message that
    /// names it. Its mentions never run @bot again.
    pub fn edit(
        &mut self,
        team_id: &str,
        channel_id: &str,
        id: &str,
        text: String,
        cx: &mut Context<Self>,
    ) -> bool {
        let mut body = Body::text(text, Some(id.to_string()));
        body.kind = "edit".into();
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
            self.error = Some(self.no_sending_key(team_id));
            self.load_team(team_id, cx);
            cx.notify();
            return false;
        };
        let Some(net) = self.net.clone() else {
            return false;
        };
        let ckey = channel_key(team_id, channel_id);
        if !self.channels.contains_key(&ckey) {
            return false;
        }
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
        if !fits(&frame, &wire) {
            self.error = Some(format!(
                "This message is too long to send. A message holds about {} KB of text.",
                MAX_MESSAGE_CT / 1024
            ));
            cx.notify();
            return false;
        }
        if body.kind == "text" && body.mentions.iter().any(|m| m == "bot") {
            self.bot.asked.insert(id.clone(), self.bot.project.clone());
        }
        let Some(c) = self.channels.get_mut(&ckey) else {
            return false;
        };
        let me = self.me.clone();
        c.messages.push(Msg {
            id: id.clone(),
            seq: None,
            at: now(),
            github_id: me.as_ref().map_or(0, |m| m.github_id),
            login: me.map(|m| m.login).unwrap_or_default(),
            body,
            delivery: Delivery::Sending,
            edited: None,
        });
        apply_edits(&mut c.messages);
        self.messages_rev += 1;
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
        let Some(body) = c
            .messages
            .iter()
            .find(|m| m.id == id)
            .map(|m| m.body.clone())
        else {
            return;
        };
        // Dropped only once the copy is queued: if it can't be (no key yet),
        // the failed one stays with its Send again, a bot answer included.
        if self.send_body(team_id, channel_id, body, cx)
            && let Some(c) = self.channels.get_mut(&ckey)
        {
            c.messages.retain(|m| m.id != id);
            self.messages_rev += 1;
            self.bot.asked.remove(id);
        }
    }
}

/// The server says it already holds message `id`, so nothing is left to
/// send. `ingest` starts a question's @bot run only while the question is in
/// the outbox, and history may file it after this (the replay raced the
/// page, or the message is parked). So a question waits in `waiting`, which
/// `ingest` starts once history is read.
fn stored_already(
    outbox: &mut BTreeMap<String, String>,
    messages: &[Msg],
    waiting: &mut Vec<String>,
    id: &str,
) {
    if outbox.remove(id).is_none() {
        return;
    }
    let asks = messages
        .iter()
        .any(|m| m.id == id && m.body.kind == "text" && m.body.mentions.iter().any(|m| m == "bot"));
    if asks && !waiting.iter().any(|w| w == id) {
        waiting.push(id.to_string());
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

const NO_KEY: &str = "This device does not hold the team's current key yet.";

/// A pending rotation is its own wait: the server would still take the old
/// key, but a removed member holds it, so nothing new is sealed with it.
fn no_key_reason(detail: &TeamDetail) -> &'static str {
    if detail.rotation_pending && detail.keyed_epoch.is_some_and(|k| k > 0) {
        "Someone left this team, so it needs a new key before anyone can send. An admin's Arakun makes it the next time it opens the team."
    } else {
        NO_KEY
    }
}

/// Whether the Worker takes this frame. Over either limit it would refuse it.
fn fits(frame: &str, wire: &WireMessage) -> bool {
    frame.len() <= MAX_FRAME && crypto::unb64(&wire.ct).is_some_and(|ct| ct.len() <= MAX_MESSAGE_CT)
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

/// An approval's keys in requests of at most [`MAX_ENVELOPES`] for `devices`
/// devices: the newest first, each part ascending.
fn grant_parts(keys: &[(u64, TeamKey)], devices: usize) -> Vec<Vec<(u64, TeamKey)>> {
    let per = (MAX_ENVELOPES / devices.max(1)).max(1);
    keys.rchunks(per).map(<[_]>::to_vec).collect()
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

/// The keys from the newest back to the first gap, since an approval must
/// cover every keyed epoch from its first. A gap of epochs `skipped(older,
/// newer)` knows were skipped is no gap: they have no key to give.
fn contiguous_tail(
    keys: &[(u64, TeamKey)],
    skipped: impl Fn(u64, u64) -> bool,
) -> Vec<(u64, TeamKey)> {
    let mut out: Vec<(u64, TeamKey)> = Vec::new();
    for (epoch, key) in keys.iter().rev() {
        if out
            .last()
            .is_some_and(|(e, _)| *e != epoch + 1 && !skipped(*epoch, *e))
        {
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

/// Members no team key may go to: a device of theirs doesn't check out, or
/// the user compared numbers with them and found them different.
fn doubted(kept: &Kept, t: &TeamState) -> Vec<String> {
    let mut who = t.untrusted.clone();
    for m in &t.detail.members {
        let devices = t.devices.get(&m.github_id).into_iter().flatten();
        if !who.contains(&m.login) && devices.into_iter().any(|d| kept.mismatched(&d.device_id)) {
            who.push(m.login.clone());
        }
    }
    who
}

fn next_backfill(kept: &Kept, t: &TeamState, mine: &str) -> Vec<KeyDelivery> {
    let team = &t.detail.team_id;
    let keys = kept.keys(team);
    let doubted = doubted(kept, t);
    let mut batch = Vec::new();
    for (m, d) in recipients(t) {
        if d.device_id == mine || d.created_at < m.joined_at || doubted.contains(&m.login) {
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

/// Fails when the user said "They don't" to a device after the upload was
/// planned. A request already sent is not undone; the next one doesn't go.
fn still_trusted(doubts: &AtomicU64, planned: u64) -> Result<(), ApiError> {
    if doubts.load(Ordering::SeqCst) == planned {
        Ok(())
    } else {
        Err(ApiError::Status(
            0,
            "A device check failed meanwhile, so key delivery stopped.".into(),
        ))
    }
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
    invites: Option<Vec<TeamInvite>>,
    epoch_made: HashMap<u64, u64>,
    approved_by: Option<String>,
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
    let dated = call.dated_envelopes(team_id)?;
    let mut epoch_made = HashMap::new();
    for d in &dated {
        if let Some(at) = d.made_at {
            epoch_made.insert(d.envelope.epoch, at);
        }
    }
    // Who sealed this device's oldest key: the admin who let it in.
    let approved_by = dated
        .iter()
        .min_by_key(|d| d.envelope.epoch)
        .and_then(|d| {
            devices
                .iter()
                .find(|(_, ds)| ds.iter().any(|x| x.device_id == d.envelope.from))
        })
        .and_then(|(gid, _)| detail.members.iter().find(|m| m.github_id == *gid))
        .map(|m| m.login.clone());
    for e in dated.into_iter().map(|d| d.envelope) {
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
    let (pending, invites) = if admin {
        // An older server has no list of open invites.
        (call.pending(team_id)?, call.team_invites(team_id).ok())
    } else {
        (Vec::new(), None)
    };
    Ok(TeamRead {
        detail,
        devices,
        untrusted,
        keys,
        bad_envelopes: bad,
        pending,
        invites,
        epoch_made,
        approved_by,
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
    fn a_refusal_fails_only_a_message_still_on_its_way() {
        let msg = |id: &str, delivery| Msg {
            id: id.into(),
            seq: None,
            at: 0,
            github_id: 21,
            login: "a".into(),
            body: Body::text("hi".to_string(), None),
            delivery,
        };
        let mut messages = vec![msg("sent", Delivery::Sent), msg("new", Delivery::Sending)];
        // A replay of a stored message refused for another reason.
        assert!(!refuse(
            &mut messages,
            "sent",
            "too many messages, slow down"
        ));
        assert_eq!(messages[0].delivery, Delivery::Sent);
        assert!(refuse(&mut messages, "new", "too many messages, slow down"));
        assert_eq!(
            messages[1].delivery,
            Delivery::Failed("too many messages, slow down".into())
        );
        assert!(!refuse(&mut messages, "gone", "refused"));
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
    fn a_bot_question_the_server_already_holds_still_runs() {
        let msg = |id: &str, text: &str, bot: bool| {
            let mut body = Body::text(text, None);
            if bot {
                body.mentions = vec!["bot".into()];
            }
            Msg {
                id: id.into(),
                seq: None,
                at: 0,
                github_id: 1,
                login: "dani".into(),
                body,
                delivery: Delivery::Sending,
                edited: None,
            }
        };
        let messages = [msg("q", "@bot review", true), msg("plain", "hello", false)];
        let mut outbox: BTreeMap<String, String> = ["q", "plain"]
            .into_iter()
            .map(|id| (id.to_string(), "frame".to_string()))
            .collect();
        let mut waiting = Vec::new();
        stored_already(&mut outbox, &messages, &mut waiting, "q");
        stored_already(&mut outbox, &messages, &mut waiting, "plain");
        assert!(outbox.is_empty());
        assert_eq!(waiting, ["q"]);
        // History filed it first and started the run: the late reply to the
        // replay must not start a second one.
        let mut waiting = Vec::new();
        stored_already(&mut outbox, &messages, &mut waiting, "q");
        assert!(waiting.is_empty());
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
            first_key_epoch: None,
        };
        let members = vec![m(1, "karlo"), m(2, "rik"), m(3, "dani")];
        let got: Vec<(&str, bool)> = here_now(&members, &[3, 1])
            .into_iter()
            .map(|(m, on)| (m.login.as_str(), on))
            .collect();
        assert_eq!(got, vec![("karlo", true), ("dani", true), ("rik", false)]);
    }

    #[test]
    fn edits_apply_by_the_same_sender_newest_first() {
        let m = |id: &str, seq: u64, gid: u64, body: Body| Msg {
            id: id.into(),
            seq: Some(seq),
            at: seq,
            github_id: gid,
            login: "x".into(),
            body,
            delivery: Delivery::Sent,
            edited: None,
        };
        let edit = |text: &str, of: &str| {
            let mut b = Body::text(text, Some(of.into()));
            b.kind = "edit".into();
            b
        };
        let mut all = vec![
            m("a", 1, 7, Body::text("first", None)),
            m("e2", 4, 7, edit("third", "a")),
            m("e1", 3, 7, edit("second", "a")),
            // Someone else's edit, and an edit of a non-text message.
            m("x", 5, 8, edit("forged", "a")),
            m("b", 2, 8, Body::text("bot", None)),
        ];
        all[4].body.kind = "bot".into();
        all.push(m("y", 6, 8, edit("nope", "b")));
        apply_edits(&mut all);
        assert_eq!(all[0].body.text, "third");
        let e = all[0].edited.as_ref().unwrap();
        assert_eq!(
            (e.id.as_str(), e.seq, e.original.as_str()),
            ("e2", Some(4), "first")
        );
        assert_eq!(all[4].body.text, "bot");
        assert!(all[4].edited.is_none());
        // Applying again changes nothing.
        apply_edits(&mut all);
        assert_eq!(all[0].body.text, "third");
        assert_eq!(all[0].edited.as_ref().unwrap().original, "first");
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
    fn a_pending_rotation_is_not_a_missing_key() {
        let mut t = delivery_team(vec![], 4).detail;
        assert_eq!(no_key_reason(&t), NO_KEY);
        t.epoch = 5;
        t.rotation_pending = true;
        assert!(no_key_reason(&t).starts_with("Someone left this team"));
        // A new team waits for its first key, not for a rotation.
        t.keyed_epoch = Some(0);
        assert_eq!(no_key_reason(&t), NO_KEY);
    }

    #[test]
    fn a_message_over_the_workers_limits_does_not_fit() {
        let me = Device::generate();
        let seal = |text: &str| {
            let body = Body::text(text, None);
            let wire = crypto::seal_message("T", "C", &crypto::ulid(), 1, &[1; 32], &me, &body);
            let frame = serde_json::json!({ "type": "send", "message": wire }).to_string();
            (frame, wire)
        };
        let (frame, wire) = seal("hello");
        assert!(fits(&frame, &wire));
        let (frame, wire) = seal(&"x".repeat(MAX_MESSAGE_CT));
        assert!(frame.len() <= MAX_FRAME);
        assert!(!fits(&frame, &wire));
        let (_, small) = seal("hello");
        assert!(!fits(&"x".repeat(MAX_FRAME + 1), &small));
    }

    #[test]
    fn approvals_give_keys_back_to_the_first_gap() {
        let k = |e: u64| (e, [e as u8; 32]);
        let none = |_: u64, _: u64| false;
        assert_eq!(
            contiguous_tail(&[k(1), k(2), k(3)], none),
            vec![k(1), k(2), k(3)]
        );
        assert_eq!(
            contiguous_tail(&[k(1), k(2), k(4), k(5)], none),
            vec![k(4), k(5)]
        );
        assert!(contiguous_tail(&[], none).is_empty());
    }

    #[test]
    fn approvals_go_past_an_epoch_seen_skipped() {
        let k = |e: u64| (e, [e as u8; 32]);
        let mut t = delivery_team(vec![], 1).detail;
        let mut kept = Kept::default();
        // Two removals before a rotation: epoch 3 is current, 2 never keyed.
        t.epoch = 3;
        t.rotation_pending = true;
        assert!(kept.observe_skips(&t));
        assert!(!kept.observe_skips(&t));
        let skipped = |older, newer| kept.skipped_between("T", older, newer);
        let held = [k(1), k(3), k(4)];
        assert_eq!(contiguous_tail(&held, skipped), held.to_vec());
        // A key this device lacks is still a gap.
        let held = [k(1), k(2), k(4), k(5)];
        assert_eq!(contiguous_tail(&held, skipped), vec![k(4), k(5)]);
        // Epoch 3 may still get a key: only 2 is known to be skipped.
        assert!(!kept.skipped_between("T", 1, 4));
        // A pending rotation right after the keyed epoch skips nothing.
        let mut next = delivery_team(vec![], 4).detail;
        next.epoch = 5;
        next.rotation_pending = true;
        assert!(!kept.observe_skips(&next));
    }

    #[test]
    fn an_approval_too_big_for_one_request_goes_newest_first() {
        let keys: Vec<(u64, TeamKey)> = (1..=26).map(|e| (e, [e as u8; 32])).collect();
        let parts = grant_parts(&keys, 20);
        let epochs: Vec<Vec<u64>> = parts
            .iter()
            .map(|p| p.iter().map(|(e, _)| *e).collect())
            .collect();
        assert_eq!(epochs, vec![(2..=26).collect::<Vec<_>>(), vec![1]]);
        assert!(parts.iter().all(|p| p.len() * 20 <= MAX_ENVELOPES));
        assert_eq!(grant_parts(&keys[..3], 2), vec![keys[..3].to_vec()]);
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

    #[test]
    fn a_failed_check_stops_uploads_planned_before_it() {
        let doubts = AtomicU64::new(0);
        let planned = doubts.load(Ordering::SeqCst);
        assert!(still_trusted(&doubts, planned).is_ok());
        doubts.fetch_add(1, Ordering::SeqCst);
        assert!(still_trusted(&doubts, planned).is_err());
        assert!(still_trusted(&doubts, doubts.load(Ordering::SeqCst)).is_ok());
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
            invites: None,
            epoch_made: HashMap::new(),
            approved_by: None,
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
        let approved = seal_all(&net, "T", &contiguous_tail(&held, |_, _| false), &[old]).unwrap();
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
        // "They don't" on the Compare panel stops every key to that person.
        kept.set_verified(&new.device_id, false);
        assert!(next_backfill(&kept, &t, "").is_empty());
        assert_eq!(doubted(&kept, &t), vec![t.detail.members[0].login.clone()]);
        kept.set_verified(&new.device_id, true);
        assert!(doubted(&kept, &t).is_empty());
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
        let envelopes = seal_all(
            &a,
            &team,
            &contiguous_tail(&[(1, key)], |_, _| false),
            &theirs,
        )
        .unwrap();
        let reviewed: Vec<String> = p.devices.iter().map(|d| d.device_id.clone()).collect();
        a.call()
            .approve(
                &team,
                mb.github_id,
                &p.request_id,
                &reviewed,
                Some(1),
                &envelopes,
            )
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

    #[test]
    fn only_the_sockets_asked_for_longest_ago_are_closed() {
        let open = |n: u64| -> Vec<(String, u64, bool)> {
            (1..=n).map(|i| (format!("t/c{i}"), i, false)).collect()
        };
        // Up to the cap besides the one in view, nothing closes.
        assert!(spare_links(open(3), "t/c3", 2).is_empty());
        assert!(spare_links(open(9), "t/c9", SPARE_LINKS).is_empty());
        // Beyond it the oldest go, oldest last.
        assert_eq!(spare_links(open(5), "t/c5", 2), ["t/c2", "t/c1"]);
        // The one in view stays however long ago it was asked for, and
        // takes none of the spare places.
        assert_eq!(spare_links(open(5), "t/c1", 2), ["t/c3", "t/c2"]);
        // With none in view the same number of spare ones stay.
        assert_eq!(spare_links(open(3), "", 2), ["t/c1"]);
    }

    #[test]
    fn a_busy_channel_keeps_its_socket() {
        let mut open: Vec<(String, u64, bool)> =
            (1..=5).map(|i| (format!("t/c{i}"), i, false)).collect();
        // Still reading history, or holding a message not acknowledged yet.
        open[0].2 = true;
        assert_eq!(spare_links(open.clone(), "t/c5", 2), ["t/c2"]);
        // A busy one among the newest still takes its place.
        open[3].2 = true;
        assert_eq!(spare_links(open, "t/c5", 2), ["t/c2"]);
    }
}
