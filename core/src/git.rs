//! Thin wrappers over the git CLI. Everything here blocks; callers run it on the
//! background executor. `GIT_OPTIONAL_LOCKS=0` keeps our polling `status` from taking
//! the index lock while an agent or terminal is committing in the same worktree.
//!
//! Every call has a time limit ([`GIT_LOCAL`], [`GIT_REMOTE`] or [`GIT_LONG`]): the
//! background executor has a few threads, and a call that hangs would hold one for good.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Output};
use std::time::Duration;

use anyhow::{Context as _, Result, anyhow, bail};

use crate::platform::{GIT_LOCAL, GIT_LONG, GIT_REMOTE};

/// Git's well-known empty tree, the diff base for repositories without commits.
const EMPTY_TREE: &str = "4b825dc642cb6eb9a060e54bf8d69288fbee4904";

/// Git in `dir`. In a sandboxed project's folders it runs no hooks and no
/// fsmonitor: the agent can write what those run (a hooks folder in the
/// checkout, as husky sets it), and most of these calls run by themselves.
pub fn git(dir: &Path) -> Command {
    guarded(dir, false).0
}

/// Git for what the user asked for that runs their hooks (commit, push, pull,
/// merge, a new worktree): pinned like [`git`], but hooks run as they would in
/// a terminal.
fn git_hooked(dir: &Path) -> Command {
    guarded(dir, true).0
}

/// [`git`], and whether `dir` is in a sandboxed project's folders, where a read
/// of the work tree adds [`STATUS_GUARD`] or [`DIFF_GUARD`].
fn guarded(dir: &Path, hooks: bool) -> (Command, bool) {
    use crate::agent::sandbox::GitPin;
    let pin = crate::agent::sandbox::git_pin(dir);
    if let GitPin::Refused(why) = &pin {
        // Fails the way git does, with the reason, whatever arguments follow.
        let mut cmd = crate::platform::command("sh");
        cmd.current_dir(dir)
            .args(["-c", "printf '%s\\n' \"$0\" >&2; exit 128", why]);
        return (cmd, true);
    }
    let mut cmd = crate::platform::command("git");
    cmd.current_dir(dir)
        .env("GIT_OPTIONAL_LOCKS", "0")
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("LC_ALL", "C");
    // No lower_priority here: its pre_exec makes the spawn a full fork() of arakun,
    // which holds the process's memory map while it copies it, and git runs every
    // few seconds. A fork of a 1.7 GB arakun stalled the window for up to seconds.
    // `dir` picks the repository, not a GIT_DIR inherited from whatever started arakun.
    unpinned(&mut cmd);
    let GitPin::Pinned {
        git_dir,
        common,
        work_tree,
    } = pin
    else {
        return (cmd, false);
    };
    // GIT_COMMON_DIR wins over a `commondir` file, GIT_DIR over a `.git` file.
    cmd.env("GIT_DIR", git_dir)
        .env("GIT_COMMON_DIR", common)
        .env("GIT_WORK_TREE", work_tree)
        .args(["-c", "core.fsmonitor=false"]);
    if !hooks {
        cmd.args(["-c", "core.hooksPath=/dev/null"]);
    }
    (cmd, true)
}

/// After `status` in a sandboxed folder: a submodule's dirt is not looked at.
/// To see it git runs `git status` in the submodule, with that submodule's own
/// config, which the agent can write (a nested repository it added is one), and
/// a filter there runs a command even with hooks and fsmonitor off.
pub const STATUS_GUARD: &[&str] = &["--ignore-submodules=dirty"];

/// After `diff` in a sandboxed folder: [`STATUS_GUARD`], and no external diff
/// or text conversion, whose drivers the agent picks with `.gitattributes`.
pub const DIFF_GUARD: &[&str] = &[
    "--ignore-submodules=dirty",
    "--no-ext-diff",
    "--no-textconv",
];

/// `git status` in `dir`, with [`STATUS_GUARD`] where it applies.
pub fn git_status(dir: &Path) -> Command {
    let (mut cmd, guard) = guarded(dir, false);
    cmd.arg("status");
    if guard {
        cmd.args(STATUS_GUARD);
    }
    cmd
}

/// `git diff` in `dir`, with [`DIFF_GUARD`] where it applies.
pub fn git_diff(dir: &Path) -> Command {
    let (mut cmd, guard) = guarded(dir, false);
    cmd.arg("diff");
    if guard {
        cmd.args(DIFF_GUARD);
    }
    cmd
}

/// Clears the variables that pin git to one repository. In a linked worktree, git
/// gives hooks GIT_DIR (and GIT_INDEX_FILE) pointing at the real repository; a
/// test run from a hook inherits them, and its `git init` and `git commit` would
/// land on the branch being pushed instead of in its temporary folder.
pub fn unpinned(cmd: &mut Command) -> &mut Command {
    for var in ["GIT_DIR", "GIT_WORK_TREE", "GIT_INDEX_FILE"] {
        cmd.env_remove(var);
    }
    cmd
}

/// [`Command::output`] for git and gh, stopped after `limit`. A call that took too long
/// fails with an error that names it.
pub fn output(cmd: &mut Command, limit: Duration) -> Result<Output> {
    crate::platform::output_within(cmd, None, limit).map_err(|e| {
        if e.kind() == std::io::ErrorKind::TimedOut {
            // Kept whole, so `timed_out` can tell it apart.
            anyhow::Error::new(e)
        } else {
            let program = Path::new(cmd.get_program())
                .file_stem()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned();
            anyhow!("could not run {program}: {e}")
        }
    })
}

/// Whether `err` is a git or gh call that ran past its limit and was stopped.
pub fn timed_out(err: &anyhow::Error) -> bool {
    err.chain()
        .filter_map(|e| e.downcast_ref::<std::io::Error>())
        .any(|e| e.kind() == std::io::ErrorKind::TimedOut)
}

/// Whether `cmd` ran and succeeded within `limit`.
fn succeeds(cmd: &mut Command, limit: Duration) -> bool {
    output(cmd, limit).is_ok_and(|o| o.status.success())
}

fn run(cmd: &mut Command, limit: Duration) -> Result<Output> {
    let out = output(cmd, limit)?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        let err = err
            .trim()
            .trim_start_matches("fatal: ")
            .trim_start_matches("error: ");
        bail!("{}", if err.is_empty() { "git failed" } else { err });
    }
    Ok(out)
}

fn stdout(cmd: &mut Command) -> Result<String> {
    Ok(String::from_utf8_lossy(&run(cmd, GIT_LOCAL)?.stdout).into_owned())
}

/// Root of the main working tree for any folder inside a repository or one of its
/// linked worktrees, so a project is always keyed by its main checkout.
pub fn main_root(dir: &Path) -> Result<PathBuf> {
    let common = stdout(git(dir).args(["rev-parse", "--path-format=absolute", "--git-common-dir"]))
        .map_err(|_| anyhow!("{} is not inside a git repository", dir.display()))?;
    let common = PathBuf::from(common.trim());
    if common.file_name().is_some_and(|n| n == ".git") {
        return Ok(common.parent().unwrap_or(&common).to_path_buf());
    }
    // Bare repository or unusual layout: fall back to this folder's top level.
    let top = stdout(git(dir).args(["rev-parse", "--show-toplevel"]))?;
    Ok(PathBuf::from(top.trim()))
}

/// Top level of the working tree that contains `dir` (a linked worktree's own root,
/// not its main checkout).
pub fn top_level(dir: &Path) -> Result<PathBuf> {
    let top = stdout(git(dir).args(["rev-parse", "--show-toplevel"]))
        .map_err(|_| anyhow!("{} is not inside a git repository", dir.display()))?;
    Ok(PathBuf::from(top.trim()))
}

pub fn init(dir: &Path) -> Result<()> {
    run(git(dir).args(["init", "-q"]), GIT_LOCAL)?;
    Ok(())
}

/// Clones `url` into `dest`, which must not exist yet. A GitHub repository goes
/// through `gh` when it is installed, so a private one uses its sign-in instead of
/// a password prompt git can't show; plain git is the fallback when `gh` is missing
/// or fails. A failed attempt removes what it left in `dest`, so a retry starts clean.
pub fn clone(url: &str, on_github: bool, dest: &Path) -> Result<()> {
    if dest.exists() {
        bail!("{} already exists", dest.display());
    }
    let parent = dest
        .parent()
        .ok_or_else(|| anyhow!("{} has no parent folder", dest.display()))?;
    std::fs::create_dir_all(parent)
        .with_context(|| format!("could not create {}", parent.display()))?;
    // `dest` did not exist above, so anything in it now is this clone's.
    let clean = || {
        let _ = std::fs::remove_dir_all(dest);
    };
    if on_github && gh_available() {
        let done = run_loud(
            gh(parent)
                .args(["repo", "clone", url])
                .arg(dest)
                .args(["--", "--quiet"]),
            GIT_LONG,
        );
        if done.is_ok() {
            return Ok(());
        }
        clean();
    }
    run_loud(
        git(parent).args(["clone", "--quiet", "--", url]).arg(dest),
        GIT_LONG,
    )
    .inspect_err(|_| clean())?;
    Ok(())
}

/// The URL of `dir`'s `origin` remote, if it has one.
pub fn origin_url(dir: &Path) -> Option<String> {
    let url = stdout(git(dir).args(["remote", "get-url", "origin"])).ok()?;
    Some(url.trim().to_string()).filter(|u| !u.is_empty())
}

/// The branch checked out in `dir`; None on a detached HEAD.
pub fn current_branch(dir: &Path) -> Option<String> {
    let out = stdout(git(dir).args(["symbolic-ref", "--short", "-q", "HEAD"])).ok()?;
    Some(out.trim().to_string()).filter(|b| !b.is_empty())
}

/// Branch checked out in a repository, read straight from `.git/HEAD` (no process),
/// for listing many repositories quickly.
pub fn head_branch_fast(repo: &Path) -> Option<String> {
    let head = std::fs::read_to_string(repo.join(".git/HEAD")).ok()?;
    head.trim()
        .strip_prefix("ref: refs/heads/")
        .map(str::to_string)
}

#[derive(Clone, Debug)]
pub struct Worktree {
    pub path: PathBuf,
    pub branch: Option<String>,
    pub is_main: bool,
}

pub fn worktrees(repo: &Path) -> Result<Vec<Worktree>> {
    let text = stdout(git(repo).args(["worktree", "list", "--porcelain"]))?;
    let mut out = Vec::new();
    for block in text.split("\n\n") {
        let mut path = None;
        let mut branch = None;
        let mut bare = false;
        let mut prunable = false;
        for line in block.lines() {
            if let Some(p) = line.strip_prefix("worktree ") {
                path = Some(PathBuf::from(p));
            } else if let Some(b) = line.strip_prefix("branch ") {
                branch = Some(b.trim_start_matches("refs/heads/").to_string());
            } else if line == "bare" {
                bare = true;
            } else if line.starts_with("prunable") {
                prunable = true;
            }
        }
        if let Some(path) = path
            && !bare
            && !prunable
        {
            out.push(Worktree {
                is_main: out.is_empty(),
                path,
                branch,
            });
        }
    }
    Ok(out)
}

#[derive(Clone, Debug, Default)]
pub struct Branches {
    pub current: Option<String>,
    pub local: Vec<String>,
    pub remote: Vec<String>,
    /// The branch origin calls its default (`origin/HEAD`), when the clone knows it.
    pub origin_head: Option<String>,
}

pub fn branches(repo: &Path) -> Result<Branches> {
    let text = stdout(git(repo).args([
        "for-each-ref",
        "--sort=-committerdate",
        "--format=%(HEAD)%00%(refname)",
        "refs/heads",
        "refs/remotes",
    ]))?;
    let mut b = Branches::default();
    for line in text.lines() {
        let Some((head, name)) = line.split_once('\0') else {
            continue;
        };
        if let Some(local) = name.strip_prefix("refs/heads/") {
            if head == "*" {
                b.current = Some(local.to_string());
            }
            b.local.push(local.to_string());
        } else if let Some(remote) = name.strip_prefix("refs/remotes/")
            && !remote.ends_with("/HEAD")
        {
            b.remote.push(remote.to_string());
        }
    }
    b.origin_head = read(
        repo,
        &["symbolic-ref", "--short", "refs/remotes/origin/HEAD"],
    )
    .ok()
    .and_then(|h| Some(h.trim().strip_prefix("origin/")?.to_string()))
    .filter(|h| !h.is_empty());
    Ok(b)
}

