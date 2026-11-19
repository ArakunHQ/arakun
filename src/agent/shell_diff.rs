//! What a turn's shell commands changed in the checkout. Edit tools report
//! their own diffs; `sed -i`, formatters and generated files don't, so the
//! working files are snapshotted as a git tree when a turn starts and when it
//! ends, and the text files that differ become the turn's shell changes.

use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use serde::{Deserialize, Serialize};

/// Files kept per turn, and the largest one whose text is kept.
const MAX_FILES: usize = 20;
const MAX_BYTES: usize = 64 * 1024;
/// Files bigger than this stay out of the snapshot. `git add` copies each
/// version into the checkout's `.git/objects`, where it stays until gc, so a
/// generated file (a database, a dataset) would add a copy every turn.
const MAX_SNAPSHOT_BYTES: u64 = 1 << 20;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ShellChange {
    /// Absolute, like the paths of edit tools.
    pub path: PathBuf,
    /// None for a file the turn created.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub old: Option<String>,
    /// Empty for a file the turn deleted.
    pub new: String,
    pub added: u32,
    pub removed: u32,
}

/// A checkout's working files at one moment.
#[derive(Clone, Debug)]
pub struct Snapshot {
    top: PathBuf,
    tree: String,
    /// Files left out for their size, by their path from `top`.
    big: Vec<String>,
}

fn git(dir: &Path, index: Option<&Path>, args: &[&str]) -> Option<Vec<u8>> {
    git_with_input(dir, index, args, None)
}

fn git_with_input(
    dir: &Path,
    index: Option<&Path>,
    args: &[&str],
    input: Option<&[u8]>,
) -> Option<Vec<u8>> {
    let mut cmd = crate::platform::command("git");
    cmd.args(args).current_dir(dir);
    if let Some(index) = index {
        cmd.env("GIT_INDEX_FILE", index);
    }
    crate::platform::output_within(&mut cmd, input, crate::platform::GIT_LOCAL)
        .ok()
        .filter(|o| o.status.success())
        .map(|o| o.stdout)
}

/// The changed and untracked files over `MAX_SNAPSHOT_BYTES`, by their path
/// from `top`, for the index at `index`.
fn too_big(top: &Path, index: &Path) -> Option<Vec<String>> {
    let changed = git(
        top,
        Some(index),
        &[
            "ls-files",
            "-z",
            "--modified",
            "--others",
            "--exclude-standard",
        ],
    )?;
    Some(
        changed
            .split(|&b| b == 0)
            .filter_map(|p| std::str::from_utf8(p).ok())
            .filter(|p| {
                !p.is_empty()
                    && std::fs::metadata(top.join(p)).is_ok_and(|m| m.len() > MAX_SNAPSHOT_BYTES)
            })
            .map(str::to_string)
            .collect(),
    )
}

/// Stages every file but `big` in the index at `index`, and takes `big` out of
/// it: an exclude only stops a file being updated, so a tracked one would
/// keep its older version.
fn stage(top: &Path, index: &Path, big: &[String]) -> Option<()> {
    let mut specs = b".\0".to_vec();
    let mut paths = Vec::new();
    for path in big {
        specs.extend_from_slice(b":(exclude,literal)");
        specs.extend_from_slice(path.as_bytes());
        specs.push(0);
        paths.extend_from_slice(path.as_bytes());
        paths.push(0);
    }
    git_with_input(
        top,
        Some(index),
        &["add", "-A", "--pathspec-from-file=-", "--pathspec-file-nul"],
        Some(&specs),
    )?;
    if !big.is_empty() {
        git_with_input(
            top,
            Some(index),
            &["update-index", "--force-remove", "-z", "--stdin"],
            Some(&paths),
        )?;
    }
    Some(())
}

fn line(out: Vec<u8>) -> String {
    String::from_utf8_lossy(&out).trim().to_string()
}

