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
    ApiError, Call, DeviceRow, Endorsement, InviteRow, InviteeState, Me, MemberRow, MutedTeam,
    PendingRow, Remind, Role, SenderRow, StaleEnvelope, Stored, TeamDetail, TeamInvite, TeamRow,
    WaitingRow,
};
use super::coord::{self, Event as PeerEvent, Grant};
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

/// The account, membership and key state a row was drawn for. An action
/// started from that row runs, and its answer lands, only while it holds.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct Roster {
    account: u64,
    membership: u64,
    epoch: u64,
    keyed_epoch: Option<u64>,
    rotation_pending: bool,
}

pub struct TeamState {
    pub detail: TeamDetail,
    pub role: Role,
    /// Each member's devices whose endorsement chain checks out.
    pub devices: HashMap<u64, Vec<DeviceRow>>,
    /// Members with a device whose chain does not check out.
    pub untrusted: Vec<String>,
    /// People who wrote in the team's channels and are not members now, with
    /// their devices whose chain checks out, so their messages still show.
    pub former: HashMap<u64, Former>,
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
    /// For admins: members' devices holding a key whose sender is no longer
    /// an owner's or admin's device, which they refuse (see [`next_backfill`]).
    stale: HashSet<StaleEnvelope>,
    devices_read: Instant,
}

/// Someone who wrote in a team's channels and has left it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Former {
    pub login: String,
    /// Their devices whose endorsement chain checks out.
    pub devices: Vec<DeviceRow>,
}

impl TeamState {
    pub fn login(&self, github_id: u64) -> String {
        self.detail
            .members
            .iter()
            .find(|m| m.github_id == github_id)
            .map(|m| m.login.clone())
            .or_else(|| self.former.get(&github_id).map(|f| f.login.clone()))
            .unwrap_or_else(|| "someone".into())
    }

