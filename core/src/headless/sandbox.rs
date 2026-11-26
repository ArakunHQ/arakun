//! Where a headless agent's tools run: inside bubblewrap on Linux and Seatbelt
//! (`sandbox-exec`) on macOS, with the workspace writable, the rest of the machine
//! read-only, credentials and the host's sockets out of sight, and no network
//! unless a `--allow` grants it.
//!
//! Seatbelt cannot mount over a path, so where bubblewrap shows an empty `$HOME`
//! and its own `/tmp`, the Seatbelt profile refuses to read what is in them. It
//! starts from "allow" and denies what matters (files, sockets, the keychain,
//! launching apps); bubblewrap's namespaces cut more by default.
//!
//! The model's API calls and the [`broker`](super::broker)'s actions run in the
//! host process, outside; nothing inside can send, push or pay on its own.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, OnceLock, RwLock};
use std::time::Duration;

use serde::{Deserialize, Serialize};

/// What a run may get beyond the sandbox's defaults.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Cap {
    /// The network, still without credentials.
    Net,
    /// The network and `~/.modal.toml`, read-only.
    Modal,
    /// The `read_email` tool: the host reads mail for it.
    MailRead,
}

impl Cap {
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "net" => Some(Cap::Net),
            "modal" => Some(Cap::Modal),
            "mail-read" => Some(Cap::MailRead),
            _ => None,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Cap::Net => "net",
            Cap::Modal => "modal",
            Cap::MailRead => "mail-read",
        }
    }
}

/// `net,modal` as caps; the error names the one it doesn't know.
pub fn parse_caps(s: &str) -> Result<Vec<Cap>, String> {
    let mut caps = Vec::new();
    for c in s.split(',').map(str::trim).filter(|c| !c.is_empty()) {
        let cap =
            Cap::parse(c).ok_or_else(|| format!("unknown --allow {c}: net, modal or mail-read"))?;
        if !caps.contains(&cap) {
            caps.push(cap);
        }
    }
    Ok(caps)
}

pub fn caps_names(caps: &[Cap]) -> String {
    if caps.is_empty() {
        "none".into()
    } else {
        caps.iter().map(|c| c.name()).collect::<Vec<_>>().join(",")
    }
}

/// Paths under `$HOME` the sandbox never sees: keys, tokens, mail, browser
/// profiles, shell histories, other agents' logins and arakun's own data.
pub const HIDE: &[&str] = &[
    ".ssh",
    ".gnupg",
    ".password-store",
    ".local/share/keyrings",
    ".local/share/kwalletd",
    ".pki",
    ".config/himalaya",
    ".hermes",
    ".config/evolution",
    ".local/share/evolution",
    ".config/goa-1.0",
    ".gmail-mcp",
    ".config/gh",
    ".config/hub",
    ".git-credentials",
    ".config/git/credentials",
    ".netrc",
    ".authinfo",
    ".authinfo.gpg",
    ".config/cloudflare",
    ".wrangler",
    ".config/.wrangler",
    ".cloudflared",
    ".local/share/opencode",
    ".config/opencode",
    ".opencode",
    "opencode.json",
    ".modal.toml",
    #[cfg(feature = "coderlabs")]
    ".okc",
    #[cfg(feature = "coderlabs")]
    ".config/okc",
    #[cfg(feature = "coderlabs")]
    ".okc_budget.json",
    ".runpod",
    ".docker",
    ".aws",
    ".azure",
    ".config/gcloud",
    ".kube",
    ".fly",
    ".config/stripe",
    ".local/share/com.vercel.cli",
    ".supabase",
    ".config/tailscale",
    ".config/rclone",
    ".config/posthog",
    ".config/openrouter",
    ".config/solana",
    ".local/share/solana",
    ".cargo/credentials",
    ".cargo/credentials.toml",
    ".npmrc",
    ".pypirc",
    ".gem/credentials",
    ".cache/huggingface",
    ".huggingface",
    ".xurl",
    ".x-session.json",
    ".claude",
    ".claude.json",
    ".claude.json.backup",
    ".mcp.json",
    ".mcp-auth",
    ".config/Claude",
    ".config/Claude-3p",
    ".local/share/claude",
    ".local/share/claude-desktop",
    ".codex",
    ".gemini",
    ".cursor",
    ".config/Cursor",
    ".config/Code",
    ".config/discord",
    ".config/Discord",
    ".local/share/arakun",
    ".local/state/arakun",
    ".mozilla",
    ".config/BraveSoftware",
    ".config/chromium",
    ".config/google-chrome",
    ".config/cef_user_data",
    ".agent-browser",
    ".zsh_history",
    ".bash_history",
    ".zsh-history.bak",
    ".local/share/fish/fish_history",
    ".local/share/atuin",
    ".python_history",
    ".aider.chat.history.md",
    ".aider.input.history",
    ".env",
    ".pulse-cookie",
];

/// macOS places under `$HOME` the sandbox never sees: browsers, mail and messages.
pub const MAC_HIDE: &[&str] = &[
    "Library/Application Support/Google/Chrome",
    "Library/Application Support/BraveSoftware",
    "Library/Application Support/Firefox",
    "Library/Application Support/Arc",
    "Library/Application Support/Microsoft Edge",
    "Library/Application Support/Chromium",
    "Library/Application Support/Claude",
    "Library/Application Support/Code",
    "Library/Application Support/Cursor",
    "Library/Application Support/discord",
    "Library/Application Support/1Password",
    "Library/Cookies",
    "Library/Mail",
    "Library/Messages",
    "Library/Safari",
    "Library/Group Containers/group.com.apple.notes",
];

/// What a sandbox sees of `$HOME`, read-only: toolchains and their caches.
/// The rest of `$HOME` is an empty tmpfs.
pub const TOOLCHAINS: &[&str] = &[
    ".cargo/bin",
    ".cargo/registry",
    ".cargo/git",
    ".cargo/config.toml",
    ".rustup",
    ".local/bin",
    ".local/lib",
    ".local/share/uv/python",
    ".local/share/uv/tools",
    ".local/share/pipx/venvs",
    ".npm-global/bin",
    ".npm-global/lib",
    ".bun/bin",
    "go/bin",
    ".nix-profile",
    ".local/state/nix",
    ".gitconfig",
    ".config/git",
];

use crate::secrets::secret_file;

/// Folders a secrets scan skips: packages and build output, never the user's.
const SKIP: &[&str] = &[
    ".git",
    "node_modules",
    "target",
    ".venv",
    "venv",
    "site-packages",
    "dist-packages",
    "__pycache__",
    ".cache",
    ".mypy_cache",
    ".pytest_cache",
    ".next",
];

/// Files under `root` that look like secrets, a few levels down.
pub fn scan_secrets(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut budget = 50_000usize;
    let mut stack = vec![(root.to_path_buf(), 0usize)];
    if std::fs::metadata(root).is_ok_and(|m| m.is_file()) {
        return if root
            .file_name()
            .is_some_and(|n| secret_file(&n.to_string_lossy()))
        {
            vec![root.to_path_buf()]
        } else {
            out
        };
    }
    while let Some((dir, depth)) = stack.pop() {
        for e in std::fs::read_dir(&dir).into_iter().flatten().flatten() {
            if budget == 0 {
                return out;
            }
            budget -= 1;
            let name = e.file_name().to_string_lossy().into_owned();
            let Ok(t) = e.file_type() else { continue };
            let path = e.path();
            if t.is_dir() {
                if depth < 8 && !SKIP.contains(&name.as_str()) {
                    stack.push((path, depth + 1));
                }
            } else if path
                .strip_prefix(root)
                .is_ok_and(crate::secrets::secret_path)
            {
                // By its folder too: everything under `.ssh/` or `.aws/`.
                out.push(path);
            }
        }
    }
    out.sort();
    out
}

/// A `--read` path: there, and holding nothing the hide list or a secrets
/// scan would keep out. Refused, not trimmed, so what the agent sees is
/// what the user named.
pub fn check_read(raw: &Path, hidden: &[Hidden]) -> Result<PathBuf, String> {
    let p =
        crate::platform::canonicalize(raw).map_err(|e| format!("--read {}: {e}", raw.display()))?;
    if p == Path::new("/") || p == crate::platform::home() {
        return Err(format!(
            "--read {} is too wide; name the folder the agent needs",
            p.display()
        ));
    }
    if let Some(h) = hidden
        .iter()
        .find(|h| p.starts_with(&h.path) || h.path.starts_with(&p))
    {
        return Err(format!(
            "--read {}: it is or holds {}, which stays hidden",
            p.display(),
            h.path.display()
        ));
    }
    let found = scan_secrets(&p);
    if !found.is_empty() {
        let list: Vec<String> = found
            .iter()
            .take(5)
            .map(|f| f.display().to_string())
            .collect();
        return Err(format!(
            "--read {} holds what looks like secrets: {}{}",
            p.display(),
            list.join(", "),
            if found.len() > 5 { ", …" } else { "" }
        ));
    }
    Ok(p)
}

/// Top-level names in `$HOME` and `~/.config` hidden for what they look like.
fn secret_name(name: &str) -> bool {
    let n = name.to_lowercase();
    [
        "key",
        "secret",
        "password",
        "passwd",
        "token",
        "wallet",
        "credential",
        "cookie",
    ]
    .iter()
    .any(|w| n.contains(w))
        || n.starts_with(".env")
        || n.ends_with(".env")
        || n.ends_with(".pem")
        || crate::secrets::secret_file(name)
}

/// Words in the name of a variable that holds a secret. `AUTH` is not one:
/// `SSH_AUTH_SOCK` and `XAUTHORITY` only point at one (see [`secret_var`]).
const SECRET_WORDS: &[&str] = &[
    "TOKEN",
    "SECRET",
    "PASSWORD",
    "PASSWD",
    "CREDENTIAL",
    "COOKIE",
    "API_KEY",
    "APIKEY",
    "PRIVATE",
];

