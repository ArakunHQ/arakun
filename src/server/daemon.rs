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
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use futures::StreamExt as _;
use futures::channel::mpsc;
use gpui::{App, AppContext as _, Context, Entity, Global, Subscription, Task};
use serde_json::json;

use super::local::{self, Reply, Request};
use super::pty::{self, Pty};
use super::store::{self, Place, Record, Store};
use crate::account::Account;
use crate::agent::log::render_log;
use crate::agent::{
    self, AgentKind, Entry, Setting, SettingOption, Thread, ThreadEvent, ThreadStatus, Transcript,
    catalog, marketplace, workers_ai,
};
use crate::clone_source;
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
/// How much of a terminal's output may be on its way to a device before it
/// says it drew some: `yes` must not bury the connection.
const TERM_WINDOW: u64 = 128 * 1024;
/// The most one `term_output` carries.
const TERM_CHUNK: usize = 32 * 1024;
/// How long a terminal no device follows keeps running: through a laptop's
/// sleep, but not forever once the app that opened it is gone.
const TERM_ALONE: Duration = Duration::from_secs(60 * 60);

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

/// A terminal a device opened (`pty`), and the devices that follow it.
struct Term {
    pty: Pty,
    place: PathBuf,
    following: HashMap<ClientId, Follower>,
    /// Since when no device follows it.
    alone_since: Option<Instant>,
}