/// Creates a worktree at `path`. A branch that already exists locally is checked out;
/// otherwise a new branch is made from `base` (a ref, or the current HEAD when None).
/// `only_new` fails instead of checking out a branch that exists: one made for a random
/// name gets renamed later, and an existing branch with that name is someone's work.
pub fn add_worktree(
    repo: &Path,
    path: &Path,
    branch: &str,
    base: Option<&str>,
    only_new: bool,
) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("could not create {}", parent.display()))?;
    }
    let exists = !only_new
        && succeeds(
            git(repo).args([
                "show-ref",
                "--verify",
                "--quiet",
                &format!("refs/heads/{branch}"),
            ]),
            GIT_LOCAL,
        );
    let mut cmd = git_hooked(repo);
    cmd.args(["worktree", "add"]);
    if exists {
        cmd.arg(path).arg(branch);
    } else {
        // Started from origin/main, the branch would track main: `git push` and the
        // ahead/behind count would then point at the wrong branch.
        cmd.arg("--no-track").arg("-b").arg(branch).arg(path);
        if let Some(base) = base {
            cmd.arg(base);
        }
    }
    // Checks out every file of the branch, which takes a while in a big repository.
    run(&mut cmd, GIT_LONG)?;
    Ok(())
}

/// Removes the worktree at `path` only while it has nothing uncommitted: git
/// refuses otherwise. For a removal nobody confirmed.
pub fn remove_clean_worktree(repo: &Path, path: &Path) -> Result<()> {
    run(git(repo).args(["worktree", "remove"]).arg(path), GIT_LOCAL)?;
    let _ = run(git(repo).args(["worktree", "prune"]), GIT_LOCAL);
    Ok(())
}

/// Where a new environment starts by default: `branch` (the project's base branch),
/// else origin's default branch, as it is on origin and fetched first, so it begins
/// from what was pushed and merged rather than a local branch that may be behind.
/// When origin has no such branch, the local branch of that name; None when there is
/// neither, and the caller then starts from the current commit.
pub fn new_env_base(repo: &Path, branch: Option<&str>) -> Option<String> {
    let branch = match branch.map(base_branch_name).filter(|b| !b.is_empty()) {
        Some(b) => b.to_string(),
        None => origin_default_branch(repo),
    };
    origin_base(repo, &branch).or_else(|| {
        let local = format!("refs/heads/{branch}");
        succeeds(
            git(repo).args(["show-ref", "--verify", "--quiet", &local]),
            GIT_LOCAL,
        )
        .then_some(local)
    })
}

/// A base branch as typed: `origin/main` names the same branch as `main`.
pub fn base_branch_name(typed: &str) -> &str {
    let typed = typed.trim();
    typed.strip_prefix("origin/").unwrap_or(typed)
}

/// The branch origin calls its default. A clone knows it as `origin/HEAD`; a
/// repository that added origin later asks origin, and `main` when that fails too.
fn origin_default_branch(repo: &Path) -> String {
    if let Ok(head) = read(
        repo,
        &["symbolic-ref", "--short", "refs/remotes/origin/HEAD"],
    ) && let Some(b) = head.trim().strip_prefix("origin/")
        && !b.is_empty()
    {
        return b.to_string();
    }
    output(
        git(repo).args(["ls-remote", "--symref", "origin", "HEAD"]),
        ORIGIN_TIMEOUT,
    )
    .ok()
    .filter(|o| o.status.success())
    .and_then(|o| symref_head(&String::from_utf8_lossy(&o.stdout)))
    .unwrap_or_else(|| "main".into())
}

/// The branch in `git ls-remote --symref origin HEAD`'s answer
/// (`ref: refs/heads/main\tHEAD`).
fn symref_head(listed: &str) -> Option<String> {
    listed.lines().find_map(|l| {
        let (target, name) = l.strip_prefix("ref: ")?.split_once('\t')?;
        (name == "HEAD")
            .then(|| target.strip_prefix("refs/heads/"))
            .flatten()
            .filter(|b| !b.is_empty())
            .map(str::to_string)
    })
}

/// `branch` as it is on origin, fetched first. None when origin has no such
/// branch, or it can't be reached and was never fetched.
fn origin_base(repo: &Path, branch: &str) -> Option<String> {
    let remote = format!("refs/remotes/origin/{branch}");
    // Exit code 2: origin answered and has no such branch. A copy fetched before it
    // was deleted must not be used then.
    let listed = status_within(
        git(repo)
            .args(["ls-remote", "--exit-code", "--heads", "origin"])
            .arg(format!("refs/heads/{branch}")),
        ORIGIN_TIMEOUT,
    );
    match listed.and_then(|s| s.code()) {
        Some(2) => return None,
        // An explicit destination: origin's fetch refspec may not cover this branch.
        Some(0) => {
            let _ = status_within(
                git(repo)
                    .args(["fetch", "--quiet", "origin"])
                    .arg(format!("+refs/heads/{branch}:{remote}")),
                ORIGIN_TIMEOUT,
            );
        }
        // Offline, no access or too slow: a remote branch fetched earlier is still a
        // better start.
        _ => {}
    }
    // The full name: a local branch called origin/<branch> would make the short one
    // ambiguous.
    succeeds(
        git(repo).args(["show-ref", "--verify", "--quiet", &remote]),
        GIT_LOCAL,
    )
    .then_some(remote)
}

/// How long each call to origin may take while an environment is made. A new
/// environment waits on it, so a stalled origin must not hold it up for long.
const ORIGIN_TIMEOUT: Duration = Duration::from_secs(10);

/// Runs `cmd` and returns how it exited, or None when it could not start or ran
/// past `limit`. Then the whole tree is ended: a stalled git fetch would otherwise
/// leave its ssh running.
fn status_within(cmd: &mut Command, limit: Duration) -> Option<ExitStatus> {
    output(cmd, limit).ok().map(|o| o.status)
}

pub fn remove_worktree(repo: &Path, path: &Path, delete_branch: Option<&str>) -> Result<()> {
    // Deletes the whole folder, untracked builds and dependencies too.
    run(
        git(repo).args(["worktree", "remove", "--force"]).arg(path),
        GIT_LONG,
    )?;
    let _ = run(git(repo).args(["worktree", "prune"]), GIT_LOCAL);
    if let Some(branch) = delete_branch {
        run(git(repo).args(["branch", "-D", branch]), GIT_LOCAL)?;
    }
    Ok(())
}

pub fn branch_name_ok(repo: &Path, name: &str) -> bool {
    succeeds(
        git(repo).args(["check-ref-format", "--branch", name]),
        GIT_LOCAL,
    )
}

/// Renames the branch checked out in `dir` from `old` to `new`. Refuses when the
/// branch is no longer `old` or was pushed (it has an upstream): the remote would
/// keep the old name. When `new` is taken, `next_free` tries `new-2`, `new-3`…;
/// otherwise it fails. Returns the name the branch got.
pub fn rename_branch(dir: &Path, old: &str, new: &str, next_free: bool) -> Result<String> {
    let current =
        stdout(git(dir).args(["symbolic-ref", "--short", "-q", "HEAD"])).unwrap_or_default();
    let current = current.trim();
    if current != old {
        bail!(
            "the branch is {} now, not {old}",
            if current.is_empty() {
                "detached"
            } else {
                current
            }
        );
    }
    if old == new {
        return Ok(new.to_string());
    }
    // A push without --set-upstream leaves no upstream, only the remote-tracking ref.
    // A check that failed or timed out says nothing, and must not let the rename go on.
    let unsure = || format!("could not check whether {old} was pushed");
    let upstream = output(
        git(dir).args(["config", "--get", &format!("branch.{old}.merge")]),
        GIT_LOCAL,
    )
    .with_context(unsure)?;
    // `config --get` exits 1 only when the key is not set.
    let upstream = match upstream.status.code() {
        Some(0) => true,
        Some(1) => false,
        _ => bail!("{}", unsure()),
    };
    let tracked = stdout(git(dir).args([
        "for-each-ref",
        "--format=%(refname)",
        &format!("refs/remotes/*/{old}"),
    ]))
    .with_context(unsure)?;
    let tracked = !tracked.trim().is_empty();
    if upstream || tracked {
        bail!("{old} was already pushed; rename it on the remote too, or keep the name");
    }
    if !branch_name_ok(dir, new) {
        bail!("\"{new}\" is not a valid branch name");
    }
    let exists = |name: &str| {
        succeeds(
            git(dir).args([
                "show-ref",
                "--verify",
                "--quiet",
                &format!("refs/heads/{name}"),
            ]),
            GIT_LOCAL,
        )
    };
    let name = if !exists(new) {
        new.to_string()
    } else if next_free {
        (2..10)
            .map(|n| format!("{new}-{n}"))
            .find(|n| !exists(n))
            .ok_or_else(|| anyhow!("branches {new} to {new}-9 already exist"))?
    } else {
        bail!("a branch named {new} already exists");
    };
    run(git(dir).args(["branch", "-m", old, &name]), GIT_LOCAL)?;
    Ok(name)
}

/// Commits a push would send, or None when the branch has no remote or no upstream
/// (nothing has ever been pushed, so "not on the remote" says nothing useful).
pub fn unpushed(dir: &Path) -> Option<u32> {
    let s = sync(dir).ok()?;
    (s.remote.is_some() && s.upstream.is_some()).then_some(s.ahead)
}

/// Commits no remote has, for a branch without an upstream. None when the branch
/// does have one (then `unpushed` answers) or there is nothing to count.
pub fn local_only(dir: &Path) -> Option<u32> {
    let s = sync(dir).ok()?;
    if s.branch.is_none() || s.upstream.is_some() {
        return None;
    }
    stdout(git(dir).args(["rev-list", "--count", "HEAD", "--not", "--remotes"]))
        .ok()
        .and_then(|n| n.trim().parse().ok())
        .filter(|n| *n > 0)
}

