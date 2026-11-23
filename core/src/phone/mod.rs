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
//! hands the bytes over. Only paired phones may connect: pairing starts on the
//! computer with a short code that lives two minutes and works once (the QR code
//! carries it), and gives the phone a token of its own, which the computer keeps
//! hashed and can revoke. Every WebSocket brings that token.

pub mod e2e;
pub mod identity;
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
use quiet::Quiet;
use serde::{Deserialize, Serialize};
use sha2::Digest as _;
use tungstenite::{Message, WebSocket};

use crate::phone_page::Page;

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
}

/// A paired phone. Its token is kept only as a hash: the file does not open the door.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Device {
    pub id: String,
    pub name: String,
    /// Empty for a device the relay brought in: it has no token, and an
    /// empty hash never matches one (`device_for`).
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
}

impl Default for Notices {
    fn default() -> Self {
        Notices {
            done: true,
            input: true,
            error: true,
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
    /// The key pair push requests are signed with (PKCS#8, base64url), made on
    /// first use and kept: phones bind their subscriptions to its public half.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vapid: Option<String>,
    /// Who this computer is to the relay (PKCS#8, base64url): its label derives
    /// from this key, so a new one is a new address for every phone.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub identity: Option<String>,
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
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            enabled: false,
            port: DEFAULT_PORT,
            reach: Reach::Lan,
            vapid: None,
            identity: None,
            relay_base: default_relay_base(),
            devices: Vec::new(),
            relay_account: None,
        }
    }
}

impl Settings {
    /// Where devices find this computer through the relay: the relay's own
    /// page, which lists the account's computers.
    pub fn relay_address(&self) -> String {
        relay::page_url(&self.relay_base)
    }

    /// Forgets the relay identity and every paired phone, and saves them on the
    /// background writer (`save_soon`), for the window's click handler: the
    /// computer gets a new address, and nothing paired against the old one may
    /// keep working.
    pub fn renew_identity(&mut self) -> identity::Identity {
        self.identity = None;
        self.devices.clear();
        self.identity_soon()
    }

    /// This computer's label on the relay, when it has a key yet. Unlike
    /// `identity`, makes none.
    pub fn relay_label(&self) -> Option<String> {
        use base64::Engine as _;
        let pkcs8 = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(self.identity.as_ref()?)
            .ok()?;
        Some(identity::Identity::from_pkcs8(&pkcs8)?.label())
    }

    /// The computer's relay identity, made and saved the first time.
    pub fn identity(&mut self) -> identity::Identity {
        self.identity_saving(Self::save)
    }

    /// `identity`, saved with `save_soon`.
    pub fn identity_soon(&mut self) -> identity::Identity {
        self.identity_saving(Self::save_soon)
    }

    fn identity_saving(&mut self, save: impl FnOnce(&Self)) -> identity::Identity {
        use base64::Engine as _;
        let kept = self.identity.as_ref().and_then(|s| {
            base64::engine::general_purpose::URL_SAFE_NO_PAD
                .decode(s)
                .ok()
        });
        if let Some(id) = kept.and_then(|b| identity::Identity::from_pkcs8(&b)) {
            return id;
        }
        let id = identity::Identity::generate();
        self.identity = Some(id.pkcs8_b64());
        save(self);
        id
    }

    /// The push signing keys, made and saved the first time.
    pub fn vapid(&mut self) -> push::Vapid {
        self.vapid_saving(Self::save)
    }

    /// `vapid`, saved with `save_soon`.
    pub fn vapid_soon(&mut self) -> push::Vapid {
        self.vapid_saving(Self::save_soon)
    }

