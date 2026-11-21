//! A repository to clone, read from what a person pastes: a URL, an
//! scp-style address, a `git clone …` line or a GitHub `owner/repo`. The
//! project picker clones on this machine with it, and the server
//! (`crate::server`) on its own when a paired app asks.

use std::path::{Path, PathBuf};

use gpui::SharedString;

#[derive(Clone, Debug, PartialEq)]
pub struct CloneSource {
    /// What git (or `gh`) is given.
    pub url: String,
    /// On github.com, so `gh` can clone it with its sign-in.
    pub on_github: bool,
    /// `owner/repo` on GitHub, else the URL as typed.
    pub label: SharedString,
    /// Folder name of the clone.
    pub name: String,
    /// A bare `owner/repo`, which may also be a search.
    pub shorthand: bool,
}

/// A repository to clone, from a URL, an scp-style address, a pasted
/// `git clone …` or `gh repo clone …` line, or a GitHub `owner/repo`.
pub fn parse_clone_source(query: &str) -> Option<CloneSource> {
    let mut q = query.trim();
    for prefix in ["gh repo clone ", "git clone "] {
        if let Some(rest) = q.strip_prefix(prefix) {
            q = rest;
        }
    }
    // The first argument: flags (and the values of those that take one) and a
    // target folder after it are ignored.
    let words = shell_words(q);
    let mut words = words.iter().map(String::as_str);
    let token = loop {
        let word = words.next()?;
        if TAKES_VALUE.contains(&word) {
            words.next();
        } else if !word.starts_with('-') {
            break word;
        }
    };
    // A query string or fragment (`?tab=readme`, `#readme`) is not part of the repository.
    let token = token.split(['?', '#']).next()?.trim_end_matches('/');

    let (host, path, url) = if let Some((scheme, rest)) = token.split_once("://") {
        if !matches!(scheme, "https" | "http" | "ssh" | "git" | "git+ssh") {
            return None;
        }
        let (authority, path) = rest.split_once('/')?;
        // A password or token would end up in the row and in git's arguments,
        // where other processes can read it. A plain ssh user (`ssh://git@host`) is fine.
        if let Some((user, _)) = authority.rsplit_once('@')
            && (user.contains(':') || scheme.starts_with("http"))
        {
            return None;
        }
        let host = authority.rsplit('@').next()?.split(':').next()?;
        (host.to_string(), path.to_string(), token.to_string())
    } else if let Some((user_host, path)) = token.split_once(':')
        && let Some((_, host)) = user_host.split_once('@')
        && !host.is_empty()
        && !user_host.contains('/')
    {
        (host.to_string(), path.to_string(), token.to_string())
    } else if let Some(path) = token.strip_prefix("github.com/") {
        let url = format!("https://{token}");
        ("github.com".to_string(), path.to_string(), url)
    } else {
        let (owner, repo) = token.split_once('/')?;
        if !is_github_name(owner) || !is_github_name(repo.trim_end_matches(".git")) {
            return None;
        }
        let repo = repo.trim_end_matches(".git");
        return Some(CloneSource {
            url: format!("https://github.com/{owner}/{repo}.git"),
            on_github: true,
            label: format!("{owner}/{repo}").into(),
            name: repo.to_string(),
            shorthand: true,
        });
    };

    let host = host.to_lowercase();
    if host == "github.com" || host == "www.github.com" {
        // A link to a file, branch or pull request still clones its repository.
        let mut parts = path.split('/').filter(|p| !p.is_empty());
        let owner = parts.next()?;
        let repo = parts.next()?.trim_end_matches(".git");
        if !is_github_name(owner) || !is_github_name(repo) {
            return None;
        }
        let url = if url.starts_with("http") {
            format!("https://github.com/{owner}/{repo}.git")
        } else {
            url
        };
        return Some(CloneSource {
            url,
            on_github: true,
            label: format!("{owner}/{repo}").into(),
            name: repo.to_string(),
            shorthand: false,
        });
    }
    let name = path
        .trim_end_matches('/')
        .rsplit('/')
        .next()?
        .trim_end_matches(".git")
        .to_string();
    // The name becomes a folder: nothing that could step out of the clone folder
    // (`..`, or `\` and `C:` on Windows).
    if !is_github_name(&name) || host.is_empty() {
        return None;
    }
    Some(CloneSource {
        label: url.trim_end_matches(".git").to_string().into(),
        url,
        on_github: false,
        name,
        shorthand: false,
    })
}

/// Splits a pasted command into words the way a shell quotes them
/// (`git clone -c "user.name=a b" 'https://…'`), without expanding anything.
fn shell_words(line: &str) -> Vec<String> {
    let mut words = Vec::new();
    let mut word: Option<String> = None;
    let mut quote = None;
    for c in line.chars() {
        match quote {
            Some(q) if c == q => quote = None,
            Some(_) => word.get_or_insert_default().push(c),
            None if c == '"' || c == '\'' => {
                quote = Some(c);
                word.get_or_insert_default();
            }
            None if c.is_whitespace() => words.extend(word.take()),
            None => word.get_or_insert_default().push(c),
        }
    }
    words.extend(word);
    words
}

/// Options of `git clone` and `gh repo clone` whose value is the next word.
const TAKES_VALUE: &[&str] = &[
    "-b",
    "--branch",
    "-o",
    "--origin",
    "-c",
    "--config",
    "--depth",
    "--filter",
    "-j",
    "--jobs",
    "--reference",
    "--reference-if-able",
    "--separate-git-dir",
    "--shallow-exclude",
    "--shallow-since",
    "--template",
    "-u",
    "--upload-pack",
    "--upstream-remote-name",
    "--server-option",
    "--bundle-uri",
];