/// The files of the checkout at `dir` as they are now, tracked and untracked
/// but not ignored, without touching its index.
pub fn snapshot(dir: &Path) -> Option<Snapshot> {
    static N: AtomicU64 = AtomicU64::new(0);
    // From `dir` rather than --show-toplevel, which resolves symlinks: paths
    // have to match the ones the agent's edit tools report.
    let cdup = line(git(dir, None, &["rev-parse", "--show-cdup"])?);
    let mut top = dir.to_path_buf();
    for c in Path::new(&cdup).components() {
        if c == Component::ParentDir {
            top.pop();
        }
    }
    let index = PathBuf::from(line(git(
        &top,
        None,
        &["rev-parse", "--git-path", "index"],
    )?));
    let index = if index.is_relative() {
        top.join(index)
    } else {
        index
    };
    let tmp = std::env::temp_dir().join(format!(
        "arakun-snap-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::Relaxed)
    ));
    // A copy keeps the index's stat cache, so only files changed since get hashed.
    // It also keeps the index's mtime: git rehashes entries modified in the same
    // second as the index ("racy" ones), and a fresh mtime would hide a same-size
    // edit made in that second.
    let _ = std::fs::copy(&index, &tmp);
    if let Ok(mtime) = std::fs::metadata(&index).and_then(|m| m.modified()) {
        let _ = std::fs::File::options()
            .write(true)
            .open(&tmp)
            .and_then(|f| f.set_modified(mtime));
    }
    let staged = too_big(&top, &tmp).and_then(|big| {
        stage(&top, &tmp, &big)?;
        Some((git(&top, Some(&tmp), &["write-tree"])?, big))
    });
    let _ = std::fs::remove_file(&tmp);
    let (tree, big) = staged?;
    Some(Snapshot {
        top,
        tree: line(tree),
        big,
    })
}

/// The text files that differ between two snapshots of the same checkout.
pub fn changes(from: &Snapshot, to: &Snapshot) -> Vec<ShellChange> {
    if from.top != to.top || from.tree == to.tree {
        return Vec::new();
    }
    let top = &from.top;
    let Some(out) = git(
        top,
        None,
        &[
            "diff",
            "--no-renames",
            "--numstat",
            "-z",
            &from.tree,
            &to.tree,
        ],
    ) else {
        return Vec::new();
    };
    // None when the tree has no such file, Err when it is too big or not text.
    let text = |tree: &str, path: &str| -> Result<Option<String>, ()> {
        let spec = format!("{tree}:{path}");
        // The size first, so a big file is not read just to be dropped.
        let Some(size) = git(top, None, &["cat-file", "-s", &spec]) else {
            return Ok(None);
        };
        if line(size).parse::<usize>().map_or(true, |n| n > MAX_BYTES) {
            return Err(());
        }
        let blob = git(top, None, &["cat-file", "blob", &spec]).ok_or(())?;
        String::from_utf8(blob).map(Some).map_err(|_| ())
    };
    let mut changes = Vec::new();
    for record in String::from_utf8_lossy(&out).split('\0') {
        let mut parts = record.splitn(3, '\t');
        let (Some(added), Some(removed), Some(path)) = (parts.next(), parts.next(), parts.next())
        else {
            continue;
        };
        // Binary files count "-".
        let (Ok(added), Ok(removed)) = (added.parse(), removed.parse()) else {
            continue;
        };
        let (Ok(old), Ok(new)) = (text(&from.tree, path), text(&to.tree, path)) else {
            continue;
        };
        // A file left out of either snapshot for its size is missing there,
        // not created or deleted.
        if from.big.iter().chain(&to.big).any(|b| b == path) {
            continue;
        }
        changes.push(ShellChange {
            path: top.join(path),
            old,
            new: new.unwrap_or_default(),
            added,
            removed,
        });
        if changes.len() == MAX_FILES {
            break;
        }
    }
    changes
}

#[cfg(test)]
mod tests {
    use std::process::Command;

    use super::*;

