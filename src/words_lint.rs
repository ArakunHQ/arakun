//! Keeps one word per thing in what the user reads: a test that reads the
//! sources and fails on a retired word in a string of prose. The words and
//! what to say instead are in the glossary in CONTRIBUTING.md.
//!
//! Only strings with a space in them count, so a git argument (`"worktree"`),
//! a file name or a JSON key passes. Comments, log lines on stderr and the
//! tests at the bottom of a file are skipped.

use std::path::Path;

/// A retired word, matched whole and in any case, and what to use instead.
const RETIRED: &[(&str, &str)] = &[
    ("worktree", "environment"),
    ("worktrees", "environments"),
    ("conversation", "session"),
    ("conversations", "sessions"),
    ("chat", "session"),
    ("chats", "sessions"),
    ("notification", "notice"),
    ("notifications", "notices"),
    ("go to…", "the palette"),
    ("find anything", "the palette"),
    ("servers tab", "Computers tab"),
    (
        "extension",
        "item (MCP server, plugin, skill or widget), or tab",
    ),
    (
        "extensions",
        "items (MCP servers, plugins, skills and widgets), or tabs",
    ),
    ("toolkit", "Marketplace"),
];

/// Phrases that keep a retired word on purpose, because they name something
/// else: git's own worktrees, the system's and GitHub's notifications.
const ALLOWED_PHRASES: &[&str] = &[
    "git worktree",
    "desktop notification",
    "push notification",
    "github notification",
    // Search keywords list the old word beside the new one, so it still finds.
    "environment worktree",
    // Linear's marker in an issue body, stored in Linear, not read.
    "linear:extension",
];

/// Files whose strings are not the interface: prompts and requests to an
/// agent or a service, and parsers of what other tools write.
const EXEMPT: &[&str] = &[
    "src/words_lint.rs",
    // What every agent is told about the app.
    "core/src/agent/launch.rs",
    // Prompts to agents and the GitHub API, in GitHub's words.
    "src/github/mod.rs",
    "src/github/pr/prompts.rs",
    // Hooks and events of other apps, in their words.
    "src/island_host/external.rs",
    "src/island_host/desktop.rs",
    "core/src/agent/native.rs",
    "core/src/agent/rpc.rs",
    "core/src/agent/workers_ai.rs",
    // The OS's own notifications.
    "core/src/platform.rs",
    // Cloudflare's notifications.
    "src/cloudflare_view.rs",
];

/// Lines that keep a retired word: the text of an older version, read back.
const ALLOWED_LINES: &[&str] = &["Compacted the conversation ("];

/// The string literals on `line`, outside comments. Raw strings and strings
/// that run over several lines are not seen; prose rarely uses them.
fn strings(line: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '/' if chars.peek() == Some(&'/') => break,
            // A char literal such as `'"'`: skip it so its quote opens nothing.
            '\'' => {
                let rest: String = chars.clone().take(3).collect();
                if rest.starts_with('\\') {
                    chars.nth(2);
                } else if rest.chars().nth(1) == Some('\'') {
                    chars.nth(1);
                }
            }
            '"' => {
                let mut s = String::new();
                while let Some(c) = chars.next() {
                    match c {
                        '\\' => {
                            chars.next();
                            s.push(' ');
                        }
                        '"' => break,
                        _ => s.push(c),
                    }
                }
                out.push(s);
            }
            _ => {}
        }
    }
    out
}

/// The retired words in one string of prose, with what to use instead.
fn retired(text: &str) -> Vec<String> {
    if !text.trim().contains(' ') {
        return Vec::new();
    }
    let mut text = text.to_lowercase();
    for phrase in ALLOWED_PHRASES {
        text = text.replace(phrase, " ");
    }
    let mut found = Vec::new();
    for (word, instead) in RETIRED {
        let whole = text.match_indices(word).any(|(i, _)| {
            let before = text[..i].chars().next_back();
            let after = text[i + word.len()..].chars().next();
            !before.is_some_and(|c| c.is_alphanumeric() || c == '_' || c == '-')
                && !after.is_some_and(|c| c.is_alphanumeric() || c == '_' || c == '-')
        });
        if whole {
            found.push(format!("\"{word}\": say \"{instead}\""));
        }
    }
    found
}

fn sources(dir: &Path, out: &mut Vec<std::path::PathBuf>) {
    for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        let p = entry.path();
        if p.is_dir() {
            sources(&p, out);
        } else if p.extension().is_some_and(|x| x == "rs") {
            out.push(p);
        }
    }
}

#[test]
fn the_interface_says_one_word_per_thing() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut files = Vec::new();
    sources(&root.join("src"), &mut files);
    sources(&root.join("core/src"), &mut files);
    files.sort();
    let mut report = Vec::new();
    for file in files {
        let rel = file
            .strip_prefix(root)
            .unwrap_or(&file)
            .to_string_lossy()
            .replace('\\', "/");
        if EXEMPT.contains(&rel.as_str()) {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(&file) else {
            continue;
        };
        let mut test_attr = false;
        for (n, line) in text.lines().enumerate() {
            let t = line.trim();
            // The tests at the bottom of the file: their strings are fixtures.
            if test_attr && t.starts_with("mod tests") {
                break;
            }
            test_attr = t == "#[cfg(test)]";
            if t.starts_with("eprintln!") || ALLOWED_LINES.iter().any(|a| line.contains(a)) {
                continue;
            }
            for s in strings(line) {
                for p in retired(&s) {
                    report.push(format!("{rel}:{}: {p}", n + 1));
                }
            }
        }
    }
    assert!(
        report.is_empty(),
        "{} retired words (see the glossary in CONTRIBUTING.md):\n{}",
        report.len(),
        report.join("\n")
    );
}

#[test]
fn the_word_lint_catches_what_it_should() {
    assert_eq!(
        retired("This worktree is empty."),
        ["\"worktree\": say \"environment\""]
    );
    assert_eq!(
        retired("Search conversations..."),
        ["\"conversations\": say \"sessions\""]
    );
    assert_eq!(retired("Find in chat").len(), 1);
    assert_eq!(retired("No notifications yet").len(), 1);
    assert_eq!(
        retired("Open the Servers tab"),
        ["\"servers tab\": say \"Computers tab\""]
    );
    assert!(retired("your servers show up here").is_empty());
    // Not prose, part of a longer word, or a phrase that names something else.
    assert!(retired("worktree").is_empty());
    assert!(retired("worktree ").is_empty());
    assert!(retired("Sign in with ChatGPT here").is_empty());
    assert!(retired("keep chatting with it").is_empty());
    assert!(retired("an environment is a git worktree of it").is_empty());
    assert!(retired("shows as a desktop notification too").is_empty());
    assert!(retired("the hook-notification event").is_empty());
    assert_eq!(strings(r#"a("one two") // "three four""#), ["one two"]);
    assert_eq!(strings(r#"f('"', "x \" y")"#), ["x   y"]);
}
