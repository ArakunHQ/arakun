//! The open pull request of a delegated issue: a session continues on it
//! instead of opening a second one. It is one of the issue's links in Linear,
//! in the project's own repository, and `gh` says it is still open.

use std::path::Path;

use serde::Deserialize;

use super::issues::Link;
use crate::git;
use crate::platform::GIT_REMOTE;

/// Most pull request links asked about: each is a call to GitHub.
const ASKED: usize = 5;

/// An open pull request the session pushes to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OpenPr {
    pub url: String,
    pub number: u64,
    /// Its head branch, in the project's repository.
    pub branch: String,
}

/// A GitHub pull request link: `owner/name` and its number. Anything after
/// the number (`/files`, `#discussion`) is let go.
pub fn parse_url(url: &str) -> Option<(String, u64)> {
    let rest = url.trim().strip_prefix("https://github.com/")?;
    let mut parts = rest.split(['/', '?', '#']);
    let (owner, name) = (parts.next()?, parts.next()?);
    if parts.next()? != "pull" {
        return None;
    }
    let number = parts.next()?.parse().ok().filter(|n| *n > 0)?;
    let ok = |s: &str| {
        !s.is_empty()
            && s.chars()
                .all(|c| c.is_ascii_alphanumeric() || "-_.".contains(c))
    };
    (ok(owner) && ok(name)).then(|| (format!("{owner}/{name}"), number))
}

/// The issue's pull request links in a repository `ours` says is the
/// project's, once each, as `https://github.com/owner/name/pull/N`.
pub fn candidates(links: &[Link], ours: impl Fn(&str) -> bool) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for link in links {
        let Some((slug, number)) = parse_url(&link.url) else {
            continue;
        };
        let url = format!("https://github.com/{slug}/pull/{number}");
        if !out.contains(&url) && ours(&slug) {
            out.push(url);
        }
    }
    out.truncate(ASKED);
    out
}

/// What `gh pr view --json` says of one pull request.
#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct View {
    pub url: String,
    pub number: u64,
    /// OPEN, CLOSED or MERGED.
    pub state: String,
    pub head_ref_name: String,
    #[serde(default)]
    pub is_cross_repository: bool,
}

/// The first open pull request of `views`. A merged or closed one does not
/// count. One from a fork is an error for now: the session could not push to it.
pub fn pick(views: &[View]) -> Result<Option<OpenPr>, String> {
    let open: Vec<&View> = views
        .iter()
        .filter(|v| v.state.eq_ignore_ascii_case("open") && !v.head_ref_name.is_empty())
        .collect();
    if let Some(v) = open.iter().find(|v| !v.is_cross_repository) {
        return Ok(Some(OpenPr {
            url: v.url.clone(),
            number: v.number,
            branch: v.head_ref_name.clone(),
        }));
    }
    match open.first() {
        Some(v) => Err(fork_line(v.number)),
        None => Ok(None),
    }
}

pub fn fork_line(number: u64) -> String {
    format!(
        "its open pull request #{number} is from a fork, and Arakun can't push to a fork's \
         branch yet"
    )
}

/// Off the UI thread: the issue's open pull request in the repository at
/// `repo`, whose `origin` it must be in. None when there is none, or when
/// `gh` can't tell (not installed, signed out, offline): the session then
/// starts as it would without one.
pub fn open_pr(repo: &Path, links: &[Link]) -> Result<Option<OpenPr>, String> {
    let urls = candidates(links, |slug| {
        git::remote_for(repo, slug).as_deref() == Some("origin")
    });
    let views: Vec<View> = urls.iter().filter_map(|url| view(repo, url)).collect();
    pick(&views)
}

