//! `arakun <command>` talks to the running app over a Unix socket, so scripts,
//! terminals and agents inside arakun can open pages, files and terminals in their
//! own environment. One JSON line goes each way. Windows 10 and later have the same
//! AF_UNIX sockets; `uds_windows` exposes them, as std only does on Unix.
//!
//! The app exports `ARAKUN_SOCKET` and puts a `arakun` shim first on `PATH` for
//! everything it spawns, so the command works in its terminals and agents even
//! when arakun is not installed.

use std::io::{BufRead as _, BufReader, Read as _, Write as _};
#[cfg(unix)]
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use futures::channel::{mpsc, oneshot};
use serde::{Deserialize, Serialize};
#[cfg(windows)]
use uds_windows::{UnixListener, UnixStream};

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct Request {
    pub cmd: String,
    #[serde(default)]
    pub args: Vec<String>,
    /// Where the command ran; picks the environment it acts on.
    pub cwd: PathBuf,
    /// Show the result in the pane beside the active tab.
    #[serde(default)]
    pub side: bool,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct Reply {
    pub ok: bool,
    #[serde(default)]
    pub message: String,
}

impl Reply {
    pub fn ok(message: impl Into<String>) -> Self {
        Self {
            ok: true,
            message: message.into(),
        }
    }

    pub fn err(message: impl Into<String>) -> Self {
        Self {
            ok: false,
            message: message.into(),
        }
    }
}

pub type Incoming = (Request, oneshot::Sender<Reply>);

pub fn socket_path() -> PathBuf {
    if let Some(p) = std::env::var_os("ARAKUN_SOCKET") {
        return p.into();
    }
    // A separate state file (a second, test instance) gets its own socket.
    if let Some(state) = std::env::var_os("ARAKUN_STATE") {
        return PathBuf::from(state).with_extension("sock");
    }
    runtime_dir().join("control.sock")
}

/// The socket sandboxed agents get instead (`crate::agent::sandbox`), in a folder
/// of its own so a sandbox can show it without the main one.
pub fn sandbox_socket_path() -> PathBuf {
    let main = socket_path();
    let dir = main.parent().unwrap_or(Path::new(".")).join("sandbox");
    let name = main.file_name().map_or_else(
        || "control.sock".into(),
        |n| n.to_string_lossy().into_owned(),
    );
    dir.join(name)
}

/// Where the `arakun` shim is, when there is one.
pub fn shim_dir() -> Option<PathBuf> {
    let bin = shim_path();
    bin.is_dir().then_some(bin)
}

fn shim_path() -> PathBuf {
    shim_path_for(std::env::var_os("ARAKUN_STATE"), runtime_dir)
}

/// A second instance with its own state (`arakun dev`) links its own shim,
/// beside its socket, so it doesn't replace the daily app's.
fn shim_path_for(state: Option<std::ffi::OsString>, runtime: impl FnOnce() -> PathBuf) -> PathBuf {
    state
        .map(|state| PathBuf::from(state).with_extension("runtime"))
        .unwrap_or_else(runtime)
        .join("bin")
}

/// Whether a sandboxed agent may send `req`: only commands that show it things
/// or tell the user, never one that runs or opens something outside.
pub fn sandbox_allows(req: &Request) -> Result<(), String> {
    // Not the browser's debugging port, when one is open: a page there could drive the
    // browser outside the sandbox.
    let web = |u: &str| {
        (u.starts_with("http://") || u.starts_with("https://"))
            && !crate::browser::is_debug_endpoint(u)
    };
    let arg = |i: usize| req.args.get(i).map_or("", String::as_str);
    let ok = match req.cmd.as_str() {
        // Asking the user for a secret shows a card; the value stays outside.
        "logs" | "notify" | "ls" | "rename" | "show" | "focus" | "secret" => true,
        // Asking for the check is fine; setting it would run a command outside.
        "check" => arg(0).is_empty(),
        // A file:// page could show it what the sandbox hides.
        "browse" => web(arg(0)),
        "page" => match arg(1) {
            "open" => web(arg(2)),
            // A screenshot to a path of its choosing would write outside.
            "screenshot" | "shot" => arg(2).is_empty(),
            _ => true,
        },
        // A file tab, not the file's default app.
        "open" => {
            crate::explorer::file_kind(Path::new(arg(0))) != crate::explorer::FileKind::External
        }
        _ => false,
    };
    if ok {
        Ok(())
    } else {
        Err(format!(
            "`arakun {}` is not available to a sandboxed agent: it would act outside the sandbox",
            req.cmd
        ))
    }
}

pub(crate) fn runtime_dir() -> PathBuf {
    #[cfg(windows)]
    return crate::state::data_dir();
    #[cfg(not(windows))]
    std::env::var_os("XDG_RUNTIME_DIR")
        .map(|d| PathBuf::from(d).join("arakun"))
        .unwrap_or_else(crate::state::data_dir)
}

/// Sets up what children inherit: `ARAKUN_SOCKET`, and a `arakun` shim pointing at
/// this binary first on `PATH`. Call before any thread starts.
pub fn export_env() {
    let socket = socket_path();
    let bin = shim_path();
    let linked = std::env::current_exe().ok().is_some_and(|exe| {
        let _ = std::fs::create_dir_all(&bin);
        link_shim(&exe, &bin)
    });
    // SAFETY: called first thing in `main`, before the app starts any thread.
    unsafe {
        std::env::set_var("ARAKUN_SOCKET", &socket);
        if linked {
            let path = std::env::var_os("PATH").unwrap_or_default();
            let mut dirs = vec![bin];
            dirs.extend(std::env::split_paths(&path).filter(|d| !d.ends_with("arakun/bin")));
            if let Ok(joined) = std::env::join_paths(dirs) {
                std::env::set_var("PATH", joined);
            }
        }
    }
}

#[cfg(unix)]
fn link_shim(exe: &Path, bin: &Path) -> bool {
    let shim = bin.join("arakun");
    let _ = std::fs::remove_file(&shim);
    std::os::unix::fs::symlink(exe, &shim).is_ok()
}

/// A symlink needs Developer Mode or an elevated process on Windows; a hard link to the
/// running binary needs neither, and a copy covers a binary on another drive. The shim
/// another arakun is running from cannot be replaced, and is the same binary anyway.
#[cfg(windows)]
fn link_shim(exe: &Path, bin: &Path) -> bool {
    let shim = bin.join("arakun.exe");
    let same = |a: &Path, b: &Path| {
        let (Ok(a), Ok(b)) = (std::fs::metadata(a), std::fs::metadata(b)) else {
            return false;
        };
        a.len() == b.len() && a.modified().ok() == b.modified().ok()
    };
    if same(exe, &shim) {
        return true;
    }
    if std::fs::remove_file(&shim).is_err() && shim.exists() {
        return true;
    }
    std::fs::hard_link(exe, &shim).is_ok() || std::fs::copy(exe, &shim).is_ok()
}

/// Listens for commands; `None` when another arakun already answers on the socket.
/// Two copies on one state don't race to clear a stale socket here: only the
/// one holding the state's lock (`claim`) starts a window.
pub fn serve() -> Option<mpsc::UnboundedReceiver<Incoming>> {
    let path = socket_path();
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let listener = match crate::private_socket::bind(&path) {
        Ok(l) => l,
        Err(e) if e.kind() == std::io::ErrorKind::AddrInUse => {
            if UnixStream::connect(&path).is_ok() {
                eprintln!("arakun: another instance answers on {}", path.display());
                return None;
            }
            // Left behind by a crash.
            let _ = std::fs::remove_file(&path);
            crate::private_socket::bind(&path)
                .map_err(|e| eprintln!("arakun: control socket {}: {e}", path.display()))
                .ok()?
        }
        Err(e) => {
            eprintln!("arakun: control socket {}: {e}", path.display());
            return None;
        }
    };
    SERVING.store(true, Ordering::Relaxed);
    let (tx, rx) = mpsc::unbounded();
    serve_on(listener, tx.clone(), false, "arakun-control").ok()?;
    serve_sandboxed(tx);
    Some(rx)
}

fn serve_on(
    listener: UnixListener,
    tx: mpsc::UnboundedSender<Incoming>,
    sandboxed: bool,
    name: &str,
) -> std::io::Result<()> {
    std::thread::Builder::new()
        .name(name.into())
        .spawn(move || {
            crate::accept::serve(
                listener.incoming(),
                CONNECTIONS,
                "arakun-control-conn",
                || false,
                |stream| {
                    let tx = tx.clone();
                    move || answer(stream, tx, sandboxed)
                },
            );
        })
        .map(|_| ())
}

/// The second socket, for sandboxed agents; without it they only lose `arakun`.
fn serve_sandboxed(tx: mpsc::UnboundedSender<Incoming>) {
    let path = sandbox_socket_path();
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    // Only one instance gets here (it holds the main socket), so an old one is stale.
    let _ = std::fs::remove_file(&path);
    match crate::private_socket::bind(&path) {
        Ok(listener) => {
            let _ = serve_on(listener, tx, true, "arakun-control-sandbox");
        }
        Err(e) => eprintln!("arakun: sandbox control socket {}: {e}", path.display()),
    }
}

/// Commands from the user's own processes; each is one line and its reply.
const CONNECTIONS: crate::accept::Limits = crate::accept::Limits {
    total: 64,
    per_peer: 64,
};

static SERVING: AtomicBool = AtomicBool::new(false);
/// A project to add once the window is up, from `arakun <dir>` when arakun was not running.
static OPEN_AT_START: OnceLock<PathBuf> = OnceLock::new();

pub fn open_at_start() -> Option<PathBuf> {
    OPEN_AT_START.get().cloned()
}

/// Removes the socket on a clean exit, if this process created it.
pub fn cleanup() {
    if SERVING.load(Ordering::Relaxed) {
        let _ = std::fs::remove_file(socket_path());
        let _ = std::fs::remove_file(sandbox_socket_path());
    }
}

fn answer(stream: UnixStream, tx: mpsc::UnboundedSender<Incoming>, sandboxed: bool) {
    let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
    let mut line = String::new();
    let read = BufReader::new((&stream).take(1 << 20)).read_line(&mut line);
    let reply = match read.map_err(|e| e.to_string()).and_then(|_| {
        let req =
            serde_json::from_str::<Request>(&line).map_err(|e| format!("bad request: {e}"))?;
        if sandboxed {
            sandbox_allows(&req)?;
        }
        Ok(req)
    }) {
        Ok(req) => {
            let (done, wait) = oneshot::channel();
            if tx.unbounded_send((req, done)).is_err() {
                Reply::err("arakun is shutting down")
            } else {
                futures::executor::block_on(wait)
                    .unwrap_or_else(|_| Reply::err("arakun did not answer"))
            }
        }
        Err(e) => Reply::err(e),
    };
    let mut stream = stream;
    let _ = writeln!(
        stream,
        "{}",
        serde_json::to_string(&reply).unwrap_or_default()
    );
}

const HELP: &str = "\
arakun — command line for the running arakun window

Usage:
  arakun                       start arakun
  arakun --version             print the version
  arakun <dir> | add [dir]     add a project (or switch to it) and show it
  arakun browse [-s] <url>     open a browser tab (a path opens as file://)
  arakun open [-s] <file[:N]>  open a file tab, at line N
  arakun palette               bring the window up with Ctrl+K open (bind it
                             to a global key, such as Super+K)
  arakun run [--ready <text>] [--port <n>] [--timeout <secs>] <command…>
                             run a command in a new terminal of the dock; with
                             --ready, --port or both, wait until its output
                             shows the text (any case) and the port answers,
                             and fail with its output if it exits first or is
                             not up after --timeout seconds (default 60)
  arakun logs [-n N] [term]    print a dock terminal's last N lines (its number
                             or a word of its title; lists them when unclear)
  arakun logs [term] --until <text> [--timeout <secs>]
                             wait until the terminal prints the text (any
                             case) after this call, and print what it printed;
                             fail if it is not there after --timeout (60 s)
  arakun check                 run the project's check command here, as arakun
                             does after agent turns that edit files
  arakun check --set <command…>
                             make it the project's check command
  arakun rename <branch>       rename this environment's branch (not yet
                             pushed); its folder stays where it is
  arakun notify <message…>     send a desktop notification
  arakun shrink [--] <command…>
                             run a command and print a summary of its output
                             (failures of a test run, a log's repeated lines
                             collapsed, JSON's outline); the whole output goes
                             to a file it names. Needs no running Arakun
  arakun feedback --kind <bug|feature|ux|other> <text…|->
                             tell the Arakun team about a problem or a missing
                             feature in Arakun itself (up to 4000 characters;
                             - reads stdin; needs usage telemetry on)
  arakun notices [--unseen]    the notifications Arakun has shown, newest first
  arakun usage tools [--days N] [--json]
                             where the tokens of Workers AI sessions' tool
                             results go: each tool's calls and tokens, bash by
                             the program it runs (cargo, git…), largest first
  arakun usage where [--days N] [--json]
                             where the tokens of Claude Code sessions go, from
                             their transcripts: unused skills, plugins and MCP
                             servers, long CLAUDE.md files, noisy tools and
                             full sessions, what to fix first (estimates)
  arakun seat add <name>       save a Claude seat: a token from `claude setup-token`,
                             read from stdin and checked with one small call;
                             each new Claude Code session then runs on the seat
                             with the most room in its 5-hour window, for life
  arakun seat ls               the seats and their 5-hour and weekly usage
  arakun seat rm <name>        forget a seat
  arakun ls                    list projects, environments and their sessions
  arakun island [state]        show the Dynamic Island in a state, to try it:
                             sleeping, idle, working, speaking, listening;
                             list pins or unpins the agents' list; auto
                             follows the agents again; fps prints the frames
                             it painted in the last 2 s; with no state, print
                             the one it shows. The island is a program of its
                             own (arakun island-host, which arakun starts when
                             none runs), so these work with arakun closed
  arakun island alert <title> [subtitle]
                             show a card on the island
  arakun island view | later | approve | deny
                             press the card's button (approve and deny
                             answer a headless agent's request)
  arakun agent new [--model <provider/model>] [--effort <level>] [--mode <id>] [--dir <dir>] [--title <text>] [--ask] <prompt…>
                             start a background agent, listed in the sidebar,
                             and print its id: OpenCode, or arakun's own Workers
                             AI agent for a workers-ai/@cf/… model; --effort sets
                             the reasoning (none, low, high, max: per model); it
                             allows its own tool calls unless --ask
  arakun agent ls              list the OpenCode and Workers AI sessions and their status
  arakun agent log [-n N] <id> print a session's status and last N entries (30)
  arakun agent wait [--timeout <secs>] <id>
                             wait until it stops working (at most 3600 s) and
                             print its status and last message; fail on error
  arakun agent send <id> <text…>
                             send it a follow-up
  arakun agent stop <id>       stop its running turn
  arakun agent run [--model <id>] [--dir <dir>] [--title <text>] [--effort <level>]
                   [--policy readonly|workspace|full] <prompt…>
                             start a headless agent (Workers AI) in a process of
                             its own: it goes on with arakun closed; prints its
                             id. ls, log, wait, send and stop work on it too.
                             The policy (default workspace) picks the tool calls
                             it runs alone; the rest wait for approve/deny, and
                             mail, git push, publishing and system changes always do.
                             --wait [--timeout <s>] [--result <file>]: block until
                             the turn ends and print its summary as JSON (what
                             would ask is declined); --no-sandbox --yes confirms
                             no sandbox here; --append-system-prompt <file>
  arakun agent pending [--json]
                             tool calls headless agents wait on you for
  arakun agent approve|deny <id> <req_id>
                             answer one (it times out denied after 24 h)
  arakun agent rm <id>         delete a finished headless session
  arakun server …              arakun without a window, for a machine nobody sits
                             at: it runs agent sessions there, and paired
                             devices follow and answer them (arakun server help)
  arakun fetch [--html | --links] <url>
                             a web page as Markdown (or HTML, or its links),
                             loaded in Cloudflare Browser Run: for sites that
                             turn curl away (arakun fetch --help)
  arakun crawl [--limit N] [--depth N] <url>
                             a site's pages as Markdown, through Browser Run
  arakun show                  bring the window up, also when it was closed
                             with Keep running in the background on
  arakun quit                  quit Arakun
  arakun restart               quit and start again, on the installed build;
                             agent turns running go on by themselves after it
  arakun update [--check]      download the latest release and install it (a
                             running Arakun installs it when it quits);
                             --check only says whether there is one;
                             --channel nightly|stable picks nightly builds
                             of main (they can break) or releases
  arakun mem …                 memory agents keep across sessions: save, search,
                             get, context, list, forget (arakun mem help)
  arakun peer-review <goal>    use one review paired in Teams for this environment
                             and print the team's recommendation
  arakun github draft <owner/repo>#<n> --verdict <v> --body-file <file|-> [--head <sha>] [--agent <name>]
                             save a review as a draft for the GitHub tab (v:
                             approve, request-changes or comment; - reads stdin;
                             --head is the commit read, which an approval needs);
                             nothing is sent to GitHub until you post it there
  arakun github pilot connect <url> --token -
                             connect arakun-pilot, the pull request agent: checks
                             the Worker, then saves its settings (also status,
                             disconnect; arakun github pilot help)
  arakun dev …                 a test arakun on a virtual display, to screenshot
                             and click without touching this screen (arakun dev help)
";

/// The heading of the browser commands, printed after the commands of the features built in.
const HELP_BROWSER: &str = "
Browser tabs of the environment (click, type, press and eval only act on
pages served from this machine: localhost, 127.0.0.1, file://):
";

/// `arakun help` lines of the commands only some builds have.
#[cfg(feature = "coderlabs")]
const HELP_CODERLABS: &str = "\
  arakun canvas <source>       open the Canvas tab on a canvas: a JSON (Doop's
                             get_canvas, or a list of frames), a folder with
                             canvas.json, an .html, or a Doop canvas URL (read
                             with the agent key in DOOP_KEY or
                             ~/.config/arakun/doop_key)
  arakun canvas --stats        frame times of the canvas while it last moved, and
                             what its animations cost since the last --stats
  arakun canvas --bench [zoom] [px/s]
                             pan by itself over every frame (default 0.3 and
                             2400 px/s), to measure frame times with --stats
  arakun voz [on|off]          start or end a voice call with the island's
                             assistant (no word flips it; so does a click on
                             the island's raccoon)
";

/// The page commands, indented as `arakun help` lists them.
fn page_help() -> String {
    PAGE_USAGE.lines().map(|l| format!("  {l}\n")).collect()
}

const HELP_END: &str = "
Commands act on the environment the current directory belongs to.
-s, --side shows the tab in the pane beside the active one.
-h, --help after a command prints its usage. A prompt, message or command line
that starts with a dash needs -- before it (arakun notify -- -1 test failed).
";

enum Cli {
    /// Not a CLI invocation: start the app.
    App,
    /// Start the app with this project added (arakun was not running).
    AppWith(PathBuf),
    Exit(i32),
}

/// Handles `arakun <command>`; returns `false` when the app should start.
pub fn run_cli() -> bool {
    let open = match cli(std::env::args().skip(1).collect()) {
        Cli::App => None,
        Cli::AppWith(dir) => Some(dir),
        Cli::Exit(code) => std::process::exit(code),
    };
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/"));
    let req = match &open {
        Some(dir) => Request {
            cmd: "add".into(),
            args: vec![dir.display().to_string()],
            cwd,
            side: false,
        },
        None => Request {
            cmd: "focus".into(),
            args: Vec::new(),
            cwd,
            side: false,
        },
    };
    let lock = crate::state::lock_path();
    match claim(&lock, &socket_path(), &req, CLAIM_WAIT) {
        Claim::Owner(file) => {
            let _ = OWNER.set(file);
        }
        Claim::Unlocked => {}
        Claim::HandedOff(reply) if reply.ok || req.cmd == "focus" => {
            // An older Arakun doesn't know `focus`; it is running all the same.
            if reply.ok && !reply.message.is_empty() {
                println!("{}", reply.message);
            } else {
                println!("arakun is already running");
            }
            std::process::exit(0);
        }
        // The project could not be added there (say, the folder is gone).
        Claim::HandedOff(reply) => {
            eprintln!("arakun: {}", reply.message);
            std::process::exit(1);
        }
        Claim::Busy => {
            eprintln!(
                "arakun: another Arakun holds {} and does not answer; quit it first, \
                 or set ARAKUN_STATE to run one with its own state",
                lock.display()
            );
            std::process::exit(1);
        }
    }
    if let Some(dir) = open {
        let _ = OPEN_AT_START.set(dir);
    }
    false
}

/// The state lock, held until this process exits; the system lets go of it
/// even after a crash.
static OWNER: OnceLock<std::fs::File> = OnceLock::new();

/// How long a second launch waits for the Arakun holding the state to answer
/// on its socket (it may still be starting) or to quit (`arakun restart`).
const CLAIM_WAIT: Duration = Duration::from_secs(10);

/// What starting the app found about the state it would use.
#[derive(Debug)]
enum Claim {
    /// This process loads and saves the state until it exits.
    Owner(std::fs::File),
    /// The lock could not be taken at all (say, a file system without
    /// locks): start as before rather than not at all.
    Unlocked,
    /// Another Arakun holds the state and took the request.
    HandedOff(Reply),
    /// Another Arakun holds the state and did not answer in time.
    Busy,
}

/// Takes the state's lock, or hands `req` to the Arakun that holds it. Two
/// windows on one state would each save over the other's projects on quit and
/// run the same schedules and rules twice.
fn claim(lock: &Path, socket: &Path, req: &Request, wait: Duration) -> Claim {
    let deadline = Instant::now() + wait;
    loop {
        match try_lock(lock) {
            Ok(Some(file)) => return Claim::Owner(file),
            Ok(None) => {}
            Err(e) => {
                eprintln!("arakun: could not lock {}: {e}", lock.display());
                return Claim::Unlocked;
            }
        }
        // A window that takes the connection but hangs must not hold this
        // launch past its deadline.
        let left = deadline
            .saturating_duration_since(Instant::now())
            .max(Duration::from_millis(200));
        if let Ok(reply) = send_within(socket, req, left) {
            return Claim::HandedOff(reply);
        }
        if Instant::now() >= deadline {
            return Claim::Busy;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// The open lock file when this process got the lock, `None` when another
/// one holds it.
fn try_lock(path: &Path) -> std::io::Result<Option<std::fs::File>> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(path)?;
    match file.try_lock() {
        Ok(()) => Ok(Some(file)),
        Err(std::fs::TryLockError::WouldBlock) => Ok(None),
        Err(std::fs::TryLockError::Error(e)) => Err(e),
    }
}

fn cli(args: Vec<String>) -> Cli {
    let Some(first) = args.first() else {
        // One window: launching it again (the app launcher) shows the running one,
        // hidden or not, rather than starting a second Arakun.
        let req = Request {
            cmd: "show".into(),
            args: Vec::new(),
            cwd: PathBuf::from("/"),
            side: false,
        };
        return match send(&req) {
            Ok(reply) if reply.ok => Cli::Exit(0),
            _ => Cli::App,
        };
    };
    // Needs no display or running window, so the release workflow can run the
    // packaged binary with it.
    if first == "--version" || first == "-V" {
        println!("arakun {}", env!("ARAKUN_VERSION"));
        return Cli::Exit(0);
    }
    // Agents start it; it talks MCP on stdio, not to a person.
    if first == "mcp" {
        return Cli::Exit(crate::secret_mcp::serve());
    }
    if first == "mem" {
        let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/"));
        return Cli::Exit(crate::memory::cli(&args[1..], &cwd));
    }
    #[cfg(feature = "team")]
    if first == "team" {
        return Cli::Exit(crate::team::cli(&args[1..]));
    }
    if first == "github" {
        return Cli::Exit(crate::github::cli(&args[1..]));
    }
    if first == "notices" {
        return Cli::Exit(crate::notices::cli(&args[1..]));
    }
    if first == "seat" {
        return Cli::Exit(crate::agent::seats::cli(&args[1..]));
    }
    if first == "iso" {
        let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/"));
        return Cli::Exit(crate::iso::cli::run(&args[1..], &cwd));
    }
    #[cfg(feature = "coderlabs")]
    if first == "lab" {
        return Cli::Exit(crate::lab::cli(&args[1..]));
    }
    if first == "update" {
        return Cli::Exit(crate::update::cli(&args[1..]));
    }
    if first == "shrink" {
        return Cli::Exit(crate::shrink::cli(&args[1..]));
    }
    if first == "usage" && args.get(1).is_some_and(|a| a == "where") {
        return Cli::Exit(crate::usage_where::cli(&args[2..]));
    }
    if first == "usage" {
        return Cli::Exit(crate::agent::ledger::cli(&args[1..]));
    }
    if first == "fetch" || first == "crawl" {
        return Cli::Exit(crate::fetch::cli(&args[1..], first == "crawl"));
    }
    if first == "agent-host" {
        return Cli::Exit(crate::headless::host_main(&args[1..]));
    }
    if first == "server" {
        return Cli::Exit(crate::server::cli(&args[1..]));
    }
    if first == "island-host" {
        return Cli::Exit(crate::island_host::main());
    }
    if first == "dev" {
        return match crate::dev::cli(&args[1..]) {
            Some(code) => Cli::Exit(code),
            None => cli(args[1..].to_vec()),
        };
    }
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/"));
    let (side, rest) = side_flag(first, &args[1..]);
    // Help comes before anything runs: `arakun notify --help` sent a
    // notification that said `--help`, and `arakun agent run --help` started
    // a paid run with it as the prompt.
    if usage_of(first).is_some() && rest.first().is_some_and(|a| is_help(a)) {
        return stopped(first, Stop::Help);
    }
    if first == "agent" && rest.get(1).is_some_and(|a| is_help(a)) {
        return match rest[0].as_str() {
            "run" => Cli::Exit(report(Stop::Help, crate::headless::RUN_USAGE)),
            _ => stopped(first, Stop::Help),
        };
    }
    // These take no flags of their own: a dash there is a typo, unless `--`
    // comes first to let a message or a name start with one.
    let rest = if NO_FLAGS.contains(&first.as_str()) {
        match payload(&rest) {
            Ok(rest) => rest.to_vec(),
            Err(stop) => return stopped(first, stop),
        }
    } else {
        rest
    };
    let request = |cmd: &str, args: Vec<String>| Request {
        cmd: cmd.into(),
        args,
        cwd: cwd.clone(),
        side,
    };
    let req = match first.as_str() {
        "help" | "-h" | "--help" => {
            let page = page_help();
            let extra: &[&str] = &[
                #[cfg(feature = "coderlabs")]
                HELP_CODERLABS,
                #[cfg(feature = "coderlabs")]
                crate::lab::HELP,
                #[cfg(feature = "team")]
                crate::team::HELP,
            ];
            print!("{HELP}{}{HELP_BROWSER}{page}{HELP_END}", extra.concat());
            return Cli::Exit(0);
        }
        #[cfg(feature = "coderlabs")]
        "canvas" => match rest.as_slice() {
            [flag] if flag == "--stats" => request("canvas", vec![flag.clone()]),
            [flag, rest @ ..]
                if flag == "--bench"
                    && rest.len() <= 2
                    && rest.iter().all(|a| a.parse::<f32>().is_ok()) =>
            {
                request("canvas", args_with(flag, rest))
            }
            [source] if source.starts_with("http://") || source.starts_with("https://") => {
                request("canvas", vec![source.clone()])
            }
            [source] => match cwd.join(source).canonicalize() {
                Ok(path) => request("canvas", vec![path.display().to_string()]),
                Err(e) => return fail(&format!("{source}: {e}")),
            },
            _ => {
                return usage(
                    "arakun canvas <file.json | folder | file.html | doop url> | --stats",
                );
            }
        },
        "browse" => match rest.as_slice() {
            [target] => request("browse", vec![browse_target(&cwd, target)]),
            _ => return stopped(first, Stop::Usage),
        },
        "palette" if rest.is_empty() => request("palette", Vec::new()),
        "palette" => return stopped(first, Stop::Usage),
        "open" => match rest.as_slice() {
            [file] => match resolve_file(&cwd, file) {
                Ok((path, line)) => request(
                    "open",
                    vec![
                        path.display().to_string(),
                        line.map(|n| n.to_string()).unwrap_or_default(),
                    ],
                ),
                Err(e) => return fail(&e),
            },
            _ => return stopped(first, Stop::Usage),
        },
        "run" => match run_args(&rest) {
            Ok(args) => request("run", args),
            Err(stop) => return stopped(first, stop),
        },
        "logs" => match logs_args(&rest) {
            Ok((args, None)) => request("logs", args),
            Ok((mut args, Some(wait))) => {
                args.push("one".into());
                return logs_until(&request("logs", args), &wait);
            }
            Err(stop) => return stopped(first, stop),
        },
        "check" if rest.is_empty() => return check_here(&request("check", Vec::new())),
        "check" => match rest.as_slice() {
            [flag, command @ ..] if flag == "--set" && !command.is_empty() => {
                request("check", vec![command.join(" ")])
            }
            _ => return stopped(first, Stop::Usage),
        },
        "page" => match page_args(&rest) {
            Ok(args) => request("page", args),
            Err(stop) => return stopped(first, stop),
        },
        "agent" if let Some(code) = crate::headless::cli(&rest, &cwd) => return Cli::Exit(code),
        "agent" => match agent_args(&rest) {
            Ok(AgentCmd::Send(mut args)) => {
                if args[0] == "new" && !args[2].is_empty() {
                    match resolve_dir(&cwd, &args[2]) {
                        Ok(dir) => args[2] = dir.display().to_string(),
                        Err(e) => return fail(&e),
                    }
                }
                request("agent", args)
            }
            Ok(AgentCmd::Wait { id, timeout }) => {
                return agent_wait(&request("agent", vec!["status".into(), id]), timeout);
            }
            Err(stop) => return stopped(first, stop),
        },
        #[cfg(feature = "coderlabs")]
        "voz" => return Cli::Exit(crate::island_host::voz(&rest)),
        #[cfg(feature = "coderlabs")]
        "reunion" | "reunión" => return Cli::Exit(crate::island_host::reunion(&rest)),
        "island" => match island_args(&rest) {
            Some(args) => return Cli::Exit(crate::island_host::cli(&args)),
            None => return stopped(first, Stop::Usage),
        },
        "rename" => match rest.as_slice() {
            [name] if !name.starts_with('-') => request("rename", vec![name.clone()]),
            _ => return stopped(first, Stop::Usage),
        },
        "feedback" => {
            match feedback_args(&rest, std::io::stdin(), std::env::var("ARAKUN_AGENT").ok()) {
                Ok(args) => request("feedback", args),
                Err(stop) => return stopped(first, stop),
            }
        }
        "notify" if !rest.is_empty() => request("notify", vec![rest.join(" ")]),
        "notify" => return stopped(first, Stop::Usage),
        "peer-review" if !rest.is_empty() => request("peer-review", vec![rest.join(" ")]),
        "peer-review" => return stopped(first, Stop::Usage),
        "ls" => request("ls", Vec::new()),
        "show" if rest.is_empty() => request("show", Vec::new()),
        "show" => return usage("arakun show"),
        "quit" if rest.is_empty() => request("quit", Vec::new()),
        "quit" => return usage("arakun quit"),
        "restart" if rest.is_empty() => request("restart", Vec::new()),
        "restart" => return stopped(first, Stop::Usage),
        "add" => match rest.as_slice() {
            [] => request("add", vec![cwd.display().to_string()]),
            [dir] => match resolve_dir(&cwd, dir) {
                Ok(dir) => request("add", vec![dir.display().to_string()]),
                Err(e) => return fail(&e),
            },
            _ => return stopped(first, Stop::Usage),
        },
        // `arakun .` like an editor.
        dir if args.len() == 1 && Path::new(dir).is_dir() => match resolve_dir(&cwd, dir) {
            Ok(dir) => request("add", vec![dir.display().to_string()]),
            Err(e) => return fail(&e),
        },
        dir if Path::new(dir).is_dir() => return usage("arakun <dir>"),
        // A typo must not start a second window on the same state.
        word => return unknown(word),
    };
    match send(&req) {
        Ok(reply) if reply.ok => {
            if !reply.message.is_empty() {
                println!("{}", reply.message);
            }
            Cli::Exit(0)
        }
        Ok(reply) => fail(&reply.message),
        Err(e)
            if matches!(
                e.kind(),
                std::io::ErrorKind::NotFound | std::io::ErrorKind::ConnectionRefused
            ) =>
        {
            if req.cmd == "add" {
                Cli::AppWith(PathBuf::from(&req.args[0]))
            } else {
                fail("arakun is not running")
            }
        }
        Err(e) => fail(&format!("could not reach arakun: {e}")),
    }
}

#[cfg(not(windows))]
pub(crate) const ISLAND_USAGE: &str = "arakun island [sleeping|idle|working|speaking|listening|list|auto|fps|view|later|approve|deny] \
     | arakun island alert <title> [subtitle]";
#[cfg(windows)]
pub(crate) const ISLAND_USAGE: &str = "arakun island [sleeping|idle|working|speaking|listening|list|auto|fps|view|later|approve|deny] \
     | arakun island alert <title> [subtitle] \
     | arakun island session <id> <working|waiting|done|error|ended> [project] [doing] \
     | arakun island hook [agent]";

/// `arakun island`'s state, or `alert` with its title and subtitle.
fn island_args(rest: &[String]) -> Option<Vec<String>> {
    match rest {
        [] => Some(Vec::new()),
        [state]
            if [
                "sleeping",
                "idle",
                "working",
                "speaking",
                "listening",
                "list",
                "auto",
                "ver",
                "luego",
                "aprobar",
                "rechazar",
                "fps",
                "view",
                "later",
                "approve",
                "deny",
            ]
            .contains(&state.as_str()) =>
        {
            Some(vec![state.clone()])
        }
        #[cfg(feature = "coderlabs")]
        [wake, state] if wake == "wake" && ["maybe", "yes", "no"].contains(&state.as_str()) => {
            Some(vec![wake.clone(), state.clone()])
        }
        [alert, title, subtitle @ ..] if alert == "alert" && subtitle.len() <= 1 => Some(
            [alert, title]
                .into_iter()
                .chain(subtitle)
                .cloned()
                .collect(),
        ),
        // `island_host::external` checks the rest.
        #[cfg(windows)]
        [first, ..] if first == "session" || first == "hook" => Some(rest.to_vec()),
        _ => None,
    }
}

/// Takes `-s`/`--side` out of a command's arguments. For `run`, `notify` and
/// `agent` only before the rest, which is a command line or a message of its own
/// (`arakun run du -s .`); elsewhere anywhere, as `arakun open file -s` reads.
fn side_flag(cmd: &str, args: &[String]) -> (bool, Vec<String>) {
    let is_flag = |a: &String| matches!(a.as_str(), "-s" | "--side");
    if matches!(cmd, "run" | "notify" | "agent" | "feedback" | "peer-review") {
        let lead = args.iter().take_while(|a| is_flag(a)).count();
        return (lead > 0, args[lead..].to_vec());
    }
    let side = args.iter().any(is_flag);
    (side, args.iter().filter(|a| !is_flag(a)).cloned().collect())
}

/// `--kind <kind> <text…|->` as `[kind, text, agent]`. `-` reads the text from
/// `stdin`; `agent` is the `ARAKUN_AGENT` of the caller. The app checks again.
fn feedback_args(
    rest: &[String],
    stdin: impl std::io::Read,
    agent: Option<String>,
) -> Result<Vec<String>, Stop> {
    let max = crate::surveys::agent::MAX_CHARS;
    let (flag, rest) = rest.split_first().ok_or(Stop::Usage)?;
    if flag != "--kind" {
        // Help, or a flag it does not know, is not the payload.
        flags_end(flag)?;
        return Err(Stop::Usage);
    }
    let (kind, rest) = match rest.split_first() {
        Some((kind, rest)) if crate::surveys::ids::AGENT_KINDS.contains(&kind.as_str()) => {
            (kind, rest)
        }
        _ => return Err(Stop::Usage),
    };
    let rest = payload(rest)?;
    let text = match rest {
        [] => return Err(Stop::Usage),
        [dash] if dash == "-" => {
            let mut bytes = Vec::new();
            // A little past the limit is enough to know it is too long.
            let _ = stdin.take(4 * max as u64 + 4).read_to_end(&mut bytes);
            String::from_utf8_lossy(&bytes).into_owned()
        }
        words => words.join(" "),
    };
    let text = text.trim();
    if text.is_empty() {
        return Err(Stop::Usage);
    }
    if text.chars().count() > max {
        return Err(Stop::Message(format!(
            "the feedback is over {max} characters; shorten it"
        )));
    }
    let agent = crate::surveys::agent::clean_agent(&agent.unwrap_or_default());
    Ok(vec![kind.clone(), text.to_string(), agent])
}

/// Seconds `arakun run --ready/--port` waits unless told otherwise, and the most it may.
pub const RUN_WAIT: u64 = 60;
pub const MAX_RUN_WAIT: u64 = 600;

/// `[--ready text] [--port n] [--timeout secs] command…`, sent as `[command]`,
/// or `[command, text, port, secs]` when there is something to wait for.
/// Only flags before the command count, so `vite --port 3000` keeps its own.
fn run_args(rest: &[String]) -> Result<Vec<String>, Stop> {
    let (mut ready, mut port, mut timeout) = (String::new(), String::new(), String::new());
    let mut i = 0;
    while let Some(flag) = rest.get(i) {
        let slot = match flag.as_str() {
            "--ready" => &mut ready,
            "--port" => &mut port,
            "--timeout" => &mut timeout,
            _ => {
                i += flags_end(flag)?;
                break;
            }
        };
        *slot = rest
            .get(i + 1)
            .filter(|v| !v.is_empty())
            .ok_or(Stop::Usage)?
            .clone();
        i += 2;
    }
    let command = rest[i..].join(" ");
    if command.trim().is_empty() {
        return Err(Stop::Usage);
    }
    if !port.is_empty() && port.parse::<u16>().ok().filter(|p| *p > 0).is_none() {
        return Err(Stop::Usage);
    }
    if !timeout.is_empty()
        && timeout
            .parse::<u64>()
            .ok()
            .filter(|s| (1..=MAX_RUN_WAIT).contains(s))
            .is_none()
    {
        return Err(Stop::Usage);
    }
    match (ready.is_empty() && port.is_empty(), timeout.is_empty()) {
        (true, true) => Ok(vec![command]),
        (true, false) => Err(Stop::Usage),
        (false, _) => Ok(vec![command, ready, port, timeout]),
    }
}

/// How long the client waits for an answer: `arakun page` waits for loads and
/// clicks, each bounded on the app's side; `arakun run --ready` for its timeout.
fn reply_wait(req: &Request) -> Duration {
    if req.cmd == "peer-review" {
        return Duration::from_secs(16 * 60);
    }
    let run = (req.cmd == "run" && req.args.len() > 1).then(|| {
        req.args
            .get(3)
            .and_then(|s| s.parse().ok())
            .unwrap_or(RUN_WAIT)
            + 15
    });
    Duration::from_secs(run.unwrap_or(90).max(90))
}

/// Lines `arakun logs` prints unless told otherwise.
pub const LOG_LINES: usize = 200;

/// What `arakun logs --until` waits for in a terminal's new output.
#[derive(Debug, PartialEq)]
struct LogsWait {
    text: String,
    timeout: Duration,
}

/// `[-n lines] [terminal] [--until text [--timeout secs]]` in any order, sent
/// as `[lines, terminal]`; the wait stays with the client, which polls.
fn logs_args(rest: &[String]) -> Result<(Vec<String>, Option<LogsWait>), Stop> {
    let mut lines = LOG_LINES;
    let mut which: Option<&String> = None;
    let (mut until, mut timeout) = (None, None);
    let mut it = rest.iter();
    let bad = || Stop::Usage;
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "-n" | "--lines" => {
                lines = it
                    .next()
                    .and_then(|n| n.parse().ok())
                    .filter(|n| *n > 0)
                    .ok_or_else(bad)?
            }
            "--until" => until = Some(it.next().filter(|t| !t.is_empty()).ok_or_else(bad)?.clone()),
            "--timeout" => {
                timeout = Some(
                    it.next()
                        .and_then(|s| s.parse::<u64>().ok())
                        .filter(|s| (1..=MAX_RUN_WAIT).contains(s))
                        .ok_or_else(bad)?,
                )
            }
            // No payload: help or a flag it doesn't know counts anywhere.
            _ if flags_end(arg)? > 0 => return Err(bad()),
            _ if which.is_none() => which = Some(arg),
            _ => return Err(bad()),
        }
    }
    let wait = match (until, timeout) {
        (Some(text), secs) => Some(LogsWait {
            text,
            timeout: Duration::from_secs(secs.unwrap_or(RUN_WAIT)),
        }),
        (None, Some(_)) => return Err(bad()),
        (None, None) => None,
    };
    Ok((
        vec![lines.to_string(), which.cloned().unwrap_or_default()],
        wait,
    ))
}

/// What a terminal printed since `before` was read: what follows the last lines
/// of `before` in `now`. Scrollback may have dropped the start of `before`, so
/// the match is on a few lines; the newest of them may since have grown (a
/// prompt with a command typed after it), so it only has to start the same.
fn fresh_output<'a>(before: &str, now: &'a str) -> &'a str {
    let anchor: Vec<&str> = before
        .lines()
        .rev()
        .filter(|l| !l.trim().is_empty())
        .take(3)
        .collect();
    if anchor.is_empty() {
        return now;
    }
    let lines: Vec<(usize, &str)> = now
        .split_inclusive('\n')
        .scan(0, |at, l| {
            let start = *at;
            *at += l.len();
            Some((start, l))
        })
        .collect();
    let line = |i: usize| lines[i].1.trim_end_matches(['\n', '\r']);
    let non_empty: Vec<usize> = (0..lines.len())
        .filter(|&i| !line(i).trim().is_empty())
        .collect();
    // The anchor reads newest first; walk `now`'s non-empty lines the same way.
    // Fewer anchor lines are tried when the full set no longer fits on screen.
    for size in (1..=anchor.len()).rev() {
        for w in (size - 1..non_empty.len()).rev() {
            let same = |k: usize| {
                let (now, was) = (line(non_empty[w - k]).trim_end(), anchor[k].trim_end());
                if k == 0 {
                    now.starts_with(was)
                } else {
                    now == was
                }
            };
            if (0..size).all(same) {
                let end = lines[non_empty[w]];
                return &now[end.0 + end.1.len()..];
            }
        }
    }
    now
}

