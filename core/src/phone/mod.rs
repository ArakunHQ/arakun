//! The phone: a small web page arakun serves on the LAN so a phone can watch
//! sessions, read them live, send prompts and answer the agent. The page lives
//! in `phone/` and is embedded in the binary; this module is its server and the
//! wire format (`wire`). The shell does the thinking (`shell::phone`): it owns
//! the threads, and gets what phones say through a channel, like `control`.
//!
//! Plain HTTP and WebSocket on one TCP port, no TLS of its own. Phones reach it
//! one of two ways (`Reach`): over the local network, listening on every
//! interface; or through Tailscale (`tailscale`), listening on the loopback only
//! while `tailscale serve` terminates HTTPS at the machine's tailnet name and
//! hands the bytes over. Plain HTTP only carries the page itself. Everything
//! else goes on one WebSocket (`lan`), sealed end to end as through the relay
//! (`e2e`), so someone on the same Wi-Fi reads nothing of it. Only paired
//! phones may connect: pairing starts on the computer with a short code that
//! lives two minutes and works once (the QR code carries it, with the label of
//! the key the phone then checks), and gives the phone a token of its own,
//! which the computer keeps hashed and can revoke. Every connection brings that
//! token inside the sealed channel.

pub mod e2e;
pub mod identity;
pub mod lan;
pub mod linear;
pub mod notify;
pub mod push;
mod quiet;
pub mod relay;
pub mod tailscale;
mod transport;
mod upload;
pub mod wire;

pub use transport::Transport;
pub use upload::Uploads;

use std::collections::HashMap;
use std::io::{Read as _, Write as _};
use std::net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream, ToSocketAddrs as _, UdpSocket};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, mpsc as std_mpsc};
use std::time::{Duration, Instant};

use futures::channel::mpsc;
use serde::{Deserialize, Serialize};
use sha2::Digest as _;

use crate::phone_page::Page;
use crate::secret::Secret;

pub const DEFAULT_PORT: u16 = 7474;
/// How long a pairing code is good for.
pub const PAIR_FOR: Duration = Duration::from_secs(120);
/// Wrong codes before the current one is thrown away.
const PAIR_TRIES: u8 = 5;
/// The most a request body may carry (a pairing request is a few dozen bytes).
const MAX_BODY: usize = 4096;
/// How long a request body may take to arrive, all of it.
const BODY_DEADLINE: Duration = Duration::from_secs(5);
/// A phone is pinged this often, and dropped after this long without a word:
/// two missed pings. One that left the network without closing would pass for
/// someone watching until TCP gave up, minutes later.
/// Short in tests, which wait for a phone to be dropped.
const PHONE_PING_EVERY: Duration = if cfg!(test) {
    Duration::from_millis(300)
} else {
    Duration::from_secs(15)
};
const PHONE_SILENCE_MAX: Duration = if cfg!(test) {
    Duration::from_secs(1)
} else {
    Duration::from_secs(40)
};
/// How long one write to a phone may block. A page that stopped reading (a
/// frozen tab) would hold its connection's thread forever.
const PHONE_WRITE_TIMEOUT: Duration = Duration::from_secs(10);
/// How long one pass may spend sending queued messages before it reads again.
/// A long queue of slow writes would otherwise leave the phone's pongs unread
/// until it looked silent.
const PHONE_SEND_PASS: Duration = Duration::from_millis(100);
/// Messages waiting for one phone. A phone that lets this many pile up is not
/// reading, and is dropped rather than kept in memory.
const PHONE_QUEUE: usize = 1024;
/// A few phones each load the page and keep a socket; anyone on the network
/// can open connections, so one address must not take every place.
const CONNECTIONS: crate::accept::Limits = crate::accept::Limits {
    total: 128,
    per_peer: 32,
};

pub type ClientId = u64;

/// What a connected phone asks for.
#[derive(Deserialize, Debug, Clone, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ClientMsg {
    /// Watch a session: its last entries now, then every change. It takes the
    /// place of the one watched before, unless `keep` (a desktop following
    /// several): then both are watched.
    Open {
        session: String,
        #[serde(default)]
        keep: bool,
    },
    /// Stop watching `session`, or every session when it says none (back to
    /// the inbox).
    Close {
        #[serde(default)]
        session: Option<String>,
    },
    /// Older entries of the watched session, `from..to`.
    Entries {
        session: String,
        from: usize,
        to: usize,
    },
    Send {
        session: String,
        text: String,
        /// The photos this prompt carries: ids of finished `Upload`s, whose
        /// saved files are attached as `@` mentions when the prompt goes out.
        #[serde(default)]
        uploads: Vec<String>,
        /// Stops the running turn and sends this as soon as it stopped,
        /// instead of queueing it behind the turn (`Ctrl+Enter` in the app).
        /// A server without it queues the prompt, as before.
        #[serde(default)]
        now: bool,
    },
    /// A piece of a photo the phone is sending: `data` is base64, piece `part`
    /// of `of` under `upload`'s id. Kept apart because a relay frame is small;
    /// the pieces are saved as a pasted image once the last one lands.
    Upload {
        session: String,
        upload: String,
        name: String,
        part: usize,
        of: usize,
        data: String,
    },
    /// A new session in the environment at `env` (its path), with `text` as its
    /// first prompt. Without an agent, the one that environment picks.
    Start {
        env: String,
        #[serde(default)]
        agent: Option<String>,
        text: String,
        /// Settings picked for it, `[key, value]` (`modelId`, `reasoningEffort`),
        /// the model first. The environment remembers them, as the desktop does.
        #[serde(default)]
        settings: Vec<(String, String)>,
        /// The folder the prompt was written in, when that is another: a
        /// relative mention of a file there but not in `env` points there.
        #[serde(default)]
        mentions_from: Option<String>,
    },
    /// The model and reasoning a new session of `agent` in `env` would start with,
    /// and their choices.
    Options {
        env: String,
        agent: String,
    },
    /// The same for a session, as it runs.
    Settings {
        session: String,
    },
    /// Changes one of a session's settings (`key` as in `Start`).
    Setting {
        session: String,
        // Not a secret: the setting's name.
        key: String,
        value: String,
    },
    Cancel {
        session: String,
        /// Pauses the queued follow-ups instead of dropping them, as Esc does
        /// in the app. A server without it drops them, as before.
        #[serde(default)]
        keep_queue: bool,
    },
    /// Takes one queued follow-up out of the queue: `now` sends it as soon as
    /// the running turn stopped (which it stops), otherwise it is dropped.
    /// `text` is the follow-up's own, so one that moved in the queue since is
    /// still found, and one that went already is left alone.
    Unqueue {
        session: String,
        index: usize,
        text: String,
        #[serde(default)]
        now: bool,
    },
    /// Lets the queue go again after an interrupt paused it.
    ResumeQueue {
        session: String,
    },
    Permission {
        session: String,
        option: String,
    },
    /// The agent's questionnaire answered: one answer per question, picked
    /// options and any typed words joined with ", ".
    Answer {
        session: String,
        answers: Vec<String>,
    },
    SkipQuestion {
        session: String,
    },
    /// The secret card answered: what was typed, one value per field, or no
    /// `values` to decline. Refused when the connection is not encrypted.
    Secret {
        session: String,
        id: u64,
        #[serde(default)]
        values: Option<crate::agent::secret::Typed>,
    },
    /// Copies the session into a new one. Answered with `forked` once the new
    /// one is in the inbox.
    Fork {
        session: String,
    },
    /// Hands the session to another agent there: a session of `agent` takes
    /// its place, shown the conversation so far and told about it with the
    /// next prompt, and the old one is gone. `settings` are the model and the
    /// reasoning picked for `agent`, as with `start`. Answered with
    /// `continued` once the new one is in the inbox.
    ContinueWith {
        session: String,
        agent: String,
        #[serde(default)]
        settings: Vec<(String, String)>,
    },
    /// Summarizes the session's conversation so far, to free context. What it
    /// did shows in the session.
    Compact {
        session: String,
    },
    /// Rewinding the session to before its prompt at `entry`. Without
    /// `confirm`, what it would undo, answered with `rewind_plan`; with it,
    /// the rewind itself: the files go back, and the session before the
    /// prompt is a new one, answered with `rewound`. A `rewind_plan` with
    /// `gone` ends either, and says why when it failed.
    Rewind {
        session: String,
        entry: usize,
        #[serde(default)]
        confirm: bool,
    },
    /// Stages with git only the files the session changed. Answered with
    /// `staged`.
    Stage {
        session: String,
    },
    /// The files the session changed, turn by turn.
    Changes {
        session: String,
    },
    /// One diff of the session: the entry it is in and its index there.
    Diff {
        session: String,
        entry: usize,
        index: usize,
    },
    /// The session's entries from `from` on, whole, as its transcript holds
    /// them: what the app asks of a server, to show a session as its own.
    Transcript {
        session: String,
        #[serde(default)]
        from: usize,
    },
    /// An image the session's agent named by its path, for the app to show:
    /// the file is on the server. Answered with `image`.
    Image {
        session: String,
        path: String,
        /// The entry that named it last when asked, told back with the answer.
        #[serde(default)]
        at: usize,
    },
    /// Makes a folder already on the server one sessions can start in. `~/`
    /// stands for the home folder there. Answered with `place_added`.
    AddPlace {
        path: String,
    },
    /// Clones a repository on the server and makes its folder a place: a URL,
    /// a `git clone` line or a GitHub `owner/repo`. Answered with `place_added`
    /// once the clone is done.
    Clone {
        url: String,
    },
    /// The desktop app's Linear settings for this server: which Linear
    /// teams and projects it takes work for, in which of its places, and the
    /// key to read those issues with. Answered with `linear_settings`.
    LinearSettings(crate::linear::remote::ServerSettings),
    /// A new worktree of the repository `from` is a checkout of, on a new
    /// `branch`, made a place. Answered with `place_added`.
    NewEnv {
        from: String,
        branch: String,
    },
    /// Renames the branch checked out at place `place` from `from` to `to`,
    /// unless it was renamed meanwhile. Answered with the inbox.
    RenameBranch {
        place: String,
        from: String,
        to: String,
    },
    /// The sessions at place `place` stop asking for attention: those not
    /// working go idle, and their agents stop unless a device has them open.
    /// Answered with the inbox.
    SettleEnv {
        place: String,
    },
    /// Removes the worktree at place `place`, its folder, and its sessions;
    /// with `delete_branch`, its branch too. A repository's main folder is
    /// never removed. Answered with `env_deleted`: whether the folder is gone
    /// (`deleted`), and what went wrong (`error`).
    DeleteEnv {
        place: String,
        #[serde(default)]
        delete_branch: bool,
        /// The branch the user agreed to delete. One checked out there
        /// since is not deleted, and neither is the folder.
        #[serde(default)]
        branch: Option<String>,
    },
    /// Both agents' plan limits, as the desktop knows them.
    Usage,
    /// The Claude Code plugins this machine's marketplaces offer, and the MCP
    /// servers Claude has here: what the app asks of a server to install
    /// there. Answered with a `market` message.
    Market,
    /// Installs a plugin (`name@marketplace`) for the user here, on in every
    /// project. Answered with `market_done`.
    InstallPlugin {
        id: String,
    },
    /// Adds an MCP server from the registry for the user here, `install` filled
    /// in with `values`. Answered with `market_done`.
    InstallServer {
        name: String,
        install: crate::agent::marketplace::Install,
        #[serde(default)]
        values: HashMap<String, String>,
    },
    /// What the app installed from the marketplace on its own computer: the
    /// server adds what it lacks, removing nothing. Answered with `mirror_done`.
    Mirror(crate::agent::mirror::Snapshot),
    /// Whether the page is on the phone's screen. A page in the background
    /// keeps its socket, so without this it would pass for someone watching.
    Visible {
        visible: bool,
    },
    /// How long the person has been idle at the desktop app's computer, in
    /// seconds. The app following this server says it on change and at least
    /// every 30 s; phones never do. A server has no display to tell.
    Idle {
        secs: u64,
    },
    /// A terminal in place `place`, under the id `term` the app picked: the
    /// login shell there, running `run` first when it says one. Its output
    /// follows as `term_output`, then `term_exit` once it ends.
    TermOpen {
        term: String,
        place: String,
        #[serde(default)]
        run: Option<String>,
        cols: u16,
        rows: u16,
    },
    /// Follows terminal `term` again, from offset `from` of its output: what
    /// the app missed while the connection was down.
    TermAttach {
        term: String,
        from: u64,
    },
    /// Bytes typed into a terminal, base64.
    TermInput {
        term: String,
        data: String,
    },
    TermResize {
        term: String,
        cols: u16,
        rows: u16,
    },
    /// The app drew a terminal's output up to offset `at`: more may come.
    TermSeen {
        term: String,
        at: u64,
    },
    /// Hangs a terminal up.
    TermClose {
        term: String,
    },
    /// Git in place `place`, for the app's Changes panel: status, diffs,
    /// staging, commits, pushes and pull requests, with this machine's `git`
    /// and `gh` logins. Answered with `git`, which carries the same `ref`.
    Git {
        #[serde(rename = "ref")]
        reference: u64,
        place: String,
        call: crate::git_call::Call,
    },
    /// The folder `dir` of place `place` (relative, `""` for the place
    /// itself), for the app's Explorer. Answered with `dir`.
    ListDir {
        place: String,
        #[serde(default)]
        dir: String,
    },
    /// The text of file `path` of place `place` (relative), for a file tab.
    /// Answered with `file`.
    ReadFile {
        place: String,
        path: String,
    },
    /// The files of place `place` that `@` mentions complete. Answered with
    /// `files`.
    ListFiles {
        place: String,
    },
    Ping,
}

pub enum Incoming {
    /// A paired device opened a socket.
    Connected(ClientId, String),
    Message(ClientId, ClientMsg),
    Disconnected(ClientId),
    /// A new device brought the right code and got its token; `Access` has it.
    Paired,
    /// A device the relay brought in connected for the first time; `Access`
    /// has it now. `name` and `kind` are its signed-in device's, as the
    /// relay named them.
    NewDevice {
        name: String,
        kind: String,
    },
    /// A device the relay brought in came back under a new name; `Access`
    /// has it.
    Renamed,
    /// A device turned its push notifications on or off, or chose which; `Access` has it.
    Subscribed,
    /// A device unpaired itself; `Access` no longer has it and its sockets are closing.
    Unpaired,
    /// The connection to the relay changed state.
    Relay(relay::Status),
    /// The relay passed on an issue delegated to Arakun in Linear. It may
    /// come again: the relay resends it until the app takes it.
    Linear(linear::Delivery),
}

/// A paired phone. Its token is kept only as a hash: the file does not open the door.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Device {
    pub id: String,
    pub name: String,
    /// Empty for a device the relay brought in: it has no token, and an
    /// empty hash never matches one (`device_for`).
    // Not a secret: a hash, which opens nothing.
    pub token_hash: String,
    /// What the relay used to check for a phone paired through it. The relay
    /// admits devices by account now; this is only read from old files and
    /// ignored.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub ticket_hash: String,
    pub paired_at: u64,
    pub last_seen: u64,
    /// Where to push notifications to this phone, while it wants them.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub push: Option<push::Subscription>,
    /// Which of them it wants.
    #[serde(default)]
    pub notices: Notices,
    /// Sessions it asked to hear nothing about, by the id the phone sees.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub muted: Vec<String>,
}

/// What a phone asks to be told about. Everything, until it says otherwise.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
pub struct Notices {
    /// A turn ended.
    pub done: bool,
    /// The agent needs a permission or an answer.
    pub input: bool,
    /// A turn failed.
    pub error: bool,
    /// How long the person is away from the computer before a notice goes
    /// out. 0 sends at once.
    #[serde(default = "default_away_after_secs")]
    pub away_after_secs: u32,
    /// A `done` for a shorter turn stays quiet. 0 tells of every turn.
    #[serde(default = "default_min_turn_secs")]
    pub min_turn_secs: u32,
}

fn default_away_after_secs() -> u32 {
    120
}

