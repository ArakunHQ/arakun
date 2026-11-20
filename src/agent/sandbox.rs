//! The sandbox an agent in an app session runs in, when its project asks for one
//! (Settings › Project › Sandbox agents): Seatbelt on macOS, bubblewrap on Linux.
//!
//! Unlike a headless run, the whole agent program is inside, so it keeps what it
//! needs to work: the network, its own login and settings, the toolchains and
//! their caches. It can write the workspace, the repository's Git folder (but not
//! its config or hooks), those caches and the temporary folders, and nothing else.
//! Credentials of other tools stay hidden, and so do the workspace's secret files.
//! It reaches arakun only through a socket that refuses commands which would run
//! something outside ([`crate::control::sandbox_allows`]).

use std::path::{Path, PathBuf};
use std::sync::RwLock;

use anyhow::{Result, anyhow};

use super::AgentKind;
use super::launch::LaunchSpec;
use crate::headless::sandbox::{
    self as hs, Hidden, READ, Runner, sbpl_list, sbpl_pin, sbpl_str, sbpl_subpath,
};

/// Projects whose agents run sandboxed, as the shell last saved them, each with
/// its repository's shared Git folder.
static PROJECTS: RwLock<Vec<(PathBuf, Option<PathBuf>)>> = RwLock::new(Vec::new());

pub fn set_projects(paths: impl IntoIterator<Item = PathBuf>) {
    let projects = paths
        .into_iter()
        .map(|p| {
            let p = crate::platform::canonicalize(&p).unwrap_or(p);
            let git = git_common_dir(&p);
            (p, git)
        })
        .collect();
    *PROJECTS.write().unwrap() = projects;
}

/// The session in `cwd` belongs to a project with the sandbox on: `cwd` is in
/// it, or in another worktree of its repository.
pub fn wanted(cwd: &Path) -> bool {
    let projects = PROJECTS.read().unwrap();
    if projects.is_empty() {
        return false;
    }
    let cwd = crate::platform::canonicalize(cwd).unwrap_or_else(|_| cwd.to_path_buf());
    if projects.iter().any(|(p, _)| cwd.starts_with(p)) {
        return true;
    }
    let Some(git) = git_common_dir(&cwd) else {
        return false;
    };
    projects.iter().any(|(_, g)| g.as_ref() == Some(&git))
}

/// The repository's shared Git folder: `.git` itself, or for a worktree the one
/// its `.git` file leads to.
fn git_common_dir(ws: &Path) -> Option<PathBuf> {
    let dot = ws.join(".git");
    if dot.is_dir() {
        return crate::platform::canonicalize(&dot).ok();
    }
    let text = std::fs::read_to_string(&dot).ok()?;
    let gitdir = ws.join(text.trim().strip_prefix("gitdir:")?.trim());
    let common = match std::fs::read_to_string(gitdir.join("commondir")) {
        Ok(c) => gitdir.join(c.trim()),
        Err(_) => gitdir,
    };
    crate::platform::canonicalize(common).ok()
}

/// Under `$HOME`, writable: package and build caches. Not the folders that hold
/// programs (`~/.cargo/bin`, toolchains): what the agent put there would run
/// outside the next time the user builds.
const CACHES: &[&str] = &[
    ".cargo/registry",
    ".cargo/git",
    ".npm/_cacache",
    ".npm/_logs",
    ".cache",
    ".bun/install/cache",
    "go/pkg/mod",
    ".gradle/caches",
    ".m2/repository",
    ".pnpm-store",
    ".local/share/pnpm/store",
    ".yarn/berry/cache",
    ".cache/uv",
    "Library/Caches",
    "Library/pnpm/store",
];