/// `arakun logs --until`: polls the terminal until new output shows the text,
/// then prints that output; fails when the time runs out or a command that
/// `arakun run` started exits first.
fn logs_until(req: &Request, wait: &LogsWait) -> Cli {
    let mut all = req.clone();
    all.args[0] = usize::MAX.to_string();
    let before = match ask(&all) {
        Ok(text) => text,
        Err(e) => return fail(&e),
    };
    let needle = wait.text.to_lowercase();
    let start = Instant::now();
    loop {
        std::thread::sleep(Duration::from_millis(250));
        let now = match ask(&all) {
            Ok(text) => text,
            Err(e) => return fail(&e),
        };
        let fresh = fresh_output(&before, &now);
        let lines: usize = req.args[0].parse().unwrap_or(LOG_LINES);
        let shown = || {
            let all: Vec<&str> = fresh.trim_end().lines().collect();
            all[all.len().saturating_sub(lines)..].join("\n")
        };
        if fresh.to_lowercase().contains(&needle) {
            println!("{}", shown());
            return Cli::Exit(0);
        }
        let exited = fresh.lines().rev().find_map(|l| {
            l.strip_prefix("[exit ")?
                .strip_suffix(']')?
                .parse::<i32>()
                .ok()
        });
        if let Some(code) = exited {
            eprintln!("{}", shown());
            return fail(&format!(
                "the command exited with code {code} before its output showed \"{}\"",
                wait.text
            ));
        }
        if start.elapsed() >= wait.timeout {
            eprintln!("{}", shown());
            return fail(&format!(
                "no new output showed \"{}\" within {} s",
                wait.text,
                wait.timeout.as_secs()
            ));
        }
    }
}

