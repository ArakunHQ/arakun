//! What the clipboard holds, when it is worth a card at the top of the
//! palette: a GitHub link to open, or an error to fix with an agent.

/// A GitHub link, read from its URL.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GitHubLink {
    Pull { repo: String, number: u64 },
    Issue { repo: String, number: u64 },
    Run { repo: String, id: u64 },
    Commit { repo: String, sha: String },
}

impl GitHubLink {
    pub fn repo(&self) -> &str {
        match self {
            GitHubLink::Pull { repo, .. }
            | GitHubLink::Issue { repo, .. }
            | GitHubLink::Run { repo, .. }
            | GitHubLink::Commit { repo, .. } => repo,
        }
    }
}

/// An error message: its first line, where it points (file and line), and
/// the text itself, trimmed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ErrorText {
    /// What kind of error, to say on the card: "a Rust error".
    pub language: &'static str,
    pub headline: String,
    pub location: Option<(String, u32)>,
    pub text: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Clip {
    GitHub { url: String, link: GitHubLink },
    Error(ErrorText),
}

/// Reads the clipboard's text; None for anything not worth a card.
pub fn classify(text: &str) -> Option<Clip> {
    let t = text.trim();
    if t.is_empty() || t.len() > 64 * 1024 {
        return None;
    }
    if !t.contains(char::is_whitespace)
        && let Some(link) = github_link(t)
    {
        return Some(Clip::GitHub {
            url: t.to_string(),
            link,
        });
    }
    error_text(t).map(Clip::Error)
}

/// `https://github.com/o/r/pull/64`, with or without the scheme, and with any
/// tail (`/files`, `#issuecomment-…`).
pub fn github_link(url: &str) -> Option<GitHubLink> {
    let rest = url
        .trim_start_matches("https://")
        .trim_start_matches("http://")
        .trim_start_matches("www.")
        .strip_prefix("github.com/")?;
    let rest = rest.split(['#', '?']).next()?;
    let parts: Vec<&str> = rest.split('/').filter(|p| !p.is_empty()).collect();
    let [owner, name, kind, id, ..] = parts.as_slice() else {
        return None;
    };
    let repo = format!("{owner}/{name}");
    match *kind {
        "pull" => Some(GitHubLink::Pull {
            repo,
            number: id.parse().ok()?,
        }),
        "issues" => Some(GitHubLink::Issue {
            repo,
            number: id.parse().ok()?,
        }),
        "actions" => {
            // actions/runs/<id>
            let run = parts.get(4)?;
            (*id == "runs").then_some(())?;
            Some(GitHubLink::Run {
                repo,
                id: run.parse().ok()?,
            })
        }
        "commit" if id.len() >= 7 && id.chars().all(|c| c.is_ascii_hexdigit()) => {
            Some(GitHubLink::Commit {
                repo,
                sha: id.to_string(),
            })
        }
        _ => None,
    }
}

/// Compiler and runtime errors the way tools print them.
fn error_text(t: &str) -> Option<ErrorText> {
    let lines: Vec<&str> = t.lines().collect();
    let first = |pred: &dyn Fn(&str) -> bool| lines.iter().find(|l| pred(l.trim())).copied();
    let (language, headline) = if let Some(l) =
        first(&|l| l.starts_with("error[E") || (l.starts_with("error: ") && t.contains("-->")))
    {
        ("a Rust error", l)
    } else if let Some(l) = first(&|l| l.contains("panicked at")) {
        ("a Rust panic", l)
    } else if let Some(l) = first(&|l| l.contains("error TS") && l.contains(':')) {
        ("a TypeScript error", l)
    } else if let Some(l) = first(&|l| l.starts_with("Traceback (most recent call last)")) {
        let last = lines
            .iter()
            .rev()
            .find(|l| !l.trim().is_empty())
            .copied()
            .unwrap_or(l);
        ("a Python error", last)
    } else if let Some(l) = first(&|l| {
        (l.starts_with("Error: ")
            || l.starts_with("TypeError: ")
            || l.starts_with("ReferenceError: "))
            && t.contains("    at ")
    }) {
        ("a JavaScript error", l)
    } else {
        let l = first(&|l| l.starts_with("FAILED ") || l.starts_with("FAIL "))?;
        ("a failed test", l)
    };
    Some(ErrorText {
        language,
        headline: headline.trim().chars().take(200).collect(),
        location: location(t),
        text: lines
            .iter()
            .take(40)
            .copied()
            .collect::<Vec<_>>()
            .join("\n"),
    })
}

