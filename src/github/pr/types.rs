//! Which pull request or issue, and the shapes `gh` answers in.

use serde::Deserialize;

use super::*;
use crate::github::Notification;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ItemKind {
    Pull,
    Issue,
}

/// Which pull request or issue: enough to name it to `gh`.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct ItemRef {
    /// `owner/name`.
    pub repo: String,
    pub number: u64,
    pub kind: ItemKind,
}

/// Whether `repo` is `owner/name` with nothing that could change what a `gh`
/// path or a file name means.
pub(super) fn valid_repo(repo: &str) -> bool {
    let ok = |p: &str| {
        !p.is_empty()
            && p != "."
            && p != ".."
            && p.chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
    };
    repo.split_once('/')
        .is_some_and(|(owner, name)| ok(owner) && ok(name))
}

impl ItemRef {
    /// The pull request or issue a notification is about; None for the other
    /// kinds (CI runs, releases, commits, discussions).
    pub fn from_notification(n: &Notification) -> Option<Self> {
        let mut item = n
            .subject
            .url
            .as_deref()
            .and_then(Self::from_url)
            .or_else(|| Self::from_url(&n.web_url()))?;
        if valid_repo(&n.repository.full_name) {
            item.repo = n.repository.full_name.clone();
        }
        Some(item)
    }

    /// From a web address (`https://github.com/o/r/pull/12/files`) or an API one
    /// (`https://api.github.com/repos/o/r/pulls/12`). The host is not kept: `gh`
    /// is asked with `-R owner/name`, which means its default host. Only for
    /// addresses `gh` itself gave (a notification, a search); one a person or
    /// an agent typed goes through `from_url_on`.
    pub fn from_url(url: &str) -> Option<Self> {
        Self::split_url(url).map(|(_, item)| item)
    }

    /// From a web address typed by someone, only when it is on `host`, the
    /// host `gh` talks to. Any other would name a pull request there that
    /// `-R owner/name` reads, and posts to, on `host`.
    pub fn from_url_on(url: &str, host: &str) -> Option<Self> {
        let (at, item) = Self::split_url(url)?;
        let at = at.to_ascii_lowercase();
        let host = host.to_ascii_lowercase();
        // `www.` is an alias only on top of the host: with `GH_HOST` set to
        // `www.ghe.corp`, an address on that host must still match it.
        (at == host || at.strip_prefix("www.") == Some(host.as_str())).then_some(item)
    }

    /// The host of a web or API address, with the item it names.
    pub fn host_and_item(url: &str) -> Option<(String, Self)> {
        Self::split_url(url)
    }

    fn split_url(url: &str) -> Option<(String, Self)> {
        let url = url.trim();
        let url = url
            .strip_prefix("https://")
            .or_else(|| url.strip_prefix("http://"))
            .unwrap_or(url);
        let url = url.split(['?', '#']).next()?;
        let mut parts: Vec<&str> = url.split('/').filter(|p| !p.is_empty()).collect();
        // The host, then what an API address puts before the repository.
        if parts.is_empty() || !parts[0].contains('.') {
            return None;
        }
        let host = parts.remove(0).to_string();
        if parts.starts_with(&["api", "v3", "repos"]) {
            parts.drain(..3);
        } else if parts.first() == Some(&"repos") {
            parts.remove(0);
        }
        let [owner, name, what, number, ..] = parts.as_slice() else {
            return None;
        };
        let kind = match *what {
            "pull" | "pulls" => ItemKind::Pull,
            "issues" => ItemKind::Issue,
            _ => return None,
        };
        let repo = format!("{owner}/{name}");
        let number = number.parse().ok().filter(|n| *n > 0)?;
        valid_repo(&repo).then_some((host, Self { repo, number, kind }))
    }

    /// From `owner/repo#12`, the short form people and agents type. It does not
    /// say which kind it is: a pull request is assumed.
    pub fn parse(text: &str) -> Option<Self> {
        let (repo, number) = text.trim().split_once('#')?;
        let number = number.parse().ok().filter(|n| *n > 0)?;
        valid_repo(repo).then(|| Self {
            repo: repo.to_string(),
            number,
            kind: ItemKind::Pull,
        })
    }

    /// `owner/repo#12`.
    pub fn label(&self) -> String {
        format!("{}#{}", self.repo, self.number)
    }

    /// Its web address on the host `gh` talks to, which is where `-R
    /// owner/name` finds it: with `GH_HOST` set, not github.com.
    pub fn url(&self) -> String {
        self.url_on(&gh_host())
    }

