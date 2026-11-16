//! `arakun server`: Arakun without a window, for a machine nobody sits at. It
//! runs agent sessions there (any agent signed in on that machine) and keeps
//! them going while nothing is connected.
//!
//! `arakun server serve` is the server ([`daemon`]). Paired devices reach it
//! the way phones reach the app, with the same pairing, transports and
//! messages (`crate::phone`): through the relay, end-to-end encrypted, unless
//! it is told another way. A device asks before an agent may do what the agent
//! asks permission for; the user answers from wherever they are.
//!
//! The other commands talk to the running server over a local socket
//! ([`local`]): they pair devices, name the folders sessions can start in,
//! and start, follow and answer sessions by hand, for instance over SSH.
//!
//! Everything it keeps is in `<data>/arakun/server/`, apart from the app's own
//! files: `phone.json` (how it is reached, its relay identity and the paired
//! devices), `state.json` (folders and sessions, see [`store`]), `sessions/`
//! (transcripts) and `uploads/` (photos sent with prompts). The app and a
//! server can run on one machine; they share nothing.
//!
//! The same module is built into `arakun-server` (the `server/` package),
//! which leaves the window out and so needs no display libraries.

// Named by path: `arakun-server` includes this file from its own package.
#[path = "server/daemon.rs"]
pub mod daemon;
#[path = "server/local.rs"]
mod local;
#[path = "server/store.rs"]
mod store;

use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use daemon::Options;
use local::Request;

use crate::phone::Reach;

/// Entries `log` prints unless told otherwise.
pub const LOG: usize = 30;
/// Seconds `wait` waits unless told otherwise.
const WAIT: u64 = 3600;

static NAME: OnceLock<&'static str> = OnceLock::new();

/// How the command is typed, for the hints it prints: `arakun server`, or
/// `arakun-server` in the build that is only the server.
pub fn name() -> &'static str {
    NAME.get().copied().unwrap_or("arakun server")
}

/// Called once by `arakun-server` before it reads its arguments.
#[allow(dead_code)] // the app is always `arakun server`
pub fn set_name(name: &'static str) {
    let _ = NAME.set(name);
}

/// The server's folder. `ARAKUN_SERVER_DIR` names another, for a second
/// server on the machine.
pub fn dir() -> PathBuf {
    std::env::var_os("ARAKUN_SERVER_DIR")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .unwrap_or_else(|| crate::state::data_dir().join("server"))
}

/// The socket the commands reach the server through.
pub fn socket_path() -> PathBuf {
    // A server with a folder of its own gets its own socket in it.
    if std::env::var_os("ARAKUN_SERVER_DIR").is_some() {
        return dir().join("server.sock");
    }
    #[cfg(not(windows))]
    if let Some(runtime) = std::env::var_os("XDG_RUNTIME_DIR") {
        return PathBuf::from(runtime).join("arakun").join("server.sock");
    }
    dir().join("server.sock")
}

fn help() -> String {
    HELP.replace("{name}", name())
}

const HELP: &str = "\
{name} — Arakun without a window, for a machine nobody sits at

It runs agent sessions on this machine and keeps them going while nothing is
connected. Paired devices reach it as phones reach the app: through the relay
(the default; end-to-end encrypted), Tailscale, or the local network.

Usage:
  {name} serve [--reach relay|tailscale|lan] [--port <n>] [--relay <url>]
                             run the server until it is stopped (Ctrl+C, a
                             signal or `quit`). What it is started with is kept
                             for the next run. --port is for tailscale and lan;
                             --relay names a relay of your own
  {name} status              how it is reached, and what it holds
  {name} quit                stop the running server
  {name} pair                print a link (and its QR code) that pairs one
                             device; good for two minutes, and for one device
  {name} devices             the paired devices
  {name} unpair <device>     forget one, by its id or name
  {name} add [--agent <agent>] [dir]
                             let sessions start in a folder (default: this
                             one); --agent is the agent they start with
  {name} remove [dir]        take a folder off that list; its sessions stay
  {name} ls                  the folders and the sessions
  {name} new [--agent claude|droid|opencode|workers-ai] [--dir <dir>]
             [--model <id>] [--effort <level>] [--title <text>] <prompt…>
                             start a session and print its id. The model and
                             the effort are the agent's own names for them,
                             and the folder keeps them for its next session
  {name} log [-n N] <id>     a session's status and last N entries (30)
  {name} wait [--timeout <secs>] <id>
                             wait until it stops working (at most 3600 s) and
                             print its status and last message, or what it
                             asks; fail on error
  {name} send <id> <text…>   send it a follow-up
  {name} stop <id>           stop its running turn
  {name} approve <id> | deny <id>
                             answer the permission it asks for
  {name} answer <id> <answer>…
                             answer its question (one answer for each)
  {name} rm <id>             forget a session that is not working