/// `git fetch` of every remote, quiet and never interactive; keeps the ahead/behind
/// counts fresh without a pull.
pub fn fetch(dir: &Path) -> Result<()> {
    run(git(dir).args(["fetch", "--all", "--quiet"]), GIT_REMOTE)?;
    Ok(())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum ChangeKind {
    Modified,
    Added,
    Deleted,
    Renamed,
    Untracked,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Change {
    pub path: String,
    pub kind: ChangeKind,
    pub added: u32,
    pub removed: u32,
    /// Something in the index differs from HEAD (porcelain X).
    pub staged: bool,
    /// Something in the worktree differs from the index (porcelain Y).
    pub unstaged: bool,
}

impl Change {
    pub fn fully_staged(&self) -> bool {
        self.staged && !self.unstaged
    }
}

/// Whether HEAD points at a commit: not in a repository before its first one.
/// An error when git could not tell, such as for a repository it refuses to
/// read (not a repository, untrusted owner) or a probe that ran past
/// `GIT_LOCAL`: that is no sign of a new one.
pub fn has_commits(dir: &Path) -> Result<bool> {
    let out = output(
        git(dir).args(["rev-parse", "--verify", "--quiet", "HEAD"]),
        GIT_LOCAL,
    )?;
    // `--verify --quiet` exits 1, silently, only when HEAD names no commit.
    match out.status.code() {
        Some(0) => Ok(true),
        Some(1) => Ok(false),
        _ => bail!(
            "git could not read {}: {}",
            dir.display(),
            String::from_utf8_lossy(&out.stderr).trim()
        ),
    }
}

/// The ref uncommitted changes diff against. `EMPTY_TREE` only when git says
/// HEAD is absent: otherwise `discard` would take the unborn-repository path
/// and delete work.
fn diff_base(dir: &Path) -> Result<&'static str> {
    Ok(if has_commits(dir)? {
        "HEAD"
    } else {
        EMPTY_TREE
    })
}

/// Uncommitted changes against HEAD (staged and unstaged together, as the Changes
/// list shows them), with line counts. Untracked files count all their lines as added.
pub fn status(dir: &Path) -> Result<Vec<Change>> {
    Ok(read_status(dir)?.0)
}

/// The changes and where the branch stands. The Changes list polls this every few
/// seconds, so one `git status` answers both, and what rarely changes is kept.
pub fn status_and_sync(dir: &Path) -> Result<(Vec<Change>, Sync)> {
    let (changes, head) = read_status(dir)?;
    Ok((changes, sync_from(dir, head)?))
}

/// What `git status --branch` says about HEAD.
#[derive(Debug, Default, PartialEq, Eq)]
struct Head {
    /// HEAD names a commit: false before the first one.
    born: bool,
    /// None when HEAD is detached.
    branch: Option<String>,
    /// `origin/main`. Git still names it after it was deleted on the remote.
    upstream: Option<String>,
    /// Ahead and behind the upstream. None when the upstream is gone.
    ab: Option<(u32, u32)>,
}

fn read_status(dir: &Path) -> Result<(Vec<Change>, Head)> {
    let text = stdout(git_status(dir).args([
        "--porcelain=v2",
        "-z",
        "--branch",
        "--untracked-files=all",
        "--no-renames",
    ]))?;
    let (mut changes, head) = parse_status(&text);
    if changes.iter().any(|c| c.kind != ChangeKind::Untracked) {
        let base = if head.born { "HEAD" } else { EMPTY_TREE };
        let numstat = stdout(git_diff(dir).args([base, "--numstat", "-z", "--no-renames"]))
            .unwrap_or_default();
        let mut counts = HashMap::new();
        for rec in numstat.split('\0').filter(|s| !s.is_empty()) {
            let mut f = rec.splitn(3, '\t');
            let (Some(a), Some(r), Some(p)) = (f.next(), f.next(), f.next()) else {
                continue;
            };
            counts.insert(p, (a.parse().unwrap_or(0), r.parse().unwrap_or(0)));
        }
        for c in &mut changes {
            if let Some(&(a, r)) = counts.get(c.path.as_str()) {
                c.added = a;
                c.removed = r;
            }
        }
    }
    count_untracked(dir, &mut changes);
    changes.sort_by(|a, b| a.path.cmp(&b.path));
    Ok((changes, head))
}

/// Reads `git status --porcelain=v2 -z --branch`.
fn parse_status(text: &str) -> (Vec<Change>, Head) {
    let mut changes = Vec::new();
    let mut head = Head {
        born: true,
        ..Head::default()
    };
    let mut parts = text.split('\0').filter(|s| !s.is_empty());
    while let Some(entry) = parts.next() {
        if let Some(header) = entry.strip_prefix("# ") {
            let (key, value) = header.split_once(' ').unwrap_or((header, ""));
            match key {
                "branch.oid" => head.born = value != "(initial)",
                "branch.head" if value != "(detached)" => head.branch = Some(value.to_string()),
                "branch.upstream" => head.upstream = Some(value.to_string()),
                "branch.ab" => {
                    let mut n = value
                        .split(' ')
                        .map(|n| n.trim_start_matches(['+', '-']).parse().unwrap_or(0));
                    head.ab = Some((n.next().unwrap_or(0), n.next().unwrap_or(0)));
                }
                _ => {}
            }
            continue;
        }
        // The fields before the path: the path is last and may hold spaces.
        let fields = match entry.as_bytes()[0] {
            b'?' => 1,
            b'1' => 8,
            // A rename or copy: its old path is the next entry.
            b'2' => {
                parts.next();
                9
            }
            b'u' => 10,
            // Ignored files, and whatever a later git adds.
            _ => continue,
        };
        let mut f = entry.splitn(fields + 1, ' ');
        let (Some(kind), Some(path)) = (f.next(), f.nth(fields - 1)) else {
            continue;
        };
        // v2 writes an unchanged side as `.` where v1 wrote a space.
        let (x, y) = match kind {
            "?" => (b'?', b'?'),
            _ => match entry.as_bytes().get(2..4) {
                Some(&[x, y]) => (x, y),
                _ => continue,
            },
        };
        let change_kind = match (x, y) {
            (b'?', b'?') => ChangeKind::Untracked,
            (b'A', _) => ChangeKind::Added,
            (b'D', _) | (_, b'D') => ChangeKind::Deleted,
            (b'R', _) => ChangeKind::Renamed,
            _ => ChangeKind::Modified,
        };
        let untracked = change_kind == ChangeKind::Untracked;
        changes.push(Change {
            path: path.to_string(),
            kind: change_kind,
            added: 0,
            removed: 0,
            staged: !untracked && x != b'.',
            unstaged: untracked || y != b'.',
        });
    }
    (changes, head)
}

/// A file's modified time and size: when both are the same, so is what we read
/// from it.
type Stamp = (std::time::SystemTime, u64);

fn stamp(path: &Path) -> Option<Stamp> {
    let meta = std::fs::metadata(path).ok()?;
    Some((meta.modified().ok()?, meta.len()))
}

/// A folder's untracked files and their line counts.
type Counted = HashMap<String, (Stamp, u32)>;

/// Line counts of untracked files, by folder. Without them the poll would read every
/// untracked file again every few seconds: a new build folder git doesn't ignore is
/// hundreds of MB.
static UNTRACKED: std::sync::LazyLock<std::sync::Mutex<HashMap<PathBuf, Counted>>> =
    std::sync::LazyLock::new(Default::default);

/// Counts the lines of the untracked files, reading only those that changed since
/// the last call for `dir`.
fn count_untracked(dir: &Path, changes: &mut [Change]) {
    let lock = || {
        UNTRACKED
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    };
    // Taken out, so other folders' polls don't wait while this one reads files.
    let old = lock().remove(dir).unwrap_or_default();
    let mut new = HashMap::new();
    for c in changes
        .iter_mut()
        .filter(|c| c.kind == ChangeKind::Untracked)
    {
        let path = dir.join(&c.path);
        let now = stamp(&path);
        c.added = match old.get(&c.path) {
            Some(&(was, lines)) if Some(was) == now => lines,
            _ => count_lines(&path),
        };
        if let Some(now) = now {
            new.insert(c.path.clone(), (now, c.added));
        }
    }
    // Only what is still untracked is kept, so the cache can't grow past that.
    if !new.is_empty() {
        lock().insert(dir.to_path_buf(), new);
    }
}

fn count_lines(path: &Path) -> u32 {
    use std::io::Read as _;
    let Ok(file) = std::fs::File::open(path) else {
        return 0;
    };
    let mut buf = Vec::new();
    if file.take(4 << 20).read_to_end(&mut buf).is_err() || buf.contains(&0) {
        return 0;
    }
    let lines = buf.iter().filter(|&&b| b == b'\n').count();
    (lines + usize::from(!buf.is_empty() && !buf.ends_with(b"\n"))) as u32
}

/// Every tracked and untracked (but not ignored) file, for `@` completion.
pub fn ls_files(dir: &Path) -> Result<Vec<String>> {
    let text = stdout(git(dir).args(["ls-files", "-co", "--exclude-standard", "-z"]))?;
    let mut files: Vec<String> = text
        .split('\0')
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect();
    files.sort_unstable();
    files.truncate(20_000);
    Ok(files)
}

/// Unified diff of one file against HEAD; untracked files diff against /dev/null.
pub fn diff(dir: &Path, path: &str, untracked: bool) -> Result<String> {
    if untracked {
        let out = output(
            git_diff(dir).args(["--no-color", "--no-index", "--", "/dev/null", path]),
            GIT_LOCAL,
        )?;
        // --no-index exits 1 when the files differ, which is the expected case.
        return Ok(String::from_utf8_lossy(&out.stdout).into_owned());
    }
    stdout(git_diff(dir).args(["--no-color", diff_base(dir)?, "--", path]))
}

/// What a commit would hold, for a model to describe: `git diff --stat` and
/// the diff (cut at `cap` characters, and whether it was). With `all`, every
/// uncommitted change, new files too, as "Commit all" takes them; else only
/// what is staged.
pub fn commit_diff(dir: &Path, all: bool, cap: usize) -> Result<(String, String, bool)> {
    let base = diff_base(dir)?;
    let range: &[&str] = if all { &[base] } else { &["--cached", base] };
    let args = |extra: &[&'static str]| {
        let mut a = vec!["--no-color"];
        a.extend_from_slice(extra);
        a.extend_from_slice(range);
        a.push("--");
        a
    };
    let mut stat = stdout(git_diff(dir).args(args(&["--stat=200"])))?;
    let mut diff = stdout(git_diff(dir).args(args(&[])))?;
    // Counted in characters, as `cut_at_line` cuts.
    let mut chars = diff.chars().count();
    let mut skipped = false;
    if all {
        let new = read(dir, &["ls-files", "--others", "--exclude-standard", "-z"])?;
        for path in new.split('\0').filter(|p| !p.is_empty()) {
            stat.push_str(&format!(" {path} (new file)\n"));
            // The diffs stop once past the cap; the list goes on, cut below.
            if chars > cap {
                skipped = true;
                continue;
            }
            // A file of more than 4 bytes per character of the cap has a diff
            // past it: skip it rather than hold all of it in memory.
            if std::fs::metadata(dir.join(path)).is_ok_and(|m| m.len() > 4 * cap as u64) {
                skipped = true;
                continue;
            }
            let more = self::diff(dir, path, true)?;
            chars += more.chars().count();
            diff.push_str(&more);
        }
    }
    let (diff, cut) = cut_at_line(diff, cap);
    Ok((cap_stat(stat, STAT_LINES), diff, cut || skipped))
}

/// How many lines of the list of files a model reads: a folder of
/// dependencies nobody ignored would otherwise list tens of thousands.
const STAT_LINES: usize = 300;

/// `stat` cut to `keep` lines, with a line that says how many files were
/// left out. Git's summary line ("3 files changed, …") is no file: a file's
/// line has a ` | ` or is a new file's, whatever its path says.
fn cap_stat(stat: String, keep: usize) -> String {
    let mut lines = stat.lines();
    let mut kept: String = lines
        .by_ref()
        .take(keep)
        .map(|l| format!("{l}\n"))
        .collect();
    let more = lines
        .filter(|l| l.contains(" | ") || l.ends_with(" (new file)"))
        .count();
    if more > 0 {
        kept.push_str(&format!(" … and {more} more files\n"));
    }
    kept
}

/// The subjects of the last `n` commits on HEAD, newest first: the style a
/// new commit message follows. Empty before the first commit.
pub fn recent_subjects(dir: &Path, n: usize) -> Vec<String> {
    let count = format!("-{n}");
    read(dir, &["log", &count, "--format=%s", "--no-merges"])
        .map(|out| {
            out.lines()
                .map(str::trim)
                .filter(|l| !l.is_empty())
                .map(String::from)
                .collect()
        })
        .unwrap_or_default()
}

// ---------------------------------------------------------------------------
// Workflow: remotes, commit, push/pull, staging, merge, pull requests.

/// Like `run`, but a failure carries everything the command printed: commit hooks,
/// pushes and merges often explain themselves on stdout.
fn run_loud(cmd: &mut Command, limit: Duration) -> Result<String> {
    let out = output(cmd, limit)?;
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    if out.status.success() {
        return Ok(stdout);
    }
    let stderr = String::from_utf8_lossy(&out.stderr);
    let text = strip_ansi(&format!("{}\n{}", stderr.trim(), stdout.trim()));
    let lines: Vec<&str> = text
        .lines()
        .map(str::trim_end)
        .filter(|l| !l.trim().is_empty() && !l.starts_with("hint:"))
        .collect();
    let tail = &lines[lines.len().saturating_sub(12)..];
    let msg = tail
        .iter()
        .map(|l| {
            l.trim_start_matches("fatal: ")
                .trim_start_matches("error: ")
        })
        .collect::<Vec<_>>()
        .join("\n");
    bail!(
        "{}",
        if msg.is_empty() {
            "git failed".to_string()
        } else {
            msg
        }
    )
}

pub fn strip_ansi(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\x1b' {
            if chars.peek() == Some(&'[') {
                chars.next();
                for c in chars.by_ref() {
                    if c.is_ascii_alphabetic() {
                        break;
                    }
                }
            }
            continue;
        }
        if c != '\r' {
            out.push(c);
        }
    }
    out
}

/// Paths from `git status` are literal, never globs.
fn git_paths(dir: &Path) -> Command {
    let mut cmd = git(dir);
    cmd.env("GIT_LITERAL_PATHSPECS", "1");
    cmd
}

fn has_head(dir: &Path) -> Result<bool> {
    Ok(diff_base(dir)? == "HEAD")
}

