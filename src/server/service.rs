//! `arakun server service …`: keeps the server running after the shell closes and
//! starts it again at login or boot. On Linux it is a systemd user unit with
//! lingering on, so it runs while nobody is logged in and starts at boot; as
//! root (a VPS often has only root) it is a system unit instead. macOS gets a
//! launchd agent. Each runs this same binary with `serve`, the `PATH` of the
//! shell that installed it (where the agents are) and `ARAKUN_SERVER_DIR` if
//! it is set.
//!
//! Both restart the server when it fails, but not after `quit`: `quit` exits
//! cleanly, and stopping the server is what the user asked for.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use super::local::{self, Request};
use super::{dir, fail, name, socket_path};

/// How long `install` waits for the started server to answer on its socket.
const START_WAIT: Duration = Duration::from_secs(15);

/// What the service runs, and what it is called.
#[derive(Debug, PartialEq)]
pub struct Spec {
    /// The systemd unit (without `.service`) or the launchd label.
    pub name: String,
    pub exe: PathBuf,
    pub args: Vec<String>,
    pub env: Vec<(String, String)>,
    /// Where launchd writes the server's output. systemd keeps it in the journal.
    pub log: PathBuf,
}

/// `service …`, after the word `service`.
#[derive(Debug, PartialEq)]
pub enum Action {
    /// `serve`'s flags, and whether to only print the file.
    Install {
        flags: Vec<String>,
        print: bool,
    },
    Uninstall,
    Status,
    Logs {
        follow: bool,
    },
}

pub const USAGE: &str = "service install [--print] [serve flags] | uninstall | status | logs [-f]";

pub fn parse(rest: &[String]) -> Result<Action, ()> {
    let (sub, rest) = rest.split_first().ok_or(())?;
    match (sub.as_str(), rest) {
        ("install", rest) => {
            let print = rest.iter().any(|a| a == "--print");
            let flags: Vec<String> = rest.iter().filter(|a| *a != "--print").cloned().collect();
            // The flags are `serve`'s: checked here, not when the service first starts.
            let mut serve = vec!["serve".to_string()];
            serve.extend(flags.iter().cloned());
            match super::parse(&serve, Path::new("/")) {
                Ok(super::Command::Serve(_)) => Ok(Action::Install { flags, print }),
                _ => Err(()),
            }
        }
        ("uninstall" | "remove", []) => Ok(Action::Uninstall),
        ("status", []) => Ok(Action::Status),
        ("logs" | "log", []) => Ok(Action::Logs { follow: false }),
        ("logs" | "log", [f]) if f == "-f" || f == "--follow" => Ok(Action::Logs { follow: true }),
        _ => Err(()),
    }
}

pub fn run(action: Action) -> i32 {
    match action {
        Action::Install { flags, print } => match spec(&flags) {
            Ok(spec) if print => {
                print!("{}", file_text(&spec));
                0
            }
            Ok(spec) => install(&spec),
            Err(e) => fail(&e),
        },
        Action::Uninstall => uninstall(&service_name()),
        Action::Status => status(&service_name()),
        Action::Logs { follow } => logs(&service_name(), follow),
    }
}

/// `arakun-server`, or `arakun-server-<folder>` for a server in another folder,
/// so that a second server on the machine gets a service of its own.
pub fn service_name() -> String {
    name_for(
        std::env::var_os("ARAKUN_SERVER_DIR")
            .map(PathBuf::from)
            .as_deref(),
    )
}

fn name_for(server_dir: Option<&Path>) -> String {
    let base = if cfg!(target_os = "macos") {
        "dev.arakun.server"
    } else {
        "arakun-server"
    };
    let suffix: String = server_dir
        .filter(|p| p.is_absolute())
        .and_then(|p| p.file_name())
        .map(|n| {
            n.to_string_lossy()
                .chars()
                .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
                .collect()
        })
        .unwrap_or_default();
    let sep = if cfg!(target_os = "macos") { '.' } else { '-' };
    match suffix.trim_matches('-') {
        "" => base.to_string(),
        s => format!("{base}{sep}{s}"),
    }
}

