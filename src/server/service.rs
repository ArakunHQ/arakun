//! `arakun server service …`: keeps the server running after the shell closes and
//! starts it again at login or boot. Linux gets a systemd user unit, and macOS
//! gets a launchd agent. Both run this same binary with `serve`, the `PATH` of
//! the shell that installed them (where the agents are) and `ARAKUN_SERVER_DIR`
//! if it is set.
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
        let dirs = std::env::split_paths(&path).filter(|d| seen.insert(d.clone()));
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
    Ok(Spec {
        name: service_name(),
        exe,
        args,
        env,
        log: dir().join("server.log"),
    })
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
        } else {
            "when the machine boots"
        }
    );
    #[cfg(target_os = "linux")]
    linger();
    println!(
        "\nPair a device:   {n} pair\nLogs:            {n} service logs -f\nRemove it:       {n} service uninstall",
        n = name()
    );
    0
}

const UNSUPPORTED: &str = "a service needs systemd (Linux) or launchd (macOS); \
     on Windows, start `serve` from Task Scheduler at logon";

/// The unit or agent file for `name`; None where there is no service manager.
fn file_path(name: &str) -> Option<PathBuf> {
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
        unit(spec)
    }
}

/// The systemd user unit.
fn unit(spec: &Spec) -> String {
    let mut exec = vec![systemd_quote(&spec.exe.to_string_lossy())];
    exec.extend(spec.args.iter().map(|a| systemd_quote(a)));
    let env: String = spec
        .env
        .iter()
        .map(|(k, v)| format!("Environment={}\n", systemd_quote(&format!("{k}={v}"))))
        .collect();
    format!(
        "# Written by `{n} service install`; `{n} service uninstall` removes it.\n\
         [Unit]\n\
         Description=Arakun server\n\
         \n\
         [Service]\n\
         ExecStart={exec}\n\
         {env}\
         Restart=on-failure\n\
         RestartSec=5\n\
         \n\
         [Install]\n\
         WantedBy=default.target\n",
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
        manage("systemctl", &["--user", "daemon-reload"])?;
        manage("systemctl", &["--user", "enable", "--now", name]).map(|_| ())
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
        let _ = manage("systemctl", &["--user", "disable", "--now", name]);
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
        let _ = manage("systemctl", &["--user", "daemon-reload"]);
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
        let state = |verb: &str| {
            manage("systemctl", &["--user", verb, name])
                .unwrap_or_else(|e| e.rsplit(": ").next().unwrap_or("").to_string())
                .trim()
                .to_string()
        };
        println!("systemd: {}, {}", state("is-enabled"), state("is-active"));
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
        cmd.args(["--user", "-u", name, "-n", "200", "--no-pager"]);
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

/// Without lingering, systemd stops a user's services when their last login
/// ends, and starts them only when they log in again.
#[cfg(target_os = "linux")]
fn linger() {
    let on = manage("loginctl", &["show-user", "--property=Linger", "--value"])
        .is_ok_and(|v| v.trim() == "yes");
    if on || manage("loginctl", &["enable-linger"]).is_ok() {
        return;
    }
    println!(
        "\nTo keep it running while you are logged out, and start it at boot, run:\n  \
         sudo loginctl enable-linger \"$USER\""
    );
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
        let text = unit(&sample());
        assert!(text.contains(
            "ExecStart=\"/home/me/my bin/arakun-server\" \"serve\" \"--reach\" \"lan\"\n"
        ));
        // `%` is a specifier to systemd.
        assert!(text.contains("Environment=\"PATH=/usr/bin:/home/me/100%%\"\n"));
        assert!(text.contains("Restart=on-failure"));
        assert!(text.contains("WantedBy=default.target"));
        assert_eq!(systemd_quote(r#"a"b\c"#), r#""a\"b\\c""#);
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