A session is named by its id or the start of it. Agents use the logins of
this machine: sign in to each one here first.
";

/// What the arguments ask for.
#[derive(Debug, PartialEq)]
enum Command {
    Help,
    Serve(Options),
    /// One request to the running server; its reply is printed.
    Ask(Request),
    Pair,
    Wait {
        id: String,
        timeout: Duration,
    },
}

/// A flag's value, taken off the front of `rest` along with the flag.
fn flag<'a>(rest: &mut &'a [String], names: &[&str]) -> Result<Option<&'a str>, ()> {
    match rest {
        [f, value, tail @ ..] if names.contains(&f.as_str()) => {
            *rest = tail;
            Ok(Some(value))
        }
        // The flag with nothing after it.
        [f] if names.contains(&f.as_str()) => Err(()),
        _ => Ok(None),
    }
}

/// `dir` (from `cwd` when relative) as the server names it: canonical.
fn resolve_dir(cwd: &Path, dir: &str) -> Result<PathBuf, String> {
    let path = cwd.join(dir);
    match crate::platform::canonicalize(&path) {
        Ok(p) if p.is_dir() => Ok(p),
        _ => Err(format!("not a directory: {}", path.display())),
    }
}

fn parse(args: &[String], cwd: &Path) -> Result<Command, String> {
    let Some((sub, mut rest)) = args.split_first() else {
        return Ok(Command::Help);
    };
    let usage = |text: &str| format!("usage: {} {text}", name());
    let ask = |cmd: &str, args: Vec<String>| {
        Ok(Command::Ask(Request {
            cmd: cmd.into(),
            args,
        }))
    };
    match sub.as_str() {
        "help" | "-h" | "--help" => Ok(Command::Help),
        "serve" => {
            let text = "serve [--reach relay|tailscale|lan] [--port <n>] [--relay <url>]";
            let mut opts = Options::default();
            while !rest.is_empty() {
                if let Some(v) = flag(&mut rest, &["--reach"]).map_err(|()| usage(text))? {
                    opts.reach = Some(match v {
                        "relay" => Reach::Relay,
                        "tailscale" => Reach::Tailscale,
                        "lan" => Reach::Lan,
                        _ => return Err(usage(text)),
                    });
                } else if let Some(v) = flag(&mut rest, &["--port"]).map_err(|()| usage(text))? {
                    opts.port = Some(v.parse().map_err(|_| usage(text))?);
                } else if let Some(v) = flag(&mut rest, &["--relay"]).map_err(|()| usage(text))? {
                    opts.relay = Some(v.to_string());
                } else {
                    return Err(usage(text));
                }
            }
            Ok(Command::Serve(opts))
        }
        "status" | "devices" | "ls" | "quit" if rest.is_empty() => ask(sub, Vec::new()),
        "pair" if rest.is_empty() => Ok(Command::Pair),
        "status" | "devices" | "ls" | "quit" | "pair" => Err(usage(sub)),
        "unpair" => match rest {
            [device] => ask("unpair", vec![device.clone()]),
            _ => Err(usage("unpair <device>")),
        },
        "add" => {
            let text = "add [--agent <agent>] [dir]";
            let agent = flag(&mut rest, &["--agent"]).map_err(|()| usage(text))?;
            let dir = match rest {
                [] => ".",
                [dir] => dir,
                _ => return Err(usage(text)),
            };
            let dir = resolve_dir(cwd, dir)?;
            ask(
                "add",
                vec![dir.display().to_string(), agent.unwrap_or("").to_string()],
            )
        }
        "remove" => {
            let dir = match rest {
                [] => ".",
                [dir] => dir,
                _ => return Err(usage("remove [dir]")),
            };
            // A folder that is gone can still be taken off the list, as it was named.
            let dir = resolve_dir(cwd, dir).unwrap_or_else(|_| cwd.join(dir));
            ask("remove", vec![dir.display().to_string()])
        }
        "new" => {
            let text = "new [--agent <agent>] [--dir <dir>] [--model <id>] [--effort <level>] [--title <text>] <prompt…>";
            let (mut agent, mut dir, mut model, mut effort, mut title) = ("", ".", "", "", "");
            loop {
                let before = rest.len();
                for (names, slot) in [
                    (&["--agent"][..], &mut agent),
                    (&["--dir", "--cwd"][..], &mut dir),
                    (&["--model"][..], &mut model),
                    (&["--effort"][..], &mut effort),
                    (&["--title"][..], &mut title),
                ] {
                    if let Some(v) = flag(&mut rest, names).map_err(|()| usage(text))? {
                        *slot = v;
                    }
                }
                if rest.len() == before {
                    break;
                }
            }
            let prompt = rest.join(" ");
            if prompt.trim().is_empty() {
                return Err(usage(text));
            }
            let dir = resolve_dir(cwd, dir)?;
            ask(
                "new",
                vec![
                    agent.to_string(),
                    dir.display().to_string(),
                    title.to_string(),
                    model.to_string(),
                    effort.to_string(),
                    prompt,
                ],
            )
        }
        "log" => {
            let text = "log [-n N] <id>";
            let lines = match flag(&mut rest, &["-n", "--lines"]).map_err(|()| usage(text))? {
                Some(n) => n
                    .parse::<usize>()
                    .ok()
                    .filter(|n| *n > 0)
                    .ok_or_else(|| usage(text))?,
                None => LOG,
            };
            match rest {
                [id] => ask("log", vec![id.clone(), lines.to_string()]),
                _ => Err(usage(text)),
            }
        }
        "wait" => {
            let text = "wait [--timeout <secs>] <id>";
            let secs = match flag(&mut rest, &["--timeout"]).map_err(|()| usage(text))? {
                Some(s) => s
                    .parse::<u64>()
                    .ok()
                    .filter(|s| *s > 0)
                    .ok_or_else(|| usage(text))?,
                None => WAIT,
            };
            match rest {
                [id] => Ok(Command::Wait {
                    id: id.clone(),
                    timeout: Duration::from_secs(secs),
                }),
                _ => Err(usage(text)),
            }
        }
        "send" => match rest {
            [id, text @ ..] if !text.join(" ").trim().is_empty() => {
                ask("send", vec![id.clone(), text.join(" ")])
            }
            _ => Err(usage("send <id> <text…>")),
        },
        "stop" | "approve" | "deny" | "rm" => match rest {
            [id] => ask(sub, vec![id.clone()]),
            _ => Err(usage(&format!("{sub} <id>"))),
        },
        "answer" => match rest {
            [_id, _first, ..] => ask("answer", rest.to_vec()),
            _ => Err(usage("answer <id> <answer>…")),
        },
        other => Err(format!(
            "unknown command `{} {other}` (see {} help)",
            name(),
            name()
        )),
    }
}