fn default_min_turn_secs() -> u32 {
    30
}

impl Default for Notices {
    fn default() -> Self {
        Notices {
            done: true,
            input: true,
            error: true,
            away_after_secs: default_away_after_secs(),
            min_turn_secs: default_min_turn_secs(),
        }
    }
}

/// What `POST /push` changes in `Notices`: a field it leaves out stays as it was.
#[derive(Deserialize)]
struct NoticesChange {
    done: Option<bool>,
    input: Option<bool>,
    error: Option<bool>,
    away_after_secs: Option<u32>,
    min_turn_secs: Option<u32>,
}

impl NoticesChange {
    fn apply(&self, n: Notices) -> Notices {
        Notices {
            done: self.done.unwrap_or(n.done),
            input: self.input.unwrap_or(n.input),
            error: self.error.unwrap_or(n.error),
            away_after_secs: self.away_after_secs.unwrap_or(n.away_after_secs),
            min_turn_secs: self.min_turn_secs.unwrap_or(n.min_turn_secs),
        }
    }
}

/// How phones get to this computer.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, Default, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Reach {
    /// The same network, in the clear: the server listens on every interface.
    #[default]
    Lan,
    /// The tailnet, over HTTPS at the machine's name: the server listens on the
    /// loopback and `tailscale serve` brings the connections.
    Tailscale,
    /// Through the relay on the internet, end to end encrypted: this computer
    /// dials out to `relay_base` with its account's session, and devices
    /// signed in to the same account meet it there.
    Relay,
}

impl Reach {
    /// The address the server listens on. The relay needs no listener; the
    /// loopback keeps the page reachable here while that mode takes shape.
    pub fn bind(self) -> Ipv4Addr {
        match self {
            Reach::Lan => Ipv4Addr::UNSPECIFIED,
            Reach::Tailscale | Reach::Relay => Ipv4Addr::LOCALHOST,
        }
    }
}

/// Where the relay lives by default: the account service, which the session
/// comes from too.
pub const DEFAULT_RELAY_BASE: &str = crate::cloud::DEFAULT_URL;

/// The default before relays went by account, when each computer had a host
/// under it. It answers only "update" now, so a file that still names it means
/// the default.
const FORMER_RELAY_BASE: &str = "https://arakun.dev";

fn default_relay_base() -> String {
    DEFAULT_RELAY_BASE.to_string()
}

fn relay_base_or_default<'de, D: serde::Deserializer<'de>>(d: D) -> Result<String, D::Error> {
    let base = String::deserialize(d)?;
    Ok(if base.trim().trim_end_matches('/') == FORMER_RELAY_BASE {
        default_relay_base()
    } else {
        base
    })
}

/// What is kept across runs: whether the phone is on, how phones reach it, and
/// the devices paired so far.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Settings {
    pub enabled: bool,
    pub port: u16,
    #[serde(default)]
    pub reach: Reach,
    /// The key pair push requests are signed with (PKCS#8, base64url), as a
    /// file from before the keyring kept it. The keyring keeps it now
    /// (`vapid`); this holds it only until it is moved there, or while the
    /// keyring will not take it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    vapid: Option<Secret>,
    /// Who this computer is to the relay (PKCS#8, base64url), as a file from
    /// before the keyring kept it, until it is moved there (`identity`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    identity: Option<Secret>,
    /// The public half of the relay identity (the raw point, base64url). The
    /// label derives from it, so `relay_label` needs no keyring.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    identity_public: Option<String>,
    /// The relay's origin (scheme and host). Changed by hand to point at a
    /// relay of one's own, together with `ARAKUN_TEAMS_URL` for the session.
    #[serde(
        default = "default_relay_base",
        deserialize_with = "relay_base_or_default"
    )]
    pub relay_base: String,
    #[serde(default)]
    pub devices: Vec<Device>,
    /// The GitHub id of the account the relay's devices were let in by:
    /// under another account they are dropped (`relay_owner`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub relay_account: Option<u64>,
    /// The sidebar's card that invites to the phone app beta was closed.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub beta_card_closed: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            enabled: false,
            port: DEFAULT_PORT,
            reach: Reach::Lan,
            vapid: None,
            identity: None,
            identity_public: None,
            relay_base: default_relay_base(),
            devices: Vec::new(),
            relay_account: None,
            beta_card_closed: false,
        }
    }
}

impl Settings {
    /// Where devices find this computer through the relay: the relay's own
    /// page, which lists the account's computers.
    pub fn relay_address(&self) -> String {
        relay::page_url(&self.relay_base)
    }

    /// Forgets the relay identity and every paired phone, with a new key in
    /// the keyring: the computer gets a new address, and nothing paired
    /// against the old one may keep working. When the keyring does not take
    /// the new key, nothing changes. The caller saves. Asks the keyring, so
    /// never on the UI thread.
    pub fn renew_identity(&mut self) -> Result<identity::Identity, String> {
        self.renew_identity_in(&Keyring, &key_account(IDENTITY))
    }

    fn renew_identity_in(
        &mut self,
        vault: &impl Vault,
        account: &str,
    ) -> Result<identity::Identity, String> {
        let _held = KEYS.lock().unwrap_or_else(|e| e.into_inner());
        let id = identity::Identity::generate();
        vault
            .set(account, &label(IDENTITY_KEY), &id.pkcs8_b64())
            .map_err(|e| format!("The keyring could not keep the new {IDENTITY_KEY}: {e}"))?;
        self.identity = None;
        self.identity_public = Some(b64(id.public_raw()));
        self.devices.clear();
        Ok(id)
    }

    /// This computer's label on the relay, when it has a key yet. Unlike
    /// `identity`, makes none and reads no keyring: the label comes from the
    /// public half in the file.
    pub fn relay_label(&self) -> Option<String> {
        if let Some(id) = self
            .identity
            .as_ref()
            .map(Secret::expose)
            .and_then(identity_from)
        {
            return Some(id.label());
        }
        let public = base64::Engine::decode(&B64, self.identity_public.as_ref()?).ok()?;
        Some(identity::label_of(&public))
    }

    /// The computer's relay identity, from the keyring, made the first time,
    /// and saved when the file changed. Asks the keyring, so never on the UI
    /// thread. An error is a keyring that could not say (locked, or its
    /// prompt dismissed): no key is made then, as the one it holds may be
    /// what every phone is paired with.
    pub fn identity(&mut self) -> Result<identity::Identity, String> {
        let (id, changed) = self.identity_in(&Keyring, &key_account(IDENTITY))?;
        if changed {
            self.save();
        }
        Ok(id)
    }

    fn identity_in(
        &mut self,
        vault: &impl Vault,
        account: &str,
    ) -> Result<(identity::Identity, bool), String> {
        let before = (self.identity.clone(), self.identity_public.clone());
        let id = private_key(
            &mut self.identity,
            vault,
            account,
            IDENTITY_KEY,
            identity_from,
            || {
                let id = identity::Identity::generate();
                let secret = id.pkcs8_b64();
                (id, secret)
            },
        )?;
        self.identity_public = Some(b64(id.public_raw()));
        let changed = before != (self.identity.clone(), self.identity_public.clone());
        Ok((id, changed))
    }

    /// The push signing keys, from the keyring, made the first time, and
    /// saved when the file changed. Like `identity`, never on the UI thread.
    pub fn vapid(&mut self) -> Result<push::Vapid, String> {
        let (v, changed) = self.vapid_in(&Keyring, &key_account(VAPID))?;
        if changed {
            self.save();
        }
        Ok(v)
    }

    fn vapid_in(
        &mut self,
        vault: &impl Vault,
        account: &str,
    ) -> Result<(push::Vapid, bool), String> {
        let before = self.vapid.clone();
        let v = private_key(
            &mut self.vapid,
            vault,
            account,
            VAPID_KEY,
            vapid_from,
            || {
                let v = push::Vapid::generate();
                let secret = v.pkcs8_b64();
                (v, secret)
            },
        )?;
        Ok((v, before != self.vapid))
    }

    /// The keys a phone link runs with: the push keys, and the identity
    /// that signs every phone's handshake, on the network as through the
    /// relay. Reads the file and the keyring, so it runs on a background
    /// thread. Only the key fields are written back, to a fresh read of the
    /// file, so a save the window made meanwhile is kept.
    pub fn load_keys() -> Result<Keys, String> {
        let mut settings = Settings::load();
        let (vapid, mut changed) = settings.vapid_in(&Keyring, &key_account(VAPID))?;
        let (identity, c) = settings.identity_in(&Keyring, &key_account(IDENTITY))?;
        changed |= c;
        if changed {
            let mut now = Settings::load();
            now.vapid = settings.vapid;
            now.identity = settings.identity;
            now.identity_public = settings.identity_public;
            now.save_soon();
        }
        Ok(Keys { vapid, identity })
    }
}

/// The private keys a phone link runs with, as `Settings::load_keys` gives them.
pub struct Keys {
    pub vapid: push::Vapid,
    /// The key phones check the handshake against, here or through the relay.
    pub identity: identity::Identity,
}

/// The keyring names of the private keys, before `key_account` scopes them.
const IDENTITY: &str = "phone-identity";
const VAPID: &str = "phone-vapid";
/// What errors call them; the keyring's own app shows them with `label`.
const IDENTITY_KEY: &str = "phone relay key";
const VAPID_KEY: &str = "phone push key";

fn label(what: &str) -> String {
    format!("arakun: {what}")
}

const B64: base64::engine::GeneralPurpose = base64::engine::general_purpose::URL_SAFE_NO_PAD;

/// The keyring account for `name`, for the phone file this process uses. A
/// test Arakun (`ARAKUN_STATE`) and `arakun server` each have a phone file
/// of their own, so each keeps keys of its own too: with the app's keys,
/// the server would take the app's place at the relay.
fn key_account(name: &str) -> String {
    match SETTINGS_FILE.get() {
        Some(path) => crate::secret_store::account_for(
            name,
            Some(&crate::secret_store::profile(&path.to_string_lossy())),
        ),
        None => crate::secret_store::account(name),
    }
}

fn b64(bytes: &[u8]) -> String {
    base64::Engine::encode(&B64, bytes)
}

fn identity_from(s: &str) -> Option<identity::Identity> {
    identity::Identity::from_pkcs8(&base64::Engine::decode(&B64, s).ok()?)
}

fn vapid_from(s: &str) -> Option<push::Vapid> {
    push::Vapid::from_pkcs8(&base64::Engine::decode(&B64, s).ok()?)
}

/// Where the private keys are kept: the keyring, or a fake one in the tests.
trait Vault {
    fn get(&self, account: &str) -> Result<Option<String>, String>;
    fn set(&self, account: &str, label: &str, secret: &str) -> Result<(), String>;
    /// Whether to try moving a file's key to the keyring now. Once a run is
    /// enough: a keyring that said no would be asked again on every relay
    /// restart, and may prompt each time.
    fn try_move(&self, account: &str) -> bool;
}

/// The OS keyring (`secret_store`), with what it gave kept in memory: the
/// keys are asked for on every link start, and a prompt each time would be
/// too many.
struct Keyring;

static CACHE: Mutex<Option<HashMap<String, String>>> = Mutex::new(None);
static MOVES: Mutex<Option<std::collections::HashSet<String>>> = Mutex::new(None);

impl Vault for Keyring {
    fn get(&self, account: &str) -> Result<Option<String>, String> {
        let cached = CACHE
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get_or_insert_default()
            .get(account)
            .cloned();
        if cached.is_some() {
            return Ok(cached);
        }
        let got = crate::secret_store::get(account)?;
        if let Some(secret) = &got {
            remember(account, secret);
        }
        Ok(got)
    }

    fn set(&self, account: &str, label: &str, secret: &str) -> Result<(), String> {
        crate::secret_store::set(account, label, secret)?;
        remember(account, secret);
        Ok(())
    }

    fn try_move(&self, account: &str) -> bool {
        MOVES
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get_or_insert_default()
            .insert(account.to_string())
    }
}

fn remember(account: &str, secret: &str) {
    CACHE
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get_or_insert_default()
        .insert(account.to_string(), secret.to_string());
}

/// One key is looked up, or made, at a time: two threads that both found
/// none would each make one, and phones would pair with the one that lost.
static KEYS: Mutex<()> = Mutex::new(());

/// A private key: the copy an old file holds first, else the keyring's, else
/// a new one when the keyring says it has none.
///
/// The file's copy is what phones were paired with, so it wins and moves to
/// the keyring; `file` is cleared only once the keyring took it, and the
/// caller saves. When the keyring refuses, the file's copy is still used. A
/// keyring that cannot say (`Err`) never leads to a new key: a locked one
/// must not look like a first run.
fn private_key<T>(
    file: &mut Option<Secret>,
    vault: &impl Vault,
    account: &str,
    what: &str,
    parse: impl Fn(&str) -> Option<T>,
    make: impl FnOnce() -> (T, String),
) -> Result<T, String> {
    let _held = KEYS.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(kept) = file.clone()
        && let Some(key) = parse(kept.expose())
    {
        if vault.try_move(account) {
            match vault.set(account, &label(what), kept.expose()) {
                Ok(()) => *file = None,
                Err(e) => eprintln!(
                    "arakun: the keyring did not take the {what}, so it stays in the phone settings: {e}"
                ),
            }
        }
        return Ok(key);
    }
    match vault.get(account) {
        Ok(Some(secret)) => {
            let key =
                parse(&secret).ok_or_else(|| format!("The {what} in the keyring is damaged"))?;
            // A copy in the file that does not parse is of no use.
            *file = None;
            Ok(key)
        }
        Ok(None) => {
            let (key, secret) = make();
            // Used only once the keyring has it: a key kept nowhere would be
            // paired with and then lost.
            vault
                .set(account, &label(what), &secret)
                .map_err(|e| format!("The keyring could not keep the new {what}: {e}"))?;
            *file = None;
            Ok(key)
        }
        Err(e) => Err(format!("The keyring could not give the {what}: {e}")),
    }
}

struct Pairing {
    /// The six digits a phone brings to this computer's door on the network.
    code: String,
    until: Instant,
    failures: u8,
}

/// Who may connect: the paired devices, and while pairing, the code a new one must bring.
/// Shared between the shell, which pairs and unpairs, and the server threads, which check.
pub struct Access {
    pub devices: Vec<Device>,
    pairing: Option<Pairing>,
    /// Signs the links to the images prompts mention. New every run: a link is
    /// only good for the file it names, and only while this instance serves it.
    pub image_key: String,
    /// The public half of the push signing keys, which phones subscribe with.
    pub push_key: String,
}

pub type Shared = Arc<Mutex<Access>>;

impl Access {
    pub fn new(devices: Vec<Device>, push_key: String) -> Shared {
        Arc::new(Mutex::new(Access {
            devices,
            pairing: None,
            image_key: random_hex(16),
            push_key,
        }))
    }

    /// A phone turned notifications on (with where to send them) or off.
    /// Only a device of the door it asked through.
    pub fn subscribe(&mut self, device: &str, door: Door, sub: Option<push::Subscription>) -> bool {
        match self.device_mut(device, door) {
            Some(d) => {
                d.push = sub;
                true
            }
            None => false,
        }
    }

    /// A phone chose what to be told about. Only a device of the door it
    /// asked through.
    pub fn set_notices(&mut self, device: &str, door: Door, notices: Notices) -> bool {
        match self.device_mut(device, door) {
            Some(d) => {
                d.notices = notices;
                true
            }
            None => false,
        }
    }

    /// A phone mutes or unmutes one session. Only a device of the door it
    /// asked through.
    pub fn set_muted(&mut self, device: &str, door: Door, session: &str, muted: bool) -> bool {
        match self.device_mut(device, door) {
            Some(d) => {
                d.muted.retain(|s| s != session);
                if muted {
                    d.muted.push(session.to_string());
                }
                true
            }
            None => false,
        }
    }

    /// A session is gone: no device keeps it muted. Whether any changed.
    pub fn forget_session(&mut self, session: &str) -> bool {
        let mut changed = false;
        for d in &mut self.devices {
            let before = d.muted.len();
            d.muted.retain(|s| s != session);
            changed |= d.muted.len() != before;
        }
        changed
    }