/// Under `$HOME`, the agent's own settings that run commands (hooks, MCP servers,
/// notify programs): read-only, so they can't run something outside later.
fn own_frozen(kind: AgentKind, home: &Path) -> Vec<PathBuf> {
    let rel: &[&str] = match kind {
        AgentKind::Claude => &[".claude/settings.json", ".claude/hooks"],
        AgentKind::Codex => &[".codex/config.toml"],
        AgentKind::OpenCode => &[".config/opencode", "opencode.json"],
        AgentKind::Droid => &[".factory/settings.json", ".factory/hooks"],
        AgentKind::Devin => &[".config/devin"],
        AgentKind::WorkersAi => &[],
    };
    rel.iter()
        .map(|r| home.join(r))
        .filter(|p| p.exists())
        .collect()
}

/// Under `$HOME`, the agent's own login, settings and history: shown to it and
/// writable, though the hide list has them for everyone else.
fn own(kind: AgentKind, home: &Path) -> Vec<PathBuf> {
    let rel: &[&str] = match kind {
        AgentKind::Claude => &[
            ".claude",
            ".claude.json",
            ".claude.json.backup",
            ".config/claude",
            ".local/share/claude",
            ".local/state/claude",
        ],
        AgentKind::Codex => &[".codex"],
        AgentKind::OpenCode => &[
            ".local/share/opencode",
            ".config/opencode",
            ".opencode",
            "opencode.json",
            ".local/state/opencode",
        ],
        AgentKind::Droid => &[".factory"],
        AgentKind::Devin => &[".devin", ".config/devin", ".local/share/devin"],
        AgentKind::WorkersAi => &[],
    };
    let mut out: Vec<PathBuf> = rel.iter().map(|r| home.join(r)).collect();
    if kind == AgentKind::Codex
        && let Some(h) = std::env::var_os("CODEX_HOME").map(PathBuf::from)
    {
        out.push(h);
    }
    if kind == AgentKind::Devin
        && let Some(dir) = super::launch::devin_credentials_path().parent()
    {
        out.push(dir.to_path_buf());
    }
    out
}

/// One agent's sandbox in an app session.
#[derive(Clone, Debug)]
pub struct Session {
    pub runner: Runner,
    pub home: PathBuf,
    pub workspace: PathBuf,
    /// Writable besides the workspace: the Git folder, caches, the agent's own.
    pub writable: Vec<PathBuf>,
    /// Shown again, read-only, after the hide list: the adapters and the shim.
    pub shown: Vec<PathBuf>,
    /// Shown again and writable after the hide list: the memory store, `tmp`.
    pub shown_rw: Vec<PathBuf>,
    pub hidden: Vec<Hidden>,
    /// Read-only even inside what is writable: the Git config and hooks.
    pub frozen: Vec<PathBuf>,
    /// The session's TMPDIR.
    pub tmp: PathBuf,
    /// An empty file, bound over hidden files (bubblewrap).
    pub empty: PathBuf,
    /// The control socket for sandboxed agents.
    pub socket: PathBuf,
    /// Read-only extras outside (a resolv.conf under /run).
    pub extra_ro: Vec<PathBuf>,
}

