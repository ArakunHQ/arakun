//! The server itself: a windowless GPUI app that owns the agent threads, the
//! way the shell does in the app. Paired devices reach it as phones reach the
//! app (`crate::phone`: the same pairing, transports and messages), and the
//! user on this machine reaches it through the local socket ([`local`]).
//!
//! A session's agent runs while it has work or someone is looking at it, and
//! is let go after, as the app does with a session whose tab is closed. Its
//! transcript is saved while it works, so a server that dies loses seconds.

use std::cell::Cell;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use futures::StreamExt as _;
use futures::channel::mpsc;
use gpui::{App, AppContext as _, Context, Entity, Global, Subscription, Task};
use serde_json::json;

use super::local::{self, Reply, Request};
use super::store::{self, Place, Record, Store};
use crate::account::Account;
use crate::agent::log::render_log;
use crate::agent::{
    self, AgentKind, Entry, Setting, SettingOption, Thread, ThreadEvent, ThreadStatus, Transcript,
    catalog, workers_ai,
};
use crate::model::{Status, unix_now};
use crate::phone::relay::{self, RelayClient};
use crate::phone::{
    self, Access, ClientId, ClientMsg, Incoming, Reach, Server, Transport, push, tailscale, wire,
};

/// Entry changes wait this long for more before they go out, so a streaming
/// agent costs a few messages a second, not one per token.
const GATHER: Duration = Duration::from_millis(90);
const TICK: Duration = Duration::from_secs(1);
/// How often a working session's transcript is saved.
const SAVE_EVERY: Duration = Duration::from_secs(2);
/// How often Tailscale is asked again: quickly while something is in the way
/// and the user is probably fixing it, then slowly; and now and then once the
/// way is open, in case it closes.
const TAILSCALE_RETRY: Duration = Duration::from_secs(3);
const TAILSCALE_RETRY_LATER: Duration = Duration::from_secs(15);
const TAILSCALE_QUICK_TRIES: u32 = 20;
const TAILSCALE_RECHECK: Duration = Duration::from_secs(60);

/// The settings a device can pick: the model, then the reasoning (its choices
/// depend on the model).
const DEVICE_SETTINGS: [Setting; 2] = [Setting::Model, Setting::Effort];

/// What `arakun server serve` was started with; what is left out stays as the
/// last run had it.
#[derive(Debug, Default, PartialEq)]
pub struct Options {
    pub reach: Option<Reach>,
    pub port: Option<u16>,
    pub relay: Option<String>,
}

/// Where devices find the server, as last worked out.
#[derive(Clone, Debug, PartialEq)]
enum Address {
    Pending(String),
    Ready(String),
    Problem(String),
}

