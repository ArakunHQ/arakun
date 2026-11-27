//! The sandbox an agent in an app session runs in, when its project asks for one
//! (Settings › Project › Sandbox agents): Seatbelt on macOS, bubblewrap on Linux.
//!
//! Unlike a headless run, the whole agent program is inside, so it keeps what it
//! needs to work: the network, its own login and settings, the toolchains and
//! their caches. It can write the workspace, the repository's Git folder (but not
//! its config, its hooks, or what leads git to another Git folder), those caches
//! and the temporary folders, and nothing else.
//! Credentials of other tools stay hidden, and so do the workspace's secret files.
//! It reaches arakun only through a socket that refuses commands which would run
//! something outside (the app's `control::sandbox_allows`).

use std::path::{Path, PathBuf};
use std::sync::RwLock;

use anyhow::{Result, anyhow};

use super::AgentKind;
use super::launch::LaunchSpec;
use crate::headless::sandbox::{
    self as hs, Hidden, READ, Runner, bwrap_freeze, sbpl_freeze, sbpl_list, sbpl_pin,
    sbpl_regex_escape, sbpl_secret_names, sbpl_str, sbpl_subpath,
};

/// The folders of every project and environment, as the shell last saved
/// them: whether its agents run sandboxed, the variables they get anyway, and
/// for a sandboxed one its repository's shared Git folder.
static PROJECTS: RwLock<Vec<Folder>> = RwLock::new(Vec::new());

struct Folder {
    path: PathBuf,
    sandbox: bool,
    pass: Vec<String>,
    git: Option<PathBuf>,
}

/// A project's folder and its choices for its agents.
pub struct ProjectFolder {
    pub path: PathBuf,
    /// Its agents run sandboxed.
    pub sandbox: bool,
    /// Variables its agents get even though they look like secrets
    /// (Settings › Project › Pass to agents).
    pub pass: Vec<String>,
}

/// Each folder with its project's choices.
pub fn set_projects(folders: impl IntoIterator<Item = ProjectFolder>) {
    let folders = folders
        .into_iter()
        .map(
            |ProjectFolder {
                 path: p,
                 sandbox,
                 pass,
             }| {
                let path = crate::platform::canonicalize(&p).unwrap_or(p);
                // Only a sandboxed project's repository is looked up: an untrusted
                // one elsewhere must not stop that project's sessions.
                let git = sandbox
                    .then(|| git_dirs(&path).ok().flatten().map(|(common, _)| common))
                    .flatten();
                Folder {
                    path,
                    sandbox,
                    pass,
                    git,
                }
            },
        )
        .collect();
    *PROJECTS.write().unwrap() = folders;
}

/// The variables the project of `cwd` passes to its agents. The deepest
/// folder answers, like [`wanted`].
pub fn passed(cwd: &Path) -> Vec<String> {
    let folders = PROJECTS.read().unwrap();
    if folders.iter().all(|f| f.pass.is_empty()) {
        return Vec::new();
    }
    let cwd = crate::platform::canonicalize(cwd).unwrap_or_else(|_| cwd.to_path_buf());
    folders
        .iter()
        .filter(|f| cwd.starts_with(&f.path))
        .max_by_key(|f| f.path.components().count())
        .map(|f| f.pass.clone())
        .unwrap_or_default()
}

/// The variables of arakun's own environment an agent of `kind` in `cwd`
/// doesn't get: other tools' tokens and keys, sandboxed or not. `set` are the
/// ones its launch sets itself, which stay.
pub fn scrubbed(kind: AgentKind, cwd: &Path, sandboxed: bool, set: &[String]) -> Vec<String> {
    scrub(kind, sandboxed, &passed(cwd), set)
}

fn scrub(kind: AgentKind, sandboxed: bool, pass: &[String], set: &[String]) -> Vec<String> {
    let mut gone: Vec<String> = std::env::vars_os()
        .filter_map(|(k, _)| k.into_string().ok())
        .filter(|k| {
            !set.contains(k)
                && !pass.iter().any(|p| p.eq_ignore_ascii_case(k))
                && foreign_secret(k, kind, sandboxed)
        })
        .collect();
    gone.sort();
    gone
}

/// The session in `cwd` belongs to a project with the sandbox on: `cwd` is in
/// it or one of its environments (the shell lists both), or in another
/// worktree of its repository. An error, so nothing starts, when `cwd`'s Git
/// metadata can't be trusted: unsandboxed would be the quiet way out of it.
///
/// A folder the shell knows answers with its own project's choice, so a project
/// without the sandbox never has its Git metadata checked.
pub fn wanted(cwd: &Path) -> Result<bool, String> {
    let folders = PROJECTS.read().unwrap();
    if !folders.iter().any(|f| f.sandbox) {
        return Ok(false);
    }
    let cwd = crate::platform::canonicalize(cwd).unwrap_or_else(|_| cwd.to_path_buf());
    // The deepest one: a project inside another answers for its own folders.
    if let Some(f) = folders
        .iter()
        .filter(|f| cwd.starts_with(&f.path))
        .max_by_key(|f| f.path.components().count())
    {
        return Ok(f.sandbox);
    }
    let git = git_dirs(&cwd)?.map(|(common, _)| common);
    Ok(git.is_some_and(|git| {
        folders
            .iter()
            .any(|f| f.sandbox && f.git.as_ref() == Some(&git))
    }))
}

/// How arakun's own git may run in a folder ([`git_pin`]).
#[derive(Debug, PartialEq, Eq)]
pub enum GitPin {
    /// As git finds the repository.
    Open,
    /// On this repository only: a sandboxed agent can write the folders git
    /// would otherwise follow (`.git` as a file, `commondir`), so git is told
    /// where they are instead.
    Pinned {
        git_dir: PathBuf,
        common: PathBuf,
        work_tree: PathBuf,
    },
    /// Not at all: the Git metadata leads somewhere it can't be trusted.
    Refused(String),
}

