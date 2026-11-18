//! Where a headless agent's tools run: inside bubblewrap, with the workspace
//! writable, the rest of the machine read-only, credentials and the host's
//! sockets out of sight, and no network unless a `--allow` grants it.
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

/// A file that looks like it holds a secret, wherever it is.
fn secret_file(name: &str) -> bool {
    let n = name.to_lowercase();
    let (stem, ext) = n
        .rsplit_once('.')
        .filter(|(a, _)| !a.is_empty())
        .unwrap_or((n.as_str(), ""));
    // Words count in data files only, so password_reset.rs is code, not a secret.
    let data = [
        "", "json", "txt", "toml", "yaml", "yml", "ini", "conf", "cfg",
    ]
    .contains(&ext);
    n == ".env"
        || n.starts_with(".env.")
        || n.ends_with(".env")
        || [".pem", ".key", ".p12", ".pfx", ".keystore", ".jks", ".gpg", ".asc"].iter().any(|x| n.ends_with(x))
        || ["id_rsa", "id_ed25519", "id_ecdsa", "id_dsa"].iter().any(|k| n.starts_with(k))
        || [".netrc", ".npmrc", ".pypirc", ".git-credentials", "auth.json", "credentials", "credentials.json", "credentials.toml", ".modal.toml"].contains(&n.as_str())
        || (data && ["secret", "password", "passwd", "wallet", "keypair", "apikey", "api_key", "api-key"].iter().any(|w| stem.contains(w)))
        // token.json, gh-token.txt; not tokenizer.json.
        || (data && stem.split(['-', '_', '.']).any(|w| w == "token" || w == "tokens"))
}

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
            if t.is_dir() {
                if depth < 8 && !SKIP.contains(&name.as_str()) {
                    stack.push((e.path(), depth + 1));
                }
            } else if secret_file(&name) {
                out.push(e.path());
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
}

/// Environment variables the sandbox never gets: tokens, keys, and the
/// addresses of the host's agents, buses and display.
pub fn secret_var(name: &str, caps: &[Cap]) -> bool {
    let n = name.to_uppercase();
    if n.starts_with("MODAL_") && caps.contains(&Cap::Modal) {
        return false;
    }
    const WORDS: &[&str] = &[
        "TOKEN",
        "SECRET",
        "PASSWORD",
        "PASSWD",
        "CREDENTIAL",
        "AUTH",
        "COOKIE",
        "API_KEY",
        "APIKEY",
        "PRIVATE",
    ];
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
        "SSH_",
        "GPG_",
        "GNUPG",
        "DBUS_",
        "WAYLAND_",
        "HYPRLAND_",
        "KITTY_",
        "PULSE_",
        "PIPEWIRE_",
        "KRB5",
        "BW_",
        "OP_",
        "TAILSCALE",
    ];
    const EXACT: &[&str] = &[
        "XDG_RUNTIME_DIR",
        "DISPLAY",
        "TMUX",
        "TMUX_PANE",
        "ZELLIJ",
        "NETRC",
        "KUBECONFIG",
        "HIMALAYA_CONFIG",
        "GIT_ASKPASS",
        "SUDO_ASKPASS",
        "XDG_SESSION_COOKIE",
    ];
    WORDS.iter().any(|w| n.contains(w))
        || n.contains("SOCK")
        || n.ends_with("KEY")
        || n.ends_with("_KEYS")
        || PREFIXES.iter().any(|p| n.starts_with(p))
        || EXACT.contains(&n.as_str())
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

/// One session's sandbox, as `bwrap` arguments.
#[derive(Clone, Debug)]
pub struct Sandbox {
    pub bwrap: PathBuf,
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
    /// Bound read-only over the writable workspace (`.git/config`, `.git/hooks`).
    pub frozen: Vec<PathBuf>,
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

    /// `bwrap …` around `inner`, run in `cwd` (the workspace when outside it).
    pub fn argv(&self, cwd: &Path, inner: &[String]) -> Vec<String> {
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
        for f in &self.frozen {
            a.extend(["--ro-bind".to_string(), s(f), s(f)]);
        }
        for f in &self.extra_ro {
            a.extend(["--ro-bind".to_string(), s(f), s(f)]);
        }
        let cwd = if cwd.starts_with(&self.workspace) || cwd.starts_with("/tmp") {
            cwd
        } else {
            &self.workspace
        };
        a.extend(["--chdir".to_string(), s(cwd), "--".to_string()]);
        a.extend(inner.iter().cloned());
        a
    }