/// What arakun answered, or why there is no answer.
fn ask(req: &Request) -> Result<String, String> {
    match send(req) {
        Ok(reply) if reply.ok => Ok(reply.message),
        Ok(reply) => Err(reply.message),
        Err(e)
            if matches!(
                e.kind(),
                std::io::ErrorKind::NotFound | std::io::ErrorKind::ConnectionRefused
            ) =>
        {
            Err("arakun is not running".into())
        }
        Err(e) => Err(format!("could not reach arakun: {e}")),
    }
}

const AGENT_USAGE: &str = "\
arakun agent new [--model <provider/model>] [--effort <level>] [--dir <dir>] [--title <text>] [--ask] <prompt…>
       arakun agent ls [--all] | log [-n N] <id> | wait [--timeout <secs>] <id> | send <id> <text…> | stop <id>
       arakun agent run [--model <id>] [--dir <dir>] [--title <t>] [--effort <e>] [--policy <p>] <prompt…>
       arakun agent pending [--json] | approve <id> <req_id> | deny <id> <req_id> [reason…] | rm <id>";

/// Entries `arakun agent log` prints, and seconds `arakun agent wait` waits,
/// unless told otherwise.
pub const AGENT_LOG: usize = 30;
pub const AGENT_WAIT: u64 = 3600;