/// How far a device has a terminal's output.
#[derive(Clone, Copy, Default)]
struct Follower {
    /// The offset sent up to.
    sent: u64,
    /// The offset it said it drew up to.
    seen: u64,
    /// It was told the terminal ended.
    told_exit: bool,
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
    /// The sessions each device is looking at: one for a phone, the ones it
    /// follows for a desktop.
    watching: HashMap<ClientId, Vec<String>>,
    /// Devices whose page left the screen: they still get updates, but a push too.
    hidden: HashSet<ClientId>,
    /// Places whose worktree is being deleted: nothing new starts in them.
    deleting: HashSet<PathBuf>,
    /// How many worktrees were deleted so far: a folder looked at before one
    /// went may be that one.
    deletions: u64,
    /// Sessions whose missing adapter the server installed: one try each, so
    /// an install that does not help is not run again and again.
    installed_for: HashSet<String>,
    /// The device that asked each session for a fork or a rewind: the new
    /// session is told to it.
    asked: HashMap<String, ClientId>,
    /// Rewinds a device asked for, until their plan is sent or they ran.
    rewinds: HashMap<String, Rewinding>,
    /// Sessions being handed to another agent, until the new one is saved.
    handing_over: HashSet<String>,
    /// Sessions whose entries changed since the last flush, with the lowest index that did.
    dirty: HashMap<String, usize>,
    /// Sessions whose status, permission or usage changed since the last flush.
    state_dirty: Vec<String>,
    inbox_dirty: bool,
    flush: Option<Task<()>>,
    last_inbox: String,
    last_usage: String,
    /// Folders a device's clone is going into, until it is done.
    cloning: Clones,
    /// What git says of each place, read to the side ([`PlaceMeta`]): a
    /// folder on a mount that stops answering would hold up every device.
    meta: HashMap<PathBuf, PlaceMeta>,
    /// A read of every place's [`PlaceMeta`] is under way.
    reading_meta: bool,
    uploads: phone::Uploads,
    /// The last thing each session notified about, so one turn says it once.
    notified: HashMap<String, Notice>,
    writer: Writer,
    /// The saved transcript a device read last, for the pages after the first.
    last_read: agent::saved::LastRead,
    code: Rc<Cell<i32>>,
    stopping: bool,
    /// The local socket, until the server stops.
    listening: Option<local::Listening>,
    /// Terminals devices opened, by the id the device gave each.
    terms: HashMap<String, Term>,
    /// Hears a terminal's id when it printed or ended.
    term_wake: mpsc::UnboundedSender<String>,
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

/// Writes what is kept, in the order it was handed over. What is written is
/// no longer owed, unless a newer save of the file was handed over meanwhile;
/// what fails stays kept. A failure is said once per file, until it is written.
fn write_all(kept: &mut Vec<Snapshot>, reported: &mut HashSet<PathBuf>, owed: &Owed) {
    kept.retain(
        |(path, number, transcript)| match write_transcript(path, transcript) {
            Ok(()) => {
                reported.remove(path);
                let mut owed = owed.lock().unwrap();
                if owed.get(path) == Some(number) {
                    owed.remove(path);
                }
                false
            }
            Err(e) => {
                if reported.insert(path.clone()) {
                    eprintln!(
                        "arakun server: could not save {}: {e}; it is kept and tried again",
                        path.display()
                    );
                }
                true
            }
        },
    );
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
                // Snapshots not on disk yet: those handed over, and those whose
                // write failed. Each file has only its newest. A failed one is
                // tried again on every flush, not only when a batch of jobs
                // starts: a flush may come in the batch that kept it.
                let mut kept: Vec<Snapshot> = Vec::new();
                let mut reported: HashSet<PathBuf> = HashSet::new();
                while let Ok(first) = rx.recv() {
                    for job in std::iter::once(first).chain(rx.try_iter()) {
                        match job {
                            Job::Save(path, number, transcript) => {
                                kept.retain(|s| s.0 != path);
                                kept.push((path, number, transcript));
                            }
                            Job::Forget(path) => {
                                kept.retain(|s| s.0 != path);
                                reported.remove(&path);
                            }
                            Job::Flush(done) => {
                                write_all(&mut kept, &mut reported, &owed);
                                let _ = done.send(kept.iter().map(|s| s.0.clone()).collect());
                            }
                        }
                    }
                    write_all(&mut kept, &mut reported, &owed);
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
    // The socket first: it is what says a server already runs in this folder.
    // A second `serve` must be turned away before it saves how it was started
    // over what the running one, and its next start, go by.
    let socket = super::socket_path();
    let (listening, requests) = match local::serve(&socket) {
        Ok(found) => found,
        Err(e) => {
            eprintln!("arakun server: {e}");
            return 1;
        }
    };
    // Before a device can reach it: a server that cannot read what it kept does not start.
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
        env!("ARAKUN_VERSION"),
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
pub(super) fn make_private_dir(dir: &Path) -> std::io::Result<()> {
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
            "{why} Trying again in {}.",
            relay::wait_text(retry_in)
        )),
        relay::Status::Refused { why, retry_in } => Address::Problem(format!(
            "{why} Trying again in {}.",
            relay::wait_text(retry_in)
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

/// Whether a file is there. `Path::exists` says "no" when it could not find
/// out (a folder this user may not look in, a disk that went away), and a
/// transcript taken for missing gets a new session saved over it.
fn there(path: &Path) -> std::io::Result<bool> {
    path.try_exists()
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

/// Stops a session's turn and drops the follow-ups that waited behind it;
/// says how many. In the app they would stay, held, until someone lets them
/// go or removes them. Nobody sees them here: held, they would stand in front
/// of every prompt sent after, and none of those would run.
fn stop_turn(thread: &Entity<Thread>, cx: &mut App) -> usize {
    thread.update(cx, |t, cx| {
        t.cancel(cx);
        let waited = t.queued().len();
        for _ in 0..waited {
            t.remove_queued(0, cx);
        }
        waited
    })
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
        let (term_wake, mut term_woken) = mpsc::unbounded::<String>();
        tasks.push(cx.spawn(async move |this, cx| {
            while let Some(first) = term_woken.next().await {
                let mut ids = vec![first];
                while let Ok(id) = term_woken.try_recv() {
                    if !ids.contains(&id) {
                        ids.push(id);
                    }
                }
                let sent = this.update(cx, |this, _| {
                    for id in ids {
                        this.term_flush(&id);
                    }
                });
                if sent.is_err() {
                    break;
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
        tasks.push(cx.background_spawn(crate::guard::task(async {
            if catalog::droid_native() && agent::droid_bin().is_ok() {
                catalog::refresh();
            }
            if agent::claude_cli().is_some() {
                catalog::refresh_claude();
            }
            if agent::which("opencode").is_some() {
                catalog::refresh_opencode();
            }
            catalog::refresh_codex();
            catalog::refresh_devin();
        })));
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
            deleting: HashSet::new(),
            deletions: 0,
            installed_for: HashSet::new(),
            asked: HashMap::new(),
            rewinds: HashMap::new(),
            handing_over: HashSet::new(),
            dirty: HashMap::new(),
            state_dirty: Vec::new(),
            inbox_dirty: false,
            flush: None,
            last_inbox: String::new(),
            last_usage: String::new(),
            cloning: Clones::default(),
            uploads: phone::Uploads::default(),
            meta: HashMap::new(),
            reading_meta: false,
            notified: HashMap::new(),
            writer: parts.writer,
            last_read: Default::default(),
            code: parts.code,
            stopping: false,
            listening: Some(parts.listening),
            terms: HashMap::new(),
            term_wake,
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
        let thread = if there(&transcript).map_err(|e| {
            format!(
                "the transcript of {id} could not be looked for ({e}); nothing is sent to it until {} can be read",
                transcript.display()
            )
        })? {
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
        self.watching.values().flatten().any(|w| w == id)
    }

    /// Whether a device has the session `id` on a screen it shows.
    fn seen(&self, id: &str) -> bool {
        self.watching
            .iter()
            .any(|(client, w)| w.iter().any(|w| w == id) && !self.hidden.contains(client))
    }

    /// Whether `place` is in a worktree being deleted.
    fn being_deleted(&self, place: &Path) -> bool {
        inside_any(place, &self.deleting)
    }

    /// Whether new work may start in `place`: every path that starts or
    /// sends a turn asks, the device's and the local commands' alike.
    fn admit(&self, place: &Path) -> Result<(), String> {
        admission(place, &self.deleting)
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
        // Nobody sits at a server to press Install: a missing Claude or Codex
        // adapter is installed with npm at once, and once. Decided before the
        // notice: the error that asks for the install is about to be fixed,
        // so it is not pushed to the devices or kept as the session's status.
        // An install that fails reports its own error the usual way.
        let installing = matches!(
            ev,
            ThreadEvent::StatusChanged | ThreadEvent::NeedsPermission
        ) && self.install_missing_adapter(id, thread, cx);
        if !installing {
            self.notify(id, thread, ev, cx);
        }
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
            ThreadEvent::StatusChanged | ThreadEvent::NeedsPermission if installing => {}
            ThreadEvent::StatusChanged | ThreadEvent::NeedsPermission => {
                let status = status_of(thread.read(cx));
                if let Some(s) = self.store.session_mut(id)
                    && s.status != status
                {
                    s.status = status;
                    s.updated = unix_now();
                }
                // A rewind asked for while the agent was starting.
                self.rewind_step(id, thread, cx);
            }
            ThreadEvent::RewindChanged { .. } => self.rewind_step(id, thread, cx),
            ThreadEvent::Forked(meta) => self.add_fork(id, meta.clone(), None, cx),
            ThreadEvent::Rewound { meta, prompt } => {
                self.add_fork(id, meta.clone(), Some(prompt.clone()), cx)
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

    /// Session `id`'s thread, for something that changes its files or its
    /// place: not while that place is being deleted.
    fn thread_to_change(
        &mut self,
        id: &str,
        cx: &mut Context<Self>,
    ) -> Result<Entity<Thread>, String> {
        let place = self
            .store
            .session(id)
            .ok_or("that session is gone")?
            .place
            .clone();
        self.admit(&place)?;
        self.ensure_live(id, cx)
    }

    /// Takes the rewind a device asked of session `id` one step on: its
    /// agent starts first, then the plan is asked of it, then sent to the
    /// device or, when it was confirmed, run.
    fn rewind_step(&mut self, id: &str, thread: &Entity<Thread>, cx: &mut Context<Self>) {
        let Some(r) = self.rewinds.get(id).copied() else {
            return;
        };
        let t = thread.read(cx);
        let plan = t.rewind_plan().filter(|p| p.entry == r.entry);
        let failed = match plan {
            Some(p) if p.loading || p.running => return,
            Some(_) if r.confirm => {
                self.rewinds
                    .insert(id.to_string(), Rewinding { driving: true, ..r });
                thread.update(cx, |t, cx| t.confirm_rewind(cx));
                return;
            }
            Some(p) => {
                let msg = rewind_plan_message(id, p);
                self.rewinds.remove(id);
                self.transport.send(r.client, msg);
                return;
            }
            // It ran, failed or was dropped; the session says which.
            None if r.driving => None,
            None if matches!(t.status(), ThreadStatus::Connecting) => return,
            None if t.is_busy() => Some("it is working; rewind once the turn is done"),
            None if !t.can_rewind(r.entry) => Some("its agent there cannot rewind to that prompt"),
            None => {
                // A confirm whose plan is gone (another took its place) only
                // asks for a new one: the device reviews it before it runs.
                let r = Rewinding {
                    driving: true,
                    confirm: false,
                    ..r
                };
                self.rewinds.insert(id.to_string(), r);
                thread.update(cx, |t, cx| t.begin_rewind(r.entry, cx));
                return;
            }
        };
        self.rewinds.remove(id);
        self.transport
            .send(r.client, rewind_gone(id, r.entry, failed));
    }

    /// Session `from` was forked, or rewound with `prompt` taken out, into a
    /// new session of the agent's (`meta`): a session of its own here, in the
    /// same place, and told to the device that asked.
    fn add_fork(
        &mut self,
        from: &str,
        meta: agent::SessionMeta,
        prompt: Option<String>,
        cx: &mut Context<Self>,
    ) {
        let Some(parent) = self.store.session(from) else {
            return;
        };
        let (place, agent) = (parent.place.clone(), parent.agent);
        let id = store::new_id();
        let now = unix_now();
        self.store.sessions.insert(
            0,
            Record {
                id: id.clone(),
                title: meta.title.clone(),
                agent,
                place,
                created: now,
                updated: now,
                added: 0,
                removed: 0,
                meta: Some(meta),
                failed: false,
                status: Status::Idle,
            },
        );
        self.save_store();
        // The inbox goes first: the device opens the session it holds.
        self.sync_inbox(cx);
        eprintln!("arakun server: {id} forked from {from}");
        if let Some(client) = self.asked.remove(from) {
            let msg = match prompt {
                Some(prompt) => {
                    json!({ "type": "rewound", "id": from, "session": id, "prompt": prompt })
                }
                None => json!({ "type": "forked", "id": from, "session": id }),
            };
            self.transport.send(client, msg.to_string());
        }
    }

    /// Hands session `id` to `agent`, in the same place, as "Continue with"
    /// does in the app: a session of `agent` takes its place, showing the
    /// conversation so far and told about it with the next prompt, and the
    /// old one is gone. `settings` are picked for `agent` there first.
    /// `client` is told the new session, or why there is none.
    fn continue_with(
        &mut self,
        client: ClientId,
        id: String,
        agent: &str,
        settings: &[(String, String)],
        cx: &mut Context<Self>,
    ) {
        let (old, agent, entries) = match self.can_continue(&id, agent, cx) {
            Ok(ready) => ready,
            Err(e) => return self.transport.send(client, session_error(&id, &e)),
        };
        if let Some(place) = self.store.place_mut(&old.place) {
            for (key, value) in settings {
                if let Some(s) = device_setting(key) {
                    place.pick(agent, s, value);
                }
            }
        }
        let picked = self
            .store
            .place(&old.place)
            .map(|p| p.picked(agent))
            .unwrap_or_default();
        let talked = entries.iter().any(|e| matches!(e, Entry::User { .. }));
        let linked = self.store.linked_path(&id);
        let markdown = talked
            .then(|| agent::notes::transcript_markdown(&old.title, old.agent.label(), &entries));
        let thread = if talked {
            let handoff = agent::notes::handoff(
                old.agent.label(),
                &old.title,
                &entries,
                &linked.to_string_lossy(),
                agent::notes::HANDOFF_BUDGET,
            );
            Thread::continued(
                agent,
                old.place.clone(),
                old.title.clone(),
                entries,
                handoff,
                cx,
            )
        } else {
            // Nothing was said yet: a fresh session of `agent`, nothing to keep.
            Thread::start(agent, old.place.clone(), None, cx)
        };
        thread.update(cx, |t, cx| {
            // Queued until the agent's session exists.
            for (s, value) in &picked {
                t.set_setting(*s, value, cx);
            }
        });
        let new = store::new_id();
        let path = self.store.transcript_path(&new);
        let transcript = thread.read(cx).transcript(cx);
        self.handing_over.insert(id.clone());
        // The old session goes only once the new one is on disk; the files
        // are written to the side, so a slow disk holds up nothing else.
        cx.spawn(async move |this, cx| {
            let (p, l) = (path.clone(), linked);
            let written = cx
                .background_spawn(crate::guard::task(async move {
                    if let Some(markdown) = markdown {
                        l.parent()
                            .map_or(Ok(()), std::fs::create_dir_all)
                            .and_then(|_| std::fs::write(&l, markdown))
                            .map_err(|e| format!("could not write {}: {e}", l.display()))?;
                    }
                    write_transcript(&p, &transcript)
                        .map_err(|e| format!("could not write {}: {e}", p.display()))
                }))
                .await;
            let _ = this.update(cx, |this, cx| {
                this.handing_over.remove(&id);
                // A prompt may have reached the old one meanwhile.
                let written = written.and_then(|()| {
                    this.can_continue(&id, agent.id(), cx)
                        .map(|(old, _, _)| old)
                });
                let old = match written {
                    Ok(old) => old,
                    Err(e) => {
                        thread.update(cx, |t, _| t.shutdown());
                        let _ = std::fs::remove_file(&path);
                        return this.transport.send(client, session_error(&id, &e));
                    }
                };
                let at = this
                    .store
                    .sessions
                    .iter()
                    .position(|s| s.id == id)
                    .unwrap_or(0);
                this.drop_session(&id, cx);
                let now = unix_now();
                this.store.sessions.insert(
                    at.min(this.store.sessions.len()),
                    Record {
                        id: new.clone(),
                        title: old.title.clone(),
                        agent,
                        place: old.place.clone(),
                        created: now,
                        updated: now,
                        added: old.added,
                        removed: old.removed,
                        meta: None,
                        failed: false,
                        status: Status::Idle,
                    },
                );
                this.attach(&new, thread, cx);
                this.save_store();
                // The inbox goes first: the device opens the session it holds.
                this.sync_inbox(cx);
                eprintln!("arakun server: {new} continues {id} with {}", agent.label());
                let msg = json!({ "type": "continued", "id": id, "session": new });
                this.transport.send(client, msg.to_string());
            });
        })
        .detach();
    }

    /// Whether session `id` can be handed to `agent` now: its record, the
    /// agent, and the entries it has so far.
    fn can_continue(
        &self,
        id: &str,
        agent: &str,
        cx: &App,
    ) -> Result<(Record, AgentKind, Vec<Entry>), String> {
        let agent = AgentKind::parse(agent).ok_or_else(|| unknown_agent(agent))?;
        let old = self
            .store
            .session(id)
            .ok_or("that session is gone")?
            .clone();
        self.admit(&old.place)?;
        if old.agent == agent {
            return Err(format!("it already runs {}", agent.label()));
        }
        if self.rewinds.contains_key(id) {
            return Err("it is being rewound".into());
        }
        let entries = match self.thread(id) {
            Some(thread) => {
                let t = thread.read(cx);
                if matches!(status_of(t), Status::Working | Status::NeedsInput) || t.in_background()
                {
                    return Err(
                        "it is working; continue with another agent once the turn is done".into(),
                    );
                }
                // The last turn's shell changes are not in its entries yet.
                if t.shell_pending().is_some_and(|p| p.peek().is_none()) {
                    return Err(
                        "it is still recording its shell changes; try again in a moment".into(),
                    );
                }
                t.entries().to_vec()
            }
            None => self.saved_entries(id)?,
        };
        Ok((old, agent, entries))
    }

    /// The entries saved for session `id`, whose agent is not running: none
    /// when it has no transcript yet, and an error when the one it has
    /// cannot be read, or its last save is not written.
    fn saved_entries(&self, id: &str) -> Result<Vec<Entry>, String> {
        let path = self.store.transcript_path(id);
        if self.writer.behind(&path) {
            return Err(format!(
                "the last turn of {id} could not be saved to {} (is the disk full?)",
                path.display()
            ));
        }
        if !there(&path).map_err(|e| format!("its transcript could not be looked for ({e})"))? {
            return Ok(Vec::new());
        }
        Thread::read_transcript(&path)
            .map(|t| t.entries)
            .map_err(|e| format!("its transcript could not be read ({e:#})"))
    }

    /// Stages with git the files session `id` changed, and tells `client`.
    fn stage(&mut self, client: ClientId, id: String, cx: &mut Context<Self>) {
        let Some(place) = self.store.session(&id).map(|r| r.place.clone()) else {
            self.transport
                .send(client, session_error(&id, "that session is gone"));
            return;
        };
        if let Err(e) = self.admit(&place) {
            self.transport.send(client, staged_message(&id, Err(e)));
            return;
        }
        // What the last turn's shell commands changed may still be worked out.
        let pending = self
            .live
            .get(&id)
            .and_then(|l| l.thread.read(cx).shell_pending());
        cx.spawn(async move |this, cx| {
            if let Some(pending) = pending {
                pending.await;
            }
            let Ok(files) = this.update(cx, |this, cx| {
                this.with_entries(&id, cx, agent::edited_files)
                    .into_iter()
                    .filter_map(|p| {
                        Some(p.strip_prefix(&place).ok()?.to_string_lossy().into_owned())
                    })
                    .filter(|p| !p.is_empty())
                    .collect::<Vec<String>>()
            }) else {
                return;
            };
            // Git is run to the side: a repository that hangs holds up nothing else.
            let staged = cx
                .background_spawn(crate::guard::task(async move {
                    crate::git::stage_only(&place, &files).map_err(|e| e.to_string())
                }))
                .await;
            let _ = this.update(cx, |this, _| {
                this.transport.send(client, staged_message(&id, staged))
            });
        })
        .detach();
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
                self.last_read.with(&path, f)
            }
        }
    }

    fn tick(&mut self, cx: &mut Context<Self>) {
        if self.stopping {
            return;
        }
        self.last_read.trim();
        // Dropping one hangs it up.
        self.terms.retain(|_, t| {
            t.alone_since
                .is_none_or(|since| since.elapsed() < TERM_ALONE)
        });
        self.read_meta(cx);
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
        self.terms.clear();
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
        // Git runs in a process group of its own, which the terminal's Ctrl-C
        // does not reach: left alone, a clone would go on writing a folder
        // that no place names, or leave half of one.
        self.cloning.stop_all();
        let given_up = Instant::now() + CLONE_STOP_WAIT;
        while self.cloning.busy() && Instant::now() < given_up {
            std::thread::sleep(Duration::from_millis(20));
        }
        // The process ends without dropping the app, so the socket goes here.
        self.listening = None;
        if self.reach == Reach::Tailscale {
            let _ = tailscale::stop_serving();
        }
        self.code.set(0);
        cx.quit();
    }

    // ---- terminals ----------------------------------------------------------------

    /// Opens a terminal in `place` for `client`, which follows it from its start.
    fn term_open(
        &mut self,
        client: ClientId,
        term: String,
        place: PathBuf,
        run: Option<String>,
        size: (u16, u16),
    ) {
        if self.terms.contains_key(&term) {
            // Sent again after a drop: it is open already.
            return;
        }
        let opened = self
            .admit(&place)
            .and_then(|()| match self.store.place(&place) {
                Some(_) => Ok(()),
                None => Err("that folder is not one of this server's".to_string()),
            })
            .and_then(|()| {
                let command = run.as_deref().map(crate::platform::run_then_shell);
                Pty::spawn(
                    term.clone(),
                    place.clone(),
                    command,
                    size,
                    self.term_wake.clone(),
                )
                .map_err(|e| format!("could not start a shell in {}: {e}", place.display()))
            });
        match opened {
            Ok(pty) => {
                let following = HashMap::from([(client, Follower::default())]);
                self.terms.insert(
                    term,
                    Term {
                        pty,
                        place,
                        following,
                        alone_since: None,
                    },
                );
            }
            Err(e) => {
                eprintln!("arakun server: {e}");
                self.transport.send(client, error_about("term_open", &e));
                self.transport.send(client, term_exit(&term, None));
            }
        }
    }

    /// Hangs up the terminals in `place` or a folder in it, and tells their devices.
    fn hang_up_terms_in(&mut self, place: &Path) {
        let ids: Vec<String> = self
            .terms
            .iter()
            .filter(|(_, t)| t.place.starts_with(place))
            .map(|(id, _)| id.clone())
            .collect();
        for id in ids {
            if let Some(t) = self.terms.remove(&id) {
                for client in t.following.keys() {
                    self.transport.send(*client, term_exit(&id, None));
                }
            }
        }
    }

    /// Sends each device following terminal `id` what it has not had yet, as
    /// far as its window goes, and that it ended once all of it went.
    fn term_flush(&mut self, id: &str) {
        let Some(t) = self.terms.get_mut(id) else {
            return;
        };
        let out = t.pty.output();
        for (client, f) in &mut t.following {
            let (chunks, ended) = term_due(&out, f);
            for (at, bytes) in chunks {
                self.transport.send(*client, term_output(id, at, &bytes));
            }
            if let Some(code) = ended {
                self.transport.send(*client, term_exit(id, code));
            }
        }
    }

    // ---- devices ----------------------------------------------------------------

    fn scope(&self, cwd: &Path) -> wire::Scope {
        wire::Scope {
            cwd: cwd.to_path_buf(),
            key: Some(self.access.lock().unwrap().image_key.clone()),
        }
    }

    /// Writes the devices as the transport now knows them (paired, seen, unpaired).
    fn save_devices(&mut self) {
        self.settings.devices = self.access.lock().unwrap().devices.clone();
        self.settings.save();
        self.transport.sync_tickets(&self.access);
    }

    /// Reads every place's [`PlaceMeta`] to the side, and sends the inbox
    /// again when it changed. One read at a time.
    fn read_meta(&mut self, cx: &mut Context<Self>) {
        if self.reading_meta {
            return;
        }
        self.reading_meta = true;
        let paths: Vec<PathBuf> = self.store.places.iter().map(|p| p.path.clone()).collect();
        cx.spawn(async move |this, cx| {
            let read: HashMap<PathBuf, PlaceMeta> = cx
                .background_executor()
                .spawn(async move {
                    paths
                        .into_iter()
                        .map(|p| {
                            let meta = PlaceMeta::read(&p);
                            (p, meta)
                        })
                        .collect()
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                this.reading_meta = false;
                // A place added while it was read keeps what was read for it then.
                let mut meta = read;
                for (path, m) in &this.meta {
                    if this.store.place(path).is_some() && !meta.contains_key(path) {
                        meta.insert(path.clone(), m.clone());
                    }
                }
                if meta != this.meta {
                    this.meta = meta;
                    this.sync_inbox(cx);
                }
            });
        })
        .detach();
    }

    /// What git said of `path` last.
    fn meta_of(&self, path: &Path) -> PlaceMeta {
        self.meta.get(path).cloned().unwrap_or_default()
    }

    fn place_label(&self, path: &Path) -> String {
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| path.display().to_string());
        match self.meta_of(path).branch {
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
                let PlaceMeta { branch, repo } = self.meta_of(&p.path);
                // Worktrees of one repository are one project, named for it.
                let project = repo
                    .as_ref()
                    .and_then(|(root, _)| root.file_name())
                    .map_or_else(|| name.clone(), |n| n.to_string_lossy().into_owned());
                wire::inbox_place(wire::InboxPlace {
                    path: &p.path,
                    project: &project,
                    env: &name,
                    branch: branch.as_deref(),
                    agent: p.agent.id(),
                    repo: repo.as_ref().map(|(root, _)| root.as_path()),
                    origin: repo
                        .as_ref()
                        .and_then(|(_, o)| o.as_deref())
                        .map(without_login)
                        .as_deref(),
                })
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
                    place: &s.place,
                    pr: s.meta.as_ref().and_then(|m| m.pr.as_deref()),
                    asks: self
                        .live
                        .get(&s.id)
                        .and_then(|l| wire::asks(l.thread.read(cx))),
                })
            })
            .collect();
        wire::server_inbox(&phone::host_name(), sessions, places)
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
            if watched.iter().any(|w| w == id) {
                self.transport.send(*client, msg.clone());
            }
        }
    }

    /// A device looked away from its session: the agent may stop if no one else is looking.
    fn stop_watching(&mut self, client: ClientId, cx: &mut Context<Self>) {
        for id in self.watching.remove(&client).unwrap_or_default() {
            self.retire_if_unwatched(&id, cx);
        }
    }

    /// A device looked away from `id` only.
    fn stop_watching_one(&mut self, client: ClientId, id: &str, cx: &mut Context<Self>) {
        if let Some(watched) = self.watching.get_mut(&client) {
            watched.retain(|w| w != id);
            if watched.is_empty() {
                self.watching.remove(&client);
            }
        }
        self.retire_if_unwatched(id, cx);
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
                self.asked.retain(|_, c| *c != id);
                self.rewinds.retain(|_, r| r.client != id);
                // Its terminals keep running, for when it comes back.
                for term in self.terms.values_mut() {
                    if term.following.remove(&id).is_some() && term.following.is_empty() {
                        term.alone_since = Some(Instant::now());
                    }
                }
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
                        relay::Status::Refused { why, retry_in } => Some(format!(
                            "the relay turned this server away: {why} Trying again in {}.",
                            relay::wait_text(*retry_in)
                        )),
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
        let gone = |session: &str| session_error(session, "that session is gone");
        match msg {
            ClientMsg::Open { session, keep } => {
                let Some(record) = self.store.session(&session) else {
                    self.transport.send(client, gone(&session));
                    return;
                };
                let place = self.place_label(&record.place);
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
                let watched = self.watching.entry(client).or_default();
                if keep {
                    if !watched.contains(&session) {
                        watched.push(session.clone());
                    }
                } else {
                    // The sessions it looked at before have one watcher fewer.
                    let before = std::mem::replace(watched, vec![session.clone()]);
                    for b in before.iter().filter(|b| **b != session) {
                        self.retire_if_unwatched(b, cx);
                    }
                }
                self.transport.send(client, reply);
            }
            ClientMsg::Close { session: None } => self.stop_watching(client, cx),
            ClientMsg::Close {
                session: Some(session),
            } => self.stop_watching_one(client, &session, cx),
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
                now,
            } => {
                let text = self.uploads.attach(text, &uploads);
                if let Err(e) = self.send_to_session(&session, text, now, cx) {
                    self.transport.send(client, session_error(&session, &e));
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
                mentions_from,
            } => {
                let path = PathBuf::from(env);
                if let Err(e) = self.admit(&path) {
                    self.transport.send(client, error_about("start", &e));
                    return;
                }
                // A first prompt written in the main folder and started in a
                // new worktree: a file it names that git did not check out
                // there (untracked, ignored) is still found where it was
                // written, as the app does with its own worktrees. Only from
                // one of this server's folders.
                let text = match mentions_from.map(PathBuf::from) {
                    Some(from)
                        if self.admit(&from).is_ok() && self.store.place(&from).is_some() =>
                    {
                        agent::attach::move_mentions(&text, &from, &path)
                    }
                    _ => text,
                };
                let Some(place) = self.store.place_mut(&path) else {
                    // Said to be about the start: the app waits for its answer.
                    self.transport
                        .send(client, error_about("start", "that project is gone"));
                    return;
                };
                let agent = agent
                    .as_deref()
                    .and_then(AgentKind::parse)
                    .unwrap_or(place.agent);
                let text = text.trim().to_string();
                if text.is_empty() {
                    // Answered all the same: the app pairs each answer with
                    // its oldest start, so a silent one would shift the rest.
                    self.transport
                        .send(client, error_about("start", "the prompt is empty"));
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
                        session_error(&session, "that setting is not one a device changes"),
                    );
                    return;
                };
                let Some(thread) = self.thread(&session) else {
                    self.transport.send(
                        client,
                        session_error(&session, "that session is not running"),
                    );
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
                    stop_turn(&thread, cx);
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
                    self.transport.send(client, session_error(&session, &e));
                }
            }
            ClientMsg::SkipQuestion { session } => {
                if let Some(thread) = self.thread(&session) {
                    thread.update(cx, |t, cx| t.skip_question(cx));
                }
            }
            ClientMsg::Fork { session } => match self.thread_to_change(&session, cx) {
                Ok(thread) => {
                    self.asked.insert(session.clone(), client);
                    // Once its agent has started, if it had to.
                    thread.update(cx, |t, cx| t.fork(cx));
                }
                Err(e) => self.transport.send(client, session_error(&session, &e)),
            },
            ClientMsg::Compact { session } => match self.thread_to_change(&session, cx) {
                Ok(thread) if thread.read(cx).is_busy() => self.transport.send(
                    client,
                    session_error(&session, "it is working; compact it once the turn is done"),
                ),
                Ok(thread) => thread.update(cx, |t, cx| t.compact(None, cx)),
                Err(e) => self.transport.send(client, session_error(&session, &e)),
            },
            ClientMsg::Rewind {
                session,
                entry,
                confirm,
            } => match self.thread_to_change(&session, cx) {
                Ok(thread) => {
                    // Only a rewind that runs makes a session to reply with; a
                    // preview must not take a pending fork's reply.
                    if confirm {
                        self.asked.insert(session.clone(), client);
                    }
                    let r = Rewinding {
                        client,
                        entry,
                        confirm,
                        driving: false,
                    };
                    self.rewinds.insert(session.clone(), r);
                    self.rewind_step(&session, &thread, cx);
                }
                Err(e) => self
                    .transport
                    .send(client, rewind_gone(&session, entry, Some(&e))),
            },
            ClientMsg::ContinueWith {
                session,
                agent,
                settings,
            } => {
                if self.handing_over.contains(&session) {
                    let e = "it is being handed to another agent already";
                    self.transport.send(client, session_error(&session, e));
                } else {
                    self.continue_with(client, session, &agent, &settings, cx);
                }
            }
            ClientMsg::Stage { session } => self.stage(client, session, cx),
            ClientMsg::Changes { session } => {
                if self.store.session(&session).is_none() {
                    self.transport.send(client, gone(&session));
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
                    self.transport.send(client, gone(&session));
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
                    None => session_error(&session, "that change is gone"),
                };
                self.transport.send(client, reply);
            }
            ClientMsg::Transcript { session, from } => {
                let Some(record) = self.store.session(&session) else {
                    self.transport.send(client, gone(&session));
                    return;
                };
                let (agent, place) = (record.agent.id(), record.place.clone());
                let frames = self.with_entries(&session, cx, |all, _| {
                    wire::transcript_message(&session, agent, &place, all, from)
                });
                for frame in frames {
                    self.transport.send(client, frame);
                }
            }
            ClientMsg::TermOpen {
                term,
                place,
                run,
                cols,
                rows,
            } => self.term_open(client, term, PathBuf::from(place), run, (cols, rows)),
            ClientMsg::TermAttach { term, from } => {
                let Some(t) = self.terms.get_mut(&term) else {
                    // The server started again since, or it was hung up.
                    self.transport.send(client, term_exit(&term, None));
                    return;
                };
                t.alone_since = None;
                t.following.insert(
                    client,
                    Follower {
                        sent: from,
                        seen: from,
                        told_exit: false,
                    },
                );
                self.term_flush(&term);
            }
            ClientMsg::TermInput { term, data } => {
                use base64::Engine as _;
                let bytes = base64::engine::general_purpose::STANDARD.decode(data);
                if let (Some(t), Ok(bytes)) = (self.terms.get(&term), bytes) {
                    t.pty.write(bytes);
                }
            }
            ClientMsg::TermResize { term, cols, rows } => {
                if let Some(t) = self.terms.get(&term) {
                    t.pty.resize(cols, rows);
                }
            }
            ClientMsg::TermSeen { term, at } => {
                if let Some(f) = self
                    .terms
                    .get_mut(&term)
                    .and_then(|t| t.following.get_mut(&client))
                {
                    f.seen = f.seen.max(at).min(f.sent);
                }
                self.term_flush(&term);
            }
            ClientMsg::TermClose { term } => {
                self.terms.remove(&term);
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
                            // How Claude Code is signed in on this machine: a
                            // key or an endpoint has no plan limits to show.
                            agent::claude_auth::access();
                        })
                        .await;
                    let _ = this.update(cx, |this, cx| this.usage_changed(cx));
                })
                .detach();
                let usage = self.usage(cx);
                self.transport.send(client, usage);
            }
            ClientMsg::Market => {
                self.answer_off_thread(client, marketplace::report, cx);
            }
            ClientMsg::InstallPlugin { id } => {
                self.answer_off_thread(client, move || marketplace::serve_plugin(&id), cx);
            }
            ClientMsg::InstallServer {
                name,
                install,
                values,
            } => {
                self.answer_off_thread(
                    client,
                    move || marketplace::serve_server(&name, &install, &values),
                    cx,
                );
            }
            ClientMsg::Visible { visible } => {
                if visible {
                    self.hidden.remove(&client);
                } else {
                    self.hidden.insert(client);
                }
            }
            ClientMsg::AddPlace { path } => self.add_device_place(client, path, cx),
            ClientMsg::Clone { url } => {
                let Some(source) = clone_source::parse_clone_source(&url) else {
                    self.transport.send(
                        client,
                        place_failed(
                            "That is not a repository to clone: give its URL or owner/repo.",
                        ),
                    );
                    return;
                };
                // Git would send this machine's logins for the host in the clear.
                if source.url.starts_with("http://") {
                    self.transport.send(
                        client,
                        place_failed(
                            "Give an https:// or ssh address: plain http is not encrypted.",
                        ),
                    );
                    return;
                }
                let home = crate::platform::home();
                let parent = clone_source::clone_parent(
                    self.store.places.iter().map(|p| p.path.as_path()),
                    &home,
                );
                let dest = parent.join(&source.name);
                // A clone takes as long as the network: the server answers
                // everyone else meanwhile.
                cx.spawn(async move |this, cx| {
                    // Reserved by its real path, which is the one a folder
                    // added meanwhile is known by, whatever path it was given.
                    let dest = cx
                        .background_executor()
                        .spawn(async move { real_dest(&dest) })
                        .await;
                    // A second clone there would find the first one half done.
                    let reserved = this.update(cx, |this, _| {
                        if this.stopping {
                            return None;
                        }
                        let run = this.cloning.start(&dest);
                        if run.is_none() {
                            this.transport.send(
                                client,
                                place_failed(&format!(
                                    "{} is being cloned already.",
                                    dest.display()
                                )),
                            );
                        }
                        run
                    });
                    let Ok(Some(run)) = reserved else {
                        return;
                    };
                    let into = dest.clone();
                    let done = cx
                        .background_executor()
                        .spawn(async move {
                            // Busy first: a stop asked from now on is seen.
                            run.busy.store(true, Ordering::SeqCst);
                            let cloned = clone_into(&source, &into, &run.stop);
                            run.busy.store(false, Ordering::SeqCst);
                            let path = cloned.and_then(|()| real_dir(&into))?;
                            let meta = PlaceMeta::read(&path);
                            Ok((path, meta))
                        })
                        .await;
                    let _ = this.update(cx, |this, cx| {
                        this.cloning.end(&dest);
                        this.place_added(client, done, AgentKind::Claude, cx);
                    });
                })
                .detach();
            }
            ClientMsg::NewEnv { from, branch } => {
                let from = PathBuf::from(from);
                let Some(agent) = self.store.place(&from).map(|p| p.agent) else {
                    self.transport
                        .send(client, place_failed("that project is gone"));
                    return;
                };
                cx.spawn(async move |this, cx| {
                    let made = cx
                        .background_executor()
                        .spawn(async move {
                            // Git makes no worktree of a repository with no
                            // commits yet. None says so.
                            if !has_commits(&from) {
                                return None;
                            }
                            Some(add_worktree(&from, &branch).map(|path| {
                                let meta = PlaceMeta::read(&path);
                                (path, meta)
                            }))
                        })
                        .await;
                    // The new folder starts with the agent of the one it came from.
                    let _ = this.update(cx, |this, cx| match made {
                        Some(made) => this.place_added_with(client, made, agent, true, cx),
                        // The app starts its first prompt in that folder then.
                        None => this.transport.send(client, no_commits()),
                    });
                })
                .detach();
            }
            ClientMsg::RenameBranch { place, from, to } => {
                let place = PathBuf::from(place);
                if self.store.place(&place).is_none() {
                    self.transport.send(
                        client,
                        error_about("rename_branch", "that folder is not one of this server's"),
                    );
                    return;
                }
                cx.spawn(async move |this, cx| {
                    let done = cx
                        .background_executor()
                        .spawn({
                            let (place, from, to) = (place.clone(), from.clone(), to.clone());
                            async move { rename_branch(&place, &from, &to) }
                        })
                        .await;
                    let _ = this.update(cx, |this, cx| match done {
                        // The app shows the new name, and tells the agent, once it is so.
                        Ok(true) => {
                            this.inbox_dirty = true;
                            this.sync_inbox(cx);
                            let renamed = json!({
                                "type": "branch_renamed", "place": place, "from": from, "to": to,
                            });
                            this.transport.send(client, renamed.to_string());
                        }
                        // Renamed by hand meanwhile: that name stays, and nothing changed.
                        Ok(false) => {}
                        Err(e) => this
                            .transport
                            .send(client, error_about("rename_branch", &e)),
                    });
                })
                .detach();
            }
            ClientMsg::SettleEnv { place } => {
                let place = PathBuf::from(place);
                if self.store.place(&place).is_none() {
                    self.transport.send(
                        client,
                        wire::error("that folder is not one of this server's"),
                    );
                    return;
                }
                self.settle_place(&place, cx);
                self.sync_inbox(cx);
            }
            ClientMsg::DeleteEnv {
                place,
                delete_branch,
                branch: confirmed,
            } => {
                let path = PathBuf::from(&place);
                if let Err(e) = self.deletable(&path, cx) {
                    self.transport
                        .send(client, env_deleted(&place, false, Some(&e)));
                    return;
                }
                // Until it is gone, or stays: no session starts there meanwhile.
                self.deleting.insert(path.clone());
                cx.spawn(async move |this, cx| {
                    let p = path.clone();
                    // Git's files are read to the side, as every place's are.
                    let checked = cx
                        .background_executor()
                        .spawn(async move {
                            removable(&p)?;
                            let branch = delete_branch.then(|| store::branch(&p)).flatten();
                            branch_agreed(branch.as_deref(), confirmed.as_deref())?;
                            Ok::<_, String>(branch)
                        })
                        .await;
                    let done = match checked {
                        Ok(branch) => {
                            // A shell there would hold the folder open. Only
                            // now: a refused deletion keeps its terminals.
                            let hung_up = this.update(cx, |this, _| {
                                this.hang_up_terms_in(&path);
                            });
                            if hung_up.is_err() {
                                return;
                            }
                            let p = path.clone();
                            cx.background_executor()
                                .spawn(async move { remove_worktree(&p, branch.as_deref()) })
                                .await
                        }
                        Err(e) => Err(e),
                    };
                    let _ = this.update(cx, |this, cx| {
                        this.deleting.remove(&path);
                        // The folder is gone even when its branch stayed.
                        let (deleted, error) = match done {
                            Ok(branch_kept) => {
                                let saved = this.forget_place(&path, cx);
                                this.sync_inbox(cx);
                                (true, deleted_warning(branch_kept, saved.err()))
                            }
                            Err(e) => (false, Some(e)),
                        };
                        this.transport
                            .send(client, env_deleted(&place, deleted, error.as_deref()));
                    });
                })
                .detach();
            }
            ClientMsg::Ping => {}
        }
    }