/// Variables that point at the host's agents, buses and display.
const ADDRESS_PREFIXES: &[&str] = &[
    "SSH_",
    "GPG_",
    "GNUPG",
    "DBUS_",
    "WAYLAND_",
    "HYPRLAND_",
    "KITTY_",
    "PULSE_",
    "PIPEWIRE_",
];
const ADDRESS_EXACT: &[&str] = &[
    "XDG_RUNTIME_DIR",
    "DISPLAY",
    "XAUTHORITY",
    "TMUX",
    "TMUX_PANE",
    "ZELLIJ",
];

/// Its name says it holds a secret value: `GH_TOKEN`, `DB_PASS`, `SIGNING_KEY`.
fn names_secret_value(n: &str) -> bool {
    SECRET_WORDS.iter().any(|w| n.contains(w))
        || n.ends_with("KEY")
        || n.ends_with("_KEYS")
        // `DB_PASS`, `SMTP_PWD`: the names a value is a secret under.
        || crate::secrets::names_secret(n)
}

/// The address of a host socket, bus or display, and no secret itself:
/// `SSH_AUTH_SOCK`, `DBUS_SESSION_BUS_ADDRESS`, `DISPLAY`. The sandbox drops
/// these with the sockets it hides; an agent outside it keeps them, since it
/// can reach those sockets anyway.
pub fn host_address(name: &str) -> bool {
    let n = name.to_uppercase();
    (n.contains("SOCK")
        || ADDRESS_PREFIXES.iter().any(|p| n.starts_with(p))
        || ADDRESS_EXACT.contains(&n.as_str()))
        && !names_secret_value(&n)
}

/// Environment variables the sandbox never gets: tokens, keys, and the
/// addresses of the host's agents, buses and display.
pub fn secret_var(name: &str, caps: &[Cap]) -> bool {
    let n = name.to_uppercase();
    if n.starts_with("MODAL_") && caps.contains(&Cap::Modal) {
        return false;
    }
    const PREFIXES: &[&str] = &[
        "CLOUDFLARE_",
        "CF_",
        "MODAL_",
        "GH_",
        "GITHUB_",
        "AWS_",
        "AZURE_",
        "GOOGLE_",
        "GEMINI_",
        "OPENAI_",
        "ANTHROPIC_",
        "OPENCODE_",
        "OPENROUTER_",
        "ARAKUN_",
        "HF_",
        "HUGGING",
        "RUNPOD_",
        #[cfg(feature = "coderlabs")]
        "OKC_",
        "STRIPE_",
        "SUPABASE_",
        "VERCEL_",
        "NPM_CONFIG_",
        "DOCKER_",
        "VAULT_",
        "KRB5",
        "BW_",
        "OP_",
        "TAILSCALE",
    ];
    const EXACT: &[&str] = &[
        "NETRC",
        "KUBECONFIG",
        "HIMALAYA_CONFIG",
        "GIT_ASKPASS",
        "SUDO_ASKPASS",
        "XDG_SESSION_COOKIE",
    ];
    names_secret_value(&n)
        || n.contains("AUTH")
        || n.contains("SOCK")
        || PREFIXES.iter().any(|p| n.starts_with(p))
        || ADDRESS_PREFIXES.iter().any(|p| n.starts_with(p))
        || EXACT.contains(&n.as_str())
        || ADDRESS_EXACT.contains(&n.as_str())
}

/// The environment a sandboxed command starts with.
pub fn scrub_env(
    vars: impl IntoIterator<Item = (String, String)>,
    caps: &[Cap],
    path: &str,
) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = vars
        .into_iter()
        .filter(|(k, _)| !secret_var(k, caps) && !matches!(k.as_str(), "PATH" | "TMPDIR"))
        .collect();
    out.push(("PATH".into(), path.into()));
    out.push(("TMPDIR".into(), "/tmp".into()));
    out.sort();
    out
}

/// A path hidden from the sandbox, folders under an empty tmpfs.
#[derive(Clone, Debug, PartialEq)]
pub struct Hidden {
    pub path: PathBuf,
    pub dir: bool,
}

/// Something bound back into the empty `$HOME`; a link stays a link.
#[derive(Clone, Debug, PartialEq)]
pub enum Bound {
    Path(PathBuf),
    Link(PathBuf, PathBuf),
}

/// The program that confines a command.
#[derive(Clone, Debug, PartialEq)]
pub enum Runner {
    /// Linux: `bwrap`, with namespaces and bind mounts.
    Bwrap(PathBuf),
    /// macOS: `sandbox-exec` with a Seatbelt profile.
    Seatbelt(PathBuf),
}

impl Runner {
    pub fn program(&self) -> &Path {
        match self {
            Runner::Bwrap(p) | Runner::Seatbelt(p) => p,
        }
    }

    fn name(&self) -> &'static str {
        match self {
            Runner::Bwrap(_) => "bubblewrap",
            Runner::Seatbelt(_) => "sandbox-exec",
        }
    }
}

/// One session's sandbox, as `bwrap` arguments or a Seatbelt profile.
#[derive(Clone, Debug)]
pub struct Sandbox {
    pub runner: Runner,
    pub home: PathBuf,
    /// Bound back read-only over the empty `$HOME`: [`TOOLCHAINS`] that exist.
    pub toolchains: Vec<Bound>,
    /// `--read` paths, read-only.
    pub reads: Vec<PathBuf>,
    pub workspace: PathBuf,
    /// The session's own `/tmp`, kept between calls.
    pub tmp: PathBuf,
    /// An empty file, bound over hidden files.
    pub empty: PathBuf,
    pub caps: Vec<Cap>,
    pub hidden: Vec<Hidden>,
    /// Bound read-only over the writable workspace: the Git metadata ([`git_freeze`]).
    pub frozen: Vec<PathBuf>,
    /// Bound over itself, writable: `.git`, so it can't be renamed and replaced.
    pub thawed: Vec<PathBuf>,
    /// Read-only extras the caps need (a resolv.conf under /run).
    pub extra_ro: Vec<PathBuf>,
}

fn s(p: &Path) -> String {
    p.to_string_lossy().into_owned()
}

impl Sandbox {
    pub fn net(&self) -> bool {
        self.caps.iter().any(|c| matches!(c, Cap::Net | Cap::Modal))
    }

    /// Where the sandbox's own temporary folder is, seen from inside.
    pub fn shown_tmp(&self) -> PathBuf {
        match self.runner {
            Runner::Bwrap(_) => PathBuf::from("/tmp"),
            Runner::Seatbelt(_) => self.tmp.clone(),
        }
    }

    /// `cwd` when it is inside the workspace or the sandbox's `/tmp`, else the workspace.
    fn inner_cwd<'a>(&'a self, cwd: &'a Path) -> &'a Path {
        if cwd.starts_with(&self.workspace) || cwd.starts_with(self.shown_tmp()) {
            cwd
        } else {
            &self.workspace
        }
    }

    /// The runner's arguments around `inner`, run in `cwd` (the workspace when
    /// outside it). Seatbelt cannot change folder; [`Self::command`] does.
    pub fn argv(&self, cwd: &Path, inner: &[String]) -> Vec<String> {
        match self.runner {
            Runner::Bwrap(_) => self.bwrap_argv(cwd, inner),
            Runner::Seatbelt(_) => {
                let mut a = vec!["-p".to_string(), self.seatbelt_profile()];
                a.extend(inner.iter().cloned());
                a
            }
        }
    }

    fn bwrap_argv(&self, cwd: &Path, inner: &[String]) -> Vec<String> {
        let mut a: Vec<String> = [
            "--die-with-parent",
            "--new-session",
            "--unshare-pid",
            "--unshare-ipc",
            "--unshare-uts",
            "--unshare-cgroup-try",
        ]
        .map(String::from)
        .to_vec();
        if !self.net() {
            a.push("--unshare-net".into());
        }
        let mut push = |xs: &[&str]| a.extend(xs.iter().map(|x| x.to_string()));
        push(&[
            "--ro-bind",
            "/",
            "/",
            "--dev",
            "/dev",
            "--proc",
            "/proc",
            "--tmpfs",
            "/run",
            "--tmpfs",
            "/var/tmp",
        ]);
        let tmp = s(&self.tmp);
        push(&["--bind", &tmp, "/tmp"]);
        if let Ok(rt) = std::env::var("XDG_RUNTIME_DIR")
            && Path::new(&rt).is_absolute()
            && !rt.starts_with("/run/")
            && !rt.starts_with("/tmp")
            && Path::new(&rt).is_dir()
        {
            push(&["--tmpfs", &rt]);
        }
        let ws = s(&self.workspace);
        // $HOME is empty but for what is bound back.
        if self.home != Path::new("/") {
            a.extend(["--tmpfs".to_string(), s(&self.home)]);
        }
        if Path::new("/root").is_dir() && self.home != Path::new("/root") {
            a.extend(["--tmpfs".to_string(), "/root".to_string()]);
        }
        let hide = |a: &mut Vec<String>, h: &Hidden| {
            if h.dir {
                a.extend(["--tmpfs".to_string(), s(&h.path)]);
            } else {
                a.extend(["--ro-bind".to_string(), s(&self.empty), s(&h.path)]);
            }
        };
        // Hidden places outside $HOME (arakun's data under another XDG_DATA_HOME…);
        // /tmp and /run are the sandbox's own already.
        for h in self.hidden.iter().filter(|h| {
            !h.path.starts_with(&self.home)
                && !["/tmp", "/run", "/var/tmp"]
                    .iter()
                    .any(|t| h.path.starts_with(t))
        }) {
            hide(&mut a, h);
        }
        let mut shown: Vec<&Path> = Vec::new();
        for b in &self.toolchains {
            match b {
                Bound::Path(p) => {
                    a.extend(["--ro-bind".to_string(), s(p), s(p)]);
                    shown.push(p);
                }
                Bound::Link(p, to) => a.extend(["--symlink".to_string(), s(to), s(p)]),
            }
        }
        for p in &self.reads {
            a.extend(["--ro-bind".to_string(), s(p), s(p)]);
            shown.push(p);
        }
        a.extend(["--bind".to_string(), ws.clone(), ws.clone()]);
        shown.push(&self.workspace);
        // The hide list again, over whatever was bound back.
        for h in self
            .hidden
            .iter()
            .filter(|h| shown.iter().any(|p| h.path.starts_with(p) && h.path != *p))
        {
            hide(&mut a, h);
        }
        a.extend(bwrap_freeze(&self.frozen, &self.thawed));
        for f in &self.extra_ro {
            a.extend(["--ro-bind".to_string(), s(f), s(f)]);
        }
        a.extend([
            "--chdir".to_string(),
            s(self.inner_cwd(cwd)),
            "--".to_string(),
        ]);
        a.extend(inner.iter().cloned());
        a
    }