#[derive(Debug, PartialEq)]
enum AgentCmd {
    /// Sent as `[subcommand, args…]`.
    Send(Vec<String>),
    /// Stays with the client, which polls the session's status.
    Wait { id: String, timeout: Duration },
}

/// `arakun agent …`. `new [--model m] [--effort e] [--dir d] [--title t] [--ask] prompt…`
/// is sent as `[new, model, dir, title, "ask" or "", prompt, effort]`, its flags only
/// before the prompt; `log [-n N] id` as `[log, id, N]`; the others as typed.
fn agent_args(rest: &[String]) -> Result<AgentCmd, Stop> {
    let bad = || Stop::Usage;
    let (sub, rest) = rest.split_first().ok_or_else(bad)?;
    let args = match (sub.as_str(), rest) {
        ("new", _) => {
            let (mut model, mut dir, mut title, mut ask) =
                (String::new(), String::new(), String::new(), false);
            let (mut effort, mut mode) = (String::new(), String::new());
            let mut i = 0;
            while let Some(flag) = rest.get(i) {
                let slot = match flag.as_str() {
                    "--model" => &mut model,
                    "--effort" => &mut effort,
                    "--dir" => &mut dir,
                    "--title" => &mut title,
                    "--mode" => &mut mode,
                    "--ask" => {
                        ask = true;
                        i += 1;
                        continue;
                    }
                    _ => {
                        i += flags_end(flag)?;
                        break;
                    }
                };
                *slot = rest
                    .get(i + 1)
                    .filter(|v| !v.is_empty())
                    .ok_or_else(bad)?
                    .clone();
                i += 2;
            }
            let prompt = rest[i..].join(" ");
            if prompt.trim().is_empty() {
                return Err(bad());
            }
            let ask = if ask {
                "ask".to_string()
            } else {
                String::new()
            };
            vec!["new".into(), model, dir, title, ask, prompt, effort, mode]
        }
        ("log" | "wait", _) => {
            let (mut id, mut lines, mut secs) = (None, AGENT_LOG, AGENT_WAIT);
            let mut it = rest.iter();
            while let Some(arg) = it.next() {
                match arg.as_str() {
                    "-n" | "--lines" if sub == "log" => {
                        lines = it
                            .next()
                            .and_then(|n| n.parse().ok())
                            .filter(|n| *n > 0)
                            .ok_or_else(bad)?
                    }
                    "--timeout" if sub == "wait" => {
                        secs = it
                            .next()
                            .and_then(|s| s.parse().ok())
                            .filter(|s| *s > 0)
                            .ok_or_else(bad)?
                    }
                    _ if is_help(arg) => return Err(Stop::Help),
                    _ if id.is_none() && !arg.starts_with('-') => id = Some(arg.clone()),
                    _ => return Err(bad()),
                }
            }
            let id = id.ok_or_else(bad)?;
            if sub == "wait" {
                return Ok(AgentCmd::Wait {
                    id,
                    timeout: Duration::from_secs(secs),
                });
            }
            vec!["log".into(), id, lines.to_string()]
        }
        ("ls", []) => vec!["ls".into()],
        // Every session, Claude Code and Droid ones too (the voice needs to find them by title).
        ("ls", [all]) if all == "--all" => vec!["ls".into(), "all".into()],
        ("send", [id, text @ ..]) if !text.join(" ").trim().is_empty() => {
            vec!["send".into(), id.clone(), text.join(" ")]
        }
        ("stop", [id]) => vec!["stop".into(), id.clone()],
        _ => return Err(bad()),
    };
    Ok(AgentCmd::Send(args))
}