    /// Whether this person is not a member of the team now: their messages
    /// show as from a former member.
    pub fn is_former(&self, github_id: u64) -> bool {
        !self.detail.members.iter().any(|m| m.github_id == github_id)
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

/// Marks the message `id` as refused by the server, with why. Only one still
/// on its way: a replay of a stored message may be refused for another
/// reason (a rate limit, say), and that must not offer to send it twice. A
/// refused edit showed while it was on its way, so the edits are applied
/// again: the message goes back to the text the server has.
fn refuse(messages: &mut [Msg], id: &str, why: &str) -> bool {
    let Some(m) = messages
        .iter_mut()
        .find(|m| m.id == id && m.delivery == Delivery::Sending)
    else {
        return false;
    };
    m.delivery = Delivery::Failed(why.to_string());
    if m.body.kind == "edit" {
        apply_edits(messages);
    }
    true
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

/// What checking and opening a channel's stored messages needs, copied off
/// the hub: a long history is checked and opened on a background thread,
/// and the window stays responsive meanwhile.
#[derive(Clone)]
pub(super) struct Opener {
    team_id: String,
    channel_id: String,
    /// Each known device's member and signing key (base64), by device ID.
    senders: HashMap<String, (u64, String)>,
    /// The team keys this device holds, by epoch.
    keys: HashMap<u64, TeamKey>,
}

/// What became of one stored message.
#[derive(Debug, PartialEq)]
pub(super) enum Opened {
    /// From a device not known (yet).
    Unknown,
    /// Its signature, its sender or its sealing did not check out.
    Hidden,
    /// Sealed with a key this device does not hold.
    Locked,
    Open {
        github_id: u64,
        body: Box<Body>,
    },
}

impl Opener {
    fn new(team_id: &str, channel_id: &str, t: &TeamState, kept: &Kept) -> Self {
        Opener {
            team_id: team_id.to_string(),
            channel_id: channel_id.to_string(),
            // Former members too: their messages still show.
            senders: t
                .devices
                .iter()
                .chain(t.former.iter().map(|(gid, f)| (gid, &f.devices)))
                .flat_map(|(gid, ds)| {
                    ds.iter()
                        .map(|d| (d.device_id.clone(), (*gid, d.sig_pub.clone())))
                })
                .collect(),
            keys: kept.keys(team_id).into_iter().collect(),
        }
    }

    /// Checks one message's signature against its sender's device, then opens it.
    fn open(&self, s: &Stored) -> Opened {
        let Some((gid, sig_pub)) = self.senders.get(&s.message.device_id) else {
            return Opened::Unknown;
        };
        let Some(sig_pub) = crypto::unb64(sig_pub) else {
            return Opened::Hidden;
        };
        if !crypto::verify_message(&self.team_id, &self.channel_id, &s.message, &sig_pub) {
            return Opened::Hidden;
        }
        let Some(key) = self.keys.get(&s.message.epoch) else {
            return Opened::Locked;
        };
        match crypto::open_message(&self.team_id, &self.channel_id, &s.message, key) {
            Ok(body) => Opened::Open {
                github_id: *gid,
                body: Box::new(body),
            },
            Err(_) => Opened::Hidden,
        }
    }

    /// Whether `other` would open `s` as this one does: the same sender
    /// device and the same key for its epoch.
    fn agrees(&self, other: &Opener, s: &Stored) -> bool {
        let m = &s.message;
        self.team_id == other.team_id
            && self.channel_id == other.channel_id
            && self.senders.get(&m.device_id) == other.senders.get(&m.device_id)
            && self.keys.get(&m.epoch) == other.keys.get(&m.epoch)
    }
}

/// Checks and opens a batch of stored messages. A long history makes this
/// costly, so it runs off the UI thread. With no opener (the team is not
/// read yet) every message stays unknown.
pub(super) fn open_batch(opener: Option<&Opener>, stored: Vec<Stored>) -> Vec<(Stored, Opened)> {
    stored
        .into_iter()
        .map(|s| {
            let opened = opener.map_or(Opened::Unknown, |o| o.open(&s));
            (s, opened)
        })
        .collect()
}

/// A batch opened with `opened_with`, as `now` opens it. A device list read
/// again or a key that arrived while the batch was opened changes what some
/// messages come to; only those are opened again.
fn reopen(
    opened_with: Option<&Opener>,
    now: &Opener,
    batch: Vec<(Stored, Opened)>,
) -> Vec<(Stored, Opened)> {
    batch
        .into_iter()
        .map(|(s, opened)| match opened_with {
            Some(o) if o.agrees(now, &s) => (s, opened),
            _ => {
                let opened = now.open(&s);
                (s, opened)
            }
        })
        .collect()
}

impl ChannelState {
    /// Messages sealed with a key this device does not hold.
    pub fn locked(&self) -> usize {
        self.locked.len()
    }

    /// How many stored messages from `first` to `last` this device could
    /// not open: sealed with a key it does not hold, or from a device it
    /// does not know yet.
    pub(super) fn unopened_between(&self, first: u64, last: u64) -> usize {
        unopened(
            self.locked.iter().chain(&self.unknown).map(|s| s.seq),
            first,
            last,
        )
    }

    /// Whether the channel has a socket, connected or still dialing.
    pub fn linked(&self) -> bool {
        self.link.is_some()
    }

    /// A channel nothing was read of yet, with no socket.
    fn new(team_id: &str, channel_id: &str) -> Self {
        ChannelState {
            team_id: team_id.to_string(),
            channel_id: channel_id.to_string(),
            messages: Vec::new(),
            history: Cursor::default(),
            link_id: 0,
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
            rekey: Vec::new(),
            link: None,
            shown: Instant::now(),
            seen_ids: HashSet::new(),
            here: Vec::new(),
        }
    }
}

/// Moves presence from the channel picked before to the one picked now: the
/// old socket goes quiet, the new one counts. Neither hangs up.
fn move_presence(
    channels: &mut HashMap<String, ChannelState>,
    from: Option<&str>,
    to: Option<&str>,
) {
    if from == to {
        return;
    }
    if let Some(c) = from.and_then(|k| channels.get_mut(k)) {
        if let Some(link) = &c.link {
            link.set_counted(false);
        }
        // A quiet socket hears no more lists, so this one would go stale.
        c.here.clear();
    }
    if let Some(link) = to
        .and_then(|k| channels.get(k))
        .and_then(|c| c.link.as_ref())
    {
        link.set_counted(true);
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
    /// Messages the server refused because a removal retired their key, with
    /// the epoch each was sealed with. They are sealed again and sent once
    /// this device holds a newer key: the server stored none of them.
    rekey: Vec<(String, u64)>,
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

    /// The last message with none missing before it: what counts as read.
    /// A live one past a gap does not, the ones before it are still unread.
    fn shown(&self) -> u64 {
        self.through
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
    /// Teams whose invites this user asked not to see. Empty on an older
    /// server, which has no such list.
    pub muted: Vec<MutedTeam>,
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
    /// The one paired agent review this device takes part in, if any.
    pub(super) peer_review: Option<Grant>,
    /// The channel the tab shows, by channel key: what arrives there is read.
    viewing: Option<String>,
    /// The channel picked in the tab, by channel key, on screen or not. Only
    /// its socket counts toward presence: the others (read for Decisions, or
    /// kept open after the user moved on) are quiet, so other members do not
    /// see this user under Here now there.
    present: Option<String>,
    /// Unread counts of channels with no socket here, from their history.
    unread_read: HashMap<String, usize>,
    /// When those were last read.
    unread_at: Option<Instant>,
    /// When an invite was sent or withdrawn from here, by team. A team read
    /// that started before then keeps the invite list as it is here.
    invites_changed: HashMap<String, Instant>,
    /// The newest `seq` of each of those channels when it was counted: one
    /// that has not moved since is not read again.
    unread_newest: HashMap<String, u64>,
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
    /// The server answered 404 to `GET /devices`: it is older than that call,
    /// so team reads ask for each member's devices.
    devices_per_user: bool,
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
            muted: Vec::new(),
            team: HashMap::new(),
            channels: HashMap::new(),
            notices: Vec::new(),
            my_devices: Vec::new(),
            error: None,
            busy: HashSet::new(),
            bot: super::bot::Bots::default(),
            peer_review: None,
            viewing: None,
            present: None,
            unread_read: HashMap::new(),
            unread_at: None,
            unread_newest: HashMap::new(),
            invites_changed: HashMap::new(),
            events: tx,
            generation: 0,
            messages_rev: 0,
            roster_version: 0,
            applied_roster: 0,
            membership: HashMap::new(),
            setup: HashMap::new(),
            reread: HashSet::new(),
            devices_per_user: false,
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

    /// The team as this device has it now, while its list row and its detail
    /// agree on the epoch and this user's role.
    pub(super) fn roster(&self, team: &str) -> Option<Roster> {
        let state = self.team.get(team)?;
        let row = self.teams.iter().find(|r| r.team_id == team)?;
        // A rotation can finish within one epoch, and it changes which keys
        // are live, so it counts too.
        if row.epoch != state.detail.epoch
            || row.role != state.role
            || row.rotation_pending != state.detail.rotation_pending
        {
            return None;
        }
        Some(Roster {
            account: self.generation,
            membership: *self.membership.get(team)?,
            epoch: row.epoch,
            keyed_epoch: state.detail.keyed_epoch,
            rotation_pending: row.rotation_pending,
        })
    }

    /// Whether the session the calls go out with is still the signed-in one.
    fn current_account(&self, cx: &Context<Self>) -> bool {
        self.net.as_ref().is_some_and(|net| {
            self.teams_entity
                .read(cx)
                .signed()
                .is_some_and(|(base, session, me)| {
                    base == net.base && session == net.session && me.github_id == self.github_id()
                })
        })
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
            if !self.teams_entity.read(cx).enabled() {
                self.peer_review = None;
            }
            return;
        }
        self.generation += 1;
        self.peer_review = None;
        self.net = None;
        self.sealer = None;
        self.kept = Kept::default();
        self.teams.clear();
        self.membership.clear();
        self.setup.clear();
        self.reread.clear();
        self.devices_per_user = false;
        self.invites.clear();
        self.muted.clear();
        self.team.clear();
        self.channels.clear();
        self.messages_rev += 1;
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
        self.present = None;
        self.unread_read.clear();
        self.unread_at = None;
        self.unread_newest.clear();
        self.invites_changed.clear();
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
                if this
                    .update(cx, |hub, cx| {
                        hub.expire_review();
                        hub.refresh(cx);
                    })
                    .is_err()
                {
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
        let per_user = self.devices_per_user;
        self.run(
            cx,
            move |net| {
                let teams = net.call().teams()?;
                let invites = net.call().invites()?;
                // An older server has no list of requests; the teams file
                // still remembers this device's.
                let waiting = net.call().waiting().ok();
                let muted = net.call().muted().unwrap_or_default();
                // Every member's devices once, for all the team reads below.
                let directory = if per_user || teams.is_empty() {
                    Directory::Ask
                } else {
                    match net.call().people_devices(None) {
                        Ok(people) => Directory::Read(Arc::new(people)),
                        Err(ApiError::Status(404, _)) => Directory::PerUser,
                        // Each team read asks again and shows its own error.
                        Err(_) => Directory::Ask,
                    }
                };
                Ok((teams, invites, waiting, muted, directory))
            },
            move |hub, result, cx| match result {
                _ if version < hub.applied_roster => {}
                Ok((teams, invites, waiting, muted, directory)) => {
                    if matches!(directory, Directory::PerUser) {
                        hub.devices_per_user = true;
                    }
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
                        if hub.peer_review.as_ref().is_some_and(|g| {
                            hub.channels.get(&g.channel).is_some_and(|c| c.team_id == t)
                        }) {
                            hub.peer_review = None;
                        }
                        if let Some(was) = hub.team.get(&t) {
                            hub.info = Some(format!("You're no longer in {}.", was.detail.name));
                        }
                        hub.team.remove(&t);
                        hub.setup.remove(&t);
                        hub.notices.retain(|n| n.team_id != t);
                        hub.channels.retain(|_, c| c.team_id != t);
                        hub.messages_rev += 1;
                        changed |= hub.kept.forget_team(&t);
                    }
                    changed |= hub.kept.retain_teams(&ids);
                    if changed {
                        hub.save(cx);
                    }
                    update_memberships(&mut hub.membership, &hub.teams, &teams, version);
                    hub.teams = teams;
                    hub.invites = invites;
                    hub.muted = muted;
                    let ids: Vec<String> = hub.teams.iter().map(|t| t.team_id.clone()).collect();
                    for id in ids {
                        hub.reload_team(&id, false, directory.clone(), cx);
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
        self.reload_team(team_id, true, Directory::Ask, cx);
    }

    /// `after_change`: asked for after something changed the team, so a read
    /// on its way may be out of date. The 20 s refresh only reads again; were
    /// it to drop a read on its way too, one slower than that would never land.
    fn reload_team(
        &mut self,
        team_id: &str,
        after_change: bool,
        mut directory: Directory,
        cx: &mut Context<Self>,
    ) {
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
        let unknown = self.unknown_senders(team_id);
        let team_id = team_id.to_string();
        let admin = row.role.is_admin();
        let asked = Instant::now();
        if self.devices_per_user {
            directory = Directory::PerUser;
        }
        self.run(
            cx,
            {
                let team_id = team_id.clone();
                move |net| read_team(net, &team_id, admin, &held, &directory, &unknown)
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
                    Ok(mut read) => {
                        let changed = hub
                            .invites_changed
                            .get(&team_id)
                            .is_some_and(|at| *at >= asked);
                        let here = hub.team.get(&team_id).and_then(|t| t.invites.clone());
                        read.invites = invites_after_read(read.invites, here, changed);
                        hub.team_read(&team_id, row.role, read, cx)
                    }
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

    /// The devices of messages in this team's channels that wait for their
    /// sender, each with a channel that holds one, for read_team to look up.
    fn unknown_senders(&self, team_id: &str) -> Vec<(String, String)> {
        let mut out: Vec<(String, String)> = Vec::new();
        let former = self.team.get(team_id).map(|t| &t.former);
        for c in self.channels.values().filter(|c| c.team_id == team_id) {
            for s in &c.unknown {
                let device = &s.message.device_id;
                let known = former.is_some_and(|f| {
                    f.values()
                        .flat_map(|f| &f.devices)
                        .any(|d| &d.device_id == device)
                });
                if !known && !out.iter().any(|(_, d)| d == device) {
                    out.push((c.channel_id.clone(), device.clone()));
                }
            }
        }
        out
    }

    fn team_read(&mut self, team_id: &str, role: Role, read: TeamRead, cx: &mut Context<Self>) {
        let mut changed = false;
        for (&gid, devices) in &read.devices {
            let known = self.kept.roots.contains_key(&gid);
            if !devices.is_empty() && !pin_directory(&mut self.kept, gid, devices) {
                self.team.remove(team_id);
                self.channels.retain(|_, c| c.team_id != team_id);
                self.messages_rev += 1;
                self.error = Some(format!(
                    "The trusted device for account {gid} changed. Key delivery is paused."
                ));
                return;
            }
            changed |= !known && !devices.is_empty();
        }
        let mut former = self
            .team
            .get(team_id)
            .map(|t| t.former.clone())
            .unwrap_or_default();
        changed |= adopt_former(&mut self.kept, &mut former, read.former);
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
        let problem = key_problem(read.bad_envelopes, read.from_non_admins);
        let rotation = read.detail.rotation_pending;
        let epoch = read.detail.epoch;
        if self.peer_review.as_ref().is_some_and(|g| {
            self.channels
                .get(&g.channel)
                .is_some_and(|c| c.team_id == team_id)
                && (rotation
                    || g.epoch != read.detail.epoch
                    || !read.detail.members.iter().any(|m| m.github_id == g.peer))
        }) {
            self.peer_review = None;
        }
        self.team.insert(
            team_id.to_string(),
            TeamState {
                detail: read.detail,
                role,
                devices: read.devices,
                untrusted: read.untrusted,
                former,
                channels,
                pending: read.pending,
                invites: read.invites,
                epoch_made: read.epoch_made,
                approved_by: read.approved_by,
                keyless,
                problem,
                stale: read.stale,
                devices_read: Instant::now(),
            },
        );
        if let Some(row) = self.teams.iter_mut().find(|t| t.team_id == team_id) {
            row.rotation_pending = rotation;
            // A rotation made on another device reaches the detail first, and
            // `roster` needs the two to agree; epochs only grow.
            row.epoch = row.epoch.max(epoch);
        }
        // Messages that waited on a device list, and any sealed with a key
        // that just arrived, get another look. Only the senders this read
        // looked for are given up on; one that came in meanwhile waits.
        let keys: Vec<String> = self
            .channels
            .iter()
            .filter(|(_, c)| c.team_id == team_id)
            .map(|(k, _)| k.clone())
            .collect();
        for key in keys {
            self.reopen_waiting(&key, read.looked_up.clone(), cx);
        }
        self.resend_rekeyed(team_id, cx);
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
    /// Nor does a device whose key came from someone no longer an owner or
    /// admin, which the server lists as stale: see [`next_backfill`].
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
        // Say why nothing went out; the view keeps the login typed.
        if self.roster(team_id).is_none() {
            self.error = Some(format!(
                "The team is still loading. Invite @{login} again in a moment."
            ));
            cx.notify();
            return;
        }
        if !self.team[team_id].role.is_admin() {
            return;
        }
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
                move |net| net.call().invite(&team_id, &login).map(|i| (i, login))
            },
            move |hub, result, cx| {
                hub.busy.remove(&busy);
                // The server has acted or refused by now, whatever the roster
                // did meanwhile: the error and the reload hold for any roster.
                match result {
                    // Listed at once: a team read already under way does not
                    // know of it, and an older server lists no invites.
                    Ok((invited, typed)) => {
                        let by = hub.me.as_ref().map(|m| m.login.clone());
                        let shown = if invited.login.is_empty() {
                            typed
                        } else {
                            invited.login
                        };
                        match invite_sent(&shown, invited.invitee_state) {
                            Some(Ok(said)) => hub.info = Some(said),
                            Some(Err(said)) => hub.error = Some(said),
                            None => {}
                        }
                        if let Some(t) = hub.team.get_mut(&team_id) {
                            let login = shown.to_ascii_lowercase();
                            let list = t.invites.get_or_insert_with(Vec::new);
                            list.retain(|i| i.login != login);
                            list.insert(
                                0,
                                TeamInvite {
                                    invite_id: invited.invite_id,
                                    login,
                                    invited_by_login: by,
                                    created_at: now(),
                                    expires_at: invited.expires_at,
                                    invitee_state: invited.invitee_state,
                                },
                            );
                        }
                        hub.invites_changed.insert(team_id.clone(), Instant::now());
                        hub.load_team(&team_id, cx);
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

    /// Takes back an invite nobody answered yet.
    pub(super) fn withdraw(
        &mut self,
        team_id: &str,
        scope: Roster,
        invite_id: &str,
        cx: &mut Context<Self>,
    ) {
        if self.roster(team_id) != Some(scope)
            || !self.team[team_id].role.is_admin()
            || !self.team[team_id]
                .invites
                .iter()
                .flatten()
                .any(|i| i.invite_id == invite_id)
        {
            return;
        }
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
                let (team_id, invite_id) = (team_id.clone(), invite_id.clone());
                move |net| net.call().withdraw(&team_id, &invite_id)
            },
            move |hub, result, cx| {
                hub.busy.remove(&busy);
                match result {
                    Ok(()) => {
                        // Gone at once, so Withdraw is not offered again.
                        if let Some(list) =
                            hub.team.get_mut(&team_id).and_then(|t| t.invites.as_mut())
                        {
                            list.retain(|i| i.invite_id != invite_id);
                        }
                        hub.invites_changed.insert(team_id.clone(), Instant::now());
                        hub.load_team(&team_id, cx);
                    }
                    Err(e) => hub.error = Some(format!("Could not withdraw the invite: {e}")),
                }
            },
        );
    }

    /// Turns an invite down: it goes away for good, and the team may not
    /// invite again for a week. With `mute`, not until [`Hub::unmute`].
    pub fn decline(&mut self, invite: &InviteRow, mute: bool, cx: &mut Context<Self>) {
        let kind = if mute { "mute" } else { "decline" };
        let busy = format!("{kind}:{}", invite.invite_id);
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
                move |net| net.call().decline(&id, mute)
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

    /// Shows the team's invites again, and lets it invite at once.
    pub fn unmute(&mut self, team_id: &str, cx: &mut Context<Self>) {
        let busy = format!("unmute:{team_id}");
        if !self.busy.insert(busy.clone()) {
            return;
        }
        self.error = None;
        self.info = None;
        let id = team_id.to_string();
        self.run(
            cx,
            {
                let id = id.clone();
                move |net| net.call().unmute(&id)
            },
            move |hub, result, cx| {
                hub.busy.remove(&busy);
                match result {
                    Ok(()) => {
                        hub.muted.retain(|m| m.team_id != id);
                        hub.refresh(cx);
                    }
                    Err(e) => {
                        hub.error = Some(format!("Could not show the team's invites again: {e}"))
                    }
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
    pub(super) fn approve(
        &mut self,
        team_id: &str,
        scope: Roster,
        reviewed: &PendingRow,
        cx: &mut Context<Self>,
    ) {
        if self.roster(team_id) != Some(scope) || !self.current_account(cx) {
            return;
        }
        let Some(t) = self.team.get(team_id) else {
            return;
        };
        let Some(version) = self.membership.get(team_id).copied() else {
            return;
        };
        let team_epoch = t.detail.epoch;
        let reviewed_epoch = t.detail.keyed_epoch;
        let keyed_epoch = reviewed_epoch.unwrap_or(team_epoch);
        // Only the request as it was shown: the same ID, login and devices.
        if !t.role.is_admin() || !t.pending.contains(reviewed) {
            return;
        }
        let p = reviewed.clone();
        let github_id = p.github_id;
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
                // The server has acted or refused by now. Only the record of
                // what was given belongs to the membership and epoch it was
                // sealed for; the error and the reload hold for any roster.
                let current = hub.membership.get(&team_id) == Some(&version)
                    && hub
                        .team
                        .get(&team_id)
                        .is_some_and(|t| t.detail.epoch == team_epoch);
                match result {
                    Ok((batch, older)) => {
                        if current {
                            hub.kept.admitted(&team_id, github_id, team_epoch, &batch);
                            hub.save(cx);
                        }
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

    pub(super) fn reject(
        &mut self,
        team_id: &str,
        scope: Roster,
        reviewed: &PendingRow,
        cx: &mut Context<Self>,
    ) {
        if self.roster(team_id) != Some(scope)
            || !self.team[team_id].role.is_admin()
            || !self.team[team_id].pending.contains(reviewed)
        {
            return;
        }
        let (github_id, request_id) = (reviewed.github_id, reviewed.request_id.clone());
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
    pub(super) fn remove(
        &mut self,
        team_id: &str,
        scope: Roster,
        member: &MemberRow,
        cx: &mut Context<Self>,
    ) {
        if self.roster(team_id) != Some(scope)
            || !self.team[team_id].detail.members.contains(member)
        {
            return;
        }
        let github_id = member.github_id;
        let team = &self.team[team_id];
        if github_id != self.github_id()
            && (!team.role.is_admin() || (member.role == Role::Owner && team.role != Role::Owner))
        {
            return;
        }
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

    /// Whether this user may give `member` the role `to` in this team as the
    /// row was drawn: only an owner changes roles, and the team keeps one.
    /// The server checks the same, and again as it writes.
    fn may_set_role(&self, team_id: &str, scope: Roster, member: &MemberRow, to: Role) -> bool {
        let Some(t) = self.team.get(team_id) else {
            return false;
        };
        self.roster(team_id) == Some(scope)
            && t.detail.members.contains(member)
            && may_give(t.role, &t.detail.members, member, to)
    }

    /// Makes `member` an owner, an admin or a member. On this user's own row
    /// a lower role steps down, while another owner remains.
    pub(super) fn set_role(
        &mut self,
        team_id: &str,
        scope: Roster,
        member: &MemberRow,
        to: Role,
        cx: &mut Context<Self>,
    ) {
        let busy = format!("role:{team_id}");
        if !self.may_set_role(team_id, scope, member, to) || !self.busy.insert(busy.clone()) {
            return;
        }
        let github_id = member.github_id;
        let team_id = team_id.to_string();
        self.run(
            cx,
            move |net| net.call().set_role(&team_id, github_id, to),
            move |hub, result, cx| {
                hub.busy.remove(&busy);
                match result {
                    Ok(()) => {}
                    Err(ApiError::Status(409, _)) => {
                        hub.error =
                            Some("A team needs an owner. Make someone else an owner first.".into())
                    }
                    Err(e) => hub.error = Some(format!("Could not change the role: {e}")),
                }
                // The role is in the team list too, so read both again.
                hub.refresh(cx);
            },
        );
    }

    /// Hands the team over to `member`: they become an owner, then this user
    /// an admin. In that order, so a failure in between leaves two owners.
    pub(super) fn hand_over(
        &mut self,
        team_id: &str,
        scope: Roster,
        member: &MemberRow,
        cx: &mut Context<Self>,
    ) {
        let me = self.github_id();
        let busy = format!("role:{team_id}");
        if member.github_id == me
            || !self.may_set_role(team_id, scope, member, Role::Owner)
            || !self.busy.insert(busy.clone())
        {
            return;
        }
        let github_id = member.github_id;
        let login = member.login.clone();
        let team_id = team_id.to_string();
        self.run(
            cx,
            move |net| {
                net.call().set_role(&team_id, github_id, Role::Owner)?;
                match net.call().set_role(&team_id, me, Role::Admin) {
                    Ok(()) => Ok(None),
                    Err(ApiError::Unauthorized) => Err(ApiError::Unauthorized),
                    Err(e) => Ok(Some(e)),
                }
            },
            move |hub, result, cx| {
                hub.busy.remove(&busy);
                match result {
                    Ok(None) => {}
                    Ok(Some(e)) => {
                        hub.error = Some(format!(
                            "@{login} is an owner now, and so are you still: {e}"
                        ))
                    }
                    Err(e) => hub.error = Some(format!("Could not hand the team over: {e}")),
                }
                hub.refresh(cx);
            },
        );
    }

    /// Sends again the messages refused for a retired key, sealed with the
    /// key the team has now, once that key is newer than theirs. Until then
    /// they stay as they are, with their Send again.
    fn resend_rekeyed(&mut self, team_id: &str, cx: &mut Context<Self>) {
        let Some((epoch, _)) = self.sending_key(team_id) else {
            return;
        };
        let mut due = Vec::new();
        for c in self.channels.values_mut().filter(|c| c.team_id == team_id) {
            for id in take_rekeyed(&mut c.rekey, epoch) {
                due.push((c.channel_id.clone(), id));
            }
        }
        for (channel_id, id) in due {
            // A message already sent again by hand is gone, and this does
            // nothing.
            self.resend(team_id, &channel_id, &id, cx);
        }
    }

    /// Do not encrypt a new message with a key still held by a removed member.
    fn sending_key(&self, team_id: &str) -> Option<(u64, TeamKey)> {
        self.roster(team_id)?;
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
                counted: self.present.as_deref() == Some(key.as_str()),
            },
            self.events.clone(),
        );
        let in_view = key.clone();
        let state = self
            .channels
            .entry(key)
            .or_insert_with(|| ChannelState::new(team_id, channel_id));
        state.link_id = link_id;
        state.link = Some(link);
        state.shown = Instant::now();
        // Decisions opens channels nobody looks at: the one on screen stays.
        let in_view = self.viewing.clone().unwrap_or(in_view);
        self.close_spare_links(&in_view);
        cx.notify();
    }

    /// Hangs up the channels asked for longest ago, beyond [`SPARE_LINKS`].
    /// What was read stays, and `open_channel` dials again and reads what
    /// the channel missed.
    pub fn close_spare_links(&mut self, in_view: &str) {
        // A paired review needs its channel connected until it ends.
        let paired = self.peer_review.as_ref().map(|g| g.channel.clone());
        let open = self
            .channels
            .iter()
            .filter(|(_, c)| c.link.is_some())
            .map(|(key, c)| {
                let busy = c.loading
                    || c.reading
                    || !c.outbox.is_empty()
                    || !c.bot_waiting.is_empty()
                    || paired.as_deref() == Some(key.as_str());
                (key.clone(), c.shown, busy)
            })
            .collect();
        for key in spare_links(open, in_view, SPARE_LINKS) {
            if let Some(c) = self.channels.get_mut(&key) {
                // Dropping the link stops its thread, so nothing dials again,
                // and `on_socket` ignores what it still sends.
                c.link = None;
                c.status = socket::Status::Connecting;
                // Nobody is known to be here without a socket.
                c.here.clear();
            }
        }
    }

    /// Hangs up a channel's socket and keeps what was read: opening it again
    /// reads on from there. A channel with something still to send, or an
    /// @bot question waiting for its history, keeps its link.
    pub fn close_channel(&mut self, team_id: &str, channel_id: &str, cx: &mut Context<Self>) {
        let key = channel_key(team_id, channel_id);
        // A paired review needs its channel connected until it ends.
        if self.peer_review.as_ref().is_some_and(|g| g.channel == key) {
            return;
        }
        let Some(c) = self.channels.get_mut(&key) else {
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
        c.here.clear();
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
                    if self.peer_review.as_ref().is_some_and(|g| g.channel == key) {
                        self.peer_review = None;
                    }
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
                        self.ingest(key, vec![stored], None, cx);
                        if first_gap {
                            self.catch_up(key, cx);
                        }
                    }
                }
                // A quiet socket hears none, but an older server sends them
                // anyway, and one sent before the socket went quiet may still
                // arrive: only the picked channel's list is kept.
                Some("presence") if self.present.as_deref() == Some(key) => {
                    if let Some(here) = presence(&v) {
                        let c = self.channels.get_mut(key).expect("looked up above");
                        c.here = here;
                    }
                }
                Some("error") => {
                    let id = v["id"].as_str().unwrap_or_default().to_string();
                    let kind = refusal(&v);
                    let why = match kind {
                        Refusal::OldKey => OLD_KEY.to_string(),
                        _ => v["error"].as_str().unwrap_or("refused").to_string(),
                    };
                    let c = self.channels.get_mut(key).expect("looked up above");
                    // The reply to a replay may arrive after catch-up finished.
                    if kind == Refusal::Stored {
                        stored_already(&mut c.outbox, &c.messages, &mut c.bot_waiting, &id);
                        c.needs_history = true;
                        self.catch_up(key, cx);
                    } else if refuse(&mut c.messages, &id, &why) {
                        let epoch = c.outbox.remove(&id).as_deref().map_or(0, frame_epoch);
                        // A review event is not sent again: its exchange ends
                        // here, and a rotation cancels it anyway.
                        let review = c
                            .messages
                            .iter()
                            .any(|m| m.id == id && m.body.kind == "agent_review");
                        if kind == Refusal::OldKey && !review {
                            c.rekey.push((id.clone(), epoch));
                        }
                        self.review_refused(&id, &why);
                        let c = self.channels.get_mut(key).expect("looked up above");
                        // Send again asks with the project open then.
                        self.bot.asked.remove(&id);
                        self.messages_rev += 1;
                        if matches!(kind, Refusal::OldKey | Refusal::Epoch) {
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
        let opener = self.opener(key);
        let key = key.to_string();
        self.run(
            cx,
            // Checked and opened on this thread too: a long history would
            // stall the window on the UI thread.
            move |net| {
                let stored =
                    history_since(after, |seq| net.call().history(&team_id, &channel_id, seq))?;
                let batch = open_batch(opener.as_ref(), stored);
                Ok((opener, batch))
            },
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
                    Ok((opener, stored)) => {
                        if after != 0
                            && stored
                                .first()
                                .is_some_and(|(s, _)| s.seq > after.saturating_add(1))
                        {
                            hub.info = Some(
                                "Some older messages are no longer in this channel's history."
                                    .into(),
                            );
                        }
                        let progress = !stored.is_empty();
                        if let Some((last, _)) = stored.last() {
                            c.history.page(last.seq);
                        }
                        c.needs_history = c.history.gap();
                        hub.ingest_opened(&key, opener.as_ref(), stored, None, cx);
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

    /// What opening this channel's messages needs now; `None` while its
    /// team is not read.
    fn opener(&self, key: &str) -> Option<Opener> {
        let c = self.channels.get(key)?;
        let t = self.team.get(&c.team_id)?;
        Some(Opener::new(&c.team_id, &c.channel_id, t, &self.kept))
    }

    /// Checks, opens and files stored messages here and now: for one that
    /// arrived live, where a trip to a background thread would only delay it.
    fn ingest(
        &mut self,
        key: &str,
        stored: Vec<Stored>,
        looked_up: Option<&HashSet<String>>,
        cx: &mut Context<Self>,
    ) {
        let opener = self.opener(key);
        let batch = open_batch(opener.as_ref(), stored);
        self.ingest_opened(key, opener.as_ref(), batch, looked_up, cx);
    }

    /// Opens the messages that wait for a device list or a key again, off the
    /// UI thread, now that the team was just read: a channel can hold
    /// thousands sealed with a key this device never got. `looked_up`: the
    /// sender devices that read looked for, as for [`Self::ingest_opened`].
    fn reopen_waiting(&mut self, key: &str, looked_up: HashSet<String>, cx: &mut Context<Self>) {
        let Some(c) = self.channels.get(key) else {
            return;
        };
        if c.unknown.is_empty() && c.locked.is_empty() {
            return;
        }
        // They stay where they wait until the result lands, so the counts
        // of locked messages do not flicker meanwhile.
        let waiting: Vec<Stored> = c.unknown.iter().chain(&c.locked).cloned().collect();
        let opener = self.opener(key);
        let generation = self.generation;
        let key = key.to_string();
        cx.spawn(async move |this, cx| {
            let (opener, batch) = cx
                .background_spawn(async move {
                    let batch = open_batch(opener.as_ref(), waiting);
                    (opener, batch)
                })
                .await;
            let _ = this.update(cx, |hub, cx| {
                if hub.generation != generation {
                    return;
                }
                let Some(c) = hub.channels.get_mut(&key) else {
                    return;
                };
                // Only what still waits: another look that landed first may
                // have opened or dropped some, and must not count them twice.
                let still: HashSet<String> = c
                    .unknown
                    .iter()
                    .chain(&c.locked)
                    .map(|s| s.message.id.clone())
                    .collect();
                let batch: Vec<_> = batch
                    .into_iter()
                    .filter(|(s, _)| still.contains(&s.message.id))
                    .collect();
                let taken: HashSet<String> =
                    batch.iter().map(|(s, _)| s.message.id.clone()).collect();
                c.unknown.retain(|s| !taken.contains(&s.message.id));
                c.locked.retain(|s| !taken.contains(&s.message.id));
                hub.ingest_opened(&key, opener.as_ref(), batch, Some(&looked_up), cx);
                cx.notify();
            });
        })
        .detach();
    }

    /// Files checked and opened messages by `seq`. `opened_with` is what
    /// opened them; a message whose sender or key changed since is opened
    /// again here. Ones from a device not known yet wait for the team's
    /// device lists to be read again; once a read just looked for that
    /// device (`looked_up`) and did not find it, those are dropped.
    fn ingest_opened(
        &mut self,
        key: &str,
        opened_with: Option<&Opener>,
        batch: Vec<(Stored, Opened)>,
        looked_up: Option<&HashSet<String>>,
        cx: &mut Context<Self>,
    ) {
        let now = self.opener(key);
        let Some(c) = self.channels.get_mut(key) else {
            return;
        };
        let (Some(now), Some(t)) = (now, self.team.get(&c.team_id)) else {
            c.unknown.extend(batch.into_iter().map(|(s, _)| s));
            return;
        };
        let batch = reopen(opened_with, &now, batch);
        let mut unknown_seen = false;
        // This device's @bot questions the server just stored: the bot runs now.
        let mut asked = Vec::new();
        let mut peer_mail = Vec::new();
        // What this device sent that the server has not echoed yet: a stored
        // message replaces its copy. The rest were filed through here and
        // are in `seen_ids`, so a long channel is not scanned per message.
        let unechoed: HashMap<String, usize> = c
            .messages
            .iter()
            .enumerate()
            .filter(|(_, m)| !c.seen_ids.contains(&m.id))
            .map(|(i, m)| (m.id.clone(), i))
            .collect();
        for (s, opened) in batch {
            if c.seen_ids.contains(&s.message.id) {
                continue;
            }
            let (gid, body) = match opened {
                Opened::Unknown => {
                    if looked_up.is_some_and(|l| l.contains(&s.message.device_id)) {
                        c.hidden += 1;
                    } else {
                        unknown_seen = true;
                        c.unknown.push(s);
                    }
                    continue;
                }
                Opened::Hidden => {
                    c.hidden += 1;
                    continue;
                }
                Opened::Locked => {
                    c.locked.push(s);
                    continue;
                }
                Opened::Open { github_id, body } => (github_id, *body),
            };
            if c.outbox.remove(&s.message.id).is_some()
                && body.kind == "text"
                && body.mentions.iter().any(|m| m == "bot")
            {
                asked.push(s.message.id.clone());
            }
            c.seen_ids.insert(s.message.id.clone());
            if let Some(event) = PeerEvent::read(&body) {
                peer_mail.push((
                    gid,
                    s.message.epoch,
                    event,
                    body.text.clone(),
                    s.message.id.clone(),
                    body.reply_to.clone(),
                ));
            }
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
            match unechoed.get(&msg.id) {
                Some(&i) => c.messages[i] = msg,
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
        self.review_stored(key);
        for (sender, epoch, event, text, id, parent) in peer_mail {
            if event == PeerEvent::Blocked
                && let Some(grant) = &self.peer_review
                && grant.channel == key
                && grant.epoch == epoch
                && sender == grant.peer
                && let Some(request) = self.channels[key]
                    .messages
                    .iter()
                    .find(|m| Some(&m.id) == parent.as_ref() && m.github_id == self.github_id())
                    .and_then(|m| PeerEvent::read(&m.body))
                && grant.waiting_for(&request)
            {
                if let Some(grant) = self.peer_review.as_mut()
                    && let Some(caller) = grant.caller.take()
                {
                    let _ = caller.send(crate::control::Reply::err(
                        "The peer reported a blocked review. See the Teams channel.",
                    ));
                }
                self.peer_review = None;
                continue;
            }
            if let PeerEvent::Withdrawn { ticket } = &event {
                // The request and the withdrawal crossed: nobody will answer.
                if let Some(grant) = self.peer_review.as_mut()
                    && grant.channel == key
                    && sender == grant.peer
                    && grant.waiting_on(ticket)
                {
                    if let Some(caller) = grant.caller.take() {
                        let _ = caller.send(crate::control::Reply::err(
                            "The peer is no longer available. Ask them to pair again.",
                        ));
                    }
                    self.peer_review = None;
                    self.error = Some(
                        "The paired review ended: the other member is no longer available.".into(),
                    );
                }
                continue;
            }
            if let Some(grant) = &mut self.peer_review
                && grant.channel == key
                && grant.epoch == epoch
                && let Some(turn) = grant.receive(sender, event, &text)
            {
                let stop = grant.stop.clone();
                let ticket = grant.ticket.clone();
                let (team, channel) = (team_id.clone(), channel_id.clone());
                let generation = self.generation;
                cx.spawn(async move |this, cx| {
                    let result = cx.background_spawn(async move { super::bot::review_peer(&turn.prompt, &stop) }).await;
                    let _ = this.update(cx, |hub, cx| {
                        if !hub.peer_review.as_ref().is_some_and(|g| g.ticket == ticket && g.live()) {
                            // The grant ran out or was replaced while Claude ran. The
                            // requester still waits for notes, so say there are none.
                            if matches!(turn.response, PeerEvent::Notes { .. })
                                && hub.generation == generation
                                && hub.review_epoch(&team, &channel) == Some(epoch)
                            {
                                let body = PeerEvent::Blocked.body(
                                    "This device's pairing ended before the review finished. Pair again.".into(),
                                    Some(id),
                                );
                                hub.send_body(&team, &channel, body, cx);
                            }
                            return;
                        }
                        // The grant is spent: a channel that can't take the answer now
                        // (a gap, a rotation, a lost socket) ends it, and says so.
                        if hub.review_epoch(&team, &channel) != Some(epoch) {
                            if let Some(caller) = hub.peer_review.as_mut().and_then(|g| g.caller.take()) {
                                let _ = caller.send(crate::control::Reply::err(
                                    "The channel changed before the review could be posted. Pair again.",
                                ));
                            }
                            hub.peer_review = None;
                            hub.error = Some("The paired review ended: the channel changed before its answer could be posted.".into());
                            cx.notify();
                            return;
                        }
                        let body = match result {
                            Ok(s) if coord::valid_text(s.trim()) => turn.response.body(s.trim().into(), Some(id)),
                            failed => {
                                // The channel gets a fixed text; this device learns the
                                // cause, or pairing again only repeats it.
                                let why = failed.err().unwrap_or_else(|| {
                                    "Claude Code's answer was empty or longer than 4000 bytes".into()
                                });
                                hub.error = Some(format!("The paired review could not finish: {why}"));
                                cx.notify();
                                PeerEvent::Blocked.body("Could not finish this review within its limits. A member can take over.".into(), Some(id))
                            }
                        };
                        let answer = if matches!(PeerEvent::read(&body), Some(PeerEvent::Done)) {
                            crate::control::Reply::ok(body.text.clone())
                        } else if matches!(turn.response, PeerEvent::Notes { .. }) {
                            // A caller here asked at the same time as the peer, and the
                            // peer's request ran (`Grant::receive`).
                            crate::control::Reply::err(
                                "The other member asked at the same time; their review ran instead.",
                            )
                        } else {
                            crate::control::Reply::err("No recommendation was available.")
                        };
                        let done = matches!(PeerEvent::read(&body), Some(PeerEvent::Done));
                        match hub.send_review_body(&team, &channel, body, cx) {
                            // The caller hears once the server stores the
                            // recommendation, so it never holds one the channel lacks.
                            Some(sent) if done => {
                                if let Some(g) = hub.peer_review.as_mut() {
                                    g.posted = Some(sent);
                                    g.answer = Some(answer);
                                }
                            }
                            Some(_) => {
                                if let Some(caller) = hub.peer_review.as_mut().and_then(|g| g.caller.take()) {
                                    let _ = caller.send(answer);
                                }
                                hub.peer_review = None;
                            }
                            None => hub.peer_review = None,
                        }
                    });
                }).detach();
            }
        }
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

    // ---- paired agent reviews --------------------------------------------------

    /// The key epoch a review in this channel runs under: only while the
    /// channel is connected, its history has no gap and no rotation is pending.
    fn review_epoch(&self, team: &str, channel: &str) -> Option<u64> {
        self.roster(team)?;
        let t = self.team.get(team)?;
        let c = self.channels.get(&channel_key(team, channel))?;
        (!t.detail.rotation_pending
            && self.kept.key(team, t.detail.epoch).is_some()
            && !c.loading
            && !c.needs_history
            && c.status == socket::Status::Connected)
            .then_some(t.detail.epoch)
    }

    /// Drops this device's grant. A ticket nobody used yet is taken back in
    /// the channel while the channel can still carry that, so the peer does
    /// not send a request to a ticket that is gone.
    pub(super) fn end_review(&mut self, cx: &mut Context<Self>) {
        let Some(grant) = self.peer_review.take() else {
            return;
        };
        if !grant.unspent() {
            return;
        }
        let Some((team, channel)) = self
            .channels
            .get(&grant.channel)
            .map(|c| (c.team_id.clone(), c.channel_id.clone()))
        else {
            return;
        };
        if self.review_epoch(&team, &channel) == Some(grant.epoch) {
            let body = grant
                .withdrawn()
                .body("No longer available for the paired review.".into(), None);
            self.send_body(&team, &channel, body, cx);
        }
    }

    /// Ends a grant whose window ran out, or whose request got no notes in
    /// time: the peer's grant can end where it has no way to say so.
    fn expire_review(&mut self) {
        let Some(grant) = self.peer_review.as_mut() else {
            return;
        };
        if grant.overdue() {
            if let Some(caller) = grant.caller.take() {
                let _ = caller.send(crate::control::Reply::err(
                    "The peer did not answer in time. Ask them to pair again.",
                ));
            }
            self.error = Some(
                "The paired review ended: the other member's agent did not answer in time.".into(),
            );
            self.peer_review = None;
        } else if !grant.live() {
            self.peer_review = None;
        }
    }

    /// `send_body` for a review event, with the id it was queued under.
    fn send_review_body(
        &mut self,
        team: &str,
        channel: &str,
        body: Body,
        cx: &mut Context<Self>,
    ) -> Option<String> {
        if !self.send_body(team, channel, body, cx) {
            return None;
        }
        // `send_body` adds the message last.
        self.channels
            .get(&channel_key(team, channel))
            .and_then(|c| c.messages.last())
            .map(|m| m.id.clone())
    }

    /// The server stored this device's last review event: a recommendation
    /// can go to the caller now.
    fn review_stored(&mut self, key: &str) {
        let Some(grant) = self.peer_review.as_mut() else {
            return;
        };
        let stored = grant.channel == key
            && grant.posted.as_ref().is_some_and(|id| {
                self.channels[key]
                    .messages
                    .iter()
                    .any(|m| &m.id == id && m.delivery == Delivery::Sent)
            });
        if !stored {
            return;
        }
        grant.posted = None;
        if let Some(answer) = grant.answer.take() {
            if let Some(caller) = grant.caller.take() {
                let _ = caller.send(answer);
            }
            self.peer_review = None;
        }
    }

    /// The server refused this device's last review event, so the peer never
    /// saw it: end the exchange now rather than wait out its time.
    fn review_refused(&mut self, id: &str, why: &str) {
        let Some(grant) = self.peer_review.as_mut() else {
            return;
        };
        if grant.posted.as_deref() != Some(id) {
            return;
        }
        if let Some(caller) = grant.caller.take() {
            let _ = caller.send(crate::control::Reply::err(format!(
                "The server refused the review message: {why}"
            )));
        }
        self.error = Some(format!(
            "The paired review ended: the server refused its message: {why}"
        ));
        self.peer_review = None;
    }

    pub(super) fn review_command(
        &mut self,
        team: &str,
        channel: &str,
        cmd: &str,
        cx: &mut Context<Self>,
    ) -> Result<(), String> {
        if cmd == "off" {
            self.end_review(cx);
            cx.notify();
            return Ok(());
        }
        let key = channel_key(team, channel);
        if !self.teams_entity.read(cx).enabled() || self.bot.project.is_none() {
            return Err("Open a local project with Teams enabled to pair an agent.".into());
        }
        let epoch = self
            .review_epoch(team, channel)
            .ok_or("Wait for a connected channel with its current key.")?;
        let me = self.github_id();
        let (event, text) = if let Some(login) = cmd.strip_prefix("pair @") {
            let peer = self.team[team]
                .detail
                .members
                .iter()
                .find(|m| m.login.eq_ignore_ascii_case(login) && m.github_id != me)
                .ok_or("Choose another current member by GitHub login.")?
                .github_id;
            // A ticket for someone else, or in another channel, goes away here.
            self.end_review(cx);
            let grant = Grant::new(key, epoch, peer);
            let ready = grant.ready();
            self.peer_review = Some(grant);
            (
                ready,
                format!("Available for one Claude review with @{login} for 15 minutes."),
            )
        } else if let Some(goal) = cmd.strip_prefix("review ") {
            let g = self
                .peer_review
                .as_mut()
                .filter(|g| g.channel == key && g.epoch == epoch && g.live())
                .ok_or("Pair this device with a member first.")?;
            // A ready from a peer who has since left the channel names a
            // grant that is likely gone; ask them to pair again instead.
            if !self.channels[&key].here.contains(&g.peer) {
                return Err("The other member is not in this channel now. Ask them to open it and pair again.".into());
            }
            let other = self.channels[&key]
                .messages
                .iter()
                .rev()
                .find_map(|m| {
                    if m.github_id == g.peer
                        && m.delivery == Delivery::Sent
                        && now().saturating_sub(m.at) < coord::WINDOW.as_secs()
                        && let Some(PeerEvent::Ready { peer, ticket }) = PeerEvent::read(&m.body)
                        && peer == me
                    {
                        Some(ticket)
                    } else {
                        None
                    }
                })
                .ok_or("The other member must pair a device with you in this channel first.")?;
            if self.channels[&key].messages.iter().any(|m| {
                m.github_id == g.peer
                    && m.delivery == Delivery::Sent
                    && matches!(PeerEvent::read(&m.body),
                        Some(PeerEvent::Withdrawn { ticket }) if ticket == other)
            }) {
                return Err(
                    "The other member is no longer available. Ask them to pair again.".into(),
                );
            }
            let event = g
                .request(other, goal)
                .ok_or("This ticket is in use, or the goal is empty or longer than 4000 bytes.")?;
            (event, goal.into())
        } else {
            return Err("Use /agents pair @login, /agents review <goal>, or /agents off.".into());
        };
        let Some(sent) = self.send_review_body(team, channel, event.body(text, None), cx) else {
            self.peer_review = None;
            return Err("Could not send the review event; pair again.".into());
        };
        if let Some(g) = self.peer_review.as_mut() {
            g.posted = Some(sent);
        }
        Ok(())
    }

    /// The channel picked in the tab, if any: only its socket counts toward
    /// presence. The one picked before goes quiet without hanging up.
    pub fn set_present(&mut self, key: Option<String>, cx: &mut Context<Self>) {
        if self.present == key {
            return;
        }
        move_presence(&mut self.channels, self.present.as_deref(), key.as_deref());
        self.present = key;
        cx.notify();
    }

    // ---- unread ---------------------------------------------------------------

    /// The channel the tab shows now, if any: its messages count as read.
    pub fn set_viewing(&mut self, key: Option<String>, cx: &mut Context<Self>) {
        if self.viewing == key {
            return;
        }
        self.viewing = key.clone();
        if let Some(key) = key {
            // The newest for the cap on open sockets too.
            if let Some(c) = self.channels.get_mut(&key) {
                c.shown = Instant::now();
            }
            self.unread_read.remove(&key);
            self.mark_read(&key, cx);
        }
        cx.notify();
    }

    fn mark_read(&mut self, key: &str, cx: &mut Context<Self>) {
        let Some(c) = self.channels.get(key) else {
            return;
        };
        if self.kept.mark_read(key, c.history.shown()) {
            // Counted from the old mark, so the next poll counts again.
            self.unread_newest.remove(key);
            self.save(cx);
        }
    }

    /// Messages from others after the last one this device showed. Channels
    /// with a socket count what they hold; the rest what history said. One
    /// whose socket was closed holds a part, so the larger of the two counts.
    pub fn unread(&self, team_id: &str, channel_id: &str) -> usize {
        let key = channel_key(team_id, channel_id);
        if self.viewing.as_deref() == Some(key.as_str()) {
            return 0;
        }
        let me = self.github_id();
        match self.channels.get(&key) {
            // A channel this device never read waits for the poll to set
            // where it starts, rather than counting its whole history.
            Some(c) if !c.loading && self.kept.last_read(&key).is_some() => {
                let after = self.kept.last_read(&key).unwrap_or(0);
                let held = c
                    .messages
                    .iter()
                    .filter(|m| m.seq.is_some_and(|s| s > after))
                    .filter(|m| m.github_id != me && counts_as_unread(&m.body.kind))
                    .count();
                if c.link.is_some() {
                    held
                } else {
                    held.max(self.unread_read.get(&key).copied().unwrap_or(0))
                }
            }
            _ => self.unread_read.get(&key).copied().unwrap_or(0),
        }
    }

    /// Reads the unread counts of channels with no socket here: on focus
    /// (`soon`), else at most every [`UNREAD_AGAIN`]. Each asks for its newest
    /// `seq` first and reads its history only when that moved. A channel this
    /// device never read starts from its newest message instead.
    pub fn refresh_unread(&mut self, soon: bool, cx: &mut Context<Self>) {
        let wait = if soon { UNREAD_FOCUS } else { UNREAD_AGAIN };
        if self.unread_at.is_some_and(|at| at.elapsed() < wait) || self.team.is_empty() {
            return;
        }
        self.unread_at = Some(Instant::now());
        let me = self.github_id();
        let mut asks = Vec::new();
        let mut skipped = false;
        for (team_id, t) in &self.team {
            let Some(scope) = self.roster(team_id) else {
                skipped = true;
                continue;
            };
            let mut channels = Vec::new();
            for ch in &t.channels {
                let key = channel_key(team_id, &ch.id);
                let after = self.kept.last_read(&key);
                // A channel whose socket was closed hears nothing new either.
                // One with a socket that was never read still needs its start.
                if after.is_some() && self.channels.get(&key).is_some_and(|c| c.link.is_some()) {
                    continue;
                }
                let known = self.unread_newest.get(&key).copied();
                channels.push((key, ch.id.clone(), after, known));
            }
            if channels.is_empty() {
                continue;
            }
            // Only what another member's device signed counts, as on a socket.
            let senders: HashMap<String, Vec<u8>> = t
                .devices
                .iter()
                .filter(|(gid, _)| **gid != me)
                .flat_map(|(_, ds)| ds)
                .filter_map(|d| Some((d.device_id.clone(), crypto::unb64(&d.sig_pub)?)))
                .collect();
            let keys: HashMap<u64, TeamKey> = self.kept.keys(team_id).into_iter().collect();
            asks.push((team_id.clone(), scope, senders, keys, channels));
        }
        if skipped {
            self.unread_soon();
        }
        if asks.is_empty() {
            return;
        }
        self.run(
            cx,
            move |net| {
                let mut out = Vec::new();
                for (team_id, scope, senders, keys, channels) in asks {
                    let counts = |channel_id: &str, s: &Stored| {
                        let Some(sig_pub) = senders.get(&s.message.device_id) else {
                            return false;
                        };
                        // A message this device has no key for does not
                        // show when the channel opens, so it is not unread.
                        crypto::verify_message(&team_id, channel_id, &s.message, sig_pub)
                            && keys
                                .get(&s.message.epoch)
                                .and_then(|k| {
                                    crypto::open_message(&team_id, channel_id, &s.message, k).ok()
                                })
                                .is_some_and(|b| counts_as_unread(&b.kind))
                    };
                    for (key, channel_id, after, known) in channels {
                        // One channel failing (an old server, a removed
                        // channel) leaves its count as it was.
                        let Ok(newest) = net.call().newest(&team_id, &channel_id) else {
                            continue;
                        };
                        let polled = match unread_poll(after, newest, known) {
                            Next::Done(p) => p,
                            Next::Read(after) => {
                                let Ok(n) = unread_since(
                                    after,
                                    |seq| net.call().history(&team_id, &channel_id, seq),
                                    |s| counts(&channel_id, s),
                                ) else {
                                    continue;
                                };
                                Polled::Count(n, newest)
                            }
                            Next::Keep => continue,
                        };
                        out.push((key, team_id.clone(), scope, polled));
                    }
                }
                Ok(out)
            },
            |hub, result, cx| {
                let Ok(polls) = result else {
                    return;
                };
                let mut moved = false;
                for (key, team, scope, polled) in polls {
                    // A poll for a membership that has since changed is not
                    // this one's, and the old count is the old membership's
                    // too: drop it and read again for the one there is now.
                    if hub.roster(&team) != Some(scope) {
                        hub.unread_read.remove(&key);
                        hub.unread_newest.remove(&key);
                        hub.unread_soon();
                        continue;
                    }
                    match polled {
                        Polled::Start(newest) => {
                            moved |= hub.kept.mark_read(&key, newest);
                            hub.unread_read.insert(key.clone(), 0);
                            hub.unread_newest.insert(key, newest);
                        }
                        // A channel that opened meanwhile counts what it holds.
                        Polled::Count(_, _)
                            if hub.channels.get(&key).is_some_and(|c| c.link.is_some()) => {}
                        Polled::Count(n, newest) => {
                            hub.unread_read.insert(key.clone(), n);
                            hub.unread_newest.insert(key, newest);
                        }
                    }
                }
                if moved {
                    hub.save(cx);
                }
                cx.notify();
            },
        );
    }

    /// Lets the next [`Hub::refresh_unread`] read after [`UNREAD_FOCUS`]
    /// rather than [`UNREAD_AGAIN`], for a team whose roster was not settled.
    fn unread_soon(&mut self) {
        self.unread_at = Instant::now().checked_sub(UNREAD_AGAIN - UNREAD_FOCUS);
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

/// What to say once an invite went out: `Ok` when it shows in their Team tab,
/// `Err` when it waits there unseen, so the admin is not told it worked.
/// `None` when the server did not say, as an older one does not.
fn invite_sent(login: &str, state: Option<InviteeState>) -> Option<Result<String, String>> {
    match state? {
        InviteeState::Ready => Some(Ok(format!("Invited @{login}. It shows in their Team tab."))),
        InviteeState::NoAccount => Some(Err(format!(
            "Invited @{login}, but they have not signed in to Arakun yet, so they can't see it."
        ))),
        InviteeState::NotEnabled => Some(Err(format!(
            "Invited @{login}, but Teams is not on for their account yet, so they can't see it. \
             Ask an Arakun admin to turn it on."
        ))),
        InviteeState::Unknown => None,
    }
}

/// The invite list a team read leaves: the server's, unless the read started
/// before an invite or a withdraw from here (`changed`) or the server lists
/// none, when the list `here` stays.
fn invites_after_read(
    read: Option<Vec<TeamInvite>>,
    here: Option<Vec<TeamInvite>>,
    changed: bool,
) -> Option<Vec<TeamInvite>> {
    match read {
        Some(read) if !changed => Some(read),
        read => here.or(read),
    }
}

/// What the unread poll learned of one channel.
#[derive(Debug, PartialEq, Eq)]
enum Polled {
    /// Never read on this device: it starts at its newest message.
    Start(u64),
    /// How many unread, and the newest `seq` they were counted up to.
    Count(usize, u64),
}

/// What one channel's poll does next.
#[derive(Debug, PartialEq, Eq)]
enum Next {
    /// Known without reading history.
    Done(Polled),
    /// Read the history after this read mark and count it.
    Read(u64),
    /// Nothing came since the last count: it stands.
    Keep,
}

/// What one channel's poll does once it knows the channel's `newest` seq,
/// `after` being this device's read mark and `known` the newest seen at the
/// last count.
fn unread_poll(after: Option<u64>, newest: u64, known: Option<u64>) -> Next {
    match after {
        None => Next::Done(Polled::Start(newest)),
        Some(after) if newest <= after => Next::Done(Polled::Count(0, newest)),
        Some(_) if known == Some(newest) => Next::Keep,
        Some(after) => Next::Read(after),
    }
}

/// Kinds that show as a message of their own, and so count as unread.
/// Acceptances show under their answer.
fn counts_as_unread(kind: &str) -> bool {
    matches!(kind, "text" | "bot" | "decision" | "agent_review")
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

/// Why a message the server refused for a retired key was not sent.
const OLD_KEY: &str =
    "the team is getting a new key because someone left. It sends by itself once the key is here";

/// How the client takes a refusal from the channel socket.
#[derive(Debug, PartialEq)]
enum Refusal {
    /// The channel holds the message already.
    Stored,
    /// Sealed with a key a removal retired: read the team, seal it again.
    OldKey,
    /// Another epoch problem (one newer than the team's): read the team.
    Epoch,
    Other,
}

/// What a socket `error` frame means. Servers that send `code` are matched
/// on it; older ones only on their wording, which is all they had.
fn refusal(frame: &serde_json::Value) -> Refusal {
    match frame["code"].as_str() {
        Some("exists") => return Refusal::Stored,
        Some("rotation_pending" | "stale_epoch") => return Refusal::OldKey,
        Some("unknown_epoch") => return Refusal::Epoch,
        // Nothing to do but show it; its wording must not decide.
        Some("retry") => return Refusal::Other,
        _ => {}
    }
    let why = frame["error"].as_str().unwrap_or_default();
    if why.contains("exists") {
        Refusal::Stored
    } else if why.starts_with("stale epoch") || why.contains("has no envelopes yet") {
        Refusal::OldKey
    } else if why.contains("epoch") {
        Refusal::Epoch
    } else {
        Refusal::Other
    }
}

/// The epoch a queued `send` frame was sealed with, 0 if it has none.
fn frame_epoch(frame: &str) -> u64 {
    serde_json::from_str::<serde_json::Value>(frame)
        .ok()
        .and_then(|v| v["message"]["epoch"].as_u64())
        .unwrap_or(0)
}

/// Takes the refused messages that a key of `epoch` can send again: those
/// sealed with an older one. The rest wait for a newer key, so a refusal the
/// team read does not explain yet never sends in a loop.
fn take_rekeyed(rekey: &mut Vec<(String, u64)>, epoch: u64) -> Vec<String> {
    let (due, wait): (Vec<_>, Vec<_>) = rekey.drain(..).partition(|(_, e)| *e < epoch);
    *rekey = wait;
    due.into_iter().map(|(id, _)| id).collect()
}

/// A pending rotation is its own wait: a removed member holds the newest key
/// with envelopes, so nothing new is sealed with it, and the server refuses
/// it too.
fn no_key_reason(detail: &TeamDetail) -> &'static str {
    if detail.rotation_pending && detail.keyed_epoch.is_some_and(|k| k > 0) {
        "Someone left this team, so it needs a new key before anyone can send. An admin's Arakun makes it the next time it opens the team."
    } else {
        NO_KEY
    }
}

/// Whether someone with role `mine` may give `member` of `members` the role
/// `to`: an owner may change anyone's, their own too, as long as the team
/// keeps an owner. The Worker's rule for `PUT .../members/<id>`.
pub(super) fn may_give(mine: Role, members: &[MemberRow], member: &MemberRow, to: Role) -> bool {
    let owners = members.iter().filter(|m| m.role == Role::Owner).count();
    mine == Role::Owner && member.role != to && (member.role != Role::Owner || owners > 1)
}

/// What a team read says about the keys this device refused: `bad` did not
/// check out, `from_non_admins` came from someone no longer an owner or admin.
fn key_problem(bad: usize, from_non_admins: usize) -> Option<String> {
    let mut said = Vec::new();
    if bad > 0 {
        said.push(format!(
            "{} team key{} for this device did not check out and {} ignored.",
            bad,
            if bad == 1 { "" } else { "s" },
            if bad == 1 { "was" } else { "were" },
        ));
    }
    if from_non_admins > 0 {
        said.push(format!(
            "{} team key{} for this device came from someone who is no longer an owner or admin. An admin's Arakun sends {} again the next time it opens the team.",
            from_non_admins,
            if from_non_admins == 1 { "" } else { "s" },
            if from_non_admins == 1 { "it" } else { "them" },
        ));
    }
    (!said.is_empty()).then(|| said.join(" "))
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

/// What [`find_former`] found: the people, and the devices it looked for
/// and got an answer about.
type FoundFormer = (Vec<(u64, Former)>, HashSet<String>);

/// Looks up the senders in `unknown` that no member's trusted device
/// matches: people who have left the team. Each answer is checked like a
/// member's devices, and kept only if the device it is for checks out.
/// Returns them, and the devices that got an answer: a member's device, or
/// one the server answered about, `404` included (an older server has no
/// such lookup and answers that). A device whose lookup failed otherwise is
/// left out, so its messages wait for the next read.
fn find_former(
    members: &HashMap<u64, Vec<DeviceRow>>,
    unknown: &[(String, String)],
    mut lookup: impl FnMut(&str, &str) -> Result<Option<SenderRow>, ApiError>,
) -> Result<FoundFormer, ApiError> {
    let mut former: Vec<(u64, Former)> = Vec::new();
    let mut looked_up = HashSet::new();
    for (channel, device) in unknown {
        let placed = members.values().flatten().any(|d| &d.device_id == device)
            || former
                .iter()
                .any(|(_, f)| f.devices.iter().any(|d| &d.device_id == device));
        // The ID came from the server inside a message and goes into a path.
        if placed || !crypto::is_device_id(device) {
            looked_up.insert(device.clone());
            continue;
        }
        match lookup(channel, device) {
            Ok(row) => {
                looked_up.insert(device.clone());
                if let Some(f) = row.and_then(|row| former_sender(device, row)) {
                    former.push(f);
                }
            }
            Err(ApiError::Unauthorized) => return Err(ApiError::Unauthorized),
            Err(_) => {}
        }
    }
    Ok((former, looked_up))
}

/// The person a sender lookup names, with their trusted devices, if
/// `device` is one of them.
fn former_sender(device: &str, row: SenderRow) -> Option<(u64, Former)> {
    let trusted = trusted_devices(row.github_id, &row.devices);
    if !trusted.iter().any(|d| d.device_id == device) {
        return None;
    }
    let former = Former {
        login: row.login,
        devices: trusted,
    };
    Some((row.github_id, former))
}

/// Adds the former members a read found to `former`, each pinned like a
/// member. One whose first device differs from the pinned one is left out,
/// so their messages stay hidden; nothing else changes, since no key goes
/// to them. Answers whether a new pin needs saving.
fn adopt_former(
    kept: &mut Kept,
    former: &mut HashMap<u64, Former>,
    found: Vec<(u64, Former)>,
) -> bool {
    let mut changed = false;
    for (gid, f) in found {
        let known = kept.roots.contains_key(&gid);
        if pin_directory(kept, gid, &f.devices) {
            changed |= !known;
            former.insert(gid, f);
        }
    }
    changed
}

fn unopened(seqs: impl Iterator<Item = u64>, first: u64, last: u64) -> usize {
    seqs.filter(|seq| (first..=last).contains(seq)).count()
}

/// How many history pages an unread count reads at most. The label stops
/// at "99+", so more only matters when a page holds little that counts.
const UNREAD_PAGES: usize = 5;

/// Counts the stored messages after `after` that `counts` says are unread.
/// The first page can be full of what does not count (this user's own
/// messages, edits, acceptances), so it reads on until the count passes what
/// the label shows, history ends or [`UNREAD_PAGES`] were read.
fn unread_since(
    mut after: u64,
    mut read: impl FnMut(u64) -> Result<Vec<Stored>, ApiError>,
    counts: impl Fn(&Stored) -> bool,
) -> Result<usize, ApiError> {
    let mut n = 0;
    for _ in 0..UNREAD_PAGES {
        let page = read(after)?;
        n += page.iter().filter(|s| counts(s)).count();
        let last = page.last().map(|s| s.seq);
        match last {
            // A page that does not advance would be read again forever.
            Some(seq) if page.len() >= 200 && n < 100 && seq > after => after = seq,
            _ => break,
        }
    }
    Ok(n)
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

/// The keys this admin's device owes members' devices, at most one upload's
/// worth, each in the epochs granted to that membership:
/// - to a device the member added after joining, once per device and epoch:
///   the approval covered the devices they had;
/// - to any device, one older than the membership included, whose key for an
///   epoch the server lists as stale: its sender is no longer a device of an
///   owner or admin (demoted, or gone), so the device refuses it. A record
///   that this device delivered that key does not count: the server keeps the
///   first admin's copy, which may be the one gone stale.
fn next_backfill(kept: &Kept, t: &TeamState, mine: &str) -> Vec<KeyDelivery> {
    let team = &t.detail.team_id;
    let keys = kept.keys(team);
    let doubted = doubted(kept, t);
    let stale: HashSet<(&str, u64)> = t
        .stale
        .iter()
        .map(|s| (s.device_id.as_str(), s.epoch))
        .collect();
    let mut batch = Vec::new();
    for (m, d) in recipients(t) {
        if d.device_id == mine || doubted.contains(&m.login) {
            continue;
        }
        let added_later = d.created_at >= m.joined_at;
        for &(epoch, key) in &keys {
            let owed = stale.contains(&(d.device_id.as_str(), epoch))
                || (added_later
                    && kept.needs_delivery(team, m.github_id, m.joined_at, &d.device_id, epoch));
            if epoch <= t.detail.epoch
                && m.first_key_epoch.is_some_and(|first| epoch >= first)
                && owed
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
    /// Senders who are not members now, found by their devices.
    former: Vec<(u64, Former)>,
    /// The unknown sender devices this read looked for and got an answer
    /// about, found or not.
    looked_up: HashSet<String>,
    /// Keys from envelopes this device did not hold yet, checked.
    keys: Vec<(u64, TeamKey)>,
    bad_envelopes: usize,
    /// Keys whose sender is no longer an owner's or admin's device.
    from_non_admins: usize,
    pending: Vec<PendingRow>,
    invites: Option<Vec<TeamInvite>>,
    epoch_made: HashMap<u64, u64>,
    approved_by: Option<String>,
    stale: HashSet<StaleEnvelope>,
}

/// Where a team read gets its members' devices.
#[derive(Clone)]
enum Directory {
    /// Read by the refresh in one request, for all the teams it reads.
    Read(Arc<HashMap<u64, Vec<DeviceRow>>>),
    /// Not read yet: one request for the team's members.
    Ask,
    /// A server from before `GET /devices`: one request per member.
    PerUser,
}

/// The devices of each user in `ids`. What `directory` lacks (a member who
/// joined after it was read) comes in one request, or one per user from a
/// server without the batched call.
fn member_devices(
    ids: &[u64],
    directory: &Directory,
    batch: impl FnOnce(&[u64]) -> Result<HashMap<u64, Vec<DeviceRow>>, ApiError>,
    mut one: impl FnMut(u64) -> Result<Vec<DeviceRow>, ApiError>,
) -> Result<HashMap<u64, Vec<DeviceRow>>, ApiError> {
    let mut found = HashMap::new();
    if let Directory::Read(people) = directory {
        for id in ids {
            if let Some(rows) = people.get(id) {
                found.insert(*id, rows.clone());
            }
        }
    }
    let missing: Vec<u64> = ids
        .iter()
        .copied()
        .filter(|id| !found.contains_key(id))
        .collect();
    if !missing.is_empty() && !matches!(directory, Directory::PerUser) {
        match batch(&missing) {
            Ok(people) => found.extend(people),
            Err(ApiError::Status(404, _)) => {}
            Err(e) => return Err(e),
        }
    }
    // Left out of a batch (out of reach by now) or never batched: the
    // per-user call answers, or says why not.
    for id in ids {
        if !found.contains_key(id) {
            found.insert(*id, one(*id)?);
        }
    }
    Ok(found)
}

/// `unknown`: devices of messages waiting for their sender, each with a
/// channel that holds one of its messages.
fn read_team(
    net: &Net,
    team_id: &str,
    admin: bool,
    held: &HashSet<u64>,
    directory: &Directory,
    unknown: &[(String, String)],
) -> Result<TeamRead, ApiError> {
    let call = net.call();
    let detail = call.team(team_id)?;
    let ids: Vec<u64> = detail.members.iter().map(|m| m.github_id).collect();
    let mut rows_of = member_devices(
        &ids,
        directory,
        |missing| call.people_devices(Some(missing)),
        |id| call.devices(id),
    )?;
    let mut devices = HashMap::new();
    let mut untrusted = Vec::new();
    for m in &detail.members {
        let rows = rows_of.remove(&m.github_id).unwrap_or_default();
        let trusted = trusted_devices(m.github_id, &rows);
        if trusted.len() != rows.len() {
            untrusted.push(m.login.clone());
        }
        devices.insert(m.github_id, trusted);
    }
    let (former, looked_up) = find_former(&devices, unknown, |channel, device| {
        call.sender(team_id, channel, device)
    })?;
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
    let mut from_non_admins = 0;
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
        // Refused, not trusted on the server's word that it was an admin's
        // when it was made. The server lists it as stale to the admins, and
        // one of their devices seals this epoch again.
        let Some(sender) = admins.get(e.from.as_str()) else {
            from_non_admins += 1;
            continue;
        };
        let opened = crypto::unb64(&sender.sig_pub)
            .ok_or(crypto::CryptoError::BadSignature)
            .and_then(|sig_pub| crypto::open_envelope(&e, team_id, &net.local.device, &sig_pub));
        match opened {
            Ok(key) => keys.push((e.epoch, key)),
            Err(_) => bad += 1,
        }
    }
    let (pending, invites, stale) = if admin {
        // An older server has no list of open invites, nor of stale
        // envelopes; without the second, keys only go to new devices.
        (
            call.pending(team_id)?,
            call.team_invites(team_id).ok(),
            call.stale_envelopes(team_id)
                .map(HashSet::from_iter)
                .unwrap_or_default(),
        )
    } else {
        (Vec::new(), None, HashSet::new())
    };
    Ok(TeamRead {
        detail,
        devices,
        untrusted,
        former,
        looked_up,
        keys,
        bad_envelopes: bad,
        from_non_admins,
        pending,
        invites,
        epoch_made,
        approved_by,
        stale,
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

    /// A message `from` sent in channel C of team T, sealed with `key`.
    fn stored(seq: u64, from: &Device, epoch: u64, key: &TeamKey, text: &str) -> Stored {
        Stored {
            seq,
            at: seq,
            message: crypto::seal_message(
                "T",
                "C",
                &crypto::ulid(),
                epoch,
                key,
                from,
                &Body::text(text, None),
            ),
        }
    }

    #[test]
    fn a_batch_opens_off_the_hub_as_it_would_on_it() {
        let (a, stranger) = (Device::generate(), Device::generate());
        let t = delivery_team(vec![row(&a, 1, None)], 1);
        let mut kept = Kept::default();
        kept.put_key("T", 1, &[1; 32]);
        let opener = Opener::new("T", "C", &t, &kept);
        let mut forged = stored(4, &a, 1, &[1; 32], "forged");
        forged.message.ct.push('A');
        let batch = vec![
            stored(1, &a, 1, &[1; 32], "hello"),
            stored(2, &stranger, 1, &[1; 32], "who"),
            stored(3, &a, 2, &[2; 32], "later key"),
            forged,
        ];
        let opened: Vec<Opened> = open_batch(Some(&opener), batch.clone())
            .into_iter()
            .map(|(_, o)| o)
            .collect();
        assert_eq!(
            opened,
            vec![
                Opened::Open {
                    github_id: 21,
                    body: Box::new(Body::text("hello", None))
                },
                Opened::Unknown,
                Opened::Locked,
                Opened::Hidden,
            ]
        );
        // Before the team is read, nothing is known.
        assert!(
            open_batch(None, batch)
                .iter()
                .all(|(_, o)| *o == Opened::Unknown)
        );
    }

    #[test]
    fn what_changed_while_a_batch_was_opened_is_opened_again() {
        let (a, b) = (Device::generate(), Device::generate());
        let t = delivery_team(vec![row(&a, 1, None)], 1);
        let mut kept = Kept::default();
        kept.put_key("T", 1, &[1; 32]);
        let before = Opener::new("T", "C", &t, &kept);
        let batch = open_batch(
            Some(&before),
            vec![
                stored(1, &a, 1, &[1; 32], "kept"),
                stored(2, &b, 1, &[1; 32], "new device"),
                stored(3, &a, 2, &[2; 32], "new key"),
            ],
        );
        // Meanwhile B's device was read and the key of epoch 2 arrived.
        let t = delivery_team(vec![row(&a, 1, None), row(&b, 2, None)], 2);
        kept.put_key("T", 2, &[2; 32]);
        let now = Opener::new("T", "C", &t, &kept);
        let texts: Vec<String> = reopen(Some(&before), &now, batch)
            .into_iter()
            .map(|(_, o)| match o {
                Opened::Open { body, .. } => body.text,
                other => panic!("not opened: {other:?}"),
            })
            .collect();
        assert_eq!(texts, ["kept", "new device", "new key"]);
        // One that agrees keeps what it came to, unopened again.
        let s = stored(4, &a, 1, &[1; 32], "x");
        let kept_as = vec![(s, Opened::Hidden)];
        assert_eq!(reopen(Some(&now), &now, kept_as)[0].1, Opened::Hidden);
    }

    #[test]
    fn finds_a_former_member_by_the_device_they_wrote_with() {
        let [member, root, laptop, forged, stranger, flaky] =
            std::array::from_fn(|_| Device::generate());
        let members = HashMap::from([(22, vec![row(&member, 1, None)])]);
        let theirs = vec![row(&root, 1, None), row(&laptop, 2, Some(&root))];
        // Claims the first device endorsed it, but signed that itself.
        let mut bad = row(&forged, 3, Some(&root));
        bad.endorsement_sig = Some(forged.endorse(21, forged.sig_pub(), forged.enc_pub()));
        let answer = |devices: Vec<DeviceRow>| SenderRow {
            github_id: 21,
            login: "gone".into(),
            devices,
        };
        let unknown: Vec<(String, String)> = [
            ("C1", member.id()),
            ("C1", laptop.id()),
            // The same person, placed by the answer above.
            ("C2", root.id()),
            ("C1", forged.id()),
            ("C1", stranger.id()),
            ("C1", "not/a/device-id-at-all"),
            ("C1", flaky.id()),
        ]
        .map(|(c, d)| (c.to_string(), d.to_string()))
        .into();
        let mut asked = Vec::new();
        let (found, looked_up) = find_former(&members, &unknown, |channel, device| {
            asked.push((channel.to_string(), device.to_string()));
            if device == laptop.id() {
                Ok(Some(answer(theirs.clone())))
            } else if device == forged.id() {
                Ok(Some(answer([theirs.clone(), vec![bad.clone()]].concat())))
            } else if device == flaky.id() {
                Err(ApiError::Network("offline".into()))
            } else {
                // A device the channel has no message from, or an older
                // server without the lookup.
                Ok(None)
            }
        })
        .unwrap();
        assert_eq!(found.len(), 1);
        let (gid, f) = &found[0];
        assert_eq!((*gid, f.login.as_str()), (21, "gone"));
        assert_eq!(f.devices, theirs);
        let ids = |ds: &[&Device]| ds.iter().map(|d| d.id().to_string()).collect::<Vec<_>>();
        assert_eq!(
            asked.iter().map(|(_, d)| d.clone()).collect::<Vec<_>>(),
            ids(&[&laptop, &forged, &stranger, &flaky])
        );
        assert!(asked.iter().all(|(c, _)| c == "C1"));
        // Every device but the one whose lookup failed is given up on if
        // still unplaced; that one waits for the next read.
        let mut expect: HashSet<String> = ids(&[&member, &laptop, &root, &forged, &stranger])
            .into_iter()
            .collect();
        expect.insert("not/a/device-id-at-all".into());
        assert_eq!(looked_up, expect);

        // A session that ended stops the read, as any call does.
        let ended = find_former(&HashMap::new(), &unknown[1..2], |_, _| {
            Err(ApiError::Unauthorized)
        });
        assert!(matches!(ended, Err(ApiError::Unauthorized)));
    }

    #[test]
    fn a_former_member_is_pinned_like_a_member() {
        let [root, other] = std::array::from_fn(|_| Device::generate());
        let former_of = |d: &Device| Former {
            login: "gone".into(),
            devices: vec![row(d, 1, None)],
        };
        let mut kept = Kept::default();
        let mut former = HashMap::new();
        assert!(adopt_former(
            &mut kept,
            &mut former,
            vec![(21, former_of(&root))]
        ));
        assert!(former.contains_key(&21));
        // Seen again: nothing new to save.
        assert!(!adopt_former(
            &mut kept,
            &mut former,
            vec![(21, former_of(&root))]
        ));
        // A different first device for a pinned account is not taken.
        let mut fresh = HashMap::new();
        assert!(!adopt_former(
            &mut kept,
            &mut fresh,
            vec![(21, former_of(&other))]
        ));
        assert!(fresh.is_empty());
    }

    #[test]
    fn a_former_members_messages_have_a_sender_and_a_name() {
        let [member, gone] = std::array::from_fn(|_| Device::generate());
        let mut t = delivery_team(vec![row(&member, 1, None)], 1);
        let mut kept = Kept::default();
        kept.put_key("T", 1, &[1; 32]);
        let sender = |t: &TeamState, from: &Device| match Opener::new("T", "C", t, &kept)
            .open(&stored(1, from, 1, &[1; 32], "hi"))
        {
            Opened::Open { github_id, .. } => Some(github_id),
            _ => None,
        };
        assert_eq!(sender(&t, &gone), None);
        assert_eq!(t.login(30), "someone");
        t.former.insert(
            30,
            Former {
                login: "gone".into(),
                devices: vec![row(&gone, 1, None)],
            },
        );
        assert_eq!(sender(&t, &gone), Some(30));
        assert_eq!(sender(&t, &member), Some(21));
        assert_eq!(t.login(30), "gone");
        assert!(t.is_former(30));
        assert!(!t.is_former(21));
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
            edited: None,
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
    fn a_refusal_is_read_by_its_code_and_by_the_wording_of_older_servers() {
        let frame = |code: Option<&str>, error: &str| {
            let mut v = serde_json::json!({ "type": "error", "id": "m", "error": error });
            if let Some(code) = code {
                v["code"] = code.into();
            }
            v
        };
        let pending = "a key rotation is pending: epoch 1 is retired, so wait for the new team key and send again";
        assert_eq!(
            refusal(&frame(Some("rotation_pending"), pending)),
            Refusal::OldKey
        );
        assert_eq!(
            refusal(&frame(
                Some("stale_epoch"),
                "stale epoch: fetch the new team key"
            )),
            Refusal::OldKey
        );
        assert_eq!(
            refusal(&frame(Some("exists"), "a message with this id exists")),
            Refusal::Stored
        );
        assert_eq!(
            refusal(&frame(Some("unknown_epoch"), "unknown epoch")),
            Refusal::Epoch
        );
        // A known code decides, whatever the text says.
        assert_eq!(
            refusal(&frame(Some("unknown_epoch"), "too far ahead")),
            Refusal::Epoch
        );
        assert_eq!(
            refusal(&frame(
                Some("retry"),
                "the epoch check failed: send it again"
            )),
            Refusal::Other
        );
        assert_eq!(
            refusal(&frame(Some("exists"), "already stored")),
            Refusal::Stored
        );
        // Servers from before `code`.
        assert_eq!(
            refusal(&frame(None, "stale epoch: fetch the new team key")),
            Refusal::OldKey
        );
        assert_eq!(
            refusal(&frame(
                None,
                "epoch 2 has no envelopes yet: keep using epoch 1 until the rotation ends"
            )),
            Refusal::OldKey
        );
        assert_eq!(
            refusal(&frame(None, "a message with this id exists")),
            Refusal::Stored
        );
        assert_eq!(refusal(&frame(None, "unknown epoch")), Refusal::Epoch);
        assert_eq!(
            refusal(&frame(None, "too many messages, slow down")),
            Refusal::Other
        );
        // A code this client does not know falls back to the wording.
        assert_eq!(
            refusal(&frame(Some("later"), "a message with this id exists")),
            Refusal::Stored
        );
        assert_eq!(
            refusal(&frame(Some("later"), "bad signature")),
            Refusal::Other
        );
    }

    #[test]
    fn a_message_refused_for_an_old_key_goes_again_only_with_a_newer_one() {
        let frame = serde_json::json!({ "type": "send", "message": { "id": "a", "epoch": 3 } });
        assert_eq!(frame_epoch(&frame.to_string()), 3);
        assert_eq!(frame_epoch("not json"), 0);
        let mut rekey = vec![("a".to_string(), 3), ("b".to_string(), 4)];
        // The team read still holds the key the server refused: nothing goes.
        assert!(take_rekeyed(&mut rekey, 3).is_empty());
        assert_eq!(rekey.len(), 2);
        assert_eq!(take_rekeyed(&mut rekey, 4), vec!["a".to_string()]);
        assert_eq!(rekey, vec![("b".to_string(), 4)]);
        assert_eq!(take_rekeyed(&mut rekey, 6), vec!["b".to_string()]);
        assert!(rekey.is_empty());
    }

    #[test]
    fn reads_each_members_devices_once_and_asks_only_for_the_rest() {
        let a = Device::generate();
        let rows = |n: u64| vec![row(&a, n, None)];
        let read = Directory::Read(Arc::new(HashMap::from([(1, rows(1)), (2, rows(2))])));
        let asked = std::cell::RefCell::new(Vec::new());
        let one = |id: u64| {
            asked.borrow_mut().push(id);
            Ok(rows(id))
        };

        // The refresh's read covers everyone: no request at all.
        let got = member_devices(&[1, 2], &read, |_| panic!("no batch"), one).unwrap();
        assert_eq!(got[&2], rows(2));
        assert!(asked.borrow().is_empty());

        // A member who joined since is asked for in one request; one the
        // batch leaves out falls to the per-user call.
        let got = member_devices(
            &[1, 3, 4],
            &read,
            |missing| {
                assert_eq!(missing, [3, 4]);
                Ok(HashMap::from([(3, rows(3))]))
            },
            one,
        )
        .unwrap();
        assert_eq!((got[&1].clone(), got[&3].clone()), (rows(1), rows(3)));
        assert_eq!(*asked.borrow(), [4]);

        // A server from before the batched call: one request per member.
        asked.borrow_mut().clear();
        let old = |_: &[u64]| Err(ApiError::Status(404, "not found".into()));
        let got = member_devices(&[5, 6], &Directory::Ask, old, one).unwrap();
        assert_eq!(got.len(), 2);
        assert_eq!(*asked.borrow(), [5, 6]);
        asked.borrow_mut().clear();
        member_devices(&[7], &Directory::PerUser, |_| panic!("no batch"), one).unwrap();
        assert_eq!(*asked.borrow(), [7]);

        // Any other failure ends the read, as a per-user one did.
        let down = |_: &[u64]| Err(ApiError::Network("down".into()));
        assert!(member_devices(&[8], &Directory::Ask, down, |_| panic!("no fallback")).is_err());
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
    fn only_what_was_loaded_counts_as_read() {
        let mut c = Cursor::default();
        // Read only through history, no live frame yet.
        c.page(40);
        assert_eq!(c.shown(), 40);
        // 41 and 42 are not here yet: 43 must not mark them read.
        c.live(43);
        assert_eq!(c.shown(), 40);
        c.page(43);
        assert_eq!(c.shown(), 43);
        c.live(44);
        assert_eq!(c.shown(), 44);
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
        // A newer edit shows while it is on its way. Once the server refuses
        // it, the message is back at the last edit the server has.
        let mut queued = m("e3", 0, 7, edit("fourth", "a"));
        (queued.seq, queued.delivery) = (None, Delivery::Sending);
        all.push(queued);
        apply_edits(&mut all);
        assert_eq!(all[0].body.text, "fourth");
        assert!(refuse(&mut all, "e3", "too long"));
        assert_eq!(all[6].delivery, Delivery::Failed("too long".into()));
        assert_eq!(all[0].body.text, "third");
        assert_eq!(all[0].edited.as_ref().unwrap().id, "e2");
        assert!(!refuse(&mut all, "nope", "x"));
        // With no other edit, it is the original again and not "(edited)".
        let mut one = vec![m("a", 1, 7, Body::text("first", None))];
        let mut queued = m("e1", 0, 7, edit("second", "a"));
        (queued.seq, queued.delivery) = (None, Delivery::Sending);
        one.push(queued);
        apply_edits(&mut one);
        assert_eq!(one[0].body.text, "second");
        assert!(refuse(&mut one, "e1", "refused"));
        assert_eq!(one[0].body.text, "first");
        assert!(one[0].edited.is_none());
    }

    #[test]
    fn a_team_read_from_before_an_invite_keeps_the_list_here() {
        let inv = |id: &str| TeamInvite {
            invite_id: id.into(),
            login: id.into(),
            invited_by_login: None,
            created_at: 1,
            expires_at: 2,
            invitee_state: None,
        };
        let here = Some(vec![inv("new")]);
        let read = Some(vec![inv("old")]);
        assert_eq!(invites_after_read(read.clone(), here.clone(), false), read);
        assert_eq!(invites_after_read(read.clone(), here.clone(), true), here);
        // An older server lists none: what was sent from here stays.
        assert_eq!(invites_after_read(None, here.clone(), false), here);
        assert_eq!(invites_after_read(None, None, false), None);
    }

    #[test]
    fn an_invite_that_waits_unseen_does_not_say_it_worked() {
        let said = |state| invite_sent("Rik", Some(state));
        assert!(
            said(InviteeState::Ready)
                .unwrap()
                .unwrap()
                .contains("Team tab")
        );
        assert!(
            said(InviteeState::NoAccount)
                .unwrap()
                .unwrap_err()
                .contains("not signed in")
        );
        assert!(
            said(InviteeState::NotEnabled)
                .unwrap()
                .unwrap_err()
                .contains("Arakun admin")
        );
        // An older server, or a state this app does not know, says nothing.
        assert_eq!(said(InviteeState::Unknown), None);
        assert_eq!(invite_sent("Rik", None), None);
    }

    #[test]
    fn the_unread_poll_reads_history_only_when_something_came() {
        // Never read here: it starts at the newest, so nothing is unread.
        assert_eq!(unread_poll(None, 40, None), Next::Done(Polled::Start(40)));
        assert_eq!(
            unread_poll(Some(40), 40, None),
            Next::Done(Polled::Count(0, 40))
        );
        assert_eq!(unread_poll(Some(30), 40, None), Next::Read(30));
        assert_eq!(unread_poll(Some(30), 40, Some(40)), Next::Keep);
        assert_eq!(unread_poll(Some(30), 41, Some(40)), Next::Read(30));
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
            former: HashMap::new(),
            channels: vec![],
            pending: vec![],
            invites: None,
            epoch_made: HashMap::new(),
            approved_by: None,
            keyless: false,
            problem: None,
            stale: HashSet::new(),
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
    fn only_unopened_messages_inside_the_range_count() {
        assert_eq!(unopened([3, 5, 9, 12].into_iter(), 5, 9), 2);
        assert_eq!(unopened([3, 12].into_iter(), 5, 9), 0);
        assert_eq!(unopened(std::iter::empty(), 0, u64::MAX), 0);
    }

    #[test]
    fn an_unread_count_reads_past_a_page_of_what_does_not_count() {
        let stored = |seq: u64, device: &str| Stored {
            seq,
            at: seq,
            message: WireMessage {
                id: seq.to_string(),
                device_id: device.into(),
                epoch: 1,
                nonce: String::new(),
                ct: String::new(),
                sig: String::new(),
            },
        };
        // 200 of this user's own messages, then three from someone else.
        let history: Vec<Stored> = (1..=203)
            .map(|seq| stored(seq, if seq <= 200 { "mine" } else { "theirs" }))
            .collect();
        let mut asked = Vec::new();
        let read = |after: u64| {
            asked.push(after);
            Ok(history
                .iter()
                .filter(|s| s.seq > after)
                .take(200)
                .cloned()
                .collect())
        };
        let n = unread_since(0, read, |s| s.message.device_id != "mine").unwrap();
        assert_eq!(n, 3);
        assert_eq!(asked, [0, 200]);

        // Enough for the label after one page: no second read.
        let mut asked = 0;
        let full = |after: u64| {
            asked += 1;
            Ok((after + 1..=after + 200)
                .map(|seq| stored(seq, "theirs"))
                .collect())
        };
        assert_eq!(unread_since(0, full, |_| true).unwrap(), 200);
        assert_eq!(asked, 1);

        // History with nothing that counts stops at the cap.
        let mut asked = 0;
        let endless = |after: u64| {
            asked += 1;
            Ok((after + 1..=after + 200)
                .map(|seq| stored(seq, "mine"))
                .collect())
        };
        assert_eq!(unread_since(0, endless, |_| false).unwrap(), 0);
        assert_eq!(asked, UNREAD_PAGES);
        assert!(unread_since(0, |_| Err(ApiError::Unreadable), |_| true).is_err());
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

    fn stale(device: &DeviceRow, epoch: u64) -> StaleEnvelope {
        StaleEnvelope {
            device_id: device.device_id.clone(),
            epoch,
        }
    }

    #[test]
    fn keys_from_a_demoted_admin_go_again_to_every_device_that_holds_them() {
        let net = offline_net();
        let root = Device::generate();
        // Both devices were there when the member joined (at 10), so the
        // approval covered them and nothing was owed to them before.
        let old = row(&root, 5, None);
        let other = row(&Device::generate(), 9, Some(&root));
        let mut t = delivery_team(vec![old.clone(), other.clone()], 3);
        t.detail.members[0].first_key_epoch = Some(2);
        // This admin's device approved them itself, so its own record says
        // they hold every key; the server kept the other admin's copies.
        let mut kept = Kept::default();
        let held = [(1, [1; 32]), (2, [2; 32]), (3, [3; 32])];
        let approved = seal_all(&net, "T", &held[1..], &[old.clone(), other.clone()]).unwrap();
        kept.admitted("T", 21, 3, &approved);
        kept.observe_grants(&t.detail);
        for (e, key) in held {
            kept.put_key("T", e, &key);
        }
        assert!(next_backfill(&kept, &t, "").is_empty());

        // The admin who sealed them was demoted: the server lists their
        // envelopes, the epoch before this membership's grant included.
        t.stale = [
            stale(&old, 1),
            stale(&old, 2),
            stale(&old, 3),
            stale(&other, 3),
        ]
        .into();
        let mut owed: Vec<_> = next_backfill(&kept, &t, "")
            .iter()
            .map(|s| (s.device.device_id.clone(), s.epoch))
            .collect();
        owed.sort();
        let mut want = vec![
            (old.device_id.clone(), 2),
            (old.device_id.clone(), 3),
            (other.device_id.clone(), 3),
        ];
        want.sort();
        assert_eq!(owed, want);
        // Not to this device itself, and never a key it does not hold.
        assert!(
            next_backfill(&kept, &t, &old.device_id)
                .iter()
                .all(|s| s.device.device_id == other.device_id)
        );
        let mut without = Kept::default();
        without.put_key("T", 2, &[2; 32]);
        without.observe_grants(&t.detail);
        let only = next_backfill(&without, &t, "");
        assert_eq!(only.len(), 1);
        assert_eq!(
            (only[0].epoch, &only[0].device.device_id),
            (2, &old.device_id)
        );

        // The same once the member is an admin: their devices get keys too.
        t.detail.members[0].role = Role::Admin;
        assert_eq!(next_backfill(&kept, &t, "").len(), 3);
        // A member whose device did not match gets no key, stale or not.
        kept.set_verified(&other.device_id, false);
        assert!(next_backfill(&kept, &t, "").is_empty());
        kept.set_verified(&other.device_id, true);

        // Sealed again and replaced on the server: nothing more is owed.
        let sent = upload_batch(&net, "T", next_backfill(&kept, &t, ""), |envs| {
            assert_eq!(envs.len(), 3);
            Ok(false)
        })
        .unwrap();
        record_batch(&mut kept, "T", &sent);
        t.stale.clear();
        assert!(next_backfill(&kept, &t, "").is_empty());
    }

    #[test]
    fn only_an_owner_changes_roles_and_a_team_keeps_one() {
        let member = |github_id, role| MemberRow {
            github_id,
            login: format!("u{github_id}"),
            role,
            joined_at: 0,
            first_key_epoch: Some(1),
        };
        let (owner, admin, plain) = (
            member(1, Role::Owner),
            member(2, Role::Admin),
            member(3, Role::Member),
        );
        let team = vec![owner.clone(), admin.clone(), plain.clone()];
        for to in [Role::Owner, Role::Admin] {
            assert!(may_give(Role::Owner, &team, &plain, to));
        }
        assert!(may_give(Role::Owner, &team, &admin, Role::Member));
        assert!(may_give(Role::Owner, &team, &admin, Role::Owner));
        // Nothing to change, and nobody but an owner changes anything.
        assert!(!may_give(Role::Owner, &team, &plain, Role::Member));
        for mine in [Role::Admin, Role::Member] {
            for to in [Role::Owner, Role::Admin, Role::Member] {
                assert!(!may_give(mine, &team, &plain, to));
            }
        }
        // The only owner cannot step down; one of two can, either one.
        assert!(!may_give(Role::Owner, &team, &owner, Role::Admin));
        let two = vec![owner.clone(), member(2, Role::Owner), plain.clone()];
        assert!(may_give(Role::Owner, &two, &owner, Role::Admin));
        assert!(may_give(Role::Owner, &two, &two[1], Role::Member));
    }

    #[test]
    fn a_key_from_someone_no_longer_an_admin_says_it_comes_again() {
        assert_eq!(key_problem(0, 0), None);
        assert_eq!(
            key_problem(1, 0).as_deref(),
            Some("1 team key for this device did not check out and was ignored.")
        );
        let both = key_problem(2, 3).unwrap();
        assert!(both.starts_with(
            "2 team keys for this device did not check out and were ignored. 3 team keys"
        ));
        assert!(both.ends_with("sends them again the next time it opens the team."));
        assert!(key_problem(0, 1).unwrap().contains("sends it again"));
    }

    /// Two members against a real Teams Worker, through the same calls and
    /// crypto the hub makes: create a team with its first key and a channel,
    /// invite, accept, approve with sealed keys, chat both ways over the
    /// sockets with a reply, read the history, then remove and rotate.
    ///
    /// Needs a server and two fresh users with the `teams` flag and no
    /// devices yet (see the Teams section of cloud/README.md for running one
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
        let read = read_team(&b, &team, false, &HashSet::new(), &Directory::Ask, &[]).unwrap();
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
            counted: true,
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

        // The two local model outputs are fixtures; the devices, encrypted
        // bodies, authenticated sockets and stored acknowledgements are real.
        let mut handoff = |net: &Net, link: &Link, who: &str, body: Body| {
            let id = send(net, link, &body);
            loop {
                let (recipient, stored) = heard(&id);
                if recipient == who {
                    assert!(crypto::verify_message(
                        &team,
                        &channel,
                        &stored.message,
                        &crypto::unb64(net.local.device.sig_pub()).unwrap()
                    ));
                    return crypto::open_message(&team, &channel, &stored.message, &key).unwrap();
                }
            }
        };
        let scope = channel_key(&team, &channel);
        let mut lead = Grant::new(scope.clone(), 1, mb.github_id);
        let mut reviewer = Grant::new(scope, 1, ma.github_id);
        let offer = handoff(
            &b,
            &lb,
            "a",
            reviewer.ready().body("Available".into(), None),
        );
        let Some(PeerEvent::Ready { peer, ticket }) = PeerEvent::read(&offer) else {
            panic!("no reviewer")
        };
        assert_eq!(peer, ma.github_id);
        let goal = "Check the release plan";
        let ask = lead.request(ticket, goal).unwrap();
        let got = handoff(&a, &la, "b", ask.body(goal.into(), None));
        let review = reviewer
            .receive(ma.github_id, PeerEvent::read(&got).unwrap(), &got.text)
            .unwrap();
        let got = handoff(
            &b,
            &lb,
            "a",
            review.response.body("Add a rollback check".into(), None),
        );
        let synthesis = lead
            .receive(mb.github_id, PeerEvent::read(&got).unwrap(), &got.text)
            .unwrap();
        assert!(synthesis.prompt.contains("Add a rollback check"));
        let got = handoff(
            &a,
            &la,
            "b",
            synthesis
                .response
                .body("Test the rollback before release".into(), None),
        );
        assert_eq!(PeerEvent::read(&got), Some(PeerEvent::Done));

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
        let read2 = read_team(&second, &team, true, &HashSet::new(), &Directory::Ask, &[]).unwrap();
        assert_eq!(read2.keys, vec![(1, key)]);

        // A removes B: a new key for A's devices only, and B is out. Until
        // it is made, the server takes nothing, not even the old key.
        a.call().remove_member(&team, mb.github_id).unwrap();
        let mut closed = false;
        let old = crypto::seal_message(
            &team,
            &channel,
            &crypto::ulid(),
            1,
            &key,
            &a.local.device,
            &Body::text("during the rotation", None),
        );
        la.send(serde_json::json!({ "type": "send", "message": old }).to_string());
        loop {
            match next() {
                (who, socket::Event::Status(socket::Status::Refused(_))) if who == "b" => {
                    closed = true
                }
                (who, socket::Event::Frame(v)) if who == "a" && v["id"] == old.id.as_str() => {
                    assert_eq!(refusal(&v), Refusal::OldKey, "{v}");
                    break;
                }
                _ => {}
            }
        }
        // B wrote in the channel, so A still finds B's devices to check what
        // B wrote, though they share no team any more.
        let wrote = [(channel.clone(), b.local.device.id().to_string())];
        let read = read_team(
            &a,
            &team,
            true,
            &HashSet::from([1]),
            &Directory::Ask,
            &wrote,
        )
        .unwrap();
        assert_eq!(read.former.len(), 1);
        assert_eq!(read.former[0].0, mb.github_id);
        assert!(read.looked_up.contains(b.local.device.id()));
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
    // tungstenite's handshake callback returns its large error type.
    #[allow(clippy::result_large_err)]
    fn only_the_picked_channel_counts_toward_presence() {
        use std::net::TcpListener;
        use tungstenite::Message;
        // A server that answers every socket and reports, by path, how it
        // was dialed and each `presence` frame it got.
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let (seen_tx, seen) = std::sync::mpsc::channel::<(String, String)>();
        std::thread::spawn(move || {
            for conn in listener.incoming().flatten() {
                let seen_tx = seen_tx.clone();
                std::thread::spawn(move || {
                    let mut path = String::new();
                    let Ok(mut ws) = tungstenite::accept_hdr(
                        conn,
                        |req: &tungstenite::handshake::server::Request, res| {
                            path = req.uri().to_string();
                            Ok(res)
                        },
                    ) else {
                        return;
                    };
                    let (route, query) = path.split_once('?').unwrap_or((&path, ""));
                    let route = route.to_string();
                    let _ = seen_tx.send((route.clone(), format!("dial {query}")));
                    while let Ok(m) = ws.read() {
                        if let Message::Text(t) = m {
                            let v: serde_json::Value = serde_json::from_str(t.as_str()).unwrap();
                            let _ = seen_tx.send((route.clone(), format!("on {}", v["on"])));
                        }
                    }
                });
            }
        });
        let next = || {
            seen.recv_timeout(Duration::from_secs(10))
                .expect("nothing came in time")
        };
        let (tx, _rx) = mpsc::unbounded();
        let mut channels = HashMap::new();
        for (key, counted) in [("a", false), ("b", false), ("c", true)] {
            let mut c = ChannelState::new("t", key);
            c.link = Some(Link::open(
                key.into(),
                Dial {
                    url: format!("ws://127.0.0.1:{port}/{key}"),
                    session: "s".into(),
                    device: "d".into(),
                    counted,
                },
                tx.clone(),
            ));
            c.here = vec![1, 2];
            channels.insert(key.to_string(), c);
        }
        let mut dials: Vec<_> = (0..3).map(|_| next()).collect();
        dials.sort();
        assert_eq!(
            dials,
            [
                ("/a".to_string(), "dial presence=0".to_string()),
                ("/b".into(), "dial presence=0".into()),
                ("/c".into(), "dial ".into()),
            ]
        );
        // Picking a counts it; the one picked before goes quiet and forgets
        // who was there. Neither hangs up.
        move_presence(&mut channels, Some("c"), Some("a"));
        let mut got = vec![next(), next()];
        got.sort();
        assert_eq!(
            got,
            [
                ("/a".to_string(), "on true".to_string()),
                ("/c".into(), "on false".into()),
            ]
        );
        assert!(channels["c"].here.is_empty());
        assert_eq!(channels["b"].here, [1, 2]);
        // Leaving the tab's channels for a page with none: nothing counts.
        move_presence(&mut channels, Some("a"), None);
        assert_eq!(next(), ("/a".to_string(), "on false".to_string()));
        move_presence(&mut channels, None, None);
        assert!(seen.recv_timeout(Duration::from_millis(200)).is_err());
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
