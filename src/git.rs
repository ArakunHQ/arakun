//! Thin wrappers over the git CLI. Everything here blocks; callers run it on the
//! background executor. `GIT_OPTIONAL_LOCKS=0` keeps our polling `status` from taking
//! the index lock while an agent or terminal is committing in the same worktree.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use anyhow::{Context as _, Result, anyhow, bail};

/// Git's well-known empty tree, the diff base for repositories without commits.
const EMPTY_TREE: &str = "4b825dc642cb6eb9a060e54bf8d69288fbee4904";

pub(crate) fn git(dir: &Path) -> Command {
    let mut cmd = crate::platform::command("git");
    cmd.current_dir(dir)
        .env("GIT_OPTIONAL_LOCKS", "0")
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("LC_ALL", "C");
    // A status scan of a big repository shouldn't compete with the window for the CPU.
    crate::platform::lower_priority(&mut cmd);
    // `dir` picks the repository, not a GIT_DIR inherited from whatever started arakun.
    unpinned(&mut cmd);
    cmd
}

/// Clears the variables that pin git to one repository. In a linked worktree, git
/// gives hooks GIT_DIR (and GIT_INDEX_FILE) pointing at the real repository; a
/// test run from a hook inherits them, and its `git init` and `git commit` would
/// land on the branch being pushed instead of in its temporary folder.
pub(crate) fn unpinned(cmd: &mut Command) -> &mut Command {
    for var in ["GIT_DIR", "GIT_WORK_TREE", "GIT_INDEX_FILE"] {
        cmd.env_remove(var);
    }
    cmd
}

fn run(cmd: &mut Command) -> Result<Output> {
    let out = cmd.output().context("could not run git")?;
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
    Ok(String::from_utf8_lossy(&run(cmd)?.stdout).into_owned())
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
    run(git(dir).args(["init", "-q"]))?;
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
        );
        if done.is_ok() {
            return Ok(());
        }
        clean();
    }
    run_loud(git(parent).args(["clone", "--quiet", "--", url]).arg(dest))
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
        && git(repo)
            .args([
                "show-ref",
                "--verify",
                "--quiet",
                &format!("refs/heads/{branch}"),
            ])
            .status()
            .is_ok_and(|s| s.success());
    let mut cmd = git(repo);
    cmd.args(["worktree", "add"]);
    if exists {
        cmd.arg(path).arg(branch);
    } else {
        cmd.arg("-b").arg(branch).arg(path);
        if let Some(base) = base {
            cmd.arg(base);
        }
    }
    run(&mut cmd)?;
    Ok(())
}

pub fn remove_worktree(repo: &Path, path: &Path, delete_branch: Option<&str>) -> Result<()> {
    run(git(repo).args(["worktree", "remove", "--force"]).arg(path))?;
    let _ = run(git(repo).args(["worktree", "prune"]));
    if let Some(branch) = delete_branch {
        run(git(repo).args(["branch", "-D", branch]))?;
    }
    Ok(())
}