fn fail(message: &str) -> i32 {
    eprintln!("{}: {message}", name());
    1
}

/// Asks the running server; its message, or why there is none.
fn ask(req: &Request) -> Result<String, String> {
    ask_within(req, local::REPLY_WAIT)
}

/// [`ask`], giving the server at most `wait` to answer.
fn ask_within(req: &Request, wait: Duration) -> Result<String, String> {
    let socket = socket_path();
    match local::send(&socket, req, wait) {
        Ok(reply) if reply.ok => Ok(reply.message),
        Ok(reply) => Err(reply.message),
        Err(e)
            if matches!(
                e.kind(),
                std::io::ErrorKind::NotFound | std::io::ErrorKind::ConnectionRefused
            ) =>
        {
            Err(format!(
                "no server is running here; start one with `{} serve`",
                name()
            ))
        }
        Err(e)
            if matches!(
                e.kind(),
                std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
            ) =>
        {
            Err("the server did not answer in time".into())
        }
        Err(e) => Err(format!("{}: {e}", socket.display())),
    }
}

/// `arakun server …` (and all of `arakun-server …`): the exit code.
pub fn cli(args: &[String]) -> i32 {
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/"));
    let command = match parse(args, &cwd) {
        Ok(c) => c,
        Err(e) if e.starts_with("usage: ") => {
            eprintln!("{e}");
            return 2;
        }
        Err(e) => return fail(&e),
    };
    match command {
        Command::Help => {
            print!("{}", help());
            0
        }
        Command::Serve(opts) => serve(opts),
        Command::Ask(req) => match ask(&req) {
            Ok(message) => {
                println!("{message}");
                0
            }
            Err(e) => fail(&e),
        },
        Command::Pair => match ask(&Request::new("pair", &[])) {
            Ok(message) => {
                print!("{}", pairing_text(&message));
                0
            }
            Err(e) => fail(&e),
        },
        Command::Wait { id, timeout } => wait(&id, timeout),
    }
}