    /// Runs `job` off the main thread (it waits on `claude`) and sends what it
    /// gives to `client`, in `part`s when it is too big for one frame.
    fn answer_off_thread(
        &self,
        client: ClientId,
        job: impl FnOnce() -> serde_json::Value + Send + 'static,
        cx: &mut Context<Self>,
    ) {
        cx.spawn(async move |this, cx| {
            let reply = cx.background_executor().spawn(async move { job() }).await;
            let _ = this.update(cx, |this, _| {
                for frame in wire::parts(reply.to_string()) {
                    this.transport.send(client, frame);
                }
            });
        })
        .detach();
    }

    /// Adds a folder already on this machine that a device named, `typed`.
    fn add_device_place(&mut self, client: ClientId, typed: String, cx: &mut Context<Self>) {
        let home = crate::platform::home();
        let (cloning, mark) = self.cloning.seen();
        let deletions = self.deletions;
        // A folder on a mount that does not answer would hold up
        // every device: it is looked at to the side.
        cx.spawn(async move |this, cx| {
            let looked = typed.clone();
            let found = cx
                .background_executor()
                .spawn(async move {
                    let path = device_path(&looked, &home)?;
                    // A failed clone takes its folder away again.
                    if being_cloned(&path, &cloning) {
                        return Err(format!("{} is being cloned.", path.display()));
                    }
                    let meta = PlaceMeta::read(&path);
                    Ok((path, meta))
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                // A clone that started while the folder was looked at may
                // have made it, failed and taken it away since, and a
                // deletion that ended may have taken it: look again.
                let moved = this.cloning.started_since(mark) || this.deletions != deletions;
                if found.is_ok() && moved {
                    return this.add_device_place(client, typed, cx);
                }
                this.place_added(client, found, AgentKind::Claude, cx);
            });
        })
        .detach();
    }

    /// The place's sessions stop asking for attention, as the app settles an
    /// environment's: they go idle and their agents stop. A turn still running
    /// is left alone, and so is a question on a device's screen: it is the
    /// user's to answer.
    fn settle_place(&mut self, place: &Path, cx: &mut Context<Self>) {
        let ids: Vec<String> = self
            .store
            .sessions
            .iter()
            .filter(|s| s.place == place)
            .map(|s| s.id.clone())
            .collect();
        for id in ids {
            let stop = match self.live.get(&id).map(|l| l.thread.read(cx)) {
                Some(t) if t.in_background() => continue,
                Some(t) => match status_of(t) {
                    Status::Working => continue,
                    // A failure watched or not: what it shows goes with it.
                    Status::Error => true,
                    // A page in the background is not someone looking.
                    Status::NeedsInput if self.seen(&id) => continue,
                    status => status == Status::NeedsInput,
                },
                None => false,
            };
            if let Some(s) = self.store.session_mut(&id) {
                s.status = Status::Idle;
            }
            // Its agent waits on an answer nobody will give, or failed: it
            // stops now, as the app's does, and a device sees it idle.
            if stop {
                self.retire_settled(&id, cx);
            } else {
                self.retire_if_unwatched(&id, cx);
            }
        }
        self.save_store();
        self.inbox_dirty = true;
    }

    /// Stops the agent of a session just settled, once its shell changes are
    /// in: let go before, the transcript would never get them. Devices that
    /// watch it see it idle, with nothing asked, before its thread goes.
    fn retire_settled(&mut self, id: &str, cx: &mut Context<Self>) {
        let Some(live) = self.live.get(id) else {
            return;
        };
        let t = live.thread.read(cx);
        // A prompt sent while the changes were worked out: that turn stays.
        if status_of(t) == Status::Working || t.in_background() {
            return;
        }
        if let Some(pending) = t.shell_pending().filter(|p| p.peek().is_none()) {
            let id = id.to_string();
            cx.spawn(async move |this, cx| {
                pending.await;
                let _ = this.update(cx, |this, cx| this.retire_settled(&id, cx));
            })
            .detach();
            return;
        }
        if self.watched(id) {
            let state = wire::state_message(id, t, Status::Idle);
            self.send_watchers(id, settled_state(&state));
        }
        self.retire(id, cx);
    }