    /// A session took a new id (it was continued with another agent): a
    /// device that muted it keeps it muted. Whether any changed.
    pub fn rename_muted(&mut self, old: &str, new: &str) -> bool {
        let mut changed = false;
        for d in &mut self.devices {
            if let Some(i) = d.muted.iter().position(|s| s == old) {
                if d.muted.iter().any(|s| s == new) {
                    d.muted.remove(i);
                } else {
                    d.muted[i] = new.to_string();
                }
                changed = true;
            }
        }
        changed
    }

    /// The device with this id that came in through `door`. The relay names
    /// ids itself, so one could equal the id of a device paired on the
    /// network: a request through one door must never change the other's.
    fn device_mut(&mut self, id: &str, door: Door) -> Option<&mut Device> {
        self.devices
            .iter_mut()
            .find(|d| d.id == id && door.admits(d))
    }

    /// The push subscriptions that stopped working, dropped. Each comes as
    /// the device and the subscription the push went to: a phone that
    /// subscribed again while that push was on its way keeps its new one.
    pub fn drop_subscriptions(&mut self, gone: &[(String, push::Subscription)]) {
        for d in &mut self.devices {
            if gone
                .iter()
                .any(|(id, sub)| *id == d.id && d.push.as_ref() == Some(sub))
            {
                d.push = None;
            }
        }
    }

    /// The image a signed link points at, when the signature holds and it is
    /// an image file small enough to send.
    pub fn image_file(&self, path: &str, sig: &str) -> Option<PathBuf> {
        let path = percent_decode(path)?;
        if image_sig(&self.image_key, &path) != sig {
            return None;
        }
        let file = PathBuf::from(path);
        crate::agent::attach::mime_type(&file)?;
        let len = std::fs::metadata(&file).ok().filter(|m| m.is_file())?.len();
        (len <= crate::agent::attach::MAX_IMAGE_BYTES).then_some(file)
    }

    /// A fresh code; the previous one, if any, stops working.
    pub fn begin_pairing(&mut self) -> String {
        let code = new_code();
        self.pairing = Some(Pairing {
            code: code.clone(),
            until: Instant::now() + PAIR_FOR,
            failures: 0,
        });
        code
    }

    pub fn cancel_pairing(&mut self) {
        self.pairing = None;
    }

    /// The code phones may bring right now, and how long it lasts.
    pub fn pairing(&mut self) -> Option<(String, Duration)> {
        let p = self.pairing.as_ref()?;
        let left = p.until.saturating_duration_since(Instant::now());
        if left.is_zero() {
            self.pairing = None;
            return None;
        }
        Some((p.code.clone(), left))
    }

    /// A phone brought a code and its name: the device and its token when the code is
    /// right. The code is spent either way after enough wrong tries.
    pub fn pair(&mut self, code: &str, name: &str, now: u64) -> Option<(Device, String)> {
        let (current, _) = self.pairing()?;
        if !self.try_pairing(code, &current) {
            return None;
        }
        Some(self.admit(name, now))
    }

    /// Whether `brought` is `expected`; a wrong one counts, and enough of them
    /// spend the pairing. A right one spends it too: it works once.
    fn try_pairing(&mut self, brought: &str, expected: &str) -> bool {
        if hash(brought) != hash(expected) {
            if let Some(p) = self.pairing.as_mut() {
                p.failures += 1;
                if p.failures >= PAIR_TRIES {
                    self.pairing = None;
                }
            }
            return false;
        }
        self.pairing = None;
        true
    }

    /// A new device with a fresh token.
    fn admit(&mut self, name: &str, now: u64) -> (Device, String) {
        let token = random_hex(32);
        let device = Device {
            id: random_hex(8),
            name: device_name(name),
            token_hash: hash(&token),
            ticket_hash: String::new(),
            paired_at: now,
            last_seen: now,
            push: None,
            notices: Notices::default(),
            muted: Vec::new(),
        };
        self.devices.push(device.clone());
        (device, token)
    }

    /// The device a token belongs to, noting it was seen now. A device the
    /// relay brought in has no token: its empty hash is never a match.
    pub fn device_for(&mut self, token: &str, now: u64) -> Option<Device> {
        let h = hash(token);
        let d = self
            .devices
            .iter_mut()
            .find(|d| !d.token_hash.is_empty() && d.token_hash == h)?;
        d.last_seen = now;
        Some(d.clone())
    }

    /// The device with this id that came in through `door`, noting it was
    /// seen now. One paired on the network is never found through the
    /// relay's door, whatever id the relay names, nor the other way round.
    pub fn seen(&mut self, id: &str, door: Door, now: u64) -> Option<Device> {
        let d = self
            .devices
            .iter_mut()
            .find(|d| door.admits(d) && d.id == id)?;
        d.last_seen = now;
        Some(d.clone())
    }

    /// The device the relay brought in on a channel of this computer's
    /// account, by the id the relay gave it, noting it was seen now. Made the
    /// first time, with no token. Its name follows the one the relay gives,
    /// which is its signed-in device's. What changed comes back too, so the
    /// caller knows to save.
    pub fn relay_device(&mut self, id: &str, name: &str, now: u64) -> (Device, Seen) {
        let name = device_name(name);
        if let Some(d) = self
            .devices
            .iter_mut()
            .find(|d| d.token_hash.is_empty() && d.id == id)
        {
            d.last_seen = now;
            let seen = if d.name == name {
                Seen::Known
            } else {
                d.name = name;
                Seen::Renamed
            };
            return (d.clone(), seen);
        }
        let device = Device {
            id: id.to_string(),
            name,
            token_hash: String::new(),
            ticket_hash: String::new(),
            paired_at: now,
            last_seen: now,
            push: None,
            notices: Notices::default(),
            muted: Vec::new(),
        };
        self.devices.push(device.clone());
        (device, Seen::New)
    }

    /// Forgets a device by id, whichever door it came in through: for this
    /// computer's own user, who sees them all.
    pub fn unpair(&mut self, id: &str) -> bool {
        let before = self.devices.len();
        self.devices.retain(|d| d.id != id);
        self.devices.len() != before
    }

    /// A device let go of this computer through `door`: only that door's
    /// device with the id goes.
    pub fn unpair_through(&mut self, id: &str, door: Door) -> bool {
        let before = self.devices.len();
        self.devices.retain(|d| !(d.id == id && door.admits(d)));
        self.devices.len() != before
    }
}

/// The way a device came in: through the relay, by its account, or on the
/// network or Tailscale, with a token.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Door {
    Relay,
    Local,
}

impl Door {
    /// Whether `d` came in through this door: the relay's devices have no token.
    pub fn admits(self, d: &Device) -> bool {
        d.token_hash.is_empty() == (self == Door::Relay)
    }

    /// The door `d` came in through.
    pub fn of(d: &Device) -> Door {
        if Door::Relay.admits(d) {
            Door::Relay
        } else {
            Door::Local
        }
    }
}

/// The account as far as the computer knows it, for the devices the relay
/// brought in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AccountNow {
    /// Not known yet (the sign-in is still being read): nothing changes.
    Unknown,
    SignedOut,
    /// Signed in as this GitHub id.
    SignedIn(u64),
}

/// Whether every device the relay brought in must go, given the account
/// they were let in by (`kept`) and the account now; and the account to keep
/// after. They were let in by an account: under another one, or none, they
/// are nobody's, and their push subscriptions would notify a stranger.
pub fn relay_owner(kept: Option<u64>, now: AccountNow) -> (bool, Option<u64>) {
    match now {
        AccountNow::Unknown => (false, kept),
        AccountNow::SignedOut => (true, None),
        AccountNow::SignedIn(id) => (kept.is_some_and(|k| k != id), Some(id)),
    }
}

/// How often a computer asks its account which devices are still signed
/// in, while it holds a relay device that gets pushes: a device signed out
/// while it is not connected is dropped within this long.
pub const RELAY_PRUNE_EVERY: Duration = Duration::from_secs(15 * 60);

/// Whether it is time to ask the account for its signed-in devices and drop
/// the relay devices it no longer lists: only while signed in, only when a
/// relay device has a push subscription (nobody else is notified), and not
/// within `RELAY_PRUNE_EVERY` of the last time it was asked.
pub fn relay_prune_due(
    signed_in: bool,
    devices: &[Device],
    last: Option<Instant>,
    now: Instant,
) -> bool {
    signed_in
        && devices
            .iter()
            .any(|d| Door::Relay.admits(d) && d.push.is_some())
        && last.is_none_or(|at| now.saturating_duration_since(at) >= RELAY_PRUNE_EVERY)
}

/// The account's signed-in devices as it listed them, and when it was asked
/// (unix seconds).
#[derive(Clone, Copy)]
pub struct SignedInDevices<'a> {
    pub ids: &'a [String],
    pub asked_at: u64,
}

impl SignedInDevices<'_> {
    /// Whether the account still has `d`, as far as the list can say. A
    /// device that came in after the list was asked is not in it, and
    /// signed in all the same: the relay let it in.
    fn has(&self, d: &Device) -> bool {
        self.ids.contains(&d.id) || d.paired_at >= self.asked_at
    }
}

/// Drops the devices the relay brought in that their account no longer
/// has: with `listed`, each one the account's list does not have; with
/// `None`, every one. A device signed out elsewhere keeps no record here,
/// so its push subscription goes with it. Devices paired on the network
/// are never touched. The ids dropped come back.
pub fn prune_relay_devices(
    devices: &mut Vec<Device>,
    listed: Option<SignedInDevices<'_>>,
) -> Vec<String> {
    let mut dropped = Vec::new();
    devices.retain(|d| {
        let keep = !Door::Relay.admits(d) || listed.is_some_and(|l| l.has(d));
        if !keep {
            dropped.push(d.id.clone());
        }
        keep
    });
    dropped
}

/// What `Access::relay_device` found.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Seen {
    /// Not known before: made now.
    New,
    /// Known, under another name until now.
    Renamed,
    /// Known, as it is.
    Known,
}

/// A device's name as kept: trimmed, at most 60 letters, never empty.
fn device_name(name: &str) -> String {
    let name = name.trim().chars().take(60).collect::<String>();
    if name.is_empty() {
        "Phone".into()
    } else {
        name
    }
}

fn hash(s: &str) -> String {
    let digest = sha2::Sha256::digest(s.as_bytes());
    digest.iter().map(|b| format!("{b:02x}")).collect()
}

/// The link a phone loads an image from: its path, signed with the run's key.
pub fn image_link(key: &str, path: &Path) -> String {
    let path = path.to_string_lossy();
    format!(
        "/image?s={}&p={}",
        image_sig(key, &path),
        percent_encode(&path)
    )
}

// Hashing the path first keeps a known link from being extended into another.
fn image_sig(key: &str, path: &str) -> String {
    hash(&format!("{key}\n{}", hash(path)))[..32].to_string()
}

fn percent_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

fn percent_decode(s: &str) -> Option<String> {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            let hex = s.get(i + 1..i + 3)?;
            out.push(u8::from_str_radix(hex, 16).ok()?);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(out).ok()
}

/// Where `arakun server` keeps its settings; the app's own file when unset.
static SETTINGS_FILE: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();

/// Makes this process keep its phones in `path`, and its relay identity and
/// push keys under keyring names of that file's own (`key_account`). `arakun
/// server` calls it before anything loads the settings: with the app's file
/// and keys it would take the app's place at the relay.
pub fn use_settings_file(path: PathBuf) {
    let _ = SETTINGS_FILE.set(path);
}

fn settings_file() -> PathBuf {
    if let Some(path) = SETTINGS_FILE.get() {
        return path.clone();
    }
    // A test instance with its own state file keeps its own phone, on its own port.
    crate::paths::store(crate::paths::PHONE)
}

impl Settings {
    pub fn load() -> Self {
        Self::read().ok().flatten().unwrap_or_default()
    }

    /// What the file holds; `None` when there is none yet. A file that is
    /// there but cannot be read, or does not hold settings, is an error. The
    /// app takes the defaults then (`load`). `arakun server` must not: the
    /// defaults would put it on the plain network, and its next save would
    /// write them over the relay identity and every paired device.
    pub fn read() -> Result<Option<Settings>, String> {
        Self::read_from(&settings_file())
    }

    fn read_from(path: &Path) -> Result<Option<Settings>, String> {
        match crate::durable::read(path) {
            Ok(bytes) => serde_json::from_slice(&bytes).map(Some).map_err(|e| {
                format!(
                    "{} is not what Arakun wrote ({e}); move it aside to start over",
                    path.display()
                )
            }),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(format!("{}: {e}", path.display())),
        }
    }

    /// Whole or not at all, and for this user only: the file holds the paired
    /// devices (and an old file the relay identity, until the keyring takes
    /// it), and it is rewritten each time a device connects. A plain
    /// write cut short (a full disk) would leave it empty.
    pub fn save(&self) {
        let path = settings_file();
        if let Err(e) = self.save_to(&path) {
            eprintln!("arakun: could not save {}: {e}", path.display());
        }
    }

    /// `save` on the background writer, for the window's click handlers, where
    /// the disk flushes would stall it. A `load` right after still sees it.
    pub fn save_soon(&self) {
        let path = settings_file();
        match serde_json::to_vec_pretty(self) {
            Ok(json) => drop(crate::durable::write_private_soon(
                path,
                json,
                "could not save the phone settings",
            )),
            Err(e) => eprintln!("arakun: could not save {}: {e}", path.display()),
        }
    }

    fn save_to(&self, path: &Path) -> std::io::Result<()> {
        let json = serde_json::to_vec_pretty(self).map_err(std::io::Error::other)?;
        crate::durable::write_private(path, &json)
    }
}

