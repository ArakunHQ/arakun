//! `arakun server service …`: keeps the server running after the shell closes and
//! starts it again at login or boot. On Linux it is a systemd user unit with
//! lingering on, so it runs while nobody is logged in and starts at boot; as
//! root (a VPS often has only root) it is a system unit instead. macOS gets a
//! launchd agent. Each runs this same binary with `serve`, the `PATH` of the
//! shell that installed it (where the agents are), `ARAKUN_SERVER_DIR` if
//! it is set, and `ARAKUN_GITHUB=readonly` if that is.
//!
//! Both restart the server when it fails, but not after `quit`: `quit` exits
//! cleanly, and stopping the server is what the user asked for.

use std::ffi::OsString;
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
    let root = system_unit();
    let env = service_env(|key| std::env::var_os(key), root)?;
    text("this program's path", &exe)?;
    text("the server's folder", &dir())?;
    if let Some(file) = file_path(&service_name()) {
        text("the service file's path", &file)?;
    }
    if root && !only_root_writes(&exe) {
        return Err(format!(
            "a system service runs {} as root at every boot, so it must be owned by root, \
             in folders only root can write to. Copy it there first:\n  \
             install -m755 {} /usr/local/bin/arakun-server\n\
             or install the service as a normal user, without sudo",
            exe.display(),
            exe.display()
        ));
    }
    if root && let Some(found) = changed_by_others(&kept()) {
        return Err(format!(
            "a system service runs as root, and {} can be changed by other users; \
             unset XDG_DATA_HOME and ARAKUN_SERVER_DIR, or point them at a folder only root can write to",
            found.display()
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

/// The folders whose contents decide what a root server does: its own, the
/// data folder and the app's folder in it (the agents' adapters). Each, as
/// either variable can move one without the others.
fn kept() -> [PathBuf; 3] {
    [
        dir(),
        crate::platform::data_home(),
        crate::platform::app_dir(),
    ]
}

/// The first of `folders` that someone other than root could change: the
/// closest part of it that exists, as one that does not yet can only be made
/// by whoever may write to that part.
fn changed_by_others(folders: &[PathBuf]) -> Option<PathBuf> {
    folders
        .iter()
        .filter_map(|d| d.ancestors().find(|p| p.exists()))
        .find(|p| !only_root_writes(p))
        .map(Path::to_path_buf)
}

/// `path` with its symlinks resolved, as far as it exists; the rest is joined
/// on as it was named. None for a path with `..` in it.
fn resolve(path: &Path) -> Option<PathBuf> {
    if path
        .components()
        .any(|c| matches!(c, std::path::Component::ParentDir))
    {
        return None;
    }
    let found = path.ancestors().find(|p| p.exists())?;
    let rest = path.strip_prefix(found).ok()?;
    let resolved = crate::platform::canonicalize(found).ok()?;
    // Joining an empty rest would add a trailing slash.
    Some(match rest.as_os_str().is_empty() {
        true => resolved,
        false => resolved.join(rest),
    })
}

/// The variables the service gets from the installing shell (read with `get`):
/// the `PATH` its agents are found on, and what `dir()` depends on, so the
/// service uses the folder and socket the commands look for.
fn service_env(
    get: impl Fn(&str) -> Option<OsString>,
    root: bool,
) -> Result<Vec<(String, String)>, String> {
    let mut env = Vec::new();
    if let Some(path) = get("PATH") {
        // Shells that source their profile twice repeat entries.
        let mut seen = std::collections::HashSet::new();
        // A root service runs what it finds there as root at every boot: only
        // folders no one else can write to. A relative folder would resolve
        // against wherever an agent runs, and a symlink's owner could repoint
        // it: absolute, resolved paths only.
        let dirs = std::env::split_paths(&path)
            .filter_map(|d| match root {
                false => Some(d),
                // Resolved once, and that path checked: what is checked is what runs.
                true if d.is_absolute() => crate::platform::canonicalize(&d)
                    .ok()
                    .filter(|c| only_root_writes(c)),
                true => None,
            })
            // The unit file is text: a folder it cannot name exactly is left out,
            // not written as some other folder.
            .filter(|d| d.to_str().is_some())
            .filter(|d| seen.insert(d.clone()));
        if let Some(path) = std::env::join_paths(dirs)
            .ok()
            .and_then(|p| p.into_string().ok())
        {
            env.push(("PATH".to_string(), path));
        }
    }
    // `dir()` ignores a relative one of either, and so does the service.
    for key in ["XDG_DATA_HOME", "ARAKUN_SERVER_DIR"] {
        let Some(value) = get(key).filter(|v| Path::new(v).is_absolute()) else {
            continue;
        };
        let value = PathBuf::from(value);
        // A root service keeps the folder it checked, not a name whose
        // symlinks someone could repoint before the next start.
        let value = if root {
            resolve(&value)
                .filter(|r| changed_by_others(std::slice::from_ref(r)).is_none())
                .ok_or_else(|| {
                    format!(
                        "a system service runs as root, and {key}={} can be changed by other \
                         users; unset it, or point it at a folder only root can write to",
                        value.display()
                    )
                })?
        } else {
            value
        };
        env.push((key.to_string(), text(key, &value)?.to_string()));
    }
    // A server set up not to push or open pull requests stays that way once
    // it runs as a service. Only that one value means anything.
    if get("ARAKUN_GITHUB").is_some_and(|v| v == "readonly") {
        env.push(("ARAKUN_GITHUB".to_string(), "readonly".to_string()));
    }
    Ok(env)
}

/// `path` as the unit file must hold it: exactly, so not when it is not UTF-8.
/// Written with its bad bytes replaced, it would name another folder.
fn text<'a>(what: &str, path: &'a Path) -> Result<&'a str, String> {
    path.to_str().ok_or_else(|| {
        format!(
            "{what} ({}) is not valid UTF-8, and the service file could not name it exactly",
            path.display()
        )
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
    if reinstall && let Err(e) = stop(&spec.name) {
        return fail(&format!("could not stop the running service: {e}"));
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
        // Not XDG_CONFIG_HOME: the user's systemd reads its own environment, not
        // this shell's. `start` enables the file by its path, wherever it looks.
        Some(home.join(format!(".config/systemd/user/{name}.service")))
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
    // systemd expands `$` in the arguments, not in the program's own path.
    let mut exec = vec![systemd_quote(&spec.exe.to_string_lossy())];
    exec.extend(spec.args.iter().map(|a| exec_quote(a)));
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

/// [`systemd_quote`] for an `ExecStart=` word, where systemd also expands
/// `$VAR` and `${VAR}`, quoted or not.
fn exec_quote(word: &str) -> String {
    systemd_quote(word).replace('$', "$$")
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
        // By its path: a user's systemd with its own XDG_CONFIG_HOME does not look
        // in ~/.config, and enabling a file by path links it in where it does.
        let _ = name;
        systemctl(&["daemon-reload"])?;
        systemctl(&["enable", "--now", &file.to_string_lossy()]).map(|_| ())
    }
}

/// Stops the service and takes it out of the service manager, if it is there;
/// why not, when it may still be running.
fn stop(name: &str) -> Result<(), String> {
    #[cfg(target_os = "macos")]
    for domain in domains() {
        let target = format!("{domain}/{name}");
        // Only an answer of "no" is "not loaded": a launchctl that cannot be
        // started says nothing, and the plist must stay.
        let loaded = || {
            crate::platform::command("launchctl")
                .args(["print", &target])
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status()
                .map(|s| s.success())
                .map_err(|e| format!("launchctl: {e}"))
        };
        if !loaded()? {
            continue;
        }
        manage("launchctl", &["bootout", &target])?;
        // bootout returns before the agent is gone, and a bootstrap until then
        // fails with "Input/output error".
        let asked = Instant::now();
        while loaded()? {
            if asked.elapsed() > START_WAIT {
                return Err(format!("launchd still has {target} loaded"));
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    }
    #[cfg(not(target_os = "macos"))]
    systemctl(&["disable", "--now", name])?;
    Ok(())
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
    // The file stays while the server may run: it is how `uninstall` finds it again.
    if let Err(e) = stop(name) {
        return fail(&format!(
            "could not stop the service, so it is still installed: {e}"
        ));
    }
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
        // systemd expands `$` on ExecStart= even in quotes; not on Environment=.
        let mut spec = sample();
        spec.args.push("$HOME/${X}".into());
        spec.env.push(("A".into(), "$B".into()));
        spec.exe = PathBuf::from("/opt/$tools/arakun-server");
        let text = unit(&spec, false);
        assert!(text.contains(" \"$$HOME/$${X}\"\n"), "{text}");
        // The program's own path is not expanded, so it keeps its one `$`.
        assert!(
            text.contains("ExecStart=\"/opt/$tools/arakun-server\" "),
            "{text}"
        );
        assert!(text.contains("Environment=\"A=$B\"\n"), "{text}");
    }

    #[test]
    fn the_service_gets_the_path_and_what_its_folder_depends_on() {
        let shell = |key: &str| {
            let value = match key {
                "PATH" => "/usr/bin:/home/me/.local/bin:/usr/bin",
                "XDG_DATA_HOME" => "/home/me/data",
                "ARAKUN_SERVER_DIR" => "relative/dir",
                "HOME" => "/home/me",
                _ => return None,
            };
            Some(OsString::from(value))
        };
        assert_eq!(
            service_env(shell, false).unwrap(),
            vec![
                ("PATH".into(), "/usr/bin:/home/me/.local/bin".into()),
                ("XDG_DATA_HOME".into(), "/home/me/data".into()),
            ]
        );
        let other = |key: &str| (key == "ARAKUN_SERVER_DIR").then(|| OsString::from("/srv/a"));
        assert_eq!(
            service_env(other, false).unwrap(),
            vec![("ARAKUN_SERVER_DIR".into(), "/srv/a".into())]
        );
        for root in [false, true] {
            let read_only =
                |key: &str| (key == "ARAKUN_GITHUB").then(|| OsString::from("readonly"));
            assert_eq!(
                service_env(read_only, root).unwrap(),
                vec![("ARAKUN_GITHUB".into(), "readonly".into())]
            );
        }
        let other_value = |key: &str| (key == "ARAKUN_GITHUB").then(|| OsString::from("on"));
        assert!(service_env(other_value, false).unwrap().is_empty());
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

    #[cfg(unix)]
    #[test]
    fn a_root_service_keeps_only_folders_root_alone_can_change() {
        // The app's folder in the data folder counts, not only the data folder.
        assert!(kept().contains(&crate::platform::app_dir()));
        let mine = std::env::temp_dir().join(format!("arakun-kept-{}", std::process::id()));
        std::fs::create_dir_all(&mine).unwrap();
        if !is_root() {
            let found = changed_by_others(&[PathBuf::from("/"), mine.join("arakun")]);
            assert_eq!(found, Some(mine.clone()));
            let dir = mine.display().to_string();
            let shell = |key: &str| (key == "XDG_DATA_HOME").then(|| OsString::from(&dir));
            assert!(service_env(shell, true).is_err());
        }
        let _ = std::fs::remove_dir(&mine);
        assert_eq!(changed_by_others(&[PathBuf::from("/")]), None);
    }

    #[cfg(unix)]
    #[test]
    fn a_name_the_unit_cannot_hold_exactly_is_refused_not_mangled() {
        use std::os::unix::ffi::OsStringExt as _;
        let bad = || OsString::from_vec(b"/srv/server-\xff".to_vec());
        let dir = |key: &str| (key == "ARAKUN_SERVER_DIR").then(bad);
        let e = service_env(dir, false).unwrap_err();
        assert!(
            e.contains("ARAKUN_SERVER_DIR") && e.contains("UTF-8"),
            "{e}"
        );
        // On PATH such a folder is left out; the rest stays.
        let path = |key: &str| {
            (key == "PATH").then(|| OsString::from_vec(b"/usr/bin:/opt/\xff/bin:/bin".to_vec()))
        };
        assert_eq!(
            service_env(path, false).unwrap(),
            vec![("PATH".into(), "/usr/bin:/bin".into())]
        );
    }

    /// The root case from review: a UTF-8 symlink that resolves to a folder whose
    /// name is not. What a root unit would keep is the resolved path, and that is
    /// refused. (The ownership check needs a root-owned target, so this covers
    /// resolving and the UTF-8 check.)
    #[cfg(unix)]
    #[test]
    fn a_symlink_to_a_name_that_is_not_utf8_is_refused_once_resolved() {
        use std::os::unix::ffi::OsStrExt as _;
        let base = std::env::temp_dir().join(format!("arakun-utf8-{}", std::process::id()));
        std::fs::create_dir_all(&base).unwrap();
        let target = base.join(std::ffi::OsStr::from_bytes(b"server-\xff"));
        // macOS (APFS) refuses such names; Linux allows them, and CI runs there.
        if std::fs::create_dir(&target).is_ok() {
            let alias = base.join("alias");
            std::os::unix::fs::symlink(&target, &alias).unwrap();
            assert!(text("ARAKUN_SERVER_DIR", &alias).is_ok());
            let resolved = resolve(&alias.join("data")).unwrap();
            assert!(resolved.as_os_str().as_bytes().contains(&0xff));
            let e = text("ARAKUN_SERVER_DIR", &resolved).unwrap_err();
            assert!(e.contains("UTF-8"), "{e}");
            let _ = std::fs::remove_file(&alias);
            let _ = std::fs::remove_dir(&target);
        }
        let _ = std::fs::remove_dir(&base);
    }

    #[cfg(unix)]
    #[test]
    fn resolving_follows_what_exists_and_refuses_dot_dot() {
        let tmp = crate::platform::canonicalize(std::env::temp_dir()).unwrap();
        assert_eq!(
            resolve(&std::env::temp_dir().join("arakun-not-there/server")),
            Some(tmp.join("arakun-not-there/server"))
        );
        assert_eq!(resolve(Path::new("/srv/../etc")), None);
        // A folder that exists comes back as it is, without a trailing slash.
        assert_eq!(resolve(&std::env::temp_dir()), Some(tmp));
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