    fn url_on(&self, host: &str) -> String {
        let page = match self.kind {
            ItemKind::Pull => "pull",
            ItemKind::Issue => "issues",
        };
        format!("https://{host}/{}/{page}/{}", self.repo, self.number)
    }

    /// The `gh` subcommand family: `gh pr …` or `gh issue …`.
    pub(super) fn family(&self) -> &'static str {
        match self.kind {
            ItemKind::Pull => "pr",
            ItemKind::Issue => "issue",
        }
    }
}

// ---------------------------------------------------------------------------
// What gh returns

#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct Actor {
    /// Empty for a deleted account.
    pub login: String,
    pub name: String,
    pub is_bot: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct Label {
    pub name: String,
    /// Six hex digits, no `#`.
    pub color: String,
    pub description: String,
}

/// Someone whose review was asked for: a person (`login`) or a team (`slug`).
#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct ReviewRequest {
    pub login: String,
    pub name: String,
    pub slug: String,
}

impl ReviewRequest {
    pub fn who(&self) -> &str {
        [&self.login, &self.slug, &self.name]
            .into_iter()
            .find(|s| !s.is_empty())
            .map_or("", String::as_str)
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct CommitRef {
    pub oid: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Review {
    /// GraphQL node id; an inline comment's `review` names it.
    pub id: String,
    pub author: Actor,
    pub body: String,
    pub submitted_at: String,
    /// `APPROVED`, `CHANGES_REQUESTED`, `COMMENTED`, `DISMISSED` or `PENDING`.
    pub state: String,
    /// The head commit the review was written against.
    pub commit: CommitRef,
}

/// A comment on the conversation (not on a line of the diff).
#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Comment {
    pub id: String,
    pub author: Actor,
    pub body: String,
    pub created_at: String,
    pub url: String,
    /// Hidden on GitHub (spam, outdated, off-topic).
    pub is_minimized: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct CommitAuthor {
    pub login: String,
    pub name: String,
    pub email: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Commit {
    pub oid: String,
    pub message_headline: String,
    pub message_body: String,
    pub authored_date: String,
    /// Changes on a rebase, so it tells when the commit reached the branch.
    pub committed_date: String,
    pub authors: Vec<CommitAuthor>,
}

impl Commit {
    /// The first author's login, or their name when the email has no account.
    pub fn author(&self) -> &str {
        self.authors.first().map_or("", |a| {
            if a.login.is_empty() {
                a.name.as_str()
            } else {
                a.login.as_str()
            }
        })
    }

    pub fn short_oid(&self) -> &str {
        self.oid.get(..7).unwrap_or(&self.oid)
    }
}

/// One entry of `statusCheckRollup` as gh gives it: a check run (`name`,
/// `status`, `conclusion`) or a commit status (`context`, `state`).
#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Check {
    pub name: String,
    pub status: String,
    pub conclusion: String,
    pub details_url: String,
    pub workflow_name: String,
    pub started_at: String,
    pub completed_at: String,
    pub context: String,
    pub state: String,
    pub target_url: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct ChangedFile {
    pub path: String,
    pub additions: u32,
    pub deletions: u32,
    /// `ADDED`, `MODIFIED`, `DELETED`, `RENAMED`, `COPIED` or `CHANGED`.
    pub change_type: String,
}

/// A comment on a line of the diff, from the REST API.
#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct InlineComment {
    /// What `reply` needs.
    pub id: u64,
    pub path: String,
    /// The line in the current diff; None once the code under it changed.
    pub line: Option<u32>,
    pub original_line: Option<u32>,
    pub body: String,
    pub user: Actor,
    pub created_at: String,
    /// The first comment of the thread this one answers.
    pub in_reply_to_id: Option<u64>,
    pub diff_hunk: String,
    pub html_url: String,
    /// `LEFT` when it is on a line the change removed, else `RIGHT`.
    pub side: String,
    /// The node id of the review it was sent with (`Review::id`). None when
    /// GitHub didn't say (see `fetch_pull`).
    #[serde(skip)]
    pub review: Option<String>,
    /// Whether its thread was marked resolved. None when GitHub didn't say.
    #[serde(skip)]
    pub resolved: Option<bool>,
    /// Whether the code under its thread changed since.
    #[serde(skip)]
    pub outdated: Option<bool>,
    /// The node id of its thread. None when GitHub didn't say.
    #[serde(skip)]
    pub thread: Option<String>,
}

impl InlineComment {
    /// The line to show it at: the current one, or where it was written.
    pub fn shown_line(&self) -> Option<u32> {
        self.line.or(self.original_line)
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Pull {
    pub number: u64,
    pub title: String,
    pub body: String,
    /// `OPEN`, `CLOSED` or `MERGED`.
    pub state: String,
    pub is_draft: bool,
    pub author: Actor,
    pub base_ref_name: String,
    pub head_ref_name: String,
    pub head_ref_oid: String,
    /// The branch lives in a fork, not in the pull request's repository.
    pub is_cross_repository: bool,
    /// `MERGEABLE`, `CONFLICTING` or `UNKNOWN` (GitHub is still working it out).
    pub mergeable: String,
    /// `CLEAN`, `BLOCKED`, `BEHIND`, `DIRTY`, `DRAFT`, `UNSTABLE`, `HAS_HOOKS` or `UNKNOWN`.
    pub merge_state_status: String,
    /// `APPROVED`, `CHANGES_REQUESTED`, `REVIEW_REQUIRED`, or empty when the
    /// repository asks for no review.
    pub review_decision: String,
    pub additions: u32,
    pub deletions: u32,
    pub changed_files: u32,
    pub created_at: String,
    pub updated_at: String,
    /// Empty while it is open.
    pub closed_at: String,
    pub merged_at: String,
    pub url: String,
    pub labels: Vec<Label>,
    pub assignees: Vec<Actor>,
    pub review_requests: Vec<ReviewRequest>,
    /// The last review of each reviewer.
    pub latest_reviews: Vec<Review>,
    pub reviews: Vec<Review>,
    pub comments: Vec<Comment>,
    pub commits: Vec<Commit>,
    pub status_check_rollup: Vec<Check>,
    pub files: Vec<ChangedFile>,
    /// Comments on lines of the diff. Not part of `gh pr view`: `fetch_pull`
    /// reads them apart.
    #[serde(skip)]
    pub inline: Vec<InlineComment>,
}

impl Pull {
    /// The branch an agent works on locally. A fork's branch gets a name of
    /// its own: it is often `main`, which would be the clone's own main.
    pub fn local_branch(&self) -> String {
        if self.is_cross_repository {
            format!("pr-{}", self.number)
        } else {
            self.head_ref_name.clone()
        }
    }

    pub fn open(&self) -> bool {
        self.state == "OPEN"
    }

    pub fn merged(&self) -> bool {
        self.state == "MERGED"
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Issue {
    pub number: u64,
    pub title: String,
    pub body: String,
    /// `OPEN` or `CLOSED`.
    pub state: String,
    pub author: Actor,
    pub labels: Vec<Label>,
    pub assignees: Vec<Actor>,
    pub comments: Vec<Comment>,
    pub created_at: String,
    pub updated_at: String,
    /// Empty while it is open.
    pub closed_at: String,
    pub url: String,
}

impl Issue {
    pub fn open(&self) -> bool {
        self.state == "OPEN"
    }
}

pub use arakun_core::github::short_sha;

pub(super) fn secs(time: &str) -> u64 {
    crate::ci::parse_time(time).unwrap_or(0)
}

pub(super) fn same_login(a: &str, b: &str) -> bool {
    !a.is_empty() && a.eq_ignore_ascii_case(b)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::github::pr::fixtures::*;
    use crate::github::{Owner, Repository, Subject};

    #[test]
    fn references_come_from_addresses() {
        let pull = |repo: &str, number| {
            Some(ItemRef {
                repo: repo.into(),
                number,
                kind: ItemKind::Pull,
            })
        };
        let issue = |repo: &str, number| {
            Some(ItemRef {
                repo: repo.into(),
                number,
                kind: ItemKind::Issue,
            })
        };
        assert_eq!(
            ItemRef::from_url("https://github.com/ArakunHQ/arakun/pull/34"),
            pull("ArakunHQ/arakun", 34)
        );
        assert_eq!(
            ItemRef::from_url(" github.com/o/r/pull/7/files?diff=split#diff-abc "),
            pull("o/r", 7)
        );
        assert_eq!(
            ItemRef::from_url("https://github.com/o/r.js/issues/33#issuecomment-6884196378"),
            issue("o/r.js", 33)
        );
        assert_eq!(
            ItemRef::from_url("https://api.github.com/repos/o/r/pulls/12"),
            pull("o/r", 12)
        );
        assert_eq!(
            ItemRef::from_url("https://api.github.com/repos/o/r/issues/12/comments"),
            issue("o/r", 12)
        );
        assert_eq!(
            ItemRef::from_url("https://ghe.corp/api/v3/repos/o/r/pulls/4"),
            pull("o/r", 4)
        );
        // A typed address counts only on gh's host: o/r#7 elsewhere is
        // another pull request.
        assert_eq!(
            ItemRef::from_url_on("https://ghe.corp/o/r/pull/7", "github.com"),
            None
        );
        assert_eq!(
            ItemRef::from_url_on("https://github.com.evil.io/o/r/pull/7", "github.com"),
            None
        );
        assert_eq!(
            ItemRef::from_url_on("https://www.GitHub.com/o/r/pull/7", "github.com"),
            pull("o/r", 7)
        );
        assert_eq!(
            ItemRef::from_url_on("https://www.ghe.corp/o/r/pull/7", "www.ghe.corp"),
            pull("o/r", 7)
        );
        assert_eq!(
            ItemRef::from_url_on("https://ghe.corp/o/r/pull/7", "GHE.corp"),
            pull("o/r", 7)
        );
        for bad in [
            "",
            "https://github.com/o/r",
            "https://github.com/o/r/pulls",
            "https://github.com/o/r/pull/abc",
            "https://github.com/o/r/pull/0",
            "https://github.com/o/r/commit/2af8316",
            "https://github.com/o/r/actions/runs/1",
            "https://github.com/../r/pull/1",
            "o/r/pull/1",
            "not an address",
        ] {
            assert_eq!(ItemRef::from_url(bad), None, "{bad:?}");
        }

        assert_eq!(
            ItemRef::parse("ArakunHQ/arakun#34"),
            pull("ArakunHQ/arakun", 34)
        );
        for bad in [
            "arakun#34",
            "o/r#",
            "o/r#x",
            "o/r#0",
            "o/r/x#1",
            "o/r",
            "o/..#1",
            "o/r #1",
        ] {
            assert_eq!(ItemRef::parse(bad), None, "{bad:?}");
        }

        let it = item(ItemKind::Pull);
        assert_eq!(
            (it.label().as_str(), it.url_on("github.com").as_str()),
            (
                "ArakunHQ/arakun#41",
                "https://github.com/ArakunHQ/arakun/pull/41"
            )
        );
        assert_eq!(
            item(ItemKind::Issue).url_on("github.com"),
            "https://github.com/ArakunHQ/arakun/issues/41"
        );
        // With GH_HOST on GitHub Enterprise, the address names that host.
        assert_eq!(
            it.url_on("ghe.corp"),
            "https://ghe.corp/ArakunHQ/arakun/pull/41"
        );
        assert_eq!(ItemRef::from_url(&it.url_on("github.com")), Some(it));
    }

    #[test]
    fn references_come_from_notifications() {
        let note = |kind: &str, url: Option<&str>| Notification {
            id: "1".into(),
            subject: Subject {
                title: "t".into(),
                url: url.map(str::to_string),
                latest_comment_url: None,
                kind: kind.into(),
            },
            repository: Repository {
                full_name: "ArakunHQ/arakun".into(),
                html_url: "https://github.com/ArakunHQ/arakun".into(),
                owner: Owner {
                    login: "ArakunHQ".into(),
                },
            },
            ..Default::default()
        };
        let from = |kind, url| ItemRef::from_notification(&note(kind, url));
        assert_eq!(
            from(
                "PullRequest",
                Some("https://api.github.com/repos/arakunhq/arakun/pulls/39")
            ),
            Some(ItemRef {
                repo: "ArakunHQ/arakun".into(),
                number: 39,
                kind: ItemKind::Pull
            })
        );
        assert_eq!(
            from(
                "Issue",
                Some("https://api.github.com/repos/ArakunHQ/arakun/issues/33")
            ),
            Some(ItemRef {
                repo: "ArakunHQ/arakun".into(),
                number: 33,
                kind: ItemKind::Issue
            })
        );
        // Nothing to open natively: no number, or not a pull request or issue.
        assert_eq!(from("PullRequest", None), None);
        assert_eq!(from("CheckSuite", None), None);
        assert_eq!(
            from(
                "Commit",
                Some("https://api.github.com/repos/ArakunHQ/arakun/commits/2af8316")
            ),
            None
        );
        assert_eq!(
            from(
                "Release",
                Some("https://api.github.com/repos/ArakunHQ/arakun/releases/9911")
            ),
            None
        );
    }
}