impl Session {
    pub fn new(kind: AgentKind, cwd: &Path) -> Result<Self> {
        let runner = hs::find_runner().ok_or_else(|| {
            anyhow!(if cfg!(target_os = "macos") {
                "sandbox-exec is not at /usr/bin/sandbox-exec. Turn off Sandbox agents in Settings › Project to run this agent without it."
            } else if cfg!(windows) {
                "Windows has no agent sandbox yet. Turn off Sandbox agents in Settings › Project to run this agent."
            } else {
                "bubblewrap (bwrap) is not installed. Install it (pacman -S bubblewrap / apt install bubblewrap), or turn off Sandbox agents in Settings › Project."
            })
        })?;
        let home = crate::platform::home();
        let real = |p: PathBuf| crate::platform::canonicalize(&p).unwrap_or(p);
        let workspace =
            crate::platform::canonicalize(cwd).map_err(|e| anyhow!("{}: {e}", cwd.display()))?;
        let mine = own(kind, &home);
        // The agent's own folders leave the hide list; a hidden one that holds one
        // of them stays hidden (the hide list goes first, then they come back).
        let mut hidden: Vec<Hidden> = hs::hidden(&home, &[])
            .into_iter()
            .filter(|h| !mine.iter().any(|m| h.path.starts_with(m)))
            .collect();
        hidden.extend(
            hs::scan_secrets(&workspace)
                .into_iter()
                .map(|path| Hidden { path, dir: false }),
        );
        let mut writable = vec![workspace.clone()];
        let git = git_common_dir(&workspace);
        let mut frozen = Vec::new();
        if let Some(g) = &git {
            if !g.starts_with(&workspace) {
                writable.push(g.clone());
            }
            frozen.extend(
                [g.join("config"), g.join("hooks")]
                    .into_iter()
                    .filter(|p| p.exists()),
            );
        }
        frozen.extend(own_frozen(kind, &home));
        for p in CACHES.iter().map(|r| home.join(r)).chain(mine) {
            if p.exists() {
                writable.push(real(p));
            }
        }
        let dir = crate::platform::app_dir().join("sandbox").join(format!(
            "{}-{}",
            kind.id(),
            std::process::id()
        ));
        let tmp = dir.join("tmp");
        std::fs::create_dir_all(&tmp).map_err(|e| anyhow!("{}: {e}", tmp.display()))?;
        let tmp = real(tmp);
        let empty = dir.join("empty");
        std::fs::write(&empty, b"").map_err(|e| anyhow!("{}: {e}", empty.display()))?;
        // Seatbelt matches real paths (/private/tmp, not /tmp); the socket may not
        // be there yet, its folder is.
        let socket = crate::control::sandbox_socket_path();
        if let Some(dir) = socket.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let socket = match (socket.parent(), socket.file_name()) {
            (Some(dir), Some(name)) => real(dir.to_path_buf()).join(name),
            _ => socket,
        };
        let mut shown = vec![super::launch::agents_dir()];
        if let Some(bin) = crate::control::shim_dir() {
            shown.push(bin);
        }
        // The shim leads to arakun itself, which may sit in a hidden folder.
        if let Ok(exe) = std::env::current_exe() {
            shown.push(exe);
        }
        let shown = shown.into_iter().filter(|p| p.exists()).map(real).collect();
        let memory = crate::memory::root();
        let _ = std::fs::create_dir_all(&memory);
        // Both sit in arakun's own folder, which the hide list has.
        let shown_rw = vec![real(memory), tmp.clone()];
        let mut extra_ro = Vec::new();
        if let Ok(r) = crate::platform::canonicalize("/etc/resolv.conf")
            && r.starts_with("/run")
        {
            extra_ro.push(r);
        }
        Ok(Session {
            runner,
            home,
            workspace,
            writable,
            shown,
            shown_rw,
            hidden,
            frozen,
            tmp,
            empty,
            socket,
            extra_ro,
        })
    }

    /// `spec`, run inside: the runner first, then the agent's own command.
    pub fn wrap(&self, spec: LaunchSpec) -> LaunchSpec {
        let mut inner = vec![spec.program.to_string_lossy().into_owned()];
        inner.extend(spec.args);
        let mut args = match &self.runner {
            Runner::Bwrap(_) => self.bwrap_args(),
            Runner::Seatbelt(_) => vec!["-p".into(), self.seatbelt_profile()],
        };
        args.extend(inner);
        let mut env = spec.env;
        env.push(("TMPDIR".into(), self.tmp.clone().into_os_string()));
        env.push(("ARAKUN_SOCKET".into(), self.socket.clone().into_os_string()));
        env.push(("ARAKUN_SANDBOX".into(), "1".into()));
        LaunchSpec {
            program: self.runner.program().to_path_buf(),
            args,
            env,
        }
    }

