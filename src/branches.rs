//! The branch map of a repository: every live branch against the default one,
//! its commits (and which agent wrote them), where it forked and merged, how far
//! it is ahead and behind, whether it would conflict, and its pull request.
//! Read from the local clone plus one `gh pr list`; the Branches tab draws it.

use std::collections::HashSet;
use std::path::Path;

use serde::Deserialize;

use crate::git;

/// Commits kept per branch and on the default branch.
const BRANCH_COMMITS: usize = 20;
const MAIN_COMMITS: usize = 80;
/// Branches read at most (the most recently touched).
const MAX_BRANCHES: usize = 40;
/// Pull requests asked of GitHub (open ones and the latest closed or merged).
const PR_LIMIT: &str = "60";

/// Who wrote a commit, from its author and Co-Authored-By trailers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Who {
    Claude,
    Droid,
    Person,
}

#[derive(Clone, Debug)]
pub struct Commit {
    pub sha: String,
    /// Committer time, unix seconds.
    pub time: u64,
    pub author: String,
    pub title: String,
    pub who: Who,
    /// Who wrote it with the author, from the first Co-Authored-By trailer
    /// ("Claude Opus 5.5"), without the email.
    pub coauthor: Option<String>,
    /// Files changed, lines added and removed; None when git did not say.
    pub stat: Option<Stat>,
    /// On some remote.
    pub pushed: bool,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Stat {
    pub files: u32,
    pub added: u32,
    pub removed: u32,
}

impl Commit {
    pub fn short(&self) -> &str {
        self.sha.get(..7).unwrap_or(&self.sha)
    }
}

/// The checks GitHub ran on a pull request's head.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Checks {
    None,
    Pending,
    Passed,
    Failed,
}

#[derive(Clone, Debug)]
pub struct Pr {
    pub number: u64,
    pub title: String,
    pub url: String,
    /// OPEN, MERGED or CLOSED.
    pub state: String,
    pub draft: bool,
    /// APPROVED, CHANGES_REQUESTED, REVIEW_REQUIRED or "".
    pub review: String,
    /// MERGEABLE, CONFLICTING or UNKNOWN.
    pub mergeable: String,
    pub checks: Checks,
    pub author: String,
    pub merged_at: u64,
    pub updated_at: u64,
}

impl Pr {
    pub fn open(&self) -> bool {
        self.state == "OPEN"
    }
    pub fn merged(&self) -> bool {
        self.state == "MERGED"
    }
}

#[derive(Clone, Debug, Default)]
pub struct Branch {
    pub name: String,
    /// Only on the remote (no local branch), or only a pull request (no ref at all).
    pub remote_only: bool,
    pub ghost: bool,
    /// Its last commit, unix seconds.
    pub time: u64,
    /// Commits not on the default branch, newest first.
    pub commits: Vec<Commit>,
    /// The commit on the default branch it grew from.
    pub fork: Option<String>,
    pub ahead: u64,
    pub behind: u64,
    /// Files a merge into the default branch would conflict on; None when not checked.
    pub conflicts: Option<Vec<String>>,
    /// Already contained in the default branch.
    pub contained: bool,
    pub pr: Option<Pr>,
    /// The commit on the default branch that brought it in (a merge or a squash "(#N)").
    pub merged_by: Option<String>,
}

/// What a branch needs, the most urgent first. The Branches tab sorts by it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Status {
    Conflict,
    Failing,
    Ready,
    Review,
    Working,
    Local,
    Merged,
    Idle,
}

impl Status {
    pub fn label(self) -> &'static str {
        match self {
            Status::Conflict => "Conflicts",
            Status::Failing => "Checks failing",
            Status::Ready => "Ready to merge",
            Status::Review => "In review",
            Status::Working => "Working",
            Status::Local => "Not pushed",
            Status::Merged => "Merged",
            Status::Idle => "Inactive",
        }
    }
}

/// A branch untouched for this long, with no open PR and no environment, is inactive.
pub const IDLE_SECS: u64 = 14 * 86_400;
/// Merged branches stay in view this long.
pub const MERGED_SECS: u64 = 7 * 86_400;