/// The service for this binary, as it was started.
fn spec(flags: &[String]) -> Result<Spec, String> {
    let exe = std::env::current_exe().map_err(|e| format!("cannot find this program: {e}"))?;
    let exe = crate::platform::canonicalize(&exe).unwrap_or(exe);
    // The app takes `server serve`; `arakun-server` takes `serve`.
    let mut args: Vec<String> = name()
        .split_once(' ')
        .map(|(_, sub)| vec![sub.to_string()])
        .unwrap_or_default();
    args.push("serve".into());
    args.extend(flags.iter().cloned());
    let mut env = Vec::new();
    if let Some(path) = std::env::var_os("PATH") {
        // Shells that source their profile twice repeat entries.
        let mut seen = std::collections::HashSet::new();
        // A root service runs what it finds there as root at every boot: only
        // folders no one else can write to.
        let root = system_unit();
        // A relative folder would resolve against wherever an agent runs, and a
        // symlink's owner could repoint it: absolute, resolved paths only.
        let dirs = std::env::split_paths(&path)
            .filter_map(|d| match root {
                false => Some(d),
                true if d.is_absolute() && only_root_writes(&d) => {
                    crate::platform::canonicalize(&d).ok()
                }
                true => None,
            })
            .filter(|d| seen.insert(d.clone()));
        if let Ok(path) = std::env::join_paths(dirs) {
            env.push(("PATH".to_string(), path.to_string_lossy().into_owned()));
        }
    }
    // `dir()` ignores a relative one, and so does the service.
    if let Some(d) = std::env::var_os("ARAKUN_SERVER_DIR")
        && Path::new(&d).is_absolute()
    {
        env.push(("ARAKUN_SERVER_DIR".into(), d.to_string_lossy().into_owned()));
    }
    if system_unit() && !only_root_writes(&exe) {
        return Err(format!(
            "a system service runs {} as root at every boot, so it must be owned by root, \
             in folders only root can write to. Copy it there first:\n  \
             install -m755 {} /usr/local/bin/arakun-server\n\
             or install the service as a normal user, without sudo",
            exe.display(),
            exe.display()
        ));
    }
    Ok(Spec {
        name: service_name(),
        exe,
        args,
        env,
        log: dir().join("server.log"),
    })
}

/// Whether this installs a system unit: Linux, as root.
fn system_unit() -> bool {
    cfg!(target_os = "linux") && is_root()
}

/// Whether `path` and every folder above it belong to root and no group or
/// other user can write to them, so no one but root can change what runs.
fn only_root_writes(path: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;
        let Ok(path) = crate::platform::canonicalize(path) else {
            return false;
        };
        path.ancestors()
            .all(|p| std::fs::metadata(p).is_ok_and(|m| m.uid() == 0 && m.mode() & 0o022 == 0))
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        false
    }
}

/// A build in Cargo's `target/` folder changes or goes away with the next build.
fn in_build_folder(exe: &Path) -> bool {
    let parts: Vec<_> = exe.components().map(|c| c.as_os_str()).collect();
    parts
        .windows(2)
        .any(|w| w[0] == "target" && (w[1] == "debug" || w[1] == "release"))
}

/// Whether a server answers on this folder's socket.
fn answers() -> bool {
    local::send(
        &socket_path(),
        &Request::new("status", &[]),
        Duration::from_secs(2),
    )
    .is_ok_and(|r| r.ok)
}

fn install(spec: &Spec) -> i32 {
    let Some(file) = file_path(&spec.name) else {
        return fail(UNSUPPORTED);
    };
    let reinstall = file.exists();
    // A server started by hand holds the socket, and the service's would exit at once.
    if !reinstall && answers() {
        return fail(&format!(
            "a server is already running here; stop it with `{} quit`, then run this again",
            name()
        ));
    }
    if in_build_folder(&spec.exe) {
        eprintln!(
            "note: the service runs {}, which the next build replaces. Copy it somewhere \
             that stays first, for example ~/.local/bin, and install from there.",
            spec.exe.display()
        );
    }
    if let Err(e) = super::daemon::make_private_dir(&dir()) {
        return fail(&format!("{}: {e}", dir().display()));
    }
    if let Some(parent) = file.parent()
        && let Err(e) = std::fs::create_dir_all(parent)
    {
        return fail(&format!("{}: {e}", parent.display()));
    }
    // Before the unit starts: lingering is what starts the user's systemd when
    // nobody is logged in, and keeps it (and the server) running after logout.
    #[cfg(target_os = "linux")]
    if !is_root() {
        linger();
    }
    // Reinstalling stops the running one first, so it starts again with the new file.
    if reinstall {
        stop(&spec.name);
    }
    if let Err(e) = std::fs::write(&file, file_text(spec)) {
        return fail(&format!("{}: {e}", file.display()));
    }
    if let Err(e) = start(&spec.name, &file) {
        return fail(&e);
    }
    println!("Installed {}", file.display());
    let started = Instant::now();
    while !answers() {
        if started.elapsed() > START_WAIT {
            return fail(&format!(
                "the service did not start; see `{} service logs`",
                name()
            ));
        }
        std::thread::sleep(Duration::from_millis(250));
    }
    println!(
        "The server is running, and starts again {}.",
        if cfg!(target_os = "macos") {
            "when you log in"
        } else if is_root() || lingers() {
            "when the machine boots, whether or not anyone is logged in"
        } else {
            "when you log in"
        }
    );
    println!(
        "\nPair a device:   {n} pair\nLogs:            {n} service logs -f\nRemove it:       {n} service uninstall",
        n = name()
    );
    0
}