fn serve(opts: Options) -> i32 {
    // Under launchd the PATH has no agent in it; the login shell's has.
    crate::platform::import_login_path();
    // The agents' prompts describe the app's window and the commands that
    // drive it; there is no window here.
    if std::env::var_os("ARAKUN_AGENT_HINT").is_none() {
        // SAFETY: the first thing this process does, before it starts any thread.
        unsafe { std::env::set_var("ARAKUN_AGENT_HINT", "off") };
    }
    daemon::run(opts)
}

/// What `pair` prints, from the server's reply: the link, its QR code for a
/// phone to scan off the terminal, and the code to type where there is one.
fn pairing_text(reply: &str) -> String {
    let v: serde_json::Value = serde_json::from_str(reply).unwrap_or_default();
    let text = |key: &str| v[key].as_str().unwrap_or("").to_string();
    let (url, code, waiting) = (text("url"), text("code"), text("waiting"));
    let minutes = v["seconds"].as_u64().unwrap_or(120) / 60;
    let mut out = format!(
        "Open this on the device within {minutes} minutes. It pairs one device.\n\n  {url}\n"
    );
    if !code.is_empty() {
        out.push_str(&format!("\n  code: {code}\n"));
    }
    if let Ok(qr) = qrcode::QrCode::new(url.as_bytes()) {
        let image = qr
            .render::<qrcode::render::unicode::Dense1x2>()
            .quiet_zone(true)
            .build();
        out.push_str(&format!("\n{image}\n"));
    }
    if !waiting.is_empty() {
        out.push_str(&format!(
            "\nThe server is not reachable right now ({waiting}); the link works once it is.\n"
        ));
    }
    out
}

