//! A branch drawn against the default branch: the commits on each, where the
//! branch grew from and, once merged, where it came back. Read from local refs.

use std::collections::HashSet;
use std::path::Path;

use anyhow::{Result, anyhow};

use crate::git;

/// Commits shown per lane at most.
const BRANCH_MAX: usize = 25;
const MAIN_NEWER: usize = 6;
const MAIN_OLDER: usize = 3;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Lane {
    Main,
    Branch,
}

#[derive(Clone, Debug)]
pub struct Commit {
    pub sha: String,
    pub title: String,
    /// Committer time, unix seconds.
    pub time: u64,
    pub lane: Lane,
    /// On the remote, so CI can have seen it.
    pub pushed: bool,
}

impl Commit {
    pub fn short(&self) -> &str {
        self.sha.get(..7).unwrap_or(&self.sha)
    }
}

#[derive(Clone, Debug, Default)]
pub struct Graph {
    pub branch: String,
    /// The default branch, as shown ("main").
    pub main: String,
    /// Newest first.
    pub rows: Vec<Commit>,
    /// Row of the commit on main the branch grew from.
    pub fork: Option<usize>,
    /// Row of the merge commit on main that brought the branch in.
    pub merge: Option<usize>,
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

/// `origin/main` (what CI ran on), else a local default branch.
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

fn log(dir: &Path, args: &[&str], lane: Lane) -> Vec<Commit> {
    let mut full = vec!["log", "--format=%H%x1f%ct%x1f%s"];
    full.extend_from_slice(args);
    let Ok(text) = git::read(dir, &full) else {
        return Vec::new();
    };
    text.lines()
        .filter_map(|l| {
            let mut parts = l.splitn(3, '\x1f');
            let sha = parts.next()?.to_string();
            let time = parts.next()?.parse().ok()?;
            let title = parts.next().unwrap_or_default().to_string();
            Some(Commit {
                sha,
                title,
                time,
                lane,
                pushed: true,
            })
        })
        .collect()
}

fn one(dir: &Path, args: &[&str]) -> Option<String> {
    git::read(dir, args)
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

pub fn build(dir: &Path, branch: &str) -> Result<Graph> {
    let main_ref =
        default_ref(dir).ok_or_else(|| anyhow!("This repository has no main branch."))?;
    let main = main_ref.trim_start_matches("origin/").to_string();
    let local = format!("refs/heads/{branch}");
    let remote = format!("refs/remotes/origin/{branch}");
    let tip = if exists(dir, &local) {
        local
    } else if exists(dir, &remote) {
        remote.clone()
    } else {
        return Err(anyhow!("{branch} isn't in this clone yet."));
    };
    let unpushed: HashSet<String> = {
        let upstream = if exists(dir, &remote) {
            remote.clone()
        } else {
            main_ref.clone()
        };
        one(dir, &["rev-list", &format!("{upstream}..{tip}")])
            .map(|s| s.lines().map(str::to_string).collect())
            .unwrap_or_default()
    };
    let mark = |mut c: Vec<Commit>| {
        for commit in &mut c {
            commit.pushed = !unpushed.contains(&commit.sha);
        }
        c
    };
    let n = |k: usize| k.to_string();

    if branch == main {
        let rows = mark(log(
            dir,
            &["--first-parent", "-n", &n(BRANCH_MAX), &tip],
            Lane::Main,
        ));
        return Ok(Graph {
            branch: branch.into(),
            main,
            rows,
            fork: None,
            merge: None,
        });
    }

    let merged = git::read(dir, &["merge-base", "--is-ancestor", &tip, &main_ref]).is_ok();
    // The oldest commit on main's first-parent line that contains the branch.
    let merge_sha = merged
        .then(|| {
            one(
                dir,
                &[
                    "rev-list",
                    "--first-parent",
                    "--ancestry-path",
                    &format!("{tip}..{main_ref}"),
                ],
            )
        })
        .flatten()
        .and_then(|s| s.lines().last().map(str::to_string));
    let base = match &merge_sha {
        Some(m) => one(dir, &["merge-base", &format!("{m}^1"), &tip]),
        None => one(dir, &["merge-base", &main_ref, &tip]),
    }
    .ok_or_else(|| anyhow!("{branch} shares no history with {main}."))?;

    let branch_rows = mark(log(
        dir,
        &["-n", &n(BRANCH_MAX), &format!("{base}..{tip}")],
        Lane::Branch,
    ));
    let newer = log(
        dir,
        &[
            "--first-parent",
            "-n",
            "200",
            &format!("{base}..{main_ref}"),
        ],
        Lane::Main,
    );
    let newer: Vec<Commit> = match merge_sha
        .as_ref()
        .and_then(|m| newer.iter().position(|c| &c.sha == m))
    {
        Some(at) => newer[at.saturating_sub(MAIN_NEWER / 2)..].to_vec(),
        None => newer.into_iter().take(MAIN_NEWER).collect(),
    };
    let older = log(
        dir,
        &["--first-parent", "-n", &n(MAIN_OLDER), &base],
        Lane::Main,
    );

    let mut rows: Vec<Commit> = newer.into_iter().chain(branch_rows).collect();
    // Newest first; the merge commit stays above what it merged.
    rows.sort_by(|a, b| {
        b.time
            .cmp(&a.time)
            .then((a.lane == Lane::Branch).cmp(&(b.lane == Lane::Branch)))
    });
    let has_branch = rows.iter().any(|c| c.lane == Lane::Branch);
    let fork = (has_branch && !older.is_empty()).then_some(rows.len());
    rows.extend(older);
    let merge = merge_sha
        .and_then(|m| rows.iter().position(|c| c.sha == m))
        .filter(|_| has_branch);
    Ok(Graph {
        branch: branch.into(),
        main,
        rows,
        fork,
        merge,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn git_at(dir: &Path, t: u64, args: &[&str]) {
        let date = format!("@{} +0000", 1_700_000_000 + t);
        let out = crate::git::unpinned(&mut crate::platform::command("git"))
            .current_dir(dir)
            .args(args)
            .env("GIT_AUTHOR_DATE", &date)
            .env("GIT_COMMITTER_DATE", &date)
            .env("GIT_AUTHOR_NAME", "t")
            .env("GIT_AUTHOR_EMAIL", "t@t")
            .env("GIT_COMMITTER_NAME", "t")
            .env("GIT_COMMITTER_EMAIL", "t@t")
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    fn commit(dir: &Path, t: u64, msg: &str) {
        git_at(dir, t, &["commit", "--allow-empty", "-q", "-m", msg]);
    }

    fn titles(g: &Graph) -> Vec<(&str, Lane)> {
        g.rows.iter().map(|c| (c.title.as_str(), c.lane)).collect()
    }

    #[test]
    fn draws_a_branch_against_main_before_and_after_merging() {
        let dir = std::env::temp_dir().join(format!("arakun-graph-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        git_at(&dir, 1, &["init", "-q", "-b", "main"]);
        commit(&dir, 10, "c1");
        commit(&dir, 20, "c2");
        git_at(&dir, 21, &["checkout", "-q", "-b", "feat"]);
        commit(&dir, 30, "f1");
        commit(&dir, 40, "f2");
        git_at(&dir, 41, &["checkout", "-q", "main"]);
        commit(&dir, 35, "c3");

        let g = build(&dir, "feat").unwrap();
        assert_eq!(g.main, "main");
        assert_eq!(
            titles(&g),
            [
                ("f2", Lane::Branch),
                ("c3", Lane::Main),
                ("f1", Lane::Branch),
                ("c2", Lane::Main),
                ("c1", Lane::Main)
            ]
        );
        assert_eq!((g.fork, g.merge), (Some(3), None));
        // Nothing has an upstream here, so the branch's own commits are unpushed.
        assert!(!g.rows[0].pushed && g.rows[1].pushed);

        git_at(
            &dir,
            50,
            &["merge", "-q", "--no-ff", "-m", "Merge feat", "feat"],
        );
        let g = build(&dir, "feat").unwrap();
        assert_eq!(g.rows[0].title, "Merge feat");
        assert_eq!(g.merge, Some(0));
        assert_eq!(g.rows[g.fork.unwrap()].title, "c2");

        let g = build(&dir, "main").unwrap();
        assert_eq!(
            g.rows.iter().map(|c| c.title.as_str()).collect::<Vec<_>>(),
            ["Merge feat", "c3", "c2", "c1"]
        );
        assert!(build(&dir, "nope").is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