/// Where the checked-out branch stands against its remote.
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Sync {
    pub branch: Option<String>,
    /// Remote a push goes to: the upstream's, else `origin`, else the first one.
    pub remote: Option<String>,
    /// Upstream branch (`origin/main`); None when unset or deleted on the remote.
    pub upstream: Option<String>,
    /// The upstream is `<remote>/<branch>`, so push and upstream agree.
    pub published: bool,
    /// Commits a push would send.
    pub ahead: u32,
    /// Upstream commits a pull would bring.
    pub behind: u32,
    pub github: bool,
    /// The GitHub CLI is installed.
    pub gh: bool,
}

/// A folder's remotes and the config file they were read from, with its stamp then.
#[derive(Clone)]
struct Remotes {
    config: PathBuf,
    stamp: Stamp,
    list: Vec<(String, String)>,
}

/// Remotes by folder, read again only when the repository's config changes.
static REMOTES: std::sync::LazyLock<std::sync::Mutex<HashMap<PathBuf, Remotes>>> =
    std::sync::LazyLock::new(Default::default);

/// Each remote's name and URL. Remotes set outside the repository's own config (an
/// include or a worktree's config) are seen once that config changes too.
fn remotes(dir: &Path) -> Result<Vec<(String, String)>> {
    let lock = || {
        REMOTES
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    };
    let cached = lock().get(dir).cloned();
    let known = cached.and_then(|r| {
        let now = stamp(&r.config)?;
        Some((r, now))
    });
    let config = match known {
        Some((r, now)) if r.stamp == now => return Ok(r.list),
        Some((r, _)) => r.config,
        // A linked worktree shares the main checkout's config.
        None => dir
            .join(stdout(git(dir).args(["rev-parse", "--git-common-dir"]))?.trim_end())
            .join("config"),
    };
    // Stamped before it is read, so a change made during the read is seen next time.
    let before = stamp(&config);
    // Exits 1 when there are no remotes at all.
    let urls = output(
        git(dir).args(["config", "--get-regexp", r"^remote\..*\.url$"]),
        GIT_LOCAL,
    )?;
    let mut list = Vec::new();
    for line in String::from_utf8_lossy(&urls.stdout).lines() {
        let Some((key, url)) = line.split_once(' ') else {
            continue;
        };
        if let Some(name) = key
            .strip_prefix("remote.")
            .and_then(|k| k.strip_suffix(".url"))
        {
            list.push((name.to_string(), url.trim().to_string()));
        }
    }
    match before {
        Some(stamp) if urls.status.code().is_some_and(|c| c <= 1) => {
            lock().insert(
                dir.to_path_buf(),
                Remotes {
                    config,
                    stamp,
                    list: list.clone(),
                },
            );
        }
        _ => {
            lock().remove(dir);
        }
    }
    Ok(list)
}

/// Where the checked-out branch stands against its remote, without the changes.
pub fn sync(dir: &Path) -> Result<Sync> {
    let text =
        stdout(git_status(dir).args(["--porcelain=v2", "-z", "--branch", "--untracked-files=no"]))?;
    sync_from(dir, parse_status(&text).1)
}

fn sync_from(dir: &Path, head: Head) -> Result<Sync> {
    let mut s = Sync {
        branch: head.branch,
        ..Sync::default()
    };
    let remotes = remotes(dir)?;
    if remotes.is_empty() {
        return Ok(s);
    }
    s.gh = gh_available();
    let Some(branch) = s.branch.clone() else {
        return Ok(s);
    };
    // An upstream deleted on the remote has no counts: it is no upstream any more.
    let (track_ahead, behind) = head.ab.unwrap_or_default();
    if head.ab.is_some() {
        s.upstream = head.upstream;
    }
    s.behind = behind;
    let upstream_remote = s.upstream.as_ref().and_then(|u| {
        remotes
            .iter()
            .filter(|(r, _)| u.starts_with(&format!("{r}/")))
            .max_by_key(|(r, _)| r.len())
    });
    let remote = upstream_remote
        .or_else(|| remotes.iter().find(|(r, _)| r == "origin"))
        .or(remotes.first())
        .cloned();
    if let Some((name, url)) = remote {
        s.github = url.contains("github.com");
        s.published = s.upstream.as_deref() == Some(&format!("{name}/{branch}"));
        s.remote = Some(name);
    }
    s.ahead = if s.published {
        track_ahead
    } else {
        stdout(git(dir).args(["rev-list", "--count", "HEAD", "--not", "--remotes"]))
            .ok()
            .and_then(|n| n.trim().parse().ok())
            .unwrap_or(0)
    };
    Ok(s)
}

pub fn gh_available() -> bool {
    static GH: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *GH.get_or_init(|| succeeds(crate::platform::command("gh").arg("--version"), GIT_LOCAL))
}

/// `git add -A` first when `all`, then commits with the given message.
pub fn commit(dir: &Path, message: &str, all: bool) -> Result<()> {
    if all {
        run_loud(git(dir).args(["add", "-A"]), GIT_LONG)?;
    }
    // Commit hooks may run checks that take minutes.
    if let Err(e) = run_loud(
        git_hooked(dir).args(["commit", "-q", "-m", message]),
        GIT_LONG,
    ) {
        let text = e.to_string();
        // Git's own wording here is a page of `git status` advice.
        if text.contains("nothing to commit") || text.contains("no changes added to commit") {
            bail!(
                "{}",
                if all {
                    "Nothing to commit."
                } else {
                    "Nothing is staged."
                }
            );
        }
        return Err(e);
    }
    Ok(())
}

pub fn push(dir: &Path, remote: &str, branch: &str) -> Result<()> {
    run_loud(
        git_hooked(dir).args(["push", "-u", remote, branch]),
        GIT_LONG,
    )?;
    Ok(())
}

pub fn pull(dir: &Path) -> Result<()> {
    run_loud(git_hooked(dir).args(["pull", "--ff-only"]), GIT_LONG)?;
    Ok(())
}

pub fn stage(dir: &Path, path: &str) -> Result<()> {
    run_loud(git_paths(dir).args(["add", "--"]).arg(path), GIT_LOCAL)?;
    Ok(())
}

/// Stages the changes git sees to `files` (relative to `dir`), and only those;
/// returns the ones it staged, relative to the repository's top.
pub fn stage_only(dir: &Path, files: &[String]) -> Result<Vec<String>> {
    // Status names files from the repository's top, and `dir` may be a folder
    // inside it: both sides, and `git add`, use the top.
    let prefix = stdout(git(dir).args(["rev-parse", "--show-prefix"]))?;
    let prefix = prefix.trim_end_matches(['\r', '\n']);
    let top = top_level(dir)?;
    let changed: std::collections::HashSet<String> =
        status(dir)?.into_iter().map(|c| c.path).collect();
    let picked: Vec<String> = files
        .iter()
        .map(|f| format!("{prefix}{f}"))
        .filter(|f| changed.contains(f))
        .collect();
    if !picked.is_empty() {
        run_loud(
            git_paths(&top).args(["add", "-A", "--"]).args(&picked),
            GIT_LOCAL,
        )?;
    }
    Ok(picked)
}

pub fn unstage(dir: &Path, path: &str) -> Result<()> {
    if has_head(dir)? {
        run_loud(
            git_paths(dir).args(["restore", "--staged", "--"]).arg(path),
            GIT_LOCAL,
        )?;
    } else {
        run_loud(
            git_paths(dir)
                .args(["rm", "-q", "--cached", "--"])
                .arg(path),
            GIT_LOCAL,
        )?;
    }
    Ok(())
}

/// Throws away every change to one file: tracked files go back to HEAD (index and
/// worktree), files git doesn't have in HEAD are deleted.
pub fn discard(dir: &Path, change: &Change) -> Result<()> {
    if change.path.split('/').any(|c| c == "..") || Path::new(&change.path).is_absolute() {
        bail!("refusing to touch {}", change.path);
    }
    let file = dir.join(&change.path);
    if change.kind == ChangeKind::Untracked {
        std::fs::remove_file(&file).with_context(|| format!("could not delete {}", change.path))?;
        return Ok(());
    }
    if has_head(dir)? {
        run_loud(
            git_paths(dir)
                .args(["restore", "--source=HEAD", "--staged", "--worktree", "--"])
                .arg(&change.path),
            GIT_LOCAL,
        )?;
    } else {
        run_loud(
            git_paths(dir)
                .args(["rm", "-q", "-f", "--cached", "--"])
                .arg(&change.path),
            GIT_LOCAL,
        )?;
        if file.exists() {
            std::fs::remove_file(&file)
                .with_context(|| format!("could not delete {}", change.path))?;
        }
    }
    Ok(())
}

/// What merging `branch` into the main checkout would do, or why it can't.
#[derive(Clone, Debug)]
pub struct MergePlan {
    pub base: Option<String>,
    pub commits: u32,
    /// Tracked files with uncommitted changes in the main checkout.
    pub base_dirty: usize,
}

impl MergePlan {
    pub fn blocker(&self, branch: &str) -> Option<String> {
        match &self.base {
            None => Some("The main checkout is on a detached HEAD.".into()),
            Some(b) if b == branch => Some(format!("The main checkout is already on {branch}.")),
            Some(b) if self.base_dirty > 0 => Some(format!(
                "The main checkout ({b}) has {} uncommitted change{}. Commit or stash {} first.",
                self.base_dirty,
                if self.base_dirty == 1 { "" } else { "s" },
                if self.base_dirty == 1 { "it" } else { "them" },
            )),
            Some(b) if self.commits == 0 => {
                Some(format!("{b} already has every commit of {branch}."))
            }
            Some(_) => None,
        }
    }
}

fn dirty_tracked(dir: &Path) -> Result<usize> {
    let text = stdout(git_status(dir).args(["--porcelain=v1", "-z", "--untracked-files=no"]))?;
    Ok(text
        .split('\0')
        .filter(|s| s.len() > 3 && !s.starts_with("??"))
        .count())
}

pub fn merge_plan(main: &Path, branch: &str) -> Result<MergePlan> {
    let head = output(
        git(main).args(["symbolic-ref", "-q", "--short", "HEAD"]),
        GIT_LOCAL,
    )?;
    let base = head
        .status
        .success()
        .then(|| String::from_utf8_lossy(&head.stdout).trim().to_string())
        .filter(|b| !b.is_empty());
    let commits = match &base {
        Some(b) => stdout(git(main).args([
            "rev-list",
            "--count",
            &format!("refs/heads/{b}..refs/heads/{branch}"),
        ]))?
        .trim()
        .parse()
        .unwrap_or(0),
        None => 0,
    };
    Ok(MergePlan {
        base,
        commits,
        base_dirty: dirty_tracked(main)?,
    })
}

/// `git merge --no-ff <branch>` in the main checkout, checked again right before it
/// runs. A merge that stops on conflicts is aborted so the checkout stays clean.
pub fn merge_into_main(main: &Path, branch: &str) -> Result<()> {
    if let Some(why) = merge_plan(main, branch)?.blocker(branch) {
        bail!("{why}");
    }
    if let Err(e) = run_loud(
        git_hooked(main).args(["merge", "--no-ff", "--no-edit", branch]),
        GIT_LONG,
    ) {
        // Long too: an abort stopped halfway would leave the checkout worse off.
        abort(main, "merge").map_err(|abort| anyhow!("{e}\n{abort}"))?;
        bail!("{e}\nThe merge was aborted; nothing changed.");
    }
    Ok(())
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct PullRequest {
    pub number: u64,
    pub url: String,
    /// OPEN, CLOSED or MERGED.
    pub state: String,
    #[serde(default, rename = "isDraft")]
    pub is_draft: bool,
}

/// Stdout of a read-only git command, for callers that parse it themselves.
pub fn read(dir: &Path, args: &[&str]) -> Result<String> {
    stdout(git(dir).args(args))
}

/// `git revert --no-edit <sha>` in a checkout; a revert that conflicts is aborted.
pub fn revert(dir: &Path, sha: &str) -> Result<()> {
    if let Err(e) = run_loud(git_hooked(dir).args(["revert", "--no-edit", sha]), GIT_LONG) {
        abort(dir, "revert").map_err(|abort| anyhow!("{e}\n{abort}"))?;
        bail!("{e}\nThe revert was aborted; nothing changed.");
    }
    Ok(())
}

/// `git <op> --abort` after `op` stopped halfway. An error that says the checkout is
/// left mid-way when the abort failed or timed out: "nothing changed" would be wrong.
fn abort(dir: &Path, op: &str) -> Result<()> {
    let why = match output(git(dir).args([op, "--abort"]), GIT_LONG) {
        Ok(out) if out.status.success() => return Ok(()),
        Ok(out) => String::from_utf8_lossy(&out.stderr).trim().to_string(),
        Err(e) => e.to_string(),
    };
    bail!(
        "The {op} stopped halfway and could not be aborted ({why}). \
         Run `git {op} --abort` in {} before you go on.",
        dir.display()
    )
}

/// A file as it was at `rev`; None when the commit isn't in the local clone.
pub fn show_file(dir: &Path, rev: &str, path: &str) -> Option<String> {
    let out = output(git(dir).arg("show").arg(format!("{rev}:{path}")), GIT_LOCAL).ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).into_owned())
}