    /// The Seatbelt profile: the same rules as [`Self::bwrap_argv`], as far as
    /// Seatbelt can say them. A later rule wins over an earlier one only when both
    /// name the same operations, so each allow repeats its deny's.
    pub fn seatbelt_profile(&self) -> String {
        let q = sbpl_subpath;
        let list = |ps: &[&Path]| sbpl_list(ps);
        let mut out = String::from("(version 1)\n(allow default)\n");
        // Nothing launched outside the sandbox: no apps, no Apple events, no keychain
        // or clipboard.
        out.push_str("(deny appleevent-send)\n");
        out.push_str("(deny mach-lookup");
        for name in SEATBELT_SERVICES {
            out.push_str(&format!(" (global-name \"{name}\")"));
        }
        out.push_str(")\n");
        let tmp = q(&self.tmp);
        let ws = q(&self.workspace);
        out.push_str("(deny file-write*)\n");
        out.push_str(&format!(
            "(allow file-write* {ws} {tmp} (literal \"/dev/null\") (literal \"/dev/zero\") \
             (literal \"/dev/dtracehelper\") (literal \"/dev/tty\") (regex #\"^/dev/fd/\"))\n"
        ));
        // Also when they are not there yet: a deny names a path, not a file, so
        // `git init` can't plant a hook or an fsmonitor the user's git runs
        // outside, nor a `commondir` that leads it to config elsewhere.
        let git = self.workspace.join(".git");
        let git_own = [git.join("config"), git.join("hooks"), git.join("commondir")];
        let mut frozen: Vec<&Path> = self.frozen.iter().map(PathBuf::as_path).collect();
        frozen.extend(git_own.iter().map(PathBuf::as_path));
        frozen.sort();
        frozen.dedup();
        let thawed: Vec<&Path> = self.thawed.iter().map(PathBuf::as_path).collect();
        out.push_str(&sbpl_freeze(&frozen, &thawed));
        // $HOME and the shared temporary folders read as empty, but for what is
        // shown again; names and sizes stay visible, as Seatbelt has no tmpfs.
        let mut dark = vec![self.home.as_path()];
        if Path::new("/root").is_dir() && self.home != Path::new("/root") {
            dark.push(Path::new("/root"));
        }
        dark.extend([
            Path::new("/private/tmp"),
            Path::new("/private/var/tmp"),
            Path::new("/private/var/folders"),
        ]);
        out.push_str(&format!("(deny {READ} {})\n", list(&dark)));
        let mut shown: Vec<PathBuf> = Vec::new();
        for b in &self.toolchains {
            match b {
                Bound::Path(p) => shown.push(p.clone()),
                // A link is read where it leads.
                Bound::Link(p, _) => shown.extend(crate::platform::canonicalize(p).ok()),
            }
        }
        shown.extend(self.reads.iter().cloned());
        shown.push(self.workspace.clone());
        let shown: Vec<&Path> = shown.iter().map(PathBuf::as_path).collect();
        out.push_str(&format!("(allow {READ} {})\n", list(&shown)));
        let hidden: Vec<&Path> = self.hidden.iter().map(|h| h.path.as_path()).collect();
        if !hidden.is_empty() {
            out.push_str(&format!("(deny {READ} {})\n", list(&hidden)));
            out.push_str(&format!("(deny file-write* {})\n", list(&hidden)));
        }
        out.push_str(&sbpl_secret_names(&self.workspace));
        let pinned: Vec<&Path> = frozen.iter().chain(&hidden).copied().collect();
        out.push_str(&sbpl_pin(&[self.workspace.as_path()], &pinned));
        // The sandbox's own /tmp, last: it may sit under a hidden folder.
        out.push_str(&format!("(allow {READ} {tmp})\n"));
        out.push_str(&format!("(allow file-write* {tmp})\n"));
        // The host's sockets (ssh-agent, Docker, arakun's own) are out of reach;
        // the sandbox's own are not.
        let uq = |p: &Path| format!("(remote unix-socket {})", q(p));
        out.push_str("(deny network-outbound (remote unix-socket))\n");
        out.push_str(&format!(
            "(allow network-outbound {} {})\n",
            uq(&self.workspace),
            uq(&self.tmp)
        ));
        out.push_str("(deny network-bind (local unix-socket))\n");
        out.push_str(&format!(
            "(allow network-bind (local unix-socket {ws}) (local unix-socket {tmp}))\n"
        ));
        if self.net() {
            // Name lookups go through mDNSResponder's socket.
            out.push_str(
                "(allow network-outbound (remote unix-socket (path-literal \"/private/var/run/mDNSResponder\")))\n",
            );
        } else {
            out.push_str("(deny network-outbound (remote ip))\n");
            out.push_str("(deny network-bind (local ip))\n");
        }
        out
    }

    /// The command that runs `inner` inside, with a scrubbed environment.
    pub fn command(&self, cwd: &Path, inner: &[String]) -> Command {
        let mut cmd = crate::platform::command(self.runner.program());
        cmd.args(self.argv(cwd, inner));
        let path = crate::agent::workers_ai::child_path()
            .to_string_lossy()
            .into_owned();
        let mut env = scrub_env(std::env::vars(), &self.caps, &path);
        if let Runner::Seatbelt(_) = self.runner {
            // Seatbelt cannot put the sandbox's folder at /tmp; TMPDIR says where it is.
            for (k, v) in &mut env {
                if k == "TMPDIR" {
                    *v = s(&self.tmp);
                }
            }
            cmd.current_dir(self.inner_cwd(cwd));
        }
        cmd.env_clear().envs(env);
        cmd
    }

    /// Runs `inner` inside to the end (or `timeout`), `input` on its stdin.
    pub fn output(
        &self,
        cwd: &Path,
        inner: &[String],
        input: Option<&[u8]>,
        timeout: Duration,
    ) -> Result<std::process::Output, String> {
        run_with_timeout(self.command(cwd, inner), input, timeout)
            .map_err(|e| format!("the sandbox did not run it: {e}"))
    }
}

/// Runs `cmd` to the end, stopping it and what it started after `timeout`; `input` goes
/// to its stdin. See [`crate::platform::output_within`].
pub fn run_with_timeout(
    mut cmd: Command,
    input: Option<&[u8]>,
    timeout: Duration,
) -> Result<std::process::Output, String> {
    crate::platform::output_within(&mut cmd, input, timeout).map_err(|e| e.to_string())
}

/// Paths a mail account's password command reads, one script deep.
fn mail_secrets(home: &Path, config: &Path) -> Vec<PathBuf> {
    let Ok(text) = std::fs::read_to_string(config) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    let paths = |text: &str| -> Vec<PathBuf> {
        text.split(|c: char| {
            c.is_whitespace()
                || matches!(
                    c,
                    '"' | '\'' | '=' | ';' | '(' | ')' | '<' | '>' | '|' | '`'
                )
        })
        .filter_map(|w| {
            let w = w.trim_end_matches([',', ':']);
            if let Some(r) = w
                .strip_prefix("~/")
                .or_else(|| w.strip_prefix("$HOME/"))
                .or_else(|| w.strip_prefix("${HOME}/"))
            {
                Some(home.join(r))
            } else if w.starts_with('/') && w.len() > 1 {
                Some(PathBuf::from(w))
            } else {
                None
            }
        })
        .collect()
    };
    for line in text
        .lines()
        .filter(|l| l.contains("command") || l.contains("file") || l.contains("path"))
    {
        for p in paths(line.split_once('=').map_or("", |(_, v)| v)) {
            if !p.starts_with(home) {
                continue;
            }
            if let Ok(script) =
                std::fs::read_to_string(&p).map(|t| t.chars().take(20_000).collect::<String>())
            {
                out.extend(
                    paths(&script)
                        .into_iter()
                        .filter(|q| q.starts_with(home) && q.exists()),
                );
            }
            out.push(p);
        }
    }
    out
}