pub fn branch_name_ok(repo: &Path, name: &str) -> bool {
    git(repo)
        .args(["check-ref-format", "--branch", name])
        .output()
        .is_ok_and(|o| o.status.success())
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
    let upstream = git(dir)
        .args(["config", "--get", &format!("branch.{old}.merge")])
        .output()
        .is_ok_and(|o| o.status.success());
    let tracked = stdout(git(dir).args([
        "for-each-ref",
        "--format=%(refname)",
        &format!("refs/remotes/*/{old}"),
    ]))
    .is_ok_and(|refs| !refs.trim().is_empty());
    if upstream || tracked {
        bail!("{old} was already pushed; rename it on the remote too, or keep the name");
    }
    if !branch_name_ok(dir, new) {
        bail!("\"{new}\" is not a valid branch name");
    }
    let exists = |name: &str| {
        git(dir)
            .args([
                "show-ref",
                "--verify",
                "--quiet",
                &format!("refs/heads/{name}"),
            ])
            .status()
            .is_ok_and(|s| s.success())
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
    run(git(dir).args(["branch", "-m", old, &name]))?;
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
    run(git(dir).args(["fetch", "--all", "--quiet"]))?;
    Ok(())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChangeKind {
    Modified,
    Added,
    Deleted,
    Renamed,
    Untracked,
}

#[derive(Clone, Debug, PartialEq, Eq)]
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

fn diff_base(dir: &Path) -> &'static str {
    let has_head = git(dir)
        .args(["rev-parse", "--verify", "--quiet", "HEAD"])
        .output()
        .is_ok_and(|o| o.status.success());
    if has_head { "HEAD" } else { EMPTY_TREE }
}

/// Uncommitted changes against HEAD (staged and unstaged together, as the Changes
/// list shows them), with line counts. Untracked files count all their lines as added.
pub fn status(dir: &Path) -> Result<Vec<Change>> {
    let text = stdout(git(dir).args([
        "status",
        "--porcelain=v1",
        "-z",
        "--untracked-files=all",
        "--no-renames",
    ]))?;
    let mut changes = Vec::new();
    let mut parts = text.split('\0').filter(|s| !s.is_empty());
    while let Some(entry) = parts.next() {
        if entry.len() < 4 {
            continue;
        }
        let (code, path) = entry.split_at(3);
        let code = code.as_bytes();
        let kind = match (code[0], code[1]) {
            (b'?', b'?') => ChangeKind::Untracked,
            (b'A', _) => ChangeKind::Added,
            (b'D', _) | (_, b'D') => ChangeKind::Deleted,
            (b'R', _) => ChangeKind::Renamed,
            _ => ChangeKind::Modified,
        };
        if code[0] == b'R' || code[0] == b'C' {
            parts.next();
        }
        let untracked = kind == ChangeKind::Untracked;
        changes.push(Change {
            path: path.to_string(),
            kind,
            added: 0,
            removed: 0,
            staged: !untracked && code[0] != b' ',
            unstaged: untracked || code[1] != b' ',
        });
    }
    if changes.is_empty() {
        return Ok(changes);
    }

    let numstat =
        stdout(git(dir).args(["diff", diff_base(dir), "--numstat", "-z", "--no-renames"]))
            .unwrap_or_default();
    let mut counts = std::collections::HashMap::new();
    for rec in numstat.split('\0').filter(|s| !s.is_empty()) {
        let mut f = rec.splitn(3, '\t');
        let (Some(a), Some(r), Some(p)) = (f.next(), f.next(), f.next()) else {
            continue;
        };
        counts.insert(
            p.to_string(),
            (a.parse().unwrap_or(0), r.parse().unwrap_or(0)),
        );
    }
    for c in &mut changes {
        if c.kind == ChangeKind::Untracked {
            c.added = count_lines(&dir.join(&c.path));
        } else if let Some(&(a, r)) = counts.get(&c.path) {
            c.added = a;
            c.removed = r;
        }
    }
    changes.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(changes)
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
        let out = git(dir)
            .args(["diff", "--no-color", "--no-index", "--", "/dev/null", path])
            .output()
            .context("could not run git")?;
        // --no-index exits 1 when the files differ, which is the expected case.
        return Ok(String::from_utf8_lossy(&out.stdout).into_owned());
    }
    stdout(git(dir).args(["diff", "--no-color", diff_base(dir), "--", path]))
}

// ---------------------------------------------------------------------------
// Workflow: remotes, commit, push/pull, staging, merge, pull requests.