impl Address {
    fn line(&self) -> String {
        match self {
            Address::Ready(url) => url.clone(),
            Address::Pending(what) => format!("not yet: {what}"),
            Address::Problem(what) => format!("unreachable: {what}"),
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Notice {
    Finished,
    Input,
    Error,
}

struct Live {
    thread: Entity<Thread>,
    /// Something changed since the transcript was last saved.
    dirty: bool,
    saved: Instant,
    _events: Subscription,
}

struct Keep(#[allow(dead_code)] Entity<Daemon>);
impl Global for Keep {}

pub struct Daemon {
    store: Store,
    /// How the server is reached and who is paired, as last saved. Kept here
    /// and never read back: a file gone bad under a running server must not
    /// become the defaults at the next save.
    settings: phone::Settings,
    live: HashMap<String, Live>,
    reach: Reach,
    transport: Transport,
    /// The paired devices and the pairing code, shared with the transport's threads.
    access: phone::Shared,
    /// Signs the push notifications this server sends.
    vapid: Arc<push::Vapid>,
    address: Address,
    /// What the log last said about how the server is reached.
    told: String,
    account: Entity<Account>,
    /// The session each device is looking at.
    watching: HashMap<ClientId, String>,
    /// Devices whose page left the screen: they still get updates, but a push too.
    hidden: HashSet<ClientId>,
    /// Sessions whose entries changed since the last flush, with the lowest index that did.
    dirty: HashMap<String, usize>,
    /// Sessions whose status, permission or usage changed since the last flush.
    state_dirty: Vec<String>,
    inbox_dirty: bool,
    flush: Option<Task<()>>,
    last_inbox: String,
    last_usage: String,
    uploads: phone::Uploads,
    /// The last thing each session notified about, so one turn says it once.
    notified: HashMap<String, Notice>,
    writer: Writer,
    code: Rc<Cell<i32>>,
    stopping: bool,
    /// The local socket, until the server stops.
    listening: Option<local::Listening>,
    _tasks: Vec<Task<()>>,
    _account: Subscription,
}

/// Transcripts are written on a thread of their own, in the order they were
/// handed over: a save never holds up the agents, and an older one never
/// lands over a newer one. A save that fails (a full disk) is kept and tried
/// again with the next job, so the newest state of a session is never dropped
/// while the file still holds an older one.
struct Writer {
    tx: Option<std::sync::mpsc::Sender<Job>>,
    thread: Option<std::thread::JoinHandle<()>>,
    /// The files still owed a write, each with the number of its newest save.
    /// The worker takes a file out when that save is on disk, so a read only
    /// waits while its own file is queued or failed.
    owed: Owed,
    saves: Cell<u64>,
}

type Owed = Arc<std::sync::Mutex<HashMap<PathBuf, u64>>>;

enum Job {
    // Boxed: a transcript is far bigger than the rest, and most jobs wait in a queue.
    Save(PathBuf, u64, Box<Transcript>),
    /// The session is gone: nothing kept for this file is written any more.
    Forget(PathBuf),
    /// Answered once every save handed over before it was tried, with the
    /// files that could not be written.
    Flush(std::sync::mpsc::Sender<Vec<PathBuf>>),
}

/// A transcript to write, and which save of its file it is.
type Snapshot = (PathBuf, u64, Box<Transcript>);

fn write_transcript(path: &Path, transcript: &Transcript) -> std::io::Result<()> {
    let bytes = serde_json::to_vec(transcript).map_err(std::io::Error::other)?;
    crate::durable::write_private(path, &bytes)
}

/// Writes `waiting`. What is written is no longer owed, unless a newer save
/// of the file was handed over meanwhile; what fails goes to `unsaved`. A
/// file already in `known` failed before, and said so then.
fn write_all(
    waiting: &mut Vec<Snapshot>,
    unsaved: &mut Vec<Snapshot>,
    known: &[PathBuf],
    owed: &Owed,
) {
    for (path, number, transcript) in waiting.drain(..) {
        match write_transcript(&path, &transcript) {
            Ok(()) => {
                let mut owed = owed.lock().unwrap();
                if owed.get(&path) == Some(&number) {
                    owed.remove(&path);
                }
            }
            Err(e) => {
                if !known.contains(&path) {
                    eprintln!(
                        "arakun server: could not save {}: {e}; it is kept and tried again",
                        path.display()
                    );
                }
                unsaved.push((path, number, transcript));
            }
        }
    }
}

impl Writer {
    /// Fails when the thread cannot be started: a server that could not
    /// save transcripts would lose every session it let go of.
    fn start() -> std::io::Result<Writer> {
        let (tx, rx) = std::sync::mpsc::channel::<Job>();
        let owed = Owed::default();
        let theirs = owed.clone();
        let thread = std::thread::Builder::new()
            .name("arakun-server-save".into())
            .spawn(move || {
                let owed = theirs;
                // Snapshots whose write failed, for the next round.
                let mut unsaved: Vec<Snapshot> = Vec::new();
                while let Ok(first) = rx.recv() {
                    // What failed last time goes first: anything newer for the
                    // same file replaces it below. Of the saves waiting, only
                    // the newest of each file is worth writing.
                    let mut waiting = std::mem::take(&mut unsaved);
                    let known: Vec<PathBuf> = waiting.iter().map(|s| s.0.clone()).collect();
                    for job in std::iter::once(first).chain(rx.try_iter()) {
                        match job {
                            Job::Save(path, number, transcript) => {
                                waiting.retain(|s| s.0 != path);
                                unsaved.retain(|s| s.0 != path);
                                waiting.push((path, number, transcript));
                            }
                            Job::Forget(path) => {
                                waiting.retain(|s| s.0 != path);
                                unsaved.retain(|s| s.0 != path);
                            }
                            Job::Flush(done) => {
                                write_all(&mut waiting, &mut unsaved, &known, &owed);
                                let _ = done.send(unsaved.iter().map(|s| s.0.clone()).collect());
                            }
                        }
                    }
                    write_all(&mut waiting, &mut unsaved, &known, &owed);
                }
            })?;
        Ok(Writer {
            tx: Some(tx),
            thread: Some(thread),
            owed,
            saves: Cell::new(0),
        })
    }

    fn save(&self, path: PathBuf, transcript: Transcript) {
        if let Some(tx) = &self.tx {
            let number = self.saves.get() + 1;
            self.saves.set(number);
            self.owed.lock().unwrap().insert(path.clone(), number);
            let _ = tx.send(Job::Save(path, number, Box::new(transcript)));
        }
    }

    /// Drops what is kept for a file whose session was removed, and waits
    /// for a write of it that may have begun: the file can be deleted after.
    fn forget(&self, path: PathBuf) {
        let Some(tx) = &self.tx else { return };
        let was_owed = self.owed.lock().unwrap().remove(&path).is_some();
        let _ = tx.send(Job::Forget(path));
        if was_owed {
            self.flush();
        }
    }

    /// Whether the disk holds an older transcript at `path` than the
    /// session's last: its save failed and is being kept. A file whose save
    /// is on disk is not owed, and the answer comes at once. Only a save of
    /// this very file that is still queued is waited for; it is written in
    /// its turn, after the ones handed over before it.
    fn behind(&self, path: &Path) -> bool {
        if !self.owed.lock().unwrap().contains_key(path) {
            return false;
        }
        self.flush().iter().any(|p| p == path)
    }

    /// Waits until every save handed over so far was tried, and returns the
    /// files that could not be written: for those the disk still holds an
    /// older transcript than the session's last.
    fn flush(&self) -> Vec<PathBuf> {
        let (done, wait) = std::sync::mpsc::channel();
        match &self.tx {
            Some(tx) if tx.send(Job::Flush(done)).is_ok() => wait.recv().unwrap_or_default(),
            _ => Vec::new(),
        }
    }

    /// Writes what is waiting and stops.
    fn finish(&mut self) {
        self.tx = None;
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// `arakun server serve`: runs until a signal or `arakun server quit`.
pub fn run(opts: Options) -> i32 {
    let dir = super::dir();
    if let Err(e) = make_private_dir(&dir) {
        eprintln!("arakun server: {}: {e}", dir.display());
        return 1;
    }
    // Before anything listens: a server that cannot read what it kept does not start.
    let store = match Store::load(&dir) {
        Ok(store) => store,
        Err(e) => {
            eprintln!("arakun server: {e}");
            return 1;
        }
    };
    phone::use_settings_file(dir.join("phone.json"));
    // No file is a first run. A file that cannot be read is not: starting on
    // the defaults would listen on the plain network and forget the devices.
    let (mut settings, first_run) = match phone::Settings::read() {
        Ok(Some(settings)) => (settings, false),
        Ok(None) => (phone::Settings::default(), true),
        Err(e) => {
            eprintln!("arakun server: {e}");
            return 1;
        }
    };
    // A server is rarely on the same network as the devices that reach it, and
    // the relay is the one way in that is encrypted without anything to set up.
    if first_run {
        settings.reach = Reach::Relay;
    }
    if let Some(reach) = opts.reach {
        settings.reach = reach;
    }
    if let Some(port) = opts.port {
        settings.port = port;
    }
    if let Some(relay) = opts.relay {
        settings.relay_base = relay;
    }
    settings.enabled = true;
    settings.save();

    let socket = super::socket_path();
    let (listening, requests) = match local::serve(&socket) {
        Ok(found) => found,
        Err(e) => {
            eprintln!("arakun server: {e}");
            return 1;
        }
    };
    let writer = match Writer::start() {
        Ok(writer) => writer,
        Err(e) => {
            eprintln!("arakun server: could not start the thread that saves transcripts: {e}");
            return 1;
        }
    };
    let vapid = Arc::new(settings.vapid());
    let access = Access::new(settings.devices.clone(), vapid.public_key());
    let (transport, incoming, address) = match settings.reach {
        Reach::Lan | Reach::Tailscale => {
            match Server::start(settings.reach.bind(), settings.port, access.clone()) {
                Ok((server, rx)) => {
                    let address = match settings.reach {
                        Reach::Lan => lan_address(server.port()),
                        _ => Address::Pending("asking Tailscale".into()),
                    };
                    (Transport::Local(server), rx, address)
                }
                Err(e) => {
                    eprintln!(
                        "arakun server: could not listen on port {}: {e}",
                        settings.port
                    );
                    return 1;
                }
            }
        }
        Reach::Relay => {
            let identity = settings.identity();
            let (client, rx) = RelayClient::start(&settings.relay_base, identity, access.clone());
            (
                Transport::Relay(client),
                rx,
                Address::Pending("connecting to the relay".into()),
            )
        }
    };
    println!(
        "arakun server {} on {}",
        env!("CARGO_PKG_VERSION"),
        phone::host_name()
    );
    println!("  data    {}", dir.display());
    println!("  socket  {}", socket.display());
    println!(
        "  reach   {}",
        reach_line(settings.reach, &transport, &address)
    );
    println!("Pair a device with `{} pair`.", super::name());

    let code = Rc::new(Cell::new(0));
    let out = code.clone();
    let reach = settings.reach;
    gpui_platform::headless().run(move |cx: &mut App| {
        let daemon = cx.new(|cx| {
            Daemon::new(
                Parts {
                    store,
                    settings,
                    reach,
                    transport,
                    access,
                    vapid,
                    address,
                    listening,
                    writer,
                    code: out,
                },
                incoming,
                requests,
                cx,
            )
        });
        cx.set_global(Keep(daemon));
    });
    code.get()
}

/// The folder holds prompts, transcripts and the keys devices are let in
/// with: only its user gets in.
fn make_private_dir(dir: &Path) -> std::io::Result<()> {
    let mut builder = std::fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt as _;
        builder.mode(0o700);
    }
    builder.create(dir)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

fn lan_address(port: u16) -> Address {
    match phone::lan_ip() {
        Some(ip) => Address::Ready(phone::lan_address(ip, port)),
        None => Address::Problem("could not tell this machine's address on the network".into()),
    }
}

fn reach_name(reach: Reach) -> &'static str {
    match reach {
        Reach::Lan => "lan",
        Reach::Tailscale => "tailscale",
        Reach::Relay => "relay",
    }
}

fn reach_line(reach: Reach, transport: &Transport, address: &Address) -> String {
    match (transport, address) {
        // The relay address stands whether or not we are connected to it right now.
        (Transport::Relay(client), Address::Pending(what) | Address::Problem(what)) => {
            format!("{}  {}  ({what})", reach_name(reach), client.page_url())
        }
        _ => format!("{}  {}", reach_name(reach), address.line()),
    }
}

fn relay_address(status: relay::Status, page: &str) -> Address {
    match status {
        relay::Status::Connecting => Address::Pending("connecting to the relay".into()),
        relay::Status::Connected => Address::Ready(page.to_string()),
        relay::Status::Reconnecting { why, retry_in } => Address::Pending(format!(
            "{why} Trying again in {} s.",
            retry_in.as_secs().max(1)
        )),
        relay::Status::Rejected(why) => Address::Problem(why),
    }
}

/// Where devices find the server at `port` through Tailscale right now, with
/// `serve` pointed at it when Tailscale can; otherwise what stands in the way.
/// Runs processes: for a background thread.
fn tailscale_address(port: u16) -> Address {
    match tailscale::status() {
        tailscale::Status::Ready { name } => match tailscale::serve(port) {
            Ok(()) => Address::Ready(tailscale::url(&name)),
            Err(e) => Address::Problem(format!("Tailscale could not serve it: {e}")),
        },
        tailscale::Status::Missing => {
            Address::Problem("Tailscale is not installed on this machine".into())
        }
        tailscale::Status::Down => Address::Problem(
            "Tailscale is not connected; `tailscale up` and this picks it up on its own".into(),
        ),
        tailscale::Status::NoHttps => Address::Problem(format!(
            "your tailnet does not issue HTTPS certificates yet; turn them on at {}",
            tailscale::ADMIN_DNS_URL
        )),
    }
}

/// What a session is doing. A prompt still waiting for the agent to connect
/// already counts as work: nobody is looking at a window here to see that it
/// is on its way, and an agent that reads as idle is let go.
fn status_of(t: &Thread) -> Status {
    match t.status() {
        ThreadStatus::NeedsInput => Status::NeedsInput,
        ThreadStatus::Working => Status::Working,
        ThreadStatus::Error(_) => Status::Error,
        ThreadStatus::Connecting | ThreadStatus::Idle
            if !t.queued().is_empty() && !t.queue_paused() =>
        {
            Status::Working
        }
        ThreadStatus::Connecting | ThreadStatus::Idle => Status::Idle,
    }
}

/// The first line of a prompt, cut to a title's length.
fn summary(text: &str) -> String {
    let text = agent::attach::title_text(text);
    let line = text
        .lines()
        .find(|l| !l.trim().is_empty())
        .unwrap_or("")
        .trim();
    agent::log::head(line, 80)
}

fn error_text(t: &Thread) -> String {
    match t.status() {
        ThreadStatus::Error(msg) => {
            let line = msg.lines().next().unwrap_or("").trim();
            if line.is_empty() {
                "Stopped with an error.".into()
            } else {
                agent::log::head(line, 160)
            }
        }
        _ => "Stopped with an error.".into(),
    }
}

fn last_message(entries: &[Entry]) -> String {
    entries
        .iter()
        .rev()
        .find_map(|e| match e {
            Entry::Agent { text } if !text.trim().is_empty() => Some(text.trim().to_string()),
            _ => None,
        })
        .unwrap_or_default()
}

/// What the last failed turn left in the transcript to say why.
fn last_error(entries: &[Entry]) -> String {
    entries
        .iter()
        .rev()
        .find_map(|e| match e {
            Entry::Notice {
                text, error: true, ..
            } => Some(text.trim().to_string()),
            _ => None,
        })
        .unwrap_or_else(|| "Stopped with an error.".into())
}

/// A photo that came with a prompt, saved where the agent can read it.
fn save_upload(dir: &Path, bytes: &[u8], ext: &str) -> std::io::Result<PathBuf> {
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis());
    let mut path = dir.join(format!("photo-{stamp}.{ext}"));
    let mut n = 1;
    while path.exists() {
        path = dir.join(format!("photo-{stamp}-{n}.{ext}"));
        n += 1;
    }
    crate::durable::write_private(&path, bytes)?;
    Ok(path)
}

/// What [`Daemon::new`] is handed, made before the app's loop starts so a
/// port that is taken or a second server stops the command with a message.
struct Parts {
    store: Store,
    settings: phone::Settings,
    reach: Reach,
    transport: Transport,
    access: phone::Shared,
    vapid: Arc<push::Vapid>,
    address: Address,
    listening: local::Listening,
    writer: Writer,
    code: Rc<Cell<i32>>,
}

impl Daemon {
    fn new(
        parts: Parts,
        mut incoming: mpsc::UnboundedReceiver<Incoming>,
        mut requests: mpsc::UnboundedReceiver<local::Incoming>,
        cx: &mut Context<Self>,
    ) -> Self {
        let mut tasks = Vec::new();
        tasks.push(cx.spawn(async move |this, cx| {
            while let Some(msg) = incoming.next().await {
                if this.update(cx, |this, cx| this.incoming(msg, cx)).is_err() {
                    break;
                }
            }
        }));
        tasks.push(cx.spawn(async move |this, cx| {
            while let Some((req, done)) = requests.next().await {
                match this.update(cx, |this, cx| this.request(&req, cx)) {
                    Ok(reply) => {
                        let _ = done.send(reply);
                    }
                    Err(_) => break,
                }
            }
        }));
        tasks.push(cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(TICK).await;
                if this.update(cx, |this, cx| this.tick(cx)).is_err() {
                    break;
                }
            }
        }));
        if let Some(signal) = crate::signals::first_signal() {
            tasks.push(cx.spawn(async move |this, cx| {
                if signal.await.is_ok() {
                    let _ = this.update(cx, |this, cx| this.shut_down(cx));
                }
            }));
        }
        if let (Reach::Tailscale, Some(port)) = (parts.reach, parts.transport.port()) {
            tasks.push(Self::watch_tailscale(port, cx));
        }
        // The choices a new session offers: asked off the main thread, and only
        // when no earlier run cached them.
        // An agent that is not installed here has nothing to say, and would
        // only put its absence in the log.
        tasks.push(cx.background_spawn(async {
            if catalog::droid_native() && agent::droid_bin().is_ok() {
                catalog::refresh();
            }
            if agent::claude_cli().is_some() {
                catalog::refresh_claude();
            }
            if agent::which("opencode").is_some() {
                catalog::refresh_opencode();
            }
        }));
        let account = cx.new(Account::new);
        let account_events = cx.observe(&account, |this, _, cx| this.usage_changed(cx));
        Daemon {
            store: parts.store,
            settings: parts.settings,
            live: HashMap::new(),
            reach: parts.reach,
            transport: parts.transport,
            access: parts.access,
            vapid: parts.vapid,
            address: parts.address,
            told: String::new(),
            account,
            watching: HashMap::new(),
            hidden: HashSet::new(),
            dirty: HashMap::new(),
            state_dirty: Vec::new(),
            inbox_dirty: false,
            flush: None,
            last_inbox: String::new(),
            last_usage: String::new(),
            uploads: phone::Uploads::default(),
            notified: HashMap::new(),
            writer: parts.writer,
            code: parts.code,
            stopping: false,
            listening: Some(parts.listening),
            _tasks: tasks,
            _account: account_events,
        }
    }

    /// Keeps asking Tailscale where devices can find us, and keeps `serve`
    /// pointed at the server while it can.
    fn watch_tailscale(port: u16, cx: &mut Context<Self>) -> Task<()> {
        cx.spawn(async move |this, cx| {
            let mut tries = 0u32;
            loop {
                let found = cx
                    .background_executor()
                    .spawn(async move { tailscale_address(port) })
                    .await;
                let ready = matches!(found, Address::Ready(_));
                let news = match &found {
                    Address::Ready(url) => format!("reachable at {url}"),
                    Address::Pending(what) | Address::Problem(what) => what.clone(),
                };
                if this
                    .update(cx, |this, _| this.set_address(found, Some(news)))
                    .is_err()
                {
                    break;
                }
                tries = if ready { 0 } else { tries + 1 };
                let wait = if ready {
                    TAILSCALE_RECHECK
                } else if tries <= TAILSCALE_QUICK_TRIES {
                    TAILSCALE_RETRY
                } else {
                    TAILSCALE_RETRY_LATER
                };
                cx.background_executor().timer(wait).await;
            }
        })
    }

    /// Takes where the server is reached now. `news` goes to the log when it
    /// differs from the last: a relay that is away is tried again for as long
    /// as it takes, and each try is not news.
    fn set_address(&mut self, address: Address, news: Option<String>) {
        self.address = address;
        if let Some(news) = news
            && self.told != news
        {
            eprintln!("arakun server: {news}");
            self.told = news;
        }
    }

    // ---- sessions ---------------------------------------------------------------

    /// Starts a session of `agent` in the place at `path` with `text` as its
    /// first prompt, and returns its id.
    fn start_session(
        &mut self,
        path: &Path,
        agent: AgentKind,
        title: Option<String>,
        text: String,
        cx: &mut Context<Self>,
    ) -> String {
        let id = store::new_id();
        let now = unix_now();
        let named = title.filter(|t| !t.trim().is_empty());
        self.store.sessions.insert(
            0,
            Record {
                id: id.clone(),
                title: named.clone().unwrap_or_else(|| summary(&text)),
                agent,
                place: path.to_path_buf(),
                created: now,
                updated: now,
                added: 0,
                removed: 0,
                meta: None,
                failed: false,
                status: Status::Working,
            },
        );
        let picked = self
            .store
            .place(path)
            .map(|p| p.picked(agent))
            .unwrap_or_default();
        let thread = Thread::start(agent, path.to_path_buf(), None, cx);
        thread.update(cx, |t, cx| {
            // Queued until the agent's session exists, then applied before the prompt.
            for (s, value) in &picked {
                t.set_setting(*s, value, cx);
            }
            if let Some(title) = &named {
                t.rename(title, cx);
            }
            t.send(text, cx);
        });
        self.attach(&id, thread, cx);
        self.save_store();
        self.inbox_changed(cx);
        eprintln!(
            "arakun server: {id} started ({} in {})",
            agent.label(),
            path.display()
        );
        id
    }

    fn attach(&mut self, id: &str, thread: Entity<Thread>, cx: &mut Context<Self>) {
        let sid = id.to_string();
        let events = cx.subscribe(&thread, move |this, thread, ev: &ThreadEvent, cx| {
            this.thread_event(&sid, &thread, ev, cx)
        });
        self.live.insert(
            id.to_string(),
            Live {
                thread,
                dirty: false,
                saved: Instant::now(),
                _events: events,
            },
        );
    }

    /// The session's thread, its agent started again if it had been let go.
    fn ensure_live(&mut self, id: &str, cx: &mut Context<Self>) -> Result<Entity<Thread>, String> {
        if let Some(live) = self.live.get(id) {
            return Ok(live.thread.clone());
        }
        let record = self
            .store
            .session(id)
            .ok_or("that session is gone")?
            .clone();
        let transcript = self.store.transcript_path(id);
        // The save its agent was let go with may still be on its way. If it
        // could not be written, the file is older than the session: starting
        // from it would lose what came after, and then save over the proof.
        if self.writer.behind(&transcript) {
            return Err(format!(
                "the last turn of {id} could not be saved to {} (is the disk full?); nothing is sent to it until it can",
                transcript.display()
            ));
        }
        // A transcript that is there and cannot be read is an error too. A new
        // session in its place would lose the agent's own session to resume,
        // and its saves would end up over the file that could not be read.
        let thread = if transcript.exists() {
            Thread::restore(&transcript, cx).map_err(|e| {
                format!(
                    "the transcript of {id} could not be read ({e:#}); {} is left as it is",
                    transcript.display()
                )
            })?
        } else {
            Thread::start(record.agent, record.place, record.meta, cx)
        };
        self.attach(id, thread.clone(), cx);
        Ok(thread)
    }

    fn thread(&self, id: &str) -> Option<Entity<Thread>> {
        self.live.get(id).map(|l| l.thread.clone())
    }

    fn watched(&self, id: &str) -> bool {
        self.watching.values().any(|w| w == id)
    }

    fn thread_event(
        &mut self,
        id: &str,
        thread: &Entity<Thread>,
        ev: &ThreadEvent,
        cx: &mut Context<Self>,
    ) {
        if self.stopping {
            return;
        }
        if let Some(live) = self.live.get_mut(id) {
            live.dirty = true;
        }
        self.note_for_devices(id, ev, cx);
        self.notify(id, thread, ev, cx);
        match ev {
            ThreadEvent::TitleChanged => {
                let title = thread.read(cx).title();
                if let Some(s) = self.store.session_mut(id)
                    && !title.is_empty()
                    && s.title != title.as_ref()
                {
                    s.title = title.to_string();
                    self.save_store();
                }
            }
            ThreadEvent::StatusChanged | ThreadEvent::NeedsPermission => {
                let status = status_of(thread.read(cx));
                if let Some(s) = self.store.session_mut(id)
                    && s.status != status
                {
                    s.status = status;
                    s.updated = unix_now();
                }
            }
            ThreadEvent::ShellChanges => {
                let stats = thread.read(cx).diff_stats();
                if let Some(s) = self.store.session_mut(id) {
                    (s.added, s.removed) = stats;
                }
                self.save_transcript(id, cx);
                self.save_store();
            }
            ThreadEvent::SubagentsChanged { .. } => {
                // The background work is done: keep what it did, and let the agent go.
                if !thread.read(cx).in_background() {
                    self.save_transcript(id, cx);
                    self.retire_if_unwatched(id, cx);
                }
            }
            ThreadEvent::Finished { stop_reason } => {
                let t = thread.read(cx);
                let (status, (added, removed), meta) = (status_of(t), t.diff_stats(), t.meta());
                if let Some(s) = self.store.session_mut(id) {
                    s.status = status;
                    (s.added, s.removed) = (added, removed);
                    s.meta = Some(meta);
                    s.updated = unix_now();
                }
                eprintln!("arakun server: {id} ended its turn ({stop_reason})");
                self.save_transcript(id, cx);
                self.save_store();
                self.retire_if_unwatched(id, cx);
            }
            _ => {}
        }
    }

    fn save_store(&mut self) {
        if let Err(e) = self.store.save() {
            eprintln!("arakun server: could not save its state: {e}");
        }
    }

    fn save_transcript(&mut self, id: &str, cx: &App) {
        let Some(live) = self.live.get_mut(id) else {
            return;
        };
        live.dirty = false;
        live.saved = Instant::now();
        let transcript = live.thread.read(cx).transcript(cx);
        self.writer.save(self.store.transcript_path(id), transcript);
    }

    /// Stops the agent of a session nobody is looking at once it has nothing to do.
    fn retire_if_unwatched(&mut self, id: &str, cx: &mut Context<Self>) {
        let Some(live) = self.live.get(id) else {
            return;
        };
        let t = live.thread.read(cx);
        let busy =
            matches!(status_of(t), Status::Working | Status::NeedsInput) || t.in_background();
        if busy || self.watched(id) {
            return;
        }
        // What the turn's shell commands changed is worked out after it
        // settles, by a task the thread owns. Let go now, the thread would
        // take it along, and the transcript would never get those changes.
        if let Some(pending) = t.shell_pending().filter(|p| p.peek().is_none()) {
            let id = id.to_string();
            cx.spawn(async move |this, cx| {
                pending.await;
                let _ = this.update(cx, |this, cx| this.retire_if_unwatched(&id, cx));
            })
            .detach();
            return;
        }
        self.retire(id, cx);
    }

    fn retire(&mut self, id: &str, cx: &mut Context<Self>) {
        self.save_transcript(id, cx);
        let Some(live) = self.live.remove(id) else {
            return;
        };
        let meta = live.thread.read(cx).meta();
        if let Some(s) = self.store.session_mut(id) {
            s.meta = Some(meta);
        }
        live.thread.update(cx, |t, _| t.shutdown());
    }

    /// Runs `f` over a session's entries and directory: the live thread's, or
    /// the transcript's when its agent is not running.
    fn with_entries<R>(&self, id: &str, cx: &App, f: impl FnOnce(&[Entry], &Path) -> R) -> R {
        match self.live.get(id) {
            Some(live) => {
                let t = live.thread.read(cx);
                f(t.entries(), t.cwd())
            }
            None => {
                // A save of this file still on its way is waited for; one that
                // failed leaves the older file to show, which is all there is.
                let path = self.store.transcript_path(id);
                self.writer.behind(&path);
                let (all, cwd) = Thread::read_transcript(&path)
                    .map(|t| (t.entries, t.meta.cwd))
                    .unwrap_or_default();
                f(&all, &cwd)
            }
        }
    }

    fn tick(&mut self, cx: &mut Context<Self>) {
        if self.stopping {
            return;
        }
        self.sync_inbox(cx);
        let due: Vec<String> = self
            .live
            .iter()
            .filter(|(_, l)| l.dirty && l.saved.elapsed() >= SAVE_EVERY)
            .map(|(id, _)| id.clone())
            .collect();
        for id in due {
            self.save_transcript(&id, cx);
        }
    }

    /// Saves everything, lets every agent go and ends the process. A turn
    /// that was running is marked interrupted in its transcript.
    fn shut_down(&mut self, cx: &mut Context<Self>) {
        if self.stopping {
            return;
        }
        self.stopping = true;
        eprintln!("arakun server: stopping");
        let ids: Vec<String> = self.live.keys().cloned().collect();
        for id in ids {
            self.retire(&id, cx);
        }
        for s in &mut self.store.sessions {
            if matches!(s.status, Status::Working | Status::NeedsInput) {
                s.status = Status::Idle;
            }
        }
        self.save_devices();
        self.save_store();
        self.writer.finish();
        // The process ends without dropping the app, so the socket goes here.
        self.listening = None;
        if self.reach == Reach::Tailscale {
            let _ = tailscale::stop_serving();
        }
        self.code.set(0);
        cx.quit();
    }

    // ---- devices ----------------------------------------------------------------

    fn scope(&self, cwd: &Path) -> wire::Scope {
        wire::Scope {
            cwd: cwd.to_path_buf(),
            key: self.access.lock().unwrap().image_key.clone(),
        }
    }

    /// Writes the devices as the transport now knows them (paired, seen, unpaired).
    fn save_devices(&mut self) {
        self.settings.devices = self.access.lock().unwrap().devices.clone();
        self.settings.save();
        self.transport.sync_tickets(&self.access);
    }

    fn place_label(path: &Path) -> String {
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| path.display().to_string());
        match store::branch(path) {
            Some(branch) => format!("{name} · {branch}"),
            None => name,
        }
    }