    fn bwrap_args(&self) -> Vec<String> {
        let s = |p: &Path| p.to_string_lossy().into_owned();
        let mut a: Vec<String> = [
            "--die-with-parent",
            "--new-session",
            "--unshare-ipc",
            "--unshare-uts",
            "--unshare-cgroup-try",
            "--ro-bind",
            "/",
            "/",
            "--dev",
            "/dev",
            "--proc",
            "/proc",
            // The host's buses and sockets (D-Bus, the keyring, Docker, arakun's own).
            "--tmpfs",
            "/run",
            "--bind",
            "/tmp",
            "/tmp",
        ]
        .map(String::from)
        .to_vec();
        if Path::new("/tmp/.X11-unix").is_dir() {
            a.extend(["--tmpfs".into(), "/tmp/.X11-unix".into()]);
        }
        if let Ok(rt) = std::env::var("XDG_RUNTIME_DIR")
            && Path::new(&rt).is_absolute()
            && !rt.starts_with("/run/")
            && Path::new(&rt).is_dir()
        {
            a.extend(["--tmpfs".into(), rt]);
        }
        for p in &self.writable {
            a.extend(["--bind".into(), s(p), s(p)]);
        }
        for h in &self.hidden {
            if h.dir {
                a.extend(["--tmpfs".into(), s(&h.path)]);
            } else {
                a.extend(["--ro-bind".into(), s(&self.empty), s(&h.path)]);
            }
        }
        for p in &self.shown {
            a.extend(["--ro-bind".into(), s(p), s(p)]);
        }
        for p in &self.shown_rw {
            a.extend(["--bind".into(), s(p), s(p)]);
        }
        for p in self.frozen.iter().chain(&self.extra_ro) {
            a.extend(["--ro-bind".into(), s(p), s(p)]);
        }
        if let Some(dir) = self.socket.parent() {
            let _ = std::fs::create_dir_all(dir);
            a.extend(["--bind".into(), s(dir), s(dir)]);
        }
        a.extend(["--chdir".into(), s(&self.workspace), "--".into()]);
        a
    }

    /// The Seatbelt profile. A later rule wins over an earlier one only when both
    /// name the same operations, so each allow repeats its deny's.
    pub fn seatbelt_profile(&self) -> String {
        let mut out = String::from("(version 1)\n(allow default)\n(deny appleevent-send)\n");
        // Logins stay in the keychain, so it stays; the clipboard and `open` don't.
        out.push_str(
            "(deny mach-lookup (global-name \"com.apple.pasteboard.1\") \
             (global-name \"com.apple.coreservices.launchservicesd\"))\n",
        );
        let mut writable = paths(&self.writable);
        writable.extend([
            self.tmp.as_path(),
            Path::new("/private/tmp"),
            Path::new("/private/var/folders"),
        ]);
        out.push_str(&format!(
            "(deny file-write*)\n(allow file-write* {} (literal \"/dev/null\") \
             (literal \"/dev/zero\") (literal \"/dev/dtracehelper\") (literal \"/dev/tty\") \
             (regex #\"^/dev/fd/\"))\n",
            sbpl_list(&writable)
        ));
        // Claude Code saves ~/.claude.json through a file beside it.
        if self.writable.iter().any(|p| p.ends_with(".claude")) {
            let prefix = self.home.join(".claude.json");
            out.push_str(&format!(
                "(allow file-write* (regex #\"^{}\"))\n",
                regex_escape(&prefix.to_string_lossy())
            ));
        }
        let hidden: Vec<&Path> = self.hidden.iter().map(|h| h.path.as_path()).collect();
        if !hidden.is_empty() {
            let list = sbpl_list(&hidden);
            out.push_str(&format!(
                "(deny {READ} {list})\n(deny file-write* {list})\n"
            ));
        }
        let mut shown = paths(&self.shown);
        shown.extend(paths(&self.shown_rw));
        if !shown.is_empty() {
            out.push_str(&format!("(allow {READ} {})\n", sbpl_list(&shown)));
        }
        if !self.shown_rw.is_empty() {
            out.push_str(&format!(
                "(allow file-write* {})\n",
                sbpl_list(&paths(&self.shown_rw))
            ));
        }
        if !self.frozen.is_empty() {
            out.push_str(&format!(
                "(deny file-write* {})\n",
                sbpl_list(&paths(&self.frozen))
            ));
        }
        // Folders above a hidden or read-only path can't be renamed out from under
        // its rule (`mv ~/.claude ~/.cache/c` to edit its settings).
        let roots: Vec<&Path> = writable
            .iter()
            .copied()
            .chain(paths(&self.shown_rw))
            .collect();
        let pinned: Vec<&Path> = hidden.iter().copied().chain(paths(&self.frozen)).collect();
        out.push_str(&sbpl_pin(&roots, &pinned));
        // Unix sockets: its own, and arakun's socket for sandboxed agents. Not the
        // host's (ssh-agent, Docker, tmux, arakun's main socket).
        let ws = sbpl_subpath(&self.workspace);
        let tmp = sbpl_subpath(&self.tmp);
        out.push_str(&format!(
            "(deny network-outbound (remote unix-socket))\n\
             (allow network-outbound (remote unix-socket {ws}) (remote unix-socket {tmp}) \
             (remote unix-socket (path-literal {})) \
             (remote unix-socket (path-literal \"/private/var/run/mDNSResponder\")))\n\
             (deny network-bind (local unix-socket))\n\
             (allow network-bind (local unix-socket {ws}) (local unix-socket {tmp}))\n",
            sbpl_str(&self.socket)
        ));
        out
    }