/// `n` bytes from the OS, as hex digits.
fn random_hex(n: usize) -> String {
    let mut bytes = vec![0u8; n];
    getrandom::fill(&mut bytes).expect("the OS has random bytes");
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Six digits to read off the screen or type.
fn new_code() -> String {
    let mut bytes = [0u8; 4];
    getrandom::fill(&mut bytes).expect("the OS has random bytes");
    format!("{:06}", u32::from_le_bytes(bytes) % 1_000_000)
}

type Outgoing = std_mpsc::SyncSender<String>;

/// A connected phone: the device it is, and the way to write to it.
struct Client {
    device: String,
    out: Outgoing,
    /// Set when the client is dropped from the map. Its loop would otherwise
    /// write out every message still queued before it noticed, which for a
    /// phone dropped for a full queue can take as long as it is slow.
    gone: Arc<AtomicBool>,
}

impl Drop for Client {
    fn drop(&mut self) {
        self.gone.store(true, Ordering::Relaxed);
    }
}

type Clients = Arc<Mutex<HashMap<ClientId, Client>>>;

/// The listening server. Dropping it stops listening and closes every phone.
pub struct Server {
    port: u16,
    /// It listens on the loopback only.
    loopback: bool,
    clients: Clients,
    stop: Arc<AtomicBool>,
}

impl Server {
    /// Listens at `bind:port`; messages from phones come out of the receiver.
    /// `identity` signs each phone's handshake, so the phone knows it reached
    /// this computer and not someone else on the network.
    pub fn start(
        bind: Ipv4Addr,
        port: u16,
        access: Shared,
        identity: Arc<identity::Identity>,
    ) -> std::io::Result<(Server, mpsc::UnboundedReceiver<Incoming>)> {
        let listener = TcpListener::bind(SocketAddr::from((bind, port)))?;
        let port = listener.local_addr()?.port();
        let (tx, rx) = mpsc::unbounded();
        let clients: Clients = Arc::default();
        let stop = Arc::new(AtomicBool::new(false));
        let (c, s) = (clients.clone(), stop.clone());
        std::thread::Builder::new()
            .name("arakun-phone".into())
            .spawn(move || {
                let mut ids = 1..;
                let stopped = s.clone();
                crate::accept::serve(
                    listener.incoming(),
                    CONNECTIONS,
                    "arakun-phone-conn",
                    || stopped.load(Ordering::Relaxed),
                    |stream| {
                        let id = ids.next().unwrap_or_default();
                        let (tx, c, s, access, identity) = (
                            tx.clone(),
                            c.clone(),
                            s.clone(),
                            access.clone(),
                            identity.clone(),
                        );
                        move || answer(stream, id, tx, c, s, &access, &identity)
                    },
                );
            })?;
        Ok((
            Server {
                port,
                loopback: bind.is_loopback(),
                clients,
                stop,
            },
            rx,
        ))
    }

    pub fn port(&self) -> u16 {
        self.port
    }

    pub fn loopback(&self) -> bool {
        self.loopback
    }

    /// The device behind a connected phone.
    pub fn device_of(&self, id: ClientId) -> Option<String> {
        self.clients
            .lock()
            .unwrap()
            .get(&id)
            .map(|c| c.device.clone())
    }

    pub fn send(&self, id: ClientId, text: String) {
        let mut clients = self.clients.lock().unwrap();
        if clients.get(&id).is_some_and(|c| !queue(&c.out, text)) {
            clients.remove(&id);
        }
    }

    pub fn broadcast(&self, text: &str) {
        self.clients
            .lock()
            .unwrap()
            .retain(|_, c| queue(&c.out, text.to_string()));
    }

    pub fn client_count(&self) -> usize {
        self.clients.lock().unwrap().len()
    }

    /// The devices with a socket open right now.
    pub fn connected_devices(&self) -> Vec<String> {
        let mut ids: Vec<String> = self
            .clients
            .lock()
            .unwrap()
            .values()
            .map(|c| c.device.clone())
            .collect();
        ids.sort();
        ids.dedup();
        ids
    }

    /// Closes every socket of a device, as unpairing it must.
    pub fn disconnect_device(&self, device: &str) {
        // Dropping a client ends its loop, which closes the socket.
        self.clients
            .lock()
            .unwrap()
            .retain(|_, c| c.device != device);
    }
}

/// Queues a message for a phone; false when its queue is full. Dropping its
/// client then ends its loop and closes the socket, and the shell hears it left.
fn queue(out: &Outgoing, text: String) -> bool {
    !matches!(out.try_send(text), Err(std_mpsc::TrySendError::Full(_)))
}

impl Drop for Server {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        // Dropping the clients ends every client loop; a connection wakes the acceptor.
        self.clients.lock().unwrap().clear();
        let _ = TcpStream::connect_timeout(
            &SocketAddr::from((Ipv4Addr::LOCALHOST, self.port)),
            Duration::from_millis(200),
        );
    }
}

/// Serves one connection: the page over HTTP, or a phone over its sealed
/// WebSocket. Nothing but the page goes in the clear.
fn answer(
    mut stream: TcpStream,
    id: ClientId,
    tx: mpsc::UnboundedSender<Incoming>,
    clients: Clients,
    stop: Arc<AtomicBool>,
    access: &Shared,
    identity: &identity::Identity,
) {
    let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
    let _ = stream.set_nodelay(true);
    // Peeking leaves the request for tungstenite to read when it is a WebSocket.
    let Some(head) = peek_head(&stream) else {
        return;
    };
    // Closing with the request still unread would reset the connection and lose the answer.
    let consume = |stream: &mut TcpStream| {
        let mut sink = vec![0u8; head.len()];
        let _ = stream.read_exact(&mut sink);
    };
    let Some(req) = Head::parse(&head) else {
        consume(&mut stream);
        let _ = respond(&mut stream, 400, "text/plain", b"bad request");
        return;
    };
    if req.upgrade {
        if req.path != lan::PATH || !same_origin(&req) {
            consume(&mut stream);
            let _ = respond(&mut stream, 403, "text/plain", b"forbidden");
            return;
        }
        let ended = || stop.load(Ordering::Relaxed);
        lan::serve(stream, id, identity, access, &clients, &tx, ended);
        return;
    }
    consume(&mut stream);
    // Read before answering: a body left unread resets the connection.
    if req.content_length > 0 {
        read_body(
            &mut stream,
            req.content_length,
            Instant::now() + BODY_DEADLINE,
        );
    }
    let reply = match req.method.as_str() {
        "GET" => page_file(&req.path),
        _ => Reply::not_found(),
    };
    let _ = respond(&mut stream, reply.status, reply.content_type, &reply.body);
}

/// A file of the page, by its path.
fn page_file(path: &str) -> Reply {
    let file = match path.trim_start_matches('/') {
        "" => "index.html",
        p => p,
    };
    match Page::get(file) {
        Some(f) => Reply {
            status: 200,
            content_type: content_type(file),
            body: f.data.into_owned(),
        },
        None => Reply::not_found(),
    }
}

/// What a request gets back.
pub struct Reply {
    pub status: u16,
    pub content_type: &'static str,
    pub body: Vec<u8>,
}

impl Reply {
    fn json(status: u16, body: &serde_json::Value) -> Reply {
        Reply {
            status,
            content_type: "application/json",
            body: body.to_string().into_bytes(),
        }
    }

    fn error(status: u16, message: &str) -> Reply {
        Reply::json(status, &serde_json::json!({ "error": message }))
    }

    fn not_found() -> Reply {
        Reply {
            status: 404,
            content_type: "text/plain",
            body: b"not found".to_vec(),
        }
    }
}

/// What a request changed, which whoever carried it must act on: tell the
/// shell, and for an unpairing, drop the device's connections.
pub enum Effect {
    None,
    Paired,
    Subscribed,
    Unpaired(String),
}

/// Answers one request about the phone's account with this computer: who it
/// is, notifications, unpairing, the images prompts mention, and the page
/// itself. It only ever comes inside a sealed channel, from the device that
/// channel belongs to: through the relay, or on this computer's own server
/// (`lan`). Pairing is not one of them: on the network it is the channel's
/// first message, and the relay brings in devices by account.
pub fn route(req: &Head, body: Option<Vec<u8>>, access: &Shared, now: u64) -> (Reply, Effect) {
    match (req.method.as_str(), req.path.as_str()) {
        ("GET", "/me") => {
            let found = {
                let mut a = access.lock().unwrap();
                req.device(&mut a, now).map(|d| (d, a.push_key.clone()))
            };
            match found {
                Some((d, push_key)) => {
                    let body = serde_json::json!({
                        "id": d.id, "name": d.name, "host": host_name(), "paired_at": d.paired_at,
                        "push_key": push_key, "notifying": d.push.is_some(), "notices": d.notices,
                        "muted": d.muted,
                    });
                    (Reply::json(200, &body), Effect::None)
                }
                None => (Reply::error(403, "not paired"), Effect::None),
            }
        }
        // The phone's push subscription (null to stop being notified) and/or
        // which notices it wants; what the body leaves out stays as it was.
        ("POST", "/push") => {
            let device = req.device(&mut access.lock().unwrap(), now);
            let Some(device) = device else {
                return (Reply::error(403, "not paired"), Effect::None);
            };
            let Some(ask) = body.and_then(|b| serde_json::from_slice::<serde_json::Value>(&b).ok())
            else {
                return (Reply::error(400, "bad request"), Effect::None);
            };
            let sub = match ask.get("subscription") {
                None => None,
                Some(serde_json::Value::Null) => Some(None),
                Some(v) => match push::Subscription::from_browser(v) {
                    Some(s) => Some(Some(s)),
                    None => return (Reply::error(400, "bad subscription"), Effect::None),
                },
            };
            let notices = match ask.get("notices") {
                None => None,
                Some(v) => match serde_json::from_value::<NoticesChange>(v.clone()) {
                    Ok(n) => Some(n),
                    Err(_) => return (Reply::error(400, "bad notices"), Effect::None),
                },
            };
            let door = req.door();
            let (notifying, notices) = {
                let mut a = access.lock().unwrap();
                if let Some(sub) = sub {
                    a.subscribe(&device.id, door, sub);
                }
                if let Some(change) = notices {
                    let current = a
                        .devices
                        .iter()
                        .find(|d| d.id == device.id && door.admits(d))
                        .map(|d| d.notices);
                    if let Some(n) = current {
                        a.set_notices(&device.id, door, change.apply(n));
                    }
                }
                let d = a
                    .devices
                    .iter()
                    .find(|d| d.id == device.id && door.admits(d));
                (
                    d.is_some_and(|d| d.push.is_some()),
                    d.map(|d| d.notices).unwrap_or_default(),
                )
            };
            let body = serde_json::json!({ "notifying": notifying, "notices": notices });
            (Reply::json(200, &body), Effect::Subscribed)
        }
        // The phone mutes or unmutes one session's notices.
        ("POST", "/mute") => {
            let device = req.device(&mut access.lock().unwrap(), now);
            let Some(device) = device else {
                return (Reply::error(403, "not paired"), Effect::None);
            };
            #[derive(Deserialize)]
            struct Mute {
                session: String,
                muted: bool,
            }
            let Some(ask) = body.and_then(|b| serde_json::from_slice::<Mute>(&b).ok()) else {
                return (Reply::error(400, "bad request"), Effect::None);
            };
            if ask.session.is_empty() {
                return (Reply::error(400, "bad request"), Effect::None);
            }
            let door = req.door();
            let muted = {
                let mut a = access.lock().unwrap();
                a.set_muted(&device.id, door, &ask.session, ask.muted);
                a.devices
                    .iter()
                    .find(|d| d.id == device.id && door.admits(d))
                    .map(|d| d.muted.clone())
                    .unwrap_or_default()
            };
            (
                Reply::json(200, &serde_json::json!({ "muted": muted })),
                Effect::Subscribed,
            )
        }
        // The phone lets go of this computer: its token dies and its sockets close.
        ("POST", "/unpair") => {
            let device = req.device(&mut access.lock().unwrap(), now);
            let Some(device) = device else {
                return (Reply::error(403, "not paired"), Effect::None);
            };
            access
                .lock()
                .unwrap()
                .unpair_through(&device.id, req.door());
            (
                Reply::json(200, &serde_json::json!({})),
                Effect::Unpaired(device.id),
            )
        }
        ("GET", "/image") => {
            let file = match (req.query("p"), req.query("s")) {
                (Some(p), Some(s)) => access.lock().unwrap().image_file(p, s),
                _ => None,
            };
            let image = file.and_then(|f| {
                Some((
                    crate::agent::attach::mime_type(&f)?,
                    std::fs::read(&f).ok()?,
                ))
            });
            match image {
                Some((mime, bytes)) => (
                    Reply {
                        status: 200,
                        content_type: mime,
                        body: bytes,
                    },
                    Effect::None,
                ),
                None => (Reply::not_found(), Effect::None),
            }
        }
        ("GET", path) => (page_file(path), Effect::None),
        _ => (Reply::not_found(), Effect::None),
    }
}

/// A `{req: {method, path, body}}` from inside the sealed channel of
/// `device`, which came in through `door`, answered as the HTTP it stands for.
pub fn sealed_request(
    req: &serde_json::Value,
    device: &str,
    door: Door,
    access: &Shared,
    now: u64,
) -> (Reply, Effect) {
    let method = req["method"].as_str().unwrap_or("GET");
    let path = req["path"].as_str().unwrap_or("/");
    let body = req["body"]
        .as_str()
        .map(|b| b.as_bytes().to_vec())
        .filter(|b| b.len() <= MAX_BODY);
    match Head::sealed(method, path, device, door) {
        Some(head) => route(&head, body, access, now),
        None => (Reply::error(400, "bad request"), Effect::None),
    }
}

/// The body that follows the head, when it is small enough and comes in time.
/// The deadline is for the whole body: a timeout per read would let a sender
/// keep the thread for hours with a byte every few seconds.
fn read_body(stream: &mut TcpStream, len: usize, deadline: Instant) -> Option<Vec<u8>> {
    if len > MAX_BODY {
        return None;
    }
    let mut body = vec![0u8; len];
    let mut got = 0;
    while got < len {
        let left = deadline.checked_duration_since(Instant::now())?;
        // A zero timeout means none to the socket.
        stream
            .set_read_timeout(Some(left.max(Duration::from_millis(1))))
            .ok()?;
        match stream.read(&mut body[got..]) {
            Ok(0) => return None,
            Ok(n) => got += n,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(_) => return None,
        }
    }
    Some(body)
}

