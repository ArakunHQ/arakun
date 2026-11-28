//! A unified diff split into files, hunks and numbered lines.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FileStatus {
    Added,
    Deleted,
    Modified,
    Renamed,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LineKind {
    Context,
    Add,
    Del,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Line {
    pub kind: LineKind,
    /// The line's number before the change; None for an added line.
    pub old_no: Option<u32>,
    /// The line's number after the change; None for a deleted line.
    pub new_no: Option<u32>,
    /// Without the `+`, `-` or space in front.
    pub text: String,
    /// It is the last line of its side and has no newline at the end.
    pub no_newline: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Hunk {
    /// The whole `@@ -1,4 +1,6 @@ fn main()` line.
    pub header: String,
    pub lines: Vec<Line>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileDiff {
    pub path: String,
    /// The name it had before, for a renamed file.
    pub old_path: Option<String>,
    pub status: FileStatus,
    pub additions: u32,
    pub deletions: u32,
    pub hunks: Vec<Hunk>,
    /// A binary file: there are no lines to show.
    pub binary: bool,
}

/// A path as git writes it when it has unusual characters: in double quotes,
/// with C escapes and bytes in octal. Anything else is returned as it is.
fn unquote(text: &str) -> String {
    let Some(inner) = text.strip_prefix('"').and_then(|t| t.strip_suffix('"')) else {
        return text.to_string();
    };
    let mut out = Vec::with_capacity(inner.len());
    let mut bytes = inner.bytes().peekable();
    while let Some(b) = bytes.next() {
        if b != b'\\' {
            out.push(b);
            continue;
        }
        match bytes.next() {
            Some(b'n') => out.push(b'\n'),
            Some(b't') => out.push(b'\t'),
            Some(b'r') => out.push(b'\r'),
            Some(d @ b'0'..=b'7') => {
                let mut value = u32::from(d - b'0');
                for _ in 0..2 {
                    match bytes.peek() {
                        Some(&d @ b'0'..=b'7') => {
                            value = value * 8 + u32::from(d - b'0');
                            bytes.next();
                        }
                        _ => break,
                    }
                }
                out.push(value as u8);
            }
            Some(other) => out.push(other),
            None => {}
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Where a path in quotes ends: the index of its closing quote.
fn quoted_end(text: &str) -> Option<usize> {
    if !text.starts_with('"') {
        return None;
    }
    let mut escaped = false;
    for (i, b) in text.bytes().enumerate().skip(1) {
        match b {
            _ if escaped => escaped = false,
            b'\\' => escaped = true,
            b'"' => return Some(i),
            _ => {}
        }
    }
    None
}

/// The old and new path of a `diff --git a/old b/new` line.
fn header_paths(rest: &str) -> (String, String) {
    let (old, new) = if let Some(end) = quoted_end(rest) {
        (
            unquote(&rest[..=end]),
            unquote(rest[end + 1..].trim_start()),
        )
    } else if let Some((old, new)) = rest.split_once(" \"") {
        (old.to_string(), unquote(&format!("\"{new}")))
    } else {
        // Git doesn't quote spaces, so a name with spaces leaves no sure place
        // to split. But both halves name the same file unless it was renamed,
        // and then `rename from` and `rename to` say the names.
        let mid = rest.len() / 2;
        match (rest.get(..mid), rest.get(mid + 1..)) {
            (Some(old), Some(new))
                if rest.as_bytes()[mid] == b' ' && old.get(2..) == new.get(2..) =>
            {
                (old.to_string(), new.to_string())
            }
            _ => match rest.split_once(" b/") {
                Some((old, new)) => (old.to_string(), format!("b/{new}")),
                None => (rest.to_string(), rest.to_string()),
            },
        }
    };
    let strip = |path: String, prefix: &str| {
        path.strip_prefix(prefix)
            .map(str::to_string)
            .unwrap_or(path)
    };
    (strip(old, "a/"), strip(new, "b/"))
}

/// The path of a `--- a/path` or `+++ b/path` line; None for `/dev/null`.
fn marker_path(text: &str) -> Option<String> {
    // Git ends the name with a tab when it has spaces.
    let text = text.strip_suffix('\t').unwrap_or(text);
    if text == "/dev/null" {
        return None;
    }
    let path = unquote(text);
    Some(
        path.strip_prefix("a/")
            .or_else(|| path.strip_prefix("b/"))
            .map(str::to_string)
            .unwrap_or(path),
    )
}

/// `@@ -3,7 +3,9 @@` as ((old start, old count), (new start, new count)).
fn hunk_ranges(line: &str) -> Option<((u32, u32), (u32, u32))> {
    let (ranges, _) = line.strip_prefix("@@ -")?.split_once(" @@")?;
    let (old, new) = ranges.split_once(" +")?;
    let range = |text: &str| -> Option<(u32, u32)> {
        match text.split_once(',') {
            Some((start, count)) => Some((start.parse().ok()?, count.parse().ok()?)),
            None => Some((text.parse().ok()?, 1)),
        }
    };
    Some((range(old)?, range(new)?))
}

/// A unified diff (`gh pr diff`, `git diff`) as files, hunks and numbered
/// lines. A diff that was cut short gives what is there.
pub fn split_diff(diff: &str) -> Vec<FileDiff> {
    let mut files: Vec<FileDiff> = Vec::new();
    // Lines the open hunk still has to give (old side, new side). Counting them
    // is what tells a deleted line that starts with `-- ` from a `--- a/file` header.
    let mut left = (0u32, 0u32);
    let mut next = (0u32, 0u32);
    // What follows the last newline is not a line: in a cut hunk it would read as an empty one.
    for line in diff.strip_suffix('\n').unwrap_or(diff).split('\n') {
        if line.starts_with('\\') {
            // "\ No newline at end of file" is about the line before it.
            if let Some(last) = files
                .last_mut()
                .and_then(|f| f.hunks.last_mut())
                .and_then(|h| h.lines.last_mut())
            {
                last.no_newline = true;
            }
            continue;
        }
        if left != (0, 0) {
            let kind = match line.as_bytes().first() {
                Some(b'+') => Some(LineKind::Add),
                Some(b'-') => Some(LineKind::Del),
                // Some tools drop the lone space of an empty context line.
                Some(b' ') | None => Some(LineKind::Context),
                _ => None,
            };
            let hunk = files.last_mut().and_then(|f| {
                f.hunks
                    .last_mut()
                    .map(|h| (&mut f.additions, &mut f.deletions, h))
            });
            if let (Some(kind), Some((additions, deletions, hunk))) = (kind, hunk) {
                let (old, new) = (kind != LineKind::Add, kind != LineKind::Del);
                hunk.lines.push(Line {
                    kind,
                    old_no: old.then_some(next.0),
                    new_no: new.then_some(next.1),
                    text: line.get(1..).unwrap_or_default().to_string(),
                    no_newline: false,
                });
                if old {
                    next.0 += 1;
                    left.0 = left.0.saturating_sub(1);
                }
                if new {
                    next.1 += 1;
                    left.1 = left.1.saturating_sub(1);
                }
                match kind {
                    LineKind::Add => *additions += 1,
                    LineKind::Del => *deletions += 1,
                    LineKind::Context => {}
                }
                continue;
            }
            // Not a line of a hunk: the hunk was shorter than it said.
            left = (0, 0);
        }
        if let Some(rest) = line.strip_prefix("diff --git ") {
            let (old, path) = header_paths(rest);
            files.push(FileDiff {
                path,
                old_path: None,
                status: FileStatus::Modified,
                additions: 0,
                deletions: 0,
                hunks: Vec::new(),
                binary: false,
            });
            // Kept only if the lines after it say the file was renamed.
            let _ = old;
            continue;
        }
        let Some(file) = files.last_mut() else {
            continue;
        };
        if line.starts_with("new file mode ") {
            file.status = FileStatus::Added;
        } else if line.starts_with("deleted file mode ") {
            file.status = FileStatus::Deleted;
        } else if let Some(old) = line.strip_prefix("rename from ") {
            file.old_path = Some(unquote(old));
            file.status = FileStatus::Renamed;
        } else if let Some(new) = line.strip_prefix("rename to ") {
            file.path = unquote(new);
            file.status = FileStatus::Renamed;
        } else if let Some(old) = line.strip_prefix("--- ") {
            match marker_path(old) {
                // The name to keep if the file was deleted; `+++` replaces it otherwise.
                Some(path) => file.path = path,
                None => file.status = FileStatus::Added,
            }
        } else if let Some(new) = line.strip_prefix("+++ ") {
            match marker_path(new) {
                Some(path) => file.path = path,
                None => file.status = FileStatus::Deleted,
            }
        } else if line.starts_with("Binary files ") || line == "GIT binary patch" {
            file.binary = true;
        } else if let Some((old, new)) = hunk_ranges(line) {
            left = (old.1, new.1);
            next = (old.0, new.0);
            file.hunks.push(Hunk {
                header: line.to_string(),
                lines: Vec::new(),
            });
        }
    }
    files
}

#[cfg(test)]
mod tests {
    use super::*;

    /// What `gh pr diff` prints for a pull request that touches a bit of
    /// everything.
    const DIFF: &str = concat!(
        "diff --git a/src/main.rs b/src/main.rs\n",
        "index 1a2b3c4..5d6e7f8 100644\n",
        "--- a/src/main.rs\n",
        "+++ b/src/main.rs\n",
        "@@ -1,5 +1,6 @@\n",
        " use std::io;\n",
        "-use std::fs;\n",
        "+use std::fs::File;\n",
        "+use std::path::Path;\n",
        " \n",
        " fn main() {\n",
        "     run();\n",
        "@@ -40,5 +41,4 @@ fn run() {\n",
        "     let a = 1;\n",
        "---- a comment that looks like a header\n",
        "-++ and one more\n",
        "+-- replaced\n",
        "     done();\n",
        " }\n",
        "diff --git a/docs/new file.md b/docs/new file.md\n",
        "new file mode 100644\n",
        "index 0000000..da44549\n",
        "--- /dev/null\n",
        "+++ b/docs/new file.md\t\n",
        "@@ -0,0 +1,2 @@\n",
        "+# Title\n",
        "+text\n",
        "\\ No newline at end of file\n",
        "diff --git a/old.txt b/old.txt\n",
        "deleted file mode 100644\n",
        "index 3b18e51..0000000\n",
        "--- a/old.txt\n",
        "+++ /dev/null\n",
        "@@ -1 +0,0 @@\n",
        "-hello world\n",
        "diff --git a/src/old name.rs b/src/new name.rs\n",
        "similarity index 90%\n",
        "rename from src/old name.rs\n",
        "rename to src/new name.rs\n",
        "index 1111111..2222222 100644\n",
        "--- a/src/old name.rs\t\n",
        "+++ b/src/new name.rs\t\n",
        "@@ -7,3 +7,3 @@ mod tests {\n",
        " a\n",
        "-b\n",
        "\\ No newline at end of file\n",
        "+c\n",
        "\\ No newline at end of file\n",
        "diff --git a/moved/a b.txt b/moved/c d.txt\n",
        "similarity index 100%\n",
        "rename from moved/a b.txt\n",
        "rename to moved/c d.txt\n",
        "diff --git a/assets/my logo.png b/assets/my logo.png\n",
        "new file mode 100644\n",
        "index 0000000..89abcde\n",
        "Binary files /dev/null and b/assets/my logo.png differ\n",
        "diff --git a/assets/icon.png b/assets/icon.png\n",
        "index 89abcde..fedcba9 100644\n",
        "GIT binary patch\n",
        "literal 12\n",
        "Tc$@(M0RR91000mG0{{R3\n",
        "\n",
        "diff --git \"a/docs/caf\\303\\251 \\\"x\\\".md\" \"b/docs/caf\\303\\251 \\\"x\\\".md\"\n",
        "old mode 100644\n",
        "new mode 100755\n",
        "diff --git a/empty b/empty\n",
        "new file mode 100644\n",
        "index 0000000..e69de29\n",
    );

    #[test]
    fn splits_a_diff_into_files_hunks_and_lines() {
        let files = split_diff(DIFF);
        let shape: Vec<_> = files
            .iter()
            .map(|f| {
                (
                    f.path.as_str(),
                    f.old_path.as_deref(),
                    f.status,
                    f.additions,
                    f.deletions,
                    f.hunks.len(),
                    f.binary,
                )
            })
            .collect();
        use FileStatus::*;
        assert_eq!(
            shape,
            [
                ("src/main.rs", None, Modified, 3, 3, 2, false),
                ("docs/new file.md", None, Added, 2, 0, 1, false),
                ("old.txt", None, Deleted, 0, 1, 1, false),
                (
                    "src/new name.rs",
                    Some("src/old name.rs"),
                    Renamed,
                    1,
                    1,
                    1,
                    false
                ),
                (
                    "moved/c d.txt",
                    Some("moved/a b.txt"),
                    Renamed,
                    0,
                    0,
                    0,
                    false
                ),
                ("assets/my logo.png", None, Added, 0, 0, 0, true),
                ("assets/icon.png", None, Modified, 0, 0, 0, true),
                ("docs/café \"x\".md", None, Modified, 0, 0, 0, false),
                ("empty", None, Added, 0, 0, 0, false),
            ]
        );

        let line = |l: &Line| (l.kind, l.old_no, l.new_no, l.text.clone());
        let first = &files[0].hunks[0];
        assert_eq!(first.header, "@@ -1,5 +1,6 @@");
        assert_eq!(
            first.lines.iter().map(line).collect::<Vec<_>>(),
            [
                (
                    LineKind::Context,
                    Some(1),
                    Some(1),
                    "use std::io;".to_string()
                ),
                (LineKind::Del, Some(2), None, "use std::fs;".to_string()),
                (
                    LineKind::Add,
                    None,
                    Some(2),
                    "use std::fs::File;".to_string()
                ),
                (
                    LineKind::Add,
                    None,
                    Some(3),
                    "use std::path::Path;".to_string()
                ),
                (LineKind::Context, Some(3), Some(4), String::new()),
                (
                    LineKind::Context,
                    Some(4),
                    Some(5),
                    "fn main() {".to_string()
                ),
                (
                    LineKind::Context,
                    Some(5),
                    Some(6),
                    "    run();".to_string()
                ),
            ]
        );
        // Changed lines that look like file headers stay lines of the hunk.
        let second = &files[0].hunks[1];
        assert_eq!(second.header, "@@ -40,5 +41,4 @@ fn run() {");
        assert_eq!(
            second.lines.iter().map(line).collect::<Vec<_>>(),
            [
                (
                    LineKind::Context,
                    Some(40),
                    Some(41),
                    "    let a = 1;".to_string()
                ),
                (
                    LineKind::Del,
                    Some(41),
                    None,
                    "--- a comment that looks like a header".to_string()
                ),
                (LineKind::Del, Some(42), None, "++ and one more".to_string()),
                (LineKind::Add, None, Some(42), "-- replaced".to_string()),
                (
                    LineKind::Context,
                    Some(43),
                    Some(43),
                    "    done();".to_string()
                ),
                (LineKind::Context, Some(44), Some(44), "}".to_string()),
            ]
        );

        // "\ No newline at end of file" marks the line before it and is no line itself.
        let added = &files[1].hunks[0].lines;
        assert_eq!(
            added
                .iter()
                .map(|l| (l.text.as_str(), l.new_no, l.no_newline))
                .collect::<Vec<_>>(),
            [("# Title", Some(1), false), ("text", Some(2), true)]
        );
        let renamed = &files[3].hunks[0].lines;
        assert_eq!(
            renamed
                .iter()
                .map(|l| (l.kind, l.text.as_str(), l.no_newline))
                .collect::<Vec<_>>(),
            [
                (LineKind::Context, "a", false),
                (LineKind::Del, "b", true),
                (LineKind::Add, "c", true)
            ]
        );
        assert_eq!(files[2].hunks[0].lines[0].old_no, Some(1));
    }

    #[test]
    fn a_diff_that_is_cut_or_odd_still_splits() {
        assert!(split_diff("").is_empty());
        assert!(split_diff("not a diff\n").is_empty());

        // Cut in the middle of a hunk: what is there is kept.
        let cut = "diff --git a/a.rs b/a.rs\n--- a/a.rs\n+++ b/a.rs\n@@ -1,9 +1,9 @@\n one\n-two\n";
        let files = split_diff(cut);
        assert_eq!(
            (
                files.len(),
                files[0].deletions,
                files[0].hunks[0].lines.len()
            ),
            (1, 1, 2)
        );

        // A hunk shorter than it says doesn't swallow the next file.
        let short = "diff --git a/a b/a\n--- a/a\n+++ b/a\n@@ -1,5 +1,5 @@\n-x\n+y\ndiff --git a/b b/b\n--- a/b\n+++ b/b\n@@ -1 +1 @@\n-p\n+q\n";
        let files = split_diff(short);
        assert_eq!(
            files
                .iter()
                .map(|f| (f.path.as_str(), f.additions, f.deletions))
                .collect::<Vec<_>>(),
            [("a", 1, 1), ("b", 1, 1)]
        );

        // Windows line ends are part of the text, not of the format.
        let crlf = "diff --git a/a b/a\n--- a/a\n+++ b/a\n@@ -1 +1 @@\n-x\r\n+y\r\n";
        assert_eq!(split_diff(crlf)[0].hunks[0].lines[1].text, "y\r");

        assert_eq!(
            header_paths("a/x y z b/x y z"),
            ("x y z".to_string(), "x y z".to_string())
        );
        assert_eq!(header_paths("a/x b/y"), ("x".to_string(), "y".to_string()));
        assert_eq!(
            header_paths("\"a/t\\tb\" b/plain"),
            ("t\tb".to_string(), "plain".to_string())
        );
        assert_eq!(
            header_paths("a/plain \"b/\\346\\227\\245\""),
            ("plain".to_string(), "日".to_string())
        );
        assert_eq!(hunk_ranges("@@ -1 +0,0 @@"), Some(((1, 1), (0, 0))));
        assert_eq!(hunk_ranges("@@ garbage @@"), None);
    }
}