    /// Why the worktree at `place` cannot be deleted, if it cannot.
    fn deletable(&self, place: &Path, cx: &App) -> Result<(), String> {
        if self.store.place(place).is_none() {
            return Err("that folder is not one of this server's".into());
        }
        if self.being_deleted(place) {
            return Err("it is being deleted already".into());
        }
        // Whether git calls it a worktree is asked to the side, after this.
        // A folder inside it added on its own goes with it, sessions and all.
        let live = self
            .store
            .sessions
            .iter()
            .filter(|s| s.place.starts_with(place))
            .filter_map(|s| self.live.get(&s.id))
            .map(|l| {
                let t = l.thread.read(cx);
                (status_of(t), t.in_background())
            });
        match busy_reason(live) {
            Some(why) => Err(why.into()),
            None => Ok(()),
        }
    }

    /// The place is gone from disk: it leaves the list, with its sessions,
    /// which have no folder to go on in, and so do places inside it. Says
    /// whether the list was saved: if not, a restart brings the place back.
    fn forget_place(&mut self, place: &Path, cx: &mut Context<Self>) -> std::io::Result<()> {
        self.hang_up_terms_in(place);
        let ids: Vec<String> = self
            .store
            .sessions
            .iter()
            .filter(|s| s.place.starts_with(place))
            .map(|s| s.id.clone())
            .collect();
        for id in ids {
            if let Some(live) = self.live.remove(&id) {
                live.thread.update(cx, |t, _| t.shutdown());
            }
            for watched in self.watching.values_mut() {
                watched.retain(|w| *w != id);
            }
            self.notified.remove(&id);
            self.writer.forget(self.store.transcript_path(&id));
            self.store.remove_session(&id);
        }
        let inside: Vec<PathBuf> = self
            .store
            .places
            .iter()
            .map(|p| p.path.clone())
            .filter(|p| p.starts_with(place))
            .collect();
        for p in inside {
            self.store.remove_place(&p);
            self.meta.remove(&p);
        }
        self.inbox_dirty = true;
        self.deletions += 1;
        self.store.save().inspect_err(|e| {
            eprintln!("arakun server: could not save its state: {e}");
        })
    }

    /// A folder a device asked for is a place now, or why it is not. The
    /// inbox goes out first, so the device knows the place it is told of.
    fn place_added(
        &mut self,
        client: ClientId,
        found: Result<(PathBuf, PlaceMeta), String>,
        agent: AgentKind,
        cx: &mut Context<Self>,
    ) {
        self.place_added_with(client, found, agent, false, cx);
    }

    /// With `made`, the folder is a worktree made for this request: one that
    /// cannot be saved stays on disk, and the error says where.
    fn place_added_with(
        &mut self,
        client: ClientId,
        found: Result<(PathBuf, PlaceMeta), String>,
        agent: AgentKind,
        made: bool,
        cx: &mut Context<Self>,
    ) {
        match found.and_then(|(p, meta)| {
            self.offer_place(&p, agent).map_err(|e| {
                if made {
                    format!(
                        "{e}. The environment is at {0}: once the server can save again, `arakun-server add {0}` lists it.",
                        p.display()
                    )
                } else {
                    e
                }
            })?;
            self.meta.insert(p.clone(), meta);
            Ok(p)
        }) {
            Ok(path) => {
                let inbox = self.inbox(cx);
                if self.last_inbox != inbox {
                    self.transport.broadcast(&inbox);
                    self.last_inbox = inbox;
                } else {
                    // The others have this one, but the device may not: it
                    // got its own on connecting, which can be older.
                    self.transport.send(client, inbox);
                }
                self.transport.send(
                    client,
                    json!({ "type": "place_added", "path": path }).to_string(),
                );
            }
            Err(e) => self.transport.send(client, place_failed(&e)),
        }
    }

    /// Makes `path` (canonical) a place for a device. It is one only once
    /// saved: a place lost at the next start would surprise the device later.
    /// The folder was looked at to the side; nothing here touches it again.
    fn offer_place(&mut self, path: &Path, agent: AgentKind) -> Result<(), String> {
        // A clone that started while the folder was looked at.
        if self.cloning.has(path) {
            return Err(format!("{} is being cloned.", path.display()));
        }
        if self.being_deleted(path) {
            return Err(format!("{} is being deleted.", path.display()));
        }
        offer(&mut self.store, path, agent)
    }

    /// Sends `text` to the session `id` as its next prompt.
    /// Sends a prompt to a session; `now` stops a running turn for it
    /// instead of queueing it behind.
    fn send_to_session(
        &mut self,
        id: &str,
        text: String,
        now: bool,
        cx: &mut Context<Self>,
    ) -> Result<(), String> {
        if let Some(s) = self.store.session(id) {
            self.admit(&s.place)?;
        }
        let thread = self.ensure_live(id, cx)?;
        thread.update(cx, |t, cx| {
            // Follow-ups that waited when the server stopped come back held,
            // for someone to let go in the app's window. Here a new prompt is
            // that someone: held, they would keep this one from ever running.
            // Let go before the prompt is sent, so they run first whether the
            // agent is still connecting or already idle; sent first, an idle
            // agent would start on the new prompt.
            if t.queue_paused() {
                t.resume_queue(cx);
            }
            if now {
                t.interrupt_with(text, cx);
            } else {
                t.send(text, cx);
            }
        });
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
        // An API key or another endpoint in place of a plan, as last read in `Usage`.
        let access = agent::claude_auth::latest().map(|a| a.label());
        wire::plan_usage(
            plan.as_ref(),
            access.as_deref(),
            &droid,
            crate::account::unix_now(),
        )
        .to_string()
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
    /// Starts the install of the adapter a session is missing, the first time
    /// it asks. True when it started one.
    fn install_missing_adapter(
        &mut self,
        id: &str,
        thread: &Entity<Thread>,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(needed) = thread.read(cx).needs_install().cloned() else {
            return false;
        };
        if !self.installed_for.insert(id.to_string()) {
            return false;
        }
        eprintln!(
            "{}: {id} needs {}; installing it into {}",
            super::name(),
            needed.package,
            needed.dir.display()
        );
        let thread = thread.clone();
        cx.defer(move |cx| thread.update(cx, |t, cx| t.install_adapter(cx)));
        true
    }

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
            .filter(|(client, watched)| {
                watched.iter().any(|w| w == id) && !self.hidden.contains(client)
            })
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
                        .send_to_session(&id, arg(1).to_string(), false, cx)
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
                            Reply::ok(match stop_turn(&t, cx) {
                                0 => format!("stopping {id}"),
                                1 => format!("stopping {id}; the follow-up that waited is dropped"),
                                n => format!(
                                    "stopping {id}; the {n} follow-ups that waited are dropped"
                                ),
                            })
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
            env!("ARAKUN_VERSION"),
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
            let branch = self
                .meta_of(&p.path)
                .branch
                .map_or(String::new(), |b| format!("  [{b}]"));
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
        self.admit(path)?;
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
        self.drop_session(id, cx);
        self.save_store();
        self.inbox_changed(cx);
        Ok(format!("removed {id}"))
    }

    /// Session `id` and its transcript are gone, its agent stopped.
    fn drop_session(&mut self, id: &str, cx: &mut Context<Self>) {
        if let Some(live) = self.live.remove(id) {
            live.thread.update(cx, |t, _| t.shutdown());
        }
        for watched in self.watching.values_mut() {
            watched.retain(|w| w != id);
        }
        self.notified.remove(id);
        self.asked.remove(id);
        self.rewinds.remove(id);
        // A save still on its way, or one kept because it failed, would bring
        // the file back after it is deleted.
        self.writer.forget(self.store.transcript_path(id));
        self.store.remove_session(id);
    }
}

/// An error about session `session`, which a device shows in that session.
/// A device that does not look for `id` shows it as any other error.
fn session_error(session: &str, message: &str) -> String {
    json!({ "type": "error", "id": session, "message": message }).to_string()
}

/// A rewind a device asked of a session (`ClientMsg::Rewind`).
#[derive(Clone, Copy)]
struct Rewinding {
    client: ClientId,
    /// The prompt's entry.
    entry: usize,
    /// Run it once its plan is in, rather than send the plan.
    confirm: bool,
    /// The session's thread was asked for the plan or the rewind: a plan
    /// gone from it now means it ran, failed or was dropped.
    driving: bool,
}

/// What rewinding to before the prompt at `plan.entry` would undo, for the device.
fn rewind_plan_message(session: &str, plan: &agent::RewindPlan) -> String {
    let evicted: Vec<[&str; 2]> = plan
        .evicted
        .iter()
        .map(|(path, why)| [path.as_str(), why.as_str()])
        .collect();
    json!({
        "type": "rewind_plan", "id": session, "entry": plan.entry,
        "restore": plan.restore, "delete": plan.delete, "evicted": evicted,
    })
    .to_string()
}

/// The rewind of session's prompt at `entry` is over, with why when it could not start.
fn rewind_gone(session: &str, entry: usize, error: Option<&str>) -> String {
    let mut msg = json!({ "type": "rewind_plan", "id": session, "entry": entry, "gone": true });
    if let Some(e) = error {
        msg["error"] = json!(e);
    }
    msg.to_string()
}

/// What `stage` did: the files staged, or why it could not.
fn staged_message(session: &str, staged: Result<Vec<String>, String>) -> String {
    match staged {
        Ok(files) => json!({ "type": "staged", "id": session, "files": files }),
        Err(e) => json!({ "type": "staged", "id": session, "error": e }),
    }
    .to_string()
}

fn unknown_agent(agent: &str) -> String {
    format!("unknown agent {agent}: claude, droid, opencode, codex, devin or workers-ai")
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
        AgentKind::Claude | AgentKind::OpenCode | AgentKind::Codex | AgentKind::Devin => {
            let options = match agent {
                AgentKind::Claude => catalog::claude_options(),
                AgentKind::Codex => catalog::codex_options(),
                AgentKind::Devin => catalog::devin_options(),
                _ => catalog::opencode_options(),
            };
            // OpenCode's and Codex's reasoning levels are per model: the cached
            // ones are the default model's.
            if agent != AgentKind::Claude
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

/// A failed folder request: its own reply, so the app does not take it for
/// an error about a session it is starting.
fn place_failed(message: &str) -> String {
    json!({ "type": "place_failed", "message": message }).to_string()
}

/// A new environment refused because its repository has no commits yet.
/// `no_commits` lets the app start a first prompt in the folder instead.
fn no_commits() -> String {
    json!({
        "type": "place_failed",
        "message": "That repository has no commits yet, so no environment can be made of it.",
        "no_commits": true,
    })
    .to_string()
}

/// Makes `path` a place in `store`, saved. A place that is there already is
/// saved too: an earlier save may have failed, and it would be gone at the
/// next start. On a failed save, a place this added goes again. A new place
/// starts with `agent`.
fn offer(store: &mut store::Store, path: &Path, agent: AgentKind) -> Result<(), String> {
    let added = store.add_place(path, agent);
    if let Err(e) = store.save() {
        if added {
            store.remove_place(path);
        }
        return Err(format!("could not save the server's state: {e}"));
    }
    Ok(())
}

/// `url` without the login a `scheme://user:token@host/...` address carries:
/// devices see the address, never what this machine signs in with.
fn without_login(url: &str) -> String {
    let Some((scheme, rest)) = url.split_once("://") else {
        return url.to_string();
    };
    let end = rest.find('/').unwrap_or(rest.len());
    match rest[..end].rfind('@') {
        Some(at) => format!("{scheme}://{}", &rest[at + 1..]),
        None => url.to_string(),
    }
}

/// What git says of a place: its branch, and the repository it is a
/// checkout of (`store::repo`). Read from its files, never on the loop that
/// answers devices.
#[derive(Clone, Default, PartialEq)]
struct PlaceMeta {
    branch: Option<String>,
    repo: Option<(PathBuf, Option<String>)>,
}

impl PlaceMeta {
    fn read(path: &Path) -> PlaceMeta {
        PlaceMeta {
            branch: store::branch(path),
            repo: store::repo(path),
        }
    }
}

/// The clones under way, by the real path ([`real_dest`]) of the folder each
/// goes into.
#[derive(Default)]
struct Clones {
    running: HashMap<PathBuf, Arc<CloneRun>>,
    /// How many have started, ever.
    started: u64,
}

/// A clone under way. The server stops it when it stops itself.
#[derive(Default)]
struct CloneRun {
    stop: AtomicBool,
    /// Git runs, or a folder it made is being removed.
    busy: AtomicBool,
}

impl Clones {
    /// Reserves `dest` for a clone, or None when one goes there already.
    fn start(&mut self, dest: &Path) -> Option<Arc<CloneRun>> {
        if self.running.contains_key(dest) {
            return None;
        }
        let run = Arc::new(CloneRun::default());
        self.running.insert(dest.to_path_buf(), run.clone());
        self.started += 1;
        Some(run)
    }

    fn end(&mut self, dest: &Path) {
        self.running.remove(dest);
    }

    fn has(&self, path: &Path) -> bool {
        self.running.contains_key(path)
    }

    /// What an Add looks at its folder with: where clones go now, and a mark
    /// for [`started_since`](Self::started_since).
    fn seen(&self) -> (Vec<PathBuf>, u64) {
        (self.running.keys().cloned().collect(), self.started)
    }

    /// Whether a clone started after `mark` was taken.
    fn started_since(&self, mark: u64) -> bool {
        self.started != mark
    }

    fn stop_all(&self) {
        for run in self.running.values() {
            run.stop.store(true, Ordering::SeqCst);
        }
    }

    fn busy(&self) -> bool {
        self.running
            .values()
            .any(|run| run.busy.load(Ordering::SeqCst))
    }
}

/// Whether `place` is one of `folders` or inside one, by whole names:
/// `/w/fix2` is not in `/w/fix`.
fn inside_any(place: &Path, folders: &HashSet<PathBuf>) -> bool {
    folders.iter().any(|f| place.starts_with(f))
}

/// Why sessions with these live statuses (and whether each still works in
/// the background) keep their place from being deleted, if they do. Work
/// under way says so first; a question says what the user can do about it.
fn busy_reason(live: impl IntoIterator<Item = (Status, bool)>) -> Option<&'static str> {
    let mut asking = false;
    for (status, background) in live {
        if status == Status::Working || background {
            return Some("a session there is still working; stop it first");
        }
        asking |= status == Status::NeedsInput;
    }
    asking.then_some("a session there is waiting for an answer; answer it or archive it first")
}

/// Why no work may start in `place` while `deleting` are being deleted.
fn admission(place: &Path, deleting: &HashSet<PathBuf>) -> Result<(), String> {
    if inside_any(place, deleting) {
        return Err("its environment is being deleted".into());
    }
    Ok(())
}

/// Whether `path` (canonical) is where a clone under way goes. Clones are
/// reserved by [`real_dest`], so this is the same path however it was given.
fn being_cloned(path: &Path, cloning: &[PathBuf]) -> bool {
    cloning.iter().any(|c| c == path)
}

/// `dest` with its links resolved, before the folder exists: its nearest
/// folder that does exist is resolved and the rest kept as given.
fn real_dest(dest: &Path) -> PathBuf {
    let mut rest = Vec::new();
    let mut at = dest;
    loop {
        if let Ok(real) = std::fs::canonicalize(at) {
            return rest.iter().rev().fold(real, |p, part| p.join(part));
        }
        match (at.parent(), at.file_name()) {
            (Some(up), Some(name)) => {
                rest.push(name.to_owned());
                at = up;
            }
            _ => return dest.to_path_buf(),
        }
    }
}

/// A folder a device named: its whole path, or one from `~/`, which stands
/// for `home`. It must exist; links are followed, as the CLI does.
fn device_path(typed: &str, home: &Path) -> Result<PathBuf, String> {
    let typed = typed.trim();
    let path = match typed.strip_prefix('~') {
        Some("") => home.to_path_buf(),
        Some(rest) if rest.starts_with('/') => home.join(&rest[1..]),
        _ => PathBuf::from(typed),
    };
    if !path.is_absolute() {
        return Err("Give the folder's whole path, or one from ~/.".into());
    }
    real_dir(&path)
}

/// `path` with its links followed. Devices get places as JSON text, which a
/// name that is not UTF-8 cannot be.
fn real_dir(path: &Path) -> Result<PathBuf, String> {
    let real = path
        .canonicalize()
        .map_err(|e| format!("{}: {e}", path.display()))?;
    if real.to_str().is_none() {
        return Err(format!("{}: the name is not UTF-8", path.display()));
    }
    if !real.is_dir() {
        return Err(format!("{} is not a folder", path.display()));
    }
    Ok(real)
}

/// What every clone runs with.
const CLONE_CONFIG: [&str; 6] = [
    // A host that stops sending gives up after a minute, not at the limit.
    "-c",
    "http.lowSpeedLimit=1",
    "-c",
    "http.lowSpeedTime=60",
    // An https address a `url.*.insteadOf` rewrites to plain http would
    // send this machine's logins for the host in the clear.
    "-c",
    "protocol.http.allow=never",
];

/// How long a clone may take before it is given up.
const CLONE_LIMIT: Duration = Duration::from_secs(20 * 60);

/// How long a stopping server waits for its clones to end and clean up.
const CLONE_STOP_WAIT: Duration = Duration::from_secs(10);

/// A branch name a device sent, as one that becomes a folder: no way out of
/// the folder it goes in, and nothing git would take for an option.
fn plain_branch(name: &str) -> bool {
    !name.is_empty()
        && !name.starts_with(['-', '.', '/'])
        && !name.ends_with(['/', '.'])
        && !name.contains("..")
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '/'))
}

