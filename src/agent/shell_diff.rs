//! What a turn's shell commands changed in the checkout. Edit tools report
//! their own diffs; `sed -i`, formatters and generated files don't, so the
//! working files are snapshotted as a git tree when a turn starts and when it
//! ends, and the text files that differ become the turn's shell changes.

use std::path::{Component, Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicU64, Ordering};

use serde::{Deserialize, Serialize};

/// Files kept per turn, and the largest one whose text is kept.
const MAX_FILES: usize = 20;
const MAX_BYTES: usize = 64 * 1024;

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
}

fn git(dir: &Path, index: Option<&Path>, args: &[&str]) -> Option<Vec<u8>> {
    let mut cmd = crate::platform::command("git");
    cmd.args(args).current_dir(dir).stdin(Stdio::null()).stderr(Stdio::null());
    if let Some(index) = index {
        cmd.env("GIT_INDEX_FILE", index);
    }
    cmd.output().ok().filter(|o| o.status.success()).map(|o| o.stdout)
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
    let index = PathBuf::from(line(git(&top, None, &["rev-parse", "--git-path", "index"])?));
    let index = if index.is_relative() { top.join(index) } else { index };
    let tmp = std::env::temp_dir().join(format!("arakun-snap-{}-{}", std::process::id(), N.fetch_add(1, Ordering::Relaxed)));
    // A copy keeps the index's stat cache, so only files changed since get hashed.
    // It also keeps the index's mtime: git rehashes entries modified in the same
    // second as the index ("racy" ones), and a fresh mtime would hide a same-size
    // edit made in that second.
    let _ = std::fs::copy(&index, &tmp);
    if let Ok(mtime) = std::fs::metadata(&index).and_then(|m| m.modified()) {
        let _ = std::fs::File::options().write(true).open(&tmp).and_then(|f| f.set_modified(mtime));
    }
    let tree = git(&top, Some(&tmp), &["add", "-A"]).and_then(|_| git(&top, Some(&tmp), &["write-tree"]));
    let _ = std::fs::remove_file(&tmp);
    Some(Snapshot { top, tree: line(tree?) })
}

/// The text files that differ between two snapshots of the same checkout.
pub fn changes(from: &Snapshot, to: &Snapshot) -> Vec<ShellChange> {
    if from.top != to.top || from.tree == to.tree {
        return Vec::new();
    }
    let top = &from.top;
    let Some(out) = git(top, None, &["diff", "--no-renames", "--numstat", "-z", &from.tree, &to.tree]) else {
        return Vec::new();
    };
    let blob = |tree: &str, path: &str| git(top, None, &["cat-file", "blob", &format!("{tree}:{path}")]);
    let text = |b: Option<Vec<u8>>| match b {
        None => Ok(None),
        Some(b) if b.len() <= MAX_BYTES => String::from_utf8(b).map(Some).map_err(|_| ()),
        Some(_) => Err(()),
    };
    let mut changes = Vec::new();
    for record in String::from_utf8_lossy(&out).split('\0') {
        let mut parts = record.splitn(3, '\t');
        let (Some(added), Some(removed), Some(path)) = (parts.next(), parts.next(), parts.next()) else {
            continue;
        };
        // Binary files count "-".
        let (Ok(added), Ok(removed)) = (added.parse(), removed.parse()) else {
            continue;
        };
        let (Ok(old), Ok(new)) = (text(blob(&from.tree, path)), text(blob(&to.tree, path))) else {
            continue;
        };
        changes.push(ShellChange { path: top.join(path), old, new: new.unwrap_or_default(), added, removed });
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
            let ok = Command::new("sh").arg("-c").arg(c).current_dir(&repo).status().unwrap().success();
            assert!(ok, "{c}");
        };
        sh("git init -q && printf 'a\\nb\\n' > kept.txt && printf 'x\\n' > gone.txt && echo target > .gitignore \
            && git add -A && git -c core.hooksPath=/dev/null -c user.name=t -c user.email=t@t commit -qm init \
            && printf 'staged\\n' > staged.txt && git add staged.txt");
        let before = snapshot(&repo).unwrap();
        sh("printf 'a\\nB\\n' > kept.txt && rm gone.txt && printf 'n\\n' > new.txt && mkdir target && echo o > target/x \
            && printf '\\0\\1' > bin.dat");
        let after = snapshot(&repo).unwrap();
        let mut got = changes(&before, &after);
        got.sort_by(|a, b| a.path.cmp(&b.path));
        let names: Vec<_> = got.iter().map(|c| c.path.strip_prefix(&repo).unwrap().display().to_string()).collect();
        assert_eq!(names, ["gone.txt", "kept.txt", "new.txt"]);
        assert_eq!((got[0].old.as_deref(), got[0].new.as_str()), (Some("x\n"), ""));
        assert_eq!((got[1].old.as_deref(), got[1].new.as_str(), got[1].added, got[1].removed), (Some("a\nb\n"), "a\nB\n", 1, 1));
        assert_eq!((got[2].old.as_deref(), got[2].new.as_str()), (None, "n\n"));
        // The checkout's own index is left as it was.
        let status = Command::new("git").args(["status", "--porcelain"]).current_dir(&repo).output().unwrap();
        assert!(String::from_utf8_lossy(&status.stdout).contains("A  staged.txt"));
        assert!(String::from_utf8_lossy(&status.stdout).contains("?? new.txt"));
        assert!(changes(&after, &snapshot(&repo).unwrap()).is_empty());
        let _ = std::fs::remove_dir_all(&repo);
    }
}