/// `arakun agent wait`: polls the session until it stops working, then prints
/// its status and the agent's last message; fails when it ended in an error or
/// the time runs out.
fn agent_wait(req: &Request, timeout: Duration) -> Cli {
    let start = Instant::now();
    loop {
        let reply = match ask(req) {
            Ok(text) => text,
            Err(e) => return fail(&e),
        };
        let (status, last) = reply.split_once('\n').unwrap_or((&reply, ""));
        if status != "working" {
            println!("{status}");
            if !last.trim().is_empty() {
                println!("{last}");
            }
            return Cli::Exit(if status == "error" { 1 } else { 0 });
        }
        if start.elapsed() >= timeout {
            return fail(&format!(
                "the session is still working after {} s",
                timeout.as_secs()
            ));
        }
        std::thread::sleep(Duration::from_millis(500));
    }
}

pub const PAGE_USAGE: &str = "\
arakun page [-t N]                 list the browser tabs (-t N picks one for a command)
arakun page screenshot [file]      save a PNG of the page and print where
arakun page console                what the page logged since it loaded
arakun page text                   the page's title, address and visible text
arakun page open <url> | reload    load a page in the tab and wait for it
arakun page click <selector|text>  click an element like a user would
arakun page type <field> <text>    type into a field (CSS selector, label or placeholder)
arakun page press <key>            press Enter, Tab, Escape, Backspace, an arrow…
arakun page eval <js>              run JavaScript in the page and print the result";

/// `[-t N] [command args…]`, sent as `[N or "", command, args…]`; the app checks the rest.
fn page_args(rest: &[String]) -> Result<Vec<String>, Stop> {
    let is_flag = |a: &String| a == "-t" || a == "--tab";
    let (tab, rest) = match rest {
        [flag, n, rest @ ..] if is_flag(flag) => (
            n.parse::<usize>()
                .ok()
                .filter(|n| *n > 0)
                .ok_or(Stop::Usage)?
                .to_string(),
            rest,
        ),
        [flag] if is_flag(flag) => return Err(Stop::Usage),
        _ => (String::new(), rest),
    };
    let rest = payload(rest)?;
    Ok(std::iter::once(tab).chain(rest.iter().cloned()).collect())
}

/// `arakun check`: asks arakun for the project's check and runs it in this
/// process, at the environment's root, the way arakun runs it after a turn.
fn check_here(req: &Request) -> Cli {
    let reply = match send(req) {
        Ok(reply) => reply,
        Err(e)
            if matches!(
                e.kind(),
                std::io::ErrorKind::NotFound | std::io::ErrorKind::ConnectionRefused
            ) =>
        {
            return fail("arakun is not running");
        }
        Err(e) => return fail(&format!("could not reach arakun: {e}")),
    };
    let Some((dir, command)) = reply.message.split_once('\n').filter(|_| reply.ok) else {
        return fail(&reply.message);
    };
    eprintln!("$ {command}");
    let mut cmd = crate::platform::shell_command(command);
    cmd.current_dir(dir).env("CI", "1");
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt as _;
        let err = cmd.exec();
        fail(&format!("could not run sh: {err}"))
    }
    #[cfg(windows)]
    match cmd.status() {
        Ok(status) => Cli::Exit(status.code().unwrap_or(1)),
        Err(e) => fail(&format!("could not run cmd.exe: {e}")),
    }
}

/// `flag` followed by `rest`, as a request's arguments.
#[cfg(feature = "coderlabs")]
fn args_with(flag: &str, rest: &[String]) -> Vec<String> {
    std::iter::once(flag.to_string())
        .chain(rest.iter().cloned())
        .collect()
}

fn usage(line: &str) -> Cli {
    eprintln!("usage: {line}");
    Cli::Exit(2)
}

fn fail(message: &str) -> Cli {
    eprintln!("arakun: {message}");
    Cli::Exit(1)
}

/// The usage of each command `cli` parses itself, for `--help` and mistakes.
fn usage_of(cmd: &str) -> Option<&'static str> {
    Some(match cmd {
        "browse" => "arakun browse [-s] <url>",
        "palette" => "arakun palette",
        "open" => "arakun open [-s] <file[:line]>",
        "run" => "arakun run [--ready <text>] [--port <n>] [--timeout <secs>] [--] <command…>",
        "logs" => "arakun logs [-n lines] [terminal] [--until <text> [--timeout <secs>]]",
        "check" => "arakun check [--set <command…>]",
        "page" => "arakun page [-t N] [command…]  (see arakun page --help)",
        "agent" => AGENT_USAGE,
        "island" => ISLAND_USAGE,
        "rename" => "arakun rename <branch>",
        "notify" => "arakun notify [--] <message…>",
        "peer-review" => "arakun peer-review <goal>",
        "ls" => "arakun ls",
        "feedback" => "arakun feedback --kind <bug|feature|ux|other> <text…|->",
        "restart" => "arakun restart",
        "add" => "arakun add [dir]",
        _ => return None,
    })
}