/// Where arakun's own git runs for `dir`. In a sandboxed project's folders it is
/// pinned to the repository [`git_dirs`] trusts, so a `commondir` or `.git` file
/// an agent wrote never leads it to config or hooks the agent wrote: those run
/// commands on the host, and the Changes panel runs `git status` by itself.
///
/// Elsewhere git finds the repository as usual, but for a `.git` folder that
/// holds a `commondir`: git makes one only in a linked worktree's folder, so
/// there it can only lead away from the config the sandbox kept read-only.
pub fn git_pin(dir: &Path) -> GitPin {
    // No sandbox on Windows, so nothing there writes what git follows.
    if cfg!(windows) {
        return GitPin::Open;
    }
    let Ok(real) = crate::platform::canonicalize(dir) else {
        return GitPin::Open;
    };
    // The checkout git would find: the nearest folder with a `.git` that is a
    // file, or a folder with a HEAD (a sandbox's empty stand-in is skipped).
    let Some(root) = real.ancestors().find(|a| {
        let dot = a.join(".git");
        std::fs::symlink_metadata(&dot).is_ok_and(|m| !m.is_dir() || dot.join("HEAD").exists())
    }) else {
        return GitPin::Open;
    };
    let folders = PROJECTS.read().unwrap();
    // The folder whose project answers: the deepest one the shell knows.
    let known = folders
        .iter()
        .filter(|f| real.starts_with(&f.path))
        .max_by_key(|f| f.path.components().count());
    let dirs = || git_dirs(root);
    let sandboxed = match known {
        Some(f) => f.sandbox.then_some(f.path.as_path()),
        // Another worktree of a sandboxed project's repository.
        None if folders.iter().any(|f| f.sandbox) => {
            let common = dirs().ok().flatten().map(|(c, _)| c);
            folders
                .iter()
                .find(|f| f.sandbox && common.is_some() && f.git == common)
                .map(|_| root)
        }
        None => None,
    };
    let Some(top) = sandboxed else {
        let dot = root.join(".git");
        return if dot.is_dir() && std::fs::symlink_metadata(dot.join("commondir")).is_ok() {
            GitPin::Refused(format!(
                "{}: a main checkout's Git folder holds a commondir, which only a linked \
                 worktree's has; arakun won't run git here until it is gone",
                dot.display()
            ))
        } else {
            GitPin::Open
        };
    };
    // A repository inside the sandboxed folder is the agent's to write, all of it.
    if root != top && root.starts_with(top) {
        return GitPin::Refused(format!(
            "{}: a repository inside a sandboxed folder; arakun runs git only on the folder's own",
            root.display()
        ));
    }
    match dirs() {
        Ok(Some((common, git_dir))) => GitPin::Pinned {
            git_dir,
            common,
            work_tree: root.to_path_buf(),
        },
        Ok(None) => GitPin::Open,
        Err(e) => GitPin::Refused(format!(
            "{e}. Its agents run sandboxed, so arakun won't run git where that leads."
        )),
    }
}

/// The repository's shared Git folder and this checkout's own: `.git` itself
/// for both, or for a worktree the folders its `.git` file leads to. None
/// without one; an error when `.git` leads somewhere the repository doesn't
/// list as this worktree's, or a `.git` folder is a link out of `ws`.
///
/// The agent can rewrite a `.git` file in its workspace, and the sandbox makes
/// what it leads to writable. So it counts only when Git registered it both
/// ways: the folder sits under `<common>/worktrees`, and its `gitdir` file
/// leads back here. The agent can't write those outside a repository it has.
fn git_dirs(ws: &Path) -> Result<Option<(PathBuf, PathBuf)>, String> {
    let real = |p: &Path| crate::platform::canonicalize(p).ok();
    let dot = ws.join(".git");
    let Ok(meta) = std::fs::metadata(&dot) else {
        return Ok(None);
    };
    let bad = |why: &str| Err(format!("{}: {why}", dot.display()));
    if meta.is_dir() {
        // A link to a folder elsewhere would make that folder writable.
        return match real(&dot) {
            // No repository to git either: a sandbox's stand-in for one it may not make.
            Some(d) if real(ws).is_some_and(|w| d.starts_with(w)) && !d.join("HEAD").exists() => {
                Ok(None)
            }
            Some(d) if real(ws).is_some_and(|w| d.starts_with(w)) => Ok(Some((d.clone(), d))),
            _ => bad("a link to a Git folder outside it"),
        };
    }
    let Some(text) = small_file(&dot) else {
        return bad("not a folder nor a short file");
    };
    let Some(to) = text.trim().strip_prefix("gitdir:") else {
        return bad("no gitdir line");
    };
    let Some(gitdir) = real(&ws.join(to.trim())) else {
        return bad("it leads to a folder that is not there");
    };
    let common = small_file(&gitdir.join("commondir")).and_then(|c| real(&gitdir.join(c.trim())));
    // Git writes it absolute; a relative one is relative to `gitdir`, not to us.
    let back = small_file(&gitdir.join("gitdir")).and_then(|b| real(&gitdir.join(b.trim())));
    match common {
        Some(common)
            if gitdir.parent() == Some(common.join("worktrees").as_path())
                && back.is_some_and(|b| Some(b) == real(&dot)) =>
        {
            Ok(Some((common, gitdir)))
        }
        _ => bad("its repository does not list this folder (git worktree repair fixes that)"),
    }
}

/// A Git metadata file, read only when it is a regular file and short: a pipe
/// in its place would block the window, which calls this on save.
fn small_file(p: &Path) -> Option<String> {
    use std::io::Read;
    let mut opts = std::fs::OpenOptions::new();
    opts.read(true);
    // Opening a pipe for reading waits for a writer; this doesn't.
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::custom_flags(&mut opts, libc::O_NONBLOCK);
    let f = opts.open(p).ok()?;
    if !f.metadata().ok()?.is_file() {
        return None;
    }
    let mut text = String::new();
    f.take(4096).read_to_string(&mut text).ok()?;
    Some(text)
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

/// Claude Code's folder: `CLAUDE_CONFIG_DIR`, or `~/.claude`.
fn claude_dir(home: &Path) -> PathBuf {
    std::env::var_os("CLAUDE_CONFIG_DIR")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .unwrap_or_else(|| home.join(".claude"))
}

/// Codex's folder: `CODEX_HOME`, or `~/.codex`.
fn codex_dir(home: &Path) -> PathBuf {
    std::env::var_os("CODEX_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .unwrap_or_else(|| home.join(".codex"))
}

/// The agent's own settings that run commands (hooks, MCP servers, notify
/// programs): read-only, so they can't run something outside later. Also when
/// they are not there yet, or the agent could create them.
fn own_frozen(kind: AgentKind, home: &Path) -> Vec<PathBuf> {
    match kind {
        AgentKind::Claude => {
            let dir = claude_dir(home);
            vec![dir.join("settings.json"), dir.join("hooks")]
        }
        AgentKind::Codex => vec![codex_dir(home).join("config.toml")],
        AgentKind::OpenCode => vec![home.join(".config/opencode"), home.join("opencode.json")],
        AgentKind::Droid => vec![
            home.join(".factory/settings.json"),
            home.join(".factory/hooks"),
        ],
        AgentKind::Devin => vec![home.join(".config/devin")],
        // MCP servers and hooks, which run commands.
        AgentKind::Cursor => vec![
            home.join(".cursor/mcp.json"),
            home.join(".cursor/hooks.json"),
            home.join(".cursor/hooks"),
        ],
        // MCP servers: agy's own file, and the Gemini settings it shares.
        AgentKind::Antigravity => vec![
            home.join(".gemini/config/mcp_config.json"),
            home.join(".gemini/settings.json"),
        ],
        AgentKind::WorkersAi => Vec::new(),
    }
}

/// Makes a read-only path that isn't there yet, so bubblewrap has something to
/// bind over it: an empty settings file, or a folder.
fn materialize(p: &Path) -> std::io::Result<()> {
    if p.exists() {
        return Ok(());
    }
    match p.extension().and_then(|e| e.to_str()) {
        Some("json") => std::fs::write(p, "{}\n"),
        Some(_) => std::fs::write(p, ""),
        None => std::fs::create_dir_all(p),
    }
}

/// Copies `from` to `to` only if nothing is at `to`, not even a dangling link:
/// the agent may have left one there to make arakun write somewhere else. The
/// copy is the user's alone (0600), as Claude Code keeps it.
fn seed(from: &Path, to: &Path) -> std::io::Result<()> {
    use std::io::Write as _;
    // Already there: no need to read the source. `create_new` below still covers
    // one made after this check.
    if std::fs::symlink_metadata(to).is_ok() {
        return Ok(());
    }
    let bytes = match std::fs::read(from) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e),
    };
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut opts, 0o600);
    match opts.open(to) {
        Ok(mut f) => f.write_all(&bytes),
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => Ok(()),
        Err(e) => Err(e),
    }
}