    /// Every session, newest first, with what a waiting one asks, and the places.
    fn inbox(&self, cx: &App) -> String {
        let places = self
            .store
            .places
            .iter()
            .map(|p| {
                let name = p.name();
                wire::inbox_place(
                    &p.path,
                    &name,
                    &name,
                    store::branch(&p.path).as_deref(),
                    p.agent.id(),
                )
            })
            .collect();
        let sessions = self
            .store
            .sessions
            .iter()
            .map(|s| {
                let project = s
                    .place
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default();
                wire::inbox_session(wire::InboxSession {
                    id: &s.id,
                    title: &s.title,
                    agent: s.agent.label(),
                    status: s.status,
                    updated: s.updated,
                    added: s.added,
                    removed: s.removed,
                    project: &project,
                    env: &project,
                    pr: s.meta.as_ref().and_then(|m| m.pr.as_deref()),
                    asks: self
                        .live
                        .get(&s.id)
                        .and_then(|l| wire::asks(l.thread.read(cx))),
                })
            })
            .collect();
        wire::inbox(&phone::host_name(), sessions, places)
    }

    fn inbox_changed(&mut self, cx: &mut Context<Self>) {
        if self.transport.client_count() == 0 {
            return;
        }
        self.inbox_dirty = true;
        self.schedule_flush(cx);
    }