/// A worktree at `path` on `branch` as it is on `origin`, for a branch with no
/// local checkout yet. `git worktree add` creates the tracking branch itself.
pub fn add_branch_worktree(repo: &Path, path: &Path, branch: &str) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("could not create {}", parent.display()))?;
    }
    let _ = output(
        git(repo).args(["fetch", "--quiet", "origin", branch]),
        GIT_REMOTE,
    );
    run_loud(
        git_hooked(repo)
            .args(["worktree", "add"])
            .arg(path)
            .arg(branch),
        GIT_LONG,
    )?;
    Ok(())
}

/// `owner/name` in a remote's address (`git@host:owner/name.git`,
/// `https://host/owner/name`, `ssh://git@host/owner/name.git`).
fn remote_slug(url: &str) -> Option<String> {
    let url = url.trim().trim_end_matches('/');
    let path = match url.split_once("://") {
        Some((_, rest)) => rest.split_once('/')?.1,
        None => url.split_once(':')?.1,
    };
    let path = path.trim_end_matches(".git");
    let mut parts = path.rsplitn(3, '/');
    let (name, owner) = (parts.next()?, parts.next()?);
    (!name.is_empty() && !owner.is_empty()).then(|| format!("{owner}/{name}"))
}

/// The remote of the repository at `dir` that points at `slug` (`owner/name`
/// on GitHub): `origin` in a clone, often `upstream` in a fork's.
pub fn remote_for(dir: &Path, slug: &str) -> Option<String> {
    let text = read(dir, &["remote", "-v"]).ok()?;
    text.lines().find_map(|line| {
        let mut cols = line.split_whitespace();
        let (name, url) = (cols.next()?, cols.next()?);
        // A push address can name another repository than the one fetched.
        if cols.next() != Some("(fetch)") {
            return None;
        }
        remote_slug(url)
            .filter(|s| s.eq_ignore_ascii_case(slug))
            .map(|_| name.to_string())
    })
}

/// A worktree at `path` on a pull request's `branch`. A local branch of that
/// name is checked out as it is; otherwise the branch is made from the pull
/// request's head on `remote` (`refs/pull/<number>/head`, which holds a
/// fork's branch too). `track` sets it to follow `<remote>/<branch>`, for a
/// branch that lives in that repository.
pub fn add_pull_worktree(
    repo: &Path,
    path: &Path,
    remote: &str,
    number: u64,
    branch: &str,
    track: bool,
) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("could not create {}", parent.display()))?;
    }
    let local = format!("refs/heads/{branch}");
    if succeeds(
        git(repo).args(["show-ref", "--verify", "--quiet", &local]),
        GIT_LOCAL,
    ) {
        run_loud(
            git_hooked(repo)
                .args(["worktree", "add"])
                .arg(path)
                .arg(branch),
            GIT_LONG,
        )?;
        return Ok(());
    }
    // Straight into the branch, not through FETCH_HEAD: another fetch in this
    // repository could replace that before the worktree is made.
    run_loud(
        git(repo).args([
            "fetch",
            "--quiet",
            remote,
            &format!("refs/pull/{number}/head:{local}"),
        ]),
        GIT_REMOTE,
    )?;
    run_loud(
        git_hooked(repo)
            .args(["worktree", "add"])
            .arg(path)
            .arg(branch),
        GIT_LONG,
    )?;
    if track {
        // Without it `git push` has nowhere to go; the worktree works either way.
        let _ = output(
            git(repo).args(["fetch", "--quiet", remote, branch]),
            GIT_REMOTE,
        );
        let _ = output(
            git(path).args(["branch", "--set-upstream-to", &format!("{remote}/{branch}")]),
            GIT_LOCAL,
        );
    }
    Ok(())
}

pub fn gh(dir: &Path) -> Command {
    let mut cmd = crate::platform::command("gh");
    cmd.current_dir(dir)
        .env("GH_PROMPT_DISABLED", "1")
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("NO_COLOR", "1");
    cmd
}

/// The pull request for the checked-out branch; None when there is none or `gh`
/// can't tell (not logged in, offline).
pub fn pr_view(dir: &Path) -> Option<PullRequest> {
    let out = output(
        gh(dir).args(["pr", "view", "--json", "url,state,number,isDraft"]),
        GIT_REMOTE,
    )
    .ok()?;
    if !out.status.success() {
        return None;
    }
    serde_json::from_slice(&out.stdout).ok()
}

/// Opens GitHub's "new pull request" page for `branch`. GitHub fills in the
/// title and body from the commits. `--fill` is left out: it makes gh read them
/// from the local clone first, which fails when `origin/<base>` is stale or the
/// branch has no commits of its own yet.
/// A `title` or `base` picked in the dialog goes along; empty leaves it to
/// GitHub.
pub fn pr_create_web(dir: &Path, branch: &str, base: &str, title: &str) -> Result<()> {
    let out = output(
        gh(dir).args(pr_web_args(branch, base.trim(), title.trim())?),
        GIT_REMOTE,
    )?;
    if !out.status.success() {
        let err = strip_ansi(&String::from_utf8_lossy(&out.stderr));
        bail!(
            "{}",
            if err.trim().is_empty() {
                "gh pr create failed"
            } else {
                err.trim()
            }
        );
    }
    Ok(())
}

/// The arguments of `gh pr create --web`.
fn pr_web_args(branch: &str, base: &str, title: &str) -> Result<Vec<String>> {
    // A name that starts with `-` would read as a flag.
    if branch.starts_with('-') || base.starts_with('-') {
        bail!("Name the branch to merge into.");
    }
    let mut args = vec!["pr", "create", "--web", "--head", branch];
    if !base.is_empty() {
        args.extend(["--base", base]);
    }
    if !title.is_empty() {
        args.extend(["--title", title]);
    }
    Ok(args.into_iter().map(str::to_string).collect())
}

/// The owner in a GitHub remote's address: `git@host:owner/repo.git`,
/// `https://host/owner/repo` or `ssh://git@host/owner/repo.git`.
fn remote_owner(url: &str) -> Option<String> {
    let url = url.trim().trim_end_matches('/');
    let path = match url.split_once("://") {
        Some((_, rest)) => rest.split_once('/')?.1,
        None => url.split_once(':')?.1,
    };
    let mut parts = path.rsplitn(2, '/');
    let (_repo, owner) = (parts.next()?, parts.next()?);
    (!owner.is_empty() && !owner.contains('/')).then(|| owner.to_string())
}

/// What `--head` names for `branch`, pushed to `remote`: `owner:branch` when
/// that remote is a fork of the repository `gh` opens the pull request in.
/// An explicit `--head` turns off gh's own look for the fork, and a bare
/// name is then looked up in the base repository. Slow: asks `gh`.
pub fn pr_head(dir: &Path, remote: &str, branch: &str) -> String {
    let pushed_to = push_owner(dir, remote);
    let base_owner = output(
        gh(dir).args(["repo", "view", "--json", "owner", "--jq", ".owner.login"]),
        GIT_REMOTE,
    )
    .ok()
    .filter(|out| out.status.success())
    .map(|out| String::from_utf8_lossy(&out.stdout).trim().to_string())
    .filter(|owner| !owner.is_empty());
    qualified_head(pushed_to.as_deref(), base_owner.as_deref(), branch)
}

/// The owner of the repository a push to `remote` goes to: its push
/// address, which can be a fork while it fetches from upstream.
fn push_owner(dir: &Path, remote: &str) -> Option<String> {
    let url = stdout(git(dir).args(["remote", "get-url", "--push", remote])).ok()?;
    remote_owner(&url)
}

fn qualified_head(pushed_to: Option<&str>, base_owner: Option<&str>, branch: &str) -> String {
    match (pushed_to, base_owner) {
        (Some(fork), Some(base)) if !fork.eq_ignore_ascii_case(base) => format!("{fork}:{branch}"),
        _ => branch.to_string(),
    }
}

/// Whether creating a pull request has to push first: read again right
/// before it, since a commit may have come while the dialog was open.
/// A `remote` that pushes elsewhere than it fetches always pushes: its
/// tracking branch is the fetch side's and says nothing about the push side,
/// and a push of what is already there changes nothing.
pub fn needs_push(dir: &Path, remote: &str) -> Option<bool> {
    let s = sync(dir).ok()?;
    Some(!s.published || s.ahead > 0 || pushes_elsewhere(dir, remote))
}

/// Whether `remote` has a push address other than the one it fetches from.
fn pushes_elsewhere(dir: &Path, remote: &str) -> bool {
    let fetch = stdout(git(dir).args(["remote", "get-url", remote]));
    let push = stdout(git(dir).args(["remote", "get-url", "--push", remote]));
    match (fetch, push) {
        (Ok(fetch), Ok(push)) => fetch.trim() != push.trim(),
        // Not knowing is no reason to skip the push.
        _ => true,
    }
}

/// A commit a new pull request would bring: its subject and the rest of
/// its message.
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct PrCommit {
    pub subject: String,
    pub body: String,
}

/// What the "Create pull request" dialog opens with.
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct PrPlan {
    /// The branch it would merge into.
    pub base: String,
    pub title: String,
    pub body: String,
    /// Commits the base doesn't have; 0 when git couldn't tell.
    pub commits: usize,
    /// The body is the repository's pull request template.
    pub template: bool,
}

/// The branch a remote calls its default (`origin/HEAD`); `main` when the
/// clone doesn't know.
pub fn default_base(dir: &Path, remote: &str) -> String {
    let head = format!("refs/remotes/{remote}/HEAD");
    read(dir, &["symbolic-ref", "--short", &head])
        .ok()
        .and_then(|out| {
            let name = out.trim().strip_prefix(&format!("{remote}/"))?.to_string();
            (!name.is_empty()).then_some(name)
        })
        .unwrap_or_else(|| "main".into())
}

/// The commits on HEAD that `remote/base` doesn't have, oldest first.
pub fn pr_commits(dir: &Path, remote: &str, base: &str) -> Vec<PrCommit> {
    pr_commits_at(dir, remote, base, "HEAD")
}

/// The commit `branch` is on now, to read everything about it from one
/// commit; None when there is no such branch.
pub fn branch_tip(dir: &Path, branch: &str) -> Option<String> {
    let name = format!("refs/heads/{branch}^{{commit}}");
    let out = read(dir, &["rev-parse", "--verify", "--quiet", &name]).ok()?;
    Some(out.trim().to_string()).filter(|id| !id.is_empty())
}

/// [`pr_commits`] of the commit `tip` instead of HEAD.
pub fn pr_commits_at(dir: &Path, remote: &str, base: &str, tip: &str) -> Vec<PrCommit> {
    let range = format!("{remote}/{base}..{tip}");
    let Ok(out) = read(
        dir,
        &["log", "--reverse", "--format=%s%x1f%b%x1e", &range, "--"],
    ) else {
        return Vec::new();
    };
    parse_pr_commits(&out)
}

fn parse_pr_commits(out: &str) -> Vec<PrCommit> {
    out.split('\u{1e}')
        .filter_map(|entry| {
            let (subject, body) = entry.trim_start_matches('\n').split_once('\u{1f}')?;
            Some(PrCommit {
                subject: subject.trim().to_string(),
                body: body.trim().to_string(),
            })
        })
        .filter(|c| !c.subject.is_empty())
        .collect()
}

/// Where GitHub looks for a pull request template, in its order.
const PR_TEMPLATES: [&str; 6] = [
    ".github/pull_request_template.md",
    ".github/PULL_REQUEST_TEMPLATE.md",
    "pull_request_template.md",
    "PULL_REQUEST_TEMPLATE.md",
    "docs/pull_request_template.md",
    "docs/PULL_REQUEST_TEMPLATE.md",
];