    /// The command that runs `inner` inside, with a scrubbed environment.
    pub fn command(&self, cwd: &Path, inner: &[String]) -> Command {
        let mut cmd = crate::platform::command(&self.bwrap);
        cmd.args(self.argv(cwd, inner));
        let path = crate::agent::workers_ai::child_path()
            .to_string_lossy()
            .into_owned();
        cmd.env_clear()
            .envs(scrub_env(std::env::vars(), &self.caps, &path));
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

pub fn find_bwrap() -> Option<PathBuf> {
    std::env::var_os("ARAKUN_BWRAP")
        .map(PathBuf::from)
        .or_else(|| crate::agent::which("bwrap"))
        .or_else(|| Some(PathBuf::from("/usr/bin/bwrap")).filter(|p| p.exists()))
}

impl Sandbox {
    /// The sandbox of the session whose data is in `session_dir`; it checks
    /// that bwrap starts, so a broken one is an error now, not a silent bypass.
    pub fn new(
        workspace: &Path,
        session_dir: &Path,
        caps: &[Cap],
        reads: &[PathBuf],
    ) -> Result<Self, String> {
        Self::with(find_bwrap(), workspace, session_dir, caps, reads)
    }

    fn with(
        bwrap: Option<PathBuf>,
        workspace: &Path,
        session_dir: &Path,
        caps: &[Cap],
        reads: &[PathBuf],
    ) -> Result<Self, String> {
        let bwrap = bwrap.ok_or("bubblewrap (bwrap) is not installed; headless agents run inside it. Install it (pacman -S bubblewrap / apt install bubblewrap), or pass --no-sandbox, which you must then confirm.")?;
        let workspace = crate::platform::canonicalize(workspace)
            .map_err(|e| format!("{}: {e}", workspace.display()))?;
        let tmp = session_dir.join("tmp");
        std::fs::create_dir_all(&tmp).map_err(|e| format!("{}: {e}", tmp.display()))?;
        let empty = session_dir.join("empty");
        std::fs::write(&empty, b"").map_err(|e| format!("{}: {e}", empty.display()))?;
        let home = crate::platform::home();
        let git = workspace.join(".git");
        let frozen = [git.join("config"), git.join("hooks")]
            .into_iter()
            .filter(|p| p.exists())
            .collect();
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
            bwrap,
            home,
            toolchains,
            reads,
            workspace,
            tmp,
            empty,
            caps: caps.to_vec(),
            hidden,
            frozen,
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
            .map_err(|e| format!("bubblewrap ({}) would not start: {e}", self.bwrap.display()))?;
        if out.status.success() {
            Ok(())
        } else {
            Err(format!(
                "bubblewrap ({}) would not start a sandbox: {}",
                self.bwrap.display(),
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
        let out = self.sh(
            cwd,
            "LC_ALL=C exec ls -1Ap --color=never -- \"$1\"",
            &[&s(dir)],
            None,
        )?;
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

    fn sb(caps: &[Cap]) -> Sandbox {
        Sandbox {
            bwrap: "/usr/bin/bwrap".into(),
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
            frozen: vec!["/home/u/proj/.git/hooks".into()],
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
        for n in [
            ".env",
            "prod.env",
            ".env.local",
            "id_ed25519",
            "server.pem",
            "gh-token.txt",
            "token.json",
            "my-wallet.json",
            "auth.json",
        ] {
            assert!(secret_file(n), "{n}");
        }
        for n in [
            "main.rs",
            "token.rs",
            "tokenizer.json",
            "password_reset.rs",
            "keyboard.c",
            "Cargo.toml",
        ] {
            assert!(!secret_file(n), "{n}");
        }
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
                .contains("not installed")
        );
        assert!(
            Sandbox::with(Some(d.join("no-bwrap")), &d, &d, &[], &[])
                .unwrap_err()
                .contains("would not start")
        );
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
        let Ok(sb) = Sandbox::with(Some(bwrap), &ws, &d.join("session"), &[], &[]) else {
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