/// Like `run`, but a failure carries everything the command printed: commit hooks,
/// pushes and merges often explain themselves on stdout.
fn run_loud(cmd: &mut Command) -> Result<String> {
    let out = cmd.output().context("could not run git")?;
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

pub(crate) fn strip_ansi(s: &str) -> String {
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

fn has_head(dir: &Path) -> bool {
    diff_base(dir) == "HEAD"
}

/// Where the checked-out branch stands against its remote.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
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

pub fn sync(dir: &Path) -> Result<Sync> {
    let mut s = Sync::default();
    // Exits 1 when there are no remotes at all.
    let urls = git(dir)
        .args(["config", "--get-regexp", r"^remote\..*\.url$"])
        .output()
        .context("could not run git")?;
    let mut remotes: Vec<(String, String)> = Vec::new();
    for line in String::from_utf8_lossy(&urls.stdout).lines() {
        let Some((key, url)) = line.split_once(' ') else {
            continue;
        };
        if let Some(name) = key
            .strip_prefix("remote.")
            .and_then(|k| k.strip_suffix(".url"))
        {
            remotes.push((name.to_string(), url.trim().to_string()));
        }
    }
    let head = git(dir)
        .args(["symbolic-ref", "-q", "--short", "HEAD"])
        .output();
    s.branch = head
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .filter(|b| !b.is_empty());
    if remotes.is_empty() {
        return Ok(s);
    }
    s.gh = gh_available();
    let Some(branch) = s.branch.clone() else {
        return Ok(s);
    };

    let track = stdout(git(dir).args([
        "for-each-ref",
        "--format=%(upstream:short)%00%(upstream:track,nobracket)",
        &format!("refs/heads/{branch}"),
    ]))
    .unwrap_or_default();
    let (upstream, track) = track.trim_end().split_once('\0').unwrap_or(("", ""));
    if !upstream.is_empty() && track != "gone" {
        s.upstream = Some(upstream.to_string());
    }
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
    let mut track_ahead = 0;
    for part in track.split(',') {
        let part = part.trim();
        if let Some(n) = part.strip_prefix("ahead ") {
            track_ahead = n.parse().unwrap_or(0);
        } else if let Some(n) = part.strip_prefix("behind ") {
            s.behind = n.parse().unwrap_or(0);
        }
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
    *GH.get_or_init(|| {
        crate::platform::command("gh")
            .arg("--version")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .is_ok_and(|s| s.success())
    })
}

/// `git add -A` first when `all`, then commits with the given message.
pub fn commit(dir: &Path, message: &str, all: bool) -> Result<()> {
    if all {
        run_loud(git(dir).args(["add", "-A"]))?;
    }
    if let Err(e) = run_loud(git(dir).args(["commit", "-q", "-m", message])) {
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
    run_loud(git(dir).args(["push", "-u", remote, branch]))?;
    Ok(())
}

pub fn pull(dir: &Path) -> Result<()> {
    run_loud(git(dir).args(["pull", "--ff-only"]))?;
    Ok(())
}

pub fn stage(dir: &Path, path: &str) -> Result<()> {
    run_loud(git_paths(dir).args(["add", "--"]).arg(path))?;
    Ok(())
}

/// Stages the changes git sees to `files` (relative to `dir`), and only those;
/// returns the ones it staged.
pub fn stage_only(dir: &Path, files: &[String]) -> Result<Vec<String>> {
    let changed: std::collections::HashSet<String> =
        status(dir)?.into_iter().map(|c| c.path).collect();
    let picked: Vec<String> = files
        .iter()
        .filter(|f| changed.contains(*f))
        .cloned()
        .collect();
    if !picked.is_empty() {
        run_loud(git_paths(dir).args(["add", "-A", "--"]).args(&picked))?;
    }
    Ok(picked)
}

pub fn unstage(dir: &Path, path: &str) -> Result<()> {
    if has_head(dir) {
        run_loud(git_paths(dir).args(["restore", "--staged", "--"]).arg(path))?;
    } else {
        run_loud(
            git_paths(dir)
                .args(["rm", "-q", "--cached", "--"])
                .arg(path),
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
    if has_head(dir) {
        run_loud(
            git_paths(dir)
                .args(["restore", "--source=HEAD", "--staged", "--worktree", "--"])
                .arg(&change.path),
        )?;
    } else {
        run_loud(
            git_paths(dir)
                .args(["rm", "-q", "-f", "--cached", "--"])
                .arg(&change.path),
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
    let text = stdout(git(dir).args(["status", "--porcelain=v1", "-z", "--untracked-files=no"]))?;
    Ok(text
        .split('\0')
        .filter(|s| s.len() > 3 && !s.starts_with("??"))
        .count())
}

pub fn merge_plan(main: &Path, branch: &str) -> Result<MergePlan> {
    let head = git(main)
        .args(["symbolic-ref", "-q", "--short", "HEAD"])
        .output()?;
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
    if let Err(e) = run_loud(git(main).args(["merge", "--no-ff", "--no-edit", branch])) {
        let _ = git(main).args(["merge", "--abort"]).output();
        bail!("{e}\nThe merge was aborted; nothing changed.");
    }
    Ok(())
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Deserialize)]
pub struct PullRequest {
    pub number: u64,
    pub url: String,
    /// OPEN, CLOSED or MERGED.
    pub state: String,
    #[serde(default, rename = "isDraft")]
    pub is_draft: bool,
}

/// Stdout of a read-only git command, for callers that parse it themselves.
pub(crate) fn read(dir: &Path, args: &[&str]) -> Result<String> {
    stdout(git(dir).args(args))
}

/// `git revert --no-edit <sha>` in a checkout; a revert that conflicts is aborted.
pub fn revert(dir: &Path, sha: &str) -> Result<()> {
    if let Err(e) = run_loud(git(dir).args(["revert", "--no-edit", sha])) {
        let _ = git(dir).args(["revert", "--abort"]).output();
        bail!("{e}\nThe revert was aborted; nothing changed.");
    }
    Ok(())
}

/// A file as it was at `rev`; None when the commit isn't in the local clone.
pub fn show_file(dir: &Path, rev: &str, path: &str) -> Option<String> {
    let out = git(dir)
        .arg("show")
        .arg(format!("{rev}:{path}"))
        .output()
        .ok()?;
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
    let _ = git(repo)
        .args(["fetch", "--quiet", "origin", branch])
        .output();
    run_loud(git(repo).args(["worktree", "add"]).arg(path).arg(branch))?;
    Ok(())
}

pub(crate) fn gh(dir: &Path) -> Command {
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
    let out = gh(dir)
        .args(["pr", "view", "--json", "url,state,number,isDraft"])
        .output()
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
    let out = gh(dir)
        .args(pr_web_args(branch, base.trim(), title.trim())?)
        .output()
        .context("could not run gh")?;
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
    let pushed_to = stdout(git(dir).args(["remote", "get-url", remote]))
        .ok()
        .and_then(|url| remote_owner(&url));
    let base_owner = gh(dir)
        .args(["repo", "view", "--json", "owner", "--jq", ".owner.login"])
        .output()
        .ok()
        .filter(|out| out.status.success())
        .map(|out| String::from_utf8_lossy(&out.stdout).trim().to_string())
        .filter(|owner| !owner.is_empty());
    qualified_head(pushed_to.as_deref(), base_owner.as_deref(), branch)
}

fn qualified_head(pushed_to: Option<&str>, base_owner: Option<&str>, branch: &str) -> String {
    match (pushed_to, base_owner) {
        (Some(fork), Some(base)) if !fork.eq_ignore_ascii_case(base) => format!("{fork}:{branch}"),
        _ => branch.to_string(),
    }
}

/// Whether creating a pull request has to push first: read again right
/// before it, since a commit may have come while the dialog was open.
pub fn needs_push(dir: &Path) -> Option<bool> {
    let s = sync(dir).ok()?;
    Some(!s.published || s.ahead > 0)
}

/// A commit a new pull request would bring: its subject and the rest of
/// its message.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PrCommit {
    pub subject: String,
    pub body: String,
}

/// What the "Create pull request" dialog opens with.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
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
fn pr_commits(dir: &Path, remote: &str, base: &str) -> Vec<PrCommit> {
    let range = format!("{remote}/{base}..HEAD");
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

/// The repository's pull request template as committed on HEAD: what GitHub
/// itself would fill in.
fn pr_template(dir: &Path) -> Option<String> {
    PR_TEMPLATES
        .iter()
        .find_map(|path| show_file(dir, "HEAD", path))
        .filter(|text| !text.trim().is_empty())
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
    let template = pr_template(dir);
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
    use std::io::Write as _;
    let (title, base) = (title.trim(), base.trim());
    if crate::github::read_only() {
        bail!("{}", crate::github::pr::READ_ONLY);
    }
    if title.is_empty() {
        bail!("Give the pull request a title.");
    }
    // A name that starts with `-` would read as a flag.
    if base.is_empty() || base.starts_with('-') || branch.starts_with('-') {
        bail!("Name the branch to merge into.");
    }
    let mut child = gh(dir)
        .args(pr_create_args(branch, base, title, draft))
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .context("could not run gh")?;
    if let Some(mut pipe) = child.stdin.take() {
        // gh may give up before it reads everything; what it wrote says why.
        let _ = pipe.write_all(body.as_bytes());
    }
    let out = child.wait_with_output().context("could not run gh")?;
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
#[cfg(all(test, unix))]
pub(crate) const NICE_OF_SHELL: &str =
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
        let dir = std::env::temp_dir().join(format!(
            "arakun-rename-{}-{:?}",
            std::process::id(),
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
    fn git_runs_at_lower_priority() {
        let dir = std::env::temp_dir();
        let alias = format!("alias.ni=!{}", super::NICE_OF_SHELL);
        let out = super::stdout(super::git(&dir).args(["-c", alias.as_str(), "ni"])).unwrap();
        let ours = unsafe { libc::getpriority(libc::PRIO_PROCESS, 0) };
        assert_eq!(out.trim().parse::<i32>().unwrap(), (ours + 10).min(19));
    }
}