    /// The agent may read `path` (already resolved inside the workspace) through
    /// arakun's `fs/read_text_file`, as it could inside.
    pub fn may_read(&self, path: &Path) -> bool {
        !self.hidden.iter().any(|h| path.starts_with(&h.path))
            && !path
                .file_name()
                .is_some_and(|n| hs::secret_file(&n.to_string_lossy()))
    }

    /// The agent may write `path` through arakun's `fs/write_text_file`.
    pub fn may_write(&self, path: &Path) -> bool {
        self.may_read(path)
            && !self.frozen.iter().any(|f| path.starts_with(f))
            && !path.components().any(|c| c.as_os_str() == ".git")
    }
}

fn paths(ps: &[PathBuf]) -> Vec<&Path> {
    ps.iter().map(PathBuf::as_path).collect()
}

fn regex_escape(s: &str) -> String {
    let mut out = String::new();
    for c in s.chars() {
        if "\\.+*?()|[]{}^$\"".contains(c) {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn session(runner: Runner) -> Session {
        Session {
            runner,
            home: "/home/u".into(),
            workspace: "/home/u/ws/env-a".into(),
            writable: vec![
                "/home/u/ws/env-a".into(),
                "/home/u/proj/.git".into(),
                "/home/u/.cargo".into(),
                "/home/u/.claude".into(),
            ],
            shown: vec!["/home/u/.local/share/arakun/agents".into()],
            shown_rw: vec![
                "/home/u/.local/share/arakun/memory".into(),
                "/home/u/.local/share/arakun/sandbox/claude-1/tmp".into(),
            ],
            hidden: vec![
                Hidden {
                    path: "/home/u/.ssh".into(),
                    dir: true,
                },
                Hidden {
                    path: "/home/u/.local/share/arakun".into(),
                    dir: true,
                },
                Hidden {
                    path: "/home/u/.cargo/credentials.toml".into(),
                    dir: false,
                },
                Hidden {
                    path: "/home/u/ws/env-a/.env".into(),
                    dir: false,
                },
            ],
            frozen: vec![
                "/home/u/proj/.git/config".into(),
                "/home/u/proj/.git/hooks".into(),
            ],
            tmp: "/home/u/.local/share/arakun/sandbox/claude-1/tmp".into(),
            empty: "/home/u/.local/share/arakun/sandbox/claude-1/empty".into(),
            socket: "/run/user/1/arakun/sandbox/control.sock".into(),
            extra_ro: Vec::new(),
        }
    }

    #[test]
    fn seatbelt_hides_then_shows_then_freezes() {
        let p = session(Runner::Seatbelt("/usr/bin/sandbox-exec".into())).seatbelt_profile();
        let at = |needle: &str| p.find(needle).unwrap_or_else(|| panic!("{needle} in\n{p}"));
        let write = at("(allow file-write* (subpath \"/home/u/ws/env-a\")");
        let hide = at("(deny file-read-data file-read-xattr (subpath \"/home/u/.ssh\")");
        let show = at(
            "(allow file-read-data file-read-xattr (subpath \"/home/u/.local/share/arakun/agents\")",
        );
        let freeze = at("(deny file-write* (subpath \"/home/u/proj/.git/config\")");
        assert!(write < hide && hide < show && show < freeze);
        assert!(p.contains("(subpath \"/home/u/.cargo/credentials.toml\")"));
        assert!(p.contains("(path-literal \"/run/user/1/arakun/sandbox/control.sock\")"));
        assert!(p.contains("(regex #\"^/home/u/\\.claude\\.json\")"));
        // Their folders can't be renamed away from the rules, nor ttys written.
        assert!(p.contains("(literal \"/home/u/proj/.git\")"));
        assert!(p.contains("(literal \"/home/u/ws/env-a\")"));
        assert!(!p.contains("^/dev/tty"));
        // Logins in the keychain still work.
        assert!(!p.contains("SecurityServer"));
    }

    #[test]
    fn bwrap_binds_then_hides_then_freezes() {
        let a = session(Runner::Bwrap("/usr/bin/bwrap".into())).bwrap_args();
        let at = |x: &[&str]| {
            a.windows(x.len())
                .position(|w| w.iter().zip(x).all(|(a, b)| a == b))
                .unwrap_or_else(|| panic!("{x:?} in {a:?}"))
        };
        let cargo = at(&["--bind", "/home/u/.cargo", "/home/u/.cargo"]);
        let cred = at(&[
            "--ro-bind",
            "/home/u/.local/share/arakun/sandbox/claude-1/empty",
            "/home/u/.cargo/credentials.toml",
        ]);
        let hide = at(&["--tmpfs", "/home/u/.local/share/arakun"]);
        let mem = at(&["--bind", "/home/u/.local/share/arakun/memory"]);
        let hooks = at(&["--ro-bind", "/home/u/proj/.git/hooks"]);
        assert!(cargo < cred && hide < mem && mem < hooks);
        assert!(at(&["--bind", "/run/user/1/arakun/sandbox"]) > at(&["--tmpfs", "/run"]));
        assert_eq!(a.last().unwrap(), "--");
    }

    #[test]
    fn fs_requests_follow_the_sandbox() {
        let s = session(Runner::Bwrap("/usr/bin/bwrap".into()));
        assert!(s.may_read(Path::new("/home/u/ws/env-a/src/main.rs")));
        assert!(!s.may_read(Path::new("/home/u/ws/env-a/.env")));
        assert!(!s.may_read(Path::new("/home/u/ws/env-a/deploy/prod.pem")));
        assert!(s.may_write(Path::new("/home/u/ws/env-a/src/main.rs")));
        assert!(!s.may_write(Path::new("/home/u/ws/env-a/.git/hooks/pre-commit")));
    }

    #[test]
    fn a_worktree_belongs_to_its_project() {
        let d = std::env::temp_dir().join(format!("arakun-sbx-wt-{}", std::process::id()));
        let main = d.join("proj");
        let wt = d.join("env-a");
        std::fs::create_dir_all(main.join(".git/worktrees/env-a")).unwrap();
        std::fs::create_dir_all(main.join("src")).unwrap();
        std::fs::create_dir_all(&wt).unwrap();
        std::fs::write(
            wt.join(".git"),
            format!("gitdir: {}\n", main.join(".git/worktrees/env-a").display()),
        )
        .unwrap();
        std::fs::write(main.join(".git/worktrees/env-a/commondir"), "../..\n").unwrap();
        let main_real = crate::platform::canonicalize(&main).unwrap();
        assert_eq!(git_common_dir(&wt), Some(main_real.join(".git")));
        set_projects([main.clone()]);
        assert!(wanted(&wt) && wanted(&main.join("src")));
        // A project opened at a worktree covers the main checkout too.
        set_projects([wt.clone()]);
        assert!(wanted(&main));
        assert!(!wanted(&d));
        set_projects([]);
        assert!(!wanted(&wt));
        std::fs::remove_dir_all(&d).unwrap();
    }
}