/// Removes a folder when dropped, unless it was taken back (`.0 = None`).
struct RemoveOnDrop(Option<PathBuf>);

impl Drop for RemoveOnDrop {
    fn drop(&mut self) {
        if let Some(d) = &self.0 {
            let _ = std::fs::remove_dir_all(d);
        }
    }
}

/// Session folders made in this run, so two sessions never share a `/tmp`.
static SESSIONS: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

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
        // Its sign-in is in ~/.cursor on macOS, ~/.config/cursor elsewhere.
        AgentKind::Cursor => &[".cursor", ".config/cursor", ".local/share/cursor-agent"],
        // agy keeps its sign-in, settings and conversations under ~/.gemini.
        AgentKind::Antigravity => &[".gemini"],
        AgentKind::WorkersAi => &[],
    };
    let mut out: Vec<PathBuf> = rel.iter().map(|r| home.join(r)).collect();
    match kind {
        AgentKind::Claude => out.push(claude_dir(home)),
        AgentKind::Codex => out.push(codex_dir(home)),
        _ => {}
    }
    if kind == AgentKind::Devin
        && let Some(dir) = super::launch::devin_credentials_path().parent()
    {
        out.push(dir.to_path_buf());
    }
    out
}

/// Inherited variables that look like secrets but which agents need: model
/// providers' keys and settings, and arakun's own.
const AGENT_VARS: &[&str] = &[
    "ANTHROPIC_",
    "CLAUDE_",
    "OPENAI_",
    "AZURE_OPENAI_",
    "CODEX_",
    "OPENCODE_",
    "OPENROUTER_",
    "GEMINI_",
    "GOOGLE_",
    "VERTEX",
    "CLOUD_ML_",
    "FACTORY_",
    "DEVIN_",
    "WINDSURF_",
    "GROQ_",
    "MISTRAL_",
    "DEEPSEEK_",
    "XAI_",
    "ARAKUN_",
];

/// An inherited variable the agent doesn't get: a token or key of another tool,
/// and in the sandbox the address of a host socket too. Outside it the agent can
/// reach those sockets anyway, and `gh`'s keyring login, `git push` over SSH and
/// a browser sign-in need them. AWS keys stay for Bedrock, which Claude Code
/// and OpenCode can use.
fn foreign_secret(name: &str, kind: AgentKind, sandboxed: bool) -> bool {
    let n = name.to_uppercase();
    if AGENT_VARS.iter().any(|p| n.starts_with(p)) {
        return false;
    }
    if n.starts_with("AWS_")
        && (kind == AgentKind::OpenCode
            || kind == AgentKind::Claude && std::env::var_os("CLAUDE_CODE_USE_BEDROCK").is_some())
    {
        return false;
    }
    hs::secret_var(name, &[]) && (sandboxed || !hs::host_address(name))
}

/// One agent's sandbox in an app session. Its folder goes when it does.
#[derive(Debug)]
pub struct Session {
    pub runner: Runner,
    pub kind: AgentKind,
    /// The session's own folder: `tmp` and `empty`.
    pub dir: PathBuf,
    /// Set for the agent inside (`CLAUDE_CONFIG_DIR` under bubblewrap).
    pub env: Vec<(String, std::ffi::OsString)>,
    pub home: PathBuf,
    pub workspace: PathBuf,
    /// Writable besides the workspace: the Git folder, caches, the agent's own.
    pub writable: Vec<PathBuf>,
    /// Shown again, read-only, after the hide list: the adapters and the shim.
    pub shown: Vec<PathBuf>,
    /// Shown again and writable after the hide list: the memory store, `tmp`.
    pub shown_rw: Vec<PathBuf>,
    pub hidden: Vec<Hidden>,
    /// Read-only even inside what is writable: the Git metadata
    /// ([`hs::git_freeze`]) and the agent's own settings that run commands.
    pub frozen: Vec<PathBuf>,
    /// Writable again inside a frozen folder, or bound over itself so it can't
    /// be renamed: the checkout's own Git folder.
    pub thawed: Vec<PathBuf>,
    /// Read-only by name only (Seatbelt): the shared Git folder's `commondir`,
    /// which must stay absent; bubblewrap has nothing to bind over it, and an
    /// empty one would break git.
    pub absent: Vec<PathBuf>,
    /// The session's TMPDIR.
    pub tmp: PathBuf,
    /// An empty file, bound over hidden files (bubblewrap).
    pub empty: PathBuf,
    /// The control socket for sandboxed agents.
    pub socket: PathBuf,
    /// Read-only extras outside (a resolv.conf under /run).
    pub extra_ro: Vec<PathBuf>,
    /// Variables the project passes to its agents ([`passed`]).
    pub pass: Vec<String>,
}