fn git(dir: &Path) -> std::process::Command {
    let mut git = git_command();
    git.arg("-C").arg(dir).stdin(std::process::Stdio::null());
    git
}

/// Whether `dir`'s HEAD names a commit. When git cannot tell, true: making
/// the worktree then says what is wrong.
fn has_commits(dir: &Path) -> bool {
    // `--verify --quiet` exits 1, silently, only when HEAD names no commit.
    local_output(git(dir).args(["rev-parse", "--verify", "--quiet", "HEAD"]))
        .map_or(true, |out| out.status.code() != Some(1))
}

/// Runs git on the repository's files only; its last words when it fails.
fn run_git(cmd: &mut std::process::Command) -> Result<(), String> {
    let out = local_output(cmd).map_err(|e| format!("could not run git: {e}"))?;
    if out.status.success() {
        return Ok(());
    }
    let why = String::from_utf8_lossy(&out.stderr);
    Err(format!(
        "git failed: {}",
        why.lines()
            .rfind(|l| !l.trim().is_empty())
            .unwrap_or("no reason given")
    ))
}

/// `cmd`'s output, stopped once it ran for [`crate::platform::GIT_LOCAL`]: a
/// git that hangs (a stuck disk, a lock) must not hold a thread for good.
fn local_output(cmd: &mut std::process::Command) -> std::io::Result<std::process::Output> {
    crate::platform::output_within(cmd, None, crate::platform::GIT_LOCAL)
}

/// What `run_bounded` says when git ran out of `limit`.
fn took_too_long(limit: Duration) -> String {
    format!("git took longer than {} seconds", limit.as_secs())
}

/// Output from an offset, and how a terminal ended (its code when it had one).
type Due = (Vec<(u64, Vec<u8>)>, Option<Option<i32>>);

/// What a device following a terminal is due now: output from where it is,
/// as far as its window goes, and the exit once all of it went. `f` moves on
/// past what is returned.
fn term_due(out: &pty::Output, f: &mut Follower) -> Due {
    let mut chunks = Vec::new();
    loop {
        let room = TERM_WINDOW.saturating_sub(f.sent - f.seen);
        if room == 0 {
            break;
        }
        let (at, bytes) = out.since(f.sent, TERM_CHUNK.min(room as usize));
        if bytes.is_empty() {
            break;
        }
        // What fell out of the kept output never comes: it counts as seen.
        f.seen += at - f.sent;
        f.sent = at + bytes.len() as u64;
        chunks.push((at, bytes));
    }
    let ended = match out.exit {
        Some(code) if f.sent >= out.end() && !f.told_exit => {
            f.told_exit = true;
            Some(code)
        }
        _ => None,
    };
    (chunks, ended)
}

/// A piece of a terminal's output, from offset `at`.
fn term_output(term: &str, at: u64, bytes: &[u8]) -> String {
    use base64::Engine as _;
    let data = base64::engine::general_purpose::STANDARD.encode(bytes);
    json!({ "type": "term_output", "term": term, "at": at, "data": data }).to_string()
}

/// A terminal ended: `code` is its shell's, when it had one.
fn term_exit(term: &str, code: Option<i32>) -> String {
    json!({ "type": "term_exit", "term": term, "code": code }).to_string()
}

/// An error that says which request it answers, for a client that waits on
/// that one and must not take another's error for it.
fn error_about(about: &str, message: &str) -> String {
    json!({ "type": "error", "message": message, "about": about }).to_string()
}

/// A worktree of the repository `from` is a checkout of, on a new `branch`,
/// where the app makes its own: `~/arakun-workspaces/<project>/<branch>`.
fn add_worktree(from: &Path, branch: &str) -> Result<PathBuf, String> {
    worktree_in(
        &crate::platform::home().join("arakun-workspaces"),
        from,
        branch,
    )
}

/// `add_worktree`, under `base`.
fn worktree_in(base: &Path, from: &Path, branch: &str) -> Result<PathBuf, String> {
    worktree_within(base, from, branch, WORKTREE_LIMIT)
}

/// `worktree_in`, with git and its hooks given `limit`.
fn worktree_within(
    base: &Path,
    from: &Path,
    branch: &str,
    limit: Duration,
) -> Result<PathBuf, String> {
    if !plain_branch(branch) {
        return Err(format!("{branch:?} is not a branch name this server takes"));
    }
    // Named as the inbox names its repository, so the path the app shows
    // before is the one made, from any worktree of it.
    let (root, _) = store::repo(from).ok_or("that folder is not in a git repository")?;
    let project = store::repo_name(&root).ok_or("that repository has no name")?;
    let dest = base.join(project).join(branch);
    if dest.exists() {
        return Err(format!("{} already exists", dest.display()));
    }
    // A link on the way would have git write outside the workspaces.
    if !real_dest(&dest).starts_with(real_dest(base)) {
        return Err(format!(
            "{} leads outside {}",
            dest.display(),
            base.display()
        ));
    }
    // Git runs in `from` itself: a checkout whose git folder lives elsewhere
    // has no checkout at that folder's parent. A hook that never ends would
    // hold the server's answer: it is given a time.
    let made = run_bounded(
        git(from)
            .args(["worktree", "add", "--quiet", "-b", branch])
            .arg(&dest),
        limit,
        None,
    );
    match made {
        Ok(()) => dest.canonicalize().map_err(|e| e.to_string()),
        // Stopped part way, maybe in the checkout itself: what is there may
        // be half a worktree, not one to add.
        Err(e) if dest.exists() && e == took_too_long(limit) => Err(format!(
            "{e}, so it was stopped. What it left is at {}, on branch {branch}: remove both before you try again.",
            dest.display()
        )),
        // A hook that failed after the checkout: the worktree is there, and
        // asking again would only say so.
        Err(e) if dest.exists() => Err(format!(
            "{e}. The environment is at {} anyway: `arakun-server add` it, or remove it.",
            dest.display()
        )),
        Err(e) => Err(e),
    }
}

/// How long making a worktree, its hooks included, may take.
const WORKTREE_LIMIT: Duration = Duration::from_secs(2 * 60);

/// Removes the worktree at `place` and what is in it, then `branch` if given,
/// as the app deletes an environment. Ok once the folder is gone; it carries
/// a warning of what stayed, when something did.
fn remove_worktree(place: &Path, branch: Option<&str>) -> Result<Option<String>, String> {
    // Git runs in the repository's own files, which hold every worktree's
    // and are wherever they are kept (`--separate-git-dir`, bare).
    let (_, root) = git_dirs(place).ok_or("that folder is not in a git repository")?;
    // Its hooks and a big folder get the time adding one gets.
    let removed = run_bounded(
        git(&root)
            .args(["worktree", "remove", "--force"])
            .arg(place),
        WORKTREE_LIMIT,
        None,
    );
    let mut stayed = Vec::new();
    match removed {
        Ok(()) => {}
        // Git takes the folder first and its own files for it after. Gone,
        // the folder is deleted: another try would only fail to find it.
        Err(e) if !place.exists() => stayed.push(format!("Git did not finish tidying up: {e}")),
        Err(e) => return Err(e),
    }
    // No `worktree prune`: it would also take the git files of another
    // worktree whose folder is away for now, on a mount that is not there.
    // A hook that never ends would keep the answer from the device.
    if let Some(b) = branch {
        let deleted = run_bounded(
            git(&root).args(["branch", "-D", "--", b]),
            WORKTREE_LIMIT,
            None,
        );
        if let Err(e) = deleted {
            stayed.push(format!("The branch stayed: {e}"));
        }
    }
    Ok((!stayed.is_empty()).then(|| stayed.join(" ")))
}

/// Whether `checked_out`, the branch a deletion would take, is the one the
/// user agreed to, `confirmed`: one switched to since the dialog showed it
/// may hold commits nobody chose to lose. A device that names none (from
/// before it could) takes the one checked out.
fn branch_agreed(checked_out: Option<&str>, confirmed: Option<&str>) -> Result<(), String> {
    match (checked_out, confirmed) {
        (Some(now), Some(agreed)) if now != agreed => Err(format!(
            "its branch is {now} now, not {agreed}; open the dialog again"
        )),
        _ => Ok(()),
    }
}

/// Whether the folder at `place` is a worktree a device may delete: never a
/// repository's main folder, which holds the history of every worktree.
fn removable(place: &Path) -> Result<(), String> {
    match git_dirs(place) {
        // Only a worktree has git files of its own beside the shared ones.
        Some((own, common)) if own != common => Ok(()),
        Some(_) => Err("a repository's main folder is not deleted".into()),
        None => Err("that folder is not a git worktree".into()),
    }
}

/// The git files of the checkout at `place` and the repository's shared
/// ones, as git resolves them.
fn git_dirs(place: &Path) -> Option<(PathBuf, PathBuf)> {
    let out = local_output(git(place).args([
        "rev-parse",
        "--path-format=absolute",
        "--git-dir",
        "--git-common-dir",
    ]))
    .ok()
    .filter(|o| o.status.success())?;
    let (own, common) = two_paths(&String::from_utf8_lossy(&out.stdout))?;
    Some((
        own.canonicalize().unwrap_or(own),
        common.canonicalize().unwrap_or(common),
    ))
}

/// The two paths git printed, a line each. Only the line ends go: a folder
/// name may end in a space.
fn two_paths(said: &str) -> Option<(PathBuf, PathBuf)> {
    let mut lines = said.lines().map(PathBuf::from);
    Some((lines.next()?, lines.next()?))
}

/// `state`, as [`wire::state_message`] put it, for a session settled from
/// its failure or question: idle, with nothing asked and no turn under way.
fn settled_state(state: &str) -> String {
    let mut msg: serde_json::Value = serde_json::from_str(state).unwrap_or_default();
    msg["status"] = json!(wire::status(Status::Idle));
    msg["permission"] = serde_json::Value::Null;
    msg["question"] = serde_json::Value::Null;
    msg["elapsed_ms"] = serde_json::Value::Null;
    msg.to_string()
}

/// What a deletion that took the folder warns of: what stayed (the branch,
/// git's own files for it), a list that could not be saved (the place comes back at the next start).
fn deleted_warning(stayed: Option<String>, unsaved: Option<std::io::Error>) -> Option<String> {
    let parts: Vec<String> = [
        stayed,
        unsaved.map(|e| {
            format!("The server could not save its list ({e}), so the environment comes back when it restarts.")
        }),
    ]
    .into_iter()
    .flatten()
    .collect();
    (!parts.is_empty()).then(|| parts.join(" "))
}

/// The answer to `delete_env`: whether the folder is gone, and what went wrong.
fn env_deleted(place: &str, deleted: bool, error: Option<&str>) -> String {
    json!({ "type": "env_deleted", "place": place, "deleted": deleted, "error": error }).to_string()
}

/// Renames the branch at `place` from `from` to `to`, as long as `from` is
/// still the one checked out there: one renamed by hand meanwhile stays.
/// Whether it renamed it.
fn rename_branch(place: &Path, from: &str, to: &str) -> Result<bool, String> {
    if !plain_branch(to) {
        return Err(format!("{to:?} is not a branch name this server takes"));
    }
    if store::branch(place).as_deref() != Some(from) {
        return Ok(false);
    }
    // Pushed: the remote keeps the old name, as the app's own rename knows.
    // A push without --set-upstream leaves only the remote-tracking ref.
    // A check that failed or timed out says nothing, and must not let the rename go on.
    let unsure = |e: std::io::Error| format!("could not check whether {from} was pushed: {e}");
    let upstream =
        local_output(git(place).args(["config", "--get", &format!("branch.{from}.merge")]))
            .map_err(unsure)?;
    // `config --get` exits 1 only when the key is not set.
    let upstream = match upstream.status.code() {
        Some(0) => true,
        Some(1) => false,
        _ => return Err(format!("could not check whether {from} was pushed")),
    };
    let tracked = local_output(
        git(place)
            .args(["for-each-ref", "--format=%(refname)"])
            .arg(format!("refs/remotes/*/{from}")),
    )
    .map_err(unsure)?;
    if !tracked.status.success() {
        return Err(format!("could not check whether {from} was pushed"));
    }
    let tracked = !tracked.stdout.trim_ascii().is_empty();
    if upstream || tracked {
        return Err(format!(
            "{from} was already pushed; rename it on the remote too, or keep the name"
        ));
    }
    run_git(git(place).args(["branch", "-m", "--", from, to])).map(|()| true)
}

/// Clones `source` into `dest`, or takes `dest` as it is when it already holds
/// that repository. A failed clone leaves nothing behind, nor does one that
/// `stop` ends.
fn clone_into(
    source: &clone_source::CloneSource,
    dest: &Path,
    stop: &AtomicBool,
) -> Result<(), String> {
    clone_with(git_command(), source, dest, stop)
}

/// [`clone_into`], with `git` as the command to start from: tests give it
/// an environment of their own.
fn clone_with(
    mut git: std::process::Command,
    source: &clone_source::CloneSource,
    dest: &Path,
    stop: &AtomicBool,
) -> Result<(), String> {
    if stop.load(Ordering::SeqCst) {
        return Err(SERVER_STOPPED.into());
    }
    if dest.exists() {
        return match checkout_origin(dest) {
            Some(found) if found.same_repo(source) => Ok(()),
            Some(found) => Err(format!("{} already holds {}", dest.display(), found.label)),
            None => Err(format!("{} already exists", dest.display())),
        };
    }
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("could not create {}: {e}", parent.display()))?;
    }
    // Made here, and only if it is not there yet: what a failed clone removes
    // is then this request's own folder.
    std::fs::create_dir(dest).map_err(|e| format!("{}: {e}", dest.display()))?;
    // Asked before the clone's own settings go on: those do not name one.
    let own_ssh = has_ssh_command(&git);
    git.args(CLONE_CONFIG)
        .args(["clone", "--quiet", "--", &source.url])
        .arg(dest)
        // Nobody is at this machine to type a password: fail instead of waiting.
        .env("GIT_TERMINAL_PROMPT", "0");
    // ssh asks for a passphrase or a new host key on the terminal, which
    // `serve` may have. A command the server set keeps its say: it may pick
    // the key that signs in. The time limit still holds for it.
    if !own_ssh {
        git.env(
            "GIT_SSH_COMMAND",
            "ssh -o BatchMode=yes -o ConnectTimeout=30 -o ServerAliveInterval=15",
        );
    }
    run_bounded(&mut git, CLONE_LIMIT, Some(stop)).inspect_err(|_| {
        let _ = std::fs::remove_dir_all(dest);
    })
}