    /// Sends the inbox to every device when it differs from what they last got.
    fn sync_inbox(&mut self, cx: &mut Context<Self>) {
        if self.transport.client_count() == 0 {
            return;
        }
        let inbox = self.inbox(cx);
        if self.last_inbox != inbox {
            self.transport.broadcast(&inbox);
            self.last_inbox = inbox;
        }
    }

    fn schedule_flush(&mut self, cx: &mut Context<Self>) {
        if self.flush.is_none() {
            self.flush = Some(cx.spawn(async move |this, cx| {
                cx.background_executor().timer(GATHER).await;
                let _ = this.update(cx, |this, cx| this.flush(cx));
            }));
        }
    }

    /// Notes what devices should hear about an event of a live thread.
    fn note_for_devices(&mut self, id: &str, ev: &ThreadEvent, cx: &mut Context<Self>) {
        // The inbox shows every session's status and title, watched or not.
        if matches!(
            ev,
            ThreadEvent::StatusChanged
                | ThreadEvent::NeedsPermission
                | ThreadEvent::TitleChanged
                | ThreadEvent::Finished { .. }
        ) && self.transport.client_count() > 0
        {
            self.inbox_dirty = true;
        }
        if !self.watched(id) {
            if self.inbox_dirty {
                self.schedule_flush(cx);
            }
            return;
        }
        let state_changed = |this: &mut Self| {
            if !this.state_dirty.iter().any(|d| d == id) {
                this.state_dirty.push(id.to_string());
            }
        };
        match ev {
            ThreadEvent::EntriesChanged { from } => {
                let e = self.dirty.entry(id.to_string()).or_insert(*from);
                *e = (*e).min(*from);
            }
            ThreadEvent::Reset => {
                self.dirty.insert(id.to_string(), 0);
            }
            // Shell changes land a moment after the turn ends; the changes chip follows them.
            ThreadEvent::StatusChanged
            | ThreadEvent::NeedsPermission
            | ThreadEvent::TitleChanged
            | ThreadEvent::Changed
            | ThreadEvent::ShellChanges => state_changed(self),
            // The agent may be let go right after a turn ends; send what is
            // pending while its thread is still there.
            ThreadEvent::Finished { .. } => {
                state_changed(self);
                self.flush(cx);
                return;
            }
            _ => return,
        }
        self.schedule_flush(cx);
    }