/// Everything to hide for a session in `workspace`, as it is on disk now.
pub fn hidden(home: &Path, caps: &[Cap]) -> Vec<Hidden> {
    let mut paths: Vec<PathBuf> = HIDE.iter().map(|r| home.join(r)).collect();
    if cfg!(target_os = "macos") {
        paths.extend(MAC_HIDE.iter().map(|r| home.join(r)));
    }
    let config = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .unwrap_or_else(|| home.join(".config"));
    let data = crate::platform::data_home();
    paths.extend([
        crate::platform::app_dir(),
        crate::headless::root(),
        data.join("opencode"),
        data.join("keyrings"),
        config.join("opencode"),
        config.join("himalaya"),
        config.join("gh"),
    ]);
    if let Some(h) = std::env::var_os("HIMALAYA_CONFIG") {
        paths.extend(std::env::split_paths(&h));
    }
    for c in [
        config.join("himalaya/config.toml"),
        home.join(".config/himalaya/config.toml"),
    ] {
        paths.extend(mail_secrets(home, &c));
    }
    for dir in [home.to_path_buf(), home.join(".config")] {
        for e in std::fs::read_dir(&dir).into_iter().flatten().flatten() {
            if secret_name(&e.file_name().to_string_lossy()) {
                paths.push(e.path());
            }
        }
    }
    if caps.contains(&Cap::Modal) {
        paths.retain(|p| p != &home.join(".modal.toml"));
    }
    let mut out: Vec<Hidden> = Vec::new();
    for p in paths {
        let Ok(meta) = std::fs::symlink_metadata(&p) else {
            continue;
        };
        // A link is hidden where it leads, and where it is.
        let real = crate::platform::canonicalize(&p).ok();
        for q in [Some(p.clone()), real].into_iter().flatten() {
            let dir = std::fs::metadata(&q)
                .map(|m| m.is_dir())
                .unwrap_or(meta.is_dir());
            if q.exists() && q != Path::new("/") && !out.iter().any(|h| h.path == q) {
                out.push(Hidden { path: q, dir });
            }
        }
    }
    // Parents first; what a hidden folder holds is gone with it.
    out.sort_by_key(|h| h.path.components().count());
    let mut kept: Vec<Hidden> = Vec::new();
    for h in out {
        if !kept.iter().any(|k| k.dir && h.path.starts_with(&k.path)) {
            kept.push(h);
        }
    }
    kept
}

/// The [`TOOLCHAINS`] there are, minus anything hidden.
pub fn toolchains(home: &Path, hidden: &[Hidden]) -> Vec<Bound> {
    let mut out = Vec::new();
    for rel in TOOLCHAINS {
        let p = home.join(rel);
        let Ok(meta) = std::fs::symlink_metadata(&p) else {
            continue;
        };
        if hidden.iter().any(|h| p.starts_with(&h.path)) {
            continue;
        }
        if meta.file_type().is_symlink() {
            if let Ok(to) = std::fs::read_link(&p) {
                out.push(Bound::Link(p, to));
            }
        } else {
            out.push(Bound::Path(p));
        }
    }
    out
}

/// What a Seatbelt rule that hides a file names; an allow must name the same.
pub const READ: &str = "file-read-data file-read-xattr";

/// `p` as a Seatbelt string, quotes and backslashes escaped.
pub fn sbpl_str(p: &Path) -> String {
    format!("\"{}\"", s(p).replace('\\', "\\\\").replace('"', "\\\""))
}

/// `(subpath "p")`: `p` and everything under it.
pub fn sbpl_subpath(p: &Path) -> String {
    format!("(subpath {})", sbpl_str(p))
}

/// Seatbelt judges a path when it is used, so renaming a folder above a hidden or
/// read-only path moves it out from under its rule (`mv sub x && cat x/.env`).
/// This denies renaming or removing each folder from a writable `root` down to a
/// `pinned` path; what is inside those folders stays writable.
pub fn sbpl_pin(roots: &[&Path], pinned: &[&Path]) -> String {
    let mut dirs: Vec<&Path> = pinned
        .iter()
        .flat_map(|p| p.ancestors().skip(1))
        // The root too: moving the whole workspace into its /tmp would do the same.
        .filter(|a| roots.iter().any(|r| a.starts_with(r)))
        .collect();
    dirs.sort();
    dirs.dedup();
    if dirs.is_empty() {
        return String::new();
    }
    let lits: Vec<String> = dirs
        .iter()
        .map(|d| format!("(literal {})", sbpl_str(d)))
        .collect();
    format!("(deny file-write-unlink {})\n", lits.join(" "))
}