    fn vapid_saving(&mut self, save: impl FnOnce(&Self)) -> push::Vapid {
        use base64::Engine as _;
        let kept = self.vapid.as_ref().and_then(|s| {
            base64::engine::general_purpose::URL_SAFE_NO_PAD
                .decode(s)
                .ok()
        });
        if let Some(v) = kept.and_then(|b| push::Vapid::from_pkcs8(&b)) {
            return v;
        }
        let v = push::Vapid::generate();
        self.vapid = Some(v.pkcs8_b64());
        save(self);
        v
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

    /// The device the relay brought in with this id, noting it was seen now.
    /// Only devices without a token: one paired on the network is never
    /// found through the relay's door, whatever id the relay names.
    pub fn seen(&mut self, id: &str, now: u64) -> Option<Device> {
        let d = self
            .devices
            .iter_mut()
            .find(|d| d.token_hash.is_empty() && d.id == id)?;
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
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Door {
    Relay,
    Local,
}

impl Door {
    /// Whether `d` came in through this door: the relay's devices have no token.
    pub fn admits(self, d: &Device) -> bool {
        d.token_hash.is_empty() == (self == Door::Relay)
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

/// Makes this process keep its phones, its relay identity and its push keys in
/// `path`. `arakun server` calls it before anything loads the settings: with
/// the app's file it would take the app's place at the relay.
pub fn use_settings_file(path: PathBuf) {
    let _ = SETTINGS_FILE.set(path);
}

fn settings_file() -> PathBuf {
    if let Some(path) = SETTINGS_FILE.get() {
        return path.clone();
    }
    // A test instance with its own state file keeps its own phone, on its own port.
    if let Some(state) = std::env::var_os("ARAKUN_STATE") {
        return PathBuf::from(state).with_file_name("phone.json");
    }
    crate::platform::app_dir().join("phone.json")
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

    /// Whole or not at all, and for this user only: the file holds the relay
    /// identity, and it is rewritten each time a device connects. A plain
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
    clients: Clients,
    stop: Arc<AtomicBool>,
}

impl Server {
    /// Listens at `bind:port`; messages from phones come out of the receiver.
    pub fn start(
        bind: Ipv4Addr,
        port: u16,
        access: Shared,
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
                        let (tx, c, s, access) = (tx.clone(), c.clone(), s.clone(), access.clone());
                        move || answer(stream, id, tx, c, s, &access)
                    },
                );
            })?;
        Ok((
            Server {
                port,
                clients,
                stop,
            },
            rx,
        ))
    }

    pub fn port(&self) -> u16 {
        self.port
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

/// Serves one connection: the page over HTTP, or a phone over WebSocket.
fn answer(
    mut stream: TcpStream,
    id: ClientId,
    tx: mpsc::UnboundedSender<Incoming>,
    clients: Clients,
    stop: Arc<AtomicBool>,
    access: &Shared,
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
    let now = crate::model::unix_now();
    if req.upgrade {
        let device = req
            .query("t")
            .and_then(|t| access.lock().unwrap().device_for(t, now));
        let Some(device) = device.filter(|_| req.path == "/ws" && same_origin(&req)) else {
            consume(&mut stream);
            let _ = respond(&mut stream, 403, "text/plain", b"forbidden");
            return;
        };
        let (out_tx, out_rx) = std_mpsc::sync_channel(PHONE_QUEUE);
        let gone = Arc::new(AtomicBool::new(false));
        clients.lock().unwrap().insert(
            id,
            Client {
                device: device.id.clone(),
                out: out_tx,
                gone: gone.clone(),
            },
        );
        let ended = || stop.load(Ordering::Relaxed) || gone.load(Ordering::Relaxed);
        talk(stream, id, device.id, out_rx, &tx, ended);
        clients.lock().unwrap().remove(&id);
        let _ = tx.unbounded_send(Incoming::Disconnected(id));
        return;
    }
    consume(&mut stream);
    // Read before answering: a body left unread resets the connection.
    let body = if req.method == "POST" {
        read_body(
            &mut stream,
            req.content_length,
            Instant::now() + BODY_DEADLINE,
        )
    } else {
        None
    };
    let (reply, effect) = route(&req, body, access, now);
    let _ = respond(&mut stream, reply.status, reply.content_type, &reply.body);
    match effect {
        Effect::None => {}
        Effect::Paired => {
            let _ = tx.unbounded_send(Incoming::Paired);
        }
        Effect::Subscribed => {
            let _ = tx.unbounded_send(Incoming::Subscribed);
        }
        Effect::Unpaired(device) => {
            clients.lock().unwrap().retain(|_, c| c.device != device);
            let _ = tx.unbounded_send(Incoming::Unpaired);
        }
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

/// Answers one request about the phone's account with this computer: pairing,
/// who it is, notifications, unpairing, the images prompts mention, and the
/// page itself. The same whether the request came over TCP or inside an
/// encrypted channel through the relay, except that pairing is the network's:
/// the relay brings in devices by account.
pub fn route(req: &Head, body: Option<Vec<u8>>, access: &Shared, now: u64) -> (Reply, Effect) {
    match (req.method.as_str(), req.path.as_str()) {
        ("POST", "/pair") if req.relay_device.is_none() => {
            let Some(ask) = body.and_then(|b| serde_json::from_slice::<PairRequest>(&b).ok())
            else {
                return (Reply::error(400, "bad request"), Effect::None);
            };
            let paired = access.lock().unwrap().pair(&ask.code, &ask.name, now);
            match paired {
                Some((device, token)) => {
                    let body = serde_json::json!({ "token": token, "id": device.id, "name": device.name, "host": host_name() });
                    (Reply::json(200, &body), Effect::Paired)
                }
                None => (Reply::error(403, "wrong or expired code"), Effect::None),
            }
        }
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
                Some(v) => match serde_json::from_value::<Notices>(v.clone()) {
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
                if let Some(n) = notices {
                    a.set_notices(&device.id, door, n);
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
        ("GET", path) => {
            let file = match path.trim_start_matches('/') {
                "" => "index.html",
                p => p,
            };
            match Page::get(file) {
                Some(f) => (
                    Reply {
                        status: 200,
                        content_type: content_type(file),
                        body: f.data.into_owned(),
                    },
                    Effect::None,
                ),
                None => (Reply::not_found(), Effect::None),
            }
        }
        _ => (Reply::not_found(), Effect::None),
    }
}

#[derive(Deserialize)]
struct PairRequest {
    code: String,
    #[serde(default)]
    name: String,
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

/// Reads and writes one WebSocket until the phone leaves or the server stops.
/// Reads time out often so writes queued by the shell go out in between.
fn talk(
    stream: TcpStream,
    id: ClientId,
    device: String,
    out: std_mpsc::Receiver<String>,
    tx: &mpsc::UnboundedSender<Incoming>,
    ended: impl Fn() -> bool,
) {
    let _ = stream.set_read_timeout(Some(Duration::from_millis(20)));
    let _ = stream.set_write_timeout(Some(PHONE_WRITE_TIMEOUT));
    // The client learns here that what it numbers is acknowledged. The
    // callback's error type is tungstenite's to choose.
    #[allow(clippy::result_large_err)]
    let announce = |_: &tungstenite::handshake::server::Request,
                    mut res: tungstenite::handshake::server::Response| {
        res.headers_mut().insert(
            wire::ACKS_HEADER,
            tungstenite::http::HeaderValue::from_static("1"),
        );
        Ok(res)
    };
    let mut ws: WebSocket<TcpStream> = match tungstenite::accept_hdr(stream, announce) {
        Ok(ws) => ws,
        Err(e) => {
            eprintln!("arakun: phone handshake: {e}");
            return;
        }
    };
    if tx.unbounded_send(Incoming::Connected(id, device)).is_err() {
        return;
    }
    let mut last_heard = Quiet::now();
    let mut last_ping = Quiet::now();
    loop {
        if ended() {
            let _ = ws.close(None);
            let _ = ws.flush();
            return;
        }
        // Judged before reading: input buffered while the computer slept
        // would otherwise pass for a phone that is still there.
        // Browsers answer pings themselves, even for a page in the background.
        if last_heard.elapsed() > PHONE_SILENCE_MAX {
            return;
        }
        let read = ws.read();
        if read.is_ok() {
            // Judged again: the computer can sleep while `read` waits, and
            // what it returns on waking may have sat in a buffer all along.
            if last_heard.elapsed() > PHONE_SILENCE_MAX {
                return;
            }
            last_heard = Quiet::now();
        }
        match read {
            Ok(Message::Text(text)) => {
                let msg = serde_json::from_str::<serde_json::Value>(text.as_str());
                let seq = msg.as_ref().ok().and_then(wire::seq);
                match msg.and_then(serde_json::from_value::<ClientMsg>) {
                    // The page's own ping: it cannot see ours, so it hears it is still here.
                    Ok(ClientMsg::Ping) => {
                        if ws.send(Message::text(wire::pong())).is_err() {
                            return;
                        }
                    }
                    Ok(msg) => {
                        if tx.unbounded_send(Incoming::Message(id, msg)).is_err() {
                            return;
                        }
                    }
                    Err(e) => {
                        let _ = ws.send(Message::text(wire::error(&format!("bad message: {e}"))));
                    }
                }
                // A bad message was read all the same: sending it again would not help.
                if let Some(seq) = seq
                    && ws.send(Message::text(wire::ack(seq))).is_err()
                {
                    return;
                }
            }
            Ok(Message::Close(_)) => {
                // tungstenite queued the answering close frame.
                let _ = ws.flush();
                return;
            }
            Ok(_) => {}
            Err(tungstenite::Error::Io(e))
                if matches!(
                    e.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) => {}
            Err(_) => return,
        }
        if last_ping.elapsed() > PHONE_PING_EVERY {
            last_ping = Quiet::now();
            if ws.send(Message::Ping(Vec::new().into())).is_err() {
                return;
            }
        }
        let pass_ends = Instant::now() + PHONE_SEND_PASS;
        loop {
            if ended() || Instant::now() > pass_ends {
                break;
            }
            match out.try_recv() {
                Ok(text) => {
                    if ws.send(Message::text(text)).is_err() {
                        return;
                    }
                }
                Err(std_mpsc::TryRecvError::Empty) => break,
                Err(std_mpsc::TryRecvError::Disconnected) => {
                    let _ = ws.close(None);
                    let _ = ws.flush();
                    return;
                }
            }
        }
        // Pongs tungstenite queued while reading.
        let _ = ws.flush();
    }
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
    authorization: Option<String>,
    content_length: usize,
    /// The device that asked, when the request came through the relay: the
    /// relay vouched for it, so it brings no token.
    relay_device: Option<String>,
}

impl Head {
    /// A request that came through an encrypted channel, from the device the
    /// relay brought in with this id: `target` is the path with its query.
    pub fn relayed(method: &str, target: &str, device: &str) -> Option<Head> {
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
            authorization: None,
            content_length: 0,
            relay_device: Some(device.to_string()),
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
            authorization: None,
            content_length: 0,
            relay_device: None,
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
                "authorization" => h.authorization = Some(value.to_string()),
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
        if self.relay_device.is_some() {
            Door::Relay
        } else {
            Door::Local
        }
    }

    /// The device asking: the one the relay brought in, or the one whose
    /// token comes as `Authorization: Bearer <token>`. Never both: a relayed
    /// request carries no token.
    fn device(&self, access: &mut Access, now: u64) -> Option<Device> {
        match &self.relay_device {
            Some(id) => access.seen(id, now),
            None => access.device_for(self.bearer()?, now),
        }
    }

    /// The token of `Authorization: Bearer <token>`.
    fn bearer(&self) -> Option<&str> {
        let auth = self.authorization.as_deref()?;
        let (scheme, token) = auth.split_once(' ')?;
        scheme.eq_ignore_ascii_case("bearer").then(|| token.trim())
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
        "HTTP/1.1 {status} {reason}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nCache-Control: no-cache\r\nConnection: close\r\n\r\n",
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

/// The link the QR code carries while pairing: the page, with the code to bring.
pub fn link(address: &str, code: &str) -> String {
    format!("{address}?p={code}")
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

    #[test]
    fn keys_are_made_once_and_saved_the_way_the_caller_asks() {
        let mut settings = Settings::default();
        let mut saves = 0;
        let id = settings.identity_saving(|_| saves += 1);
        let vapid = settings.vapid_saving(|_| saves += 1);
        assert_eq!(saves, 2);
        assert!(settings.identity.is_some() && settings.vapid.is_some());
        assert_eq!(settings.identity_saving(|_| saves += 1).label(), id.label());
        assert_eq!(
            settings.vapid_saving(|_| saves += 1).public_key(),
            vapid.public_key()
        );
        assert_eq!(saves, 2, "keys that are kept are not saved again");
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
        let (server, _rx) = Server::start(Ipv4Addr::LOCALHOST, 0, access.clone()).unwrap();
        for auth in [
            "Authorization: Bearer \r\n",
            "Authorization: Bearer sess1\r\n",
            "",
        ] {
            let me = http(&server, "GET", "/me", auth, "");
            assert!(me.starts_with("HTTP/1.1 403"), "{auth:?}: {me}");
            let unpair = http(&server, "POST", "/unpair", auth, "");
            assert!(unpair.starts_with("HTTP/1.1 403"), "{auth:?}: {unpair}");
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
        assert!(a.seen("sess1", 5).is_none());

        let (made, seen) = a.relay_device("sess1", "Rik's iPhone", 5);
        assert_eq!(seen, Seen::New);
        assert!(made.token_hash.is_empty());
        assert_eq!(a.devices[0], lan, "the network's device is left alone");
        assert_eq!(a.seen("sess1", 6).unwrap().name, "Rik's iPhone");

        let (_, seen) = a.relay_device("sess1", "Rik's iPhone", 7);
        assert_eq!(seen, Seen::Known);
        let (renamed, seen) = a.relay_device("sess1", " Rik's new iPhone ", 8);
        assert_eq!(seen, Seen::Renamed);
        assert_eq!(renamed.name, "Rik's new iPhone");
        assert_eq!(a.devices.len(), 2);
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

    fn body_of(response: &str) -> serde_json::Value {
        serde_json::from_str(response.split("\r\n\r\n").nth(1).unwrap_or("null")).unwrap()
    }

    /// The body a browser would send to `/push`: a fresh P-256 key and auth secret.
    fn browser_subscription(endpoint: &str) -> String {
        use base64::Engine as _;
        let b64 = base64::engine::general_purpose::URL_SAFE_NO_PAD;
        let rng = ring::rand::SystemRandom::new();
        let private =
            ring::agreement::EphemeralPrivateKey::generate(&ring::agreement::ECDH_P256, &rng)
                .unwrap();
        let public = private.compute_public_key().unwrap();
        let mut auth = [0u8; 16];
        ring::rand::SecureRandom::fill(&rng, &mut auth).unwrap();
        serde_json::json!({
            "subscription": { "endpoint": endpoint, "keys": { "p256dh": b64.encode(public.as_ref()), "auth": b64.encode(auth) } }
        })
        .to_string()
    }

    /// A device paired through the server, and its token.
    fn pair(server: &Server, access: &Shared, name: &str) -> String {
        let code = access.lock().unwrap().begin_pairing();
        let res = http(
            server,
            "POST",
            "/pair",
            "",
            &format!(r#"{{"code":"{code}","name":"{name}"}}"#),
        );
        assert!(res.starts_with("HTTP/1.1 200"), "{res}");
        body_of(&res)["token"].as_str().unwrap().to_string()
    }

    #[test]
    fn serves_the_page_and_refuses_sockets_without_a_token() {
        let access = Access::new(Vec::new(), "pk".into());
        let (server, _rx) = Server::start(Ipv4Addr::LOCALHOST, 0, access).unwrap();
        let index = http(&server, "GET", "/", "", "");
        assert!(index.starts_with("HTTP/1.1 200"), "{index}");
        assert!(index.contains("text/html"));
        assert!(http(&server, "GET", "/nope.txt", "", "").starts_with("HTTP/1.1 404"));
        let refused = http(
            &server,
            "GET",
            "/ws?t=wrong",
            "Upgrade: websocket\r\nConnection: Upgrade\r\n",
            "",
        );
        assert!(refused.starts_with("HTTP/1.1 403"), "{refused}");
        assert!(
            http(&server, "GET", "/me", "Authorization: Bearer nope\r\n", "")
                .starts_with("HTTP/1.1 403")
        );
    }

    #[test]
    fn images_come_only_through_signed_links() {
        let dir = std::env::temp_dir().join(format!("arakun-phone-img-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let shot = dir.join("my shot.png");
        std::fs::write(&shot, b"PNG-ish bytes").unwrap();
        std::fs::write(dir.join("notes.txt"), b"text").unwrap();
        let access = Access::new(Vec::new(), "pk".into());
        let (server, _rx) = Server::start(Ipv4Addr::LOCALHOST, 0, access.clone()).unwrap();
        let key = access.lock().unwrap().image_key.clone();

        let ok = http(&server, "GET", &image_link(&key, &shot), "", "");
        assert!(ok.starts_with("HTTP/1.1 200"), "{ok}");
        assert!(ok.contains("Content-Type: image/png"));
        assert!(ok.ends_with("PNG-ish bytes"));

        // A wrong key, a changed path, or a file that is not an image: nothing.
        let forged = image_link("other-key", &shot);
        assert!(http(&server, "GET", &forged, "", "").starts_with("HTTP/1.1 404"));
        let moved = image_link(&key, &shot).replace("my%20shot", "my%20shot2");
        assert!(http(&server, "GET", &moved, "", "").starts_with("HTTP/1.1 404"));
        let text = image_link(&key, &dir.join("notes.txt"));
        assert!(http(&server, "GET", &text, "", "").starts_with("HTTP/1.1 404"));
        assert!(http(&server, "GET", "/image?p=x", "", "").starts_with("HTTP/1.1 404"));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn pairing_takes_the_code_once_and_gives_up_after_wrong_tries() {
        let access = Access::new(Vec::new(), "pk".into());
        let (server, mut rx) = Server::start(Ipv4Addr::LOCALHOST, 0, access.clone()).unwrap();
        // No code yet: nothing to bring.
        assert!(
            http(&server, "POST", "/pair", "", r#"{"code":"000000"}"#).starts_with("HTTP/1.1 403")
        );

        let token = pair(&server, &access, "Bob's phone");
        assert_eq!(token.len(), 64);
        assert!(matches!(
            futures::executor::block_on(futures::StreamExt::next(&mut rx)),
            Some(Incoming::Paired)
        ));
        let device = access.lock().unwrap().devices[0].clone();
        assert_eq!(device.name, "Bob's phone");
        assert_ne!(device.token_hash, token, "the token is kept hashed");
        // The code was spent.
        assert!(access.lock().unwrap().pairing().is_none());

        let me = http(
            &server,
            "GET",
            "/me",
            &format!("Authorization: Bearer {token}\r\n"),
            "",
        );
        assert!(me.starts_with("HTTP/1.1 200"), "{me}");
        assert_eq!(body_of(&me)["id"], device.id);
        assert_eq!(body_of(&me)["push_key"], "pk");
        assert_eq!(body_of(&me)["notifying"], false);

        // The phone turns notifications on with its subscription, and off with null.
        let auth = format!("Authorization: Bearer {token}\r\n");
        let sub = browser_subscription("https://push.example/x");
        let on = http(&server, "POST", "/push", &auth, &sub);
        assert!(on.starts_with("HTTP/1.1 200"), "{on}");
        assert!(matches!(
            futures::executor::block_on(futures::StreamExt::next(&mut rx)),
            Some(Incoming::Subscribed)
        ));
        assert_eq!(
            access.lock().unwrap().devices[0]
                .push
                .as_ref()
                .map(|s| s.endpoint.as_str()),
            Some("https://push.example/x")
        );
        assert_eq!(
            body_of(&http(&server, "GET", "/me", &auth, ""))["notifying"],
            true
        );
        assert!(
            http(
                &server,
                "POST",
                "/push",
                &auth,
                r#"{"subscription":{"endpoint":"https://x/","keys":{}}}"#
            )
            .starts_with("HTTP/1.1 400")
        );
        assert!(http(&server, "POST", "/push", "", &sub).starts_with("HTTP/1.1 403"));

        // It picks what to hear about; the subscription stays as it was.
        assert_eq!(
            body_of(&me)["notices"],
            serde_json::json!({ "done": true, "input": true, "error": true })
        );
        let picked = http(
            &server,
            "POST",
            "/push",
            &auth,
            r#"{"notices":{"done":false,"input":true,"error":true}}"#,
        );
        assert!(picked.starts_with("HTTP/1.1 200"), "{picked}");
        assert_eq!(body_of(&picked)["notifying"], true);
        assert_eq!(body_of(&picked)["notices"]["done"], false);
        assert_eq!(
            access.lock().unwrap().devices[0].notices,
            Notices {
                done: false,
                input: true,
                error: true
            }
        );
        assert!(
            http(
                &server,
                "POST",
                "/push",
                &auth,
                r#"{"notices":{"done":"yes"}}"#
            )
            .starts_with("HTTP/1.1 400")
        );

        let off = http(&server, "POST", "/push", &auth, r#"{"subscription":null}"#);
        assert!(off.starts_with("HTTP/1.1 200"), "{off}");
        assert!(access.lock().unwrap().devices[0].push.is_none());
        assert!(
            !access.lock().unwrap().devices[0].notices.done,
            "the choice outlives the subscription"
        );

        // A wrong code, enough times, throws the current one away.
        let code = access.lock().unwrap().begin_pairing();
        let wrong = if code == "111111" { "222222" } else { "111111" };
        for _ in 0..PAIR_TRIES {
            assert!(
                http(
                    &server,
                    "POST",
                    "/pair",
                    "",
                    &format!(r#"{{"code":"{wrong}"}}"#)
                )
                .starts_with("HTTP/1.1 403")
            );
        }
        assert!(
            http(
                &server,
                "POST",
                "/pair",
                "",
                &format!(r#"{{"code":"{code}"}}"#)
            )
            .starts_with("HTTP/1.1 403")
        );
        assert_eq!(access.lock().unwrap().devices.len(), 1);
    }

    /// A connected phone's socket and its id on the server.
    fn connect_phone(
        server: &Server,
        access: &Shared,
        rx: &mut mpsc::UnboundedReceiver<Incoming>,
    ) -> (
        WebSocket<tungstenite::stream::MaybeTlsStream<TcpStream>>,
        ClientId,
    ) {
        let token = pair(server, access, "p");
        let url = format!("ws://127.0.0.1:{}/ws?t={token}", server.port());
        let (ws, _) = tungstenite::connect(url).unwrap();
        loop {
            match futures::executor::block_on(futures::StreamExt::next(rx)) {
                Some(Incoming::Connected(id, _)) => return (ws, id),
                Some(_) => {}
                None => panic!("the server stopped"),
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
        let (server, mut rx) = Server::start(Ipv4Addr::LOCALHOST, 0, access.clone()).unwrap();
        let (mut ws, _) = connect_phone(&server, &access, &mut rx);
        ws.send(Message::text(r#"{"type":"ping"}"#)).unwrap();
        loop {
            match ws.read().unwrap() {
                Message::Text(t) => {
                    assert_eq!(t.as_str(), wire::pong());
                    break;
                }
                Message::Ping(_) | Message::Pong(_) => {}
                other => panic!("unexpected {other:?}"),
            }
        }
    }

    #[test]
    fn a_phone_that_answers_pings_stays() {
        let access = Access::new(Vec::new(), "pk".into());
        let (server, mut rx) = Server::start(Ipv4Addr::LOCALHOST, 0, access.clone()).unwrap();
        let (mut ws, id) = connect_phone(&server, &access, &mut rx);
        // Reading answers the server's pings; nothing else is said.
        let tungstenite::stream::MaybeTlsStream::Plain(tcp) = ws.get_ref() else {
            unreachable!("a plain socket");
        };
        tcp.set_read_timeout(Some(Duration::from_millis(50)))
            .unwrap();
        let until = Instant::now() + PHONE_SILENCE_MAX * 2;
        let mut pinged = false;
        while Instant::now() < until {
            match ws.read() {
                Ok(Message::Ping(_)) => pinged = true,
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
        let (server, mut rx) = Server::start(Ipv4Addr::LOCALHOST, 0, access.clone()).unwrap();
        // Never read again, so the server's pings go unanswered.
        let (_ws, id) = connect_phone(&server, &access, &mut rx);
        disconnected(&mut rx, id, PHONE_SILENCE_MAX * 5);
        assert_eq!(server.device_of(id), None);
    }

    #[test]
    fn a_phone_whose_queue_fills_is_dropped() {
        let (tx, _rx) = std_mpsc::sync_channel(1);
        let server = Server {
            port: 0,
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
        let (server, mut rx) = Server::start(Ipv4Addr::LOCALHOST, 0, access.clone()).unwrap();
        let token = pair(&server, &access, "p");
        let url = format!("ws://127.0.0.1:{}/ws?t={token}", server.port());
        let (mut ws, _) = tungstenite::connect(url).unwrap();
        ws.send(Message::text(r#"{"type":"open","session":"s1"}"#))
            .unwrap();
        ws.send(Message::text(r#"{"type":"visible","visible":false}"#))
            .unwrap();
        let mut got = Vec::new();
        let device = access.lock().unwrap().devices[0].id.clone();
        while got.len() < 4 {
            match futures::executor::block_on(futures::StreamExt::next(&mut rx)) {
                Some(Incoming::Paired) => got.push("paired"),
                Some(Incoming::Connected(id, dev)) => {
                    assert_eq!(dev, device);
                    server.send(id, "hello".into());
                    got.push("connected");
                }
                Some(Incoming::Message(_, ClientMsg::Open { session, .. })) => {
                    assert_eq!(session, "s1");
                    got.push("open");
                }
                Some(Incoming::Message(_, ClientMsg::Visible { visible: false })) => {
                    got.push("hidden")
                }
                other => panic!("unexpected {}", other.is_some()),
            }
        }
        assert_eq!(ws.read().unwrap(), Message::text("hello"));
        server.broadcast("all");
        assert_eq!(ws.read().unwrap(), Message::text("all"));
        assert_eq!(server.connected_devices(), vec![device.clone()]);

        access.lock().unwrap().unpair(&device);
        server.disconnect_device(&device);
        assert!(matches!(ws.read(), Ok(Message::Close(_)) | Err(_)));
        let refused = http(
            &server,
            "GET",
            &format!("/ws?t={token}"),
            "Upgrade: websocket\r\nConnection: Upgrade\r\n",
            "",
        );
        assert!(refused.starts_with("HTTP/1.1 403"), "{refused}");
    }

    #[test]
    fn a_phone_unpairs_itself() {
        let access = Access::new(Vec::new(), "pk".into());
        let (server, mut rx) = Server::start(Ipv4Addr::LOCALHOST, 0, access.clone()).unwrap();
        let token = pair(&server, &access, "p");
        let (mut ws, _) =
            tungstenite::connect(format!("ws://127.0.0.1:{}/ws?t={token}", server.port())).unwrap();
        let auth = format!("Authorization: Bearer {token}\r\n");

        assert!(http(&server, "POST", "/unpair", "", "").starts_with("HTTP/1.1 403"));
        let gone = http(&server, "POST", "/unpair", &auth, "");
        assert!(gone.starts_with("HTTP/1.1 200"), "{gone}");
        assert!(access.lock().unwrap().devices.is_empty());
        // Its socket closes, its token stops working, and the desktop hears that someone left.
        assert!(matches!(ws.read(), Ok(Message::Close(_)) | Err(_)));
        assert!(http(&server, "GET", "/me", &auth, "").starts_with("HTTP/1.1 403"));
        let mut heard = false;
        for _ in 0..4 {
            if let Some(Incoming::Unpaired) =
                futures::executor::block_on(futures::StreamExt::next(&mut rx))
            {
                heard = true;
                break;
            }
        }
        assert!(heard);
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