fn view(repo: &Path, url: &str) -> Option<View> {
    let out = git::output(
        git::gh(repo).args([
            "pr",
            "view",
            url,
            "--json",
            "url,number,state,headRefName,isCrossRepository",
        ]),
        GIT_REMOTE,
    )
    .ok()?;
    if !out.status.success() {
        return None;
    }
    serde_json::from_slice(&out.stdout).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn link(url: &str) -> Link {
        Link {
            title: "PR".into(),
            url: url.into(),
        }
    }

    fn view(number: u64, state: &str, branch: &str, fork: bool) -> View {
        View {
            url: format!("https://github.com/a/b/pull/{number}"),
            number,
            state: state.into(),
            head_ref_name: branch.into(),
            is_cross_repository: fork,
        }
    }

    #[test]
    fn a_pull_request_link_is_read_with_or_without_a_tail() {
        let one = Some(("acme/web".to_string(), 12));
        assert_eq!(parse_url("https://github.com/acme/web/pull/12"), one);
        assert_eq!(parse_url("https://github.com/acme/web/pull/12/files"), one);
        assert_eq!(
            parse_url("https://github.com/acme/web/pull/12#issuecomment-1"),
            one
        );
        assert_eq!(parse_url("https://github.com/acme/web/issues/12"), None);
        assert_eq!(parse_url("https://gitlab.com/acme/web/pull/12"), None);
        assert_eq!(parse_url("http://github.com/acme/web/pull/12"), None);
        assert_eq!(parse_url("https://github.com/acme/web/pull/x"), None);
        assert_eq!(parse_url("https://github.com/acme/web/pull/0"), None);
        assert_eq!(parse_url("https://github.com/ac me/web/pull/1"), None);
        assert_eq!(parse_url("https://github.com/acme/web/pull"), None);
    }

    #[test]
    fn only_the_projects_own_pull_requests_are_asked_about_once_each() {
        let links = [
            link("https://sentry.io/issues/1"),
            link("https://github.com/acme/web/pull/3"),
            link("https://github.com/other/web/pull/4"),
            link("https://github.com/acme/web/pull/3/files"),
            link("https://github.com/Acme/Web/pull/5"),
        ];
        let ours = |s: &str| s.eq_ignore_ascii_case("acme/web");
        assert_eq!(
            candidates(&links, ours),
            [
                "https://github.com/acme/web/pull/3",
                "https://github.com/Acme/Web/pull/5"
            ]
        );
        let many: Vec<Link> = (1..20)
            .map(|n| link(&format!("https://github.com/acme/web/pull/{n}")))
            .collect();
        assert_eq!(candidates(&many, ours).len(), ASKED);
    }

    #[test]
    fn the_first_open_pull_request_is_used() {
        let views = [
            view(1, "MERGED", "old", false),
            view(2, "CLOSED", "gone", false),
            view(3, "OPEN", "rik/ara-1", false),
            view(4, "OPEN", "other", false),
        ];
        assert_eq!(
            pick(&views),
            Ok(Some(OpenPr {
                url: "https://github.com/a/b/pull/3".into(),
                number: 3,
                branch: "rik/ara-1".into(),
            }))
        );
        assert_eq!(pick(&views[..2]), Ok(None));
        assert_eq!(pick(&[]), Ok(None));
    }

    #[test]
    fn a_pull_request_from_a_fork_is_an_error_unless_another_is_open() {
        let fork = view(7, "OPEN", "patch-1", true);
        assert_eq!(pick(std::slice::from_ref(&fork)), Err(fork_line(7)));
        let ours = view(8, "OPEN", "ara-1", false);
        assert_eq!(pick(&[fork, ours]).unwrap().unwrap().number, 8);
    }

    #[test]
    fn gh_s_answer_is_read() {
        let text = r#"{"headRefName":"rik/ara-1","isCrossRepository":false,
            "number":3,"state":"OPEN","url":"https://github.com/a/b/pull/3"}"#;
        let v: View = serde_json::from_str(text).unwrap();
        assert_eq!(v, view(3, "OPEN", "rik/ara-1", false));
    }
}