/// `wait`: polls the session until it stops working, then prints its status
/// and what the server has to say about it; fails when it ended in an error
/// or the time runs out.
fn wait(id: &str, timeout: Duration) -> i32 {
    let req = Request::new("state", &[id]);
    let start = Instant::now();
    let still_working = || {
        fail(&format!(
            "the session is still working after {} s",
            timeout.as_secs()
        ))
    };
    loop {
        // The time left holds for the question itself too: a server that
        // takes it and says nothing must not outlast --timeout.
        let left = timeout.saturating_sub(start.elapsed());
        if left.is_zero() {
            return still_working();
        }
        let reply = match ask_within(&req, left.min(local::REPLY_WAIT)) {
            Ok(text) => text,
            Err(e) => return fail(&e),
        };
        let (status, rest) = reply.split_once('\n').unwrap_or((&reply, ""));
        if status != "working" {
            println!("{status}");
            if !rest.trim().is_empty() {
                println!("{rest}");
            }
            return if status == "error" { 1 } else { 0 };
        }
        let left = timeout.saturating_sub(start.elapsed());
        if left.is_zero() {
            return still_working();
        }
        std::thread::sleep(left.min(Duration::from_millis(500)));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(words: &[&str]) -> Vec<String> {
        words.iter().map(|w| w.to_string()).collect()
    }

    fn sent(cmd: &str, words: &[&str]) -> Result<Command, String> {
        Ok(Command::Ask(Request::new(cmd, words)))
    }

    #[test]
    fn serve_reads_how_it_is_reached() {
        let cwd = Path::new("/");
        assert_eq!(
            parse(&args(&["serve"]), cwd),
            Ok(Command::Serve(Options::default()))
        );
        assert_eq!(
            parse(
                &args(&[
                    "serve",
                    "--port",
                    "7575",
                    "--reach",
                    "tailscale",
                    "--relay",
                    "https://relay.example"
                ]),
                cwd
            ),
            Ok(Command::Serve(Options {
                reach: Some(Reach::Tailscale),
                port: Some(7575),
                relay: Some("https://relay.example".into()),
            }))
        );
        for bad in [
            &["serve", "--reach", "wifi"][..],
            &["serve", "--port", "many"],
            &["serve", "--port"],
            &["serve", "now"],
        ] {
            let e = parse(&args(bad), cwd).unwrap_err();
            assert!(e.starts_with("usage: arakun server serve"), "{e}");
        }
    }

    #[test]
    fn new_takes_its_flags_in_any_order_before_the_prompt() {
        let dir = std::env::temp_dir();
        let here = crate::platform::canonicalize(&dir).unwrap();
        let here = here.display().to_string();
        assert_eq!(
            parse(&args(&["new", "fix", "the", "build"]), &dir),
            sent("new", &["", &here, "", "", "", "fix the build"])
        );
        assert_eq!(
            parse(
                &args(&[
                    "new", "--model", "opus", "--agent", "claude", "--title", "Build", "--effort",
                    "high", "fix", "--dir", "it"
                ]),
                &dir
            ),
            // After the prompt starts, the rest is the prompt.
            sent(
                "new",
                &["claude", &here, "Build", "opus", "high", "fix --dir it"]
            )
        );
        assert!(
            parse(&args(&["new", "--agent", "claude"]), &dir)
                .unwrap_err()
                .starts_with("usage: ")
        );
        assert!(
            parse(&args(&["new", "--dir", "no-such-folder-here", "hi"]), &dir)
                .unwrap_err()
                .starts_with("not a directory")
        );
    }

    #[test]
    fn session_commands_name_their_session() {
        let cwd = Path::new("/");
        assert_eq!(
            parse(&args(&["log", "s1"]), cwd),
            sent("log", &["s1", "30"])
        );
        assert_eq!(
            parse(&args(&["log", "-n", "5", "s1"]), cwd),
            sent("log", &["s1", "5"])
        );
        assert!(parse(&args(&["log", "-n", "0", "s1"]), cwd).is_err());
        assert_eq!(
            parse(&args(&["send", "s1", "and", "the tests"]), cwd),
            sent("send", &["s1", "and the tests"])
        );
        assert!(parse(&args(&["send", "s1"]), cwd).is_err());
        assert_eq!(
            parse(&args(&["approve", "s1"]), cwd),
            sent("approve", &["s1"])
        );
        assert_eq!(
            parse(&args(&["answer", "s1", "yes", "the second"]), cwd),
            sent("answer", &["s1", "yes", "the second"])
        );
        assert!(parse(&args(&["answer", "s1"]), cwd).is_err());
        assert_eq!(
            parse(&args(&["wait", "--timeout", "5", "s1"]), cwd),
            Ok(Command::Wait {
                id: "s1".into(),
                timeout: Duration::from_secs(5)
            })
        );
        assert_eq!(
            parse(&args(&["wait", "s1"]), cwd),
            Ok(Command::Wait {
                id: "s1".into(),
                timeout: Duration::from_secs(WAIT)
            })
        );
        assert!(parse(&args(&["rm"]), cwd).is_err());
    }

    #[test]
    fn the_rest_is_help_or_an_error() {
        let cwd = Path::new("/");
        assert_eq!(parse(&[], cwd), Ok(Command::Help));
        assert_eq!(parse(&args(&["--help"]), cwd), Ok(Command::Help));
        assert_eq!(parse(&args(&["pair"]), cwd), Ok(Command::Pair));
        assert_eq!(parse(&args(&["ls"]), cwd), sent("ls", &[]));
        assert!(parse(&args(&["ls", "all"]), cwd).is_err());
        let e = parse(&args(&["fly"]), cwd).unwrap_err();
        assert!(e.contains("unknown command `arakun server fly`"), "{e}");
        assert!(help().contains("arakun server serve"));
        assert!(!help().contains("{name}"));
    }

    #[test]
    fn pairing_prints_the_link_and_the_code_when_there_is_one() {
        let lan = pairing_text(
            r#"{"url":"http://10.0.0.2:7474/?p=123456","code":"123456","seconds":120,"waiting":""}"#,
        );
        assert!(lan.contains("within 2 minutes"));
        assert!(lan.contains("\n  http://10.0.0.2:7474/?p=123456\n"));
        assert!(lan.contains("code: 123456"));
        assert!(!lan.contains("not reachable"));
        // The QR code: blocks, many lines of them.
        assert!(lan.lines().filter(|l| l.contains('█')).count() > 10);

        let relay = pairing_text(
            r#"{"url":"https://abc.relay.example/#p=c&s=s","code":"","seconds":120,"waiting":"not yet: connecting to the relay"}"#,
        );
        assert!(!relay.contains("code:"));
        assert!(relay.contains("(not yet: connecting to the relay)"));
    }

    #[test]
    fn a_server_with_its_own_folder_has_its_own_socket() {
        // Only what does not read the environment: other tests run beside this one.
        assert!(dir().ends_with("server") || std::env::var_os("ARAKUN_SERVER_DIR").is_some());
        assert_eq!(socket_path().file_name().unwrap(), "server.sock");
    }
}