impl Branch {
    /// `env`: an environment has it checked out; `working`: an agent is working there.
    pub fn status(&self, env: bool, working: bool, now: u64) -> Status {
        let pr = self.pr.as_ref();
        if pr.is_some_and(Pr::merged) || (self.contained && !self.ghost) {
            let when = pr.map_or(self.time, |p| p.merged_at.max(self.time));
            return if now.saturating_sub(when) <= MERGED_SECS || env {
                Status::Merged
            } else {
                Status::Idle
            };
        }
        // A pull request closed without merging: abandoned, or taken in some other way.
        if pr.is_some_and(|p| !p.open() && !p.merged()) && !env && !working {
            return Status::Idle;
        }
        if self.conflicts.as_ref().is_some_and(|c| !c.is_empty())
            || pr.is_some_and(|p| p.open() && p.mergeable == "CONFLICTING")
        {
            return Status::Conflict;
        }
        if let Some(p) = pr.filter(|p| p.open()) {
            if p.checks == Checks::Failed {
                return Status::Failing;
            }
            if !p.draft
                && p.checks != Checks::Pending
                && p.review != "CHANGES_REQUESTED"
                && p.review != "REVIEW_REQUIRED"
            {
                return Status::Ready;
            }
            return Status::Review;
        }
        if working {
            return Status::Working;
        }
        if now.saturating_sub(self.time) > IDLE_SECS && !env {
            return Status::Idle;
        }
        if self.commits.iter().any(|c| !c.pushed) {
            return Status::Local;
        }
        Status::Working
    }

    /// The agents that wrote its commits, most commits first.
    pub fn authors(&self) -> Vec<Who> {
        let mut counts = [(Who::Claude, 0), (Who::Droid, 0), (Who::Person, 0)];
        for c in &self.commits {
            if let Some(slot) = counts.iter_mut().find(|(w, _)| *w == c.who) {
                slot.1 += 1;
            }
        }
        counts.sort_by_key(|(_, n)| std::cmp::Reverse(*n));
        counts
            .into_iter()
            .filter(|(_, n)| *n > 0)
            .map(|(w, _)| w)
            .collect()
    }
}

#[derive(Clone, Debug, Default)]
pub struct Map {
    /// The default branch as shown ("main"), and the ref it was read from ("origin/main").
    pub main: String,
    pub main_ref: String,
    /// First-parent history of the default branch, newest first.
    pub main_commits: Vec<Commit>,
    /// Commits on the default branch's first-parent line, all of them.
    pub main_total: u64,
    /// Version tags, as (commit sha, tag name).
    pub tags: Vec<(String, String)>,
    pub branches: Vec<Branch>,
    /// Why there are no pull requests (no GitHub remote, gh signed out...).
    pub prs_error: Option<String>,
    /// The repository pull requests come from when set by hand (`arakun.github`).
    pub github: Option<String>,
    pub fetched: u64,
}

impl Map {
    pub fn branch(&self, name: &str) -> Option<&Branch> {
        self.branches.iter().find(|b| b.name == name)
    }
}

fn exists(dir: &Path, r: &str) -> bool {
    git::read(
        dir,
        &[
            "rev-parse",
            "--verify",
            "--quiet",
            &format!("{r}^{{commit}}"),
        ],
    )
    .is_ok()
}

/// `origin/HEAD`'s branch, else `origin/main`, `origin/master`, `main`, `master`.
fn default_ref(dir: &Path) -> Option<String> {
    if let Ok(head) = git::read(
        dir,
        &["symbolic-ref", "--short", "refs/remotes/origin/HEAD"],
    ) {
        let head = head.trim().to_string();
        if !head.is_empty() && exists(dir, &head) {
            return Some(head);
        }
    }
    ["origin/main", "origin/master", "main", "master"]
        .into_iter()
        .find(|r| exists(dir, r))
        .map(str::to_string)
}

// Each record starts with \x1e, so `--shortstat`'s line ends up in its own record.
const FORMAT: &str = "--format=%x1e%H%x1f%ct%x1f%an%x1f%s%x1f%(trailers:key=Co-authored-by,valueonly,separator=%x1d)%x1f";