    fn flush(&mut self, cx: &mut Context<Self>) {
        self.flush = None;
        let dirty: Vec<(String, usize)> = self.dirty.drain().collect();
        let states: Vec<String> = std::mem::take(&mut self.state_dirty);
        if std::mem::take(&mut self.inbox_dirty) {
            self.sync_inbox(cx);
        }
        for (id, from) in dirty {
            let Some(thread) = self.thread(&id) else {
                continue;
            };
            let t = thread.read(cx);
            let msg = wire::entries_message(&id, t.entries(), from, &self.scope(t.cwd()));
            self.send_watchers(&id, msg);
        }
        for id in states {
            let Some(thread) = self.thread(&id) else {
                continue;
            };
            let t = thread.read(cx);
            let msg = wire::state_message(&id, t, status_of(t));
            self.send_watchers(&id, msg);
        }
    }

    fn send_watchers(&self, id: &str, msg: String) {
        for (client, watched) in &self.watching {
            if watched == id {
                self.transport.send(*client, msg.clone());
            }
        }
    }

    /// A device looked away from its session: the agent may stop if no one else is looking.
    fn stop_watching(&mut self, client: ClientId, cx: &mut Context<Self>) {
        if let Some(id) = self.watching.remove(&client) {
            self.retire_if_unwatched(&id, cx);
        }
    }

    fn incoming(&mut self, msg: Incoming, cx: &mut Context<Self>) {
        if self.stopping {
            return;
        }
        match msg {
            Incoming::Connected(id, _device) => {
                // To this device alone: what the others last got stays as it
                // is, or a change they have not seen would never reach them.
                let inbox = self.inbox(cx);
                self.transport.send(id, inbox);
                // The device's last_seen moved.
                self.save_devices();
            }
            Incoming::Disconnected(id) => {
                self.stop_watching(id, cx);
                self.hidden.remove(&id);
            }
            // The device's sockets close on their own; `Disconnected` tidies the rest.
            Incoming::Paired | Incoming::Subscribed | Incoming::Unpaired => self.save_devices(),
            Incoming::Relay(status) => {
                if let Transport::Relay(client) = &self.transport {
                    let page = client.page_url().to_string();
                    let news = match &status {
                        relay::Status::Connecting => None,
                        relay::Status::Connected => Some(format!("reachable at {page}")),
                        relay::Status::Reconnecting { why, .. } => {
                            Some(format!("the relay is away: {why}"))
                        }
                        relay::Status::Rejected(why) => {
                            Some(format!("the relay turned this server away: {why}"))
                        }
                    };
                    self.set_address(relay_address(status, &page), news);
                }
            }
            Incoming::Message(id, msg) => self.message(id, msg, cx),
        }
    }