const UNSUPPORTED: &str = "a service needs systemd (Linux) or launchd (macOS); \
     on Windows, start `serve` from Task Scheduler at logon";

/// Whether this runs as root, where Linux gets a system unit, not a user one.
fn is_root() -> bool {
    #[cfg(unix)]
    {
        // SAFETY: getuid cannot fail.
        unsafe { libc::getuid() == 0 }
    }
    #[cfg(not(unix))]
    false
}

/// The unit or agent file for `name`; None where there is no service manager.
fn file_path(name: &str) -> Option<PathBuf> {
    if system_unit() {
        return Some(PathBuf::from(format!("/etc/systemd/system/{name}.service")));
    }
    let home = std::env::var_os("HOME").map(PathBuf::from)?;
    if cfg!(target_os = "macos") {
        Some(home.join(format!("Library/LaunchAgents/{name}.plist")))
    } else if cfg!(target_os = "linux") {
        let config = std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .filter(|p| p.is_absolute())
            .unwrap_or_else(|| home.join(".config"));
        Some(config.join(format!("systemd/user/{name}.service")))
    } else {
        None
    }
}

fn file_text(spec: &Spec) -> String {
    if cfg!(target_os = "macos") {
        plist(spec)
    } else {
        unit(spec, system_unit())
    }
}

/// The systemd unit: a user unit, or with `system` one for the whole machine.
fn unit(spec: &Spec, system: bool) -> String {
    let mut exec = vec![systemd_quote(&spec.exe.to_string_lossy())];
    exec.extend(spec.args.iter().map(|a| systemd_quote(a)));
    let env: String = spec
        .env
        .iter()
        .map(|(k, v)| format!("Environment={}\n", systemd_quote(&format!("{k}={v}"))))
        .collect();
    // A user's systemd cannot wait for the network; the server retries the relay anyway.
    let (after, wanted_by) = if system {
        (
            "After=network-online.target\nWants=network-online.target\n",
            "multi-user.target",
        )
    } else {
        ("", "default.target")
    };
    format!(
        "# Written by `{n} service install`; `{n} service uninstall` removes it.\n\
         [Unit]\n\
         Description=Arakun server\n\
         {after}\
         \n\
         [Service]\n\
         ExecStart={exec}\n\
         {env}\
         Restart=on-failure\n\
         RestartSec=5\n\
         \n\
         [Install]\n\
         WantedBy={wanted_by}\n",
        n = name(),
        exec = exec.join(" "),
    )
}

/// One word on an `ExecStart=` or `Environment=` line: quoted, with `%`
/// (systemd's specifiers), `\` and `"` escaped.
fn systemd_quote(word: &str) -> String {
    let escaped = word
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('%', "%%");
    format!("\"{escaped}\"")
}