/// Claude or Droid when a trailer or the author names them.
pub fn who(author: &str, trailers: &str) -> Who {
    let t = format!("{author} {trailers}").to_ascii_lowercase();
    if t.contains("claude") || t.contains("anthropic.com") {
        Who::Claude
    } else if t.contains("droid") || t.contains("factory.ai") {
        Who::Droid
    } else {
        Who::Person
    }
}

/// The name in the first Co-Authored-By trailer: "Claude Opus 5.5" from
/// "Claude Opus 5.5 <noreply@anthropic.com>". Trailers are split by \x1d.
pub fn coauthor(trailers: &str) -> Option<String> {
    trailers
        .split(['\x1d', '\n'])
        .map(|t| t.split('<').next().unwrap_or_default().trim())
        .find(|n| !n.is_empty())
        .map(str::to_string)
}

/// " 3 files changed, 121 insertions(+), 19 deletions(-)" as numbers.
pub fn parse_stat(text: &str) -> Option<Stat> {
    let line = text
        .lines()
        .map(str::trim)
        .find(|l| l.contains("changed"))?;
    let mut stat = Stat::default();
    for part in line.split(',') {
        let mut words = part.split_whitespace();
        let n: u32 = words.next()?.parse().ok()?;
        match words.next().unwrap_or_default() {
            w if w.starts_with("file") => stat.files = n,
            w if w.starts_with("insertion") => stat.added = n,
            w if w.starts_with("deletion") => stat.removed = n,
            _ => {}
        }
    }
    Some(stat)
}

pub(crate) fn parse_log(text: &str) -> Vec<Commit> {
    text.split('\x1e')
        .filter_map(|rec| {
            let rec = rec.trim_matches(['\n', '\r']);
            let mut p = rec.split('\x1f');
            let sha = p.next()?.trim().to_string();
            let time = p.next()?.parse().ok()?;
            let author = p.next().unwrap_or_default().to_string();
            let title = p.next().unwrap_or_default().to_string();
            let trailers = p.next().unwrap_or_default();
            let stat = parse_stat(p.next().unwrap_or_default());
            (!sha.is_empty()).then(|| Commit {
                who: who(&author, trailers),
                coauthor: coauthor(trailers),
                stat,
                sha,
                time,
                author,
                title,
                pushed: true,
            })
        })
        .collect()
}

fn log(dir: &Path, args: &[&str]) -> Vec<Commit> {
    let mut full = vec!["log", FORMAT, "--shortstat"];
    full.extend_from_slice(args);
    git::read(dir, &full)
        .map(|t| parse_log(&t))
        .unwrap_or_default()
}

/// The conflicted files of merging `branch` into `main`, without touching the checkout.
fn conflicts(dir: &Path, main: &str, branch: &str) -> Option<Vec<String>> {
    let out = git::output(
        git::git(dir).args([
            "merge-tree",
            "--write-tree",
            "--name-only",
            "--no-messages",
            main,
            branch,
        ]),
        crate::platform::GIT_LOCAL,
    )
    .ok()?;
    match out.status.code() {
        Some(0) => Some(Vec::new()),
        // The tree id, then one conflicted path per line.
        Some(1) => Some(
            String::from_utf8_lossy(&out.stdout)
                .lines()
                .skip(1)
                .filter(|l| !l.is_empty())
                .map(str::to_string)
                .collect(),
        ),
        _ => None,
    }
}