/// The repository's pull request template: what GitHub itself would fill
/// in. GitHub reads it from the default branch, so a template changed there
/// after this branch started is the one used. HEAD's only when the clone
/// hasn't fetched the default branch.
pub fn pr_template(dir: &Path, remote: &str) -> Option<String> {
    let default = format!("{remote}/{}", default_base(dir, remote));
    let fetched = read(
        dir,
        &[
            "rev-parse",
            "--verify",
            "--quiet",
            &format!("{default}^{{commit}}"),
        ],
    )
    .is_ok();
    let rev = if fetched { default.as_str() } else { "HEAD" };
    PR_TEMPLATES
        .iter()
        .find_map(|path| show_file(dir, rev, path))
        .filter(|text| !text.trim().is_empty())
}

/// What `tip` changes since it left `remote/base`, for a model to read: the
/// list of files with their counts, and the diff cut at `cap` characters (at
/// the end of a line), with whether it was cut. Empty when git can't tell.
pub fn pr_diff(
    dir: &Path,
    remote: &str,
    base: &str,
    tip: &str,
    cap: usize,
) -> (String, String, bool) {
    let range = format!("{remote}/{base}...{tip}");
    let stat = read(dir, &["diff", "--stat=200", "--no-color", &range, "--"]).unwrap_or_default();
    let diff = read(dir, &["diff", "--no-color", &range, "--"]).unwrap_or_default();
    let (diff, cut) = cut_at_line(diff, cap);
    (stat, diff, cut)
}

/// `text` cut to at most `cap` characters, at the end of a line when it has one.
fn cut_at_line(text: String, cap: usize) -> (String, bool) {
    let Some((end, _)) = text.char_indices().nth(cap) else {
        return (text, false);
    };
    let head = &text[..end];
    let end = head.rfind('\n').map_or(end, |i| i + 1);
    (text[..end].to_string(), true)
}

/// The title and body a pull request of `commits` starts with, as GitHub
/// fills its own form: one commit gives both; several give the first one's
/// subject and the list of them. A template replaces the body.
pub fn pr_fill(commits: &[PrCommit], template: Option<&str>, branch: &str) -> (String, String) {
    let title = match commits.first() {
        Some(c) => c.subject.clone(),
        // No commits known: the branch name reads better than nothing.
        None => branch
            .rsplit('/')
            .next()
            .unwrap_or(branch)
            .replace(['-', '_'], " "),
    };
    let body = match (template, commits) {
        (Some(t), _) => t.trim_end().to_string(),
        (None, [one]) => one.body.clone(),
        (None, many) => many
            .iter()
            .map(|c| format!("- {}", c.subject))
            .collect::<Vec<_>>()
            .join("\n"),
    };
    (title, body)
}

/// Reads what a new pull request for `branch` starts with.
pub fn pr_plan(dir: &Path, remote: &str, branch: &str) -> PrPlan {
    let base = default_base(dir, remote);
    plan_onto(dir, remote, branch, base)
}

/// [`pr_plan`] for another `base` than the default one, as in a stack. None
/// when the remote has no such branch (a name still being typed).
pub fn pr_plan_onto(dir: &Path, remote: &str, branch: &str, base: &str) -> Option<PrPlan> {
    let tip = format!("{remote}/{base}^{{commit}}");
    read(dir, &["rev-parse", "--verify", "--quiet", &tip]).ok()?;
    Some(plan_onto(dir, remote, branch, base.to_string()))
}

fn plan_onto(dir: &Path, remote: &str, branch: &str, base: String) -> PrPlan {
    let commits = pr_commits(dir, remote, &base);
    let template = pr_template(dir, remote);
    let (title, body) = pr_fill(&commits, template.as_deref(), branch);
    PrPlan {
        base,
        title,
        body,
        commits: commits.len(),
        template: template.is_some(),
    }
}

/// The arguments of `gh pr create`; the body goes through stdin.
fn pr_create_args(branch: &str, base: &str, title: &str, draft: bool) -> Vec<String> {
    let mut args: Vec<String> = [
        "pr",
        "create",
        "--head",
        branch,
        "--base",
        base,
        "--title",
        title,
        "--body-file",
        "-",
    ]
    .iter()
    .map(|a| a.to_string())
    .collect();
    if draft {
        args.push("--draft".into());
    }
    args
}

/// Opens a pull request for `branch` into `base` and returns its address.
pub fn pr_create(
    dir: &Path,
    branch: &str,
    base: &str,
    title: &str,
    body: &str,
    draft: bool,
) -> Result<String> {
    let (title, base) = (title.trim(), base.trim());
    if crate::host::github_read_only() {
        bail!("{}", crate::host::get().github_read_only);
    }
    if title.is_empty() {
        bail!("Give the pull request a title.");
    }
    // A name that starts with `-` would read as a flag.
    if base.is_empty() || base.starts_with('-') || branch.starts_with('-') {
        bail!("Name the branch to merge into.");
    }
    // The body goes on stdin: gh may give up before it reads it all, and what it
    // wrote then says why.
    let out = crate::platform::output_within(
        gh(dir).args(pr_create_args(branch, base, title, draft)),
        Some(body.as_bytes()),
        GIT_REMOTE,
    )
    .map_err(|e| {
        if e.kind() == std::io::ErrorKind::TimedOut {
            anyhow!("{e}")
        } else {
            anyhow!("could not run gh: {e}")
        }
    })?;
    if !out.status.success() {
        let err = strip_ansi(&String::from_utf8_lossy(&out.stderr));
        bail!(
            "{}",
            if err.trim().is_empty() {
                "gh pr create failed"
            } else {
                err.trim()
            }
        );
    }
    // gh prints the new pull request's address last.
    let text = String::from_utf8_lossy(&out.stdout);
    let url = text
        .lines()
        .rev()
        .map(str::trim)
        .find(|l| l.starts_with("https://") || l.starts_with("http://"))
        .unwrap_or_default();
    Ok(url.to_string())
}

/// Prints the nice value of the shell that runs it. Tests check that what
/// arakun spawns runs at a lower priority; /proc is read first because `ps`
/// prints "-" for a process under SCHED_IDLE.
#[cfg(unix)]
pub const NICE_OF_SHELL: &str =
    "if [ -r /proc/$$/stat ]; then awk '{print $19}' /proc/$$/stat; else ps -o ni= -p $$; fi";

#[cfg(test)]
mod tests {
    use std::process::Command;

    #[test]
    fn a_branch_on_a_fork_is_named_with_its_owner() {
        use super::{qualified_head, remote_owner};
        for url in [
            "git@github.com:me/arakun.git",
            "https://github.com/me/arakun",
            "https://github.com/me/arakun.git/",
            "ssh://git@github.com/me/arakun.git",
        ] {
            assert_eq!(remote_owner(url).as_deref(), Some("me"), "{url}");
        }
        assert_eq!(remote_owner("/srv/git/arakun.git"), None);
        // Pushed to a fork of the repository the pull request opens in.
        assert_eq!(
            qualified_head(Some("me"), Some("ArakunHQ"), "pr/x"),
            "me:pr/x"
        );
        // The same repository, or one of them unknown: the bare name.
        assert_eq!(
            qualified_head(Some("arakunhq"), Some("ArakunHQ"), "pr/x"),
            "pr/x"
        );
        assert_eq!(qualified_head(None, Some("ArakunHQ"), "pr/x"), "pr/x");
        assert_eq!(qualified_head(Some("me"), None, "pr/x"), "pr/x");
    }

    #[test]
    fn the_browser_form_gets_the_title_and_base_picked() {
        use super::pr_web_args;
        let args = pr_web_args("pr/b", "pr/a", "fix: a thing").unwrap();
        assert_eq!(
            args,
            [
                "pr",
                "create",
                "--web",
                "--head",
                "pr/b",
                "--base",
                "pr/a",
                "--title",
                "fix: a thing"
            ]
        );
        // Nothing picked: GitHub fills them in.
        assert_eq!(pr_web_args("pr/b", "", "").unwrap().len(), 5);
        assert!(pr_web_args("pr/b", "--repo", "t").is_err());
    }

    #[test]
    fn a_pull_request_starts_from_its_commits() {
        use super::{PrCommit, parse_pr_commits, pr_create_args, pr_fill};
        // As `git log --format=%s%x1f%b%x1e` prints them.
        let log = "fix(terminal): keep the tab name\u{1f}It was lost on resize.\n\nSecond paragraph.\n\u{1e}\n\
test: cover the resize\u{1f}\u{1e}\n";
        let commits = parse_pr_commits(log);
        assert_eq!(
            commits,
            [
                PrCommit {
                    subject: "fix(terminal): keep the tab name".into(),
                    body: "It was lost on resize.\n\nSecond paragraph.".into(),
                },
                PrCommit {
                    subject: "test: cover the resize".into(),
                    body: String::new(),
                },
            ]
        );
        assert!(parse_pr_commits("").is_empty());

        // A long diff ends at a line, and says it was cut; characters, not bytes.
        use super::cut_at_line;
        assert_eq!(
            cut_at_line("a\nbb\nccc\n".into(), 20),
            ("a\nbb\nccc\n".into(), false)
        );
        assert_eq!(
            cut_at_line("a\nbb\nccc\n".into(), 6),
            ("a\nbb\n".into(), true)
        );
        assert_eq!(cut_at_line("ééééé".into(), 2), ("éé".into(), true));

        // One commit is the whole pull request; several are listed.
        assert_eq!(
            pr_fill(&commits[..1], None, "pr/tab-name"),
            (
                "fix(terminal): keep the tab name".into(),
                "It was lost on resize.\n\nSecond paragraph.".into()
            )
        );
        assert_eq!(
            pr_fill(&commits, None, "pr/tab-name"),
            (
                "fix(terminal): keep the tab name".into(),
                "- fix(terminal): keep the tab name\n- test: cover the resize".into()
            )
        );
        // The repository's template is the body, as on GitHub's own form.
        assert_eq!(
            pr_fill(&commits, Some("## Problem\n\n"), "pr/tab-name").1,
            "## Problem"
        );
        // Nothing known about the commits: the branch names it.
        assert_eq!(
            pr_fill(&[], None, "pr/tab-name_fix"),
            ("tab name fix".into(), String::new())
        );

        // The title is one argument whatever it holds; the body isn't one at all.
        let args = pr_create_args("pr/x", "main", "fix: `$(rm -rf ~)` \"q\"", true);
        assert_eq!(
            args,
            [
                "pr",
                "create",
                "--head",
                "pr/x",
                "--base",
                "main",
                "--title",
                "fix: `$(rm -rf ~)` \"q\"",
                "--body-file",
                "-",
                "--draft"
            ]
        );
        assert!(!pr_create_args("pr/x", "main", "t", false).contains(&"--draft".to_string()));
    }

