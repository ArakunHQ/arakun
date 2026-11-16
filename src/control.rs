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
    let bin = runtime_dir().join("bin");
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
pub fn serve() -> Option<mpsc::UnboundedReceiver<Incoming>> {
    let path = socket_path();
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let listener = match UnixListener::bind(&path) {
        Ok(l) => l,
        Err(e) if e.kind() == std::io::ErrorKind::AddrInUse => {
            if UnixStream::connect(&path).is_ok() {
                eprintln!("arakun: another instance answers on {}", path.display());
                return None;
            }
            // Left behind by a crash.
            let _ = std::fs::remove_file(&path);
            UnixListener::bind(&path)
                .map_err(|e| eprintln!("arakun: control socket {}: {e}", path.display()))
                .ok()?
        }
        Err(e) => {
            eprintln!("arakun: control socket {}: {e}", path.display());
            return None;
        }
    };
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
    }
    SERVING.store(true, Ordering::Relaxed);
    let (tx, rx) = mpsc::unbounded();
    std::thread::Builder::new()
        .name("arakun-control".into())
        .spawn(move || {
            for stream in listener.incoming().flatten() {
                let tx = tx.clone();
                std::thread::spawn(move || answer(stream, tx));
            }
        })
        .ok()?;
    Some(rx)
}

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
    }
}