    #[test]
    fn shell_changes_are_what_differs_between_snapshots() {
        let repo = std::env::temp_dir().join(format!("arakun-shell-diff-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&repo);
        std::fs::create_dir_all(&repo).unwrap();
        let sh = |c: &str| {
            let ok = crate::git::unpinned(&mut Command::new("sh"))
                .arg("-c")
                .arg(c)
                .current_dir(&repo)
                .status()
                .unwrap()
                .success();
            assert!(ok, "{c}");
        };
        sh(
            "git init -q && printf 'a\\nb\\n' > kept.txt && printf 'x\\n' > gone.txt && echo target > .gitignore \
            && git add -A && git -c core.hooksPath=/dev/null -c user.name=t -c user.email=t@t commit -qm init \
            && printf 'staged\\n' > staged.txt && git add staged.txt",
        );
        let before = snapshot(&repo).unwrap();
        sh(
            "printf 'a\\nB\\n' > kept.txt && rm gone.txt && printf 'n\\n' > new.txt && mkdir target && echo o > target/x \
            && printf '\\0\\1' > bin.dat",
        );
        let after = snapshot(&repo).unwrap();
        let mut got = changes(&before, &after);
        got.sort_by(|a, b| a.path.cmp(&b.path));
        let names: Vec<_> = got
            .iter()
            .map(|c| c.path.strip_prefix(&repo).unwrap().display().to_string())
            .collect();
        assert_eq!(names, ["gone.txt", "kept.txt", "new.txt"]);
        assert_eq!(
            (got[0].old.as_deref(), got[0].new.as_str()),
            (Some("x\n"), "")
        );
        assert_eq!(
            (
                got[1].old.as_deref(),
                got[1].new.as_str(),
                got[1].added,
                got[1].removed
            ),
            (Some("a\nb\n"), "a\nB\n", 1, 1)
        );
        assert_eq!((got[2].old.as_deref(), got[2].new.as_str()), (None, "n\n"));
        // The checkout's own index is left as it was.
        let status = crate::git::unpinned(&mut Command::new("git"))
            .args(["status", "--porcelain"])
            .current_dir(&repo)
            .output()
            .unwrap();
        assert!(String::from_utf8_lossy(&status.stdout).contains("A  staged.txt"));
        assert!(String::from_utf8_lossy(&status.stdout).contains("?? new.txt"));
        assert!(changes(&after, &snapshot(&repo).unwrap()).is_empty());
        let _ = std::fs::remove_dir_all(&repo);
    }

    #[test]
    fn big_files_stay_out_of_the_snapshot_and_its_objects() {
        let repo = std::env::temp_dir().join(format!("arakun-shell-big-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&repo);
        std::fs::create_dir_all(&repo).unwrap();
        let git = |args: &[&str]| {
            crate::git::unpinned(&mut Command::new("git"))
                .args(args)
                .current_dir(&repo)
                .output()
                .unwrap()
        };
        assert!(git(&["init", "-q"]).status.success());
        std::fs::write(repo.join("tracked.txt"), "base\n").unwrap();
        let commit = [
            "-c",
            "core.hooksPath=/dev/null",
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@t",
            "commit",
            "-qam",
            "init",
        ];
        assert!(git(&["add", "tracked.txt"]).status.success());
        assert!(git(&commit).status.success());
        std::fs::write(repo.join("tracked.txt"), "before\n").unwrap();
        let big = vec![b'a'; MAX_SNAPSHOT_BYTES as usize + 1];
        std::fs::write(repo.join("data.db"), &big).unwrap();
        std::fs::write(repo.join("grows.txt"), "small\n").unwrap();
        std::fs::write(repo.join("small.txt"), "one\n").unwrap();
        let before = snapshot(&repo).unwrap();
        let mut grown = big.clone();
        grown.push(b'b');
        let first = git(&["hash-object", "data.db"]).stdout;
        std::fs::write(repo.join("data.db"), &grown).unwrap();
        std::fs::write(repo.join("grows.txt"), &big).unwrap();
        std::fs::write(repo.join("tracked.txt"), &big).unwrap();
        std::fs::write(repo.join("small.txt"), "two\n").unwrap();
        let after = snapshot(&repo).unwrap();

        // Files that grew past the limit are not reported as deleted, nor the
        // tracked one as gone back to its committed text.
        // Deleted after the snapshot: what counts is that it was left out.
        std::fs::remove_file(repo.join("grows.txt")).unwrap();
        let got = changes(&before, &after);
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].path, repo.join("small.txt"));
        let tree = git(&["ls-tree", "--name-only", &after.tree]).stdout;
        assert_eq!(String::from_utf8_lossy(&tree), "small.txt\n");
        // Neither version of the big file was written as an object.
        let second = git(&["hash-object", "data.db"]).stdout;
        for hash in [first, second] {
            let hash = String::from_utf8_lossy(&hash).trim().to_string();
            assert!(!git(&["cat-file", "-e", &hash]).status.success());
        }
        let _ = std::fs::remove_dir_all(&repo);
    }
}