/// `s` with what a Seatbelt regex treats as special escaped.
pub fn sbpl_regex_escape(s: &str) -> String {
    let mut out = String::new();
    for c in s.chars() {
        if "\\.+*?()|[]{}^$\"".contains(c) {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

/// Hides files under `root` named like secrets, also those made after the scan
/// at launch: a new `.env`, a key the agent fetched. The commonest names of
/// [`crate::secrets::secret_file`]; the scan still finds the rest of what is
/// there at launch.
pub fn sbpl_secret_names(root: &Path) -> String {
    let r = sbpl_regex_escape(&root.to_string_lossy());
    // As `secret_file` says them; a stem has no dot, so `password_reset.rs` and
    // `service.env.example` stay readable. Seatbelt reads the dot in `[^/.]` as
    // any character, so the stem is a class of what it may hold instead.
    const STEM: &str = "[-_A-Za-z0-9]";
    const DATA: &str = r"(\.(json|txt|toml|yaml|yml|ini|conf|cfg))?";
    let words = format!(
        r"{STEM}*(secret|password|passwd|wallet|keypair|apikey|api_key|api-key){STEM}*{DATA}"
    );
    let token = format!(r"({STEM}*[-_])?tokens?([-_]{STEM}*)?{DATA}");
    let names = [
        r"\.env(\.[^/]*)?",
        r"[^/]*\.env",
        r"[^/]*\.(pem|key|p12|pfx|keystore|jks|gpg|asc)",
        r"id_(rsa|ed25519|ecdsa|dsa)[^/]*",
        r"(\.netrc|\.npmrc|\.pypirc|\.git-credentials|auth\.json|credentials(\.json|\.toml)?|\.modal\.toml)",
        &words,
        &token,
    ];
    let rules: Vec<String> = names
        .iter()
        .map(|n| format!("(regex #\"^{r}/(.*/)?{n}$\")"))
        .collect();
    format!("(deny {READ} {})\n", rules.join(" "))
}

/// What an empty stand-in for a frozen path is, when bubblewrap needs one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stand {
    File,
    Dir,
}

/// The Git metadata a sandbox keeps read-only, with the stand-in each needs when
/// it is not there yet, and what is bound back writable: `git` is the
/// repository's shared Git folder and this checkout's own, None without one.
///
/// Git reads config and hooks, which run commands, from wherever the metadata
/// leads it: `.git` as a file names the checkout's folder, its `commondir` the
/// shared one. So those stay as they are too, and so do the other checkouts'
/// folders (their `commondir`, `config.worktree`). `.git` itself is bound over
/// itself: a mount point can't be renamed, so it can't be swapped for one the
/// agent made.
pub fn git_freeze(ws: &Path, git: Option<(&Path, &Path)>) -> (Vec<(PathBuf, Stand)>, Vec<PathBuf>) {
    let Some((common, own)) = git else {
        // No repository, and none made inside (`git init`): git outside would read its config.
        return (vec![(ws.join(".git"), Stand::Dir)], Vec::new());
    };
    let mut frozen = vec![
        (common.join("config"), Stand::File),
        (common.join("hooks"), Stand::Dir),
        (common.join("worktrees"), Stand::Dir),
        // With extensions.worktreeConfig, a checkout's own config counts too.
        (own.join("config.worktree"), Stand::File),
    ];
    if own == common {
        (frozen, vec![common.to_path_buf()])
    } else {
        // A linked worktree: its folder sits in the frozen `worktrees`, writable
        // again but for what leads to the repository and back.
        frozen.extend([
            (ws.join(".git"), Stand::File),
            (own.join("commondir"), Stand::File),
            (own.join("gitdir"), Stand::File),
        ]);
        (frozen, vec![own.to_path_buf()])
    }
}

/// Makes an empty file or folder at `p` when nothing is there, not even a
/// dangling link, so bubblewrap has something to bind over.
pub fn stand_in(p: &Path, kind: Stand) -> std::io::Result<()> {
    if std::fs::symlink_metadata(p).is_ok() {
        return Ok(());
    }
    match kind {
        Stand::File => std::fs::write(p, ""),
        Stand::Dir => std::fs::create_dir_all(p),
    }
}

/// bubblewrap's binds for `frozen` (read-only) and `thawed` (writable), a folder
/// before what is inside it, so each lands on the one around it.
pub fn bwrap_freeze(frozen: &[PathBuf], thawed: &[PathBuf]) -> Vec<String> {
    let mut all: Vec<(&PathBuf, &str)> = frozen
        .iter()
        .map(|p| (p, "--ro-bind"))
        .chain(thawed.iter().map(|p| (p, "--bind")))
        .collect();
    all.sort_by_key(|(p, _)| p.components().count());
    all.into_iter()
        .flat_map(|(p, how)| [how.to_string(), s(p), s(p)])
        .collect()
}

/// Seatbelt's rules for the same: no writes to `frozen`, but in a `thawed`
/// folder inside one of them, and again none to what is frozen inside that.
/// Seatbelt needs no mount point to keep a folder from being renamed
/// ([`sbpl_pin`]), so a thawed folder outside the frozen ones adds nothing.
pub fn sbpl_freeze(frozen: &[&Path], thawed: &[&Path]) -> String {
    let mut out = format!("(deny file-write* {})\n", sbpl_list(frozen));
    let inner: Vec<&Path> = thawed
        .iter()
        .copied()
        .filter(|t| frozen.iter().any(|f| t.starts_with(f) && t != f))
        .collect();
    if inner.is_empty() {
        return out;
    }
    out.push_str(&format!("(allow file-write* {})\n", sbpl_list(&inner)));
    let again: Vec<&Path> = frozen
        .iter()
        .copied()
        .filter(|f| inner.iter().any(|t| f.starts_with(t) && f != t))
        .collect();
    if !again.is_empty() {
        out.push_str(&format!("(deny file-write* {})\n", sbpl_list(&again)));
    }
    out
}

pub fn sbpl_list(ps: &[&Path]) -> String {
    ps.iter()
        .map(|p| sbpl_subpath(p))
        .collect::<Vec<_>>()
        .join(" ")
}

/// Mach services a Seatbelt sandbox may not reach: the keychain, the clipboard
/// and LaunchServices (`open`, which would start a program outside).
pub const SEATBELT_SERVICES: &[&str] = &[
    "com.apple.SecurityServer",
    "com.apple.securityd.xpc",
    "com.apple.security.agent",
    "com.apple.secd",
    "com.apple.pasteboard.1",
    "com.apple.coreservices.launchservicesd",
];

/// The runner this platform has: Seatbelt on macOS, bubblewrap elsewhere.
/// [`git_freeze`] for a headless run, where only the workspace is writable: a
/// linked worktree's folders are outside it, so only its `.git` file stays. A
/// `.git` folder with no `HEAD` is no repository to git (the stand-in of an
/// earlier run), so it stays read-only whole.
fn headless_git_freeze(ws: &Path) -> (Vec<(PathBuf, Stand)>, Vec<PathBuf>) {
    let git = ws.join(".git");
    match std::fs::symlink_metadata(&git) {
        Err(_) => git_freeze(ws, None),
        Ok(m) if m.is_dir() && git.join("HEAD").exists() => git_freeze(ws, Some((&git, &git))),
        Ok(m) if m.is_dir() => (vec![(git, Stand::Dir)], Vec::new()),
        Ok(_) => (vec![(git, Stand::File)], Vec::new()),
    }
}

pub fn find_runner() -> Option<Runner> {
    if cfg!(target_os = "macos") {
        return Some(PathBuf::from("/usr/bin/sandbox-exec"))
            .filter(|p| p.exists())
            .map(Runner::Seatbelt);
    }
    find_bwrap().map(Runner::Bwrap)
}

pub fn find_bwrap() -> Option<PathBuf> {
    std::env::var_os("ARAKUN_BWRAP")
        .map(PathBuf::from)
        .or_else(|| crate::agent::which("bwrap"))
        .or_else(|| Some(PathBuf::from("/usr/bin/bwrap")).filter(|p| p.exists()))
}

impl Sandbox {
    /// The sandbox of the session whose data is in `session_dir`; it checks
    /// that the runner starts, so a broken one is an error now, not a silent bypass.
    pub fn new(
        workspace: &Path,
        session_dir: &Path,
        caps: &[Cap],
        reads: &[PathBuf],
    ) -> Result<Self, String> {
        Self::with(find_runner(), workspace, session_dir, caps, reads)
    }

    fn with(
        runner: Option<Runner>,
        workspace: &Path,
        session_dir: &Path,
        caps: &[Cap],
        reads: &[PathBuf],
    ) -> Result<Self, String> {
        let runner = runner.ok_or(if cfg!(target_os = "macos") {
            "sandbox-exec is not at /usr/bin/sandbox-exec; headless agents run inside it. Pass --no-sandbox, which you must then confirm."
        } else {
            "bubblewrap (bwrap) is not installed; headless agents run inside it. Install it (pacman -S bubblewrap / apt install bubblewrap), or pass --no-sandbox, which you must then confirm."
        })?;
        let workspace = crate::platform::canonicalize(workspace)
            .map_err(|e| format!("{}: {e}", workspace.display()))?;
        let tmp = session_dir.join("tmp");
        std::fs::create_dir_all(&tmp).map_err(|e| format!("{}: {e}", tmp.display()))?;
        // Seatbelt matches real paths (/private/var, not /var).
        let tmp = crate::platform::canonicalize(&tmp).unwrap_or(tmp);
        let empty = session_dir.join("empty");
        std::fs::write(&empty, b"").map_err(|e| format!("{}: {e}", empty.display()))?;
        // Seatbelt matches real paths: a deny on a linked $HOME would match nothing.
        let home = crate::platform::home();
        let home = crate::platform::canonicalize(&home).unwrap_or(home);
        let (frozen, thawed) = headless_git_freeze(&workspace);
        if matches!(runner, Runner::Bwrap(_)) {
            // bubblewrap binds over a path, so it must be there; without that, no
            // sandbox rather than a hole in it.
            for (p, kind) in &frozen {
                stand_in(p, *kind).map_err(|e| {
                    format!("{}: {e}; the sandbox can't keep it read-only", p.display())
                })?;
            }
        }
        let frozen = frozen.into_iter().map(|(p, _)| p).collect();
        let mut extra_ro = Vec::new();
        if caps.iter().any(|c| matches!(c, Cap::Net | Cap::Modal))
            && let Ok(r) = crate::platform::canonicalize("/etc/resolv.conf")
            && r.starts_with("/run")
        {
            extra_ro.push(r);
        }
        let mut hidden = hidden(&home, caps);
        let reads = reads
            .iter()
            .map(|r| check_read(r, &hidden))
            .collect::<Result<Vec<_>, _>>()?;
        // What looks like a secret in the workspace stays out too (.env, keys).
        hidden.extend(
            scan_secrets(&workspace)
                .into_iter()
                .map(|path| Hidden { path, dir: false }),
        );
        let mut toolchains = toolchains(&home, &hidden);
        if caps.contains(&Cap::Modal) && home.join(".modal.toml").is_file() {
            toolchains.push(Bound::Path(home.join(".modal.toml")));
        }
        let sb = Sandbox {
            runner,
            home,
            toolchains,
            reads,
            workspace,
            tmp,
            empty,
            caps: caps.to_vec(),
            hidden,
            frozen,
            thawed,
            extra_ro,
        };
        sb.probe()?;
        Ok(sb)
    }

    fn probe(&self) -> Result<(), String> {
        let out = self
            .output(
                &self.workspace,
                &["true".into()],
                None,
                Duration::from_secs(20),
            )
            .map_err(|e| {
                format!(
                    "{} ({}) would not start: {e}",
                    self.runner.name(),
                    self.runner.program().display()
                )
            })?;
        if out.status.success() {
            Ok(())
        } else {
            Err(format!(
                "{} ({}) would not start a sandbox: {}",
                self.runner.name(),
                self.runner.program().display(),
                String::from_utf8_lossy(&out.stderr).trim()
            ))
        }
    }

    // ---- file tools, seen from inside ----------------------------------------------

    fn sh(
        &self,
        cwd: &Path,
        script: &str,
        args: &[&str],
        input: Option<&[u8]>,
    ) -> Result<std::process::Output, String> {
        let mut inner = vec![
            "sh".to_string(),
            "-c".to_string(),
            script.to_string(),
            "sh".to_string(),
        ];
        inner.extend(args.iter().map(|a| a.to_string()));
        self.output(cwd, &inner, input, Duration::from_secs(60))
    }

    fn failed(out: &std::process::Output, what: &str) -> String {
        let e = String::from_utf8_lossy(&out.stderr)
            .trim()
            .replace("sh: 1: ", "")
            .to_string();
        if e.is_empty() {
            format!("cannot {what}")
        } else {
            e
        }
    }

    pub fn read(&self, cwd: &Path, path: &Path) -> Result<Vec<u8>, String> {
        let out = self.sh(cwd, "exec cat -- \"$1\"", &[&s(path)], None)?;
        if out.status.success() {
            Ok(out.stdout)
        } else {
            Err(Self::failed(&out, &format!("read {}", path.display())))
        }
    }

    /// Writes `content` to `path`; whether it was there before.
    pub fn write(&self, cwd: &Path, path: &Path, content: &[u8]) -> Result<bool, String> {
        let script = "e=N; [ -e \"$1\" ] && e=Y; mkdir -p -- \"$(dirname -- \"$1\")\" && cat > \"$1\" && printf %s \"$e\"";
        let out = self.sh(cwd, script, &[&s(path)], Some(content))?;
        if out.status.success() {
            Ok(out.stdout == b"Y")
        } else {
            Err(Self::failed(&out, &format!("write {}", path.display())))
        }
    }

    /// A folder's entries, folders ending in `/`.
    pub fn list(&self, cwd: &Path, dir: &Path) -> Result<Vec<String>, String> {
        let out = self.sh(cwd, "LC_ALL=C exec ls -1Ap -- \"$1\"", &[&s(dir)], None)?;
        if !out.status.success() {
            return Err(Self::failed(&out, &format!("list {}", dir.display())));
        }
        Ok(String::from_utf8_lossy(&out.stdout)
            .lines()
            .filter(|l| !l.is_empty())
            .map(str::to_string)
            .collect())
    }
}

// ---- the session's confinement ----------------------------------------------------

/// What a headless host process runs its agent's tools under.
pub struct Jail {
    /// `None` only after the user confirmed `--no-sandbox`.
    pub sandbox: Option<Sandbox>,
    pub caps: Vec<Cap>,
    pub workspace: PathBuf,
}

fn slot() -> &'static RwLock<Option<Arc<Jail>>> {
    static JAIL: OnceLock<RwLock<Option<Arc<Jail>>>> = OnceLock::new();
    JAIL.get_or_init(Default::default)
}

/// Set once by the headless host; the app's own sessions have none.
pub fn set(jail: Option<Jail>) {
    *slot().write().unwrap() = jail.map(Arc::new);
}

pub fn current() -> Option<Arc<Jail>> {
    slot().read().unwrap().clone()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn a_command_that_outlives_its_timeout_is_stopped() {
        let mut cmd = Command::new("sleep");
        cmd.arg("30");
        let start = std::time::Instant::now();
        let out = run_with_timeout(cmd, None, Duration::from_millis(200));
        assert!(out.unwrap_err().contains("took longer"));
        // Killed and reaped, not waited out.
        assert!(start.elapsed() < Duration::from_secs(10));
        // One that ends in time gives its output.
        let mut cmd = Command::new("sh");
        cmd.args(["-c", "printf hi"]);
        let out = run_with_timeout(cmd, None, Duration::from_secs(10)).unwrap();
        assert_eq!(out.stdout, b"hi");
    }

    fn sb(caps: &[Cap]) -> Sandbox {
        Sandbox {
            runner: Runner::Bwrap("/usr/bin/bwrap".into()),
            home: "/home/u".into(),
            toolchains: vec![
                Bound::Path("/home/u/.cargo/bin".into()),
                Bound::Path("/home/u/.local/bin".into()),
                Bound::Link(
                    "/home/u/.nix-profile".into(),
                    "/home/u/.local/state/nix/profiles/profile".into(),
                ),
            ],
            reads: vec!["/home/u/data".into()],
            workspace: "/home/u/proj".into(),
            tmp: "/d/h1/tmp".into(),
            empty: "/d/h1/empty".into(),
            caps: caps.to_vec(),
            hidden: vec![
                Hidden {
                    path: "/home/u/.ssh".into(),
                    dir: true,
                },
                Hidden {
                    path: "/home/u/.netrc".into(),
                    dir: false,
                },
                Hidden {
                    path: "/home/u/proj/.env".into(),
                    dir: false,
                },
                Hidden {
                    path: "/home/u/.local/bin/mail-pass".into(),
                    dir: false,
                },
                Hidden {
                    path: "/data/arakun".into(),
                    dir: true,
                },
                Hidden {
                    path: "/tmp/x/arakun".into(),
                    dir: true,
                },
            ],
            frozen: vec![
                "/home/u/proj/.git/config".into(),
                "/home/u/proj/.git/hooks".into(),
            ],
            thawed: vec!["/home/u/proj/.git".into()],
            extra_ro: Vec::new(),
        }
    }

    fn after<'a>(a: &'a [String], flag: &str) -> Vec<&'a str> {
        a.windows(2)
            .filter(|w| w[0] == flag)
            .map(|w| w[1].as_str())
            .collect()
    }

    #[test]
    fn the_sandbox_hides_credentials_and_cuts_the_network() {
        let a = sb(&[]).argv(
            Path::new("/home/u/proj/src"),
            &["bash".into(), "-c".into(), "ls".into()],
        );
        for f in [
            "--die-with-parent",
            "--new-session",
            "--unshare-pid",
            "--unshare-ipc",
            "--unshare-uts",
            "--unshare-net",
        ] {
            assert!(a.contains(&f.to_string()), "{f} missing: {a:?}");
        }
        let tmpfs = after(&a, "--tmpfs");
        assert!(
            tmpfs.contains(&"/run")
                && tmpfs.contains(&"/home/u")
                && tmpfs.contains(&"/data/arakun")
        );
        // Under the empty $HOME, .ssh is gone without a mount of its own; /tmp is the sandbox's.
        assert!(!tmpfs.contains(&"/home/u/.ssh") && !tmpfs.contains(&"/tmp/x/arakun"));
        let ro: Vec<(&str, &str)> = a
            .windows(3)
            .filter(|w| w[0] == "--ro-bind")
            .map(|w| (w[1].as_str(), w[2].as_str()))
            .collect();
        assert!(ro.contains(&("/", "/")));
        assert!(ro.contains(&("/home/u/.cargo/bin", "/home/u/.cargo/bin")));
        assert!(ro.contains(&("/home/u/data", "/home/u/data")));
        assert!(!ro.iter().any(|(_, d)| *d == "/home/u/.netrc"));
        assert!(ro.contains(&("/d/h1/empty", "/home/u/.local/bin/mail-pass")));
        assert!(ro.contains(&("/d/h1/empty", "/home/u/proj/.env")));
        assert!(ro.contains(&("/home/u/proj/.git/hooks", "/home/u/proj/.git/hooks")));
        // `.git` is a mount point, so it can't be renamed and replaced, and what
        // is read-only in it lands on that mount.
        let at = |x: [&str; 3]| a.windows(3).position(|w| w == x).unwrap();
        let git = at(["--bind", "/home/u/proj/.git", "/home/u/proj/.git"]);
        assert!(git > at(["--bind", "/home/u/proj", "/home/u/proj"]));
        assert!(
            git < at([
                "--ro-bind",
                "/home/u/proj/.git/config",
                "/home/u/proj/.git/config"
            ])
        );
        assert!(a.windows(3).any(|w| w
            == [
                "--symlink",
                "/home/u/.local/state/nix/profiles/profile",
                "/home/u/.nix-profile"
            ]));
        let pos = |x: &str| a.iter().rposition(|y| y == x).unwrap();
        // $HOME empties first, then what comes back, then the hides over it.
        assert!(pos("/home/u") < pos("/home/u/.cargo/bin"));
        assert!(
            pos("/home/u/proj/.env") > pos("/home/u/proj")
                && pos("/home/u/.local/bin/mail-pass") > pos("/home/u/.local/bin")
        );
        assert!(after(&a, "--bind").contains(&"/d/h1/tmp"));
        assert!(after(&a, "--bind").contains(&"/home/u/proj"));
        assert_eq!(after(&a, "--chdir"), vec!["/home/u/proj/src"]);
        assert_eq!(&a[a.len() - 4..], &["--", "bash", "-c", "ls"]);
        // Outside the workspace it starts in the workspace.
        assert_eq!(
            after(&sb(&[]).argv(Path::new("/etc"), &[]), "--chdir"),
            vec!["/home/u/proj"]
        );
    }

    #[test]
    fn the_seatbelt_profile_says_the_same() {
        let mut b = sb(&[]);
        b.runner = Runner::Seatbelt("/usr/bin/sandbox-exec".into());
        let p = b.seatbelt_profile();
        let at = |needle: &str| p.find(needle).unwrap_or_else(|| panic!("{needle} in\n{p}"));
        // $HOME goes dark, its toolchains come back, its secrets go again, then /tmp.
        let dark = at("(deny file-read-data file-read-xattr (subpath \"/home/u\")");
        let shown = at("(allow file-read-data file-read-xattr (subpath \"/home/u/.cargo/bin\")");
        let hidden = at("(deny file-read-data file-read-xattr (subpath \"/home/u/.ssh\")");
        let tmp = at("(allow file-read-data file-read-xattr (subpath \"/d/h1/tmp\"))");
        assert!(dark < shown && shown < hidden && hidden < tmp);
        assert!(p.contains("(subpath \"/home/u/proj/.env\")"));
        // The Git config and hooks, there or not yet.
        assert!(p.contains(
            "(deny file-write* (subpath \"/home/u/proj/.git/commondir\") (subpath \"/home/u/proj/.git/config\") (subpath \"/home/u/proj/.git/hooks\"))"
        ));
        // Secrets made later, by name.
        assert!(p.contains("(regex #\"^/home/u/proj/(.*/)?\\.env(\\.[^/]*)?$\")"));
        assert!(p.contains("(regex #\"^/home/u/proj/(.*/)?[^/]*\\.env$\")"));
        assert!(p.contains("(global-name \"com.apple.SecurityServer\")"));
        assert!(p.contains("(deny network-outbound (remote ip))"));
        // Their folders can't be renamed away from the rules, nor ttys written.
        assert!(p.contains(
            "(deny file-write-unlink (literal \"/home/u/proj\") (literal \"/home/u/proj/.git\"))"
        ));
        assert!(!p.contains("^/dev/tty"));
        let net = Sandbox {
            caps: vec![Cap::Net],
            ..b.clone()
        };
        assert!(!net.seatbelt_profile().contains("(remote ip)"));
        // Seatbelt runs the command itself; the profile comes first.
        let a = b.argv(Path::new("/elsewhere"), &["true".into()]);
        assert_eq!((a[0].as_str(), a[2].as_str()), ("-p", "true"));
        assert_eq!(b.shown_tmp(), Path::new("/d/h1/tmp"));
        // A quote in a path can't end the string early.
        b.workspace = "/home/u/a\"b".into();
        assert!(
            b.seatbelt_profile()
                .contains("(subpath \"/home/u/a\\\"b\")")
        );
    }

    #[test]
    fn the_git_metadata_stays_put() {
        let d = std::env::temp_dir().join(format!("arakun-hs-git-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        let names = |(f, t): (Vec<(PathBuf, Stand)>, Vec<PathBuf>)| {
            let rel = |p: &Path| p.strip_prefix(&d).unwrap().to_string_lossy().into_owned();
            (
                f.iter().map(|(p, k)| (rel(p), *k)).collect::<Vec<_>>(),
                t.iter().map(|p| rel(p)).collect::<Vec<_>>(),
            )
        };
        // No repository: none made inside, so none whose config git outside reads.
        assert_eq!(
            names(headless_git_freeze(&d)),
            (vec![(".git".into(), Stand::Dir)], vec![])
        );
        // A stand-in from an earlier run is no repository either.
        std::fs::create_dir(d.join(".git")).unwrap();
        assert_eq!(
            names(headless_git_freeze(&d)).0,
            vec![(".git".into(), Stand::Dir)]
        );
        // A repository: what runs commands and what leads elsewhere; `.git` is
        // bound over itself. Its `commondir` is not there, so Seatbelt names it.
        std::fs::write(d.join(".git/HEAD"), "ref: refs/heads/main\n").unwrap();
        let (frozen, thawed) = names(headless_git_freeze(&d));
        for (p, k) in [
            (".git/config", Stand::File),
            (".git/hooks", Stand::Dir),
            (".git/worktrees", Stand::Dir),
            (".git/config.worktree", Stand::File),
        ] {
            assert!(frozen.contains(&(p.into(), k)), "{p} in {frozen:?}");
        }
        assert!(!frozen.iter().any(|(p, _)| p.ends_with("commondir")));
        assert_eq!(thawed, vec![".git".to_string()]);
        // A linked worktree: its `.git` file. The rest is outside the workspace.
        std::fs::remove_dir_all(d.join(".git")).unwrap();
        std::fs::write(d.join(".git"), "gitdir: /elsewhere\n").unwrap();
        assert_eq!(
            names(headless_git_freeze(&d)),
            (vec![(".git".into(), Stand::File)], vec![])
        );
        // Stand-ins are made only where nothing is, links included.
        stand_in(&d.join("f"), Stand::File).unwrap();
        stand_in(&d.join("dir/sub"), Stand::Dir).unwrap();
        assert!(d.join("f").is_file() && d.join("dir/sub").is_dir());
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(d.join("nowhere"), d.join("l")).unwrap();
            stand_in(&d.join("l"), Stand::File).unwrap();
            assert!(!d.join("nowhere").exists());
        }
        std::fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn a_thawed_folder_inside_a_frozen_one() {
        let frozen = [
            Path::new("/r/.git/worktrees"),
            Path::new("/r/.git/worktrees/a/commondir"),
            Path::new("/r/.git/config"),
        ];
        let p = sbpl_freeze(
            &frozen,
            &[Path::new("/r/.git/worktrees/a"), Path::new("/r/.git")],
        );
        assert_eq!(
            p,
            "(deny file-write* (subpath \"/r/.git/worktrees\") (subpath \"/r/.git/worktrees/a/commondir\") (subpath \"/r/.git/config\"))\n\
             (allow file-write* (subpath \"/r/.git/worktrees/a\"))\n\
             (deny file-write* (subpath \"/r/.git/worktrees/a/commondir\"))\n"
        );
        let owned = |ps: &[&str]| ps.iter().map(PathBuf::from).collect::<Vec<_>>();
        let a = bwrap_freeze(
            &owned(&["/r/.git/worktrees/a/commondir", "/r/.git/worktrees"]),
            &owned(&["/r/.git/worktrees/a"]),
        );
        assert_eq!(
            a,
            [
                "--ro-bind",
                "/r/.git/worktrees",
                "/r/.git/worktrees",
                "--bind",
                "/r/.git/worktrees/a",
                "/r/.git/worktrees/a",
                "--ro-bind",
                "/r/.git/worktrees/a/commondir",
                "/r/.git/worktrees/a/commondir",
            ]
        );
    }

    #[test]
    fn network_caps_share_the_network() {
        for caps in [[Cap::Net], [Cap::Modal]] {
            assert!(
                !sb(&caps)
                    .argv(Path::new("/home/u/proj"), &[])
                    .contains(&"--unshare-net".to_string())
            );
        }
        assert!(
            sb(&[Cap::MailRead])
                .argv(Path::new("/home/u/proj"), &[])
                .contains(&"--unshare-net".to_string())
        );
    }

    #[test]
    fn reads_refuse_secrets() {
        let d = std::env::temp_dir().join(format!("arakun-sbx-read-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(d.join("ok/.venv/lib")).unwrap();
        std::fs::create_dir_all(d.join("bad/sub")).unwrap();
        std::fs::write(d.join("ok/main.py"), "").unwrap();
        std::fs::write(d.join("ok/password_reset.rs"), "").unwrap();
        std::fs::write(d.join("ok/tokenizer.json"), "").unwrap();
        std::fs::write(d.join("ok/.venv/lib/.env"), "").unwrap();
        std::fs::write(d.join("bad/sub/.env"), "K=v").unwrap();
        assert_eq!(
            check_read(&d.join("ok"), &[]).unwrap(),
            crate::platform::canonicalize(d.join("ok")).unwrap()
        );
        assert!(
            check_read(&d.join("bad"), &[])
                .unwrap_err()
                .contains(".env")
        );
        std::fs::create_dir_all(d.join("ok/private")).unwrap();
        let hidden = vec![Hidden {
            path: crate::platform::canonicalize(d.join("ok/private")).unwrap(),
            dir: true,
        }];
        assert!(
            check_read(&d.join("ok"), &hidden)
                .unwrap_err()
                .contains("stays hidden")
        );
        assert!(check_read(&d.join("ok/private"), &hidden).is_err());
        assert!(check_read(Path::new("/"), &[]).is_err());
        assert!(check_read(&d.join("missing"), &[]).is_err());
        std::fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn caps_parse() {
        assert_eq!(parse_caps("net, modal,net"), Ok(vec![Cap::Net, Cap::Modal]));
        assert_eq!(parse_caps("mail-read"), Ok(vec![Cap::MailRead]));
        assert!(parse_caps("root").unwrap_err().contains("root"));
        assert_eq!(caps_names(&[]), "none");
        assert_eq!(
            serde_json::to_value([Cap::MailRead]).unwrap(),
            serde_json::json!(["mail-read"])
        );
    }

    #[test]
    fn the_hide_list_covers_what_the_user_named() {
        for p in [
            ".ssh",
            ".gnupg",
            ".config/himalaya",
            ".config/gh",
            ".config/cloudflare",
            ".local/share/opencode",
            ".config/opencode",
            ".modal.toml",
            ".netrc",
            ".git-credentials",
            ".docker",
            ".aws",
            ".claude",
            ".local/share/arakun",
            ".cargo/credentials.toml",
            ".npmrc",
            ".local/share/keyrings",
            ".mozilla",
            ".config/BraveSoftware",
        ] {
            assert!(HIDE.contains(&p), "{p} is not hidden");
        }
        for n in [
            "x-key.txt",
            "secure-arb-wallet.json",
            ".env",
            "r2-site.env",
            "wbg-admin-password.txt",
            "cert.pem",
            "id_ed25519",
            "service-account.json",
        ] {
            assert!(secret_name(n), "{n}");
        }
        for n in [".bashrc", "Projects", ".gitconfig", ".cargo"] {
            assert!(!secret_name(n), "{n}");
        }
    }

    #[test]
    fn hidden_paths_follow_caps_and_links() {
        let home = std::env::temp_dir().join(format!("arakun-sbx-home-{}", std::process::id()));
        std::fs::create_dir_all(home.join(".ssh")).unwrap();
        // macOS's temp dir is itself a link (/var -> /private/var), and links resolve.
        let home = crate::platform::canonicalize(&home).unwrap();
        std::fs::create_dir_all(home.join("real")).unwrap();
        std::fs::write(home.join(".modal.toml"), "token").unwrap();
        std::fs::write(home.join("x-key.txt"), "k").unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(home.join("real"), home.join(".codex")).unwrap();
        let h = hidden(&home, &[]);
        let has = |p: PathBuf| h.iter().any(|x| x.path == p);
        assert!(
            has(home.join(".ssh")) && has(home.join(".modal.toml")) && has(home.join("x-key.txt"))
        );
        #[cfg(unix)]
        assert!(has(home.join("real")));
        assert!(h.iter().find(|x| x.path == home.join(".ssh")).unwrap().dir);
        let h = hidden(&home, &[Cap::Modal]);
        assert!(!h.iter().any(|x| x.path == home.join(".modal.toml")));
        std::fs::remove_dir_all(&home).unwrap();
    }

    #[test]
    fn no_bwrap_no_sandbox() {
        let d = std::env::temp_dir().join(format!("arakun-sbx-none-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        assert!(
            Sandbox::with(None, &d, &d, &[], &[])
                .unwrap_err()
                .contains("--no-sandbox")
        );
        assert!(
            Sandbox::with(Some(Runner::Bwrap(d.join("no-bwrap"))), &d, &d, &[], &[])
                .unwrap_err()
                .contains("would not start")
        );
        std::fs::remove_dir_all(&d).unwrap();
    }

    /// The real thing on macOS. It reads the real `$HOME` and tries the network:
    /// `cargo test --no-default-features inside_seatbelt -- --ignored`.
    #[cfg(target_os = "macos")]
    #[test]
    #[ignore]
    fn inside_seatbelt() {
        let Some(runner) = find_runner() else { return };
        let d = crate::platform::canonicalize(std::env::temp_dir())
            .unwrap()
            .join(format!("arakun-sbx-sb-{}", std::process::id()));
        let ws = d.join("ws");
        std::fs::create_dir_all(&ws).unwrap();
        let sb = Sandbox::with(Some(runner), &ws, &d.join("session"), &[], &[]).unwrap();
        // Beside the workspace, in the host's temporary folder.
        let secret = d.join("secret");
        std::fs::create_dir_all(&secret).unwrap();
        std::fs::write(secret.join("token"), "s3cr3t").unwrap();
        let run = |c: &str| {
            let o = sb
                .output(
                    &ws,
                    &["sh".into(), "-c".into(), c.into()],
                    None,
                    Duration::from_secs(20),
                )
                .unwrap();
            (
                o.status.success(),
                String::from_utf8_lossy(&o.stdout).trim().to_string(),
            )
        };
        assert_eq!(run("echo hi > f && cat f"), (true, "hi".into()));
        assert_eq!(run("pwd"), (true, ws.display().to_string()));
        assert_eq!(std::fs::read_to_string(ws.join("f")).unwrap(), "hi\n");
        let home = crate::platform::home();
        let probe = home.join(format!(".arakun-sbx-probe-{}", std::process::id()));
        assert!(!run(&format!("touch {}", probe.display())).0);
        assert!(!probe.exists());
        // $HOME reads as empty, its toolchains aside.
        assert!(!run(&format!("ls {} > /dev/null", home.display())).0);
        if home.join(".zshrc").is_file() {
            assert!(!run(&format!("cat {}/.zshrc > /dev/null", home.display())).0);
        }
        if home.join(".cargo/bin").is_dir() {
            assert!(run(&format!("ls {}/.cargo/bin > /dev/null", home.display())).0);
            assert!(!run(&format!("touch {}/.cargo/bin/x", home.display())).0);
        }
        assert!(!run("touch /etc/x").0);
        assert!(!run(&format!("cat {}/token", secret.display())).0);
        assert!(!run("touch /private/tmp/arakun-sbx-x").0);
        // A rename can't move a secret out from under its rule.
        std::fs::create_dir_all(ws.join("sub")).unwrap();
        std::fs::write(ws.join("sub/.env"), "KEY=1").unwrap();
        let sb = Sandbox::with(find_runner(), &ws, &d.join("session"), &[], &[]).unwrap();
        let run = |c: &str| {
            let o = sb
                .output(
                    &ws,
                    &["sh".into(), "-c".into(), c.into()],
                    None,
                    Duration::from_secs(20),
                )
                .unwrap();
            (
                o.status.success(),
                String::from_utf8_lossy(&o.stdout).trim().to_string(),
            )
        };
        assert!(!run("cat sub/.env").0);
        // Nor a new name: a hard link or a symlink to it reads nothing.
        assert!(!run("ln sub/.env copy && cat copy").0);
        assert!(!run("ln -s sub/.env l && cat l").0);
        // A secret made after the scan stays hidden too.
        std::fs::write(ws.join("sub/later.env"), "KEY=2").unwrap();
        assert!(!run("cat sub/later.env").0);
        // By name as `secret_file` says it, and not what only looks alike.
        for (name, hidden) in [
            (".env.local", true),
            ("service.env", true),
            ("password.json", true),
            ("gh-token.txt", true),
            ("api_key", true),
            ("service.env.example", false),
            ("password_reset.rs", false),
            ("tokenizer.json", false),
        ] {
            std::fs::write(ws.join("sub").join(name), "x").unwrap();
            assert_eq!(!run(&format!("cat sub/{name}")).0, hidden, "{name}");
        }
        assert!(!run("mv sub sub2").0);
        assert!(!run(&format!("mv {} \"$TMPDIR/ws\"", ws.display())).0);
        assert!(run("touch sub/a && rm sub/a").0);
        // Tools still work: git (through xcrun) and a C compiler.
        if Path::new("/Library/Developer/CommandLineTools").is_dir() {
            // Not `git init` inside: .git/config and hooks are read-only before
            // they exist.
            assert!(!run("git init -q .").0);
            assert!(!ws.join(".git/config").exists());
            let _ = std::fs::remove_dir_all(ws.join(".git"));
            let init = std::process::Command::new("git")
                .args(["init", "-q"])
                .current_dir(&ws)
                .status()
                .unwrap();
            assert!(init.success());
            assert!(run("git status --short > /dev/null").0);
            assert!(!run("echo x >> .git/config").0);
            assert!(!run("touch .git/hooks/pre-commit").0);
            assert!(run("printf 'int main(void){return 0;}' > m.c && cc m.c -o m && ./m").0);
        }
        if home.join(".cargo/bin/cargo").exists() {
            assert!(run(&format!("{}/.cargo/bin/cargo --version", home.display())).0);
        }
        // No network, and no keychain.
        assert!(!run("curl -sS -m 5 -o /dev/null https://example.com").0);
        assert!(!run("security list-keychains").0);
        if let Some((k, _)) = std::env::vars().find(|(k, _)| secret_var(k, &[])) {
            assert_eq!(run(&format!("printenv {k} || echo gone")).1, "gone");
        }
        // Its own temporary folder, kept between calls.
        assert_eq!(
            run("echo t > \"$TMPDIR/t\" && cat \"$TMPDIR/t\""),
            (true, "t".into())
        );
        assert_eq!(std::fs::read_to_string(sb.tmp.join("t")).unwrap(), "t\n");
        assert!(sb.list(&ws, &ws).unwrap().contains(&"f".to_string()));
        std::fs::remove_dir_all(&d).unwrap();
    }

    /// The real thing, where bwrap is installed.
    #[cfg(target_os = "linux")]
    #[test]
    fn inside_the_sandbox() {
        let Some(bwrap) = find_bwrap().filter(|p| p.exists()) else {
            return;
        };
        let d = std::env::temp_dir().join(format!("arakun-sbx-real-{}", std::process::id()));
        let ws = d.join("ws");
        std::fs::create_dir_all(&ws).unwrap();
        let Ok(sb) = Sandbox::with(
            Some(Runner::Bwrap(bwrap)),
            &ws,
            &d.join("session"),
            &[],
            &[],
        ) else {
            return;
        };
        // Beside the workspace, under the host's /tmp, which the sandbox doesn't see.
        let secret = d.join("secret");
        std::fs::create_dir_all(&secret).unwrap();
        std::fs::write(secret.join("token"), "s3cr3t").unwrap();
        let run = |c: &str| {
            let o = sb
                .output(
                    &ws,
                    &["sh".into(), "-c".into(), c.into()],
                    None,
                    Duration::from_secs(20),
                )
                .unwrap();
            (
                o.status.success(),
                String::from_utf8_lossy(&o.stdout).trim().to_string(),
            )
        };
        assert_eq!(run("echo hi > f && cat f"), (true, "hi".into()));
        assert_eq!(std::fs::read_to_string(ws.join("f")).unwrap(), "hi\n");
        // $HOME is an empty scratch tmpfs: nothing from it, nothing back to it.
        let home = crate::platform::home();
        let probe = home.join(format!(".arakun-sbx-probe-{}", std::process::id()));
        assert!(run(&format!("touch {}", probe.display())).0);
        assert!(!probe.exists());
        for p in [
            ".ssh",
            ".config/himalaya",
            ".local/share/opencode",
            ".modal.toml",
        ] {
            assert_eq!(
                run(&format!("test -e {}/{p} || echo gone", home.display())).1,
                "gone",
                "{p}"
            );
        }
        if home.join(".cargo/bin").is_dir() {
            assert!(!run(&format!("touch {}/.cargo/bin/x", home.display())).0);
        }
        assert!(!run("cat /etc/hostname > /dev/null && touch /etc/x").0);
        assert_eq!(run("ls /run | wc -l"), (true, "0".into()));
        assert!(!run(&format!("cat {}/token", secret.display())).0);
        // Its own processes only.
        assert!(run("ls /proc | grep -c '^[0-9]'").1.parse::<u32>().unwrap() < 8);
        // Only loopback: no network.
        assert_eq!(
            run("tail -n +3 /proc/net/dev | cut -d: -f1 | tr -d ' '").1,
            "lo"
        );
        if let Some((k, _)) = std::env::vars().find(|(k, _)| secret_var(k, &[])) {
            assert_eq!(run(&format!("printenv {k} || echo gone")).1, "gone");
        }
        assert_eq!(run("echo t > /tmp/t && cat /tmp/t"), (true, "t".into()));
        assert_eq!(
            std::fs::read_to_string(d.join("session/tmp/t")).unwrap(),
            "t\n"
        );
        std::fs::remove_dir_all(&d).unwrap();
    }

    /// This machine's own secrets, by name only: `cargo test real_secrets -- --ignored`.
    #[cfg(target_os = "linux")]
    #[test]
    #[ignore]
    fn real_secrets_are_hidden() {
        let d = std::env::temp_dir().join(format!("arakun-sbx-secrets-{}", std::process::id()));
        std::fs::create_dir_all(d.join("ws")).unwrap();
        let sb = Sandbox::new(&d.join("ws"), &d.join("session"), &[], &[]).unwrap();
        let home = crate::platform::home();
        let mut seen = Vec::new();
        for h in &sb.hidden {
            let p = h.path.display().to_string();
            let o = sb.output(&d.join("ws"), &["sh".into(), "-c".into(), "if [ -d \"$1\" ]; then [ -z \"$(ls -A \"$1\" 2>/dev/null)\" ]; else [ ! -s \"$1\" ]; fi".into(), "sh".into(), p.clone()], None, Duration::from_secs(20)).unwrap();
            if !o.status.success() {
                seen.push(p);
            }
        }
        println!("{} paths hidden under {}", sb.hidden.len(), home.display());
        std::fs::remove_dir_all(&d).unwrap();
        assert!(seen.is_empty(), "visible inside: {seen:?}");
    }

    #[test]
    fn tokens_stay_out_of_the_environment() {
        let vars = [
            "CLOUDFLARE_API_TOKEN",
            "GH_TOKEN",
            "GITHUB_TOKEN",
            "MODAL_TOKEN_ID",
            "OPENAI_API_KEY",
            "SSH_AUTH_SOCK",
            "DBUS_SESSION_BUS_ADDRESS",
            "ARAKUN_SOCKET",
            "IPC_SOCKET",
            "CLAUDE_CODE_MESSAGING_SOCKET",
            "AWS_SECRET_ACCESS_KEY",
            "XDG_RUNTIME_DIR",
            "MY_SERVICE_KEY",
            "DB_PASS",
            "SMTP_PWD",
            "DISPLAY",
            "PATH",
            "HOME",
            "LANG",
            "CARGO_HOME",
            "TMPDIR",
        ]
        .map(|k| (k.to_string(), "v".to_string()));
        let env = scrub_env(vars.clone(), &[], "/usr/bin");
        let keys: Vec<&str> = env.iter().map(|(k, _)| k.as_str()).collect();
        assert_eq!(keys, vec!["CARGO_HOME", "HOME", "LANG", "PATH", "TMPDIR"]);
        assert!(
            env.contains(&("PATH".into(), "/usr/bin".into()))
                && env.contains(&("TMPDIR".into(), "/tmp".into()))
        );
        let env = scrub_env(vars, &[Cap::Modal], "/usr/bin");
        assert!(env.iter().any(|(k, _)| k == "MODAL_TOKEN_ID"));
        assert!(!env.iter().any(|(k, _)| k == "GH_TOKEN"));
    }
}