fn answer(stream: UnixStream, tx: mpsc::UnboundedSender<Incoming>) {
    let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
    let mut line = String::new();
    let read = BufReader::new((&stream).take(1 << 20)).read_line(&mut line);
    let reply = match read.map_err(|e| e.to_string()).and_then(|_| {
        serde_json::from_str::<Request>(&line).map_err(|e| format!("bad request: {e}"))
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
  arakun notices [--unseen]    the notifications Arakun has shown, newest first
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
  arakun island ver | luego | aprobar | rechazar
                             press the card's button (aprobar and rechazar
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
  arakun fetch [--html | --links] <url>
                             a web page as Markdown (or HTML, or its links),
                             loaded in Cloudflare Browser Run: for sites that
                             turn curl away (arakun fetch --help)
  arakun crawl [--limit N] [--depth N] <url>
                             a site's pages as Markdown, through Browser Run
  arakun restart               quit and start again, on the installed build;
                             agent turns running go on by themselves after it
  arakun mem …                 memory agents keep across sessions: save, search,
                             get, context, list, forget (arakun mem help)
  arakun github draft <owner/repo>#<n> --verdict <v> --body-file <file|-> [--head <sha>] [--agent <name>]
                             save a review as a draft for the GitHub tab (v:
                             approve, request-changes or comment; - reads stdin;
                             --head is the commit read, which an approval needs);
                             nothing is sent to GitHub until you post it there
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

const HELP_END: &str = "
Commands act on the environment the current directory belongs to.
-s, --side shows the tab in the pane beside the active one.
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
    match cli(std::env::args().skip(1).collect()) {
        Cli::App => false,
        Cli::AppWith(dir) => {
            let _ = OPEN_AT_START.set(dir);
            false
        }
        Cli::Exit(code) => std::process::exit(code),
    }
}

fn cli(args: Vec<String>) -> Cli {
    let Some(first) = args.first() else {
        return Cli::App;
    };
    // Needs no display or running window, so the release workflow can run the
    // packaged binary with it.
    if first == "--version" || first == "-V" {
        println!("arakun {}", env!("CARGO_PKG_VERSION"));
        return Cli::Exit(0);
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
    if first == "iso" {
        let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/"));
        return Cli::Exit(crate::iso::cli::run(&args[1..], &cwd));
    }
    #[cfg(feature = "coderlabs")]
    if first == "lab" {
        return Cli::Exit(crate::lab::cli(&args[1..]));
    }
    if first == "fetch" || first == "crawl" {
        return Cli::Exit(crate::fetch::cli(&args[1..], first == "crawl"));
    }
    if first == "agent-host" {
        return Cli::Exit(crate::headless::host_main(&args[1..]));
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
    let request = |cmd: &str, args: Vec<String>| Request {
        cmd: cmd.into(),
        args,
        cwd: cwd.clone(),
        side,
    };
    let req = match first.as_str() {
        "help" | "-h" | "--help" => {
            let page = PAGE_USAGE
                .lines()
                .map(|l| format!("  {l}\n"))
                .collect::<String>();
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
            _ => return usage("arakun browse [-s] <url>"),
        },
        "palette" if rest.is_empty() => request("palette", Vec::new()),
        "palette" => return usage("arakun palette"),
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
            _ => return usage("arakun open [-s] <file[:line]>"),
        },
        "run" => match run_args(&rest) {
            Some(args) => request("run", args),
            None => {
                return usage(
                    "arakun run [--ready <text>] [--port <n>] [--timeout <secs>] <command…>",
                );
            }
        },
        "logs" => match logs_args(&rest) {
            Some((args, None)) => request("logs", args),
            Some((mut args, Some(wait))) => {
                args.push("one".into());
                return logs_until(&request("logs", args), &wait);
            }
            None => {
                return usage(
                    "arakun logs [-n lines] [terminal] [--until <text> [--timeout <secs>]]",
                );
            }
        },
        "check" if rest.is_empty() => return check_here(&request("check", Vec::new())),
        "check" => match rest.as_slice() {
            [flag, command @ ..] if flag == "--set" && !command.is_empty() => {
                request("check", vec![command.join(" ")])
            }
            _ => return usage("arakun check [--set <command…>]"),
        },
        "page" => match page_args(&rest) {
            Some(args) => request("page", args),
            None => return usage("arakun page [-t N] [command…]  (see arakun help)"),
        },
        "agent" if let Some(code) = crate::headless::cli(&rest, &cwd) => return Cli::Exit(code),
        "agent" => match agent_args(&rest) {
            Some(AgentCmd::Send(mut args)) => {
                if args[0] == "new" && !args[2].is_empty() {
                    match resolve_dir(&cwd, &args[2]) {
                        Ok(dir) => args[2] = dir.display().to_string(),
                        Err(e) => return fail(&e),
                    }
                }
                request("agent", args)
            }
            Some(AgentCmd::Wait { id, timeout }) => {
                return agent_wait(&request("agent", vec!["status".into(), id]), timeout);
            }
            None => return usage(AGENT_USAGE),
        },
        #[cfg(feature = "coderlabs")]
        "voz" => return Cli::Exit(crate::island_host::voz(&rest)),
        #[cfg(feature = "coderlabs")]
        "reunion" | "reunión" => return Cli::Exit(crate::island_host::reunion(&rest)),
        "island" => match island_args(&rest) {
            Some(args) => return Cli::Exit(crate::island_host::cli(&args)),
            None => return usage(ISLAND_USAGE),
        },
        "rename" => match rest.as_slice() {
            [name] if !name.starts_with('-') => request("rename", vec![name.clone()]),
            _ => return usage("arakun rename <branch>"),
        },
        "notify" if !rest.is_empty() => request("notify", vec![rest.join(" ")]),
        "notify" => return usage("arakun notify <message…>"),
        "ls" => request("ls", Vec::new()),
        "restart" if rest.is_empty() => request("restart", Vec::new()),
        "restart" => return usage("arakun restart"),
        "add" => match rest.as_slice() {
            [] => request("add", vec![cwd.display().to_string()]),
            [dir] => match resolve_dir(&cwd, dir) {
                Ok(dir) => request("add", vec![dir.display().to_string()]),
                Err(e) => return fail(&e),
            },
            _ => return usage("arakun add [dir]"),
        },
        // `arakun .` like an editor; anything else (flags, unknown words) starts the app.
        dir if args.len() == 1 && Path::new(dir).is_dir() => match resolve_dir(&cwd, dir) {
            Ok(dir) => request("add", vec![dir.display().to_string()]),
            Err(e) => return fail(&e),
        },
        _ => return Cli::App,
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

pub(crate) const ISLAND_USAGE: &str = "arakun island [sleeping|idle|working|speaking|listening|list|auto|fps|ver|luego|aprobar|rechazar] \
     | arakun island alert <title> [subtitle]";

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
        _ => None,
    }
}

/// Takes `-s`/`--side` out of a command's arguments. For `run`, `notify` and
/// `agent` only before the rest, which is a command line or a message of its own
/// (`arakun run du -s .`); elsewhere anywhere, as `arakun open file -s` reads.
fn side_flag(cmd: &str, args: &[String]) -> (bool, Vec<String>) {
    let is_flag = |a: &String| matches!(a.as_str(), "-s" | "--side");
    if matches!(cmd, "run" | "notify" | "agent") {
        let lead = args.iter().take_while(|a| is_flag(a)).count();
        return (lead > 0, args[lead..].to_vec());
    }
    let side = args.iter().any(is_flag);
    (side, args.iter().filter(|a| !is_flag(a)).cloned().collect())
}

/// Seconds `arakun run --ready/--port` waits unless told otherwise, and the most it may.
pub const RUN_WAIT: u64 = 60;
pub const MAX_RUN_WAIT: u64 = 600;

/// `[--ready text] [--port n] [--timeout secs] command…`, sent as `[command]`,
/// or `[command, text, port, secs]` when there is something to wait for.
/// Only flags before the command count, so `vite --port 3000` keeps its own.
fn run_args(rest: &[String]) -> Option<Vec<String>> {
    let (mut ready, mut port, mut timeout) = (String::new(), String::new(), String::new());
    let mut i = 0;
    while let Some(flag) = rest.get(i) {
        let slot = match flag.as_str() {
            "--ready" => &mut ready,
            "--port" => &mut port,
            "--timeout" => &mut timeout,
            _ => break,
        };
        *slot = rest.get(i + 1).filter(|v| !v.is_empty())?.clone();
        i += 2;
    }
    let command = rest[i..].join(" ");
    if command.trim().is_empty() {
        return None;
    }
    if !port.is_empty() {
        port.parse::<u16>().ok().filter(|p| *p > 0)?;
    }
    if !timeout.is_empty() {
        timeout
            .parse::<u64>()
            .ok()
            .filter(|s| (1..=MAX_RUN_WAIT).contains(s))?;
    }
    match (ready.is_empty() && port.is_empty(), timeout.is_empty()) {
        (true, true) => Some(vec![command]),
        (true, false) => None,
        (false, _) => Some(vec![command, ready, port, timeout]),
    }
}

/// How long the client waits for an answer: `arakun page` waits for loads and
/// clicks, each bounded on the app's side; `arakun run --ready` for its timeout.
fn reply_wait(req: &Request) -> Duration {
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
fn logs_args(rest: &[String]) -> Option<(Vec<String>, Option<LogsWait>)> {
    let mut lines = LOG_LINES;
    let mut which: Option<&String> = None;
    let (mut until, mut timeout) = (None, None);
    let mut it = rest.iter();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "-n" | "--lines" => lines = it.next()?.parse().ok().filter(|n| *n > 0)?,
            "--until" => until = Some(it.next().filter(|t| !t.is_empty())?.clone()),
            "--timeout" => {
                timeout = Some(
                    it.next()?
                        .parse::<u64>()
                        .ok()
                        .filter(|s| (1..=MAX_RUN_WAIT).contains(s))?,
                )
            }
            _ if which.is_none() => which = Some(arg),
            _ => return None,
        }
    }
    let wait = match (until, timeout) {
        (Some(text), secs) => Some(LogsWait {
            text,
            timeout: Duration::from_secs(secs.unwrap_or(RUN_WAIT)),
        }),
        (None, Some(_)) => return None,
        (None, None) => None,
    };
    Some((
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
fn agent_args(rest: &[String]) -> Option<AgentCmd> {
    let (sub, rest) = rest.split_first()?;
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
                    _ => break,
                };
                *slot = rest.get(i + 1).filter(|v| !v.is_empty())?.clone();
                i += 2;
            }
            let prompt = rest[i..].join(" ");
            if prompt.trim().is_empty() {
                return None;
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
                        lines = it.next()?.parse().ok().filter(|n| *n > 0)?
                    }
                    "--timeout" if sub == "wait" => {
                        secs = it.next()?.parse().ok().filter(|s| *s > 0)?
                    }
                    _ if id.is_none() && !arg.starts_with('-') => id = Some(arg.clone()),
                    _ => return None,
                }
            }
            let id = id?;
            if sub == "wait" {
                return Some(AgentCmd::Wait {
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
        _ => return None,
    };
    Some(AgentCmd::Send(args))
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
fn page_args(rest: &[String]) -> Option<Vec<String>> {
    let is_flag = |a: &String| a == "-t" || a == "--tab";
    let (tab, rest) = match rest {
        [flag, n, rest @ ..] if is_flag(flag) => (
            n.parse::<usize>().ok().filter(|n| *n > 0)?.to_string(),
            rest,
        ),
        [flag] if is_flag(flag) => return None,
        _ => (String::new(), rest),
    };
    Some(std::iter::once(tab).chain(rest.iter().cloned()).collect())
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

pub fn send(req: &Request) -> std::io::Result<Reply> {
    send_to(&socket_path(), req)
}

pub fn send_to(socket: &Path, req: &Request) -> std::io::Result<Reply> {
    let mut stream = UnixStream::connect(socket)?;
    stream.set_read_timeout(Some(reply_wait(req)))?;
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
    use super::*;

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
        let args = |a: &[&str]| logs_args(&a.iter().map(|s| s.to_string()).collect::<Vec<_>>());
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
        assert_eq!(side_flag("open", &s(&["a.rs", "-s"])), (true, s(&["a.rs"])));
        assert_eq!(side_flag("browse", &s(&["--side", "x"])), (true, s(&["x"])));
    }

    #[test]
    fn run_takes_wait_flags_before_the_command() {
        let args = |a: &[&str]| run_args(&a.iter().map(|s| s.to_string()).collect::<Vec<_>>());
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
        let args = |a: &[&str]| agent_args(&a.iter().map(|s| s.to_string()).collect::<Vec<_>>());
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
        assert_eq!(a(&["ver"]), Some(vec!["ver".to_string()]));
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

    #[test]
    fn page_takes_a_tab_then_a_command() {
        let args = |a: &[&str]| page_args(&a.iter().map(|s| s.to_string()).collect::<Vec<_>>());
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
            answer(stream, tx);
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
}