impl Drop for Session {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
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
        let real = |p: PathBuf| crate::platform::canonicalize(&p).unwrap_or(p);
        // Seatbelt matches real paths: a deny on a linked $HOME would match nothing.
        let home = real(crate::platform::home());
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
        let git = git_dirs(&workspace).map_err(|e| {
            anyhow!("{e}. The sandbox won't open a Git folder it can't trust; turn off Sandbox agents in Settings › Project to run this agent without it.")
        })?;
        if let Some((g, _)) = &git
            && !g.starts_with(&workspace)
        {
            writable.push(g.clone());
        }
        let (git_frozen, thawed) = hs::git_freeze(
            &workspace,
            git.as_ref().map(|(g, o)| (g.as_path(), o.as_path())),
        );
        let absent = git.iter().map(|(g, _)| g.join("commondir")).collect();
        // Only where the agent could write it: its folder is there.
        let settings: Vec<PathBuf> = own_frozen(kind, &home)
            .into_iter()
            .filter(|p| p.parent().is_some_and(Path::is_dir))
            .collect();
        if matches!(runner, Runner::Bwrap(_)) {
            // bubblewrap binds over a path, so it must be there; without that, no
            // sandbox rather than a hole in it.
            let made = git_frozen
                .iter()
                .map(|(p, kind)| (p, hs::stand_in(p, *kind)))
                .chain(settings.iter().map(|p| (p, materialize(p))));
            for (p, r) in made {
                r.map_err(|e| {
                    anyhow!("{}: {e}; the sandbox can't keep it read-only", p.display())
                })?;
            }
        }
        let frozen: Vec<PathBuf> = git_frozen
            .into_iter()
            .map(|(p, _)| p)
            .chain(settings)
            .map(real)
            .collect();
        let thawed: Vec<PathBuf> = thawed.into_iter().map(real).collect();
        for p in CACHES.iter().map(|r| home.join(r)).chain(mine) {
            if p.exists() {
                writable.push(real(p));
            }
        }
        // Not under the control socket's folder, which a sandbox is shown.
        let dir = crate::paths::shared(crate::paths::SANDBOXES).join(format!(
            "{}-{}-{}",
            kind.id(),
            std::process::id(),
            SESSIONS.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        // Until the Session holds it, a failure on the way removes the folder.
        let mut made = RemoveOnDrop(Some(dir.clone()));
        let tmp = dir.join("tmp");
        std::fs::create_dir_all(&tmp).map_err(|e| anyhow!("{}: {e}", tmp.display()))?;
        let tmp = real(tmp);
        let empty = dir.join("empty");
        std::fs::write(&empty, b"").map_err(|e| anyhow!("{}: {e}", empty.display()))?;
        // Seatbelt matches real paths (/private/tmp, not /tmp); the socket may not
        // be there yet, its folder is.
        let socket = (crate::host::get().sandbox_socket)();
        if let Some(dir) = socket.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let socket = match (socket.parent(), socket.file_name()) {
            (Some(dir), Some(name)) => real(dir.to_path_buf()).join(name),
            _ => socket,
        };
        let mut shown = vec![super::launch::agents_dir()];
        if let Some(bin) = (crate::host::get().shim_dir)() {
            shown.push(bin);
        }
        // The shim leads to arakun itself, which may sit in a hidden folder.
        if let Ok(exe) = std::env::current_exe() {
            shown.push(exe);
        }
        let shown = shown.into_iter().filter(|p| p.exists()).map(real).collect();
        let memory = (crate::host::get().memory_root)();
        let _ = std::fs::create_dir_all(&memory);
        // Both sit in arakun's own folder, which the hide list has.
        let shown_rw = vec![real(memory), tmp.clone()];
        let mut extra_ro = Vec::new();
        if let Ok(r) = crate::platform::canonicalize("/etc/resolv.conf")
            && r.starts_with("/run")
        {
            extra_ro.push(r);
        }
        let mut env = Vec::new();
        let claude = claude_dir(&home);
        // Claude Code saves ~/.claude.json through a lock and a file beside it,
        // which a read-only $HOME refuses; with CLAUDE_CONFIG_DIR it keeps them in
        // its folder. It starts from a copy of the one outside.
        if kind == AgentKind::Claude
            && matches!(runner, Runner::Bwrap(_))
            && std::env::var_os("CLAUDE_CONFIG_DIR").is_none()
            && claude.is_dir()
        {
            let inside = claude.join(".claude.json");
            seed(&home.join(".claude.json"), &inside)
                .map_err(|e| anyhow!("{}: {e}", inside.display()))?;
            env.push(("CLAUDE_CONFIG_DIR".into(), claude.into_os_string()));
        }
        made.0 = None;
        Ok(Session {
            runner,
            kind,
            dir,
            env,
            home,
            workspace,
            writable,
            shown,
            shown_rw,
            hidden,
            frozen,
            thawed,
            absent,
            tmp,
            empty,
            socket,
            extra_ro,
            pass: passed(cwd),
        })
    }

    /// `spec`, run inside: the runner first, then the agent's own command.
    pub fn wrap(&self, spec: LaunchSpec) -> LaunchSpec {
        // The agent inherits arakun's environment; `env` drops other tools'
        // credentials from it first (GH_TOKEN, a cloud key), inside.
        let mut inner = vec!["/usr/bin/env".to_string()];
        let set: Vec<String> = spec.env.iter().map(|(k, _)| k.clone()).collect();
        for k in scrub(self.kind, true, &self.pass, &set) {
            inner.extend(["-u".to_string(), k]);
        }
        inner.push(spec.program.to_string_lossy().into_owned());
        inner.extend(spec.args);
        let mut args = match &self.runner {
            Runner::Bwrap(_) => self.bwrap_args(),
            Runner::Seatbelt(_) => vec!["-p".into(), self.seatbelt_profile()],
        };
        args.extend(inner);
        let mut env = spec.env;
        env.extend(self.inside_env());
        LaunchSpec {
            program: self.runner.program().to_path_buf(),
            args,
            env,
            unset: spec.unset,
        }
    }

    /// `script` run by `sh` inside, as the agent would run it.
    pub fn shell_command(&self, script: &str) -> std::process::Command {
        let spec = self.wrap(LaunchSpec {
            program: "/bin/sh".into(),
            args: vec!["-c".into(), script.into()],
            env: Vec::new(),
            unset: Vec::new(),
        });
        let mut cmd = crate::platform::command(&spec.program);
        cmd.args(spec.args).envs(spec.env);
        cmd
    }

    fn inside_env(&self) -> Vec<(String, std::ffi::OsString)> {
        let mut env = vec![
            ("TMPDIR".to_string(), self.tmp.clone().into_os_string()),
            ("ARAKUN_SOCKET".into(), self.socket.clone().into_os_string()),
            ("ARAKUN_SANDBOX".into(), "1".into()),
        ];
        env.extend(self.env.iter().cloned());
        env
    }

    /// A short command run inside, in the workspace.
    fn command(&self, inner: &[&str]) -> std::process::Command {
        let mut cmd = crate::platform::command(self.runner.program());
        match &self.runner {
            Runner::Bwrap(_) => cmd.args(self.bwrap_args()),
            Runner::Seatbelt(_) => cmd.args(["-p".to_string(), self.seatbelt_profile()]),
        };
        cmd.args(inner)
            .current_dir(&self.workspace)
            .envs(self.inside_env());
        cmd
    }

    fn run(&self, script: &str, args: &[&str], input: Option<&[u8]>) -> Result<Vec<u8>> {
        let mut inner = vec!["sh", "-c", script, "sh"];
        inner.extend(args);
        let out = hs::run_with_timeout(
            self.command(&inner),
            input,
            std::time::Duration::from_secs(60),
        )
        .map_err(|e| anyhow!("the sandbox did not run it: {e}"))?;
        if out.status.success() {
            Ok(out.stdout)
        } else {
            Err(anyhow!(
                "{}",
                String::from_utf8_lossy(&out.stderr)
                    .trim()
                    .replace("sh: 1: ", "")
            ))
        }
    }

    /// Up to `max` bytes of `path`, read from inside: a link the agent swaps in
    /// leads only where the agent itself could read.
    pub fn read(&self, path: &Path, max: usize) -> Result<Vec<u8>> {
        let script = "[ -f \"$1\" ] || { echo \"$1 is not a regular file\" >&2; exit 3; }; \
                      exec head -c \"$2\" -- \"$1\"";
        self.run(script, &[&path.to_string_lossy(), &max.to_string()], None)
    }

    /// Writes `content` to `path` from inside, its folders too.
    pub fn write(&self, path: &Path, content: &[u8]) -> Result<()> {
        let script = "mkdir -p -- \"$(dirname -- \"$1\")\" && cat > \"$1\"";
        self.run(script, &[&path.to_string_lossy()], Some(content))
            .map(drop)
    }

    fn bwrap_args(&self) -> Vec<String> {
        let s = |p: &Path| p.to_string_lossy().into_owned();
        let mut a: Vec<String> = [
            "--die-with-parent",
            "--new-session",
            // Its own processes only: /proc of the host's would show their
            // environments, tokens and all.
            "--unshare-pid",
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
            "--unsetenv",
            "SSH_AUTH_SOCK",
        ]
        .map(String::from)
        .to_vec();
        // Its own /tmp: the host's holds tmux's, ssh-agent's and X's sockets.
        a.extend(["--bind".into(), s(&self.tmp), "/tmp".into()]);
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
        a.extend(bwrap_freeze(&self.frozen, &self.thawed));
        for p in &self.extra_ro {
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
                sbpl_regex_escape(&prefix.to_string_lossy())
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
        out.push_str(&sbpl_secret_names(&self.workspace));
        // The workspace's Git config and hooks also when they are not there yet:
        // a deny names a path, so `git init` can't plant what the user's git
        // runs, nor a `commondir` that leads it to config elsewhere.
        let git = self.workspace.join(".git");
        let git_own = [git.join("config"), git.join("hooks"), git.join("commondir")];
        let mut frozen = paths(&self.frozen);
        frozen.extend(git_own.iter().map(PathBuf::as_path));
        frozen.extend(paths(&self.absent));
        frozen.sort();
        frozen.dedup();
        out.push_str(&sbpl_freeze(&frozen, &paths(&self.thawed)));
        // Folders above a hidden or read-only path can't be renamed out from under
        // its rule (`mv ~/.claude ~/.cache/c` to edit its settings).
        let roots: Vec<&Path> = writable
            .iter()
            .copied()
            .chain(paths(&self.shown_rw))
            .collect();
        let pinned: Vec<&Path> = hidden.iter().copied().chain(frozen).collect();
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
                .is_some_and(|n| crate::secrets::secret_file(&n.to_string_lossy()))
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

#[cfg(test)]
mod tests {
    use super::*;

    /// The tests that set the project list, one at a time: it is the process's.
    static PROJECTS_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn projects_lock() -> std::sync::MutexGuard<'static, ()> {
        PROJECTS_LOCK.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn folder(path: &Path, sandbox: bool) -> ProjectFolder {
        ProjectFolder {
            path: path.to_path_buf(),
            sandbox,
            pass: Vec::new(),
        }
    }

    /// `script` run by `sh` in `dir` on the host, with git as a test sets it up.
    #[cfg(unix)]
    fn host_sh(dir: &Path, script: &str) -> bool {
        crate::git::unpinned(&mut std::process::Command::new("sh"))
            .args(["-c", script])
            .current_dir(dir)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_AUTHOR_NAME", "t")
            .env("GIT_AUTHOR_EMAIL", "t@t")
            .env("GIT_COMMITTER_NAME", "t")
            .env("GIT_COMMITTER_EMAIL", "t@t")
            .status()
            .is_ok_and(|s| s.success())
    }

    /// A repository at `<d>/proj` with a linked worktree at `<d>/env-a`, and
    /// the agent's Git folder at `<d>/proj/evil`: its own objects and refs (the
    /// repository's), and a config whose fsmonitor and hook touch `<d>/MARKER`.
    #[cfg(unix)]
    fn repo_with_evil(d: &Path) -> Option<(PathBuf, PathBuf, PathBuf)> {
        let proj = d.join("proj");
        std::fs::create_dir_all(&proj).unwrap();
        if !host_sh(&proj, "git --version > /dev/null") {
            return None;
        }
        assert!(host_sh(
            &proj,
            "git init -q -b main && git commit -q --allow-empty -m one \
             && git worktree add -q -b env-a ../env-a",
        ));
        let marker = d.join("MARKER");
        let planted = host_sh(
            &proj,
            &format!(
                "mkdir -p evil/hooks && ln -s ../.git/objects evil/objects \
                 && ln -s ../.git/refs evil/refs \
                 && printf '#!/bin/sh\\ntouch {m}\\n' > x.sh && chmod +x x.sh \
                 && cp x.sh evil/hooks/post-index-change \
                 && printf '[core]\\n\\trepositoryformatversion = 0\\n\\tfsmonitor = {x}\\n' > evil/config",
                m = marker.display(),
                x = proj.join("x.sh").display(),
            ),
        );
        assert!(planted);
        Some((proj, d.join("env-a"), marker))
    }

    /// GHSA-j7mx-57q4-r2g5: a `commondir` the agent wrote led the app's own
    /// `git status` to the agent's config, which ran its command on the host.
    #[cfg(unix)]
    #[test]
    fn the_hosts_git_never_follows_what_an_agent_wrote() {
        let _one = projects_lock();
        let d = crate::platform::canonicalize(std::env::temp_dir())
            .unwrap()
            .join(format!("arakun-sbx-pin-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        let _rm = RemoveOnDrop(Some(d.clone()));
        let Some((proj, env_a, marker)) = repo_with_evil(&d) else {
            return;
        };
        let fired = || std::fs::remove_file(&marker).is_ok();
        // The main checkout's `commondir`: git as the user runs it falls for it.
        std::fs::write(proj.join(".git/commondir"), "../evil\n").unwrap();
        assert!(host_sh(&proj, "git status --porcelain=v2 > /dev/null"));
        assert!(fired(), "the planted fsmonitor should run for unpinned git");
        // Sandboxed, arakun's git stays on the repository and runs nothing.
        set_projects([folder(&proj, true), folder(&env_a, true)]);
        std::fs::write(proj.join("new.txt"), "x").unwrap();
        let changes = crate::git::status(&proj).unwrap();
        assert!(changes.iter().any(|c| c.path == "new.txt"));
        assert!(crate::git::diff(&proj, "new.txt", true).is_ok());
        assert!(!fired());
        // Not sandboxed: a main checkout's `commondir` stops git, as it was not git's.
        set_projects([folder(&proj, false)]);
        let e = crate::git::status(&proj).unwrap_err().to_string();
        assert!(e.contains("commondir"), "{e}");
        assert!(!fired());
        std::fs::remove_file(proj.join(".git/commondir")).unwrap();
        // A linked worktree's `commondir`, led away: it no longer belongs to
        // the repository, so git does not run there at all.
        let own = proj.join(".git/worktrees/env-a/commondir");
        std::fs::write(&own, "../../../evil\n").unwrap();
        assert!(host_sh(&env_a, "git status --porcelain=v2 > /dev/null"));
        assert!(fired(), "the planted fsmonitor should run for unpinned git");
        set_projects([folder(&proj, true), folder(&env_a, true)]);
        assert!(crate::git::status(&env_a).is_err());
        assert!(!fired());
        std::fs::write(&own, "../..\n").unwrap();
        assert!(crate::git::status(&env_a).is_ok());
        // A worktree the shell doesn't list, of the same repository, is pinned too.
        set_projects([folder(&proj, true)]);
        assert!(matches!(git_pin(&env_a), GitPin::Pinned { .. }));
        // A repository the agent made inside, with a filter in its own config
        // that `git status` would run to see whether it is dirty.
        assert!(host_sh(
            &proj,
            &format!(
                "mkdir sub && cd sub && git init -q && echo a > f && git add f \
                 && git commit -q -m s && cd .. && git add sub && git commit -q -m sub \
                 && printf '[filter \"x\"]\\n\\tclean = \"{x}; cat\"\\n' >> sub/.git/config \
                 && echo '* filter=x' > sub/.gitattributes && sleep 1 && echo b > sub/f",
                x = proj.join("x.sh").display(),
            ),
        ));
        assert!(host_sh(&proj, "git status --porcelain=v2 > /dev/null"));
        assert!(
            fired(),
            "the nested repository's filter should run for plain git"
        );
        assert!(crate::git::status(&proj).is_ok());
        assert!(crate::git::diff(&proj, "sub", false).is_ok());
        assert!(!fired());
        // Inside the sandboxed folder, that repository is the agent's alone.
        assert!(matches!(git_pin(&proj.join("sub")), GitPin::Refused(_)));
        assert!(crate::git::status(&proj.join("sub")).is_err());
        assert!(!fired());
        set_projects([]);
        assert_eq!(git_pin(&proj.join("sub")), GitPin::Open);
    }

    /// What leads git elsewhere stays put inside the real sandbox, and the agent
    /// still commits.
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[test]
    fn git_pointers_stay_put_inside() {
        if hs::find_runner().is_none_or(|r| !r.program().exists()) {
            return;
        }
        let d = crate::platform::canonicalize(std::env::temp_dir())
            .unwrap()
            .join(format!("arakun-sbx-ptr-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        let _rm = RemoveOnDrop(Some(d.clone()));
        let Some((proj, env_a, _)) = repo_with_evil(&d) else {
            return;
        };
        let commit = "git -c user.name=t -c user.email=t@t -c commit.gpgsign=false commit -q --allow-empty -m inside";
        let Ok(s) = Session::new(AgentKind::Codex, &proj) else {
            return;
        };
        // A runner that can't start here (bubblewrap without user namespaces in CI).
        if s.run("true", &[], None).is_err() {
            return;
        }
        if let Err(e) = s.run(commit, &[], None) {
            panic!("the agent can't commit: {e}");
        }
        for script in [
            "printf x >> .git/config",
            "touch .git/hooks/pre-commit",
            "mv .git .git2",
            "mkdir .git/worktrees/evil",
            "echo ../../../evil > .git/worktrees/env-a/commondir",
            "echo x >> .git/worktrees/env-a/config.worktree",
        ] {
            assert!(s.run(script, &[], None).is_err(), "{script}");
        }
        // bubblewrap can't hold a stand-in for a file git must not find; Seatbelt names it.
        if matches!(s.runner, Runner::Seatbelt(_)) {
            assert!(s.run("echo ../evil > .git/commondir", &[], None).is_err());
        }
        assert!(proj.join(".git/config").is_file());
        drop(s);
        let Ok(s) = Session::new(AgentKind::Codex, &env_a) else {
            panic!("no sandbox in the worktree");
        };
        if let Err(e) = s.run(commit, &[], None) {
            panic!("the agent can't commit: {e}");
        }
        let ws = env_a.display();
        let common = crate::platform::canonicalize(proj.join(".git")).unwrap();
        let c = common.display();
        for script in [
            format!("printf 'gitdir: {ws}/evil' > .git"),
            "mv .git git".to_string(),
            format!("echo ../../../evil > {c}/worktrees/env-a/commondir"),
            format!("echo x > {c}/worktrees/env-a/gitdir"),
            format!("mv {c}/worktrees/env-a {c}/worktrees/b"),
            format!("echo ../evil > {c}/commondir"),
            format!("printf x >> {c}/config"),
        ] {
            if script.ends_with("/commondir")
                && script.starts_with("echo ../evil")
                && matches!(s.runner, Runner::Bwrap(_))
            {
                continue;
            }
            assert!(s.run(&script, &[], None).is_err(), "{script}");
        }
    }

    fn session(runner: Runner) -> Session {
        Session {
            runner,
            kind: AgentKind::Claude,
            dir: "/nonexistent/arakun-sandbox-test".into(),
            env: Vec::new(),
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
                "/home/u/proj/.git/worktrees".into(),
                "/home/u/proj/.git/worktrees/env-a/config.worktree".into(),
                "/home/u/ws/env-a/.git".into(),
                "/home/u/proj/.git/worktrees/env-a/commondir".into(),
                "/home/u/proj/.git/worktrees/env-a/gitdir".into(),
            ],
            thawed: vec!["/home/u/proj/.git/worktrees/env-a".into()],
            absent: vec!["/home/u/proj/.git/commondir".into()],
            tmp: "/home/u/.local/share/arakun/sandbox/claude-1/tmp".into(),
            empty: "/home/u/.local/share/arakun/sandbox/claude-1/empty".into(),
            socket: "/run/user/1/arakun/sandbox/control.sock".into(),
            extra_ro: Vec::new(),
            pass: Vec::new(),
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
        let freeze = at("(deny file-write* (subpath \"/home/u/proj/.git/commondir\")");
        assert!(write < hide && hide < show && show < freeze);
        // What leads git elsewhere stays put: this checkout's `.git` file and its
        // folder's pointers, the other checkouts' folders. Its own folder is
        // writable again, but for those pointers.
        for p in [
            "/home/u/proj/.git/config",
            "/home/u/proj/.git/worktrees",
            "/home/u/ws/env-a/.git",
            "/home/u/ws/env-a/.git/commondir",
        ] {
            assert!(at(&format!("(subpath \"{p}\")")) > freeze, "{p}");
        }
        let thaw = at("(allow file-write* (subpath \"/home/u/proj/.git/worktrees/env-a\"))");
        let again =
            at("(deny file-write* (subpath \"/home/u/proj/.git/worktrees/env-a/commondir\")");
        assert!(freeze < thaw && thaw < again);
        assert!(p[again..].contains("(subpath \"/home/u/proj/.git/worktrees/env-a/gitdir\")"));
        // The workspace's own, before `git init` makes them, and later secrets.
        assert!(p.contains("(subpath \"/home/u/ws/env-a/.git/hooks\")"));
        assert!(p.contains("(regex #\"^/home/u/ws/env-a/(.*/)?\\.env(\\.[^/]*)?$\")"));
        assert!(p.contains("(regex #\"^/home/u/ws/env-a/(.*/)?[^/]*\\.env$\")"));
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
        // A folder before what is inside it: the other checkouts' Git folders
        // read-only, this one's writable again, its pointers read-only again.
        let others = at(&[
            "--ro-bind",
            "/home/u/proj/.git/worktrees",
            "/home/u/proj/.git/worktrees",
        ]);
        let own = at(&[
            "--bind",
            "/home/u/proj/.git/worktrees/env-a",
            "/home/u/proj/.git/worktrees/env-a",
        ]);
        let back = at(&["--ro-bind", "/home/u/proj/.git/worktrees/env-a/commondir"]);
        assert!(others < own && own < back);
        at(&[
            "--ro-bind",
            "/home/u/ws/env-a/.git",
            "/home/u/ws/env-a/.git",
        ]);
        // `commondir` is not there, and an empty one would break git: only Seatbelt names it.
        assert!(!a.iter().any(|x| x == "/home/u/proj/.git/commondir"));
        assert!(at(&["--bind", "/run/user/1/arakun/sandbox"]) > at(&["--tmpfs", "/run"]));
        // Its own /tmp, not the host's with tmux's and ssh-agent's sockets.
        at(&[
            "--bind",
            "/home/u/.local/share/arakun/sandbox/claude-1/tmp",
            "/tmp",
        ]);
        assert!(!a.windows(3).any(|w| w == ["--bind", "/tmp", "/tmp"]));
        at(&["--unsetenv", "SSH_AUTH_SOCK"]);
        assert_eq!(a.last().unwrap(), "--");
    }

    #[test]
    fn other_tools_credentials_stay_out() {
        // Sandboxed or not, the same tokens and keys go.
        for sandboxed in [true, false] {
            let gone = |name: &str, kind| foreign_secret(name, kind, sandboxed);
            assert!(gone("GH_TOKEN", AgentKind::Claude));
            assert!(gone("GITHUB_TOKEN", AgentKind::Codex));
            assert!(gone("STRIPE_SECRET_KEY", AgentKind::OpenCode));
            assert!(gone("CLOUDFLARE_API_TOKEN", AgentKind::Claude));
            assert!(gone("AWS_SECRET_ACCESS_KEY", AgentKind::Codex));
            assert!(gone("DB_PASS", AgentKind::Claude));
            assert!(gone("SSH_PRIVATE_KEY", AgentKind::Claude));
            assert!(gone("NETRC", AgentKind::Claude));
            assert!(!gone("AWS_SECRET_ACCESS_KEY", AgentKind::OpenCode));
            assert!(!gone("ANTHROPIC_API_KEY", AgentKind::Claude));
            assert!(!gone("CLAUDE_CODE_OAUTH_TOKEN", AgentKind::Claude));
            assert!(!gone("OPENAI_API_KEY", AgentKind::Codex));
            assert!(!gone("ARAKUN_SOCKET", AgentKind::Claude));
            assert!(!gone("PATH", AgentKind::Claude));
            assert!(!gone("HOME", AgentKind::Claude));
        }
        // The addresses of host sockets and the display go only with the
        // sockets the sandbox hides.
        for name in [
            "SSH_AUTH_SOCK",
            "DBUS_SESSION_BUS_ADDRESS",
            "DISPLAY",
            "WAYLAND_DISPLAY",
            "XAUTHORITY",
            "XDG_RUNTIME_DIR",
            "TMUX",
        ] {
            assert!(foreign_secret(name, AgentKind::Claude, true), "{name}");
            assert!(!foreign_secret(name, AgentKind::Claude, false), "{name}");
        }
        let s = session(Runner::Seatbelt("/usr/bin/sandbox-exec".into()));
        let spec = s.wrap(LaunchSpec {
            program: "/bin/agent".into(),
            args: vec!["--acp".into()],
            env: Vec::new(),
            unset: Vec::new(),
        });
        let at = spec.args.iter().position(|a| a == "/usr/bin/env").unwrap();
        assert_eq!(spec.args[spec.args.len() - 2..], ["/bin/agent", "--acp"]);
        assert!(spec.args[at..].iter().all(|a| a != "PATH"));
    }

    #[test]
    fn a_project_passes_what_it_names() {
        let _g = projects_lock();
        // A name no other test uses: the environment is the process's.
        let name = "ARAKUN147_TEST_TOKEN";
        // SAFETY: no other thread reads or writes this variable.
        unsafe { std::env::set_var(name, "x") };
        let proj = std::env::temp_dir().join("arakun-sbx-pass");
        let env_a = proj.join("env-a");
        set_projects([
            ProjectFolder {
                path: proj.clone(),
                sandbox: false,
                pass: vec!["arakun147_test_token".into()],
            },
            folder(&env_a, false),
        ]);
        assert_eq!(passed(&proj.join("src")), ["arakun147_test_token"]);
        // The environment's own folder answers for itself, with nothing passed.
        assert!(passed(&env_a).is_empty());
        for sandboxed in [true, false] {
            let gone = |cwd: &Path| scrubbed(AgentKind::Claude, cwd, sandboxed, &[]);
            assert!(!gone(&proj).iter().any(|k| k == name));
            assert!(gone(&env_a).iter().any(|k| k == name));
            // One the launch sets itself stays.
            let set = [name.to_string()];
            assert!(!scrubbed(AgentKind::Claude, &env_a, sandboxed, &set).contains(&set[0]));
        }
        set_projects([]);
        unsafe { std::env::remove_var(name) };
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

    /// The real thing: arakun's reads and writes for the agent go through it,
    /// so a link the agent swaps in leads nowhere it couldn't go itself.
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[test]
    fn fs_requests_run_inside() {
        if hs::find_runner().is_none_or(|r| !r.program().exists()) {
            return;
        }
        let d = crate::platform::canonicalize(std::env::temp_dir())
            .unwrap()
            .join(format!("arakun-sbx-fs-{}", std::process::id()));
        let _rm = RemoveOnDrop(Some(d.clone()));
        let ws = d.join("ws");
        std::fs::create_dir_all(&ws).unwrap();
        std::fs::write(ws.join(".env"), "KEY=1").unwrap();
        // A Git hook stays read-only inside, so it stands for a file outside
        // without touching $HOME.
        let git = std::process::Command::new("git")
            .args(["init", "-q"])
            .current_dir(&ws)
            .status()
            .is_ok_and(|s| s.success());
        let outside = ws.join(".git/hooks/pre-commit");
        if git {
            std::fs::write(&outside, "host").unwrap();
        }
        let Ok(s) = Session::new(AgentKind::Codex, &ws) else {
            return;
        };
        // A runner that can't start here (bubblewrap without user namespaces in CI).
        if s.run("true", &[], None).is_err() {
            return;
        }
        let dir = s.dir.clone();
        s.write(&ws.join("src/a.txt"), b"one\ntwo\n").unwrap();
        assert_eq!(s.read(&ws.join("src/a.txt"), 100).unwrap(), b"one\ntwo\n");
        assert_eq!(s.read(&ws.join("src/a.txt"), 4).unwrap(), b"one\n");
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(ws.join(".env"), ws.join("l")).unwrap();
            // Refused (Seatbelt) or empty (bubblewrap binds an empty file over it).
            assert!(s.read(&ws.join("l"), 100).map_or(true, |b| b.is_empty()));
            if git {
                std::os::unix::fs::symlink(&outside, ws.join("out")).unwrap();
                assert!(s.write(&ws.join("out"), b"agent").is_err());
                assert_eq!(std::fs::read_to_string(&outside).unwrap(), "host");
            }
        }
        assert!(s.read(&ws, 100).is_err());
        drop(s);
        assert!(!dir.exists());
    }

    #[cfg(unix)]
    #[test]
    fn the_settings_copy_never_follows_a_link() {
        let d = std::env::temp_dir().join(format!("arakun-sbx-seed-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        let from = d.join("claude.json");
        std::fs::write(&from, "{}").unwrap();
        // A link the agent left, dangling, toward a file of the user's.
        let victim = d.join("victim");
        std::os::unix::fs::symlink(&victim, d.join("inside.json")).unwrap();
        seed(&from, &d.join("inside.json")).unwrap();
        assert!(!victim.exists());
        // Nothing there: a copy, the user's alone.
        seed(&from, &d.join("fresh.json")).unwrap();
        assert_eq!(std::fs::read_to_string(d.join("fresh.json")).unwrap(), "{}");
        use std::os::unix::fs::PermissionsExt as _;
        let mode = std::fs::metadata(d.join("fresh.json"))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
        // No source: nothing to copy.
        seed(&d.join("none"), &d.join("x.json")).unwrap();
        assert!(!d.join("x.json").exists());
        std::fs::remove_dir_all(&d).unwrap();
    }

    fn git_common_dir(ws: &Path) -> Result<Option<PathBuf>, String> {
        git_dirs(ws).map(|o| o.map(|(common, _)| common))
    }

    #[test]
    fn a_worktree_belongs_to_its_project() {
        let _one = projects_lock();
        let d = std::env::temp_dir().join(format!("arakun-sbx-wt-{}", std::process::id()));
        let main = d.join("proj");
        let wt = d.join("env-a");
        std::fs::create_dir_all(main.join(".git/worktrees/env-a")).unwrap();
        std::fs::create_dir_all(main.join("src")).unwrap();
        std::fs::write(main.join(".git/HEAD"), "ref: refs/heads/main\n").unwrap();
        std::fs::create_dir_all(&wt).unwrap();
        std::fs::write(
            wt.join(".git"),
            format!("gitdir: {}\n", main.join(".git/worktrees/env-a").display()),
        )
        .unwrap();
        std::fs::write(main.join(".git/worktrees/env-a/commondir"), "../..\n").unwrap();
        std::fs::write(
            main.join(".git/worktrees/env-a/gitdir"),
            format!("{}\n", wt.join(".git").display()),
        )
        .unwrap();
        let main_real = crate::platform::canonicalize(&main).unwrap();
        assert_eq!(git_common_dir(&wt), Ok(Some(main_real.join(".git"))));
        assert_eq!(git_common_dir(&d), Ok(None));
        // A `.git` file the agent rewrote to lead elsewhere opens nothing.
        let other = d.join("other");
        std::fs::create_dir_all(other.join("x")).unwrap();
        std::fs::write(other.join("x/commondir"), "..\n").unwrap();
        std::fs::write(
            other.join("x/gitdir"),
            format!("{}\n", wt.join(".git").display()),
        )
        .unwrap();
        let forged = d.join("forged");
        std::fs::create_dir_all(&forged).unwrap();
        std::fs::write(
            forged.join(".git"),
            format!("gitdir: {}\n", other.join("x").display()),
        )
        .unwrap();
        assert!(git_common_dir(&forged).is_err());
        // One that leads to a real worktree, but not this one's.
        std::fs::write(
            forged.join(".git"),
            format!("gitdir: {}\n", main.join(".git/worktrees/env-a").display()),
        )
        .unwrap();
        assert!(git_common_dir(&forged).is_err());
        // A pipe in place of `.git` is not read, so it can't block the window.
        #[cfg(unix)]
        {
            let piped = d.join("piped");
            std::fs::create_dir_all(&piped).unwrap();
            let c =
                std::ffi::CString::new(piped.join(".git").to_string_lossy().as_bytes()).unwrap();
            assert_eq!(unsafe { libc::mkfifo(c.as_ptr(), 0o600) }, 0);
            assert!(git_common_dir(&piped).is_err());
        }
        // A `.git` folder that is a link out of the checkout.
        #[cfg(unix)]
        {
            let linked = d.join("linked");
            std::fs::create_dir_all(&linked).unwrap();
            std::os::unix::fs::symlink(main.join(".git"), linked.join(".git")).unwrap();
            assert!(git_common_dir(&linked).is_err());
        }
        // Git writes the backlink absolute, but a relative one is read from its folder.
        std::fs::write(
            main.join(".git/worktrees/env-a/gitdir"),
            "../../../../env-a/.git\n",
        )
        .unwrap();
        assert_eq!(git_common_dir(&wt), Ok(Some(main_real.join(".git"))));
        // This checkout's own folder comes back too, for its config.worktree.
        assert_eq!(
            git_dirs(&wt).unwrap().map(|(_, own)| own),
            Some(main_real.join(".git/worktrees/env-a"))
        );
        set_projects([folder(&main, true)]);
        assert!(wanted(&wt).unwrap() && wanted(&main.join("src")).unwrap());
        // Metadata it can't trust stops the launch rather than skipping the sandbox,
        // unless the shell listed the folder as one of the project's environments.
        assert!(wanted(&forged).is_err());
        set_projects([folder(&main, true), folder(&forged, true)]);
        assert!(wanted(&forged).unwrap());
        set_projects([folder(&main, true)]);
        // A project opened at a worktree covers the main checkout too.
        set_projects([folder(&wt, true)]);
        assert!(wanted(&main).unwrap());
        assert!(!wanted(&d).unwrap());
        // A project without the sandbox answers for its own folders, broken
        // metadata or not, even beside one with it.
        set_projects([folder(&main, true), folder(&forged, false)]);
        assert!(!wanted(&forged).unwrap());
        set_projects([]);
        assert!(!wanted(&wt).unwrap());
        std::fs::remove_dir_all(&d).unwrap();
    }
}