    /// A repository on `env/brave-otter` with one commit, and a `taken` branch.
    fn repo() -> std::path::PathBuf {
        // The clock alone can repeat across tests running at once (macOS
        // counts microseconds), and two tests in one folder break each other.
        static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let dir = std::env::temp_dir().join(format!(
            "arakun-rename-{}-{}-{:?}",
            std::process::id(),
            NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        for args in [
            &["init", "-q", "-b", "env/brave-otter"][..],
            &[
                "-c",
                "user.name=t",
                "-c",
                "user.email=t@t",
                // A contributor's commit.gpgsign would need a key the tests don't have.
                "-c",
                "commit.gpgsign=false",
                "commit",
                "-q",
                "--allow-empty",
                "-m",
                "x",
            ],
            &["branch", "taken"],
        ] {
            let ok = super::unpinned(&mut Command::new("git"))
                .arg("-C")
                .arg(&dir)
                .args(args)
                .status()
                .unwrap();
            assert!(ok.success());
        }
        dir
    }

    #[test]
    fn a_long_list_of_files_is_cut_and_says_by_how_many() {
        let stat = " a | 1 +\n b | 2 +-\n 2 files changed, 2 insertions(+)\n c (new file)\n d (new file)\n";
        assert_eq!(
            super::cap_stat(stat.into(), 2),
            " a | 1 +\n b | 2 +-\n … and 2 more files\n"
        );
        // Cut after git's summary line: that line is no file.
        assert_eq!(
            super::cap_stat(stat.into(), 1),
            " a | 1 +\n … and 3 more files\n"
        );
        assert_eq!(super::cap_stat(stat.into(), 10), stat);
        // A path that says "changed" is still a file.
        let stat = " a | 1 +\n 1 file changed, 1 insertion(+)\n what changed/b (new file)\n";
        assert_eq!(
            super::cap_stat(stat.into(), 1),
            " a | 1 +\n … and 1 more files\n"
        );
    }

    #[test]
    fn a_commit_diff_takes_the_staged_or_every_change() {
        let dir = repo();
        std::fs::write(dir.join("staged.txt"), "kept\n").unwrap();
        std::fs::write(dir.join("new.txt"), "fresh\n").unwrap();
        super::stage(&dir, "staged.txt").unwrap();

        let (stat, diff, cut) = super::commit_diff(&dir, false, 10_000).unwrap();
        assert!(
            stat.contains("staged.txt") && !stat.contains("new.txt"),
            "{stat}"
        );
        assert!(diff.contains("+kept") && !diff.contains("+fresh"), "{diff}");
        assert!(!cut);

        let (stat, diff, _) = super::commit_diff(&dir, true, 10_000).unwrap();
        assert!(stat.contains("new.txt (new file)"), "{stat}");
        assert!(diff.contains("+kept") && diff.contains("+fresh"), "{diff}");

        // Past the cap, the files are still all listed.
        let (stat, _, cut) = super::commit_diff(&dir, true, 10).unwrap();
        assert!(cut && stat.contains("new.txt"), "{stat}");

        // Under the cap in characters but over it in bytes: new files still
        // come in, and nothing is cut.
        std::fs::write(dir.join("staged.txt"), "é".repeat(400)).unwrap();
        super::stage(&dir, "staged.txt").unwrap();
        let (_, diff, cut) = super::commit_diff(&dir, true, 700).unwrap();
        assert!(diff.contains("+fresh") && !cut, "{diff}");

        // A new file too large for the cap is listed, not read, and the diff
        // says it is cut.
        std::fs::write(dir.join("big.txt"), "x".repeat(5_000)).unwrap();
        let (stat, diff, cut) = super::commit_diff(&dir, true, 1_000).unwrap();
        assert!(stat.contains("big.txt (new file)"), "{stat}");
        assert!(!diff.contains("xxxx") && cut, "{diff}");

        assert_eq!(super::recent_subjects(&dir, 5), vec!["x".to_string()]);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn stage_only_works_from_a_folder_inside_the_repository() {
        let dir = repo();
        let sub = dir.join("sub");
        std::fs::create_dir_all(&sub).unwrap();
        std::fs::write(sub.join("a.txt"), "a").unwrap();
        std::fs::write(sub.join("b.txt"), "b").unwrap();
        let staged = super::stage_only(&sub, &["a.txt".into(), "gone.txt".into()]).unwrap();
        assert_eq!(staged, ["sub/a.txt"]);
        let index =
            super::stdout(super::git(&dir).args(["diff", "--cached", "--name-only"])).unwrap();
        assert_eq!(index.trim(), "sub/a.txt");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_template_comes_from_the_default_branch() {
        let dir = repo();
        let git = |args: &[&str]| {
            let ok = super::unpinned(&mut Command::new("git"))
                .arg("-C")
                .arg(&dir)
                .args(["-c", "user.name=t", "-c", "user.email=t@t"])
                .args(["-c", "commit.gpgsign=false"])
                .args(args)
                .status()
                .unwrap();
            assert!(ok.success(), "{args:?}");
        };
        let template = |text: &str| {
            let path = dir.join(".github/pull_request_template.md");
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, text).unwrap();
            git(&["add", "."]);
            git(&["commit", "-q", "-m", text]);
        };
        template("old");
        // Nothing fetched: HEAD's is all there is.
        assert_eq!(super::pr_template(&dir, "origin").as_deref(), Some("old"));
        // The default branch changed it after this branch started.
        git(&["switch", "-q", "-c", "main"]);
        template("new");
        git(&["update-ref", "refs/remotes/origin/main", "main"]);
        git(&["switch", "-q", "env/brave-otter"]);
        assert_eq!(super::pr_template(&dir, "origin").as_deref(), Some("new"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_head_owner_is_the_one_a_push_goes_to() {
        let dir = repo();
        let git = |args: &[&str]| {
            let ok = super::unpinned(&mut Command::new("git"))
                .arg("-C")
                .arg(&dir)
                .args(args)
                .status()
                .unwrap();
            assert!(ok.success(), "{args:?}");
        };
        git(&[
            "remote",
            "add",
            "origin",
            "https://github.com/ArakunHQ/arakun.git",
        ]);
        assert_eq!(
            super::push_owner(&dir, "origin").as_deref(),
            Some("ArakunHQ")
        );
        // Fetches from upstream, pushes to a fork.
        git(&[
            "remote",
            "set-url",
            "--push",
            "origin",
            "git@github.com:me/arakun.git",
        ]);
        assert_eq!(super::push_owner(&dir, "origin").as_deref(), Some("me"));
        assert_eq!(super::push_owner(&dir, "nowhere"), None);
        // Its tracking branch is upstream's, so the fork always gets the push.
        assert!(super::pushes_elsewhere(&dir, "origin"));
        assert!(super::pushes_elsewhere(&dir, "nowhere"));
        git(&[
            "remote",
            "set-url",
            "--push",
            "origin",
            "https://github.com/ArakunHQ/arakun.git",
        ]);
        assert!(!super::pushes_elsewhere(&dir, "origin"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn reads_porcelain_v2_status() {
        use super::{ChangeKind, Head, parse_status};
        let text = [
            "# branch.oid 1234",
            "# branch.head main",
            "# branch.upstream origin/main",
            "# branch.ab +2 -3",
            "1 .M N... 100644 100644 100644 aaa aaa with space.rs",
            "1 A. N... 000000 100644 100644 000 bbb new.rs",
            "1 .D N... 100644 100644 000000 ccc ccc gone.rs",
            "2 R. N... 100644 100644 100644 ddd ddd R100 moved.rs",
            "was.rs",
            "u UU N... 100644 100644 100644 100644 e e e both.rs",
            "? untracked dir/a b.txt",
            "! ignored.log",
            "",
        ]
        .join("\0");
        let (changes, head) = parse_status(&text);
        assert_eq!(
            head,
            Head {
                born: true,
                branch: Some("main".into()),
                upstream: Some("origin/main".into()),
                ab: Some((2, 3)),
            }
        );
        let got: Vec<_> = changes
            .iter()
            .map(|c| (c.path.as_str(), c.kind, c.staged, c.unstaged))
            .collect();
        assert_eq!(
            got,
            [
                ("with space.rs", ChangeKind::Modified, false, true),
                ("new.rs", ChangeKind::Added, true, false),
                ("gone.rs", ChangeKind::Deleted, false, true),
                ("moved.rs", ChangeKind::Renamed, true, false),
                ("both.rs", ChangeKind::Modified, true, true),
                ("untracked dir/a b.txt", ChangeKind::Untracked, false, true),
            ]
        );
        let (_, head) = parse_status("# branch.oid (initial)\0# branch.head (detached)\0");
        assert!(!head.born);
        assert_eq!(head.branch, None);
    }

    #[test]
    fn status_and_sync_follow_the_files_and_the_remote() {
        let dir = repo();
        let git = |args: &[&str]| {
            let ok = super::unpinned(&mut Command::new("git"))
                .arg("-C")
                .arg(&dir)
                .args(args)
                .status()
                .unwrap();
            assert!(ok.success(), "git {args:?}");
        };
        let (changes, sync) = super::status_and_sync(&dir).unwrap();
        assert!(changes.is_empty());
        assert_eq!(sync.branch.as_deref(), Some("env/brave-otter"));
        assert_eq!(sync.remote, None);

        // An untracked file is read again once it changes, not before.
        std::fs::write(dir.join("notes.txt"), "a\nb\n").unwrap();
        let (changes, _) = super::status_and_sync(&dir).unwrap();
        assert_eq!(
            (changes[0].path.as_str(), changes[0].added),
            ("notes.txt", 2)
        );
        std::fs::write(dir.join("notes.txt"), "a\nb\nc\n").unwrap();
        let (changes, _) = super::status_and_sync(&dir).unwrap();
        assert_eq!(changes[0].added, 3);

        // A remote added after the first read is seen, and so is the upstream.
        let bare = dir.with_extension("remote.git");
        let ok = super::unpinned(&mut Command::new("git"))
            .args(["init", "-q", "--bare"])
            .arg(&bare)
            .status()
            .unwrap();
        assert!(ok.success());
        git(&["remote", "add", "origin", bare.to_str().unwrap()]);
        git(&["push", "-q", "-u", "origin", "env/brave-otter"]);
        git(&[
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@t",
            "-c",
            "commit.gpgsign=false",
            "commit",
            "-q",
            "--allow-empty",
            "-m",
            "y",
        ]);
        let (_, sync) = super::status_and_sync(&dir).unwrap();
        assert_eq!(sync.remote.as_deref(), Some("origin"));
        assert_eq!(sync.upstream.as_deref(), Some("origin/env/brave-otter"));
        assert!(sync.published);
        assert_eq!((sync.ahead, sync.behind), (1, 0));

        // Deleted on the remote: no upstream, and ahead counts what no remote has.
        git(&["push", "-q", "origin", ":env/brave-otter"]);
        let (_, sync) = super::status_and_sync(&dir).unwrap();
        assert_eq!(sync.upstream, None);
        assert!(!sync.published);
        assert_eq!(sync.ahead, 2);

        // Before the first commit, changes diff against the empty tree.
        git(&["checkout", "-q", "--orphan", "fresh"]);
        let (changes, sync) = super::status_and_sync(&dir).unwrap();
        assert_eq!(sync.branch.as_deref(), Some("fresh"));
        assert!(changes.iter().all(|c| c.path == "notes.txt"));
        let _ = std::fs::remove_dir_all(&bare);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_repository_has_commits_after_its_first() {
        let dir = repo();
        assert!(super::has_commits(&dir).unwrap());
        let empty = dir.join("empty");
        std::fs::create_dir_all(&empty).unwrap();
        let ok = super::unpinned(&mut Command::new("git"))
            .arg("-C")
            .arg(&empty)
            .args(["init", "-q"])
            .status()
            .unwrap();
        assert!(ok.success());
        assert!(!super::has_commits(&empty).unwrap());
        // Not a repository: git can't tell, which is no sign of a new one.
        let plain = std::env::temp_dir().join(format!("arakun-plain-{}", std::process::id()));
        std::fs::create_dir_all(&plain).unwrap();
        // Skipped when the temp dir is itself inside a repository.
        let out = super::git(&plain)
            .args(["rev-parse", "--verify", "--quiet", "HEAD"])
            .status()
            .unwrap();
        if out.code() == Some(128) {
            assert!(super::has_commits(&plain).is_err());
        }
        let _ = std::fs::remove_dir_all(&plain);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_pull_request_is_read_from_its_branch_not_from_head() {
        let dir = repo();
        let git = |args: &[&str]| {
            let ok = super::unpinned(&mut Command::new("git"))
                .arg("-C")
                .arg(&dir)
                .args(["-c", "user.name=t", "-c", "user.email=t@t"])
                .args(["-c", "commit.gpgsign=false"])
                .args(args)
                .status()
                .unwrap();
            assert!(ok.success(), "{args:?}");
        };
        git(&["update-ref", "refs/remotes/origin/main", "HEAD"]);
        git(&["switch", "-q", "taken"]);
        git(&["commit", "-q", "--allow-empty", "-m", "feat: on taken"]);
        // Another branch is checked out while the dialog for `taken` is open.
        git(&["switch", "-q", "env/brave-otter"]);
        let tip = super::branch_tip(&dir, "taken").unwrap();
        let subjects: Vec<String> = super::pr_commits_at(&dir, "origin", "main", &tip)
            .into_iter()
            .map(|c| c.subject)
            .collect();
        assert_eq!(subjects, ["feat: on taken"]);
        assert!(super::pr_commits(&dir, "origin", "main").is_empty());
        assert_eq!(super::branch_tip(&dir, "no-such-branch"), None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn clones_once_and_leaves_nothing_when_it_fails() {
        let source = repo();
        let into = source.with_extension("clones");
        let dest = into.join("copy");
        super::clone(source.to_str().unwrap(), false, &dest).unwrap();
        assert!(dest.join(".git").exists());
        assert_eq!(super::origin_url(&dest).as_deref(), source.to_str());
        // An existing folder is never cloned into.
        assert!(super::clone(source.to_str().unwrap(), false, &dest).is_err());
        assert!(dest.join(".git").exists());
        let gone = into.join("gone");
        assert!(super::clone(into.join("nothing").to_str().unwrap(), false, &gone).is_err());
        assert!(!gone.exists());
        let _ = std::fs::remove_dir_all(&into);
        let _ = std::fs::remove_dir_all(&source);
    }

    #[test]
    fn renames_the_branch_once() {
        let dir = repo();
        assert_eq!(
            super::rename_branch(&dir, "env/brave-otter", "fix/tab-name", false).unwrap(),
            "fix/tab-name"
        );
        // Someone renamed it already: leave it alone.
        assert!(super::rename_branch(&dir, "env/brave-otter", "fix/other", false).is_err());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn rename_refuses_a_branch_pushed_without_upstream() {
        let dir = repo();
        let remote = dir.with_extension("remote");
        let git = |args: &[&str]| {
            super::unpinned(&mut Command::new("git"))
                .args(args)
                .status()
                .unwrap()
                .success()
        };
        assert!(git(&["init", "-q", "--bare", remote.to_str().unwrap()]));
        let d = dir.to_str().unwrap();
        assert!(git(&[
            "-C",
            d,
            "remote",
            "add",
            "origin",
            remote.to_str().unwrap()
        ]));
        assert!(git(&["-C", d, "push", "-q", "origin", "env/brave-otter"]));
        assert!(super::rename_branch(&dir, "env/brave-otter", "fix/x", true).is_err());
        let _ = std::fs::remove_dir_all(dir);
        let _ = std::fs::remove_dir_all(remote);
    }

    #[test]
    fn only_new_never_checks_out_an_existing_branch() {
        let dir = repo();
        let tree = dir.with_extension("tree");
        assert!(super::add_worktree(&dir, &tree, "taken", None, true).is_err());
        assert!(!tree.join(".git").exists());
        super::add_worktree(&dir, &tree, "taken", None, false).unwrap();
        assert_eq!(super::current_branch(&tree).as_deref(), Some("taken"));
        let _ = std::fs::remove_dir_all(dir);
        let _ = std::fs::remove_dir_all(tree);
    }

    #[test]
    fn a_stalled_command_is_stopped_at_the_limit() {
        use std::time::Duration;
        let mut cmd = crate::platform::command("git");
        assert!(
            super::status_within(cmd.arg("--version"), Duration::from_secs(5))
                .is_some_and(|s| s.success())
        );
        #[cfg(unix)]
        {
            let mut sleep = Command::new("sleep");
            sleep.arg("30");
            let start = std::time::Instant::now();
            assert!(super::status_within(&mut sleep, Duration::from_millis(200)).is_none());
            assert!(start.elapsed() < Duration::from_secs(10));
            // What the command started goes too, as ssh under git fetch would.
            let pid_file =
                std::env::temp_dir().join(format!("arakun-status-within-{}", std::process::id()));
            let mut sh = Command::new("sh");
            sh.arg("-c")
                .arg("sleep 30 & echo $! > \"$1\"; wait")
                .arg("sh")
                .arg(&pid_file);
            assert!(super::status_within(&mut sh, Duration::from_millis(500)).is_none());
            let pid = std::fs::read_to_string(&pid_file).unwrap();
            let _ = std::fs::remove_file(&pid_file);
            std::thread::sleep(Duration::from_millis(200));
            let alive = Command::new("kill")
                .args(["-0", pid.trim()])
                .status()
                .unwrap()
                .success();
            assert!(!alive, "the grandchild still runs");
        }
    }

    #[test]
    fn a_remote_names_its_repository() {
        for url in [
            "git@github.com:ArakunHQ/arakun.git",
            "https://github.com/ArakunHQ/arakun",
            "https://github.com/ArakunHQ/arakun.git/",
            "ssh://git@github.com/ArakunHQ/arakun.git",
        ] {
            assert_eq!(
                super::remote_slug(url).as_deref(),
                Some("ArakunHQ/arakun"),
                "{url}"
            );
        }
        assert_eq!(super::remote_slug("/tmp/repo"), None);
    }

    #[test]
    fn a_remote_is_found_by_where_it_fetches() {
        let dir = repo();
        let git = |args: &[&str]| {
            assert!(
                super::git(&dir).args(args).status().unwrap().success(),
                "{args:?}"
            )
        };
        git(&["remote", "add", "origin", "git@github.com:up/r.git"]);
        git(&[
            "remote",
            "set-url",
            "--push",
            "origin",
            "git@github.com:me/r.git",
        ]);
        git(&["remote", "add", "mine", "https://github.com/me/r"]);
        assert_eq!(super::remote_for(&dir, "UP/r").as_deref(), Some("origin"));
        // origin pushes to me/r but fetches up/r: its pull requests aren't there.
        assert_eq!(super::remote_for(&dir, "me/r").as_deref(), Some("mine"));
        assert_eq!(super::remote_for(&dir, "other/r"), None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_pull_request_gets_a_worktree_on_its_branch() {
        let dir = repo();
        let remote = dir.with_extension("remote");
        let git = |args: &[&str]| {
            super::unpinned(&mut Command::new("git"))
                .args(args)
                .status()
                .unwrap()
                .success()
        };
        let (d, r) = (dir.to_str().unwrap(), remote.to_str().unwrap());
        assert!(git(&["init", "-q", "--bare", "-b", "main", r]));
        assert!(git(&["-C", d, "remote", "add", "origin", r]));
        // GitHub keeps each pull request's head under refs/pull, a fork's too.
        assert!(git(&[
            "-C",
            d,
            "push",
            "-q",
            "origin",
            "HEAD:refs/heads/feature",
            "HEAD:refs/pull/7/head",
        ]));
        let rev = |dir: &std::path::Path, r: &str| {
            super::stdout(super::git(dir).args(["rev-parse", r])).unwrap()
        };
        // No local branch: made from the pull request's head, following origin.
        let tree = dir.with_extension("feature");
        super::add_pull_worktree(&dir, &tree, "origin", 7, "feature", true).unwrap();
        assert_eq!(rev(&tree, "HEAD"), rev(&dir, "HEAD"));
        assert_eq!(
            super::read(&tree, &["branch", "--show-current"])
                .unwrap()
                .trim(),
            "feature"
        );
        assert_eq!(rev(&tree, "@{u}"), rev(&dir, "refs/remotes/origin/feature"));
        // A fork's branch under its own name: nothing to follow.
        let fork = dir.with_extension("fork");
        super::add_pull_worktree(&dir, &fork, "origin", 7, "pr-7", false).unwrap();
        assert!(
            super::git(&fork)
                .args(["rev-parse", "@{u}"])
                .output()
                .is_ok_and(|o| !o.status.success())
        );
        // A local branch is checked out as it is.
        let taken = dir.with_extension("taken");
        super::add_pull_worktree(&dir, &taken, "origin", 8, "taken", true).unwrap();
        assert_eq!(rev(&taken, "HEAD"), rev(&dir, "refs/heads/taken"));
        for p in [&dir, &remote, &tree, &fork, &taken] {
            let _ = std::fs::remove_dir_all(p);
        }
    }

    #[test]
    fn new_environments_start_from_origin() {
        let dir = repo();
        // No origin and no local main: the current commit.
        assert_eq!(super::new_env_base(&dir, None), None);
        let remote = dir.with_extension("remote");
        let tree = dir.with_extension("tree");
        let git = |args: &[&str]| {
            super::unpinned(&mut Command::new("git"))
                .args(args)
                .status()
                .unwrap()
                .success()
        };
        let d = dir.to_str().unwrap();
        let r = remote.to_str().unwrap();
        assert!(git(&["init", "-q", "--bare", "-b", "main", r]));
        assert!(git(&["-C", d, "remote", "add", "origin", r]));
        assert!(git(&[
            "-C",
            d,
            "push",
            "-q",
            "origin",
            "env/brave-otter:main"
        ]));
        assert!(git(&[
            "-C",
            d,
            "push",
            "-q",
            "origin",
            "env/brave-otter:develop"
        ]));
        // A local commit that is not on origin must not be in the new environment.
        assert!(git(&[
            "-C",
            d,
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@t",
            "-c",
            "commit.gpgsign=false",
            "commit",
            "-q",
            "--allow-empty",
            "-m",
            "local",
        ]));
        // A local branch with the remote branch's short name must not confuse it.
        assert!(git(&["-C", d, "branch", "origin/main"]));
        // Origin's fetch refspec leaves the branch out: it is fetched all the same.
        assert!(git(&[
            "-C",
            d,
            "config",
            "remote.origin.fetch",
            "+refs/heads/other:refs/remotes/origin/other"
        ]));
        // No origin/HEAD here: origin says its default is main, not the current branch.
        let base = super::new_env_base(&dir, None);
        assert_eq!(base.as_deref(), Some("refs/remotes/origin/main"));
        super::add_worktree(&dir, &tree, "fresh", base.as_deref(), true).unwrap();
        let rev = |dir: &std::path::Path, r: &str| {
            super::stdout(super::git(dir).args(["rev-parse", r])).unwrap()
        };
        assert_eq!(rev(&tree, "HEAD"), rev(&dir, "refs/remotes/origin/main"));
        assert_ne!(rev(&tree, "HEAD"), rev(&dir, "HEAD"));
        // Not tracking origin/main: a push must not go there.
        assert!(
            super::git(&tree)
                .args(["rev-parse", "@{u}"])
                .output()
                .is_ok_and(|o| !o.status.success())
        );
        // A base branch set by the user, typed either way.
        for typed in ["develop", "origin/develop", " develop "] {
            assert_eq!(
                super::new_env_base(&dir, Some(typed)).as_deref(),
                Some("refs/remotes/origin/develop")
            );
        }
        // Only local: the local branch.
        assert_eq!(
            super::new_env_base(&dir, Some("taken")).as_deref(),
            Some("refs/heads/taken")
        );
        // Deleted on origin: the copy fetched earlier is stale, and there is no
        // local one, so start from the current commit.
        assert!(git(&["--git-dir", r, "branch", "-D", "develop"]));
        assert_eq!(super::new_env_base(&dir, Some("develop")), None);
        let _ = std::fs::remove_dir_all(&tree);
        let _ = std::fs::remove_dir_all(dir);
        let _ = std::fs::remove_dir_all(remote);
    }

    #[test]
    fn reads_origins_default_branch() {
        assert_eq!(
            super::symref_head("ref: refs/heads/trunk\tHEAD\nabc123\tHEAD\n").as_deref(),
            Some("trunk")
        );
        assert_eq!(super::symref_head("abc123\tHEAD\n"), None);
        assert_eq!(super::base_branch_name(" origin/main "), "main");
        assert_eq!(super::base_branch_name("release/1.0"), "release/1.0");
    }

    #[test]
    fn reads_the_current_branch() {
        let dir = repo();
        assert_eq!(
            super::current_branch(&dir).as_deref(),
            Some("env/brave-otter")
        );
        let ok = super::unpinned(&mut Command::new("git"))
            .arg("-C")
            .arg(&dir)
            .args(["checkout", "-q", "--detach"])
            .status()
            .unwrap();
        assert!(ok.success());
        assert_eq!(super::current_branch(&dir), None);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn rename_picks_a_free_name_or_fails() {
        let dir = repo();
        assert!(super::rename_branch(&dir, "env/brave-otter", "taken", false).is_err());
        assert_eq!(
            super::rename_branch(&dir, "env/brave-otter", "taken", true).unwrap(),
            "taken-2"
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn rename_refuses_a_pushed_branch() {
        let dir = repo();
        let ok = super::unpinned(&mut Command::new("git"))
            .arg("-C")
            .arg(&dir)
            .args([
                "config",
                "branch.env/brave-otter.merge",
                "refs/heads/env/brave-otter",
            ])
            .status()
            .unwrap();
        assert!(ok.success());
        assert!(super::rename_branch(&dir, "env/brave-otter", "fix/x", true).is_err());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[cfg(unix)]
    #[test]
    fn a_hung_git_call_ends_with_an_error_that_names_it() {
        let dir = std::env::temp_dir();
        let started = std::time::Instant::now();
        let err = super::run(
            super::git(&dir).args(["-c", "alias.hang=!sleep 30", "hang"]),
            std::time::Duration::from_millis(500),
        )
        .unwrap_err();
        assert!(err.to_string().starts_with("git hang took longer"), "{err}");
        assert!(started.elapsed() < std::time::Duration::from_secs(10));
    }
}