/// The first `path:line` in the text: Rust's `--> src/a.rs:12:5`, tsc's
/// `src/a.ts(88,14)`, Python's `File "a.py", line 3`, or a bare `a.rs:12`.
pub fn location(t: &str) -> Option<(String, u32)> {
    for line in t.lines() {
        let l = line.trim();
        if let Some(rest) = l.strip_prefix("--> ").or_else(|| l.strip_prefix("──> "))
            && let Some(found) = path_line(rest)
        {
            return Some(found);
        }
        if let Some(start) = l.strip_prefix("File \"")
            && let Some((path, tail)) = start.split_once('"')
            && let Some(n) = tail
                .trim_start_matches(", line ")
                .split(|c: char| !c.is_ascii_digit())
                .next()
                .and_then(|n| n.parse().ok())
        {
            return Some((path.to_string(), n));
        }
        if let Some((path, tail)) = l.split_once('(')
            && is_path(path)
            && let Some(n) = tail.split([',', ')']).next().and_then(|n| n.parse().ok())
        {
            return Some((path.to_string(), n));
        }
    }
    t.split_whitespace().find_map(path_line)
}

fn is_path(p: &str) -> bool {
    let p = p.trim();
    !p.is_empty()
        && !p.contains(' ')
        && p.rsplit_once('.').is_some_and(|(stem, ext)| {
            !stem.is_empty()
                && (1..=5).contains(&ext.len())
                && ext.chars().all(|c| c.is_ascii_alphanumeric())
        })
}

fn path_line(word: &str) -> Option<(String, u32)> {
    let word = word.trim_matches(|c: char| c == '`' || c == '\'' || c == '"' || c == ',');
    let mut parts = word.split(':');
    let path = parts.next()?;
    let n: u32 = parts.next()?.parse().ok()?;
    is_path(path).then(|| (path.to_string(), n))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn github_links_are_read() {
        assert_eq!(
            github_link("https://github.com/ArakunHQ/arakun/pull/64"),
            Some(GitHubLink::Pull {
                repo: "ArakunHQ/arakun".into(),
                number: 64
            })
        );
        assert_eq!(
            github_link("github.com/o/r/pull/7/files#diff-1"),
            Some(GitHubLink::Pull {
                repo: "o/r".into(),
                number: 7
            })
        );
        assert_eq!(
            github_link("https://github.com/o/r/issues/3#issuecomment-9"),
            Some(GitHubLink::Issue {
                repo: "o/r".into(),
                number: 3
            })
        );
        assert_eq!(
            github_link("https://github.com/o/r/actions/runs/4812/job/1"),
            Some(GitHubLink::Run {
                repo: "o/r".into(),
                id: 4812
            })
        );
        assert_eq!(
            github_link("https://github.com/o/r/commit/faec1aa0"),
            Some(GitHubLink::Commit {
                repo: "o/r".into(),
                sha: "faec1aa0".into()
            })
        );
        assert_eq!(github_link("https://github.com/o/r"), None);
        assert_eq!(github_link("https://gitlab.com/o/r/pull/1"), None);
        assert_eq!(github_link("https://github.com/o/r/pull/abc"), None);
    }

    #[test]
    fn a_rust_error_is_an_error_with_its_place() {
        let t = "error[E0382]: use of moved value: `buttons`\n    --> src/island.rs:1675:29\n     |\n1599 |  let buttons = (a.buttons()\n";
        let Some(Clip::Error(e)) = classify(t) else {
            panic!("not an error")
        };
        assert_eq!(e.language, "a Rust error");
        assert_eq!(e.headline, "error[E0382]: use of moved value: `buttons`");
        assert_eq!(e.location, Some(("src/island.rs".into(), 1675)));
    }

    #[test]
    fn a_typescript_error_points_at_its_line() {
        let t = "src/index.ts(88,14): error TS2345: Argument of type 'string | undefined'";
        let Some(Clip::Error(e)) = classify(t) else {
            panic!("not an error")
        };
        assert_eq!(e.language, "a TypeScript error");
        assert_eq!(e.location, Some(("src/index.ts".into(), 88)));
    }

    #[test]
    fn a_python_traceback_keeps_its_last_line() {
        let t = "Traceback (most recent call last):\n  File \"app.py\", line 3, in <module>\n    x()\nNameError: name 'x' is not defined";
        let Some(Clip::Error(e)) = classify(t) else {
            panic!("not an error")
        };
        assert_eq!(e.headline, "NameError: name 'x' is not defined");
        assert_eq!(e.location, Some(("app.py".into(), 3)));
    }

    #[test]
    fn a_panic_is_an_error() {
        let t = "thread 'main' panicked at src/main.rs:12:5:\nboom";
        let Some(Clip::Error(e)) = classify(t) else {
            panic!("not an error")
        };
        assert_eq!(e.language, "a Rust panic");
        assert_eq!(e.location, Some(("src/main.rs".into(), 12)));
    }

    #[test]
    fn plain_text_is_not_a_card() {
        assert_eq!(classify("hello world"), None);
        assert_eq!(classify(""), None);
        assert_eq!(classify("the error was mine"), None);
        assert_eq!(classify("see github.com/o/r/pull/1 please"), None);
    }

    #[test]
    fn a_url_is_a_github_card() {
        assert!(matches!(
            classify("  https://github.com/o/r/pull/64\n"),
            Some(Clip::GitHub {
                link: GitHubLink::Pull { number: 64, .. },
                ..
            })
        ));
    }
}