impl CloneSource {
    /// Whether both name one repository: on GitHub by `owner/repo` whatever the
    /// protocol (GitHub ignores case there), elsewhere by URL.
    pub fn same_repo(&self, other: &CloneSource) -> bool {
        if self.on_github && other.on_github {
            self.label.eq_ignore_ascii_case(&other.label)
        } else {
            self.label == other.label
        }
    }
}

fn is_github_name(s: &str) -> bool {
    !s.is_empty()
        && !s.starts_with('.')
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
}

/// Where new clones go: the folder that already holds most of `repos`
/// (`~/code`, `~/Developer`, …), else `~/code`.
pub fn clone_parent<'a>(repos: impl IntoIterator<Item = &'a Path>, home: &Path) -> PathBuf {
    let mut counts: std::collections::HashMap<&Path, usize> = std::collections::HashMap::new();
    for repo in repos {
        if let Some(parent) = repo.parent()
            && parent != home
        {
            *counts.entry(parent).or_default() += 1;
        }
    }
    counts
        .into_iter()
        .max_by(|a, b| a.1.cmp(&b.1).then(b.0.cmp(a.0)))
        .map(|(p, _)| p.to_path_buf())
        .unwrap_or_else(|| home.join("code"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clone_sources_parse_from_urls_and_commands() {
        let gh = |q: &str| {
            let s = parse_clone_source(q).unwrap_or_else(|| panic!("{q}"));
            assert!(s.on_github, "{q}");
            (s.url, s.label.to_string(), s.name, s.shorthand)
        };
        let https = (
            "https://github.com/o/r.git".to_string(),
            "o/r".to_string(),
            "r".to_string(),
            false,
        );
        assert_eq!(gh("https://github.com/o/r"), https);
        assert_eq!(gh("https://github.com/o/r.git"), https);
        assert_eq!(gh("https://github.com/o/r/pull/12"), https);
        assert_eq!(gh("github.com/o/r/"), https);
        assert_eq!(gh("git clone https://github.com/o/r my-dir"), https);
        assert_eq!(gh("gh repo clone o/r").1, "o/r");
        assert!(gh("o/r").3);
        assert_eq!(
            gh("git@github.com:o/r.git"),
            (
                "git@github.com:o/r.git".to_string(),
                "o/r".to_string(),
                "r".to_string(),
                false
            )
        );

        // Options that take a value don't hide the repository behind them.
        assert_eq!(gh("git clone --depth 1 https://github.com/o/r"), https);
        assert_eq!(
            gh("git clone -b main --quiet https://github.com/o/r dir"),
            https
        );
        assert_eq!(gh("gh repo clone o/r -- --depth 1").1, "o/r");
        assert_eq!(gh(r#"git clone "https://github.com/o/r" 'my dir'"#), https);
        assert_eq!(gh("gh repo clone 'o/r'").1, "o/r");
        assert_eq!(
            gh(r#"git clone -c "user.name=a b" --depth 1 https://github.com/o/r"#),
            https
        );
        // Neither does a query string or a fragment.
        assert_eq!(gh("https://github.com/o/r?tab=readme-ov-file"), https);
        assert_eq!(gh("https://github.com/o/r#readme"), https);
        assert_eq!(
            parse_clone_source("https://gitlab.com/g/proj.git?x=1")
                .unwrap()
                .url,
            "https://gitlab.com/g/proj.git"
        );
        // A plain ssh user is fine; a password or token is not taken in.
        assert_eq!(
            parse_clone_source("ssh://git@gitlab.com/g/proj.git")
                .unwrap()
                .name,
            "proj"
        );
        // The message names the case by number, so not even a made-up secret is printed.
        for (case, with_credentials) in [
            "https://user:pass@gitlab.com/g/proj.git",
            "https://ghp_token@github.com/o/r",
            "ssh://git:pass@host/g/proj.git",
        ]
        .iter()
        .enumerate()
        {
            assert!(
                parse_clone_source(with_credentials).is_none(),
                "case {case}"
            );
        }

        let other = parse_clone_source("https://gitlab.com/g/sub/proj.git").unwrap();
        assert!(!other.on_github);
        assert_eq!(other.name, "proj");
        assert_eq!(other.url, "https://gitlab.com/g/sub/proj.git");
        let scp = parse_clone_source("git@gitlab.com:g/proj").unwrap();
        assert_eq!((scp.name.as_str(), scp.on_github), ("proj", false));

        // A folder name that could leave the clone folder is refused.
        for escape in [
            r"https://host/a/..\..\x",
            "https://host/C:x",
            "git@host:a/..",
        ] {
            assert!(parse_clone_source(escape).is_none(), "{escape}");
        }

        for search in [
            "arakun",
            "my repo",
            "a/b/c",
            "",
            "ftp://x/y",
            "https://github.com/o",
        ] {
            assert!(parse_clone_source(search).is_none(), "{search}");
        }
    }

    #[test]
    fn a_clone_reuses_a_folder_only_for_the_same_repository() {
        let source = |q: &str| parse_clone_source(q).unwrap();
        let wanted = source("https://github.com/owner-b/app");
        assert!(wanted.same_repo(&source("git@github.com:Owner-B/app.git")));
        assert!(!wanted.same_repo(&source("https://github.com/owner-a/app")));
        let gitlab = source("https://gitlab.com/g/app.git");
        assert!(gitlab.same_repo(&source("https://gitlab.com/g/app")));
        assert!(!gitlab.same_repo(&source("https://gitlab.com/h/app")));
    }

    #[test]
    fn pasted_commands_split_like_a_shell() {
        assert_eq!(
            shell_words(r#"git clone -c "a b" 'c d'  e"f g"h "" "#),
            ["git", "clone", "-c", "a b", "c d", "ef gh", ""]
        );
    }
}