/// The request head, without consuming it; `None` when it does not arrive whole.
fn peek_head(stream: &TcpStream) -> Option<String> {
    let mut buf = vec![0u8; 8192];
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    loop {
        let n = stream.peek(&mut buf).ok()?;
        if n == 0 {
            return None;
        }
        if let Some(end) = buf[..n].windows(4).position(|w| w == b"\r\n\r\n") {
            return String::from_utf8(buf[..end + 4].to_vec()).ok();
        }
        if n == buf.len() || std::time::Instant::now() > deadline {
            return None;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// What `route` needs of a request.
pub struct Head {
    method: String,
    path: String,
    query: String,
    upgrade: bool,
    host: Option<String>,
    origin: Option<String>,
    content_length: usize,
    /// The device that asked, and the door its sealed channel came through.
    /// A request in the clear has none: it gets only the page.
    device: Option<(String, Door)>,
}

impl Head {
    /// A request that came through an encrypted channel, from the device the
    /// relay brought in with this id: `target` is the path with its query.
    pub fn relayed(method: &str, target: &str, device: &str) -> Option<Head> {
        Head::sealed(method, target, device, Door::Relay)
    }

    /// A request that came inside the sealed channel of the device with this
    /// id, which came in through `door`.
    pub fn sealed(method: &str, target: &str, device: &str, door: Door) -> Option<Head> {
        if method != "GET" && method != "POST" {
            return None;
        }
        let (path, query) = target.split_once('?').unwrap_or((target, ""));
        Some(Head {
            method: method.to_string(),
            path: path.to_string(),
            query: query.to_string(),
            upgrade: false,
            host: None,
            origin: None,
            content_length: 0,
            device: Some((device.to_string(), door)),
        })
    }

    fn parse(head: &str) -> Option<Head> {
        let mut lines = head.lines();
        let mut first = lines.next()?.split_whitespace();
        let method = first.next()?;
        if method != "GET" && method != "POST" {
            return None;
        }
        let target = first.next()?;
        let (path, query) = target.split_once('?').unwrap_or((target, ""));
        let mut h = Head {
            method: method.to_string(),
            path: path.to_string(),
            query: query.to_string(),
            upgrade: false,
            host: None,
            origin: None,
            content_length: 0,
            device: None,
        };
        for line in lines {
            let Some((name, value)) = line.split_once(':') else {
                continue;
            };
            let value = value.trim();
            match name.trim().to_ascii_lowercase().as_str() {
                "upgrade" if value.eq_ignore_ascii_case("websocket") => h.upgrade = true,
                "host" => h.host = Some(value.to_string()),
                "origin" => h.origin = Some(value.to_string()),
                "content-length" => h.content_length = value.parse().ok()?,
                _ => {}
            }
        }
        Some(h)
    }

    fn query(&self, name: &str) -> Option<&str> {
        self.query
            .split('&')
            .filter_map(|p| p.split_once('='))
            .find(|(k, _)| *k == name)
            .map(|(_, v)| v)
    }

    /// The door this request came through.
    fn door(&self) -> Door {
        self.device.as_ref().map_or(Door::Local, |(_, door)| *door)
    }

    /// The device asking, noting it was seen now: the one whose sealed
    /// channel the request came in, of that channel's door.
    fn device(&self, access: &mut Access, now: u64) -> Option<Device> {
        let (id, door) = self.device.as_ref()?;
        access.seen(id, *door, now)
    }
}

/// A page from elsewhere may not use the phone's socket from a browser.
fn same_origin(req: &Head) -> bool {
    match (&req.origin, &req.host) {
        (Some(origin), Some(host)) => {
            let origin_host = origin
                .split("://")
                .nth(1)
                .unwrap_or(origin)
                .trim_end_matches('/');
            origin_host.eq_ignore_ascii_case(host)
        }
        (Some(_), None) => false,
        (None, _) => true,
    }
}

fn content_type(file: &str) -> &'static str {
    match file.rsplit('.').next() {
        Some("html") => "text/html; charset=utf-8",
        Some("js") => "text/javascript; charset=utf-8",
        Some("css") => "text/css; charset=utf-8",
        Some("json") | Some("webmanifest") => "application/json",
        Some("svg") => "image/svg+xml",
        Some("png") => "image/png",
        Some("webp") => "image/webp",
        Some("woff2") => "font/woff2",
        _ => "application/octet-stream",
    }
}

/// The phone page's policy, as the account host sends it (`pagePolicy` in
/// `cloud/src/teams-router.ts`). The page shows what agents print, so script
/// that slips into it through a transcript must not run.
/// - Scripts: this computer only, so no inline handlers or `javascript:` links.
/// - Sockets: `ws:` and `wss:` by name too, as some browsers do not count them
///   under 'self'. The page talks only to the host that served it.
/// - Images: `data:`, `blob:` and `https:` too, for pictures a session shows.
/// - Styles: inline too, for the few `style` attributes the page writes.
const PAGE_POLICY: &str = "default-src 'self'; script-src 'self'; connect-src 'self' ws: wss:; \
    img-src 'self' data: blob: https:; style-src 'self' 'unsafe-inline'; \
    frame-ancestors 'none'; base-uri 'none'; form-action 'self'";

fn respond(
    stream: &mut TcpStream,
    status: u16,
    content_type: &str,
    body: &[u8],
) -> std::io::Result<()> {
    let reason = match status {
        200 => "OK",
        400 => "Bad Request",
        403 => "Forbidden",
        404 => "Not Found",
        _ => "Error",
    };
    write!(
        stream,
        "HTTP/1.1 {status} {reason}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nCache-Control: no-cache\r\nContent-Security-Policy: {PAGE_POLICY}\r\nX-Content-Type-Options: nosniff\r\nReferrer-Policy: no-referrer\r\nConnection: close\r\n\r\n",
        body.len()
    )?;
    stream.write_all(body)?;
    stream.flush()
}

/// The address of this machine on its network, as the phone would reach it.
/// A UDP "connect" sends nothing; it only picks the interface with the route out.
pub fn lan_ip() -> Option<Ipv4Addr> {
    if let Ok(sock) = UdpSocket::bind("0.0.0.0:0")
        && sock.connect("8.8.8.8:80").is_ok()
        && let Ok(SocketAddr::V4(a)) = sock.local_addr()
        && !a.ip().is_loopback()
        && !a.ip().is_unspecified()
    {
        return Some(*a.ip());
    }
    let host = std::env::var("COMPUTERNAME")
        .or_else(|_| std::env::var("HOSTNAME"))
        .ok()?;
    format!("{host}:0")
        .to_socket_addrs()
        .ok()?
        .find_map(|a| match a {
            SocketAddr::V4(a) if !a.ip().is_loopback() => Some(*a.ip()),
            _ => None,
        })
}

/// Where phones reach the page over the local network.
pub fn lan_address(ip: Ipv4Addr, port: u16) -> String {
    format!("http://{ip}:{port}/")
}

/// The link the QR code carries while pairing: the page, with the code to
/// bring and, on this computer's own server, the label of the key that must
/// sign the handshake (`lan`). Through the relay the account names the key.
pub fn link(address: &str, code: &str, label: Option<&str>) -> String {
    match label {
        Some(label) => format!("{address}?p={code}&k={label}"),
        None => format!("{address}?p={code}"),
    }
}

/// The longest machine name the account takes; it answers 400 to more.
const HOST_NAME_MAX: usize = 100;

/// The machine's name, as the phone's inbox and the account's list of
/// machines show it. Read once: the system is asked on some platforms.
pub fn host_name() -> String {
    static NAME: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    NAME.get_or_init(|| {
        let found = [
            std::env::var("COMPUTERNAME").ok(),
            std::env::var("HOSTNAME").ok(),
            std::fs::read_to_string("/etc/hostname").ok(),
            // A Mac has none of these in an app started from the Finder.
            system_host_name(),
        ];
        found
            .iter()
            .flatten()
            .find_map(|name| clean_host_name(name))
            .unwrap_or_else(|| "this computer".into())
    })
    .clone()
}

/// `name` as it can be sent and shown: no control characters, trimmed, at
/// most `HOST_NAME_MAX` letters. `None` when nothing is left.
fn clean_host_name(name: &str) -> Option<String> {
    let name: String = name.chars().filter(|c| !c.is_control()).collect();
    let name: String = name.trim().chars().take(HOST_NAME_MAX).collect();
    let name = name.trim_end();
    // What a Mac off any network calls itself says nothing.
    (!name.is_empty() && name != "localhost").then(|| name.to_string())
}

/// The name the system itself gives, without the domain.
#[cfg(unix)]
fn system_host_name() -> Option<String> {
    let mut buf = [0u8; 256];
    // SAFETY: the buffer is valid for its whole length, which is what we pass.
    let r = unsafe { libc::gethostname(buf.as_mut_ptr().cast(), buf.len()) };
    if r != 0 {
        return None;
    }
    let end = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
    let name = String::from_utf8_lossy(&buf[..end]).into_owned();
    // "anns-mac.local" on a Mac: the first part is the name.
    Some(name.split('.').next().unwrap_or_default().to_string())
}

#[cfg(not(unix))]
fn system_host_name() -> Option<String> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};

    #[test]
    fn a_body_has_one_deadline_not_one_per_read() {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let addr = listener.local_addr().unwrap();
        // A byte every 40 ms keeps each read short, but not the whole body.
        let sender = std::thread::spawn(move || {
            let mut out = TcpStream::connect(addr).unwrap();
            for _ in 0..10 {
                if out.write_all(b"x").is_err() {
                    return;
                }
                std::thread::sleep(Duration::from_millis(40));
            }
        });
        let (mut slow, _) = listener.accept().unwrap();
        let start = Instant::now();
        let deadline = start + Duration::from_millis(150);
        assert_eq!(read_body(&mut slow, 10, deadline), None);
        assert!(start.elapsed() < Duration::from_millis(300));
        sender.join().unwrap();

        let mut out = TcpStream::connect(addr).unwrap();
        out.write_all(b"{}").unwrap();
        let (mut quick, _) = listener.accept().unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        assert_eq!(read_body(&mut quick, 2, deadline), Some(b"{}".to_vec()));
    }

    #[test]
    fn a_secret_answer_without_values_declines() {
        use crate::agent::secret::Typed;
        let read = |json: &str| serde_json::from_str::<ClientMsg>(json).unwrap();
        assert_eq!(
            read(r#"{"type":"secret","session":"s1","id":3}"#),
            ClientMsg::Secret {
                session: "s1".into(),
                id: 3,
                values: None,
            }
        );
        assert_eq!(
            read(r#"{"type":"secret","session":"s1","id":3,"values":["a","b"]}"#),
            ClientMsg::Secret {
                session: "s1".into(),
                id: 3,
                values: Some(Typed(vec!["a".into(), "b".into()])),
            }
        );
    }

    #[test]
    fn only_a_loopback_server_or_the_relay_is_private() {
        let access = Access::new(Vec::new(), "pk".into());
        let identity = Arc::new(identity::Identity::generate());
        let (tailnet, _rx) = Server::start(Reach::Tailscale.bind(), 0, access, identity).unwrap();
        assert!(Transport::Local(tailnet).private());
        // Not bound for real: a test should not listen on every interface.
        let lan = Server {
            port: 0,
            loopback: Reach::Lan.bind().is_loopback(),
            clients: Arc::default(),
            stop: Arc::default(),
        };
        assert!(!Transport::Local(lan).private());
    }

    #[test]
    fn a_delete_keeps_the_branch_unless_asked() {
        let read = |json: &str| serde_json::from_str::<ClientMsg>(json).unwrap();
        assert_eq!(
            read(r#"{"type":"delete_env","place":"/w/fix"}"#),
            ClientMsg::DeleteEnv {
                place: "/w/fix".into(),
                delete_branch: false,
                branch: None,
            }
        );
        assert_eq!(
            read(r#"{"type":"delete_env","place":"/w/fix","delete_branch":true,"branch":"fix"}"#),
            ClientMsg::DeleteEnv {
                place: "/w/fix".into(),
                delete_branch: true,
                branch: Some("fix".into()),
            }
        );
        assert_eq!(
            read(r#"{"type":"settle_env","place":"/w/fix"}"#),
            ClientMsg::SettleEnv {
                place: "/w/fix".into()
            }
        );
    }

    #[test]
    fn git_reads_as_the_app_sends_it() {
        use crate::git_call::Call;
        let call = Call::Push {
            remote: "origin".into(),
            branch: "fix".into(),
        };
        // As `servers::git::ServerGit` writes it.
        let sent = serde_json::json!({ "type": "git", "ref": 7, "place": "/w/fix", "call": call });
        assert_eq!(
            serde_json::from_value::<ClientMsg>(sent).unwrap(),
            ClientMsg::Git {
                reference: 7,
                place: "/w/fix".into(),
                call,
            }
        );
    }

    #[test]
    fn a_rewind_asks_for_its_plan_unless_confirmed() {
        let read = |json: &str| serde_json::from_str::<ClientMsg>(json).unwrap();
        assert_eq!(
            read(r#"{"type":"rewind","session":"s1","entry":4}"#),
            ClientMsg::Rewind {
                session: "s1".into(),
                entry: 4,
                confirm: false,
            }
        );
        assert_eq!(
            read(r#"{"type":"rewind","session":"s1","entry":4,"confirm":true}"#),
            ClientMsg::Rewind {
                session: "s1".into(),
                entry: 4,
                confirm: true,
            }
        );
        assert_eq!(
            read(r#"{"type":"stage","session":"s1"}"#),
            ClientMsg::Stage {
                session: "s1".into()
            }
        );
        assert_eq!(
            read(r#"{"type":"continue_with","session":"s1","agent":"codex"}"#),
            ClientMsg::ContinueWith {
                session: "s1".into(),
                agent: "codex".into(),
                settings: Vec::new(),
            }
        );
    }

    #[test]
    fn settings_that_cannot_be_read_are_an_error_not_the_defaults() {
        let dir = std::env::temp_dir().join(format!("arakun-phone-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let file = dir.join("phone.json");
        // Nothing there yet: a first run.
        assert_eq!(Settings::read_from(&file).map(|s| s.is_some()), Ok(false));
        let settings = Settings {
            enabled: true,
            reach: Reach::Relay,
            ..Settings::default()
        };
        settings.save_to(&file).unwrap();
        let back = Settings::read_from(&file).unwrap().unwrap();
        assert_eq!((back.enabled, back.reach), (true, Reach::Relay));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            // It holds the relay identity: this user only.
            let mode = std::fs::metadata(&file).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }
        // Cut short, as a write on a full disk leaves it: an error that names
        // the file, and never the defaults (which are the plain network).
        std::fs::write(&file, b"{ \"enabled\": tr").unwrap();
        let e = Settings::read_from(&file).unwrap_err();
        assert!(
            e.contains("phone.json") && e.contains("move it aside"),
            "{e}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A keyring in memory. `locked` makes every call fail, as a keyring
    /// that stayed locked does; `refuses` makes only storing fail.
    #[derive(Default)]
    struct FakeVault {
        entries: std::cell::RefCell<HashMap<String, String>>,
        locked: bool,
        refuses: bool,
        moves: std::cell::RefCell<std::collections::HashSet<String>>,
    }

    impl Vault for FakeVault {
        fn get(&self, account: &str) -> Result<Option<String>, String> {
            if self.locked {
                return Err("locked".into());
            }
            Ok(self.entries.borrow().get(account).cloned())
        }

        fn set(&self, account: &str, _label: &str, secret: &str) -> Result<(), String> {
            if self.locked || self.refuses {
                return Err("locked".into());
            }
            self.entries
                .borrow_mut()
                .insert(account.to_string(), secret.to_string());
            Ok(())
        }

        fn try_move(&self, account: &str) -> bool {
            self.moves.borrow_mut().insert(account.to_string())
        }
    }

    /// A phone.json from before the keyring, with both private keys in it.
    fn old_file(id: &identity::Identity, vapid: &push::Vapid) -> Settings {
        let json = serde_json::json!({
            "enabled": true,
            "port": 8765,
            "reach": "relay",
            "identity": id.pkcs8_b64(),
            "vapid": vapid.pkcs8_b64(),
        });
        serde_json::from_value(json).unwrap()
    }

    #[test]
    fn keys_are_made_once_in_the_keyring_and_never_written_to_the_file() {
        let ring = FakeVault::default();
        let mut settings = Settings::default();
        let (id, changed) = settings.identity_in(&ring, "id").unwrap();
        assert!(changed, "the public half is new");
        let (vapid, _) = settings.vapid_in(&ring, "vapid").unwrap();
        assert_eq!(ring.entries.borrow()["id"], id.pkcs8_b64());
        assert_eq!(ring.entries.borrow()["vapid"], vapid.pkcs8_b64());
        let (again, changed) = settings.identity_in(&ring, "id").unwrap();
        assert_eq!(again.label(), id.label());
        assert!(!changed, "keys that are kept are not saved again");
        let (v2, changed) = settings.vapid_in(&ring, "vapid").unwrap();
        assert_eq!(v2.public_key(), vapid.public_key());
        assert!(!changed);
        assert_eq!(settings.relay_label(), Some(id.label()));
        let json = serde_json::to_string(&settings).unwrap();
        assert!(!json.contains(&id.pkcs8_b64()), "{json}");
        assert!(!json.contains(&vapid.pkcs8_b64()), "{json}");
        assert!(!json.contains("\"identity\"") && !json.contains("\"vapid\""));
        let back: Settings = serde_json::from_str(&json).unwrap();
        assert_eq!(
            back.relay_label(),
            Some(id.label()),
            "the label needs no keyring"
        );
    }

    #[test]
    fn an_old_files_keys_move_to_the_keyring_and_leave_the_file() {
        let (id, vapid) = (identity::Identity::generate(), push::Vapid::generate());
        let mut settings = old_file(&id, &vapid);
        assert_eq!(settings.relay_label(), Some(id.label()));
        let ring = FakeVault::default();
        let (got, changed) = settings.identity_in(&ring, "id").unwrap();
        assert!(changed);
        assert_eq!(got.label(), id.label(), "the phones' key is kept");
        let (v, changed) = settings.vapid_in(&ring, "vapid").unwrap();
        assert!(changed);
        assert_eq!(v.public_key(), vapid.public_key());
        assert_eq!(ring.entries.borrow()["id"], id.pkcs8_b64());
        assert_eq!(ring.entries.borrow()["vapid"], vapid.pkcs8_b64());
        assert_eq!(settings.identity, None);
        assert_eq!(settings.vapid, None);
        let json = serde_json::to_string(&settings).unwrap();
        assert!(!json.contains(&id.pkcs8_b64()) && !json.contains(&vapid.pkcs8_b64()));
        assert_eq!(settings.relay_label(), Some(id.label()));
        // Read back, the keys come from the keyring.
        let mut back: Settings = serde_json::from_str(&json).unwrap();
        let (got, changed) = back.identity_in(&ring, "id").unwrap();
        assert_eq!(got.label(), id.label());
        assert!(!changed);
    }

    #[test]
    fn a_keyring_that_does_not_take_the_old_key_leaves_it_in_the_file() {
        let (id, vapid) = (identity::Identity::generate(), push::Vapid::generate());
        let mut settings = old_file(&id, &vapid);
        let ring = FakeVault {
            refuses: true,
            ..FakeVault::default()
        };
        let (got, _) = settings.identity_in(&ring, "id").unwrap();
        assert_eq!(got.label(), id.label());
        let (v, changed) = settings.vapid_in(&ring, "vapid").unwrap();
        assert_eq!(v.public_key(), vapid.public_key());
        assert!(!changed);
        assert_eq!(
            settings.identity.as_ref().map(Secret::expose),
            Some(id.pkcs8_b64().as_str())
        );
        let json = serde_json::to_string(&settings).unwrap();
        assert!(json.contains(&id.pkcs8_b64()), "still saved, so not lost");
        // Asked once a run: the next call uses the file's copy without asking.
        let (again, _) = settings.identity_in(&ring, "id").unwrap();
        assert_eq!(again.label(), id.label());
    }

    #[test]
    fn a_locked_keyring_makes_no_new_key() {
        let ring = FakeVault {
            locked: true,
            ..FakeVault::default()
        };
        let mut settings = Settings::default();
        assert!(settings.identity_in(&ring, "id").is_err());
        assert!(settings.vapid_in(&ring, "vapid").is_err());
        assert_eq!(settings.identity, None);
        assert_eq!(settings.identity_public, None);
        assert_eq!(settings.vapid, None);
        assert!(ring.entries.borrow().is_empty());
        // Unlocked, the key it had is the one used.
        let ring = FakeVault::default();
        let kept = identity::Identity::generate();
        ring.entries
            .borrow_mut()
            .insert("id".into(), kept.pkcs8_b64());
        assert_eq!(
            settings.identity_in(&ring, "id").unwrap().0.label(),
            kept.label()
        );
    }

    #[test]
    fn a_locked_keyring_still_runs_on_an_old_files_key() {
        let (id, vapid) = (identity::Identity::generate(), push::Vapid::generate());
        let mut settings = old_file(&id, &vapid);
        let ring = FakeVault {
            locked: true,
            ..FakeVault::default()
        };
        assert_eq!(
            settings.identity_in(&ring, "id").unwrap().0.label(),
            id.label()
        );
        assert!(settings.identity.is_some());
    }

    #[test]
    fn a_new_key_that_the_keyring_does_not_take_is_not_used() {
        let ring = FakeVault {
            refuses: true,
            ..FakeVault::default()
        };
        let mut settings = Settings::default();
        assert!(settings.identity_in(&ring, "id").is_err());
        assert_eq!(settings.identity, None);
        assert_eq!(settings.identity_public, None);
    }

    #[test]
    fn a_damaged_key_in_the_keyring_is_an_error_not_a_new_key() {
        let ring = FakeVault::default();
        ring.entries.borrow_mut().insert("id".into(), "junk".into());
        let mut settings = Settings::default();
        assert!(settings.identity_in(&ring, "id").is_err());
        assert_eq!(ring.entries.borrow()["id"], "junk");
    }

    #[test]
    fn renewing_replaces_the_key_and_forgets_the_phones_or_changes_nothing() {
        let ring = FakeVault::default();
        let mut settings = Settings::default();
        let (old, _) = settings.identity_in(&ring, "id").unwrap();
        settings.devices.push(Device {
            id: "d".into(),
            name: "Phone".into(),
            token_hash: String::new(),
            ticket_hash: String::new(),
            paired_at: 1,
            last_seen: 1,
            push: None,
            notices: Notices::default(),
            muted: Vec::new(),
        });
        let locked = FakeVault {
            locked: true,
            ..FakeVault::default()
        };
        assert!(settings.renew_identity_in(&locked, "id").is_err());
        assert_eq!(settings.devices.len(), 1);
        assert_eq!(settings.relay_label(), Some(old.label()));
        let new = settings.renew_identity_in(&ring, "id").unwrap();
        assert_ne!(new.label(), old.label());
        assert!(settings.devices.is_empty());
        assert_eq!(settings.relay_label(), Some(new.label()));
        assert_eq!(
            settings.identity_in(&ring, "id").unwrap().0.label(),
            new.label()
        );
    }

    #[test]
    fn a_subscription_that_is_gone_takes_only_itself() {
        let sub = |endpoint: &str| push::Subscription {
            endpoint: endpoint.into(),
            p256dh: "k".into(),
            auth: "a".into(),
        };
        let access = Access::new(Vec::new(), String::new());
        let mut access = access.lock().unwrap();
        let (one, _) = access.admit("One", 1);
        let (two, _) = access.admit("Two", 1);
        access.subscribe(&one.id, Door::Local, Some(sub("https://push.example/old")));
        access.subscribe(&two.id, Door::Local, Some(sub("https://push.example/two")));
        // The first phone subscribed again while a push to its old address was out.
        access.subscribe(&one.id, Door::Local, Some(sub("https://push.example/new")));
        access.drop_subscriptions(&[
            (one.id.clone(), sub("https://push.example/old")),
            (two.id.clone(), sub("https://push.example/two")),
        ]);
        let push_of = |id: &str| {
            let d = access.devices.iter().find(|d| d.id == id).unwrap();
            d.push.as_ref().map(|s| s.endpoint.clone())
        };
        assert_eq!(
            push_of(&one.id).as_deref(),
            Some("https://push.example/new")
        );
        assert_eq!(push_of(&two.id), None);
    }

    #[test]
    fn start_reads_with_and_without_an_agent() {
        let with: ClientMsg =
            serde_json::from_str(r#"{"type":"start","env":"/w/app","agent":"claude","text":"hi"}"#)
                .unwrap();
        assert_eq!(
            with,
            ClientMsg::Start {
                env: "/w/app".into(),
                agent: Some("claude".into()),
                text: "hi".into(),
                settings: vec![],
                mentions_from: None,
            }
        );
        let without: ClientMsg = serde_json::from_str(
            r#"{"type":"start","env":"/w/app","text":"hi","settings":[["modelId","m"]]}"#,
        )
        .unwrap();
        assert_eq!(
            without,
            ClientMsg::Start {
                env: "/w/app".into(),
                agent: None,
                text: "hi".into(),
                settings: vec![("modelId".into(), "m".into())],
                mentions_from: None,
            }
        );
        let moved: ClientMsg = serde_json::from_str(
            r#"{"type":"start","env":"/w/new","text":"hi","mentions_from":"/w/app"}"#,
        )
        .unwrap();
        assert!(matches!(
            moved,
            ClientMsg::Start { mentions_from: Some(f), .. } if f == "/w/app"
        ));
    }

    #[test]
    fn an_install_reads_as_the_app_sends_it() {
        use crate::agent::marketplace::Install;
        let install = Install::Remote {
            transport: "http".into(),
            url: "https://mcp.example/".into(),
            headers: vec![],
        };
        let values = HashMap::from([("TOKEN".to_string(), "t".to_string())]);
        // As `extensions_view` builds it.
        let sent = serde_json::json!({
            "type": "install_server", "name": "example", "install": install, "values": values,
        });
        assert_eq!(
            serde_json::from_value::<ClientMsg>(sent).unwrap(),
            ClientMsg::InstallServer {
                name: "example".into(),
                install,
                values,
            }
        );
        let plugin = serde_json::json!({ "type": "install_plugin", "id": "a@b" });
        assert_eq!(
            serde_json::from_value::<ClientMsg>(plugin).unwrap(),
            ClientMsg::InstallPlugin { id: "a@b".into() }
        );
        let mirror = serde_json::json!({ "type": "mirror", "plugins": ["a@b"] });
        assert_eq!(
            serde_json::from_value::<ClientMsg>(mirror).unwrap(),
            ClientMsg::Mirror(crate::agent::mirror::Snapshot {
                plugins: vec!["a@b".into()],
                ..Default::default()
            })
        );
        let market = serde_json::json!({ "type": "market" });
        assert_eq!(
            serde_json::from_value::<ClientMsg>(market).unwrap(),
            ClientMsg::Market
        );
    }

    #[test]
    fn a_prompt_names_its_uploads_and_its_pieces_come_apart() {
        let plain: ClientMsg =
            serde_json::from_str(r#"{"type":"send","session":"s1","text":"hi"}"#).unwrap();
        assert_eq!(
            plain,
            ClientMsg::Send {
                session: "s1".into(),
                text: "hi".into(),
                uploads: vec![],
                now: false,
            }
        );
        let with: ClientMsg = serde_json::from_str(
            r#"{"type":"send","session":"s1","text":"mira","uploads":["u1","u2"]}"#,
        )
        .unwrap();
        assert_eq!(
            with,
            ClientMsg::Send {
                session: "s1".into(),
                text: "mira".into(),
                uploads: vec!["u1".into(), "u2".into()],
                now: false,
            }
        );
        let now: ClientMsg =
            serde_json::from_str(r#"{"type":"send","session":"s1","text":"no","now":true}"#)
                .unwrap();
        assert_eq!(
            now,
            ClientMsg::Send {
                session: "s1".into(),
                text: "no".into(),
                uploads: vec![],
                now: true,
            }
        );
        let piece: ClientMsg = serde_json::from_str(
            r#"{"type":"upload","session":"s1","upload":"u1","name":"IMG_1.jpg","part":0,"of":2,"data":"AAEC"}"#,
        )
        .unwrap();
        assert_eq!(
            piece,
            ClientMsg::Upload {
                session: "s1".into(),
                upload: "u1".into(),
                name: "IMG_1.jpg".into(),
                part: 0,
                of: 2,
                data: "AAEC".into(),
            }
        );
    }

    #[test]
    fn head_reads_path_query_and_upgrade() {
        let h = Head::parse(
            "GET /ws?k=abc&x=1 HTTP/1.1\r\nHost: 10.0.0.2:7474\r\nUpgrade: websocket\r\nOrigin: http://10.0.0.2:7474\r\n\r\n",
        )
        .unwrap();
        assert_eq!(h.path, "/ws");
        assert_eq!(h.query("k"), Some("abc"));
        assert!(h.upgrade);
        assert!(same_origin(&h));
    }

    #[test]
    fn another_origin_is_refused() {
        let h = Head::parse(
            "GET /ws?k=abc HTTP/1.1\r\nHost: 10.0.0.2:7474\r\nOrigin: http://evil.example\r\n\r\n",
        )
        .unwrap();
        assert!(!same_origin(&h));
        let plain = Head::parse("GET / HTTP/1.1\r\nHost: 10.0.0.2:7474\r\n\r\n").unwrap();
        assert!(!plain.upgrade);
        assert!(same_origin(&plain));
    }

    #[test]
    fn the_relay_address_is_the_relay_itself() {
        let mut s = Settings::default();
        assert_eq!(s.relay_address(), "https://app.arakun.dev/");
        s.relay_base = "https://example.workers.dev/ ".into();
        assert_eq!(s.relay_address(), "https://example.workers.dev/");
        s.relay_base = "relay.example".into();
        assert_eq!(s.relay_address(), "https://relay.example/");
        s.relay_base = "http://127.0.0.1:8787".into();
        assert_eq!(s.relay_address(), "http://127.0.0.1:8787/");
    }

    #[test]
    fn settings_without_a_relay_base_get_the_default_one() {
        assert_eq!(DEFAULT_RELAY_BASE, crate::cloud::DEFAULT_URL);
        let s: Settings = serde_json::from_str(r#"{"enabled":false,"port":7474}"#).unwrap();
        assert_eq!(s.relay_base, DEFAULT_RELAY_BASE);
        assert_eq!(s.reach, Reach::Lan);
        let s: Settings =
            serde_json::from_str(r#"{"enabled":true,"port":7474,"reach":"relay"}"#).unwrap();
        assert_eq!(s.reach, Reach::Relay);
    }

    #[test]
    fn the_old_default_relay_reads_as_the_new_one() {
        for old in [
            "https://arakun.dev",
            "https://arakun.dev/",
            " https://arakun.dev ",
        ] {
            let s: Settings = serde_json::from_value(
                serde_json::json!({"enabled": true, "port": 7474, "relay_base": old}),
            )
            .unwrap();
            assert_eq!(s.relay_base, "https://app.arakun.dev", "{old}");
        }
        // A relay of one's own stays.
        let s: Settings = serde_json::from_value(
            serde_json::json!({"enabled": true, "port": 7474, "relay_base": "https://relay.example"}),
        )
        .unwrap();
        assert_eq!(s.relay_base, "https://relay.example");
    }

    /// A device the relay brought in, as the relay client records it.
    fn relay_device(id: &str) -> Device {
        Device {
            id: id.into(),
            name: "Rik's iPhone".into(),
            token_hash: String::new(),
            ticket_hash: String::new(),
            paired_at: 0,
            last_seen: 0,
            push: None,
            notices: Notices::default(),
            muted: Vec::new(),
        }
    }

    #[test]
    fn a_device_from_the_relay_never_opens_the_door_on_the_network() {
        let access = Access::new(vec![relay_device("sess1")], "pk".into());
        for token in ["", " ", "sess1"] {
            assert!(
                access.lock().unwrap().device_for(token, 1).is_none(),
                "{token:?}"
            );
        }
        let (server, _rx, label) = start(&access);
        for token in ["", " ", "sess1"] {
            let refused = greet(&server, &label, json!({ "auth": { "token": token } }))
                .unwrap()
                .1;
            assert_eq!(refused, json!({ "type": "refused", "reason": "unpaired" }));
        }
        for t in ["/ws?t=", "/ws?t=sess1", "/ws"] {
            let refused = http(
                &server,
                "GET",
                t,
                "Upgrade: websocket\r\nConnection: Upgrade\r\n",
                "",
            );
            assert!(refused.starts_with("HTTP/1.1 403"), "{t}: {refused}");
        }
        assert_eq!(access.lock().unwrap().devices.len(), 1);
    }

    #[test]
    fn the_relay_door_only_finds_devices_the_relay_brought_in() {
        // A device paired on the network whose id the relay happens to name.
        let mut lan = relay_device("sess1");
        lan.name = "Pixel".into();
        lan.token_hash = hash("token");
        let access = Access::new(vec![lan.clone()], "pk".into());
        let mut a = access.lock().unwrap();
        assert!(a.seen("sess1", Door::Relay, 5).is_none());

        let (made, seen) = a.relay_device("sess1", "Rik's iPhone", 5);
        assert_eq!(seen, Seen::New);
        assert!(made.token_hash.is_empty());
        assert_eq!(a.devices[0], lan, "the network's device is left alone");
        assert_eq!(
            a.seen("sess1", Door::Relay, 6).unwrap().name,
            "Rik's iPhone"
        );

        let (_, seen) = a.relay_device("sess1", "Rik's iPhone", 7);
        assert_eq!(seen, Seen::Known);
        let (renamed, seen) = a.relay_device("sess1", " Rik's new iPhone ", 8);
        assert_eq!(seen, Seen::Renamed);
        assert_eq!(renamed.name, "Rik's new iPhone");
        assert_eq!(a.devices.len(), 2);
    }

    #[test]
    fn device_without_muted_loads() {
        let old = r#"{"id":"a","name":"n","token_hash":"h","paired_at":1,"last_seen":2}"#;
        let d: Device = serde_json::from_str(old).unwrap();
        assert!(d.muted.is_empty());
    }

    #[test]
    fn post_mute_toggles_one_session() {
        let access = Access::new(Vec::new(), "pk".into());
        let (_, token) = access.lock().unwrap().admit("iPhone", 1);
        let (server, _rx, label) = start(&access);
        let mut phone = signed_in(&server, &label, &token);
        let mute = |session: &str, muted: bool| json!({ "session": session, "muted": muted });
        let (status, _) = phone.req("POST", "/mute", &mute("a", true));
        assert_eq!(status, 200);
        let (_, two) = phone.req("POST", "/mute", &mute("b", true));
        assert_eq!(two["muted"], json!(["a", "b"]));
        let (_, again) = phone.req("POST", "/mute", &mute("b", true));
        assert_eq!(again["muted"], json!(["a", "b"]));
        let (_, me) = phone.req("GET", "/me", &Value::Null);
        assert_eq!(me["muted"], json!(["a", "b"]));
        let (_, off) = phone.req("POST", "/mute", &mute("a", false));
        assert_eq!(off["muted"], json!(["b"]));
        assert_eq!(
            access.lock().unwrap().devices[0].muted,
            vec!["b".to_string()]
        );
        let (bad, _) = phone.req("POST", "/mute", &json!({ "session": "a" }));
        assert_eq!(bad, 400);
    }

    #[test]
    fn mute_changes_only_a_device_of_its_own_door() {
        let mut lan = relay_device("same");
        lan.token_hash = hash("token");
        let access = Access::new(vec![lan, relay_device("same")], "pk".into());
        let mut a = access.lock().unwrap();
        assert!(a.set_muted("same", Door::Relay, "s", true));
        assert!(a.devices[0].muted.is_empty());
        assert_eq!(a.devices[1].muted, vec!["s".to_string()]);
    }

    #[test]
    fn rename_muted_carries_the_mute_to_the_new_id() {
        let mut a = relay_device("a");
        a.muted = vec!["x".into(), "old".into()];
        let mut b = relay_device("b");
        b.muted = vec!["old".into(), "new".into()];
        let access = Access::new(vec![a, b, relay_device("c")], "pk".into());
        let mut g = access.lock().unwrap();
        assert!(g.rename_muted("old", "new"));
        assert_eq!(g.devices[0].muted, vec!["x".to_string(), "new".to_string()]);
        assert_eq!(g.devices[1].muted, vec!["new".to_string()]);
        assert!(g.devices[2].muted.is_empty());
        assert!(!g.rename_muted("old", "new"));
    }

    #[test]
    fn forget_session_unmutes() {
        let mut a = relay_device("a");
        a.muted = vec!["s".into(), "t".into()];
        let mut b = relay_device("b");
        b.muted = vec!["s".into()];
        let access = Access::new(vec![a, b, relay_device("c")], "pk".into());
        let mut g = access.lock().unwrap();
        assert!(g.forget_session("s"));
        assert_eq!(g.devices[0].muted, vec!["t".to_string()]);
        assert!(g.devices[1].muted.is_empty());
        assert!(!g.forget_session("s"), "nothing left to change");
    }

    #[test]
    fn notices_default_when_missing() {
        let old: Notices =
            serde_json::from_str(r#"{"done":true,"input":true,"error":true}"#).unwrap();
        assert_eq!(old.away_after_secs, 120);
        assert_eq!(old.min_turn_secs, 30);
        assert_eq!(old, Notices::default());
        let d = Notices::default();
        assert_eq!((d.away_after_secs, d.min_turn_secs), (120, 30));
    }

    #[test]
    fn a_request_changes_only_a_device_of_its_own_door() {
        // The relay named an id that a device paired on the network has too.
        let mut lan = relay_device("same");
        lan.token_hash = hash("token");
        let access = Access::new(vec![lan, relay_device("same")], "pk".into());
        let mut a = access.lock().unwrap();
        let quiet = Notices {
            done: false,
            input: false,
            error: false,
            ..Notices::default()
        };
        assert!(a.set_notices("same", Door::Relay, quiet));
        assert_eq!(
            a.devices[0].notices,
            Notices::default(),
            "the network's is untouched"
        );
        assert_eq!(a.devices[1].notices, quiet);
        let sub = push::Subscription {
            endpoint: "https://push.example/x".into(),
            p256dh: "k".into(),
            auth: "a".into(),
        };
        assert!(a.subscribe("same", Door::Local, Some(sub)));
        assert!(a.devices[0].push.is_some());
        assert!(a.devices[1].push.is_none(), "the relay's is untouched");
        assert!(a.unpair_through("same", Door::Relay));
        assert_eq!(a.devices.len(), 1);
        assert!(!a.devices[0].token_hash.is_empty(), "the network's stays");
        assert!(!a.unpair_through("same", Door::Relay), "none left there");
        assert!(!a.set_notices("same", Door::Relay, quiet));
    }

    #[test]
    fn a_machine_name_is_fit_to_send_and_show() {
        assert_eq!(
            clean_host_name(" build-box\n").as_deref(),
            Some("build-box")
        );
        assert_eq!(clean_host_name("a\u{7}b\tc").as_deref(), Some("abc"));
        assert_eq!(clean_host_name("  \n"), None);
        assert_eq!(clean_host_name("localhost"), None);
        let long = "é".repeat(150);
        assert_eq!(clean_host_name(&long).unwrap().chars().count(), 100);
        // Whatever this machine is called, the account takes it.
        let name = host_name();
        assert!(!name.is_empty() && name.chars().count() <= 100);
        assert!(!name.chars().any(char::is_control));
    }

    #[test]
    fn devices_the_account_signed_out_are_dropped_and_local_ones_stay() {
        let mut lan = relay_device("lan1");
        lan.token_hash = hash("token");
        let mut gone = relay_device("gone");
        gone.push = Some(push::Subscription {
            endpoint: "https://push.example/gone".into(),
            p256dh: "k".into(),
            auth: "a".into(),
        });
        let mut devices = vec![lan.clone(), relay_device("here"), gone];
        let ids = vec!["here".to_string(), "lan1".to_string()];
        // The devices here came in at 0, before any list was asked.
        let listed = |ids| Some(SignedInDevices { ids, asked_at: 100 });
        let dropped = prune_relay_devices(&mut devices, listed(&ids));
        assert_eq!(dropped, ["gone"]);
        let left: Vec<&str> = devices.iter().map(|d| d.id.as_str()).collect();
        assert_eq!(left, ["lan1", "here"]);
        // A local device is kept even when the account does not list it.
        let dropped = prune_relay_devices(&mut devices, listed(&[]));
        assert_eq!(dropped, ["here"]);
        assert_eq!(devices, [lan.clone()]);
        // One that signed in after the list was asked is not in it: it stays.
        let mut late = relay_device("late");
        late.paired_at = 100;
        let mut devices = vec![relay_device("old"), late.clone()];
        assert_eq!(prune_relay_devices(&mut devices, listed(&[])), ["old"]);
        assert_eq!(devices, [late]);
        // Under another account or none: every relay one goes, local ones stay.
        let mut devices = vec![relay_device("a"), lan.clone(), relay_device("b")];
        assert_eq!(prune_relay_devices(&mut devices, None), ["a", "b"]);
        assert_eq!(devices, [lan]);
    }

    #[test]
    fn the_signed_in_devices_are_asked_for_every_15_minutes_while_one_gets_pushes() {
        // Later than now, so going back from it never passes the clock's start.
        let now = Instant::now() + 2 * RELAY_PRUNE_EVERY;
        let sub = push::Subscription {
            endpoint: "https://push.example/x".into(),
            p256dh: "k".into(),
            auth: "a".into(),
        };
        let mut pushed = relay_device("r1");
        pushed.push = Some(sub.clone());
        let mut lan = relay_device("lan1");
        lan.token_hash = hash("token");
        lan.push = Some(sub);
        let devices = [relay_device("r0"), pushed];
        // Not signed in: no.
        assert!(!relay_prune_due(false, &devices, None, now));
        // No relay device with a push subscription: no (one on the network does not count).
        assert!(!relay_prune_due(
            true,
            &[relay_device("r0"), lan],
            None,
            now
        ));
        // Asked under 15 minutes ago: no.
        let recent = now - Duration::from_secs(14 * 60);
        assert!(!relay_prune_due(true, &devices, Some(recent), now));
        // Otherwise: yes.
        assert!(relay_prune_due(true, &devices, None, now));
        let old = now - RELAY_PRUNE_EVERY;
        assert!(relay_prune_due(true, &devices, Some(old), now));
    }

    #[test]
    fn the_relay_devices_go_when_the_account_changes_or_signs_out() {
        // Not known yet: nothing changes.
        assert_eq!(relay_owner(Some(1), AccountNow::Unknown), (false, Some(1)));
        // The same account, or the first one known: they stay.
        assert_eq!(
            relay_owner(Some(1), AccountNow::SignedIn(1)),
            (false, Some(1))
        );
        assert_eq!(relay_owner(None, AccountNow::SignedIn(1)), (false, Some(1)));
        // Another account, or none: they go.
        assert_eq!(
            relay_owner(Some(1), AccountNow::SignedIn(2)),
            (true, Some(2))
        );
        assert_eq!(relay_owner(Some(1), AccountNow::SignedOut), (true, None));
    }

    /// One HTTP request to the server, the whole answer back.
    fn http(server: &Server, method: &str, target: &str, extra: &str, body: &str) -> String {
        let mut s = TcpStream::connect(("127.0.0.1", server.port())).unwrap();
        write!(s, "{method} {target} HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Length: {}\r\n{extra}\r\n{body}", body.len())
            .unwrap();
        let mut out = String::new();
        s.read_to_string(&mut out).unwrap();
        out
    }

    /// A server on the loopback with a key of its own, and that key's label.
    fn start(access: &Shared) -> (Server, mpsc::UnboundedReceiver<Incoming>, String) {
        let identity = identity::Identity::generate();
        let label = identity.label();
        let (server, rx) =
            Server::start(Ipv4Addr::LOCALHOST, 0, access.clone(), Arc::new(identity)).unwrap();
        (server, rx, label)
    }

    type Ws = tungstenite::WebSocket<tungstenite::stream::MaybeTlsStream<TcpStream>>;

    /// A phone on the sealed socket, past its greeting.
    struct Phone {
        ws: Ws,
        session: e2e::Session,
        next: u64,
    }

    impl Phone {
        fn send(&mut self, msg: serde_json::Value) {
            let sealed = self.session.seal(msg.to_string().as_bytes());
            self.ws
                .send(tungstenite::Message::Binary(
                    lan::frame(relay::DATA, &sealed).into(),
                ))
                .unwrap();
        }

        /// The next sealed message, skipping pings.
        fn hear(&mut self) -> serde_json::Value {
            loop {
                match self.ws.read().unwrap() {
                    tungstenite::Message::Binary(b) => {
                        assert_eq!(b[0], relay::DATA);
                        let plain = self.session.open(&b[1..]).unwrap();
                        return serde_json::from_slice(&plain).unwrap();
                    }
                    tungstenite::Message::Ping(_) | tungstenite::Message::Pong(_) => {}
                    other => panic!("unexpected {other:?}"),
                }
            }
        }

        /// What would be an HTTP request: its status and its JSON body.
        fn req(
            &mut self,
            method: &str,
            path: &str,
            body: &serde_json::Value,
        ) -> (u16, serde_json::Value) {
            let (status, _, bytes) = self.req_raw(method, path, body);
            (status, serde_json::from_slice(&bytes).unwrap_or_default())
        }

        fn req_raw(
            &mut self,
            method: &str,
            path: &str,
            body: &serde_json::Value,
        ) -> (u16, String, Vec<u8>) {
            use base64::Engine as _;
            self.next += 1;
            let id = self.next;
            let body = match body {
                serde_json::Value::Null => serde_json::Value::Null,
                other => other.to_string().into(),
            };
            self.send(json!({ "req": { "id": id, "method": method, "path": path, "body": body } }));
            loop {
                let msg = self.hear();
                let Some(res) = msg.get("res") else { continue };
                assert_eq!(res["id"], id);
                assert_eq!(res["more"], false);
                let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
                    .decode(res["body"].as_str().unwrap())
                    .unwrap();
                return (
                    res["status"].as_u64().unwrap() as u16,
                    res["type"].as_str().unwrap().to_string(),
                    bytes,
                );
            }
        }
    }

    fn dial(server: &Server) -> Ws {
        let url = format!("ws://127.0.0.1:{}{}", server.port(), lan::PATH);
        tungstenite::connect(url).unwrap().0
    }

    /// The handshake checked against `label`, then `greeting`: the phone and
    /// the computer's answer.
    fn greet(
        server: &Server,
        label: &str,
        greeting: serde_json::Value,
    ) -> Result<(Phone, serde_json::Value), lan::GreetError> {
        let mut ws = dial(server);
        let greeted = lan::greet(&mut ws, Some(label), &greeting, Duration::from_secs(5))?;
        let phone = Phone {
            ws,
            session: greeted.session,
            next: 0,
        };
        Ok((phone, greeted.answer))
    }

    /// A phone paired through the server, its token, and the phone.
    fn pair(server: &Server, access: &Shared, label: &str, name: &str) -> (String, Phone) {
        let code = access.lock().unwrap().begin_pairing();
        let (phone, answer) = greet(
            server,
            label,
            json!({ "pair": { "code": code, "name": name } }),
        )
        .unwrap();
        assert_eq!(answer["type"], "paired", "{answer}");
        (answer["token"].as_str().unwrap().to_string(), phone)
    }

    /// A phone let in with its token.
    fn signed_in(server: &Server, label: &str, token: &str) -> Phone {
        let (phone, answer) = greet(server, label, json!({ "auth": { "token": token } })).unwrap();
        assert_eq!(answer["type"], "welcome", "{answer}");
        phone
    }

    /// The body a browser would send to `/push`: a fresh P-256 key and auth secret.
    fn browser_subscription(endpoint: &str) -> serde_json::Value {
        use base64::Engine as _;
        let b64 = base64::engine::general_purpose::URL_SAFE_NO_PAD;
        let rng = ring::rand::SystemRandom::new();
        let private =
            ring::agreement::EphemeralPrivateKey::generate(&ring::agreement::ECDH_P256, &rng)
                .unwrap();
        let public = private.compute_public_key().unwrap();
        let mut auth = [0u8; 16];
        ring::rand::SecureRandom::fill(&rng, &mut auth).unwrap();
        json!({
            "subscription": { "endpoint": endpoint, "keys": { "p256dh": b64.encode(public.as_ref()), "auth": b64.encode(auth) } }
        })
    }

    #[test]
    fn serves_the_page_and_nothing_else_in_the_clear() {
        let access = Access::new(Vec::new(), "pk".into());
        let (_, token) = access.lock().unwrap().admit("iPhone", 1);
        let (server, _rx, _) = start(&access);
        let index = http(&server, "GET", "/", "", "");
        assert!(index.starts_with("HTTP/1.1 200"), "{index}");
        assert!(index.contains("text/html"));
        // A transcript that slips markup into the page cannot run script.
        let head = index.split("\r\n\r\n").next().unwrap();
        let policy = head
            .lines()
            .find_map(|l| l.strip_prefix("Content-Security-Policy: "))
            .unwrap_or_else(|| panic!("no page policy in {head}"));
        let script = policy
            .split(';')
            .map(str::trim)
            .find(|d| d.starts_with("script-src"));
        assert_eq!(script, Some("script-src 'self'"), "{policy}");
        assert!(head.contains("X-Content-Type-Options: nosniff"), "{head}");
        // The built bundle sits in a folder of its own under phone/dist.
        let bundle = http(&server, "GET", "/assets/app.js", "", "");
        assert!(bundle.starts_with("HTTP/1.1 200"), "{bundle}");
        assert!(bundle.contains("text/javascript"));
        assert!(http(&server, "GET", "/nope.txt", "", "").starts_with("HTTP/1.1 404"));
        // The old doors in the clear are gone, even with a good token.
        let bearer = format!("Authorization: Bearer {token}\r\n");
        for (method, path) in [
            ("GET", "/me"),
            ("POST", "/push"),
            ("POST", "/mute"),
            ("POST", "/unpair"),
            ("POST", "/pair"),
            ("GET", "/image?p=x&s=y"),
        ] {
            let res = http(&server, method, path, &bearer, "{}");
            assert!(res.starts_with("HTTP/1.1 404"), "{method} {path}: {res}");
        }
        let upgrade = "Upgrade: websocket\r\nConnection: Upgrade\r\n";
        let old = http(&server, "GET", &format!("/ws?t={token}"), upgrade, "");
        assert!(old.starts_with("HTTP/1.1 403"), "{old}");
        // A page from elsewhere may not open the sealed socket either.
        let other = format!("{upgrade}Origin: http://evil.example\r\n");
        let refused = http(&server, "GET", lan::PATH, &other, "");
        assert!(refused.starts_with("HTTP/1.1 403"), "{refused}");
    }

    #[test]
    fn a_phone_refuses_a_computer_with_another_key() {
        let access = Access::new(Vec::new(), "pk".into());
        let (server, _rx, _) = start(&access);
        let other = identity::Identity::generate().label();
        let code = access.lock().unwrap().begin_pairing();
        let mut ws = dial(&server);
        let pair = json!({ "pair": { "code": code, "name": "p" } });
        let refused = lan::greet(&mut ws, Some(&other), &pair, Duration::from_secs(5));
        assert_eq!(
            refused.err(),
            Some(lan::GreetError::Untrusted("bad hello: label".into()))
        );
        // Nothing was sent, so the code still stands.
        assert!(access.lock().unwrap().pairing().is_some());
        assert!(access.lock().unwrap().devices.is_empty());
    }

    #[test]
    fn a_typed_code_trusts_the_key_it_meets_and_names_it() {
        let access = Access::new(Vec::new(), "pk".into());
        let (server, _rx, label) = start(&access);
        let code = access.lock().unwrap().begin_pairing();
        let mut ws = dial(&server);
        let pair = json!({ "pair": { "code": code, "name": "p" } });
        let greeted = lan::greet(&mut ws, None, &pair, Duration::from_secs(5)).unwrap();
        assert_eq!(greeted.label, label);
        assert_eq!(greeted.answer["type"], "paired");
    }

    #[test]
    fn a_frame_that_does_not_open_ends_the_connection() {
        let access = Access::new(Vec::new(), "pk".into());
        let (server, mut rx, label) = start(&access);
        let (_, mut phone) = pair(&server, &access, &label, "p");
        let id = loop {
            if let Incoming::Connected(id, _) = next(&mut rx) {
                break id;
            }
        };
        let mut sealed = phone.session.seal(br#"{"type":"ping"}"#);
        sealed[10] ^= 1;
        phone
            .ws
            .send(tungstenite::Message::Binary(
                lan::frame(relay::DATA, &sealed).into(),
            ))
            .unwrap();
        disconnected(&mut rx, id, Duration::from_secs(5));
    }

    #[test]
    fn a_greeting_that_is_neither_pair_nor_auth_is_refused() {
        let access = Access::new(Vec::new(), "pk".into());
        let (server, _rx, label) = start(&access);
        let (_, answer) = greet(&server, &label, json!({ "hello": 1 })).unwrap();
        assert_eq!(answer, json!({ "type": "refused", "reason": "bad" }));
    }

    #[test]
    fn images_come_only_through_signed_links() {
        let dir = std::env::temp_dir().join(format!("arakun-phone-img-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let shot = dir.join("my shot.png");
        std::fs::write(&shot, b"PNG-ish bytes").unwrap();
        std::fs::write(dir.join("notes.txt"), b"text").unwrap();
        let access = Access::new(Vec::new(), "pk".into());
        let (_, token) = access.lock().unwrap().admit("iPhone", 1);
        let (server, _rx, label) = start(&access);
        let key = access.lock().unwrap().image_key.clone();
        let mut phone = signed_in(&server, &label, &token);
        let mut get = |link: &str| phone.req_raw("GET", link, &Value::Null);

        let (status, kind, bytes) = get(&image_link(&key, &shot));
        assert_eq!((status, kind.as_str()), (200, "image/png"));
        assert_eq!(bytes, b"PNG-ish bytes");

        // A wrong key, a changed path, or a file that is not an image: nothing.
        let forged = image_link("other-key", &shot);
        assert_eq!(get(&forged).0, 404);
        let moved = image_link(&key, &shot).replace("my%20shot", "my%20shot2");
        assert_eq!(get(&moved).0, 404);
        let text = image_link(&key, &dir.join("notes.txt"));
        assert_eq!(get(&text).0, 404);
        assert_eq!(get("/image?p=x").0, 404);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    fn next(rx: &mut mpsc::UnboundedReceiver<Incoming>) -> Incoming {
        futures::executor::block_on(futures::StreamExt::next(rx)).expect("the server stopped")
    }

    #[test]
    fn pairing_takes_the_code_once_and_gives_up_after_wrong_tries() {
        let access = Access::new(Vec::new(), "pk".into());
        let (server, mut rx, label) = start(&access);
        // No code yet: nothing to bring.
        let (_, refused) = greet(
            &server,
            &label,
            json!({ "pair": { "code": "000000", "name": "x" } }),
        )
        .unwrap();
        assert_eq!(refused, json!({ "type": "refused", "reason": "code" }));

        let (token, mut phone) = pair(&server, &access, &label, "Bob's phone");
        assert_eq!(token.len(), 64);
        assert!(matches!(next(&mut rx), Incoming::Paired));
        let device = access.lock().unwrap().devices[0].clone();
        assert_eq!(device.name, "Bob's phone");
        assert_ne!(device.token_hash, token, "the token is kept hashed");
        // The code was spent.
        assert!(access.lock().unwrap().pairing().is_none());

        let (status, me) = phone.req("GET", "/me", &Value::Null);
        assert_eq!(status, 200, "{me}");
        assert_eq!(me["id"], device.id);
        assert_eq!(me["push_key"], "pk");
        assert_eq!(me["notifying"], false);

        // The phone turns notifications on with its subscription, and off with null.
        let sub = browser_subscription("https://push.example/x");
        let (on, _) = phone.req("POST", "/push", &sub);
        assert_eq!(on, 200);
        loop {
            if let Incoming::Subscribed = next(&mut rx) {
                break;
            }
        }
        assert_eq!(
            access.lock().unwrap().devices[0]
                .push
                .as_ref()
                .map(|s| s.endpoint.as_str()),
            Some("https://push.example/x")
        );
        assert_eq!(phone.req("GET", "/me", &Value::Null).1["notifying"], true);
        let broken = json!({"subscription":{"endpoint":"https://x/","keys":{}}});
        assert_eq!(phone.req("POST", "/push", &broken).0, 400);

        // It picks what to hear about; the subscription stays as it was.
        assert_eq!(
            me["notices"],
            json!({
                "done": true,
                "input": true,
                "error": true,
                "away_after_secs": 120,
                "min_turn_secs": 30
            })
        );
        let (status, picked) = phone.req(
            "POST",
            "/push",
            &json!({"notices":{"done":false,"input":true,"error":true}}),
        );
        assert_eq!(status, 200);
        assert_eq!(picked["notifying"], true);
        assert_eq!(picked["notices"]["done"], false);
        assert_eq!(
            access.lock().unwrap().devices[0].notices,
            Notices {
                done: false,
                ..Notices::default()
            }
        );
        // One setting alone changes only itself.
        let (status, away) = phone.req("POST", "/push", &json!({"notices":{"away_after_secs":0}}));
        assert_eq!(status, 200);
        assert_eq!(away["notices"]["away_after_secs"], 0);
        assert_eq!(
            access.lock().unwrap().devices[0].notices,
            Notices {
                done: false,
                away_after_secs: 0,
                ..Notices::default()
            }
        );
        let bad = json!({"notices":{"min_turn_secs":-1}});
        assert_eq!(phone.req("POST", "/push", &bad).0, 400);
        let bad = json!({"notices":{"done":"yes"}});
        assert_eq!(phone.req("POST", "/push", &bad).0, 400);

        let (off, _) = phone.req("POST", "/push", &json!({"subscription":null}));
        assert_eq!(off, 200);
        assert!(access.lock().unwrap().devices[0].push.is_none());
        assert!(
            !access.lock().unwrap().devices[0].notices.done,
            "the choice outlives the subscription"
        );

        // A wrong code, enough times, throws the current one away.
        let code = access.lock().unwrap().begin_pairing();
        let wrong = if code == "111111" { "222222" } else { "111111" };
        let bring = |code: &str| {
            greet(
                &server,
                &label,
                json!({ "pair": { "code": code, "name": "x" } }),
            )
            .unwrap()
            .1
        };
        for _ in 0..PAIR_TRIES {
            assert_eq!(bring(wrong)["reason"], "code");
        }
        assert_eq!(bring(&code)["reason"], "code");
        assert_eq!(access.lock().unwrap().devices.len(), 1);
    }

    /// A connected phone and its id on the server.
    fn connect_phone(
        server: &Server,
        access: &Shared,
        label: &str,
        rx: &mut mpsc::UnboundedReceiver<Incoming>,
    ) -> (Phone, ClientId) {
        let (_, phone) = pair(server, access, label, "p");
        loop {
            if let Incoming::Connected(id, _) = next(rx) {
                return (phone, id);
            }
        }
    }

    /// Waits for the server to say the phone left.
    fn disconnected(rx: &mut mpsc::UnboundedReceiver<Incoming>, id: ClientId, within: Duration) {
        let deadline = Instant::now() + within;
        while Instant::now() < deadline {
            match rx.try_recv() {
                Ok(Incoming::Disconnected(gone)) if gone == id => return,
                Ok(_) => {}
                Err(_) => std::thread::sleep(Duration::from_millis(10)),
            }
        }
        panic!("the phone was not dropped in time");
    }

    #[test]
    fn a_page_that_pings_hears_a_pong() {
        let access = Access::new(Vec::new(), "pk".into());
        let (server, mut rx, label) = start(&access);
        let (mut phone, _) = connect_phone(&server, &access, &label, &mut rx);
        phone.send(json!({ "type": "ping" }));
        assert_eq!(phone.hear().to_string(), wire::pong());
    }

    #[test]
    fn a_phone_that_answers_pings_stays() {
        let access = Access::new(Vec::new(), "pk".into());
        let (server, mut rx, label) = start(&access);
        let (mut phone, id) = connect_phone(&server, &access, &label, &mut rx);
        // Reading answers the server's pings; nothing else is said.
        let tungstenite::stream::MaybeTlsStream::Plain(tcp) = phone.ws.get_ref() else {
            unreachable!("a plain socket");
        };
        tcp.set_read_timeout(Some(Duration::from_millis(50)))
            .unwrap();
        let until = Instant::now() + PHONE_SILENCE_MAX * 2;
        let mut pinged = false;
        while Instant::now() < until {
            match phone.ws.read() {
                Ok(tungstenite::Message::Ping(_)) => pinged = true,
                Ok(_) => {}
                Err(tungstenite::Error::Io(e))
                    if matches!(
                        e.kind(),
                        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                    ) => {}
                Err(e) => panic!("dropped: {e}"),
            }
        }
        assert!(pinged, "the server pinged");
        assert!(server.device_of(id).is_some(), "still connected");
    }

    #[test]
    fn a_phone_that_stops_answering_is_dropped() {
        let access = Access::new(Vec::new(), "pk".into());
        let (server, mut rx, label) = start(&access);
        // Never read again, so the server's pings go unanswered.
        let (_phone, id) = connect_phone(&server, &access, &label, &mut rx);
        disconnected(&mut rx, id, PHONE_SILENCE_MAX * 5);
        assert_eq!(server.device_of(id), None);
    }

    #[test]
    fn a_phone_whose_queue_fills_is_dropped() {
        let (tx, _rx) = std_mpsc::sync_channel(1);
        let server = Server {
            port: 0,
            loopback: true,
            clients: Arc::default(),
            stop: Arc::default(),
        };
        let gone = Arc::new(AtomicBool::new(false));
        server.clients.lock().unwrap().insert(
            1,
            Client {
                device: "d".into(),
                out: tx,
                gone: gone.clone(),
            },
        );
        server.send(1, "first".into());
        assert_eq!(server.client_count(), 1, "room for one");
        assert!(!gone.load(Ordering::Relaxed));
        server.send(1, "second".into());
        assert_eq!(server.client_count(), 0, "a full queue drops it");
        // Its loop stops now, without writing out what is still queued.
        assert!(gone.load(Ordering::Relaxed), "its loop is told");

        let (tx, _rx) = std_mpsc::sync_channel(1);
        let gone = Arc::new(AtomicBool::new(false));
        server.clients.lock().unwrap().insert(
            2,
            Client {
                device: "d".into(),
                out: tx,
                gone: gone.clone(),
            },
        );
        server.broadcast("one");
        server.broadcast("two");
        assert_eq!(server.client_count(), 0, "and so does a broadcast");
        assert!(gone.load(Ordering::Relaxed), "its loop is told");
    }

    #[test]
    fn a_phone_talks_both_ways_and_unpairing_cuts_it_off() {
        let access = Access::new(Vec::new(), "pk".into());
        let (server, mut rx, label) = start(&access);
        let (token, mut phone) = pair(&server, &access, &label, "p");
        phone.send(json!({"type":"open","session":"s1","seq":1}));
        phone.send(json!({"type":"visible","visible":false}));
        let mut got = Vec::new();
        let device = access.lock().unwrap().devices[0].id.clone();
        while got.len() < 4 {
            match next(&mut rx) {
                Incoming::Paired => got.push("paired"),
                Incoming::Connected(id, dev) => {
                    assert_eq!(dev, device);
                    server.send(id, r#"{"type":"hello"}"#.into());
                    got.push("connected");
                }
                Incoming::Message(_, ClientMsg::Open { session, .. }) => {
                    assert_eq!(session, "s1");
                    got.push("open");
                }
                Incoming::Message(_, ClientMsg::Visible { visible: false }) => got.push("hidden"),
                _ => panic!("unexpected"),
            }
        }
        // What it numbered is acknowledged, sealed like the rest.
        let mut heard = vec![phone.hear(), phone.hear()];
        heard.sort_by_key(|m| m.to_string());
        assert_eq!(
            heard,
            vec![json!({"type":"ack","seq":1}), json!({"type":"hello"})]
        );
        server.broadcast(r#"{"type":"all"}"#);
        assert_eq!(phone.hear(), json!({"type":"all"}));
        assert_eq!(server.connected_devices(), vec![device.clone()]);

        access.lock().unwrap().unpair(&device);
        server.disconnect_device(&device);
        loop {
            match phone.ws.read() {
                Ok(tungstenite::Message::Close(_)) | Err(_) => break,
                Ok(_) => {}
            }
        }
        let (_, refused) = greet(&server, &label, json!({ "auth": { "token": token } })).unwrap();
        assert_eq!(refused["reason"], "unpaired");
    }

    #[test]
    fn a_phone_unpairs_itself() {
        let access = Access::new(Vec::new(), "pk".into());
        let (server, mut rx, label) = start(&access);
        let (token, mut phone) = pair(&server, &access, &label, "p");
        let mut other = signed_in(&server, &label, &token);

        let (status, _) = phone.req("POST", "/unpair", &Value::Null);
        assert_eq!(status, 200);
        assert!(access.lock().unwrap().devices.is_empty());
        // Both its sockets close, its token stops working, and the desktop
        // hears that someone left.
        for ws in [&mut phone.ws, &mut other.ws] {
            loop {
                match ws.read() {
                    Ok(tungstenite::Message::Close(_)) | Err(_) => break,
                    Ok(_) => {}
                }
            }
        }
        let (_, refused) = greet(&server, &label, json!({ "auth": { "token": token } })).unwrap();
        assert_eq!(refused["reason"], "unpaired");
        let mut heard = false;
        for _ in 0..8 {
            if let Incoming::Unpaired = next(&mut rx) {
                heard = true;
                break;
            }
        }
        assert!(heard);
    }

    #[test]
    fn idle_reads_as_the_app_sends_it() {
        // As `shell::servers::idle` writes it.
        let sent = serde_json::json!({ "type": "idle", "secs": 75 });
        assert_eq!(
            serde_json::from_value::<ClientMsg>(sent).unwrap(),
            ClientMsg::Idle { secs: 75 }
        );
    }

    #[test]
    fn queue_messages_parse_and_a_plain_cancel_drops_the_queue() {
        let parse = |s: &str| serde_json::from_str::<ClientMsg>(s).unwrap();
        assert_eq!(
            parse(r#"{"type":"cancel","session":"s1"}"#),
            ClientMsg::Cancel {
                session: "s1".into(),
                keep_queue: false,
            }
        );
        assert_eq!(
            parse(r#"{"type":"cancel","session":"s1","keep_queue":true}"#),
            ClientMsg::Cancel {
                session: "s1".into(),
                keep_queue: true,
            }
        );
        assert_eq!(
            parse(r#"{"type":"unqueue","session":"s1","index":2,"text":"go","now":true}"#),
            ClientMsg::Unqueue {
                session: "s1".into(),
                index: 2,
                text: "go".into(),
                now: true,
            }
        );
        assert_eq!(
            parse(r#"{"type":"resume_queue","session":"s1"}"#),
            ClientMsg::ResumeQueue {
                session: "s1".into()
            }
        );
    }
}