/// The launchd agent.
fn plist(spec: &Spec) -> String {
    let string = |s: &str| format!("<string>{}</string>", xml_escape(s));
    let mut args = vec![string(&spec.exe.to_string_lossy())];
    args.extend(spec.args.iter().map(|a| string(a)));
    let env: String = spec
        .env
        .iter()
        .map(|(k, v)| format!("    <key>{}</key>{}\n", xml_escape(k), string(v)))
        .collect();
    let log = string(&spec.log.to_string_lossy());
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<!-- Written by `{n} service install`; `{n} service uninstall` removes it. -->
<plist version="1.0">
<dict>
  <key>Label</key>{label}
  <key>ProgramArguments</key>
  <array>
    {args}
  </array>
  <key>EnvironmentVariables</key>
  <dict>
{env}  </dict>
  <key>RunAtLoad</key><true/>
  <key>KeepAlive</key>
  <dict><key>SuccessfulExit</key><false/></dict>
  <key>ThrottleInterval</key><integer>5</integer>
  <key>StandardOutPath</key>{log}
  <key>StandardErrorPath</key>{log}
</dict>
</plist>
"#,
        n = xml_escape(name()),
        label = string(&spec.name),
        args = args.join("\n    "),
    )
}

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// Runs a service manager command; its error output when it fails.
fn manage(program: &str, args: &[&str]) -> Result<String, String> {
    let out = crate::platform::command(program)
        .args(args)
        .stdin(std::process::Stdio::null())
        .output()
        .map_err(|e| format!("{program}: {e}"))?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    } else {
        let err = String::from_utf8_lossy(&out.stderr);
        Err(format!("{program} {}: {}", args.join(" "), err.trim()))
    }
}

/// `systemctl`, for the user's systemd unless this is root.
#[cfg(not(target_os = "macos"))]
fn systemctl(args: &[&str]) -> Result<String, String> {
    if is_root() {
        return manage("systemctl", args);
    }
    #[cfg(target_os = "linux")]
    find_user_systemd();
    let mut all = vec!["--user"];
    all.extend_from_slice(args);
    manage("systemctl", &all).map_err(|e| {
        if e.contains("bus") || e.contains("XDG_RUNTIME_DIR") {
            format!(
                "{e}\nThis user has no systemd of its own running. Log in as it over SSH \
                 (not with su or sudo -u), or run `sudo loginctl enable-linger {user}` \
                 first, then run this again. As root, this installs a system service instead.",
                user = std::env::var("USER").unwrap_or_else(|_| "$USER".into())
            )
        } else {
            e
        }
    })
}

/// The launchd domains an agent can be in: the login session's, then the
/// user's, which is the one there is over SSH with nobody logged in.
#[cfg(target_os = "macos")]
fn domains() -> [String; 2] {
    // SAFETY: getuid cannot fail.
    let uid = unsafe { libc::getuid() };
    [format!("gui/{uid}"), format!("user/{uid}")]
}

fn start(name: &str, file: &Path) -> Result<(), String> {
    #[cfg(target_os = "macos")]
    {
        let file = file.to_string_lossy();
        // The login session's error says more than the fallback's.
        let mut first = None;
        for domain in domains() {
            match manage("launchctl", &["bootstrap", &domain, &file]) {
                Ok(_) => return Ok(()),
                Err(e) => {
                    first.get_or_insert(e);
                }
            }
        }
        let _ = name;
        Err(first.unwrap_or_default())
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = file;
        systemctl(&["daemon-reload"])?;
        systemctl(&["enable", "--now", name]).map(|_| ())
    }
}

/// Stops the service and takes it out of the service manager, if it is there.
fn stop(name: &str) {
    #[cfg(target_os = "macos")]
    for domain in domains() {
        let target = format!("{domain}/{name}");
        if manage("launchctl", &["bootout", &target]).is_err() {
            continue;
        }
        // bootout returns before the agent is gone, and a bootstrap until then
        // fails with "Input/output error".
        let asked = Instant::now();
        while manage("launchctl", &["print", &target]).is_ok() && asked.elapsed() < START_WAIT {
            std::thread::sleep(Duration::from_millis(100));
        }
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = systemctl(&["disable", "--now", name]);
    }
}

fn uninstall(name: &str) -> i32 {
    let Some(file) = file_path(name) else {
        return fail(UNSUPPORTED);
    };
    if !file.exists() {
        return fail(&format!(
            "no service is installed ({} is not there)",
            file.display()
        ));
    }
    stop(name);
    if let Err(e) = std::fs::remove_file(&file) {
        return fail(&format!("{}: {e}", file.display()));
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = systemctl(&["daemon-reload"]);
    }
    println!(
        "Removed {}. The server is stopped; its sessions and devices are kept in {}.",
        file.display(),
        dir().display()
    );
    0
}