#[derive(Deserialize)]
struct GhAuthor {
    #[serde(default)]
    login: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct GhPr {
    number: u64,
    title: String,
    url: String,
    state: String,
    #[serde(default)]
    is_draft: bool,
    head_ref_name: String,
    #[serde(default)]
    review_decision: Option<String>,
    #[serde(default)]
    mergeable: Option<String>,
    #[serde(default)]
    merged_at: Option<String>,
    #[serde(default)]
    updated_at: Option<String>,
    #[serde(default)]
    author: Option<GhAuthor>,
    #[serde(default)]
    status_check_rollup: Option<Vec<serde_json::Value>>,
}

/// One state for all of a PR's check runs and status contexts.
pub(crate) fn rollup(items: &[serde_json::Value]) -> Checks {
    if items.is_empty() {
        return Checks::None;
    }
    let mut pending = false;
    for i in items {
        let s = |k: &str| i[k].as_str().unwrap_or_default().to_ascii_uppercase();
        let (conclusion, status, state) = (s("conclusion"), s("status"), s("state"));
        if [
            "FAILURE",
            "TIMED_OUT",
            "CANCELLED",
            "ACTION_REQUIRED",
            "STARTUP_FAILURE",
            "ERROR",
        ]
        .contains(&conclusion.as_str())
            || state == "FAILURE"
            || state == "ERROR"
        {
            return Checks::Failed;
        }
        if (!status.is_empty() && status != "COMPLETED")
            || state == "PENDING"
            || state == "EXPECTED"
        {
            pending = true;
        }
    }
    if pending {
        Checks::Pending
    } else {
        Checks::Passed
    }
}

fn time_of(s: &Option<String>) -> u64 {
    s.as_deref()
        .and_then(crate::account::parse_rfc3339)
        .unwrap_or(0)
}

/// What `prs` answers for a repository with no GitHub remote: no pull
/// requests to show, rather than a failure.
pub const NO_REMOTE: &str = "This repository has no GitHub remote";

/// The git config key naming the GitHub repository to read pull requests
/// from, for a clone without a GitHub remote (a filtered export, a mirror).
pub const GITHUB_KEY: &str = "arakun.github";

/// `owner/name` set with [`GITHUB_KEY`] in the repository at `dir`.
pub fn github_repo(dir: &Path) -> Option<String> {
    git::read(dir, &["config", "--get", GITHUB_KEY])
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// Points the repository at `dir` to `repo` (`owner/name`) for its pull requests.
pub fn set_github_repo(dir: &Path, repo: &str) -> Result<(), String> {
    let repo = repo
        .trim()
        .trim_start_matches("https://github.com/")
        .trim_end_matches(".git")
        .trim_matches('/');
    let valid = repo.split('/').count() == 2
        && repo
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "-_./".contains(c));
    if !valid {
        return Err("Write it as owner/name, like octocat/hello-world".into());
    }
    git::read(dir, &["config", GITHUB_KEY, repo])
        .map(|_| ())
        .map_err(|e| e.to_string())
}

fn prs(dir: &Path) -> Result<Vec<(String, Pr)>, String> {
    let mut cmd = git::gh(dir);
    if let Some(repo) = github_repo(dir) {
        cmd.args(["-R", &repo]);
    }
    cmd.args([
            "pr",
            "list",
            "--state",
            "all",
            "-L",
            PR_LIMIT,
            "--json",
            "number,title,url,state,isDraft,headRefName,reviewDecision,mergeable,mergedAt,updatedAt,author,statusCheckRollup",
        ]);
    let out = crate::platform::output_within(&mut cmd, None, crate::platform::GIT_REMOTE).map_err(
        |e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                crate::github::NO_GH.to_string()
            } else {
                e.to_string()
            }
        },
    )?;
    if !out.status.success() {
        let err = crate::github::gh_error(&out.stderr, out.status);
        return Err(
            if err.contains("no git remotes") || err.contains("none of the git remotes") {
                NO_REMOTE.into()
            } else if err.to_lowercase().contains("gh auth login") {
                crate::github::SIGNED_OUT.into()
            } else {
                err.lines().next().unwrap_or(&err).to_string()
            },
        );
    }
    let list: Vec<GhPr> =
        serde_json::from_slice(&out.stdout).map_err(|_| "unexpected answer from gh".to_string())?;
    Ok(list
        .into_iter()
        .map(|p| {
            let pr = Pr {
                number: p.number,
                title: p.title,
                url: p.url,
                state: p.state,
                draft: p.is_draft,
                review: p.review_decision.unwrap_or_default(),
                mergeable: p.mergeable.unwrap_or_default(),
                checks: rollup(p.status_check_rollup.as_deref().unwrap_or_default()),
                author: p.author.map(|a| a.login).unwrap_or_default(),
                merged_at: time_of(&p.merged_at),
                updated_at: time_of(&p.updated_at),
            };
            (p.head_ref_name, pr)
        })
        .collect())
}