/// Runs git for at most `limit`; its last words when it fails. Git runs
/// helpers (`git-remote-https`, ssh, hooks) of its own: given up, they end too,
/// as they do once `stop` is set.
fn run_bounded(
    git: &mut std::process::Command,
    limit: Duration,
    stop: Option<&AtomicBool>,
) -> Result<(), String> {
    use std::process::Stdio;
    git.stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    crate::platform::ProcessTree::prepare(git);
    let mut child = git.spawn().map_err(|e| format!("could not run git: {e}"))?;
    let tree = crate::platform::ProcessTree::adopt(&child);
    // Read as it comes, so a full pipe does not stop git.
    let mut stderr = child.stderr.take();
    let (said_tx, said) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut text = String::new();
        if let Some(err) = stderr.as_mut() {
            let _ = std::io::Read::read_to_string(err, &mut text);
        }
        let _ = said_tx.send(text);
    });
    let started = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if stop.is_some_and(|s| s.load(Ordering::SeqCst)) => {
                tree.kill();
                let _ = child.kill();
                let _ = child.wait();
                return Err(SERVER_STOPPED.into());
            }
            Ok(None) if started.elapsed() < limit => {
                std::thread::sleep(Duration::from_millis(100));
            }
            Ok(None) => {
                // Git itself too: a tree that could not be set up (a job
                // object Windows refused) would leave it running.
                tree.kill();
                let _ = child.kill();
                let _ = child.wait();
                return Err(took_too_long(limit));
            }
            Err(e) => {
                tree.kill();
                let _ = child.kill();
                return Err(format!("git: {e}"));
            }
        }
    };
    if status.success() {
        return Ok(());
    }
    // A helper left behind can hold stderr open: it goes, and what git said
    // is waited for a little only.
    tree.kill();
    let why = said
        .recv_timeout(Duration::from_secs(5))
        .unwrap_or_default();
    Err(format!(
        "git failed: {}",
        why.lines()
            .rfind(|l| !l.trim().is_empty())
            .unwrap_or("no reason given")
    ))
}

const SERVER_STOPPED: &str = "the server stopped before the clone was done";

/// Whether `git` has an ssh command of its own, from its environment or the
/// server's git config, the way git looks for one.
fn has_ssh_command(git: &std::process::Command) -> bool {
    let set = |key: &str| {
        let given = git.get_envs().find(|(k, _)| *k == key).map(|(_, v)| v);
        match given {
            Some(v) => v.is_some_and(|v| !v.is_empty()),
            None => std::env::var_os(key).is_some_and(|v| !v.is_empty()),
        }
    };
    if set("GIT_SSH_COMMAND") || set("GIT_SSH") {
        return true;
    }
    let mut config = std::process::Command::new(git.get_program());
    for (key, value) in git.get_envs() {
        match value {
            Some(v) => config.env(key, v),
            None => config.env_remove(key),
        };
    }
    // Outside any repository: a clone reads no repository's config either.
    local_output(config.current_dir(std::env::temp_dir()).args([
        "config",
        "--get",
        "core.sshCommand",
    ]))
    .is_ok_and(|o| o.status.success() && !o.stdout.trim_ascii().is_empty())
}

/// Git as the server runs it. Tests leave out this machine's settings: a
/// user's commit signing or URL rewrites would change what they check.
fn git_command() -> std::process::Command {
    let mut git = std::process::Command::new("git");
    // The folder it is run in picks the repository, not one inherited from
    // whatever started the server (a git hook sets these).
    for var in ["GIT_DIR", "GIT_WORK_TREE", "GIT_INDEX_FILE"] {
        git.env_remove(var);
    }
    if cfg!(test) {
        git.env("GIT_CONFIG_NOSYSTEM", "1").env(
            "GIT_CONFIG_GLOBAL",
            std::env::temp_dir().join("arakun-test-no-gitconfig"),
        );
    }
    git
}

/// The repository whose checkout `dir` is, by its `origin`. A folder inside
/// some other checkout is not one.
fn checkout_origin(dir: &Path) -> Option<clone_source::CloneSource> {
    let git = |args: &[&str]| {
        local_output(git_command().arg("-C").arg(dir).args(args))
            .ok()
            .filter(|o| o.status.success())
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
    };
    let top = git(&["rev-parse", "--show-toplevel"])?;
    if Path::new(&top).canonicalize().ok()? != dir.canonicalize().ok()? {
        return None;
    }
    clone_source::parse_clone_source(&git(&["remote", "get-url", "origin"])?)
}

#[cfg(test)]
mod tests {
    #[test]
    fn a_rewind_plan_goes_out_as_the_app_reads_it() {
        let plan = agent::RewindPlan {
            entry: 3,
            message_id: "m1".into(),
            text: "fix it".into(),
            restore: vec![json!({ "filePath": "/p/a.rs" })],
            delete: vec![json!({ "filePath": "/p/new.rs" })],
            evicted: vec![("/p/big.bin".into(), "too large".into())],
            loading: false,
            running: false,
        };
        let msg: serde_json::Value =
            serde_json::from_str(&rewind_plan_message("s1", &plan)).unwrap();
        assert_eq!(msg["type"], "rewind_plan");
        assert_eq!(msg["entry"], 3);
        assert_eq!(msg["restore"][0]["filePath"], "/p/a.rs");
        assert_eq!(msg["delete"][0]["filePath"], "/p/new.rs");
        assert_eq!(msg["evicted"][0], json!(["/p/big.bin", "too large"]));
        assert!(msg.get("gone").is_none());

        let gone: serde_json::Value =
            serde_json::from_str(&rewind_gone("s1", 3, Some("it is working"))).unwrap();
        assert_eq!(
            (gone["gone"].clone(), gone["entry"].clone()),
            (json!(true), json!(3))
        );
        assert_eq!(gone["error"], "it is working");
        let ran: serde_json::Value = serde_json::from_str(&rewind_gone("s1", 3, None)).unwrap();
        assert!(ran.get("error").is_none());
    }

    #[test]
    fn stage_says_the_files_or_why_not() {
        let ok: serde_json::Value =
            serde_json::from_str(&staged_message("s1", Ok(vec!["a.rs".into()]))).unwrap();
        assert_eq!(ok["files"], json!(["a.rs"]));
        let failed: serde_json::Value =
            serde_json::from_str(&staged_message("s1", Err("no repo".into()))).unwrap();
        assert_eq!(failed["error"], "no repo");
        assert!(failed.get("files").is_none());
    }