fn status(name: &str) -> i32 {
    let Some(file) = file_path(name) else {
        return fail(UNSUPPORTED);
    };
    if !file.exists() {
        println!(
            "No service is installed. `{} service install` sets one up.",
            super::name()
        );
        return if answers() {
            println!("A server started by hand is running.");
            0
        } else {
            1
        };
    }
    println!("Service: {}", file.display());
    #[cfg(target_os = "macos")]
    {
        let loaded = domains()
            .into_iter()
            .find(|d| manage("launchctl", &["print", &format!("{d}/{name}")]).is_ok());
        match loaded {
            Some(d) => println!("launchd: loaded in {d}"),
            None => println!("launchd: not loaded"),
        }
    }
    #[cfg(not(target_os = "macos"))]
    {
        #[cfg(target_os = "linux")]
        if !is_root() {
            find_user_systemd();
        }
        // The state is stdout either way; an error with none means systemd was not reached.
        let state = |verb: &str| {
            let out = crate::platform::command("systemctl")
                .args(if is_root() { &[][..] } else { &["--user"][..] })
                .args([verb, name])
                .output();
            match out {
                Ok(o) if !o.stdout.is_empty() => {
                    String::from_utf8_lossy(&o.stdout).trim().to_string()
                }
                _ => "unknown".to_string(),
            }
        };
        println!("systemd: {}, {}", state("is-enabled"), state("is-active"));
        if !is_root() {
            println!(
                "lingering: {}",
                if lingers() {
                    "on (it runs while nobody is logged in, and starts at boot)"
                } else {
                    "off (it stops when your last login ends)"
                }
            );
        }
    }
    match super::ask(&Request::new("status", &[])) {
        Ok(message) => {
            println!("\n{message}");
            0
        }
        Err(e) => {
            println!("\nThe server does not answer: {e}");
            1
        }
    }
}

fn logs(name: &str, follow: bool) -> i32 {
    let mut cmd = if cfg!(target_os = "macos") {
        let mut cmd = crate::platform::command("tail");
        cmd.args(["-n", "200"]);
        if follow {
            cmd.arg("-F");
        }
        cmd.arg(dir().join("server.log"));
        cmd
    } else if cfg!(target_os = "linux") {
        let mut cmd = crate::platform::command("journalctl");
        if !is_root() {
            cmd.arg("--user");
        }
        cmd.args(["-u", name, "-n", "200", "--no-pager"]);
        if follow {
            cmd.arg("-f");
        }
        cmd
    } else {
        return fail(UNSUPPORTED);
    };
    match cmd.status() {
        Ok(s) => s.code().unwrap_or(1),
        Err(e) => fail(&e.to_string()),
    }
}

/// Whether systemd keeps this user's services running without a login.
fn lingers() -> bool {
    #[cfg(target_os = "linux")]
    {
        // The file systemd keeps for it; `loginctl show-user` needs a session to name the user.
        let user = std::env::var("USER").unwrap_or_default();
        !user.is_empty() && Path::new("/var/lib/systemd/linger").join(user).exists()
    }
    #[cfg(not(target_os = "linux"))]
    false
}

/// Turns lingering on: without it, systemd stops a user's services when their
/// last login ends, and starts them only when they log in again.
#[cfg(target_os = "linux")]
fn linger() {
    if !lingers() && manage("loginctl", &["enable-linger"]).is_err() {
        println!(
            "Could not turn on lingering, so the server stops when your last login ends. \
             To keep it running and start it at boot, run:\n  sudo loginctl enable-linger {}\n",
            std::env::var("USER").unwrap_or_else(|_| "\"$USER\"".into())
        );
    }
}