/// Commands whose arguments are only a payload, without flags of their own.
const NO_FLAGS: &[&str] = &[
    "browse",
    "palette",
    "open",
    "island",
    "rename",
    "notify",
    "peer-review",
    "ls",
    "restart",
    "add",
];

/// Why a command's arguments stop it before it runs.
#[derive(Debug, PartialEq)]
pub(crate) enum Stop {
    /// `-h` or `--help` before the payload: print the usage and exit 0.
    Help,
    /// A flag the command doesn't take, before the payload.
    Flag(String),
    /// Anything else wrong with them.
    Usage,
    /// A mistake with its own words.
    Message(String),
}

pub(crate) fn is_help(arg: &str) -> bool {
    matches!(arg, "-h" | "--help")
}

/// Ends a command's leading flags at `arg`, which is none of the flags it
/// takes: `Ok(1)` skips the `--` that lets a payload start with a dash, and
/// `Ok(0)` means `arg` starts the payload. Help or a mistyped flag must not
/// become the payload: a prompt, a command line or a message.
pub(crate) fn flags_end(arg: &str) -> Result<usize, Stop> {
    match arg {
        "--" => Ok(1),
        _ if is_help(arg) => Err(Stop::Help),
        // `-` alone (stdin) and a quoted "- fix this" are payload.
        _ if arg.len() > 1 && arg.starts_with('-') && !arg.contains(char::is_whitespace) => {
            Err(Stop::Flag(arg.into()))
        }
        _ => Ok(0),
    }
}

/// `args` without a leading `--`, for a command that takes no flags.
fn payload(args: &[String]) -> Result<&[String], Stop> {
    match args.first() {
        Some(first) => Ok(&args[flags_end(first)?..]),
        None => Ok(args),
    }
}

/// Prints what stopped a command and returns its exit code: 0 for help,
/// which goes to stdout, and 2 for a mistake.
pub(crate) fn report(stop: Stop, usage: &str) -> i32 {
    match stop {
        Stop::Help => {
            println!("usage: {usage}");
            0
        }
        Stop::Flag(flag) => {
            eprintln!("arakun: unknown flag {flag} (-- before it passes it on)\nusage: {usage}");
            2
        }
        Stop::Usage => {
            eprintln!("usage: {usage}");
            2
        }
        Stop::Message(m) => {
            eprintln!("arakun: {m}");
            2
        }
    }
}

/// `report` for a command `cli` parses itself.
fn stopped(cmd: &str, stop: Stop) -> Cli {
    // `arakun page` has a line for each of its commands.
    if cmd == "page" && stop == Stop::Help {
        println!("{PAGE_USAGE}");
        return Cli::Exit(0);
    }
    Cli::Exit(report(stop, usage_of(cmd).unwrap_or("arakun help")))
}

/// A first word that is not a command, a flag or a directory.
fn unknown(word: &str) -> Cli {
    if word.starts_with('-') {
        eprintln!("arakun: unknown flag {word}; see arakun --help");
    } else {
        eprintln!("arakun: unknown command {word}, and no such directory; see arakun --help");
    }
    Cli::Exit(2)
}

/// Whether an Arakun window is running and listening for commands.
pub fn app_running() -> bool {
    UnixStream::connect(socket_path()).is_ok()
}

pub fn send(req: &Request) -> std::io::Result<Reply> {
    send_to(&socket_path(), req)
}

pub fn send_to(socket: &Path, req: &Request) -> std::io::Result<Reply> {
    send_within(socket, req, reply_wait(req))
}

/// `send_to`, giving up on the reply after `wait` (more than zero).
pub(crate) fn send_within(socket: &Path, req: &Request, wait: Duration) -> std::io::Result<Reply> {
    let mut stream = UnixStream::connect(socket)?;
    stream.set_read_timeout(Some(wait))?;
    stream.set_write_timeout(Some(wait))?;
    writeln!(stream, "{}", serde_json::to_string(req).unwrap_or_default())?;
    let mut line = String::new();
    BufReader::new(&stream).read_line(&mut line)?;
    serde_json::from_str(&line).map_err(std::io::Error::other)
}

/// An existing local path becomes a `file://` URL; anything else goes to the
/// browser as typed (it already turns `localhost:3000` into a URL).
pub(crate) fn browse_target(cwd: &Path, target: &str) -> String {
    let path = cwd.join(target);
    if !target.contains("://")
        && path.exists()
        && let Ok(abs) = crate::platform::canonicalize(&path)
    {
        return file_url(&abs);
    }
    target.to_string()
}

/// `file:///home/me/a.html`; on Windows `file:///C:/Users/me/a.html`.
fn file_url(abs: &Path) -> String {
    let path = abs.display().to_string().replace('\\', "/");
    if path.starts_with('/') {
        format!("file://{path}")
    } else {
        format!("file:///{path}")
    }
}

/// `file`, or `file:42` / `file:42:7` as compilers and agents print it,
/// with the line.
fn resolve_file(cwd: &Path, file: &str) -> Result<(PathBuf, Option<usize>), String> {
    let (mut name, mut line) = (file, None);
    loop {
        let path = cwd.join(name);
        if path.is_file() {
            return crate::platform::canonicalize(&path)
                .map(|p| (p, line.filter(|&n| n > 0)))
                .map_err(|e| e.to_string());
        }
        if path.is_dir() {
            return Err(format!("{name} is a directory"));
        }
        match name
            .rsplit_once(':')
            .filter(|(_, n)| !n.is_empty() && n.chars().all(|c| c.is_ascii_digit()))
        {
            Some((head, n)) => (name, line) = (head, n.parse().ok()),
            None => return Err(format!("no such file: {file}")),
        }
    }
}