/// The commit on main that took `branch` in: a merge commit with its head as a
/// parent, else a squash whose title ends in "(#N)".
fn merged_by(
    main_commits: &[Commit],
    parents: &[(String, Vec<String>)],
    head: &str,
    pr: Option<&Pr>,
) -> Option<String> {
    if let Some((sha, _)) = parents
        .iter()
        .find(|(_, ps)| ps.iter().skip(1).any(|p| p == head))
    {
        return Some(sha.clone());
    }
    let n = pr.filter(|p| p.merged())?.number;
    let tag = format!("(#{n})");
    main_commits
        .iter()
        .find(|c| c.title.trim_end().ends_with(&tag))
        .map(|c| c.sha.clone())
}

/// `for-each-ref` lines of tags as (commit, tag): an annotated tag's commit
/// is in the second column, a lightweight one's in the first.
fn parse_tags(text: &str) -> Vec<(String, String)> {
    text.lines()
        .filter_map(|l| {
            let mut p = l.split('\t');
            let (object, peeled, name) = (p.next()?, p.next()?, p.next()?);
            let sha = if peeled.is_empty() { object } else { peeled };
            (!name.is_empty()).then(|| (sha.to_string(), name.to_string()))
        })
        .collect()
}

/// Reads the branch map of the repository at `dir` (any of its worktrees).
pub fn load(dir: &Path) -> Map {
    let now = crate::model::unix_now();
    let Some(main_ref) = default_ref(dir) else {
        return Map {
            fetched: now,
            ..Default::default()
        };
    };
    let main = main_ref
        .strip_prefix("origin/")
        .unwrap_or(&main_ref)
        .to_string();
    let (main_commits, parents, refs, gh) = std::thread::scope(|sc| {
        let prs = sc.spawn(|| prs(dir));
        let main_commits = log(
            dir,
            &["--first-parent", "-n", &MAIN_COMMITS.to_string(), &main_ref],
        );
        let parents: Vec<(String, Vec<String>)> = git::read(
            dir,
            &[
                "log",
                "--first-parent",
                "--format=%H %P",
                "-n",
                &MAIN_COMMITS.to_string(),
                &main_ref,
            ],
        )
        .unwrap_or_default()
        .lines()
        .filter_map(|l| {
            let mut it = l.split_whitespace();
            Some((it.next()?.to_string(), it.map(str::to_string).collect()))
        })
        .collect();
        let refs = git::read(
            dir,
            &[
                "for-each-ref",
                "--sort=-committerdate",
                "--format=%(refname)%09%(committerdate:unix)",
                "refs/heads",
                "refs/remotes",
            ],
        )
        .unwrap_or_default();
        (
            main_commits,
            parents,
            refs,
            prs.join().unwrap_or_else(|_| Err("gh crashed".into())),
        )
    });
    let main_total = git::read(dir, &["rev-list", "--count", "--first-parent", &main_ref])
        .ok()
        .and_then(|n| n.trim().parse().ok())
        .unwrap_or(main_commits.len() as u64);
    let tags = git::read(
        dir,
        &[
            "for-each-ref",
            "--format=%(objectname)%09%(*objectname)%09%(refname:short)",
            "refs/tags",
        ],
    )
    .map(|t| parse_tags(&t))
    .unwrap_or_default();
    let (pr_list, prs_error) = match gh {
        Ok(l) => (l, None),
        Err(e) => (Vec::new(), Some(e)),
    };

    // Local branches, then remote ones nobody checked out.
    let mut names: Vec<(String, String, bool, u64)> = Vec::new();
    for line in refs.lines() {
        let Some((full, time)) = line.split_once('\t') else {
            continue;
        };
        let time = time.trim().parse().unwrap_or(0);
        let (short, remote) = if let Some(n) = full.strip_prefix("refs/heads/") {
            (n.to_string(), false)
        } else if let Some(n) = full.strip_prefix("refs/remotes/") {
            match n.split_once('/') {
                Some((_, "HEAD")) | None => continue,
                Some((_, b)) => (b.to_string(), true),
            }
        } else {
            continue;
        };
        if short == main || names.iter().any(|(n, ..)| *n == short) {
            continue;
        }
        let r = if remote {
            full.trim_start_matches("refs/remotes/").to_string()
        } else {
            short.clone()
        };
        names.push((short, r, remote, time));
    }
    names.truncate(MAX_BRANCHES);

    let mut branches: Vec<Branch> = std::thread::scope(|sc| {
        let handles: Vec<_> = names
            .iter()
            .map(|(name, r, remote, time)| {
                let (main_ref, main_commits, parents, pr_list) =
                    (&main_ref, &main_commits, &parents, &pr_list);
                sc.spawn(move || {
                    let range = format!("{main_ref}..{r}");
                    let mut commits = log(dir, &["-n", &BRANCH_COMMITS.to_string(), &range]);
                    let unpushed: HashSet<String> = if *remote {
                        HashSet::new()
                    } else {
                        git::read(
                            dir,
                            &[
                                "log",
                                "--format=%H",
                                "-n",
                                &BRANCH_COMMITS.to_string(),
                                r,
                                "--not",
                                "--remotes",
                            ],
                        )
                        .unwrap_or_default()
                        .lines()
                        .map(str::to_string)
                        .collect()
                    };
                    for c in &mut commits {
                        c.pushed = !unpushed.contains(&c.sha);
                    }
                    let counts = git::read(
                        dir,
                        &[
                            "rev-list",
                            "--left-right",
                            "--count",
                            &format!("{main_ref}...{r}"),
                        ],
                    )
                    .unwrap_or_default();
                    let mut it = counts
                        .split_whitespace()
                        .map(|n| n.parse::<u64>().unwrap_or(0));
                    let (behind, ahead) = (it.next().unwrap_or(0), it.next().unwrap_or(0));
                    let head = git::read(dir, &["rev-parse", r])
                        .unwrap_or_default()
                        .trim()
                        .to_string();
                    let fork = git::read(dir, &["merge-base", main_ref, r])
                        .ok()
                        .map(|s| s.trim().to_string())
                        .filter(|s| !s.is_empty());
                    let contained = ahead == 0;
                    let pr = pr_list
                        .iter()
                        .filter(|(h, _)| h == name)
                        .map(|(_, p)| p)
                        .max_by_key(|p| (p.open(), p.updated_at))
                        .cloned();
                    let merged_by = if contained || pr.as_ref().is_some_and(Pr::merged) {
                        merged_by(main_commits, parents, &head, pr.as_ref())
                    } else {
                        None
                    };
                    // Only branches still going somewhere are worth a merge simulation.
                    let active = !contained
                        && !pr.as_ref().is_some_and(|p| !p.open())
                        && crate::model::unix_now().saturating_sub(*time) <= IDLE_SECS;
                    let conflicts = if active {
                        conflicts(dir, main_ref, r)
                    } else {
                        None
                    };
                    Branch {
                        name: name.clone(),
                        remote_only: *remote,
                        ghost: false,
                        time: *time,
                        commits,
                        fork,
                        ahead,
                        behind,
                        conflicts,
                        contained,
                        pr,
                        merged_by,
                    }
                })
            })
            .collect();
        handles.into_iter().filter_map(|h| h.join().ok()).collect()
    });

    // Pull requests whose branch isn't here (a fork, or deleted after merging).
    for (head, pr) in &pr_list {
        if head == &main || branches.iter().any(|b| &b.name == head) {
            continue;
        }
        let recent = now.saturating_sub(pr.merged_at.max(pr.updated_at)) <= MERGED_SECS;
        if !pr.open() && !recent {
            continue;
        }
        let merged_by = merged_by(&main_commits, &parents, "", Some(pr));
        branches.push(Branch {
            name: head.clone(),
            remote_only: true,
            ghost: true,
            time: pr.merged_at.max(pr.updated_at),
            pr: Some(pr.clone()),
            contained: pr.merged(),
            merged_by,
            ..Default::default()
        });
    }
    Map {
        main,
        main_ref,
        main_commits,
        main_total,
        tags,
        branches,
        prs_error,
        github: github_repo(dir),
        fetched: now,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn agents_from_trailers() {
        assert_eq!(
            who("alice", "Claude Opus 5.5 <noreply@anthropic.com>"),
            Who::Claude
        );
        assert_eq!(
            who(
                "bob",
                "factory-droid[bot] <138933559+factory-droid[bot]@users.noreply.github.com>"
            ),
            Who::Droid
        );
        assert_eq!(who("alice", ""), Who::Person);
        let log = "\x1ea1\x1f100\x1falice\x1fFix\x1fClaude Opus 5.5 <noreply@anthropic.com>\x1dcubic <bot@cubic.dev>\x1f\n\n 1 file changed, 121 insertions(+), 19 deletions(-)\n\x1eb2\x1f90\x1fbob\x1fZoom (#12)\x1f\x1f\n";
        let c = parse_log(log);
        assert_eq!(c.len(), 2);
        assert_eq!(
            (c[0].who, c[1].who, c[1].title.as_str(), c[1].time),
            (Who::Claude, Who::Person, "Zoom (#12)", 90)
        );
        assert_eq!(c[0].coauthor.as_deref(), Some("Claude Opus 5.5"));
        assert_eq!(
            c[0].stat,
            Some(Stat {
                files: 1,
                added: 121,
                removed: 19
            })
        );
        assert_eq!((c[1].coauthor.as_deref(), c[1].stat), (None, None));
    }

    #[test]
    fn tags_point_at_their_commit() {
        assert_eq!(
            parse_tags("t1\tc1\tv0.14.1\nc2\t\tnightly\n"),
            vec![
                ("c1".to_string(), "v0.14.1".to_string()),
                ("c2".to_string(), "nightly".to_string())
            ]
        );
    }

    #[test]
    fn stats_and_coauthors_read_alone() {
        assert_eq!(
            parse_stat(" 3 files changed, 4 deletions(-)"),
            Some(Stat {
                files: 3,
                added: 0,
                removed: 4
            })
        );
        assert_eq!(parse_stat(""), None);
        assert_eq!(coauthor("cubic <bot@cubic.dev>").as_deref(), Some("cubic"));
        assert_eq!(coauthor(""), None);
    }

    #[test]
    fn checks_roll_up() {
        assert_eq!(rollup(&[]), Checks::None);
        assert_eq!(
            rollup(&[json!({"status": "COMPLETED", "conclusion": "SUCCESS"})]),
            Checks::Passed
        );
        assert_eq!(
            rollup(&[
                json!({"status": "COMPLETED", "conclusion": "SUCCESS"}),
                json!({"status": "IN_PROGRESS", "conclusion": ""})
            ]),
            Checks::Pending
        );
        assert_eq!(
            rollup(&[
                json!({"status": "IN_PROGRESS"}),
                json!({"status": "COMPLETED", "conclusion": "FAILURE"})
            ]),
            Checks::Failed
        );
        assert_eq!(rollup(&[json!({"state": "PENDING"})]), Checks::Pending);
    }

    fn pr(state: &str) -> Pr {
        Pr {
            number: 7,
            title: "x".into(),
            url: String::new(),
            state: state.into(),
            draft: false,
            review: String::new(),
            mergeable: "MERGEABLE".into(),
            checks: Checks::Passed,
            author: String::new(),
            merged_at: 0,
            updated_at: 0,
        }
    }

    #[test]
    fn statuses() {
        let now = 100 * 86_400;
        let commit = |pushed| Commit {
            sha: "a".into(),
            time: now,
            author: String::new(),
            title: String::new(),
            who: Who::Person,
            coauthor: None,
            stat: None,
            pushed,
        };
        let b = Branch {
            time: now,
            commits: vec![commit(true)],
            ahead: 1,
            ..Default::default()
        };
        assert_eq!(b.status(false, false, now), Status::Working);
        assert_eq!(
            Branch {
                commits: vec![commit(false)],
                ..b.clone()
            }
            .status(false, false, now),
            Status::Local
        );
        assert_eq!(
            Branch {
                conflicts: Some(vec!["a.rs".into()]),
                ..b.clone()
            }
            .status(false, false, now),
            Status::Conflict
        );
        assert_eq!(
            Branch {
                pr: Some(pr("OPEN")),
                ..b.clone()
            }
            .status(false, false, now),
            Status::Ready
        );
        assert_eq!(
            Branch {
                pr: Some(Pr {
                    checks: Checks::Failed,
                    ..pr("OPEN")
                }),
                ..b.clone()
            }
            .status(false, false, now),
            Status::Failing
        );
        assert_eq!(
            Branch {
                pr: Some(Pr {
                    draft: true,
                    ..pr("OPEN")
                }),
                ..b.clone()
            }
            .status(false, false, now),
            Status::Review
        );
        assert_eq!(
            Branch {
                pr: Some(Pr {
                    merged_at: now - 3600,
                    ..pr("MERGED")
                }),
                ..b.clone()
            }
            .status(false, false, now),
            Status::Merged
        );
        assert_eq!(
            Branch {
                time: now - 30 * 86_400,
                ..b.clone()
            }
            .status(false, false, now),
            Status::Idle
        );
        assert_eq!(
            Branch {
                pr: Some(pr("CLOSED")),
                conflicts: Some(vec!["a".into()]),
                ..b.clone()
            }
            .status(false, false, now),
            Status::Idle
        );
        assert_eq!(
            Branch {
                time: now - 30 * 86_400,
                ..b.clone()
            }
            .status(true, false, now),
            Status::Working
        );
        assert_eq!(
            Branch {
                time: now - 30 * 86_400,
                ..b
            }
            .status(false, true, now),
            Status::Working
        );
    }

    #[test]
    fn squash_merges_are_found_by_number() {
        let c = |sha: &str, title: &str| Commit {
            sha: sha.into(),
            time: 0,
            author: String::new(),
            title: title.into(),
            who: Who::Person,
            coauthor: None,
            stat: None,
            pushed: true,
        };
        let main = vec![c("m2", "CI más rápido (#14)"), c("m1", "Usage (#13)")];
        let p = Pr {
            number: 13,
            ..pr("MERGED")
        };
        assert_eq!(merged_by(&main, &[], "h", Some(&p)).as_deref(), Some("m1"));
        let parents = vec![("m9".to_string(), vec!["p".to_string(), "h".to_string()])];
        assert_eq!(merged_by(&main, &parents, "h", None).as_deref(), Some("m9"));
    }

    /// A throwaway repo: main with two commits, a branch that conflicts and one that doesn't.
    #[test]
    fn reads_a_repository() {
        let repo = std::env::temp_dir().join(format!("arakun-branches-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&repo);
        std::fs::create_dir_all(&repo).unwrap();
        let sh = |c: &str| {
            let ok = crate::git::unpinned(&mut std::process::Command::new("sh"))
                .arg("-c")
                .arg(c)
                .current_dir(&repo)
                .status()
                .unwrap()
                .success();
            assert!(ok, "{c}");
        };
        let g = "git -c core.hooksPath=/dev/null -c user.name=t -c user.email=t@t";
        sh(&format!(
            "git init -q -b main && echo a > f && {g} add -A && {g} commit -qm one && \
             git checkout -qb clash && echo b > f && {g} commit -qam clash-1 -m 'Co-authored-by: Claude <noreply@anthropic.com>' && \
             git checkout -q main && git checkout -qb side && echo s > s && {g} add -A && {g} commit -qm side-1 && \
             git checkout -q main && echo c > f && {g} commit -qam two"
        ));
        let map = load(&repo);
        assert_eq!((map.main.as_str(), map.main_commits.len()), ("main", 2));
        let clash = map.branch("clash").unwrap();
        assert_eq!((clash.ahead, clash.behind), (1, 1));
        assert_eq!(clash.conflicts.as_deref(), Some(&["f".to_string()][..]));
        assert_eq!(clash.commits[0].who, Who::Claude);
        assert!(!clash.commits[0].pushed);
        let side = map.branch("side").unwrap();
        assert_eq!(side.conflicts.as_deref(), Some(&[][..]));
        assert_eq!(side.fork.as_deref(), Some(map.main_commits[1].sha.as_str()));
        assert!(map.prs_error.is_some());
        let _ = std::fs::remove_dir_all(&repo);
    }
}