    #[test]
    fn a_place_is_offered_only_once_it_is_saved() {
        let dir = scratch("offer");
        let mut store = store::Store::load(&dir).unwrap();
        let app = dir.join("app");
        std::fs::create_dir(&app).unwrap();
        // A folder where the state file goes: every save fails.
        std::fs::create_dir(dir.join("state.json")).unwrap();
        assert!(offer(&mut store, &app, AgentKind::Claude).is_err());
        assert!(store.place(&app).is_none(), "what it added goes again");
        // Added before, by a local `add` whose save failed: still not saved.
        store.add_place(&app, AgentKind::Claude);
        assert!(offer(&mut store, &app, AgentKind::Claude).is_err());
        assert!(store.place(&app).is_some(), "what it did not add stays");
        std::fs::remove_dir(dir.join("state.json")).unwrap();
        assert_eq!(offer(&mut store, &app, AgentKind::Claude), Ok(()));
        let saved = std::fs::read_to_string(dir.join("state.json")).unwrap();
        assert!(saved.contains(app.to_str().unwrap()));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_folder_being_deleted_takes_what_is_inside_it() {
        let deleting = HashSet::from([PathBuf::from("/w/fix")]);
        assert!(inside_any(Path::new("/w/fix"), &deleting));
        assert!(inside_any(Path::new("/w/fix/docs"), &deleting));
        assert!(!inside_any(Path::new("/w/fix2"), &deleting));
        assert!(!inside_any(Path::new("/w"), &deleting));
    }

    #[test]
    fn a_question_blocks_a_delete_with_its_own_reason() {
        assert_eq!(
            busy_reason([(Status::Idle, false), (Status::Error, false)]),
            None
        );
        let asking = busy_reason([(Status::Idle, false), (Status::NeedsInput, false)]);
        assert!(asking.is_some_and(|w| w.contains("waiting for an answer")));
        // Work under way wins, wherever it is in the list.
        let working = busy_reason([(Status::NeedsInput, false), (Status::Working, false)]);
        assert!(working.is_some_and(|w| w.contains("still working")));
        let background = busy_reason([(Status::Idle, true)]);
        assert!(background.is_some_and(|w| w.contains("still working")));
    }

    #[test]
    fn a_deletion_says_when_its_list_was_not_saved() {
        assert_eq!(deleted_warning(None, None), None);
        let unsaved = deleted_warning(None, Some(std::io::Error::other("disk full")));
        assert!(unsaved.is_some_and(|w| w.contains("disk full") && w.contains("comes back")));
        let both = deleted_warning(
            Some("The branch stayed: in use".into()),
            Some(std::io::Error::other("disk full")),
        )
        .unwrap();
        assert!(both.starts_with("The branch stayed: in use") && both.contains("disk full"));
    }

    #[test]
    fn a_settled_session_shows_idle_with_nothing_asked() {
        let asking = json!({
            "type": "state", "id": "s1", "status": "needs_input", "permission": null,
            "question": { "text": "Which one?" }, "elapsed_ms": 1200, "title": "Fix",
        });
        let settled: serde_json::Value =
            serde_json::from_str(&settled_state(&asking.to_string())).unwrap();
        assert_eq!(settled["status"], wire::status(Status::Idle));
        assert!(settled["question"].is_null() && settled["elapsed_ms"].is_null());
        // What it says of the session itself stays.
        assert_eq!(
            (settled["id"].as_str(), settled["title"].as_str()),
            (Some("s1"), Some("Fix"))
        );
    }

    #[test]
    fn only_the_branch_the_user_agreed_to_is_deleted() {
        assert_eq!(branch_agreed(Some("fix"), Some("fix")), Ok(()));
        let switched = branch_agreed(Some("experiment"), Some("fix")).unwrap_err();
        assert!(switched.contains("experiment") && switched.contains("fix"));
        // Detached now: no branch goes, so nothing to check.
        assert_eq!(branch_agreed(None, Some("fix")), Ok(()));
        assert_eq!(branch_agreed(Some("fix"), None), Ok(()));
    }

    #[test]
    fn a_git_folder_keeps_the_space_its_name_ends_in() {
        assert_eq!(
            two_paths("/r/.git/worktrees/fix\n/r/x /.git \n"),
            Some((
                PathBuf::from("/r/.git/worktrees/fix"),
                PathBuf::from("/r/x /.git ")
            ))
        );
        assert_eq!(
            two_paths("/r/.git\r\n/r/.git\r\n").unwrap().1,
            PathBuf::from("/r/.git")
        );
        assert_eq!(two_paths("/r/.git\n"), None);
    }

    #[test]
    fn no_work_starts_in_a_folder_being_deleted() {
        let deleting = HashSet::from([PathBuf::from("/w/fix")]);
        assert!(admission(Path::new("/w/fix"), &deleting).is_err());
        // A place added inside it goes with it.
        assert!(admission(Path::new("/w/fix/docs"), &deleting).is_err());
        assert!(admission(Path::new("/w/fix2"), &deleting).is_ok());
        assert!(admission(Path::new("/w/fix"), &HashSet::new()).is_ok());
    }

    #[test]
    fn an_origin_goes_out_without_its_login() {
        assert_eq!(
            without_login("https://bot:ghp_secret@github.com/o/r.git"),
            "https://github.com/o/r.git"
        );
        assert_eq!(without_login("ssh://git@host:22/o/r"), "ssh://host:22/o/r");
        // An @ further on is the path's, and an scp-like address has no secret.
        assert_eq!(without_login("https://host/o/r@v2"), "https://host/o/r@v2");
        assert_eq!(
            without_login("git@github.com:o/r.git"),
            "git@github.com:o/r.git"
        );
    }

    #[test]
    fn a_clone_never_falls_back_to_plain_http() {
        let dir = scratch("plain-http");
        // The machine's git config could turn https into http.
        let out = std::process::Command::new("git")
            .args([
                "-c",
                "url.http://127.0.0.1:9/.insteadOf=https://example.invalid/",
            ])
            .args(CLONE_CONFIG)
            .args(["clone", "--quiet", "--", "https://example.invalid/o/r.git"])
            .arg(dir.join("r"))
            .env("GIT_TERMINAL_PROMPT", "0")
            .stdin(std::process::Stdio::null())
            .output()
            .unwrap();
        assert!(!out.status.success());
        let said = String::from_utf8_lossy(&out.stderr);
        assert!(said.contains("transport 'http' not allowed"), "{said}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    // Linux keeps any bytes as a name; the folder needs one that is not UTF-8.
    #[cfg(target_os = "linux")]
    #[test]
    fn a_link_to_a_folder_whose_name_is_not_utf8_is_refused() {
        use std::os::unix::ffi::OsStrExt as _;
        let dir = scratch("not-utf8");
        let odd = dir.join(std::ffi::OsStr::from_bytes(b"caf\xe9"));
        std::fs::create_dir(&odd).unwrap();
        let link = dir.join("cafe");
        std::os::unix::fs::symlink(&odd, &link).unwrap();
        let err = real_dir(&link).unwrap_err();
        assert!(err.contains("not UTF-8"), "{err}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    // A link is how a second path to one folder comes about.
    #[cfg(unix)]
    #[test]
    fn a_folder_being_cloned_is_known_by_any_path_to_it() {
        let dir = scratch("cloning");
        let real = dir.join("real");
        std::fs::create_dir_all(real.join("app")).unwrap();
        let link = dir.join("link");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        // The clone is reserved before its folder is there, through the link.
        let reserved = super::real_dest(&link.join("new/app"));
        assert_eq!(reserved, real.join("new/app"));
        // The folder appears, and is added by another path while the clone runs.
        std::fs::create_dir_all(real.join("new/app")).unwrap();
        let added = std::fs::canonicalize(link.join("new/app")).unwrap();
        assert!(super::being_cloned(&added, std::slice::from_ref(&reserved)));
        let app = std::fs::canonicalize(real.join("app")).unwrap();
        assert!(!super::being_cloned(&app, &[reserved]));
        let _ = std::fs::remove_dir_all(&dir);
    }

    use super::*;

    /// A folder of its own under the system's temporary one.
    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("arakun-daemon-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir.canonicalize().unwrap()
    }

    /// A repository at `dir` with one commit; `init` says how git makes it.
    fn repo_at(dir: &Path, init: &[&str]) {
        run_git(git(dir).args(["init", "--quiet"]).args(init)).unwrap();
        run_git(git(dir).args([
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@t",
            "commit",
            "--quiet",
            "--allow-empty",
            "-m",
            "first",
        ]))
        .unwrap();
    }

    #[test]
    fn a_repository_with_no_commits_is_told_apart() {
        let tmp = scratch("empty");
        let app = tmp.join("app");
        std::fs::create_dir(&app).unwrap();
        run_git(git(&app).args(["init", "-q"])).unwrap();
        assert!(!has_commits(&app));
        let made = tmp.join("made");
        std::fs::create_dir(&made).unwrap();
        repo_at(&made, &[]);
        assert!(has_commits(&made));
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn a_new_environment_is_a_worktree_named_after_its_project() {
        let tmp = scratch("plain");
        let app = tmp.join("app");
        std::fs::create_dir(&app).unwrap();
        repo_at(&app, &[]);
        let base = tmp.join("workspaces");
        let made = worktree_in(&base, &app, "rv/first").unwrap();
        assert_eq!(made, base.join("app/rv/first"));
        assert_eq!(store::branch(&made).as_deref(), Some("rv/first"));
        // From a worktree too: the project is still the main checkout's.
        let again = worktree_in(&base, &made, "second").unwrap();
        assert_eq!(again, base.join("app/second"));
        assert!(worktree_in(&base, &app, "second").is_err(), "already there");
        let _ = std::fs::remove_dir_all(&tmp);
    }

    // A hook is a script git runs: it needs to be executable.
    #[cfg(unix)]
    #[test]
    fn a_hook_that_fails_after_the_checkout_leaves_the_worktree_and_says_where() {
        use std::os::unix::fs::PermissionsExt as _;
        let tmp = scratch("hook");
        let app = tmp.join("app");
        std::fs::create_dir(&app).unwrap();
        repo_at(&app, &[]);
        let hook = app.join(".git/hooks/post-checkout");
        std::fs::create_dir_all(hook.parent().unwrap()).unwrap();
        std::fs::write(&hook, "#!/bin/sh\nexit 1\n").unwrap();
        std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o755)).unwrap();
        let base = tmp.join("workspaces");
        let err = worktree_in(&base, &app, "hooked").unwrap_err();
        let made = base.join("app/hooked");
        assert!(
            err.contains(&format!("The environment is at {} anyway", made.display())),
            "{err}"
        );
        assert_eq!(store::branch(&made).as_deref(), Some("hooked"), "it stays");
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[cfg(unix)]
    #[test]
    fn a_hook_that_hangs_is_stopped_and_not_taken_for_a_failed_one() {
        use std::os::unix::fs::PermissionsExt as _;
        let tmp = scratch("hang");
        let app = tmp.join("app");
        std::fs::create_dir(&app).unwrap();
        repo_at(&app, &[]);
        let hook = app.join(".git/hooks/post-checkout");
        std::fs::create_dir_all(hook.parent().unwrap()).unwrap();
        std::fs::write(&hook, "#!/bin/sh\nsleep 60\n").unwrap();
        std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o755)).unwrap();
        let base = tmp.join("workspaces");
        let started = Instant::now();
        let err = worktree_within(&base, &app, "stuck", Duration::from_secs(2)).unwrap_err();
        assert!(
            started.elapsed() < Duration::from_secs(30),
            "it waited on the hook"
        );
        assert!(
            err.starts_with(&took_too_long(Duration::from_secs(2))),
            "{err}"
        );
        assert!(err.contains("remove both"), "{err}");
        assert!(!err.contains("arakun-server add"), "{err}");
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn a_checkout_with_its_git_folder_elsewhere_makes_environments_too() {
        let tmp = scratch("separate");
        let app = tmp.join("app");
        std::fs::create_dir(&app).unwrap();
        std::fs::create_dir(tmp.join("git-files")).unwrap();
        let elsewhere = tmp.join("git-files").join("app.git");
        repo_at(&app, &["--separate-git-dir", &elsewhere.to_string_lossy()]);
        let base = tmp.join("workspaces");
        let made = worktree_in(&base, &app, "fix").unwrap();
        assert_eq!(made, base.join("app/fix"));
        // From that worktree, the next one goes beside it.
        let next = worktree_in(&base, &made, "next").unwrap();
        assert_eq!(next, base.join("app/next"));
        // The app names the repository as the folders are named.
        let named = |p: &Path| store::repo_name(&store::repo(p).unwrap().0).unwrap();
        assert_eq!(named(&app), "app");
        assert_eq!(named(&made), "app");
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[cfg(unix)]
    #[test]
    fn a_new_environment_does_not_follow_a_link_out_of_the_workspaces() {
        let tmp = scratch("escape");
        let app = tmp.join("app");
        std::fs::create_dir(&app).unwrap();
        repo_at(&app, &[]);
        let base = tmp.join("workspaces");
        let outside = tmp.join("outside");
        std::fs::create_dir_all(&base).unwrap();
        std::fs::create_dir(&outside).unwrap();
        std::os::unix::fs::symlink(&outside, base.join("app")).unwrap();
        let err = worktree_in(&base, &app, "fix").unwrap_err();
        assert!(err.contains("leads outside"), "{err}");
        assert!(!outside.join("fix").exists());
        // A branch with a folder in its name is still one.
        std::fs::remove_file(base.join("app")).unwrap();
        assert!(worktree_in(&base, &app, "rv/fix").is_ok());
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn a_worktree_of_a_checkout_with_its_git_folder_elsewhere_can_be_deleted() {
        let tmp = scratch("separate-delete");
        let app = tmp.join("app");
        std::fs::create_dir(&app).unwrap();
        std::fs::create_dir(tmp.join("git-files")).unwrap();
        let elsewhere = tmp.join("git-files").join("app.git");
        repo_at(&app, &["--separate-git-dir", &elsewhere.to_string_lossy()]);
        let made = worktree_in(&tmp.join("workspaces"), &app, "fix").unwrap();
        // The main folder holds the history: it stays.
        assert!(removable(&app).is_err());
        assert_eq!(removable(&made), Ok(()));
        assert_eq!(remove_worktree(&made, Some("fix")), Ok(None));
        assert!(!made.exists());
        assert!(app.exists());
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn a_branch_renamed_by_hand_meanwhile_stays() {
        let tmp = scratch("rename");
        repo_at(&tmp, &["-b", "random-name"]);
        assert_eq!(rename_branch(&tmp, "random-name", "fix-login"), Ok(true));
        assert_eq!(store::branch(&tmp).as_deref(), Some("fix-login"));
        // The app's rename comes after the user's: it changes nothing.
        assert_eq!(rename_branch(&tmp, "random-name", "other"), Ok(false));
        assert_eq!(store::branch(&tmp).as_deref(), Some("fix-login"));
        assert!(rename_branch(&tmp, "fix-login", "-x").is_err());
        // Pushed: the name stays.
        run_git(git(&tmp).args(["config", "branch.fix-login.merge", "refs/heads/fix-login"]))
            .unwrap();
        let err = rename_branch(&tmp, "fix-login", "other").unwrap_err();
        assert!(err.contains("already pushed"), "{err}");
        assert_eq!(store::branch(&tmp).as_deref(), Some("fix-login"));
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn a_file_is_not_a_folder_to_add() {
        let dir = scratch("file");
        let file = dir.join("notes.txt");
        std::fs::write(&file, "x").unwrap();
        let err = real_dir(&file).unwrap_err();
        assert!(err.contains("is not a folder"), "{err}");
        assert_eq!(real_dir(&dir), Ok(dir.clone()));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_branch_from_a_device_stays_in_its_folder() {
        for good in ["fix-login", "rv/fix_2", "v1.2"] {
            assert!(plain_branch(good), "{good}");
        }
        for bad in [
            "", "-b", "../x", "a/../b", "/abs", "x/", "a b", "x.", ".hidden", "a;b",
        ] {
            assert!(!plain_branch(bad), "{bad}");
        }
    }

    #[test]
    fn a_deleted_worktree_takes_its_folder_and_maybe_its_branch() {
        let dir = std::env::temp_dir().join(format!("arakun-server-rm-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let main = dir.join("app");
        std::fs::create_dir_all(&main).unwrap();
        let git_ok = |d: &Path, args: &[&str]| {
            run_git(
                git(d)
                    .args(["-c", "user.name=t", "-c", "user.email=t@t"])
                    .args(args),
            )
            .unwrap()
        };
        git_ok(&main, &["init", "-q", "-b", "main"]);
        git_ok(&main, &["commit", "-q", "--allow-empty", "-m", "first"]);
        let main = main.canonicalize().unwrap();
        for b in ["keep", "drop"] {
            git_ok(
                &main,
                &["worktree", "add", "-q", "-b", b, &format!("../{b}")],
            );
        }
        let (keep, drop) = (
            dir.join("keep").canonicalize().unwrap(),
            dir.join("drop").canonicalize().unwrap(),
        );
        // Only worktrees go, never the folder every one of them shares.
        assert!(removable(&main).unwrap_err().contains("main folder"));
        assert!(removable(&dir).unwrap_err().contains("not a git worktree"));
        assert_eq!(removable(&keep), Ok(()));

        // What is in a worktree goes with it, uncommitted or not.
        std::fs::write(keep.join("notes.txt"), "draft").unwrap();
        assert_eq!(remove_worktree(&keep, None), Ok(None));
        assert_eq!(remove_worktree(&drop, Some("drop")), Ok(None));
        assert!(!keep.exists() && !drop.exists());
        let has = |b: &str| run_git(git(&main).args(["rev-parse", "--verify", "-q", b])).is_ok();
        assert!(has("refs/heads/keep") && !has("refs/heads/drop"));
        // Gone already: nothing to remove.
        assert!(remove_worktree(&keep, None).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_deletion_leaves_another_worktree_that_is_away_for_now() {
        let dir = scratch("rm-away");
        let main = dir.join("app");
        std::fs::create_dir_all(&main).unwrap();
        repo_at(&main, &[]);
        for b in ["gone", "away"] {
            git_in(
                &main,
                &["worktree", "add", "-q", "-b", b, &format!("../{b}")],
            );
        }
        // Its folder is on a mount that is not there right now.
        std::fs::rename(dir.join("away"), dir.join("away-later")).unwrap();
        assert_eq!(remove_worktree(&dir.join("gone"), None), Ok(None));
        let admin = main.join(".git").join("worktrees");
        assert!(!admin.join("gone").exists());
        assert!(admin.join("away").join("HEAD").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    // Git's files are kept from being changed by a folder's permissions.
    #[cfg(unix)]
    #[test]
    fn a_worktree_whose_folder_went_is_deleted_though_git_did_not_tidy_up() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = scratch("rm-tidy");
        let main = dir.join("app");
        std::fs::create_dir_all(&main).unwrap();
        repo_at(&main, &[]);
        git_in(&main, &["worktree", "add", "-q", "-b", "fix", "../fix"]);
        let admin = main.join(".git").join("worktrees");
        let fix = dir.join("fix");
        std::fs::set_permissions(&admin, std::fs::Permissions::from_mode(0o555)).unwrap();
        let done = remove_worktree(&fix, None);
        std::fs::set_permissions(&admin, std::fs::Permissions::from_mode(0o755)).unwrap();
        // Run as root, the permissions keep nothing from going.
        if admin.join("fix").exists() {
            let warning = done.unwrap().unwrap();
            assert!(
                warning.starts_with("Git did not finish tidying up"),
                "{warning}"
            );
        }
        assert!(!fix.exists());
        // A folder that stays is not deleted.
        git_in(&main, &["worktree", "add", "-q", "-b", "kept", "../kept"]);
        git_in(&main, &["worktree", "lock", "--reason", "busy", "../kept"]);
        assert!(remove_worktree(&dir.join("kept"), None).is_err());
        assert!(dir.join("kept").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_device_names_a_folder_by_its_whole_path_or_from_home() {
        let home = scratch("home");
        std::fs::create_dir(home.join("app")).unwrap();
        assert_eq!(device_path("~", &home).unwrap(), home);
        assert_eq!(device_path(" ~/ ", &home).unwrap(), home);
        assert_eq!(device_path("~/app", &home).unwrap(), home.join("app"));
        let whole = home.join("app").display().to_string();
        assert_eq!(device_path(&whole, &home).unwrap(), home.join("app"));
        // Relative to what? The server has no folder the device stands in.
        assert!(
            device_path("code/app", &home)
                .unwrap_err()
                .contains("whole path")
        );
        assert!(device_path("~nobody/app", &home).is_err());
        assert!(device_path("~/missing", &home).is_err());
        let _ = std::fs::remove_dir_all(&home);
    }

    /// Runs git in `dir` with a name of its own and none of the user's settings.
    fn git_in(dir: &Path, args: &[&str]) {
        let ok = git_command()
            .arg("-C")
            .arg(dir)
            .args(["-c", "user.name=t", "-c", "user.email=t@t"])
            .args(args)
            .output()
            .unwrap()
            .status
            .success();
        assert!(ok, "git {args:?}");
    }

    #[test]
    fn a_clone_reuses_its_own_checkout_and_leaves_others_alone() {
        let root = scratch("clone");
        let origin = origin_repo(&root);
        let source = |url: &str| clone_source::CloneSource {
            url: url.to_string(),
            on_github: false,
            label: url.to_string().into(),
            name: "r".into(),
            shorthand: false,
        };
        let url = origin.display().to_string();
        let dest = root.join("code").join("r");
        clone_into(&source(&url), &dest, &AtomicBool::new(false)).unwrap();
        assert!(dest.join(".git").exists());
        // Asked again, the checkout is taken as it is. Its origin must read as
        // a repository address, which a local path does not.
        let remote = "https://example.com/o/r.git";
        git_in(&dest, &["remote", "set-url", "origin", remote]);
        let again = clone_source::parse_clone_source(remote).unwrap();
        clone_into(&again, &dest, &AtomicBool::new(false)).unwrap();
        let other = clone_source::parse_clone_source("https://example.com/o/s.git").unwrap();
        let err = clone_into(&other, &dest, &AtomicBool::new(false)).unwrap_err();
        assert!(err.contains("already holds"), "{err}");

        // A plain folder inside that checkout is not a checkout of it.
        let inner = dest.join("docs");
        std::fs::create_dir(&inner).unwrap();
        let err = clone_into(&again, &inner, &AtomicBool::new(false)).unwrap_err();
        assert!(err.contains("already exists"), "{err}");

        // A clone that fails removes only the folder it made.
        let missing = root.join("nothing-here").display().to_string();
        let gone = root.join("code").join("gone");
        assert!(clone_into(&source(&missing), &gone, &AtomicBool::new(false)).is_err());
        assert!(!gone.exists());
        assert!(dest.join(".git").exists());
        let _ = std::fs::remove_dir_all(&root);
    }

    /// A repository with one commit, at `dir/origin`.
    fn origin_repo(dir: &Path) -> PathBuf {
        let origin = dir.join("origin");
        std::fs::create_dir(&origin).unwrap();
        git_in(&origin, &["init", "--quiet"]);
        git_in(
            &origin,
            &["commit", "--quiet", "--allow-empty", "-m", "one"],
        );
        origin
    }

    /// An `ssh` at `path` that writes down how it was run, next to itself,
    /// and runs what git asks for at the other end here instead.
    #[cfg(unix)]
    fn fake_ssh(path: &Path) {
        use std::os::unix::fs::PermissionsExt as _;
        let script = "#!/bin/sh\n\
            printf '%s\\n' \"$@\" > \"$0.args\"\n\
            for last; do :; done\n\
            eval \"git ${last#git-}\"\n";
        std::fs::write(path, script).unwrap();
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    // The fake ssh is a shell script.
    #[cfg(unix)]
    #[test]
    fn an_ssh_clone_never_waits_on_a_prompt_and_keeps_the_servers_own_ssh() {
        let dir = scratch("ssh");
        let origin = origin_repo(&dir);
        let bin = dir.join("bin");
        std::fs::create_dir(&bin).unwrap();
        fake_ssh(&bin.join("ssh"));
        let source = clone_source::CloneSource {
            url: format!("ssh://example.invalid{}", origin.display()),
            on_github: false,
            label: "o/r".into(),
            name: "r".into(),
            shorthand: false,
        };
        let path = std::env::join_paths(std::iter::once(bin.clone()).chain(std::env::split_paths(
            &std::env::var_os("PATH").unwrap_or_default(),
        )))
        .unwrap();
        let git = || {
            let mut git = git_command();
            git.env("PATH", &path)
                .env_remove("GIT_SSH_COMMAND")
                .env_remove("GIT_SSH");
            git
        };

        // No ssh command set: ssh may not ask for a passphrase or a host key.
        clone_with(git(), &source, &dir.join("plain"), &AtomicBool::new(false)).unwrap();
        assert!(dir.join("plain/.git").exists());
        let args = std::fs::read_to_string(bin.join("ssh.args")).unwrap();
        assert!(args.lines().any(|l| l == "BatchMode=yes"), "{args}");

        // The server's git picks its own ssh, say for the key it signs in
        // with: that one is run as it is.
        std::fs::remove_file(bin.join("ssh.args")).unwrap();
        let own = dir.join("own-ssh");
        fake_ssh(&own);
        let mut configured = git();
        configured
            .env("GIT_CONFIG_COUNT", "1")
            .env("GIT_CONFIG_KEY_0", "core.sshCommand")
            .env("GIT_CONFIG_VALUE_0", &own);
        clone_with(
            configured,
            &source,
            &dir.join("own"),
            &AtomicBool::new(false),
        )
        .unwrap();
        assert!(dir.join("own/.git").exists());
        let args = std::fs::read_to_string(dir.join("own-ssh.args")).unwrap();
        assert!(!args.contains("BatchMode"), "{args}");
        assert!(!bin.join("ssh.args").exists(), "the plain ssh did not run");
        let _ = std::fs::remove_dir_all(&dir);
    }

    // A certificate for 127.0.0.1 and the authority that signed it, made
    // for this test only. They protect nothing.
    #[cfg(target_os = "linux")]
    const TEST_CA: &str = "-----BEGIN CERTIFICATE-----\n\
MIIBmjCCAT+gAwIBAgIUfclK8YbDD/E7oTFEzGr9nNhTVK8wCgYIKoZIzj0EAwIw\n\
GTEXMBUGA1UEAwwOYXJha3VuIHRlc3QgY2EwIBcNMjYxMTE4MjIwNzQxWhgPMjEy\n\
NjEwMjUyMjA3NDFaMBkxFzAVBgNVBAMMDmFyYWt1biB0ZXN0IGNhMFkwEwYHKoZI\n\
zj0CAQYIKoZIzj0DAQcDQgAErelQF52dVIXiqbahI/+vaq8H9EAig6U2bvj6A49O\n\
z7cjlqy1tAguSxCX90QwN/SgbCsgh47sZE9qq8Vo6TDox6NjMGEwHQYDVR0OBBYE\n\
FDs0jzS2DcBpQ7eYpW0sD8nb01BPMB8GA1UdIwQYMBaAFDs0jzS2DcBpQ7eYpW0s\n\
D8nb01BPMA8GA1UdEwEB/wQFMAMBAf8wDgYDVR0PAQH/BAQDAgIEMAoGCCqGSM49\n\
BAMCA0kAMEYCIQCf8GOFkipY1rWcGVoKrfsFnLMWSc86wZVMXi/Z2mi6DgIhAIxt\n\
SklF/YD6bQ6wc1DHFQEZiCjGfv2YijPx9EkeXQjr\n\
-----END CERTIFICATE-----";
    #[cfg(target_os = "linux")]
    const TEST_CERT: &str = "-----BEGIN CERTIFICATE-----\n\
MIIBuTCCAV+gAwIBAgIUEsd7LR7OTgVI9u7xto3rcxZvwuwwCgYIKoZIzj0EAwIw\n\
GTEXMBUGA1UEAwwOYXJha3VuIHRlc3QgY2EwIBcNMjYxMTE4MjIwNzQxWhgPMjEy\n\
NjEwMjUyMjA3NDFaMBQxEjAQBgNVBAMMCTEyNy4wLjAuMTBZMBMGByqGSM49AgEG\n\
CCqGSM49AwEHA0IABGzYq0E5SaAw2wL11P3j15wkZTc1G9iEHhzkK2XeAfzu3axi\n\
BqGTCyuyBo+ZOJUH1thMPnbLvR0C3FU006EWw1+jgYcwgYQwDAYDVR0TAQH/BAIw\n\
ADAOBgNVHQ8BAf8EBAMCB4AwEwYDVR0lBAwwCgYIKwYBBQUHAwEwDwYDVR0RBAgw\n\
BocEfwAAATAdBgNVHQ4EFgQUZjNa4mvSW5vadwvH3urT+QbcaI4wHwYDVR0jBBgw\n\
FoAUOzSPNLYNwGlDt5ilbSwPydvTUE8wCgYIKoZIzj0EAwIDSAAwRQIhAJYzDK6f\n\
pxaUor2zgIk6bx0tPIGQ/OmqE+i+Sx9C/q57AiBfNNVow1DD9GMLXugVvRviVpnb\n\
p2hwF/IQOdR9oPiVBQ==\n\
-----END CERTIFICATE-----";
    #[cfg(target_os = "linux")]
    const TEST_CERT_KEY: &str = "-----BEGIN PRIVATE KEY-----\n\
MIGHAgEAMBMGByqGSM49AgEGCCqGSM49AwEHBG0wawIBAQQgJluVUiTfZ7P2f/2n\n\
LE/y2RAG+2bvZUHKxQdTxLuxnKKhRANCAARs2KtBOUmgMNsC9dT949ecJGU3NRvY\n\
hB4c5Ctl3gH87t2sYgahkwsrsgaPmTiVB9bYTD52y70dAtxVNNOhFsNf\n\
-----END PRIVATE KEY-----";

    /// An https server on 127.0.0.1 that hands out the files under `root`,
    /// as git's plain ("dumb") http protocol reads them, to whoever signs in
    /// as `login` (`user:password`). Returns its port and how many requests
    /// signed in.
    #[cfg(target_os = "linux")]
    fn https_server(
        root: PathBuf,
        login: &str,
    ) -> (u16, std::sync::Arc<std::sync::atomic::AtomicUsize>) {
        use base64::Engine as _;
        use rustls::pki_types::{CertificateDer, PrivateKeyDer, pem::PemObject as _};
        use std::io::{Read as _, Write as _};
        use std::sync::{Arc, atomic::AtomicUsize, atomic::Ordering};
        let provider = Arc::new(rustls::crypto::ring::default_provider());
        let config = rustls::ServerConfig::builder_with_provider(provider)
            .with_safe_default_protocol_versions()
            .unwrap()
            .with_no_client_auth()
            .with_single_cert(
                vec![CertificateDer::from_pem_slice(TEST_CERT.as_bytes()).unwrap()],
                PrivateKeyDer::from_pem_slice(TEST_CERT_KEY.as_bytes()).unwrap(),
            )
            .unwrap();
        let config = Arc::new(config);
        let expected = format!(
            "Basic {}",
            base64::engine::general_purpose::STANDARD.encode(login)
        );
        let signed_in = Arc::new(AtomicUsize::new(0));
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let counted = signed_in.clone();
        // Lives as long as the tests do.
        std::thread::spawn(move || {
            for tcp in listener.incoming().flatten() {
                let (config, root) = (config.clone(), root.clone());
                let (expected, counted) = (expected.clone(), counted.clone());
                std::thread::spawn(move || -> std::io::Result<()> {
                    let conn =
                        rustls::ServerConnection::new(config).map_err(std::io::Error::other)?;
                    let mut tls = rustls::StreamOwned::new(conn, tcp);
                    let mut head = Vec::new();
                    let mut byte = [0u8];
                    while !head.ends_with(b"\r\n\r\n") {
                        if tls.read(&mut byte)? == 0 {
                            return Ok(());
                        }
                        head.push(byte[0]);
                    }
                    let head = String::from_utf8_lossy(&head);
                    let target = head.split_whitespace().nth(1).unwrap_or("/");
                    let signed = head.lines().any(|l| {
                        l.split_once(':').is_some_and(|(k, v)| {
                            k.eq_ignore_ascii_case("authorization") && v.trim() == expected
                        })
                    });
                    let file = root.join(target.split('?').next().unwrap().trim_start_matches('/'));
                    let (status, body) = if !signed {
                        ("401 Unauthorized", Vec::new())
                    } else {
                        counted.fetch_add(1, Ordering::SeqCst);
                        match std::fs::read(&file) {
                            Ok(body) => ("200 OK", body),
                            Err(_) => ("404 Not Found", Vec::new()),
                        }
                    };
                    write!(
                        tls,
                        "HTTP/1.1 {status}\r\nContent-Length: {}\r\n\
                         WWW-Authenticate: Basic realm=\"test\"\r\nConnection: close\r\n\r\n",
                        body.len()
                    )?;
                    tls.write_all(&body)?;
                    tls.conn.send_close_notify();
                    tls.flush()
                });
            }
        });
        (port, signed_in)
    }

    // The server's credential helper is a shell function, and the https
    // server needs rustls, which only Linux tests build.
    #[cfg(target_os = "linux")]
    #[test]
    fn an_https_clone_signs_in_with_the_servers_login_and_never_shows_it() {
        let dir = scratch("https");
        let origin = origin_repo(&dir);
        let served = dir.join("served");
        std::fs::create_dir(&served).unwrap();
        let origin = origin.to_str().unwrap();
        git_in(&served, &["clone", "--quiet", "--bare", origin, "r.git"]);
        git_in(&served.join("r.git"), &["update-server-info"]);
        let ca = dir.join("ca.pem");
        std::fs::write(&ca, TEST_CA).unwrap();
        let (port, signed_in) = https_server(served, "u:right-token");
        let source = clone_source::CloneSource {
            url: format!("https://127.0.0.1:{port}/r.git"),
            on_github: false,
            label: "o/r".into(),
            name: "r".into(),
            shorthand: false,
        };
        // How the server's git signs in: a credential helper, as `gh auth
        // setup-git` or a stored login sets up.
        let git = |password: &str| {
            let mut git = git_command();
            git.env("GIT_CONFIG_COUNT", "2")
                .env("GIT_CONFIG_KEY_0", "http.sslCAInfo")
                .env("GIT_CONFIG_VALUE_0", &ca)
                .env("GIT_CONFIG_KEY_1", "credential.helper")
                .env(
                    "GIT_CONFIG_VALUE_1",
                    format!(
                        "!f() {{ test \"$1\" = get && printf 'username=u\\npassword={password}\\n'; }}; f"
                    ),
                );
            git
        };

        clone_with(
            git("right-token"),
            &source,
            &dir.join("ok"),
            &AtomicBool::new(false),
        )
        .unwrap();
        assert!(dir.join("ok/.git").exists());
        assert!(signed_in.load(std::sync::atomic::Ordering::SeqCst) > 0);
        let config = std::fs::read_to_string(dir.join("ok/.git/config")).unwrap();
        assert!(!config.contains("right-token"), "{config}");

        // A login the host turns down: no prompt to wait on, and what the
        // device is told does not carry the password.
        let err = clone_with(
            git("wrong-token"),
            &source,
            &dir.join("no"),
            &AtomicBool::new(false),
        )
        .unwrap_err();
        assert!(!err.contains("wrong-token"), "{err}");
        assert!(err.contains("Authentication failed"), "{err}");
        assert!(!dir.join("no").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_add_looks_again_when_a_clone_started_while_it_looked() {
        let mut clones = Clones::default();
        let dest = PathBuf::from("/srv/code/app");
        // An Add takes what is being cloned: nothing yet.
        let (cloning, mark) = clones.seen();
        assert!(!being_cloned(&dest, &cloning));
        // A clone starts there while the Add looks at the folder.
        assert!(clones.start(&dest).is_some());
        assert!(clones.start(&dest).is_none(), "one clone at a time");
        assert!(clones.has(&dest));
        // It fails, takes its folder away and ends, all before the Add is
        // back: the reservation is gone, but the Add knows to look again.
        clones.end(&dest);
        assert!(!clones.has(&dest));
        assert!(clones.started_since(mark));
        // Looking again, nothing started meanwhile.
        let (_, mark) = clones.seen();
        assert!(!clones.started_since(mark));
    }

    // The fake ssh is a shell script.
    #[cfg(unix)]
    #[test]
    fn a_clone_the_server_stops_ends_and_leaves_no_folder() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = scratch("stop");
        // An ssh that never answers.
        let ssh = dir.join("slow-ssh");
        std::fs::write(&ssh, "#!/bin/sh\nexec sleep 600\n").unwrap();
        std::fs::set_permissions(&ssh, std::fs::Permissions::from_mode(0o755)).unwrap();
        let mut git = git_command();
        git.env("GIT_SSH_COMMAND", &ssh);
        let source = clone_source::CloneSource {
            url: "ssh://example.invalid/o/r.git".into(),
            on_github: false,
            label: "o/r".into(),
            name: "r".into(),
            shorthand: false,
        };
        let mut clones = Clones::default();
        let dest = dir.join("r");
        let run = clones.start(&dest).unwrap();
        let cloning = {
            let (run, dest, source) = (run.clone(), dest.clone(), source.clone());
            std::thread::spawn(move || {
                run.busy.store(true, Ordering::SeqCst);
                let cloned = clone_with(git, &source, &dest, &run.stop);
                run.busy.store(false, Ordering::SeqCst);
                cloned
            })
        };
        let started = Instant::now();
        while !dest.exists() {
            assert!(
                started.elapsed() < Duration::from_secs(10),
                "the clone began"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
        // The server stops, as `shut_down` does it.
        clones.stop_all();
        let started = Instant::now();
        while clones.busy() {
            assert!(started.elapsed() < CLONE_STOP_WAIT, "the clone ended");
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(cloning.join().unwrap(), Err(SERVER_STOPPED.into()));
        assert!(!dest.exists());
        // A clone asked for after the stop does not begin.
        let late = clone_into(&source, &dir.join("late"), &run.stop);
        assert_eq!(late, Err(SERVER_STOPPED.into()));
        assert!(!dir.join("late").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

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
            relay_address(
                relay::Status::Refused {
                    why: "Blocked.".into(),
                    retry_in: Duration::from_secs(600),
                },
                "x"
            ),
            Address::Problem("Blocked. Trying again in 10 min.".into())
        );
        assert_eq!(
            relay_address(relay::Status::Rejected("Not ours.".into()), "x"),
            Address::Problem("Not ours.".into())
        );
    }

    // On Windows a path through a file reads as "not found".
    #[cfg(unix)]
    #[test]
    fn a_transcript_that_cannot_be_looked_for_is_not_a_missing_one() {
        let dir = std::env::temp_dir().join(format!("arakun-there-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("sessions");
        std::fs::write(&file, b"not a folder").unwrap();
        assert!(matches!(there(&file), Ok(true)));
        assert!(matches!(there(&dir.join("none.json")), Ok(false)));
        // The folder is not one: nothing can be said about what is in it.
        let inside = file.join("s1-0.json");
        assert!(!inside.exists());
        assert!(there(&inside).is_err());
        std::fs::remove_dir_all(&dir).unwrap();
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

    #[test]
    fn a_terminal_s_output_waits_for_the_device_to_draw_it() {
        let mut out = pty::Output::default();
        out.push(&vec![b'y'; TERM_WINDOW as usize * 2]);
        let mut f = Follower::default();

        // One window goes out, in chunks, and then nothing until it is drawn.
        let (chunks, ended) = term_due(&out, &mut f);
        assert_eq!(chunks.len(), TERM_WINDOW as usize / TERM_CHUNK);
        assert_eq!(chunks[1].0, TERM_CHUNK as u64);
        assert_eq!(f.sent, TERM_WINDOW);
        assert_eq!(ended, None);
        assert!(term_due(&out, &mut f).0.is_empty());

        // Drawn up to half: half a window more.
        f.seen = TERM_WINDOW / 2;
        let (chunks, _) = term_due(&out, &mut f);
        assert_eq!(
            chunks.iter().map(|(_, b)| b.len() as u64).sum::<u64>(),
            TERM_WINDOW / 2
        );

        // An exit waits for the output still held back.
        out.exit = Some(Some(0));
        assert_eq!(term_due(&out, &mut f), (Vec::new(), None));
        f.seen = f.sent;
        let (chunks, ended) = term_due(&out, &mut f);
        assert_eq!(f.sent, out.end());
        assert!(!chunks.is_empty());
        assert_eq!(ended, Some(Some(0)));
        // Told once.
        assert_eq!(term_due(&out, &mut f).1, None);
    }

    #[test]
    fn a_device_that_fell_behind_skips_what_was_let_go() {
        let mut out = pty::Output::default();
        out.push(&vec![b'x'; pty::KEPT + 100]);
        let mut f = Follower::default();
        let (chunks, _) = term_due(&out, &mut f);
        assert_eq!(chunks[0].0, 100);
        assert_eq!(f.sent - f.seen, TERM_WINDOW);
    }
}