    fn message(&mut self, client: ClientId, msg: ClientMsg, cx: &mut Context<Self>) {
        let gone = || wire::error("that session is gone");
        match msg {
            ClientMsg::Open { session } => {
                let Some(record) = self.store.session(&session) else {
                    self.transport.send(client, gone());
                    return;
                };
                let place = Self::place_label(&record.place);
                let head = wire::SessionHead {
                    id: &record.id,
                    title: &record.title,
                    agent: record.agent.label(),
                    status: record.status,
                    place: &place,
                };
                let reply = match self.live.get(&session) {
                    Some(live) => {
                        let t = live.thread.read(cx);
                        wire::session_live(&head, t, status_of(t), &self.scope(t.cwd()))
                    }
                    None => self.with_entries(&session, cx, |all, cwd| {
                        wire::session_saved(&head, all, &self.scope(cwd))
                    }),
                };
                // The session it looked at before has one watcher fewer.
                if let Some(before) = self.watching.insert(client, session.clone())
                    && before != session
                {
                    self.retire_if_unwatched(&before, cx);
                }
                self.transport.send(client, reply);
            }
            ClientMsg::Close => self.stop_watching(client, cx),
            ClientMsg::Entries { session, from, to } => {
                if self.store.session(&session).is_none() {
                    return;
                }
                let (entries, count) = self.with_entries(&session, cx, |all, cwd| {
                    let to = to.max(from).min(all.len());
                    let from = from.min(to);
                    let scope = self.scope(cwd);
                    let entries: Vec<serde_json::Value> = all[from..to]
                        .iter()
                        .map(|e| wire::entry(e, &scope))
                        .collect();
                    (entries, all.len())
                });
                let reply = json!({ "type": "entries", "id": session, "from": from, "count": count, "entries": entries, "older": true });
                self.transport.send(client, reply.to_string());
            }
            ClientMsg::Send {
                session,
                text,
                uploads,
            } => {
                let text = self.uploads.attach(text, &uploads);
                if let Err(e) = self.send_to_session(&session, text, cx) {
                    self.transport.send(client, wire::error(&e));
                }
            }
            ClientMsg::Upload {
                session: _,
                upload,
                name,
                part,
                of,
                data,
            } => {
                let dir = self.store.uploads_dir();
                let saved = self
                    .uploads
                    .piece(&upload, &name, part, of, &data, |bytes, ext| {
                        save_upload(&dir, bytes, ext)
                    });
                if let Some(path) = saved {
                    let name = path
                        .file_name()
                        .map(|n| n.to_string_lossy())
                        .unwrap_or_default();
                    self.transport.send(
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
                let Some(place) = self.store.place_mut(&path) else {
                    self.transport
                        .send(client, wire::error("that project is gone"));
                    return;
                };
                let agent = agent
                    .as_deref()
                    .and_then(AgentKind::parse)
                    .unwrap_or(place.agent);
                let text = text.trim().to_string();
                if text.is_empty() {
                    return;
                }
                // The place remembers them, as it does on the desktop: the
                // model and the reasoning, and no other setting. A device that
                // could keep a permission mode for a folder would turn the
                // asking off there for every session after it.
                for (key, value) in &settings {
                    if let Some(s) = device_setting(key) {
                        place.pick(agent, s, value);
                    }
                }
                let id = self.start_session(&path, agent, None, text, cx);
                self.sync_inbox(cx);
                self.transport
                    .send(client, json!({ "type": "started", "id": id }).to_string());
            }
            ClientMsg::Options { env, agent } => {
                let kind = AgentKind::parse(&agent);
                let list = match (self.store.place(Path::new(&env)), kind) {
                    (Some(place), Some(kind)) => DEVICE_SETTINGS
                        .into_iter()
                        .filter_map(|s| Some((s, draft_choices(place, kind, s)?)))
                        .collect(),
                    _ => Vec::new(),
                };
                let reply = json!({
                    "type": "options", "env": env,
                    "agent": kind.map_or(agent.as_str(), |k| k.id()),
                    "settings": wire::settings(list),
                });
                self.transport.send(client, reply.to_string());
            }
            ClientMsg::Settings { session } => {
                if self.store.session(&session).is_none() {
                    return;
                }
                let list = match self.ensure_live(&session, cx) {
                    Ok(thread) => session_settings(thread.read(cx)),
                    Err(_) => Vec::new(),
                };
                let reply =
                    json!({ "type": "settings", "id": session, "settings": wire::settings(list) });
                self.transport.send(client, reply.to_string());
            }
            ClientMsg::Setting {
                session,
                key,
                value,
            } => {
                let Some(s) = device_setting(&key) else {
                    self.transport.send(
                        client,
                        wire::error("that setting is not one a device changes"),
                    );
                    return;
                };
                let Some(thread) = self.thread(&session) else {
                    self.transport
                        .send(client, wire::error("that session is not running"));
                    return;
                };
                thread.update(cx, |t, cx| t.set_setting(s, &value, cx));
                // The place remembers it too, for its next session of this agent.
                if let Some((place, agent)) = self
                    .store
                    .session(&session)
                    .map(|r| (r.place.clone(), r.agent))
                    && let Some(place) = self.store.place_mut(&place)
                {
                    place.pick(agent, s, &value);
                    self.save_store();
                }
                let list = session_settings(thread.read(cx));
                let reply =
                    json!({ "type": "settings", "id": session, "settings": wire::settings(list) });
                self.transport.send(client, reply.to_string());
            }
            ClientMsg::Cancel { session } => {
                if let Some(thread) = self.thread(&session) {
                    thread.update(cx, |t, cx| t.cancel(cx));
                }
            }
            ClientMsg::Permission { session, option } => {
                if let Some(thread) = self.thread(&session) {
                    thread.update(cx, |t, cx| t.respond_permission(&option, cx));
                }
            }
            ClientMsg::Answer { session, answers } => {
                if let Some(thread) = self.thread(&session)
                    && let Err(e) = answer_question(&thread, answers, cx)
                {
                    self.transport.send(client, wire::error(&e));
                }
            }
            ClientMsg::SkipQuestion { session } => {
                if let Some(thread) = self.thread(&session) {
                    thread.update(cx, |t, cx| t.skip_question(cx));
                }
            }
            ClientMsg::Changes { session } => {
                if self.store.session(&session).is_none() {
                    self.transport.send(client, gone());
                    return;
                }
                let mut msg = self.with_entries(&session, cx, wire::changes);
                msg["id"] = json!(session);
                self.transport.send(client, msg.to_string());
            }
            ClientMsg::Diff {
                session,
                entry,
                index,
            } => {
                if self.store.session(&session).is_none() {
                    self.transport.send(client, gone());
                    return;
                }
                let diff = self.with_entries(&session, cx, |all, cwd| {
                    wire::diff_message(all, entry, index, cwd)
                });
                let reply = match diff {
                    Some(mut msg) => {
                        msg["id"] = json!(session);
                        msg.to_string()
                    }
                    None => wire::error("that change is gone"),
                };
                self.transport.send(client, reply);
            }
            ClientMsg::Usage => {
                // Fresh numbers follow through `usage_changed` once the reads land.
                // Claude's are read here, off the main thread: there is no Usage
                // tab on a server to have read them already.
                self.account
                    .update(cx, |a, cx| a.refresh_if_stale(Duration::from_secs(60), cx));
                cx.spawn(async move |this, cx| {
                    cx.background_executor()
                        .spawn(async {
                            agent::plan::refresh();
                        })
                        .await;
                    let _ = this.update(cx, |this, cx| this.usage_changed(cx));
                })
                .detach();
                let usage = self.usage(cx);
                self.transport.send(client, usage);
            }
            ClientMsg::Visible { visible } => {
                if visible {
                    self.hidden.remove(&client);
                } else {
                    self.hidden.insert(client);
                }
            }
            ClientMsg::Ping => {}
        }
    }

    /// Sends `text` to the session `id` as its next prompt.
    fn send_to_session(
        &mut self,
        id: &str,
        text: String,
        cx: &mut Context<Self>,
    ) -> Result<(), String> {
        let thread = self.ensure_live(id, cx)?;
        thread.update(cx, |t, cx| t.send(text, cx));
        if let Some(s) = self.store.session_mut(id) {
            s.updated = unix_now();
        }
        self.inbox_changed(cx);
        Ok(())
    }

    /// Both agents' plan limits, as `wire::plan_usage` puts them.
    fn usage(&self, cx: &App) -> String {
        let plan = agent::plan::latest();
        let account = self.account.read(cx);
        let droid = wire::DroidPlan {
            connected: account.connected(),
            limits: account.limits(),
            error: account.error().map(|e| e.as_ref()),
            fetched_at: account.fetched_at(),
        };
        wire::plan_usage(plan.as_ref(), &droid, crate::account::unix_now()).to_string()
    }

    /// The plan limits may have moved (a fetch landed): devices hear if they did.
    fn usage_changed(&mut self, cx: &mut Context<Self>) {
        if self.transport.client_count() == 0 {
            return;
        }
        let usage = self.usage(cx);
        if self.last_usage != usage {
            self.transport.broadcast(&usage);
            self.last_usage = usage;
        }
    }

    /// Pushes a notice to the devices that asked for them when a session
    /// finishes a turn, wants an answer or fails, except to the ones looking
    /// at that very session. Subscriptions the push service says are gone are
    /// forgotten.
    fn notify(
        &mut self,
        id: &str,
        thread: &Entity<Thread>,
        ev: &ThreadEvent,
        cx: &mut Context<Self>,
    ) {
        let t = thread.read(cx);
        let (kind, body): (Notice, String) = match ev {
            ThreadEvent::Finished { stop_reason } => match stop_reason.as_str() {
                "cancelled" => return,
                "error" => (Notice::Error, error_text(t)),
                _ => (Notice::Finished, "Finished its turn.".into()),
            },
            ThreadEvent::NeedsPermission => {
                let body = if let Some(p) = t.pending_permission() {
                    if p.plan.is_some() {
                        "Has a plan ready for your approval.".to_string()
                    } else {
                        format!("Needs permission: {}", p.title)
                    }
                } else if t.pending_question().is_some() {
                    "Has a question for you.".into()
                } else {
                    "Needs your input.".into()
                };
                (Notice::Input, body)
            }
            ThreadEvent::StatusChanged if matches!(t.status(), ThreadStatus::Error(_)) => {
                (Notice::Error, error_text(t))
            }
            ThreadEvent::StatusChanged => {
                // Back at work: the next turn end or question notifies again.
                if t.status() == ThreadStatus::Working {
                    self.notified.remove(id);
                }
                return;
            }
            _ => return,
        };
        // A failing turn reports both a status change and its end; one notice is enough.
        if self.notified.insert(id.to_string(), kind) == Some(kind) {
            return;
        }
        let title = self
            .store
            .session(id)
            .map(|s| s.title.clone())
            .filter(|t| !t.is_empty())
            .unwrap_or_else(|| t.title().to_string());
        // Devices with this session on screen see the change already.
        let looking: HashSet<String> = self
            .watching
            .iter()
            .filter(|(client, watched)| *watched == id && !self.hidden.contains(client))
            .filter_map(|(client, _)| self.transport.device_of(*client))
            .collect();
        let wanted = |d: &phone::Device| match kind {
            Notice::Finished => d.notices.done,
            Notice::Input => d.notices.input,
            Notice::Error => d.notices.error,
        };
        let targets: Vec<(String, push::Subscription)> = self
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
        let (vapid, access) = (self.vapid.clone(), self.access.clone());
        cx.spawn(async move |this, cx| {
            let gone = cx
                .background_executor()
                .spawn(async move {
                    let now = unix_now();
                    let mut gone = Vec::new();
                    for (device, sub) in targets {
                        match push::send(&vapid, &sub, payload.as_bytes(), now) {
                            Ok(()) => {}
                            Err(push::Fail::Gone) => gone.push((device, sub)),
                            Err(e) => eprintln!("arakun server: could not notify a device: {e}"),
                        }
                    }
                    if !gone.is_empty() {
                        access.lock().unwrap().drop_subscriptions(&gone);
                    }
                    gone
                })
                .await;
            if !gone.is_empty() {
                let _ = this.update(cx, |this, _| this.save_devices());
            }
        })
        .detach();
    }

    // ---- the local commands -----------------------------------------------------

    fn request(&mut self, req: &Request, cx: &mut Context<Self>) -> Reply {
        if self.stopping {
            return Reply::err("the server is shutting down");
        }
        let arg = |i: usize| req.args.get(i).map_or("", String::as_str);
        match req.cmd.as_str() {
            "status" => Reply::ok(self.status_text(cx)),
            "pair" => self.pair().into(),
            "devices" => Reply::ok(self.devices_text()),
            "unpair" => self.unpair(arg(0)).into(),
            "add" => self.add_place(Path::new(arg(0)), arg(1)).into(),
            "remove" => {
                let path = Path::new(arg(0));
                if self.store.remove_place(path) {
                    self.save_store();
                    self.inbox_changed(cx);
                    Reply::ok(format!("removed {}", path.display()))
                } else {
                    Reply::err(format!("{} is not one of its folders", path.display()))
                }
            }
            "ls" => Reply::ok(self.listing()),
            "new" => self.new_session(req, cx).into(),
            "quit" => {
                // After the reply is on its way.
                cx.spawn(async move |this, cx| {
                    cx.background_executor()
                        .timer(Duration::from_millis(50))
                        .await;
                    let _ = this.update(cx, |this, cx| this.shut_down(cx));
                })
                .detach();
                Reply::ok("stopping")
            }
            "log" | "state" | "send" | "stop" | "approve" | "deny" | "answer" | "rm" => {
                let id = match self.store.find(arg(0)) {
                    Ok(id) => id,
                    Err(e) => return Reply::err(e),
                };
                match req.cmd.as_str() {
                    "log" => Reply::ok(self.log(&id, arg(1).parse().unwrap_or(super::LOG), cx)),
                    "state" => Reply::ok(self.state(&id, cx)),
                    "send" => self
                        .send_to_session(&id, arg(1).to_string(), cx)
                        .map(|()| format!("sent to {id}"))
                        .into(),
                    // One waiting on an answer counts: stopping it withdraws
                    // the question, and `rm` asks for a stop first.
                    "stop" => match self.thread(&id) {
                        Some(t)
                            if matches!(
                                status_of(t.read(cx)),
                                Status::Working | Status::NeedsInput
                            ) =>
                        {
                            t.update(cx, |t, cx| t.cancel(cx));
                            Reply::ok(format!("stopping {id}"))
                        }
                        _ => Reply::ok(format!("{id} is not working")),
                    },
                    "approve" | "deny" => self.answer_permission(&id, req.cmd == "approve", cx),
                    "answer" => match self.thread(&id) {
                        Some(t) => answer_question(&t, req.args[1..].to_vec(), cx)
                            .map(|()| format!("answered {id}"))
                            .into(),
                        None => Reply::err(format!("{id} asks nothing")),
                    },
                    _ => self.remove_session(&id, cx).into(),
                }
            }
            other => Reply::err(format!(
                "unknown command `{name} {other}` (see {name} help)",
                name = super::name()
            )),
        }
    }

    fn status_text(&self, cx: &App) -> String {
        let devices = self.access.lock().unwrap().devices.len();
        let count = |status: Status| {
            self.store
                .sessions
                .iter()
                .filter(|s| s.status == status)
                .count()
        };
        let agents = self
            .live
            .values()
            .filter(|l| l.thread.read(cx).is_busy())
            .count();
        format!(
            "arakun server {} on {}\nreach     {}\ndevices   {devices} paired, {} connected\nfolders   {}\nsessions  {} ({} working, {} waiting on you, {} agents busy)",
            env!("CARGO_PKG_VERSION"),
            phone::host_name(),
            reach_line(self.reach, &self.transport, &self.address),
            self.transport.connected_devices().len(),
            self.store.places.len(),
            self.store.sessions.len(),
            count(Status::Working),
            count(Status::NeedsInput),
            agents,
        )
    }

    /// Opens a pairing: the link a device opens (and its code, on the network),
    /// as JSON for the command to print.
    fn pair(&mut self) -> Result<String, String> {
        let base = match (&self.address, &self.transport) {
            (Address::Ready(url), _) => url.clone(),
            (_, Transport::Relay(client)) => client.page_url().to_string(),
            (Address::Pending(what) | Address::Problem(what), _) => {
                return Err(format!("devices can't reach the server yet: {what}"));
            }
        };
        let (url, code) = {
            let mut access = self.access.lock().unwrap();
            match self.reach {
                // Through the relay the link carries everything; there is no code to read out.
                Reach::Relay => {
                    let (code, secret) = access.begin_pairing_relay();
                    (phone::relay_link(&base, &code, &secret), String::new())
                }
                Reach::Lan | Reach::Tailscale => {
                    let code = access.begin_pairing();
                    (phone::link(&base, &code), code)
                }
            }
        };
        self.transport.sync_pairing(&self.access);
        let waiting = match &self.address {
            Address::Ready(_) => String::new(),
            other => other.line(),
        };
        Ok(json!({
            "url": url, "code": code, "seconds": phone::PAIR_FOR.as_secs(), "waiting": waiting,
        })
        .to_string())
    }

    fn devices_text(&self) -> String {
        let connected = self.transport.connected_devices();
        let now = unix_now();
        let rows: Vec<String> = self
            .access
            .lock()
            .unwrap()
            .devices
            .iter()
            .map(|d| {
                let seen = if connected.contains(&d.id) {
                    "connected".to_string()
                } else {
                    format!("seen {}", crate::account::ago(d.last_seen, now))
                };
                format!("{}  {}  {seen}", d.id, d.name)
            })
            .collect();
        if rows.is_empty() {
            format!("no devices; `{} pair` lets one in", super::name())
        } else {
            rows.join("\n")
        }
    }

    /// Unpairs the device `which` names: its id, the start of it, or its name.
    fn unpair(&mut self, which: &str) -> Result<String, String> {
        let found: Vec<(String, String)> = self
            .access
            .lock()
            .unwrap()
            .devices
            .iter()
            .filter(|d| !which.is_empty() && (d.id.starts_with(which) || d.name == which))
            .map(|d| (d.id.clone(), d.name.clone()))
            .collect();
        let (id, name) = match &found[..] {
            [one] => one.clone(),
            [] => {
                return Err(format!(
                    "no device {which}; `{} devices` lists them",
                    super::name()
                ));
            }
            _ => return Err(format!("more than one device matches {which}; use its id")),
        };
        self.access.lock().unwrap().unpair(&id);
        self.transport.disconnect_device(&id);
        self.save_devices();
        Ok(format!("unpaired {name} ({id})"))
    }

    /// Makes the folder at `path` (canonical) one sessions can start in.
    fn add_place(&mut self, path: &Path, agent: &str) -> Result<String, String> {
        if !path.is_dir() {
            return Err(format!("not a directory: {}", path.display()));
        }
        let kind = match agent {
            "" => None,
            a => Some(AgentKind::parse(a).ok_or_else(|| unknown_agent(a))?),
        };
        let added = self
            .store
            .add_place(path, kind.unwrap_or(AgentKind::Claude));
        if let (false, Some(kind), Some(place)) = (added, kind, self.store.place_mut(path)) {
            place.agent = kind;
        }
        self.save_store();
        self.inbox_dirty = true;
        Ok(format!(
            "{} {}",
            if added { "added" } else { "already has" },
            path.display()
        ))
    }

    fn row(&self, s: &Record) -> String {
        format!(
            "{}  {}  {}  {}  {}",
            s.id,
            wire::status(s.status),
            s.agent.label(),
            s.title,
            s.place.display()
        )
    }

    fn listing(&self) -> String {
        if self.store.places.is_empty() && self.store.sessions.is_empty() {
            return format!(
                "nothing yet; `{} new <prompt>` starts a session in this folder",
                super::name()
            );
        }
        let mut out = String::from("folders:");
        for p in &self.store.places {
            let branch = store::branch(&p.path).map_or(String::new(), |b| format!("  [{b}]"));
            out.push_str(&format!(
                "\n  {}{branch}  {}",
                p.path.display(),
                p.agent.id()
            ));
        }
        out.push_str("\nsessions:");
        for s in &self.store.sessions {
            out.push_str(&format!("\n  {}", self.row(s)));
        }
        if self.store.sessions.is_empty() {
            out.push_str("\n  (none)");
        }
        out
    }

    /// `new`, from `[agent, dir, title, model, effort, prompt]`.
    fn new_session(&mut self, req: &Request, cx: &mut Context<Self>) -> Result<String, String> {
        let [agent, dir, title, model, effort, prompt] = &req.args[..] else {
            return Err("bad request".into());
        };
        let path = Path::new(dir);
        if !path.is_dir() {
            return Err(format!("not a directory: {}", path.display()));
        }
        let kind = match agent.as_str() {
            "" => None,
            a => Some(AgentKind::parse(a).ok_or_else(|| unknown_agent(a))?),
        };
        // Whoever runs the command is the user: a folder they name is theirs to use.
        self.store
            .add_place(path, kind.unwrap_or(AgentKind::Claude));
        let place = self.store.place_mut(path).ok_or("bad request")?;
        let kind = kind.unwrap_or(place.agent);
        for (s, value) in [(Setting::Model, model), (Setting::Effort, effort)] {
            if !value.is_empty() {
                place.pick(kind, s, value);
            }
        }
        let title = (!title.is_empty()).then(|| title.clone());
        Ok(self.start_session(path, kind, title, prompt.clone(), cx))
    }

    fn log(&self, id: &str, lines: usize, cx: &App) -> String {
        let Some(s) = self.store.session(id) else {
            return String::new();
        };
        let mut out = self.row(s);
        if let Some(live) = self.live.get(id) {
            let t = live.thread.read(cx);
            if let ThreadStatus::Error(_) = t.status() {
                out.push_str(&format!("\nerror: {}", error_text(t)));
            }
            if let Some(asks) = asks_text(id, t) {
                out.push_str(&format!("\n{asks}"));
            }
        }
        let log = self.with_entries(id, cx, |all, _| render_log(all, lines));
        out.push_str("\n\n");
        out.push_str(if log.is_empty() {
            "(nothing yet)"
        } else {
            &log
        });
        out
    }

    /// The session's status, then what `arakun server wait` prints under it:
    /// what it asks, its error, or the agent's last message.
    fn state(&self, id: &str, cx: &App) -> String {
        let status = match (self.live.get(id), self.store.session(id)) {
            (Some(live), _) => status_of(live.thread.read(cx)),
            (None, Some(s)) => s.status,
            (None, None) => Status::Idle,
        };
        let rest = match (status, self.live.get(id)) {
            (Status::Working, _) => String::new(),
            (Status::NeedsInput, Some(live)) => {
                asks_text(id, live.thread.read(cx)).unwrap_or_default()
            }
            (Status::Error, Some(live)) => error_text(live.thread.read(cx)),
            // Its agent was let go after the turn failed: the transcript kept why.
            (Status::Error, None) => self.with_entries(id, cx, |all, _| last_error(all)),
            _ => self.with_entries(id, cx, |all, _| last_message(all)),
        };
        format!("{}\n{rest}", wire::status(status))
    }

    fn answer_permission(&mut self, id: &str, approve: bool, cx: &mut Context<Self>) -> Reply {
        let Some(thread) = self.thread(id) else {
            return Reply::err(format!("{id} asks nothing"));
        };
        let Some(title) = thread
            .read(cx)
            .pending_permission()
            .map(|p| p.title.clone())
        else {
            return Reply::err(format!("{id} asks for no permission"));
        };
        thread.update(cx, |t, cx| {
            if approve {
                t.allow_permission(cx)
            } else {
                t.reject_permission(cx)
            }
        });
        Reply::ok(format!(
            "{} {id}: {title}",
            if approve { "approved" } else { "denied" }
        ))
    }

    fn remove_session(&mut self, id: &str, cx: &mut Context<Self>) -> Result<String, String> {
        if let Some(thread) = self.thread(id) {
            let t = thread.read(cx);
            if matches!(status_of(t), Status::Working | Status::NeedsInput) || t.in_background() {
                return Err(format!(
                    "{id} is still working; `{} stop {id}` first",
                    super::name()
                ));
            }
        }
        if let Some(live) = self.live.remove(id) {
            live.thread.update(cx, |t, _| t.shutdown());
        }
        self.watching.retain(|_, watched| watched != id);
        self.notified.remove(id);
        // A save still on its way, or one kept because it failed, would bring
        // the file back after it is deleted.
        self.writer.forget(self.store.transcript_path(id));
        self.store.remove_session(id);
        self.save_store();
        self.inbox_changed(cx);
        Ok(format!("removed {id}"))
    }
}

fn unknown_agent(agent: &str) -> String {
    format!("unknown agent {agent}: claude, droid, opencode or workers-ai")
}

/// What a session waits on the user for, with the command that answers it.
fn asks_text(id: &str, t: &Thread) -> Option<String> {
    if let Some(p) = t.pending_permission() {
        return Some(format!(
            "needs permission: {}\n  {name} approve {id}  |  {name} deny {id}",
            p.title,
            name = super::name()
        ));
    }
    let q = t.pending_question()?;
    let mut out = String::from("asks:");
    for question in &q.questions {
        out.push_str(&format!("\n  {}", question.question));
        if !question.options.is_empty() {
            out.push_str(&format!("  ({})", question.options.join(" | ")));
        }
    }
    out.push_str(&format!(
        "\n  {} answer {id} <answer>{}",
        super::name(),
        if q.questions.len() > 1 {
            " <answer>…  (one for each question)"
        } else {
            ""
        }
    ));
    Some(out)
}

/// Answers the questionnaire a session waits on: one answer for each question.
fn answer_question(
    thread: &Entity<Thread>,
    answers: Vec<String>,
    cx: &mut Context<Daemon>,
) -> Result<(), String> {
    let fits = thread
        .read(cx)
        .pending_question()
        .is_some_and(|q| q.questions.len() == answers.len());
    if !fits {
        return Err("that question is gone".into());
    }
    thread.update(cx, |t, cx| t.answer_question(answers, cx));
    Ok(())
}

/// The setting `key` names, when it is one a device may pick.
fn device_setting(key: &str) -> Option<Setting> {
    Setting::from_key(key).filter(|s| DEVICE_SETTINGS.contains(s))
}

/// A running session's settings a device can change, with their choices.
fn session_settings(t: &Thread) -> Vec<(Setting, (String, Vec<SettingOption>))> {
    DEVICE_SETTINGS
        .into_iter()
        .filter_map(|s| Some((s, t.setting(s).filter(|(_, o)| !o.is_empty())?)))
        .collect()
}

/// What the next `agent` session of `place` starts with for a setting, and its choices.
fn draft_choices(
    place: &Place,
    agent: AgentKind,
    s: Setting,
) -> Option<(String, Vec<SettingOption>)> {
    let picked = place.picked(agent);
    let pick = |s: Setting| picked.iter().find(|(d, _)| *d == s).map(|(_, v)| v.clone());
    match agent {
        AgentKind::Droid => {
            if !catalog::droid_native() {
                return None;
            }
            let catalog = catalog::current();
            if s == Setting::Model && catalog.models.is_empty() {
                return None;
            }
            let mut settings = catalog.defaults.clone();
            for (d, v) in &picked {
                agent::apply_setting(&mut settings, &catalog.models, *d, v);
            }
            Some(agent::setting_options(&settings, &catalog.models, s))
        }
        AgentKind::Claude | AgentKind::OpenCode => {
            let options = if agent == AgentKind::Claude {
                catalog::claude_options()
            } else {
                catalog::opencode_options()
            };
            // OpenCode's reasoning levels are per model: the cached ones are the default model's.
            if agent == AgentKind::OpenCode
                && s == Setting::Effort
                && let Some(model) = pick(Setting::Model)
                && agent::acp_option(&options, Setting::Model).is_some_and(|o| o.current != model)
            {
                return None;
            }
            let o = agent::acp_option(&options, s)?;
            let (mut current, choices) = agent::acp_setting_options(o, s);
            if let Some(v) = pick(s).filter(|v| choices.iter().any(|c| c.id == *v)) {
                current = v;
            }
            Some((current, choices))
        }
        AgentKind::WorkersAi => {
            let options = workers_ai::draft_options(&picked);
            agent::acp_option(&options, s).map(|o| agent::acp_setting_options(o, s))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_address_line_says_what_is_in_the_way() {
        assert_eq!(
            Address::Ready("https://a.example/".into()).line(),
            "https://a.example/"
        );
        assert_eq!(
            Address::Pending("connecting to the relay".into()).line(),
            "not yet: connecting to the relay"
        );
        assert_eq!(
            relay_address(relay::Status::Connected, "https://l.relay/"),
            Address::Ready("https://l.relay/".into())
        );
        assert_eq!(
            relay_address(
                relay::Status::Reconnecting {
                    why: "The relay closed the connection.".into(),
                    retry_in: Duration::from_millis(200),
                },
                "https://l.relay/"
            ),
            Address::Pending("The relay closed the connection. Trying again in 1 s.".into())
        );
        assert_eq!(
            relay_address(relay::Status::Rejected("Blocked.".into()), "x"),
            Address::Problem("Blocked.".into())
        );
    }

    #[test]
    fn a_device_picks_the_model_and_the_reasoning_and_nothing_else() {
        assert_eq!(device_setting("modelId"), Some(Setting::Model));
        assert_eq!(device_setting("reasoningEffort"), Some(Setting::Effort));
        // These decide what an agent may do without asking.
        assert_eq!(device_setting("interactionMode"), None);
        assert_eq!(device_setting("autonomyLevel"), None);
        assert_eq!(device_setting("fastMode"), None);
        assert_eq!(device_setting("nonsense"), None);
    }

    #[test]
    fn a_title_is_the_prompts_first_line() {
        assert_eq!(
            summary("\n  fix the build  \nand the tests"),
            "fix the build"
        );
        let long = "x".repeat(100);
        assert_eq!(summary(&long), format!("{}…", "x".repeat(80)));
    }

    #[test]
    fn a_photo_gets_a_file_of_its_own() {
        let dir = std::env::temp_dir().join(format!("arakun-server-up-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let a = save_upload(&dir, b"one", "png").unwrap();
        let b = save_upload(&dir, b"two", "png").unwrap();
        assert_ne!(a, b);
        assert_eq!(std::fs::read(&a).unwrap(), b"one");
        assert_eq!(std::fs::read(&b).unwrap(), b"two");
        assert_eq!(a.extension().unwrap(), "png");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_last_agent_message_skips_what_came_after_it() {
        let entries = vec![
            Entry::Agent {
                text: " Done. ".into(),
            },
            Entry::Agent { text: "  ".into() },
            Entry::Notice {
                text: "Check passed".into(),
                error: false,
                mark: None,
                places: Vec::new(),
            },
        ];
        assert_eq!(last_message(&entries), "Done.");
        assert_eq!(last_message(&[]), "");
        // A notice that is no error says nothing about a failure.
        assert_eq!(last_error(&entries), "Stopped with an error.");
        let failed = [Entry::Notice {
            text: " No token. ".into(),
            error: true,
            mark: None,
            places: Vec::new(),
        }];
        assert_eq!(last_error(&failed), "No token.");
    }

    fn transcript(text: &str) -> Transcript {
        Transcript {
            version: 1,
            meta: agent::SessionMeta {
                agent: AgentKind::Claude,
                session_id: None,
                title: "t".into(),
                cwd: "/w/app".into(),
                created: 1,
                updated: 1,
                pr: None,
            },
            entries: vec![Entry::Agent { text: text.into() }],
            usage: agent::Usage::default(),
            fork_point: None,
            interrupted: false,
            resume: false,
            queue: Vec::new(),
            handoff: None,
            subagents: Vec::new(),
        }
    }

    #[test]
    fn the_writer_leaves_the_newest_of_each_file() {
        let dir = std::env::temp_dir().join(format!("arakun-server-wr-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let (a, b) = (dir.join("sessions/a.json"), dir.join("sessions/b.json"));
        let last = |path: &Path| last_message(&Thread::read_transcript(path).unwrap().entries);
        let mut writer = Writer::start().unwrap();
        writer.save(a.clone(), transcript("first"));
        writer.save(b.clone(), transcript("other"));
        writer.save(a.clone(), transcript("second"));
        // A read comes after a flush, and finds every save made before it.
        assert!(writer.flush().is_empty());
        assert_eq!(last(&a), "second");
        assert_eq!(last(&b), "other");
        // Written files are owed nothing: asking about them waits on no one.
        assert!(writer.owed.lock().unwrap().is_empty());
        assert!(!writer.behind(&a));
        // A removal too: nothing is left to write the file again.
        writer.save(a.clone(), transcript("third"));
        writer.forget(a.clone());
        writer.flush();
        let _ = std::fs::remove_file(&a);
        writer.save(b.clone(), transcript("last"));
        // Everything handed over is on disk once this returns.
        writer.finish();
        assert!(!a.exists());
        assert_eq!(last(&b), "last");
        // Stopped: a late save or flush is dropped, not a panic or a wait.
        writer.save(a.clone(), transcript("late"));
        assert!(writer.flush().is_empty());
        assert!(!a.exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_save_that_fails_is_kept_until_it_can_be_written() {
        let dir = std::env::temp_dir().join(format!("arakun-server-full-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        // A file where the folder should be: nothing can be written under it.
        let blocked = dir.join("sessions");
        std::fs::write(&blocked, b"in the way").unwrap();
        let (a, b) = (blocked.join("a.json"), blocked.join("b.json"));
        let last = |path: &Path| last_message(&Thread::read_transcript(path).unwrap().entries);
        let mut writer = Writer::start().unwrap();
        writer.save(a.clone(), transcript("one"));
        writer.save(b.clone(), transcript("gone"));
        // The flush says which files are behind, so nothing restarts from them.
        let mut behind = writer.flush();
        behind.sort();
        assert_eq!(behind, [a.clone(), b.clone()]);
        // Only a file that is owed is behind, and asking about another waits on nothing.
        assert!(writer.behind(&a) && writer.behind(&b));
        assert!(!writer.behind(&dir.join("sessions/other.json")));
        // A newer save takes the kept one's place; a removed session's is dropped.
        writer.save(a.clone(), transcript("two"));
        writer.forget(b.clone());
        assert_eq!(writer.flush(), std::slice::from_ref(&a));
        assert!(!writer.behind(&b));
        // The way is clear again: what was kept is written, the newest of it.
        std::fs::remove_file(&blocked).unwrap();
        assert!(!writer.behind(&a));
        assert_eq!(last(&a), "two");
        assert!(!b.exists());
        writer.finish();
        let _ = std::fs::remove_dir_all(&dir);
    }
}