/// After `su -` or `sudo -u` there is no XDG_RUNTIME_DIR, and `systemctl --user`
/// cannot find the user's systemd, though it runs (lingering may have just started it).
#[cfg(target_os = "linux")]
fn find_user_systemd() {
    if std::env::var_os("XDG_RUNTIME_DIR").is_none() {
        // SAFETY: getuid cannot fail.
        let run = PathBuf::from(format!("/run/user/{}", unsafe { libc::getuid() }));
        let asked = Instant::now();
        while !run.join("systemd/private").exists() && asked.elapsed() < Duration::from_secs(5) {
            std::thread::sleep(Duration::from_millis(100));
        }
        if run.is_dir() {
            // SAFETY: single-threaded here: the CLI starts no threads before this.
            unsafe { std::env::set_var("XDG_RUNTIME_DIR", run) };
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(words: &[&str]) -> Vec<String> {
        words.iter().map(|w| w.to_string()).collect()
    }

    fn sample() -> Spec {
        Spec {
            name: "arakun-server".into(),
            exe: PathBuf::from("/home/me/my bin/arakun-server"),
            args: args(&["serve", "--reach", "lan"]),
            env: vec![("PATH".into(), "/usr/bin:/home/me/100%".into())],
            log: PathBuf::from("/home/me/.local/share/arakun/server/server.log"),
        }
    }

    #[test]
    fn install_takes_serves_flags_and_checks_them() {
        assert_eq!(
            parse(&args(&["install"])),
            Ok(Action::Install {
                flags: vec![],
                print: false
            })
        );
        assert_eq!(
            parse(&args(&["install", "--reach", "tailscale", "--print"])),
            Ok(Action::Install {
                flags: args(&["--reach", "tailscale"]),
                print: true
            })
        );
        assert!(parse(&args(&["install", "--reach", "wifi"])).is_err());
        assert!(parse(&args(&["install", "now"])).is_err());
        assert_eq!(
            parse(&args(&["logs", "-f"])),
            Ok(Action::Logs { follow: true })
        );
        assert_eq!(parse(&args(&["status"])), Ok(Action::Status));
        assert!(parse(&args(&["status", "now"])).is_err());
        assert!(parse(&[]).is_err());
    }

    #[test]
    fn the_unit_quotes_what_systemd_would_read_otherwise() {
        let text = unit(&sample(), false);
        assert!(text.contains(
            "ExecStart=\"/home/me/my bin/arakun-server\" \"serve\" \"--reach\" \"lan\"\n"
        ));
        // `%` is a specifier to systemd.
        assert!(text.contains("Environment=\"PATH=/usr/bin:/home/me/100%%\"\n"));
        assert!(text.contains("Restart=on-failure"));
        assert!(text.contains("WantedBy=default.target"));
        assert_eq!(systemd_quote(r#"a"b\c"#), r#""a\"b\\c""#);
        assert!(!text.contains("network-online"));
    }

    #[test]
    fn as_root_the_unit_is_the_machines_and_waits_for_the_network() {
        let text = unit(&sample(), true);
        assert!(text.contains("WantedBy=multi-user.target"));
        assert!(text.contains("After=network-online.target\nWants=network-online.target\n"));
    }

    #[test]
    fn the_agent_restarts_on_failure_and_escapes_its_strings() {
        let mut spec = sample();
        spec.args.push("<&>".into());
        let text = plist(&spec);
        assert!(text.contains("<string>/home/me/my bin/arakun-server</string>"));
        assert!(text.contains("<string>&lt;&amp;&gt;</string>"));
        assert!(text.contains("<key>PATH</key><string>/usr/bin:/home/me/100%</string>"));
        assert!(text.contains("<key>SuccessfulExit</key><false/>"));
        assert!(text.contains("server.log</string>"));
    }

    #[test]
    fn a_second_server_gets_a_service_of_its_own() {
        let base = name_for(None);
        assert!(base == "arakun-server" || base == "dev.arakun.server");
        assert_eq!(name_for(Some(Path::new("relative"))), base);
        let other = name_for(Some(Path::new("/srv/work box")));
        assert!(
            other.starts_with(&base) && other.ends_with("work-box"),
            "{other}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn only_folders_root_alone_can_change_pass() {
        assert!(only_root_writes(Path::new("/bin/sh")) || !Path::new("/bin/sh").exists());
        // Whoever runs the tests can write their temp folder; root cannot be the only one.
        let mine = std::env::temp_dir().join(format!("arakun-svc-{}", std::process::id()));
        std::fs::create_dir_all(&mine).unwrap();
        if !is_root() {
            assert!(!only_root_writes(&mine));
        }
        let _ = std::fs::remove_dir(&mine);
        assert!(!only_root_writes(Path::new("/no/such/path")));
    }

    #[test]
    fn builds_in_target_are_noticed() {
        assert!(in_build_folder(Path::new(
            "/src/arakun/target/release/arakun-server"
        )));
        assert!(!in_build_folder(Path::new(
            "/home/me/.local/bin/arakun-server"
        )));
        assert!(!in_build_folder(Path::new("/srv/target/arakun-server")));
    }
}