fn resolve_dir(cwd: &Path, dir: &str) -> Result<PathBuf, String> {
    let path = cwd.join(dir);
    if !path.is_dir() {
        return Err(format!("not a directory: {dir}"));
    }
    crate::platform::canonicalize(&path).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {

    #[test]
    fn feedback_needs_kind() {
        assert_eq!(feedback(&["too slow"], "", None), Err(Stop::Usage));
        assert_eq!(feedback(&[], "", None), Err(Stop::Usage));
        assert_eq!(feedback(&["--kind", "bug"], "", None), Err(Stop::Usage));
        let usage = usage_of("feedback").unwrap();
        for kind in ["bug", "feature", "ux", "other"] {
            assert!(usage.contains(kind), "{usage}");
        }
    }

    #[test]
    fn feedback_rejects_unknown_kind() {
        assert_eq!(
            feedback(&["--kind", "praise", "nice"], "", None),
            Err(Stop::Usage)
        );
    }

    #[test]
    fn feedback_reads_stdin_dash() {
        assert_eq!(
            feedback(&["--kind", "ux", "-"], "  the tabs jump\n", None),
            Ok(vec!["ux".into(), "the tabs jump".into(), "agent".into()])
        );
        assert_eq!(
            feedback(&["--kind", "ux", "two", "words"], "", None),
            Ok(vec!["ux".into(), "two words".into(), "agent".into()])
        );
        // Empty text is not a report.
        assert_eq!(
            feedback(&["--kind", "ux", "-"], " \n", None),
            Err(Stop::Usage)
        );
    }

    #[test]
    fn feedback_over_4000_chars_refused() {
        let long = "a".repeat(4001);
        assert!(matches!(
            feedback(&["--kind", "bug", &long], "", None),
            Err(Stop::Message(_))
        ));
        assert!(matches!(
            feedback(&["--kind", "bug", "-"], &long, None),
            Err(Stop::Message(_))
        ));
        assert!(feedback(&["--kind", "bug", &"a".repeat(4000)], "", None).is_ok());
    }

    #[test]
    fn feedback_passes_agent_env() {
        let got = feedback(&["--kind", "bug", "x"], "", Some("codex")).unwrap();
        assert_eq!(got[2], "codex");
        let got = feedback(&["--kind", "bug", "x"], "", Some(String::new().as_str())).unwrap();
        assert_eq!(got[2], "agent");
    }

    #[test]
    fn feedback_text_keeps_dashes_and_side_words() {
        let s = |a: &[&str]| a.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert!(!side_flag("feedback", &s(&["--kind", "bug", "use", "-s"])).0);
    }
    use super::*;

    fn feedback(args: &[&str], stdin: &str, agent: Option<&str>) -> Result<Vec<String>, Stop> {
        let rest: Vec<String> = args.iter().map(|a| a.to_string()).collect();
        feedback_args(&rest, stdin.as_bytes(), agent.map(String::from))
    }

    #[test]
    fn an_isolated_instance_keeps_its_own_shim() {
        let daily = || PathBuf::from("/run/user/1000/arakun");
        assert_eq!(
            shim_path_for(None, daily),
            PathBuf::from("/run/user/1000/arakun/bin")
        );
        assert_eq!(
            shim_path_for(Some("/tmp/arakun-dev/state.json".into()), daily),
            PathBuf::from("/tmp/arakun-dev/state.runtime/bin")
        );
    }

    #[test]
    fn file_with_line_suffix_resolves() {
        let dir = std::env::temp_dir().join(format!("arakun-cli-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("a.rs"), "fn main() {}\n").unwrap();
        let want = crate::platform::canonicalize(dir.join("a.rs")).unwrap();
        assert_eq!(resolve_file(&dir, "a.rs").unwrap(), (want.clone(), None));
        assert_eq!(
            resolve_file(&dir, "a.rs:12").unwrap(),
            (want.clone(), Some(12))
        );
        assert_eq!(
            resolve_file(&dir, "a.rs:12:7").unwrap(),
            (want.clone(), Some(12))
        );
        assert_eq!(resolve_file(&dir, "a.rs:0").unwrap(), (want, None));
        assert!(resolve_file(&dir, "b.rs:3").is_err());
        assert!(resolve_file(&dir, "b.rs").is_err());
        assert!(resolve_file(&dir, ".").is_err());
        assert!(browse_target(&dir, "a.rs").starts_with("file:///"));
        assert_eq!(browse_target(&dir, "localhost:3000"), "localhost:3000");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn logs_takes_a_line_count_and_a_terminal() {
        let args =
            |a: &[&str]| logs_args(&a.iter().map(|s| s.to_string()).collect::<Vec<_>>()).ok();
        let plain = |a: &[&str]| args(a).and_then(|(sent, wait)| (wait.is_none()).then_some(sent));
        assert_eq!(plain(&[]), Some(vec![LOG_LINES.to_string(), String::new()]));
        assert_eq!(
            plain(&["2", "-n", "50"]),
            Some(vec!["50".into(), "2".into()])
        );
        assert_eq!(
            plain(&["-n", "10", "cargo"]),
            Some(vec!["10".into(), "cargo".into()])
        );
        assert_eq!(args(&["-n"]), None);
        assert_eq!(args(&["-n", "0"]), None);
        assert_eq!(args(&["a", "b"]), None);
        let (sent, wait) = args(&["2", "--until", "Compiled", "--timeout", "5"]).unwrap();
        assert_eq!(sent, vec![LOG_LINES.to_string(), "2".into()]);
        assert_eq!(
            wait,
            Some(LogsWait {
                text: "Compiled".into(),
                timeout: Duration::from_secs(5)
            })
        );
        assert_eq!(
            args(&["--until", "x"]).unwrap().1.unwrap().timeout,
            Duration::from_secs(RUN_WAIT)
        );
        assert_eq!(args(&["--until"]), None);
        assert_eq!(args(&["--until", ""]), None);
        assert_eq!(args(&["--timeout", "5"]), None);
        assert_eq!(args(&["--until", "x", "--timeout", "0"]), None);
    }

    #[test]
    fn fresh_output_is_what_came_after() {
        assert_eq!(fresh_output("", "a\nb"), "a\nb");
        assert_eq!(fresh_output("$ ", "$ ls\nx\n"), "x\n");
        assert_eq!(fresh_output("a\nb\n$ ", "a\nb\n$ make\nbuilt\n"), "built\n");
        // The last line read had no newline yet; what came after it is new.
        assert_eq!(
            fresh_output("run\nLISTO", "run\nLISTO\nSEGUNDO\n"),
            "SEGUNDO\n"
        );
        // The scrollback dropped the oldest lines meanwhile.
        assert_eq!(fresh_output("x\ny\nz\nw\n", "z\nw\nnew\n"), "new\n");
        // A watcher printing the same line again: only the new one counts.
        assert_eq!(
            fresh_output("ok\nCompiled\n", "ok\nCompiled\n\nCompiled\n"),
            "\nCompiled\n"
        );
        assert_eq!(fresh_output("ok\nCompiled\n", "ok\nCompiled\n"), "");
        // Nothing recognisable left: all of it is new.
        assert_eq!(fresh_output("gone\n", "fresh\n"), "fresh\n");
    }

    #[test]
    fn unknown_words_and_flags_exit_without_the_app() {
        let cli = |args: &[&str]| cli(args.iter().map(|a| a.to_string()).collect());
        // None of these may reach the socket or start a window: they return
        // before either.
        for args in [
            &["lgos"][..],
            &["doctor"],
            &["--verbose"],
            &["-x"],
            &["lgos", "src"],
            &["--verbose", "."],
            &["no-such-dir-here"],
        ] {
            assert!(matches!(cli(args), Cli::Exit(2)), "{args:?}");
        }
        // A directory takes nothing after it.
        assert!(matches!(cli(&[".", "extra"]), Cli::Exit(2)));
        assert!(matches!(cli(&[]), Cli::App));
    }

    #[test]
    fn help_on_a_command_prints_its_usage_and_runs_nothing() {
        let cli = |args: &[&str]| cli(args.iter().map(|a| a.to_string()).collect());
        // Each returns before the socket: none of them may reach the app.
        for args in [
            &["run", "--help"][..],
            &["run", "--port", "3000", "-h", "npm", "start"],
            &["notify", "--help"],
            &["browse", "--help"],
            &["browse", "-s", "--help"],
            &["open", "-h"],
            &["palette", "--help"],
            &["logs", "--help"],
            &["logs", "2", "--help"],
            &["check", "--help"],
            &["page", "--help"],
            &["page", "-t", "2", "--help"],
            &["island", "--help"],
            &["rename", "--help"],
            &["ls", "--help"],
            &["restart", "--help"],
            &["add", "--help"],
            &["agent", "--help"],
            &["agent", "new", "--help"],
            &["agent", "new", "--model", "m", "--help", "fix"],
            &["agent", "run", "--help"],
            &["agent", "run", "--wait", "--help", "fix"],
            &["agent", "log", "--help"],
            &["agent", "log", "-n", "5", "--help"],
            &["agent", "send", "--help"],
            &["agent", "stop", "-h"],
        ] {
            assert!(matches!(cli(args), Cli::Exit(0)), "{args:?}");
        }
        for args in [
            &["run", "--verbose", "npm"][..],
            &["notify", "--loud", "hi"],
            &["browse", "--new", "x"],
            &["logs", "--follow"],
            &["page", "--json"],
            &["agent", "new", "--modle", "m", "fix"],
            &["agent", "run", "--modle", "m", "fix"],
        ] {
            assert!(matches!(cli(args), Cli::Exit(2)), "{args:?}");
        }
    }

    #[test]
    fn help_after_the_payload_is_part_of_it() {
        let v = |a: &[&str]| a.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(run_args(&v(&["echo", "--help"])), Ok(v(&["echo --help"])));
        assert_eq!(run_args(&v(&["--", "--help"])), Ok(v(&["--help"])));
        assert_eq!(run_args(&v(&["--port", "3000", "-h"])), Err(Stop::Help));
        assert_eq!(
            run_args(&v(&["--wait", "x"])),
            Err(Stop::Flag("--wait".into()))
        );
        assert_eq!(
            payload(&v(&["--", "-1", "failed"])),
            Ok(&v(&["-1", "failed"])[..])
        );
        assert_eq!(payload(&v(&["- quoted", "x"])).map(<[_]>::len), Ok(2));
        assert_eq!(payload(&v(&["-"])).map(<[_]>::len), Ok(1));
        assert_eq!(payload(&v(&["-x"])), Err(Stop::Flag("-x".into())));
        let new = agent_args(&v(&["new", "fix", "--help"])).unwrap();
        assert!(matches!(new, AgentCmd::Send(a) if a[5] == "fix --help"));
        let new = agent_args(&v(&["new", "--", "--help"])).unwrap();
        assert!(matches!(new, AgentCmd::Send(a) if a[5] == "--help"));
        assert_eq!(agent_args(&v(&["log", "-n", "2", "-h"])), Err(Stop::Help));
        assert_eq!(
            page_args(&v(&["eval", "--help"])),
            Ok(v(&["", "eval", "--help"]))
        );
        assert_eq!(page_args(&v(&["-t", "1", "-h"])), Err(Stop::Help));
    }

    #[test]
    fn version_exits_without_the_app() {
        assert!(matches!(cli(vec!["--version".into()]), Cli::Exit(0)));
        assert!(matches!(cli(vec!["-V".into()]), Cli::Exit(0)));
    }

    #[test]
    fn side_flag_leaves_commands_alone() {
        let s = |a: &[&str]| a.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(
            side_flag("run", &s(&["du", "-s", "."])),
            (false, s(&["du", "-s", "."]))
        );
        assert_eq!(
            side_flag("run", &s(&["-s", "ls", "-s"])),
            (true, s(&["ls", "-s"]))
        );
        assert_eq!(
            side_flag("notify", &s(&["use", "-s"])),
            (false, s(&["use", "-s"]))
        );
        assert_eq!(
            side_flag("peer-review", &s(&["check", "-s", "--side"])),
            (false, s(&["check", "-s", "--side"]))
        );
        assert_eq!(side_flag("open", &s(&["a.rs", "-s"])), (true, s(&["a.rs"])));
        assert_eq!(side_flag("browse", &s(&["--side", "x"])), (true, s(&["x"])));
    }

    #[test]
    fn run_takes_wait_flags_before_the_command() {
        let args = |a: &[&str]| run_args(&a.iter().map(|s| s.to_string()).collect::<Vec<_>>()).ok();
        let v = |a: &[&str]| Some(a.iter().map(|s| s.to_string()).collect::<Vec<_>>());
        assert_eq!(args(&["npm", "run", "dev"]), v(&["npm run dev"]));
        assert_eq!(
            args(&["--ready", "Local:", "npm run dev"]),
            v(&["npm run dev", "Local:", "", ""])
        );
        assert_eq!(
            args(&[
                "--port",
                "5173",
                "--timeout",
                "30",
                "vite",
                "--port",
                "5173"
            ]),
            v(&["vite --port 5173", "", "5173", "30"])
        );
        assert_eq!(args(&[]), None);
        assert_eq!(args(&["--ready", "x"]), None);
        assert_eq!(args(&["--ready", "", "make"]), None);
        assert_eq!(args(&["--port", "http", "make"]), None);
        assert_eq!(args(&["--port", "0", "make"]), None);
        assert_eq!(args(&["--timeout", "5", "make"]), None);
        assert_eq!(args(&["--port", "80", "--timeout", "9999", "make"]), None);
        let req = |args: &[&str]| Request {
            cmd: "run".into(),
            args: args.iter().map(|s| s.to_string()).collect(),
            cwd: "/".into(),
            side: false,
        };
        assert_eq!(reply_wait(&req(&["make"])), Duration::from_secs(90));
        assert_eq!(
            reply_wait(&req(&["make", "", "80", "300"])),
            Duration::from_secs(315)
        );
    }

    #[test]
    fn agent_takes_its_subcommands() {
        let args =
            |a: &[&str]| agent_args(&a.iter().map(|s| s.to_string()).collect::<Vec<_>>()).ok();
        let sent = |a: &[&str]| Some(AgentCmd::Send(a.iter().map(|s| s.to_string()).collect()));
        assert_eq!(
            args(&["new", "fix", "the", "build"]),
            sent(&["new", "", "", "", "", "fix the build", "", ""])
        );
        assert_eq!(
            args(&[
                "new",
                "--model",
                "cloudflare-workers-ai/@cf/zai-org/glm-5.3",
                "--dir",
                "..",
                "--ask",
                "go",
                "--ask"
            ]),
            sent(&[
                "new",
                "cloudflare-workers-ai/@cf/zai-org/glm-5.3",
                "..",
                "",
                "ask",
                "go --ask",
                "",
                ""
            ])
        );
        assert_eq!(
            args(&["new", "--title", "Docs", "write", "them"]),
            sent(&["new", "", "", "Docs", "", "write them", "", ""])
        );
        assert_eq!(
            args(&[
                "new",
                "--model",
                "workers-ai/@cf/zai-org/glm-5.3",
                "--effort",
                "high",
                "go",
                "--effort",
                "x"
            ]),
            sent(&[
                "new",
                "workers-ai/@cf/zai-org/glm-5.3",
                "",
                "",
                "",
                "go --effort x",
                "high",
                ""
            ])
        );
        assert_eq!(
            args(&[
                "new",
                "--model",
                "claude/opus",
                "--mode",
                "bypassPermissions",
                "go"
            ]),
            sent(&[
                "new",
                "claude/opus",
                "",
                "",
                "",
                "go",
                "",
                "bypassPermissions"
            ])
        );
        assert_eq!(args(&["new", "--effort", "", "go"]), None);
        assert_eq!(args(&["new", "--effort"]), None);
        assert_eq!(args(&["new"]), None);
        assert_eq!(args(&["new", "--ask"]), None);
        assert_eq!(args(&["new", "--model", "", "go"]), None);
        assert_eq!(args(&["new", "--model"]), None);
        assert_eq!(args(&["ls"]), sent(&["ls"]));
        assert_eq!(args(&["ls", "x"]), None);
        assert_eq!(
            args(&["log", "abc"]),
            sent(&["log", "abc", &AGENT_LOG.to_string()])
        );
        assert_eq!(args(&["log", "-n", "5", "abc"]), sent(&["log", "abc", "5"]));
        assert_eq!(args(&["log", "abc", "-n", "0"]), None);
        assert_eq!(args(&["log"]), None);
        assert_eq!(args(&["log", "a", "b"]), None);
        assert_eq!(
            args(&["wait", "abc"]),
            Some(AgentCmd::Wait {
                id: "abc".into(),
                timeout: Duration::from_secs(AGENT_WAIT)
            })
        );
        assert_eq!(
            args(&["wait", "--timeout", "30", "abc"]),
            Some(AgentCmd::Wait {
                id: "abc".into(),
                timeout: Duration::from_secs(30)
            })
        );
        assert_eq!(args(&["wait", "abc", "-n", "3"]), None);
        assert_eq!(args(&["wait", "--timeout", "0", "abc"]), None);
        assert_eq!(
            args(&["send", "abc", "and", "now", "-s"]),
            sent(&["send", "abc", "and now -s"])
        );
        assert_eq!(args(&["send", "abc"]), None);
        assert_eq!(args(&["stop", "abc"]), sent(&["stop", "abc"]));
        assert_eq!(args(&["stop"]), None);
        assert_eq!(args(&["frobnicate"]), None);
        assert_eq!(args(&[]), None);
        let s = |a: &[&str]| a.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(
            side_flag("agent", &s(&["new", "use", "-s"])),
            (false, s(&["new", "use", "-s"]))
        );
    }

    #[test]
    fn island_takes_a_state_or_an_alert() {
        let a = |v: &[&str]| island_args(&v.iter().map(|s| s.to_string()).collect::<Vec<_>>());
        assert_eq!(a(&[]), Some(vec![]));
        assert_eq!(a(&["working"]), Some(vec!["working".to_string()]));
        assert_eq!(a(&["view"]), Some(vec!["view".to_string()]));
        assert_eq!(a(&["deny"]), Some(vec!["deny".to_string()]));
        assert_eq!(a(&["rechazar"]), Some(vec!["rechazar".to_string()]));
        assert_eq!(
            a(&["alert", "GLM terminó"]),
            Some(vec!["alert".to_string(), "GLM terminó".to_string()])
        );
        assert_eq!(a(&["alert", "t", "s"]).map(|v| v.len()), Some(3));
        assert_eq!(a(&["alert"]), None);
        assert_eq!(a(&["alert", "t", "s", "x"]), None);
        assert_eq!(a(&["dancing"]), None);
    }

    #[cfg(windows)]
    #[test]
    fn island_takes_outside_sessions_on_windows() {
        let a = |v: &[&str]| island_args(&v.iter().map(|s| s.to_string()).collect::<Vec<_>>());
        assert_eq!(a(&["hook"]).map(|v| v.len()), Some(1));
        assert_eq!(a(&["hook", "Codex"]).map(|v| v.len()), Some(2));
        assert_eq!(
            a(&["session", "s1", "done", "web"]).map(|v| v.len()),
            Some(4)
        );
    }

    #[test]
    fn page_takes_a_tab_then_a_command() {
        let args =
            |a: &[&str]| page_args(&a.iter().map(|s| s.to_string()).collect::<Vec<_>>()).ok();
        assert_eq!(args(&[]), Some(vec![String::new()]));
        assert_eq!(
            args(&["-t", "2", "shot"]),
            Some(vec!["2".into(), "shot".into()])
        );
        assert_eq!(
            args(&["eval", "-t"]),
            Some(vec!["".into(), "eval".into(), "-t".into()])
        );
        assert_eq!(args(&["--tab", "x"]), None);
        assert_eq!(args(&["-t"]), None);
    }

    #[test]
    fn requests_round_trip_over_the_socket() {
        let sock = std::env::temp_dir().join(format!("arakun-sock-{}.sock", std::process::id()));
        let _ = std::fs::remove_file(&sock);
        let listener = UnixListener::bind(&sock).unwrap();
        let (tx, mut rx) = mpsc::unbounded::<Incoming>();
        std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            answer(stream, tx, false);
        });
        std::thread::spawn(move || {
            let (req, done) =
                futures::executor::block_on(futures::StreamExt::next(&mut rx)).unwrap();
            let _ = done.send(Reply::ok(format!("{} {}", req.cmd, req.args.join(","))));
        });
        let req = Request {
            cmd: "browse".into(),
            args: vec!["x".into()],
            cwd: "/".into(),
            side: true,
        };
        let mut stream = UnixStream::connect(&sock).unwrap();
        writeln!(stream, "{}", serde_json::to_string(&req).unwrap()).unwrap();
        let mut line = String::new();
        BufReader::new(&stream).read_line(&mut line).unwrap();
        let reply: Reply = serde_json::from_str(&line).unwrap();
        assert!(reply.ok);
        assert_eq!(reply.message, "browse x");
        let _ = std::fs::remove_file(&sock);
    }

    fn claim_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("arakun-claim-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn focus() -> Request {
        Request {
            cmd: "focus".into(),
            args: Vec::new(),
            cwd: "/".into(),
            side: false,
        }
    }

    #[test]
    fn a_second_launch_on_the_same_state_does_not_start() {
        let dir = claim_dir("busy");
        let (lock, sock) = (dir.join("state.lock"), dir.join("state.sock"));
        let wait = Duration::from_millis(300);
        let first = claim(&lock, &sock, &focus(), wait);
        assert!(matches!(first, Claim::Owner(_)), "{first:?}");
        // Nothing answers on the socket: it gives up instead of starting.
        let second = claim(&lock, &sock, &focus(), wait);
        assert!(matches!(second, Claim::Busy), "{second:?}");
        // Once the first one is gone (a quit, a crash, `arakun restart`) the
        // next launch takes over.
        drop(first);
        let third = claim(&lock, &sock, &focus(), wait);
        assert!(matches!(third, Claim::Owner(_)), "{third:?}");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_second_launch_hands_off_to_the_running_one() {
        let dir = claim_dir("hand-off");
        let (lock, sock) = (dir.join("state.lock"), dir.join("state.sock"));
        let first = claim(&lock, &sock, &focus(), Duration::ZERO);
        assert!(matches!(first, Claim::Owner(_)), "{first:?}");
        // The running one answers on its socket.
        let listener = UnixListener::bind(&sock).unwrap();
        let (tx, mut rx) = mpsc::unbounded::<Incoming>();
        std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            answer(stream, tx, false);
        });
        std::thread::spawn(move || {
            let (req, done) =
                futures::executor::block_on(futures::StreamExt::next(&mut rx)).unwrap();
            let _ = done.send(Reply::ok(format!("{} {}", req.cmd, req.args.join(","))));
        });
        let add = Request {
            cmd: "add".into(),
            args: vec!["/p".into()],
            ..focus()
        };
        match claim(&lock, &sock, &add, Duration::from_secs(5)) {
            Claim::HandedOff(reply) => assert_eq!(reply.message, "add /p"),
            other => panic!("{other:?}"),
        }
        drop(first);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_running_one_that_hangs_does_not_hold_the_launch() {
        let dir = claim_dir("hung");
        let (lock, sock) = (dir.join("state.lock"), dir.join("state.sock"));
        let first = claim(&lock, &sock, &focus(), Duration::ZERO);
        assert!(matches!(first, Claim::Owner(_)), "{first:?}");
        // Takes every connection and never answers.
        let listener = UnixListener::bind(&sock).unwrap();
        std::thread::spawn(move || {
            let held: Vec<_> = listener.incoming().collect();
            drop(held);
        });
        let started = Instant::now();
        let second = claim(&lock, &sock, &focus(), Duration::from_millis(500));
        assert!(matches!(second, Claim::Busy), "{second:?}");
        assert!(started.elapsed() < Duration::from_secs(5));
        drop(first);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn instances_with_their_own_state_run_side_by_side() {
        let (a, b) = (claim_dir("own-a"), claim_dir("own-b"));
        let wait = Duration::ZERO;
        let first = claim(&a.join("state.lock"), &a.join("state.sock"), &focus(), wait);
        let second = claim(&b.join("state.lock"), &b.join("state.sock"), &focus(), wait);
        assert!(matches!(first, Claim::Owner(_)), "{first:?}");
        assert!(matches!(second, Claim::Owner(_)), "{second:?}");
        let _ = std::fs::remove_dir_all(a);
        let _ = std::fs::remove_dir_all(b);
    }

    #[test]
    fn a_sandboxed_agent_only_asks_for_what_stays_inside() {
        let req = |cmd: &str, args: &[&str]| Request {
            cmd: cmd.into(),
            args: args.iter().map(|a| a.to_string()).collect(),
            cwd: "/p".into(),
            side: false,
        };
        assert!(sandbox_allows(&req("logs", &["40"])).is_ok());
        assert!(sandbox_allows(&req("browse", &["http://localhost:5173"])).is_ok());
        assert!(sandbox_allows(&req("browse", &["file:///home/u/.ssh/id_ed25519"])).is_err());
        assert!(sandbox_allows(&req("page", &["", "screenshot"])).is_ok());
        assert!(sandbox_allows(&req("page", &["", "open", "file:///etc/passwd"])).is_err());
        assert!(sandbox_allows(&req("page", &["", "shot", "/home/u/.zshrc"])).is_err());
        assert!(sandbox_allows(&req("check", &[])).is_ok());
        assert!(sandbox_allows(&req("check", &["curl x | sh"])).is_err());
        assert!(sandbox_allows(&req("open", &["/p/src/main.rs"])).is_ok());
        for cmd in ["run", "agent", "restart", "quit", "dev", "add", "team"] {
            assert!(sandbox_allows(&req(cmd, &["x"])).is_err(), "{cmd}");
        }
    }

    const DOC_BEGIN: &str = "<!-- cli: begin; written from src/control.rs -->\n";
    const DOC_END: &str = "<!-- cli: end -->\n";

    /// Run with `ARAKUN_BLESS=1` to write `arakun help` into the docs. Only the
    /// commands every build has: the private features' are left out.
    #[test]
    fn the_docs_list_every_command() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("docs/cli.md");
        let doc = std::fs::read_to_string(&path).unwrap();
        let start = doc
            .find(DOC_BEGIN)
            .expect("docs/cli.md has no cli: begin marker");
        let end = doc[start..]
            .find(DOC_END)
            .expect("docs/cli.md has no cli: end marker")
            + start
            + DOC_END.len();
        let want = format!(
            "{DOC_BEGIN}\n```text\n{HELP}{HELP_BROWSER}{}{HELP_END}```\n\n{DOC_END}",
            page_help()
        );
        if doc[start..end] == want {
            return;
        }
        if std::env::var_os("ARAKUN_BLESS").is_some() {
            std::fs::write(&path, format!("{}{want}{}", &doc[..start], &doc[end..])).unwrap();
            return;
        }
        panic!(
            "the commands in docs/cli.md are not `arakun help`; \
             write them with `ARAKUN_BLESS=1 cargo test --no-default-features control`"
        );
    }
}
