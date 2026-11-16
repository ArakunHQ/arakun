//! One pull request or issue, read and acted on through `gh`: what the native
//! view shows (checks, what blocks the merge, who it waits on, the timeline, the
//! diff) and what it can do (comment, review, merge). Every call here blocks;
//! run them off the main thread. Reviews written by an agent are kept as drafts
//! on disk: nothing reaches GitHub until the user posts it.

use std::collections::HashMap;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::OnceLock;

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::{Notification, Problem};
use crate::git;

// ---------------------------------------------------------------------------
// References

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
fn valid_repo(repo: &str) -> bool {
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
        let at = at.strip_prefix("www.").unwrap_or(&at);
        (at == host).then_some(item)
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
    fn family(&self) -> &'static str {
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

/// A commit as people quote it: its first seven characters. A draft's SHA
/// comes from a file on disk, so it is cut on a character boundary: a byte cut
/// would panic on a damaged file.
pub fn short_sha(sha: &str) -> &str {
    sha.char_indices()
        .nth(7)
        .map_or(sha, |(end, _)| &sha[..end])
}

fn secs(time: &str) -> u64 {
    crate::ci::parse_time(time).unwrap_or(0)
}

fn same_login(a: &str, b: &str) -> bool {
    !a.is_empty() && a.eq_ignore_ascii_case(b)
}

// ---------------------------------------------------------------------------
// Reading

const PULL_FIELDS: &str = "number,title,body,state,isDraft,author,baseRefName,headRefName,headRefOid,mergeable,\
mergeStateStatus,reviewDecision,additions,deletions,changedFiles,createdAt,updatedAt,closedAt,mergedAt,url,labels,assignees,\
reviewRequests,latestReviews,reviews,comments,commits,statusCheckRollup,files";
const ISSUE_FIELDS: &str =
    "number,title,body,state,author,labels,assignees,comments,createdAt,updatedAt,closedAt,url";

/// Resolution and review of each inline comment: REST doesn't give them.
const THREADS_QUERY: &str = "query($owner:String!,$name:String!,$number:Int!){repository(owner:$owner,name:$name){\
pullRequest(number:$number){reviewThreads(first:100){nodes{isResolved isOutdated \
comments(first:100){nodes{databaseId pullRequestReview{id}}}}}}}}";

/// Drops every `null`, so a field GitHub left empty reads as its default
/// instead of failing the whole answer (a deleted author is `null`).
fn without_nulls(value: Value) -> Value {
    match value {
        Value::Object(map) => Value::Object(
            map.into_iter()
                .filter(|(_, v)| !v.is_null())
                .map(|(k, v)| (k, without_nulls(v)))
                .collect(),
        ),
        Value::Array(list) => Value::Array(
            list.into_iter()
                .filter(|v| !v.is_null())
                .map(without_nulls)
                .collect(),
        ),
        other => other,
    }
}

fn unexpected(e: impl std::fmt::Display) -> Problem {
    Problem::Other(format!("unexpected answer from gh: {e}"))
}

fn read<T: DeserializeOwned>(bytes: &[u8]) -> Result<T, Problem> {
    let value: Value = serde_json::from_slice(bytes).map_err(unexpected)?;
    serde_json::from_value(without_nulls(value)).map_err(unexpected)
}

/// The items of every page of a `gh api --paginate` answer. gh joins the pages
/// of a list into one, but older versions wrote one list after the other.
fn read_pages<T: DeserializeOwned>(bytes: &[u8]) -> Result<Vec<T>, Problem> {
    let mut all = Vec::new();
    for page in serde_json::Deserializer::from_slice(bytes).into_iter::<Value>() {
        let page: Vec<T> =
            serde_json::from_value(without_nulls(page.map_err(unexpected)?)).map_err(unexpected)?;
        all.extend(page);
    }
    Ok(all)
}

struct ThreadState {
    resolved: bool,
    outdated: bool,
    review: Option<String>,
}

/// Comment id → the state of its thread, from the answer to `THREADS_QUERY`.
fn thread_states(answer: &Value) -> HashMap<u64, ThreadState> {
    let mut states = HashMap::new();
    let threads = answer
        .pointer("/data/repository/pullRequest/reviewThreads/nodes")
        .and_then(Value::as_array);
    for thread in threads.into_iter().flatten() {
        let flag = |key: &str| thread.get(key).and_then(Value::as_bool).unwrap_or(false);
        let comments = thread.pointer("/comments/nodes").and_then(Value::as_array);
        for comment in comments.into_iter().flatten() {
            let Some(id) = comment.get("databaseId").and_then(Value::as_u64) else {
                continue;
            };
            let review = comment
                .pointer("/pullRequestReview/id")
                .and_then(Value::as_str)
                .map(str::to_string);
            states.insert(
                id,
                ThreadState {
                    resolved: flag("isResolved"),
                    outdated: flag("isOutdated"),
                    review,
                },
            );
        }
    }
    states
}

fn fetch_inline(item: &ItemRef) -> Result<Vec<InlineComment>, Problem> {
    let path = format!(
        "repos/{}/pulls/{}/comments?per_page=100",
        item.repo, item.number
    );
    read_pages(&super::gh_out(&["api", &path, "--paginate"])?)
}

fn fetch_thread_states(item: &ItemRef) -> Result<HashMap<u64, ThreadState>, Problem> {
    let (owner, name) = item.repo.split_once('/').unwrap_or((&item.repo, ""));
    let out = super::gh_out(&[
        "api",
        "graphql",
        "-f",
        &format!("query={THREADS_QUERY}"),
        "-f",
        &format!("owner={owner}"),
        "-f",
        &format!("name={name}"),
        "-F",
        &format!("number={}", item.number),
    ])?;
    Ok(thread_states(
        &serde_json::from_slice(&out).map_err(unexpected)?,
    ))
}

/// The pull request with its inline comments. Three reads run at once: the
/// pull request, its inline comments, and which threads are resolved. The last
/// one is a help, not a need: when it fails the comments keep `resolved`,
/// `outdated` and `review` as None and the rest is still returned.
pub fn fetch_pull(item: &ItemRef) -> Result<Pull, Problem> {
    let number = item.number.to_string();
    let (pull, inline, states) = std::thread::scope(|s| {
        let inline = s.spawn(|| fetch_inline(item));
        let states = s.spawn(|| fetch_thread_states(item));
        let pull = super::gh_out(&[
            "pr",
            "view",
            &number,
            "-R",
            &item.repo,
            "--json",
            PULL_FIELDS,
        ]);
        (pull, inline.join(), states.join())
    });
    let mut pull: Pull = read(&pull?)?;
    pull.inline =
        inline.map_err(|_| Problem::Other("reading the review comments failed".into()))??;
    if let Ok(Ok(states)) = states {
        for comment in &mut pull.inline {
            if let Some(state) = states.get(&comment.id) {
                comment.resolved = Some(state.resolved);
                comment.outdated = Some(state.outdated);
                comment.review = state.review.clone();
            }
        }
    }
    Ok(pull)
}

/// The commit at the head of a pull request now: what a draft is written for.
pub fn head_sha(item: &ItemRef) -> Result<String, Problem> {
    let number = item.number.to_string();
    let out = super::gh_out(&[
        "pr",
        "view",
        &number,
        "-R",
        &item.repo,
        "--json",
        "headRefOid",
        "--jq",
        ".headRefOid",
    ])?;
    let sha = String::from_utf8_lossy(&out).trim().to_string();
    if sha.is_empty() {
        Err(Problem::Other(
            "gh did not say the pull request's head commit".into(),
        ))
    } else {
        Ok(sha)
    }
}

/// What a pull request's diff is taken between: its head commit and the
/// branch it merges into. A push moves the first, a retarget the second.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Tip {
    pub head: String,
    pub base: String,
}

/// The pull request's `Tip` now, read from GitHub.
pub fn tip(item: &ItemRef) -> Result<Tip, Problem> {
    let number = item.number.to_string();
    let out = super::gh_out(&[
        "pr",
        "view",
        &number,
        "-R",
        &item.repo,
        "--json",
        "headRefOid,baseRefName",
    ])?;
    let v: serde_json::Value = serde_json::from_slice(&out)
        .map_err(|e| Problem::Other(format!("gh answered something unreadable: {e}")))?;
    let tip = Tip {
        head: v["headRefOid"].as_str().unwrap_or("").to_string(),
        base: v["baseRefName"].as_str().unwrap_or("").to_string(),
    };
    if tip.head.is_empty() || tip.base.is_empty() {
        return Err(Problem::Other(
            "gh did not say the pull request's head commit and base".into(),
        ));
    }
    Ok(tip)
}

/// The diff of a pull request together with the `Tip` it shows. `gh pr diff`
/// reads whatever the pull request is at that moment, so the tip is read
/// before and after it: when they differ (a push or a retarget landed
/// meanwhile), the diff could be of either and is refused.
pub fn fetch_diff_at(item: &ItemRef) -> Result<(Diff, Tip), Problem> {
    let before = tip(item)?;
    let diff = fetch_diff(item)?;
    let after = tip(item)?;
    if before != after {
        return Err(Problem::Other(
            "the pull request changed while its diff was read; try again".into(),
        ));
    }
    Ok((diff, after))
}

pub fn fetch_issue(item: &ItemRef) -> Result<Issue, Problem> {
    let number = item.number.to_string();
    read(&super::gh_out(&[
        "issue",
        "view",
        &number,
        "-R",
        &item.repo,
        "--json",
        ISSUE_FIELDS,
    ])?)
}

/// How much of a diff is kept: past this a view can't show it well anyway.
pub const DIFF_CAP: usize = 2 * 1024 * 1024;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Diff {
    pub text: String,
    /// The diff was longer than `DIFF_CAP`: `text` stops at the end of a line
    /// and the files after that are missing.
    pub cut: bool,
    /// The size of the whole diff.
    pub bytes: usize,
}

fn capped(mut text: String, cap: usize) -> Diff {
    let bytes = text.len();
    if bytes <= cap {
        return Diff {
            text,
            cut: false,
            bytes,
        };
    }
    let mut end = cap;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    // Half a line would read as a changed line that isn't in the diff.
    let end = text[..end].rfind('\n').map_or(0, |i| i + 1);
    text.truncate(end);
    Diff {
        text,
        cut: true,
        bytes,
    }
}

/// The diff of a pull request. GitHub refuses the whole diff past 20 000
/// lines; then it is put together from each file's patch instead.
pub fn fetch_diff(item: &ItemRef) -> Result<Diff, Problem> {
    let number = item.number.to_string();
    match super::gh_out(&["pr", "diff", &number, "-R", &item.repo]) {
        Ok(out) => Ok(capped(String::from_utf8_lossy(&out).into_owned(), DIFF_CAP)),
        Err(Problem::Other(e)) if too_large(&e) => {
            let path = format!(
                "repos/{}/pulls/{}/files?per_page=100",
                item.repo, item.number
            );
            let files: Vec<FilePatch> = read_pages(&super::gh_out(&["api", &path, "--paginate"])?)?;
            Ok(capped(diff_of_patches(&files), DIFF_CAP))
        }
        Err(e) => Err(e),
    }
}

fn too_large(error: &str) -> bool {
    error.contains("too_large") || error.contains("diff exceeded the maximum")
}

/// One file of `GET /repos/{repo}/pulls/{n}/files`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct FilePatch {
    pub filename: String,
    pub previous_filename: String,
    /// `added`, `removed`, `modified`, `renamed`, `copied`, `changed`, `unchanged`.
    pub status: String,
    /// The hunks; GitHub leaves it out for a binary file or one too large to show.
    pub patch: String,
}

/// The files' patches as one unified diff, for `split_diff`.
fn diff_of_patches(files: &[FilePatch]) -> String {
    let mut out = String::new();
    for f in files {
        let old = if f.previous_filename.is_empty() {
            &f.filename
        } else {
            &f.previous_filename
        };
        out.push_str(&format!("diff --git a/{old} b/{}\n", f.filename));
        match f.status.as_str() {
            "added" => out.push_str("new file mode 100644\n"),
            "removed" => out.push_str("deleted file mode 100644\n"),
            "renamed" => out.push_str(&format!("rename from {old}\nrename to {}\n", f.filename)),
            _ => {}
        }
        if f.patch.is_empty() {
            continue;
        }
        let (a, b) = match f.status.as_str() {
            "added" => ("/dev/null".to_string(), format!("b/{}", f.filename)),
            "removed" => (format!("a/{old}"), "/dev/null".to_string()),
            _ => (format!("a/{old}"), format!("b/{}", f.filename)),
        };
        out.push_str(&format!(
            "--- {a}\n+++ {b}\n{}\n",
            f.patch.trim_end_matches('\n')
        ));
    }
    out
}

/// The login `gh` is signed in as. Asked once per run.
pub fn me() -> Result<String, Problem> {
    static ME: OnceLock<String> = OnceLock::new();
    if let Some(login) = ME.get() {
        return Ok(login.clone());
    }
    let out = super::gh_out(&["api", "user", "--jq", ".login"])?;
    let login = String::from_utf8_lossy(&out).trim().to_string();
    if login.is_empty() {
        return Err(Problem::Other("gh did not say who is signed in".into()));
    }
    Ok(ME.get_or_init(|| login).clone())
}

// ---------------------------------------------------------------------------
// Checks

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CheckState {
    Passed,
    Failed,
    /// Queued, waiting or in progress.
    Running,
    /// Skipped or neutral: it says nothing against the change.
    Skipped,
}

/// A check run or a commit status, in one shape.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CheckRun {
    pub name: String,
    /// The workflow it belongs to; empty for a commit status.
    pub workflow: String,
    pub state: CheckState,
    pub url: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ChecksSummary {
    pub passed: usize,
    pub failed: usize,
    pub running: usize,
    pub skipped: usize,
    /// One entry per check, in GitHub's order.
    pub checks: Vec<CheckRun>,
}

impl ChecksSummary {
    pub fn total(&self) -> usize {
        self.checks.len()
    }

    /// All the checks as one state: any failing fails, any running runs.
    /// None when nothing ran or all were skipped.
    pub fn overall(&self) -> Option<CheckState> {
        if self.failed > 0 {
            Some(CheckState::Failed)
        } else if self.running > 0 {
            Some(CheckState::Running)
        } else if self.passed > 0 {
            Some(CheckState::Passed)
        } else {
            None
        }
    }

    pub fn names(&self, state: CheckState) -> Vec<String> {
        self.checks
            .iter()
            .filter(|c| c.state == state)
            .map(|c| c.name.clone())
            .collect()
    }
}

impl Check {
    fn normalised(&self) -> CheckRun {
        // A commit status has a `context` and no `name`.
        if self.name.is_empty() && !self.context.is_empty() {
            let state = match self.state.as_str() {
                "SUCCESS" => CheckState::Passed,
                "PENDING" | "EXPECTED" => CheckState::Running,
                _ => CheckState::Failed,
            };
            return CheckRun {
                name: self.context.clone(),
                workflow: String::new(),
                state,
                url: self.target_url.clone(),
            };
        }
        let state = if self.status != "COMPLETED" {
            CheckState::Running
        } else {
            match self.conclusion.as_str() {
                "SUCCESS" => CheckState::Passed,
                "SKIPPED" | "NEUTRAL" => CheckState::Skipped,
                _ => CheckState::Failed,
            }
        };
        CheckRun {
            name: self.name.clone(),
            workflow: self.workflow_name.clone(),
            state,
            url: self.details_url.clone(),
        }
    }

    fn started(&self) -> u64 {
        secs(&self.started_at).max(secs(&self.completed_at))
    }
}

/// The checks of the head commit, one per check: a check that ran again shows
/// its latest run only. Two workflows may both have a job called `build`; those
/// are two checks, so the workflow is part of what makes a run "the same".
pub fn checks_summary(pull: &Pull) -> ChecksSummary {
    let mut latest: Vec<(u64, CheckRun)> = Vec::new();
    for check in &pull.status_check_rollup {
        let (at, run) = (check.started(), check.normalised());
        match latest
            .iter_mut()
            .find(|(_, c)| c.name == run.name && c.workflow == run.workflow)
        {
            // GitHub lists a rerun after the run it replaces, so a tie goes to the later entry.
            Some(seen) if at >= seen.0 => *seen = (at, run),
            Some(_) => {}
            None => latest.push((at, run)),
        }
    }
    let mut sum = ChecksSummary {
        checks: latest.into_iter().map(|(_, c)| c).collect(),
        ..Default::default()
    };
    for check in &sum.checks {
        match check.state {
            CheckState::Passed => sum.passed += 1,
            CheckState::Failed => sum.failed += 1,
            CheckState::Running => sum.running += 1,
            CheckState::Skipped => sum.skipped += 1,
        }
    }
    sum
}

// ---------------------------------------------------------------------------
// What blocks the merge, and who it waits on

/// One reason a pull request can't be merged now. An open pull request with no
/// blockers is ready to merge.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Blocker {
    /// Already merged: there is nothing left to do.
    Merged,
    /// Closed without merging.
    Closed,
    Draft,
    /// It conflicts with the base branch.
    Conflicts,
    /// Changes requested, by these reviewers.
    ChangesRequested(Vec<String>),
    /// The repository asks for an approval it doesn't have yet.
    ReviewRequired,
    /// These checks failed. GitHub only refuses the merge for the required
    /// ones, and gh doesn't say which those are, so all of them are named.
    ChecksFailing(Vec<String>),
    ChecksRunning(Vec<String>),
    /// The base branch moved and the repository wants the branch up to date.
    BehindBase,
    /// GitHub refuses the merge for a rule that isn't visible from here (a
    /// required check that never started, open conversations, a merge queue).
    Rule,
    /// GitHub hasn't worked out yet whether it can merge; ask again shortly.
    Computing,
}

impl Blocker {
    pub fn text(&self) -> String {
        let list = |names: &[String]| names.join(", ");
        match self {
            Blocker::Merged => "Already merged".into(),
            Blocker::Closed => "Closed without merging".into(),
            Blocker::Draft => "Still a draft".into(),
            Blocker::Conflicts => "Conflicts with the base branch".into(),
            Blocker::ChangesRequested(who) if who.is_empty() => "Changes requested".into(),
            Blocker::ChangesRequested(who) => format!("Changes requested by {}", list(who)),
            Blocker::ReviewRequired => "Needs an approving review".into(),
            Blocker::ChecksFailing(names) => format!("Checks failing: {}", list(names)),
            Blocker::ChecksRunning(names) => format!("Checks still running: {}", list(names)),
            Blocker::BehindBase => "Behind the base branch".into(),
            Blocker::Rule => "Blocked by a branch rule".into(),
            Blocker::Computing => "GitHub is still checking whether it can merge".into(),
        }
    }
}

/// The blockers as one line: "Ready to merge" when there are none.
pub fn blockers_text(blockers: &[Blocker]) -> String {
    if blockers.is_empty() {
        return "Ready to merge".into();
    }
    blockers
        .iter()
        .map(Blocker::text)
        .collect::<Vec<_>>()
        .join(" · ")
}

fn conflicting(pull: &Pull) -> bool {
    pull.mergeable == "CONFLICTING" || pull.merge_state_status == "DIRTY"
}

/// Who asked for changes and still stands by it.
fn requested_changes(pull: &Pull) -> Vec<String> {
    pull.latest_reviews
        .iter()
        .filter(|r| r.state == "CHANGES_REQUESTED")
        .map(|r| r.author.login.clone())
        .collect()
}

/// Why it can't be merged yet; empty when it is ready.
pub fn blockers(pull: &Pull) -> Vec<Blocker> {
    match pull.state.as_str() {
        "MERGED" => return vec![Blocker::Merged],
        "CLOSED" => return vec![Blocker::Closed],
        _ => {}
    }
    let checks = checks_summary(pull);
    let mut out = Vec::new();
    if pull.is_draft || pull.merge_state_status == "DRAFT" {
        out.push(Blocker::Draft);
    }
    if conflicting(pull) {
        out.push(Blocker::Conflicts);
    }
    match pull.review_decision.as_str() {
        "CHANGES_REQUESTED" => out.push(Blocker::ChangesRequested(requested_changes(pull))),
        "REVIEW_REQUIRED" => out.push(Blocker::ReviewRequired),
        _ => {}
    }
    if checks.failed > 0 {
        out.push(Blocker::ChecksFailing(checks.names(CheckState::Failed)));
    }
    if checks.running > 0 {
        out.push(Blocker::ChecksRunning(checks.names(CheckState::Running)));
    }
    if pull.merge_state_status == "BEHIND" {
        out.push(Blocker::BehindBase);
    }
    if out.is_empty() {
        match (pull.merge_state_status.as_str(), pull.mergeable.as_str()) {
            ("BLOCKED", _) => out.push(Blocker::Rule),
            ("UNKNOWN", _) | (_, "UNKNOWN") => out.push(Blocker::Computing),
            _ => {}
        }
    }
    out
}

/// Whose move it is.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WaitingOn {
    /// The user, and why, as the end of "Waiting on you: …".
    You(String),
    Author,
    /// These people or teams still owe a review. Empty when a review is
    /// required and nobody was asked.
    Reviewers(Vec<String>),
    Checks,
    /// Ready to merge, merged or closed.
    Nobody,
}

/// Threads still open where someone other than `author` spoke last. A thread
/// GitHub said nothing about (`resolved` None) counts as open: missing an
/// unanswered comment is worse than showing an answered one.
pub fn unanswered_threads(pull: &Pull, author: &str) -> usize {
    threads(&pull.inline)
        .iter()
        .filter(|t| t.resolved != Some(true))
        .filter(|t| {
            t.comments
                .last()
                .is_some_and(|c| !same_login(&c.user.login, author))
        })
        .count()
}

/// What the author has to do before anyone else can move, if anything.
fn author_work(pull: &Pull) -> Option<String> {
    let list = |names: &[String]| names.join(", ");
    if pull.is_draft {
        return Some("it is still a draft".into());
    }
    if pull.review_decision == "CHANGES_REQUESTED" {
        let who = requested_changes(pull);
        return Some(if who.is_empty() {
            "changes were requested".into()
        } else {
            format!("{} requested changes", list(&who))
        });
    }
    if conflicting(pull) {
        return Some(format!("it conflicts with {}", pull.base_ref_name));
    }
    let failed = checks_summary(pull).names(CheckState::Failed);
    if !failed.is_empty() {
        return Some(format!("checks are failing: {}", list(&failed)));
    }
    match unanswered_threads(pull, &pull.author.login) {
        0 => {}
        1 => return Some("1 review comment is unanswered".into()),
        n => return Some(format!("{n} review comments are unanswered")),
    }
    (pull.merge_state_status == "BEHIND").then(|| format!("it is behind {}", pull.base_ref_name))
}

/// Whose move it is, seen by `me` (a login). A requested review comes first;
/// then what the author has to fix; then the reviewers still asked; then the
/// checks.
pub fn waiting_on(pull: &Pull, me: &str) -> WaitingOn {
    if !pull.open() {
        return WaitingOn::Nobody;
    }
    if pull
        .review_requests
        .iter()
        .any(|r| same_login(&r.login, me))
    {
        return WaitingOn::You("your review is requested".into());
    }
    if let Some(reason) = author_work(pull) {
        return if same_login(&pull.author.login, me) {
            WaitingOn::You(reason)
        } else {
            WaitingOn::Author
        };
    }
    if !pull.review_requests.is_empty() {
        return WaitingOn::Reviewers(
            pull.review_requests
                .iter()
                .map(|r| r.who().to_string())
                .collect(),
        );
    }
    if pull.review_decision == "REVIEW_REQUIRED" {
        return WaitingOn::Reviewers(Vec::new());
    }
    if checks_summary(pull).running > 0 {
        return WaitingOn::Checks;
    }
    WaitingOn::Nobody
}

/// Where one reviewer stands.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReviewerState {
    Approved,
    ChangesRequested,
    Commented,
    /// Asked and not answered yet (again).
    Requested,
}

/// Everyone who reviewed or was asked to, each once, with where they stand:
/// a request asked again after a review wins, since it is what waits. The
/// author's own answers in threads don't make them a reviewer.
pub fn reviewers(pull: &Pull) -> Vec<(String, ReviewerState)> {
    let mut out: Vec<(String, ReviewerState)> = Vec::new();
    for r in &pull.latest_reviews {
        let state = match r.state.as_str() {
            "APPROVED" => ReviewerState::Approved,
            "CHANGES_REQUESTED" => ReviewerState::ChangesRequested,
            "COMMENTED" => ReviewerState::Commented,
            _ => continue,
        };
        if r.author.login.is_empty() || same_login(&r.author.login, &pull.author.login) {
            continue;
        }
        out.push((r.author.login.clone(), state));
    }
    for req in &pull.review_requests {
        let who = req.who();
        if who.is_empty() {
            continue;
        }
        match out.iter_mut().find(|(l, _)| same_login(l, who)) {
            Some(seen) => seen.1 = ReviewerState::Requested,
            None => out.push((who.to_string(), ReviewerState::Requested)),
        }
    }
    out
}

// ---------------------------------------------------------------------------
// The user's own review

fn reviews_by<'a, 'b>(
    pull: &'a Pull,
    me: &'b str,
) -> impl Iterator<Item = &'a Review> + use<'a, 'b> {
    // A pending review was never sent: nobody else sees it.
    pull.reviews
        .iter()
        .filter(move |r| same_login(&r.author.login, me) && r.state != "PENDING")
}

/// The last review `me` sent, whatever it said: the last time they looked.
/// Answering a thread counts, since GitHub sends each answer as a review.
pub fn my_review<'a>(pull: &'a Pull, me: &str) -> Option<&'a Review> {
    // `max_by_key` keeps the last of equals, which is the later one in GitHub's order.
    reviews_by(pull, me).max_by_key(|r| secs(&r.submitted_at))
}

/// The last review of `me` that took a side: approved, asked for changes, or
/// one of those after it was dismissed. A later plain comment doesn't replace it.
pub fn my_verdict<'a>(pull: &'a Pull, me: &str) -> Option<&'a Review> {
    reviews_by(pull, me)
        .filter(|r| {
            matches!(
                r.state.as_str(),
                "APPROVED" | "CHANGES_REQUESTED" | "DISMISSED"
            )
        })
        .max_by_key(|r| secs(&r.submitted_at))
}

/// Whether the verdict of `me` no longer counts: it was dismissed, by a person
/// or by a rule that drops approvals when new commits arrive.
pub fn my_review_dismissed(pull: &Pull, me: &str) -> bool {
    my_verdict(pull, me).is_some_and(|r| r.state == "DISMISSED")
}

/// The commits that arrived after the last review of `me`: what changed since
/// they last looked. Empty when they never reviewed.
pub fn new_since_my_review<'a>(pull: &'a Pull, me: &str) -> Vec<&'a Commit> {
    let Some(review) = my_review(pull, me) else {
        return Vec::new();
    };
    // The commit the review was written on says it exactly. After a rebase it
    // is gone from the branch, and the dates are what is left.
    if let Some(at) = pull
        .commits
        .iter()
        .position(|c| !c.oid.is_empty() && c.oid == review.commit.oid)
    {
        return pull.commits[at + 1..].iter().collect();
    }
    let seen = secs(&review.submitted_at);
    pull.commits
        .iter()
        .filter(|c| secs(&c.committed_date) > seen)
        .collect()
}

// ---------------------------------------------------------------------------
// Timeline

/// A conversation on one place of the diff: the comment that opened it, then
/// its answers, oldest first.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Thread {
    pub path: String,
    pub line: Option<u32>,
    /// The piece of the diff it was written on.
    pub diff_hunk: String,
    pub resolved: Option<bool>,
    pub outdated: Option<bool>,
    pub comments: Vec<InlineComment>,
}

impl Thread {
    /// The id to answer the thread with (`reply`).
    #[allow(dead_code)] // Not offered in the view yet.
    pub fn id(&self) -> u64 {
        self.comments.first().map_or(0, |c| c.id)
    }
}

/// The inline comments as threads, in the order they were opened.
pub fn threads(inline: &[InlineComment]) -> Vec<Thread> {
    let mut sorted: Vec<&InlineComment> = inline.iter().collect();
    sorted.sort_by_key(|c| (secs(&c.created_at), c.id));
    let mut threads: Vec<Thread> = Vec::new();
    // Comment id → index of its thread, so an answer to an answer lands in the same one.
    let mut home: HashMap<u64, usize> = HashMap::new();
    for comment in sorted {
        let at = match comment.in_reply_to_id.and_then(|id| home.get(&id).copied()) {
            Some(at) => at,
            // The first of a thread, or an answer to a comment that was deleted.
            None => {
                threads.push(Thread {
                    path: comment.path.clone(),
                    line: comment.shown_line(),
                    diff_hunk: comment.diff_hunk.clone(),
                    resolved: comment.resolved,
                    outdated: comment.outdated,
                    comments: Vec::new(),
                });
                threads.len() - 1
            }
        };
        home.insert(comment.id, at);
        threads[at].comments.push(comment.clone());
    }
    threads
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Event {
    /// Unix seconds; 0 when GitHub's date can't be read.
    pub at: u64,
    pub author: String,
    /// Markdown. For a commit, the message after its first line.
    pub body: String,
    pub kind: EventKind,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EventKind {
    /// A comment on the conversation.
    Comment {
        url: String,
    },
    /// A review: its state (`APPROVED`, `CHANGES_REQUESTED`, `COMMENTED`,
    /// `DISMISSED`) and the threads it opened on the diff.
    Review {
        state: String,
        threads: Vec<Thread>,
    },
    /// A thread on the diff whose review isn't known.
    Thread(Thread),
    Commit {
        oid: String,
        headline: String,
    },
}

fn comment_events(comments: &[Comment]) -> impl Iterator<Item = Event> + '_ {
    comments.iter().map(|c| Event {
        at: secs(&c.created_at),
        author: c.author.login.clone(),
        body: c.body.clone(),
        kind: EventKind::Comment { url: c.url.clone() },
    })
}

/// Everything that happened on the pull request, oldest first: comments,
/// reviews with the threads they opened (answers under the comment they
/// answer, whenever they were written), and commits.
pub fn timeline(pull: &Pull) -> Vec<Event> {
    let mut events: Vec<Event> = comment_events(&pull.comments).collect();
    let mut loose = threads(&pull.inline);
    for review in pull.reviews.iter().filter(|r| r.state != "PENDING") {
        let opened_here = |t: &Thread| {
            !review.id.is_empty()
                && t.comments
                    .first()
                    .is_some_and(|c| c.review.as_deref() == Some(&review.id))
        };
        let (own, rest): (Vec<Thread>, Vec<Thread>) = std::mem::take(&mut loose)
            .into_iter()
            .partition(opened_here);
        loose = rest;
        // GitHub wraps every answer to a thread in a review of its own with no
        // text. The answer already shows in its thread.
        if own.is_empty() && review.body.trim().is_empty() && review.state == "COMMENTED" {
            continue;
        }
        events.push(Event {
            at: secs(&review.submitted_at),
            author: review.author.login.clone(),
            body: review.body.clone(),
            kind: EventKind::Review {
                state: review.state.clone(),
                threads: own,
            },
        });
    }
    for thread in loose {
        let Some(first) = thread.comments.first() else {
            continue;
        };
        events.push(Event {
            at: secs(&first.created_at),
            author: first.user.login.clone(),
            body: first.body.clone(),
            kind: EventKind::Thread(thread),
        });
    }
    for commit in &pull.commits {
        events.push(Event {
            at: secs(&commit.committed_date),
            author: commit.author().to_string(),
            body: commit.message_body.clone(),
            kind: EventKind::Commit {
                oid: commit.oid.clone(),
                headline: commit.message_headline.clone(),
            },
        });
    }
    // Stable: things of the same second keep GitHub's order.
    events.sort_by_key(|e| e.at);
    events
}

/// The timeline in rows: each row a range of events. Commits one author
/// pushed one after another read as one push; everything else is a row of
/// its own.
pub fn group_pushes(events: &[Event]) -> Vec<std::ops::Range<usize>> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < events.len() {
        let mut end = i + 1;
        if matches!(events[i].kind, EventKind::Commit { .. }) {
            while events.get(end).is_some_and(|n| {
                matches!(n.kind, EventKind::Commit { .. }) && n.author == events[i].author
            }) {
                end += 1;
            }
        }
        out.push(i..end);
        i = end;
    }
    out
}

/// Whether a login is an app rather than a person: `dependabot[bot]`, or a
/// bot GitHub names without the suffix in comments.
pub fn is_bot(login: &str) -> bool {
    const KNOWN: &[&str] = &[
        "github-actions",
        "dependabot",
        "renovate",
        "codecov",
        "vercel",
        "netlify",
        "cla-assistant",
        "claassistant",
        "copilot",
        "coderabbitai",
        "sonarcloud",
        "github-advanced-security",
    ];
    let l = login.to_ascii_lowercase();
    l.ends_with("[bot]") || KNOWN.contains(&l.as_str())
}

/// The comments of an issue, oldest first.
pub fn issue_timeline(issue: &Issue) -> Vec<Event> {
    let mut events: Vec<Event> = comment_events(&issue.comments).collect();
    events.sort_by_key(|e| e.at);
    events
}

// ---------------------------------------------------------------------------
// Diff

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

// ---------------------------------------------------------------------------
// Actions

/// One call to `gh`: its arguments, and the text it reads from stdin. A body
/// always travels through stdin, never as an argument.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Cmd {
    pub args: Vec<String>,
    pub stdin: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Verdict {
    Approve,
    RequestChanges,
    Comment,
}

impl Verdict {
    /// As the `arakun github draft --verdict` value.
    pub fn name(self) -> &'static str {
        match self {
            Verdict::Approve => "approve",
            Verdict::RequestChanges => "request-changes",
            Verdict::Comment => "comment",
        }
    }

    pub fn from_name(name: &str) -> Option<Self> {
        [Verdict::Approve, Verdict::RequestChanges, Verdict::Comment]
            .into_iter()
            .find(|v| v.name() == name)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MergeMethod {
    Squash,
    Merge,
    Rebase,
}

/// What every action answers when `ARAKUN_GITHUB=readonly`.
pub const READ_ONLY: &str = "GitHub is read-only here (ARAKUN_GITHUB=readonly): nothing was sent.";

/// Whether an action failed only because this Arakun must not write to GitHub.
pub fn refused_read_only(problem: &Problem) -> bool {
    matches!(problem, Problem::Other(text) if text == READ_ONLY)
}

fn invalid(why: &str) -> Problem {
    Problem::Other(why.to_string())
}

fn strings(args: &[&str]) -> Vec<String> {
    args.iter().map(|a| a.to_string()).collect()
}

fn pulls_only(item: &ItemRef) -> Result<(), Problem> {
    match item.kind {
        ItemKind::Pull => Ok(()),
        ItemKind::Issue => Err(invalid("Only a pull request can do that.")),
    }
}

fn with_body(body: &str) -> Result<Option<String>, Problem> {
    if body.trim().is_empty() {
        Err(invalid("Write something first."))
    } else {
        Ok(Some(body.to_string()))
    }
}

fn comment_cmd(item: &ItemRef, body: &str) -> Result<Cmd, Problem> {
    let stdin = with_body(body)?;
    let number = item.number.to_string();
    Ok(Cmd {
        args: strings(&[
            item.family(),
            "comment",
            &number,
            "-R",
            &item.repo,
            "--body-file",
            "-",
        ]),
        stdin,
    })
}

/// `head` is the commit the user (or the agent) read. An approval goes to
/// GitHub's review endpoint with that commit, so a push after the last look is
/// never approved unseen; without a full SHA nothing is approved.
fn review_cmd(item: &ItemRef, verdict: Verdict, body: &str, head: &str) -> Result<Cmd, Problem> {
    pulls_only(item)?;
    let flag = match verdict {
        Verdict::Approve => return approve_cmd(item, body, head),
        Verdict::RequestChanges => "--request-changes",
        Verdict::Comment => "--comment",
    };
    let number = item.number.to_string();
    let mut args = strings(&["pr", "review", &number, "-R", &item.repo, flag]);
    let stdin = with_body(body)?;
    args.extend(strings(&["--body-file", "-"]));
    Ok(Cmd { args, stdin })
}

/// `gh pr review --approve` sends no commit, so it approves whatever the head
/// is when GitHub gets it. The REST call takes `commit_id`.
fn approve_cmd(item: &ItemRef, body: &str, head: &str) -> Result<Cmd, Problem> {
    if !is_sha(head) {
        return Err(invalid(
            "Arakun doesn't know which commit you looked at; reload the pull request, then approve.",
        ));
    }
    let path = format!("repos/{}/pulls/{}/reviews", item.repo, item.number);
    let commit = format!("commit_id={head}");
    let mut args = strings(&[
        "api",
        "-X",
        "POST",
        &path,
        "-f",
        "event=APPROVE",
        "-f",
        &commit,
    ]);
    // An approval may come with no words; when it has some, they go on stdin.
    let stdin = if body.trim().is_empty() {
        None
    } else {
        args.extend(strings(&["-F", "body=@-"]));
        Some(body.to_string())
    };
    args.extend(strings(&["--jq", ".html_url"]));
    Ok(Cmd { args, stdin })
}

#[allow(dead_code)] // Not offered in the view yet.
fn reply_cmd(item: &ItemRef, comment_id: u64, body: &str) -> Result<Cmd, Problem> {
    pulls_only(item)?;
    let stdin = with_body(body)?;
    let path = format!(
        "repos/{}/pulls/{}/comments/{comment_id}/replies",
        item.repo, item.number
    );
    // `-F body=@-` takes the field from stdin, and as text whatever it looks like.
    Ok(Cmd {
        args: strings(&[
            "api",
            "-X",
            "POST",
            &path,
            "-F",
            "body=@-",
            "--jq",
            ".html_url",
        ]),
        stdin,
    })
}

/// A full commit SHA, as GitHub gives `headRefOid`.
fn is_sha(s: &str) -> bool {
    s.len() == 40 && s.bytes().all(|b| b.is_ascii_hexdigit())
}

/// `head` is the commit the user saw: GitHub refuses the merge when the
/// branch moved since, so a late push is never merged unseen.
fn merge_cmd(item: &ItemRef, method: MergeMethod, head: &str) -> Result<Cmd, Problem> {
    pulls_only(item)?;
    if !is_sha(head) {
        return Err(invalid(
            "Arakun doesn't know the pull request's last commit yet; reload it, then merge.",
        ));
    }
    let flag = match method {
        MergeMethod::Squash => "--squash",
        MergeMethod::Merge => "--merge",
        MergeMethod::Rebase => "--rebase",
    };
    Ok(Cmd {
        args: strings(&[
            "pr",
            "merge",
            &item.number.to_string(),
            "-R",
            &item.repo,
            flag,
            "--match-head-commit",
            head,
        ]),
        stdin: None,
    })
}

/// A failed merge, said plainly when the branch moved after the user looked.
fn merge_problem(p: Problem, head: &str) -> Problem {
    match &p {
        Problem::Other(text) if text.to_lowercase().contains("head branch was modified") => {
            Problem::Other(format!(
                "New commits were pushed after {}: nothing was merged. Look at them, then merge again.",
                short_sha(head)
            ))
        }
        _ => p,
    }
}

#[allow(dead_code)] // Not offered in the view yet.
fn request_review_cmd(item: &ItemRef, logins: &[String]) -> Result<Cmd, Problem> {
    pulls_only(item)?;
    // A person (`login`) or a team (`org/team`).
    let ok = |l: &String| {
        !l.is_empty()
            && l.chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '/'))
    };
    if logins.is_empty() || !logins.iter().all(ok) {
        return Err(invalid("Name who should review it."));
    }
    let number = item.number.to_string();
    Ok(Cmd {
        args: strings(&[
            "pr",
            "edit",
            &number,
            "-R",
            &item.repo,
            "--add-reviewer",
            &logins.join(","),
        ]),
        stdin: None,
    })
}

fn state_cmd(item: &ItemRef, verb: &str) -> Cmd {
    Cmd {
        args: strings(&[
            item.family(),
            verb,
            &item.number.to_string(),
            "-R",
            &item.repo,
        ]),
        stdin: None,
    }
}

fn gh_run(cmd: &Cmd) -> Result<Vec<u8>, Problem> {
    let mut gh = git::gh(&std::env::temp_dir());
    gh.args(&cmd.args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    gh.stdin(if cmd.stdin.is_some() {
        Stdio::piped()
    } else {
        Stdio::null()
    });
    let mut child = gh.spawn().map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            Problem::NoGh
        } else {
            Problem::Other(format!("could not run gh: {e}"))
        }
    })?;
    if let (Some(text), Some(mut pipe)) = (&cmd.stdin, child.stdin.take()) {
        // gh may give up before it reads everything; what it wrote says why.
        let _ = pipe.write_all(text.as_bytes());
    }
    let out = child
        .wait_with_output()
        .map_err(|e| Problem::Other(format!("could not run gh: {e}")))?;
    if !out.status.success() {
        return Err(super::problem(&String::from_utf8_lossy(&out.stderr)));
    }
    Ok(out.stdout)
}

/// Runs an action unless this Arakun is read-only, and returns what gh printed.
/// The refusal comes before anything else, so a read-only Arakun never starts gh.
fn act_with(
    read_only: bool,
    cmd: Result<Cmd, Problem>,
    run: impl FnOnce(&Cmd) -> Result<Vec<u8>, Problem>,
) -> Result<String, Problem> {
    if read_only {
        return Err(Problem::Other(READ_ONLY.into()));
    }
    run(&cmd?).map(|out| String::from_utf8_lossy(&out).trim().to_string())
}

fn act(cmd: Result<Cmd, Problem>) -> Result<String, Problem> {
    act_with(super::read_only(), cmd, gh_run)
}

/// Comments on the conversation; returns the comment's address.
pub fn comment(item: &ItemRef, body: &str) -> Result<String, Problem> {
    act(comment_cmd(item, body))
}

/// Sends a review. An approval may have an empty body, and is bound to
/// `head`, the commit that was read; the other verdicts ignore it.
pub fn review(item: &ItemRef, verdict: Verdict, body: &str, head: &str) -> Result<(), Problem> {
    act(review_cmd(item, verdict, body, head)).map(|_| ())
}

/// Answers a thread on the diff (`Thread::id`); returns the answer's address.
#[allow(dead_code)] // Not offered in the view yet.
pub fn reply(item: &ItemRef, comment_id: u64, body: &str) -> Result<String, Problem> {
    act(reply_cmd(item, comment_id, body))
}

/// Merges only if the head is still `head`, the commit the user saw.
pub fn merge(item: &ItemRef, method: MergeMethod, head: &str) -> Result<(), Problem> {
    act(merge_cmd(item, method, head))
        .map(|_| ())
        .map_err(|p| merge_problem(p, head))
}

/// Asks these people (`login`) or teams (`org/team`) for a review.
#[allow(dead_code)] // Not offered in the view yet.
pub fn request_review(item: &ItemRef, logins: &[String]) -> Result<(), Problem> {
    act(request_review_cmd(item, logins)).map(|_| ())
}

pub fn close(item: &ItemRef) -> Result<(), Problem> {
    act(Ok(state_cmd(item, "close"))).map(|_| ())
}

#[allow(dead_code)] // Not offered in the view yet.
pub fn reopen(item: &ItemRef) -> Result<(), Problem> {
    act(Ok(state_cmd(item, "reopen"))).map(|_| ())
}

/// The merge methods in a repository's settings, as `gh api repos/{repo}` gives them.
fn merge_methods(repo: &Value) -> Vec<MergeMethod> {
    [
        ("allow_squash_merge", MergeMethod::Squash),
        ("allow_merge_commit", MergeMethod::Merge),
        ("allow_rebase_merge", MergeMethod::Rebase),
    ]
    .into_iter()
    .filter(|(key, _)| repo.get(key).and_then(Value::as_bool).unwrap_or(false))
    .map(|(_, method)| method)
    .collect()
}

/// The ways this repository lets a pull request be merged. GitHub only says
/// so to people who can push; for everyone else the list is empty.
pub fn allowed_merge_methods(repo: &str) -> Result<Vec<MergeMethod>, Problem> {
    if !valid_repo(repo) {
        return Err(invalid("Not a repository name."));
    }
    let out = super::gh_out(&["api", &format!("repos/{repo}")])?;
    Ok(merge_methods(
        &serde_json::from_slice(&out).map_err(unexpected)?,
    ))
}

// ---------------------------------------------------------------------------
// Review drafts

/// A review an agent wrote and nobody sent: the user reads it in the pull
/// request view and posts, edits or discards it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Draft {
    pub repo: String,
    pub number: u64,
    pub verdict: Verdict,
    pub body: String,
    /// Which agent wrote it.
    #[serde(default)]
    pub author: String,
    /// The head commit it was written against.
    #[serde(default)]
    pub head_sha: String,
    /// Unix seconds.
    #[serde(default)]
    pub created: u64,
}

impl Draft {
    pub fn item(&self) -> ItemRef {
        ItemRef {
            repo: self.repo.clone(),
            number: self.number,
            kind: ItemKind::Pull,
        }
    }
}

fn drafts_dir() -> PathBuf {
    // Next to the state file, so a test Arakun keeps its drafts apart.
    let base = match std::env::var_os("ARAKUN_STATE").map(PathBuf::from) {
        Some(state) => state.with_file_name("github"),
        None => crate::state::data_dir().join("github"),
    };
    base.join("drafts")
}

/// One file per pull request. GitHub's names ignore case, so the file's does too.
fn draft_file(dir: &Path, repo: &str, number: u64) -> PathBuf {
    let safe: String = repo
        .to_ascii_lowercase()
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.') {
                c
            } else {
                '_'
            }
        })
        .collect();
    // `owner/name` becomes `owner_name`; the number keeps two names from colliding on a dot.
    dir.join(format!("{safe}#{number}.json"))
}

fn save_draft_in(dir: &Path, draft: &Draft) -> std::io::Result<()> {
    if !valid_repo(&draft.repo) || draft.number == 0 {
        return Err(std::io::Error::other(format!(
            "not a pull request: {}#{}",
            draft.repo, draft.number
        )));
    }
    let bytes = serde_json::to_vec_pretty(draft).map_err(std::io::Error::other)?;
    crate::durable::write(&draft_file(dir, &draft.repo, draft.number), &bytes)
}

fn load_draft_in(dir: &Path, item: &ItemRef) -> Option<Draft> {
    let bytes = std::fs::read(draft_file(dir, &item.repo, item.number)).ok()?;
    let draft: Draft = serde_json::from_slice(&bytes).ok()?;
    // `a/b.c` and `a.b/c` can't both exist on GitHub, but a file is cheap to check.
    (draft.repo.eq_ignore_ascii_case(&item.repo) && draft.number == item.number).then_some(draft)
}

fn delete_draft_in(dir: &Path, item: &ItemRef) -> std::io::Result<()> {
    match std::fs::remove_file(draft_file(dir, &item.repo, item.number)) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e),
        _ => Ok(()),
    }
}

/// Removes the draft only if it is still `posted`. The file is taken first, so
/// checking and removing are one step: a draft saved meanwhile lands on the
/// name, not on the file taken, and one taken by mistake goes back.
fn delete_draft_if_in(dir: &Path, posted: &Draft) -> std::io::Result<()> {
    use std::io::ErrorKind;
    let path = draft_file(dir, &posted.repo, posted.number);
    let mut taken = std::ffi::OsString::from(".");
    taken.push(path.file_name().unwrap_or_default());
    taken.push(format!(".{}.posted", std::process::id()));
    let taken = dir.join(taken);
    match std::fs::rename(&path, &taken) {
        Err(e) if e.kind() == ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e),
        Ok(()) => {}
    }
    let held = std::fs::read(&taken)
        .ok()
        .and_then(|b| serde_json::from_slice::<Draft>(&b).ok());
    if held.as_ref() != Some(posted) {
        // Back where it was, unless a newer draft took the name meanwhile.
        match std::fs::hard_link(&taken, &path) {
            Ok(()) => {}
            Err(e) if e.kind() == ErrorKind::AlreadyExists => {}
            // No hard links here (FAT, some network drives): move it back.
            Err(_) if !path.exists() => return std::fs::rename(&taken, &path),
            Err(e) => return Err(e),
        }
    }
    std::fs::remove_file(&taken)
}

/// Keeps the draft, in place of any earlier one for the same pull request.
pub fn save_draft(draft: &Draft) -> std::io::Result<()> {
    save_draft_in(&drafts_dir(), draft)
}

pub fn load_draft(item: &ItemRef) -> Option<Draft> {
    load_draft_in(&drafts_dir(), item)
}

/// Removes the draft; fine when there is none.
pub fn delete_draft(item: &ItemRef) -> std::io::Result<()> {
    delete_draft_in(&drafts_dir(), item)
}

/// Removes the draft once posted, unless a new one replaced it meanwhile.
pub fn delete_draft_if(posted: &Draft) -> std::io::Result<()> {
    delete_draft_if_in(&drafts_dir(), posted)
}

/// Whether the pull request got new commits after the draft was written, so
/// the draft may talk about code that changed. A draft that doesn't know its
/// commit is not called stale.
pub fn draft_is_stale(draft: &Draft, pull: &Pull) -> bool {
    !draft.head_sha.is_empty()
        && !pull.head_ref_oid.is_empty()
        && draft.head_sha != pull.head_ref_oid
}

/// How a draft goes out. An approval counts only for the commit the agent
/// read: once the pull request has other commits, or the draft doesn't say
/// which one it read, it can go out as a comment but never as an approval.
pub fn draft_posts_as(draft: &Draft, pull: &Pull) -> Verdict {
    let current = !draft.head_sha.is_empty() && draft.head_sha == pull.head_ref_oid;
    if draft.verdict == Verdict::Approve && !current {
        Verdict::Comment
    } else {
        draft.verdict
    }
}

/// Posts a draft review as `verdict`. An approval first asks GitHub for the
/// head again: a push since the last read would otherwise be approved unseen.
pub fn post_draft(item: &ItemRef, draft: &Draft, verdict: Verdict) -> Result<(), Problem> {
    if verdict == Verdict::Approve {
        let now = head_sha(item)?;
        if now != draft.head_sha {
            return Err(invalid(&format!(
                "New commits were pushed after {}: nothing was approved. Read the draft against them, or post it as a comment.",
                short_sha(&draft.head_sha)
            )));
        }
    }
    // The check above words the usual case; the commit in the request is
    // what holds when a push lands between the two.
    review(item, verdict, &draft.body, &draft.head_sha)
}

/// The prompt that hands a pull request to an agent for review: read it,
/// check it, leave a draft, and send nothing to GitHub.
pub fn review_prompt(item: &ItemRef, title: &str) -> String {
    let (url, repo, n) = (item.url(), &item.repo, item.number);
    format!(
        "Review the pull request \"{title}\" ({label}) for me and leave your review as a draft.\nURL: {url}\n\n\
First note the commit you are reviewing: `gh pr view {url} --json headRefOid --jq .headRefOid`. \
Then read it with `gh`: the description, the full diff, the existing reviews and comments, and the checks \
(`gh pr view {url} --comments`, `gh pr diff {url}`, `gh api repos/{repo}/pulls/{n}/comments`, `gh pr checks {url}`). \
Use the URL: this checkout may be another repository.\n\n\
Verify what it claims against the code where you can: read the files around the change, and build or run the tests \
if this checkout is that repository. Say what you checked and what you could not check.\n\n\
Then save the review as a draft, its text on stdin:\n\n\
arakun github draft {label} --verdict <approve|request-changes|comment> --head <that commit> --body-file - --agent <your name> <<'EOF'\n<the review>\nEOF\n\n\
I read the draft in Arakun and post, edit or discard it myself. Never post a comment or a review, approve, \
request changes, merge or close anything on GitHub yourself.",
        label = item.label(),
    )
}

/// The end of every prompt that only reads: the agent reports to the user and
/// leaves GitHub alone.
const HANDS_OFF: &str = "Only read and report back to me. Do not post a comment or a review, approve, request changes, \
merge or close anything on GitHub.";

/// The prompt behind "Catch me up": what changed since the user last
/// reviewed. `new` are the commits that arrived after that review, oldest
/// first; `since` says when it was ("2h ago"), empty when they never reviewed.
pub fn catch_up_prompt(item: &ItemRef, title: &str, since: &str, new: &[&Commit]) -> String {
    let (url, repo, n) = (item.url(), &item.repo, item.number);
    let mut p = format!(
        "Catch me up on the pull request \"{title}\" ({label}).\nURL: {url}\n\n",
        label = item.label()
    );
    if since.is_empty() {
        p.push_str("I have not reviewed it yet. Tell me where it stands: what it changes, what reviewers said, and what is still open.\n\n");
    } else if new.is_empty() {
        p.push_str(&format!(
            "I last reviewed it {since}. No commits arrived since then; tell me what was said after my review and what is still open.\n\n"
        ));
    } else {
        p.push_str(&format!(
            "I last reviewed it {since}. These commits arrived after my review:\n"
        ));
        for c in new {
            p.push_str(&format!("- {} {}\n", c.short_oid(), c.message_headline));
        }
        p.push_str(
            "\nRead what those commits changed (`git show` needs this checkout to be that repository; otherwise \
`gh api repos/{repo}/commits/<sha>`), and the comments written since my review. Tell me whether they answer what I \
asked for, and what still needs my attention.\n\n",
        );
    }
    p.push_str(&format!(
        "Read it with `gh` (`gh pr view {url} --comments`, `gh pr diff {url}`, `gh api repos/{repo}/pulls/{n}/comments`). \
Use the URL: this checkout may be another repository.\n\n{HANDS_OFF}"
    ));
    p
}

/// The prompt behind "Summarize": the conversation so far and what is open.
pub fn summarize_prompt(item: &ItemRef, title: &str) -> String {
    let url = item.url();
    let what = match item.kind {
        ItemKind::Pull => "pull request",
        ItemKind::Issue => "issue",
    };
    let read = match item.kind {
        ItemKind::Pull => format!(
            "`gh pr view {url} --comments`, `gh api repos/{}/pulls/{}/comments` for the comments on the diff",
            item.repo, item.number
        ),
        ItemKind::Issue => format!("`gh issue view {url} --comments`"),
    };
    format!(
        "Summarize the {what} \"{title}\" ({label}) for me.\nURL: {url}\n\n\
Read the whole conversation with `gh` ({read}). Use the URL: this checkout may be another repository.\n\n\
Tell me in a few lines what it is about, what each person said that still matters, what was decided, and what is \
still open or waiting on someone (name who).\n\n{HANDS_OFF}",
        label = item.label(),
    )
}

impl DraftArgs {
    /// The commit the draft is for. An approval must say which commit the
    /// agent read: the head when the draft is saved may be a later push, and
    /// the approval would then go out for code nobody read. Other verdicts
    /// fall back to `current`, which only words the "new commits" hint.
    pub fn reviewed_head(
        &self,
        current: impl FnOnce() -> Result<String, String>,
    ) -> Result<String, String> {
        match (&self.head, self.verdict) {
            (Some(head), _) => Ok(head.clone()),
            (None, Verdict::Approve) => Err(format!(
                "an approval needs --head <sha>, the commit you read; read it before the diff with `gh pr view {} --json headRefOid --jq .headRefOid`",
                self.item.url()
            )),
            (None, _) => current(),
        }
    }

    /// The draft these arguments describe, with the review's text, the head
    /// commit it was written for, and the time now.
    pub fn draft(&self, body: String, head_sha: String, now: u64) -> Draft {
        Draft {
            repo: self.item.repo.clone(),
            number: self.item.number,
            verdict: self.verdict,
            body,
            author: self.agent.clone().unwrap_or_default(),
            head_sha,
            created: now,
        }
    }
}

/// What `arakun github draft` was asked to save.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DraftArgs {
    pub item: ItemRef,
    pub verdict: Verdict,
    /// Where the review's text is; `-` means stdin.
    pub body_file: PathBuf,
    /// `--agent <name>`: who wrote it, when the caller says.
    pub agent: Option<String>,
    /// `--head <sha>`: the commit the agent read.
    pub head: Option<String>,
}

pub const DRAFT_USAGE: &str = "usage: arakun github draft <owner/repo>#<n> --verdict <approve|request-changes|comment> --body-file <file|-> [--head <sha>] [--agent <name>]";

/// The host `gh` reads `-R owner/name` on: `GH_HOST` when set, else GitHub.
pub fn gh_host() -> String {
    std::env::var("GH_HOST")
        .ok()
        .map(|h| h.trim().to_string())
        .filter(|h| !h.is_empty())
        .unwrap_or_else(|| "github.com".into())
}

/// The arguments after `arakun github draft`. The pull request may also be
/// given as its address.
pub fn parse_draft_args(args: &[String]) -> Result<DraftArgs, String> {
    parse_draft_args_on(args, &gh_host())
}

/// `parse_draft_args` for URLs on `host`: tests name it instead of reading
/// `GH_HOST`, which other tests running at the same time may see.
fn parse_draft_args_on(args: &[String], gh: &str) -> Result<DraftArgs, String> {
    let (mut item, mut verdict, mut body_file, mut agent, mut head) =
        (None, None, None, None, None);
    let mut args = args.iter();
    while let Some(arg) = args.next() {
        let (flag, inline) = match arg.split_once('=') {
            Some((flag, value)) if flag.starts_with("--") => (flag, Some(value.to_string())),
            _ => (arg.as_str(), None),
        };
        if !flag.starts_with("--") {
            if item.is_some() {
                return Err(format!("unexpected argument `{arg}`\n{DRAFT_USAGE}"));
            }
            let host = gh.to_string();
            let found = ItemRef::parse(arg).or_else(|| ItemRef::from_url_on(arg, &host));
            let found = found.ok_or_else(|| {
                if ItemRef::from_url(arg).is_some() {
                    format!("`{arg}` is not on {host}, the host gh works with; name it as owner/repo#number there")
                } else {
                    format!("`{arg}` is not a pull request (owner/repo#number)\n{DRAFT_USAGE}")
                }
            })?;
            if found.kind != ItemKind::Pull {
                return Err(format!(
                    "`{arg}` is an issue; only a pull request takes a review"
                ));
            }
            item = Some(found);
            continue;
        }
        if !matches!(flag, "--verdict" | "--body-file" | "--agent" | "--head") {
            return Err(format!("unknown option `{flag}`\n{DRAFT_USAGE}"));
        }
        let value = inline
            .or_else(|| args.next().cloned())
            .filter(|v| !v.is_empty());
        let value = value.ok_or_else(|| format!("`{flag}` needs a value\n{DRAFT_USAGE}"))?;
        match flag {
            "--verdict" => {
                let found = Verdict::from_name(&value);
                verdict = Some(found.ok_or_else(|| {
                    format!("`{value}` is not a verdict: approve, request-changes or comment")
                })?);
            }
            "--body-file" => body_file = Some(PathBuf::from(value)),
            "--head" if is_sha(&value) => head = Some(value.to_ascii_lowercase()),
            "--head" => {
                return Err(format!(
                    "`{value}` is not a full commit SHA (`gh pr view <url> --json headRefOid`)"
                ));
            }
            _ => agent = Some(value),
        }
    }
    let missing = |what: &str| format!("missing {what}\n{DRAFT_USAGE}");
    Ok(DraftArgs {
        item: item.ok_or_else(|| missing("the pull request"))?,
        verdict: verdict.ok_or_else(|| missing("--verdict"))?,
        body_file: body_file.ok_or_else(|| missing("--body-file"))?,
        agent,
        head,
    })
}

#[cfg(test)]
mod tests {
    use super::super::{Owner, Repository, Subject};
    use super::*;

    const ME: &str = "karlo";

    #[test]
    fn short_sha_cuts_on_characters() {
        assert_eq!(short_sha("0123456789abcdef"), "0123456");
        assert_eq!(short_sha("abc"), "abc");
        assert_eq!(short_sha(""), "");
        // A damaged draft file: byte 7 falls inside a character.
        assert_eq!(short_sha("abcdeféz"), "abcdefé");
        assert_eq!(short_sha("İİİİİİİİ"), "İİİİİİİ");
        assert_eq!(short_sha("🦀🦀🦀🦀🦀🦀🦀🦀🦀"), "🦀🦀🦀🦀🦀🦀🦀");
    }

    /// A pull request as `gh pr view --json` gives it: open, two commits, one
    /// review asking for changes, checks of both kinds, and a few nulls.
    const PULL: &str = r#"{
      "number": 41, "title": "fix(terminal): keep the tab name on resize", "body": "Fixes #40.",
      "state": "OPEN", "isDraft": false,
      "author": {"id": "U_1", "is_bot": false, "login": "rik", "name": "Rik"},
      "baseRefName": "main", "headRefName": "fix/tab-name", "headRefOid": "bbbbbbb2222222",
      "mergeable": "MERGEABLE", "mergeStateStatus": "BLOCKED", "reviewDecision": "CHANGES_REQUESTED",
      "additions": 12, "deletions": 3, "changedFiles": 2,
      "createdAt": "2026-11-14T10:00:00Z", "updatedAt": "2026-11-14T12:30:00Z",
      "url": "https://github.com/ArakunHQ/arakun/pull/41",
      "labels": [{"id": "L1", "name": "bug", "description": null, "color": "d73a4a"}],
      "assignees": [], "milestone": null,
      "reviewRequests": [{"__typename": "User", "login": "ana"}, {"__typename": "Team", "name": "Core", "slug": "ArakunHQ/core"}],
      "latestReviews": [
        {"id": "", "author": {"login": "Karlo"}, "body": "One thing.", "submittedAt": "2026-11-14T11:00:00Z", "state": "CHANGES_REQUESTED", "commit": {"oid": ""}}
      ],
      "reviews": [
        {"id": "PRR_1", "author": {"login": "Karlo"}, "authorAssociation": "MEMBER", "body": "One thing.",
         "submittedAt": "2026-11-14T11:00:00Z", "state": "CHANGES_REQUESTED", "commit": {"oid": "aaaaaaa1111111"}},
        {"id": "PRR_2", "author": {"login": "rik"}, "body": "", "submittedAt": "2026-11-14T11:20:00Z", "state": "COMMENTED", "commit": {"oid": "aaaaaaa1111111"}},
        {"id": "PRR_3", "author": null, "body": "Looks fine to me.", "submittedAt": "2026-11-14T11:40:00Z", "state": "COMMENTED", "commit": null}
      ],
      "comments": [
        {"id": "IC_1", "author": {"login": "rik"}, "body": "Ready for a look.", "createdAt": "2026-11-14T10:05:00Z",
         "isMinimized": false, "url": "https://github.com/ArakunHQ/arakun/pull/41#issuecomment-1"}
      ],
      "commits": [
        {"oid": "aaaaaaa1111111", "messageHeadline": "fix(terminal): keep the tab name on resize", "messageBody": "The name was read before the resize.",
         "authoredDate": "2026-11-14T09:58:00Z", "committedDate": "2026-11-14T09:58:00Z",
         "authors": [{"login": "rik", "name": "Rik", "email": "rik@example.com"}, {"login": "", "name": "Pair", "email": "pair@example.com"}]},
        {"oid": "bbbbbbb2222222", "messageHeadline": "test the resize", "messageBody": "",
         "authoredDate": "2026-11-14T12:00:00Z", "committedDate": "2026-11-14T12:00:00Z", "authors": [{"login": null, "name": "Rik V", "email": "r@example.com"}]}
      ],
      "statusCheckRollup": [
        {"__typename": "CheckRun", "name": "Clippy", "status": "COMPLETED", "conclusion": "FAILURE", "workflowName": "CI",
         "startedAt": "2026-11-14T12:01:00Z", "completedAt": "2026-11-14T12:03:00Z", "detailsUrl": "https://github.com/ArakunHQ/arakun/actions/runs/1/job/1"},
        {"__typename": "CheckRun", "name": "Clippy", "status": "COMPLETED", "conclusion": "SUCCESS", "workflowName": "CI",
         "startedAt": "2026-11-14T12:10:00Z", "completedAt": "2026-11-14T12:12:00Z", "detailsUrl": "https://github.com/ArakunHQ/arakun/actions/runs/1/job/2"},
        {"__typename": "CheckRun", "name": "Tests", "status": "IN_PROGRESS", "conclusion": "", "workflowName": "CI",
         "startedAt": "2026-11-14T12:10:00Z", "completedAt": "0001-01-01T00:00:00Z", "detailsUrl": "https://github.com/ArakunHQ/arakun/actions/runs/1/job/3"},
        {"__typename": "CheckRun", "name": "Windows", "status": "COMPLETED", "conclusion": "SKIPPED", "workflowName": "CI",
         "startedAt": "2026-11-14T12:10:00Z", "completedAt": "2026-11-14T12:10:00Z", "detailsUrl": "https://x/4"},
        {"__typename": "CheckRun", "name": "Tests", "status": "COMPLETED", "conclusion": "TIMED_OUT", "workflowName": "Nightly",
         "startedAt": "2026-11-14T12:10:00Z", "completedAt": "2026-11-14T12:40:00Z", "detailsUrl": "https://x/5"},
        {"__typename": "StatusContext", "context": "license/cla", "state": "SUCCESS", "startedAt": "2026-11-14T12:01:00Z", "targetUrl": "https://cla.example/41"},
        {"__typename": "StatusContext", "context": "deploy/preview", "state": "PENDING", "startedAt": "2026-11-14T12:01:00Z", "targetUrl": null}
      ],
      "files": [
        {"path": "src/terminal.rs", "additions": 10, "deletions": 3, "changeType": "MODIFIED"},
        {"path": "src/terminal/tests.rs", "additions": 2, "deletions": 0, "changeType": "ADDED"}
      ]
    }"#;

    /// Its inline comments as the REST API gives them: a thread Karlo opened
    /// with his review, Rik's answer, and a later lone thread.
    const INLINE: &str = r#"[
      {"id": 101, "pull_request_review_id": 9001, "path": "src/terminal.rs", "line": 42, "original_line": 40,
       "body": "This reads the name too early.", "user": {"login": "Karlo", "id": 1}, "created_at": "2026-11-14T10:58:00Z",
       "diff_hunk": "@@ -38,6 +38,8 @@ fn resize()\n+    let name = self.name();", "html_url": "https://github.com/ArakunHQ/arakun/pull/41#discussion_r101"},
      {"id": 102, "pull_request_review_id": 9002, "path": "src/terminal.rs", "line": 42, "original_line": 40, "in_reply_to_id": 101,
       "body": "Moved it after the resize.", "user": {"login": "rik"}, "created_at": "2026-11-14T11:20:00Z", "diff_hunk": "@@ -38,6 +38,8 @@"}
    ][
      {"id": 103, "path": "src/terminal/tests.rs", "line": null, "original_line": 7, "in_reply_to_id": null,
       "body": "Name the test after what it checks.", "user": null, "created_at": "2026-11-14T11:45:00Z", "diff_hunk": "@@ -0,0 +1,9 @@"}
    ]"#;

    fn pull() -> Pull {
        let mut pull: Pull = read(PULL.as_bytes()).unwrap();
        pull.inline = read_pages(INLINE.as_bytes()).unwrap();
        // What the threads query adds.
        pull.inline[0].review = Some("PRR_1".into());
        pull.inline[1].review = Some("PRR_2".into());
        for c in &mut pull.inline[..2] {
            c.resolved = Some(false);
            c.outdated = Some(false);
        }
        pull
    }

    /// The same pull request with nothing in the way.
    fn ready() -> Pull {
        let mut p = pull();
        p.merge_state_status = "CLEAN".into();
        p.review_decision = "APPROVED".into();
        p.review_requests.clear();
        p.latest_reviews[0].state = "APPROVED".into();
        p.reviews[0].state = "APPROVED".into();
        p.status_check_rollup
            .retain(|c| c.conclusion == "SUCCESS" || c.state == "SUCCESS");
        p.inline.clear();
        p
    }

    /// A head commit, as `headRefOid` gives it.
    const SHA: &str = "0123456789abcdef0123456789abcdef01234567";

    fn item(kind: ItemKind) -> ItemRef {
        ItemRef {
            repo: "ArakunHQ/arakun".into(),
            number: 41,
            kind,
        }
    }

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

    #[test]
    fn reads_a_pull_request_with_missing_and_null_fields() {
        let p = pull();
        assert_eq!(
            (p.number, p.state.as_str(), p.is_draft, p.open(), p.merged()),
            (41, "OPEN", false, true, false)
        );
        assert_eq!(
            (
                p.author.login.as_str(),
                p.base_ref_name.as_str(),
                p.head_ref_oid.as_str()
            ),
            ("rik", "main", "bbbbbbb2222222")
        );
        assert_eq!((p.additions, p.deletions, p.changed_files), (12, 3, 2));
        assert_eq!(
            (p.labels[0].name.as_str(), p.labels[0].description.as_str()),
            ("bug", "")
        );
        assert_eq!(
            p.review_requests
                .iter()
                .map(ReviewRequest::who)
                .collect::<Vec<_>>(),
            ["ana", "ArakunHQ/core"]
        );
        // A deleted account and a review with no commit are nulls, not failures.
        assert_eq!(
            (
                p.reviews[2].author.login.as_str(),
                p.reviews[2].commit.oid.as_str()
            ),
            ("", "")
        );
        assert_eq!(p.reviews[0].commit.oid, "aaaaaaa1111111");
        assert_eq!(
            (
                p.commits[0].author(),
                p.commits[1].author(),
                p.commits[1].short_oid()
            ),
            ("rik", "Rik V", "bbbbbbb")
        );
        assert_eq!(p.files[1].change_type, "ADDED");
        assert_eq!(p.status_check_rollup[6].target_url, "");
        assert_eq!(
            p.comments[0].url,
            "https://github.com/ArakunHQ/arakun/pull/41#issuecomment-1"
        );

        // Both pages of inline comments, with their nulls.
        assert_eq!(
            p.inline.iter().map(|c| c.id).collect::<Vec<_>>(),
            [101, 102, 103]
        );
        assert_eq!(
            (p.inline[1].in_reply_to_id, p.inline[2].in_reply_to_id),
            (Some(101), None)
        );
        assert_eq!(
            (
                p.inline[0].shown_line(),
                p.inline[2].line,
                p.inline[2].shown_line()
            ),
            (Some(42), None, Some(7))
        );
        assert_eq!(p.inline[2].user.login, "");

        // An answer with almost nothing in it still reads.
        let bare: Pull = read(
            br#"{"number": 3, "author": null, "statusCheckRollup": null, "reviews": [null, {}]}"#,
        )
        .unwrap();
        assert_eq!(
            (bare.number, bare.reviews.len(), bare.title.as_str()),
            (3, 1, "")
        );
        assert!(
            matches!(read::<Pull>(b"not json"), Err(Problem::Other(e)) if e.starts_with("unexpected answer from gh"))
        );
        assert!(read_pages::<InlineComment>(b"").unwrap().is_empty());
    }

    #[test]
    fn reads_an_issue() {
        let issue: Issue = read(
            br#"{"number": 33, "title": "Depot Windows runners never start", "body": "The job stays queued.", "state": "OPEN",
            "author": {"login": "rik"}, "labels": [], "assignees": [{"login": "karlo", "name": "Karlo"}], "milestone": null,
            "createdAt": "2026-11-14T20:00:00Z", "updatedAt": "2026-11-14T21:30:00Z", "url": "https://github.com/ArakunHQ/arakun/issues/33",
            "comments": [
              {"id": "IC_2", "author": {"login": "karlo"}, "body": "Yes, back to windows-latest.", "createdAt": "2026-11-14T21:30:00Z", "url": "u2"},
              {"id": "IC_1", "author": {"login": "rik"}, "body": "Revert for now?", "createdAt": "2026-11-14T21:05:57Z", "url": "u1"}
            ]}"#,
        )
        .unwrap();
        assert_eq!(
            (
                issue.number,
                issue.state.as_str(),
                issue.assignees[0].login.as_str()
            ),
            (33, "OPEN", "karlo")
        );
        let events = issue_timeline(&issue);
        assert_eq!(
            events.iter().map(|e| e.author.as_str()).collect::<Vec<_>>(),
            ["rik", "karlo"]
        );
        assert_eq!(events[0].body, "Revert for now?");
        assert_eq!(events[0].kind, EventKind::Comment { url: "u1".into() });
        assert_eq!(
            events[0].at,
            crate::ci::parse_time("2026-11-14T21:05:57Z").unwrap()
        );
    }

    #[test]
    fn thread_states_come_from_the_graphql_answer() {
        let answer: Value = serde_json::from_str(
            r#"{"data": {"repository": {"pullRequest": {"reviewThreads": {"nodes": [
              {"isResolved": true, "isOutdated": false, "comments": {"nodes": [
                {"databaseId": 101, "pullRequestReview": {"id": "PRR_1"}}, {"databaseId": 102, "pullRequestReview": null}]}},
              {"isResolved": false, "isOutdated": true, "comments": {"nodes": [{"databaseId": 103, "pullRequestReview": {"id": "PRR_3"}}, {}]}}
            ]}}}}}"#,
        )
        .unwrap();
        let states = thread_states(&answer);
        assert_eq!(states.len(), 3);
        assert_eq!(
            (
                states[&101].resolved,
                states[&101].outdated,
                states[&101].review.as_deref()
            ),
            (true, false, Some("PRR_1"))
        );
        assert_eq!(
            (states[&102].resolved, states[&102].review.as_deref()),
            (true, None)
        );
        assert_eq!(
            (states[&103].resolved, states[&103].outdated),
            (false, true)
        );
        assert!(thread_states(&serde_json::json!({"errors": [{"message": "nope"}]})).is_empty());
    }

    #[test]
    fn a_too_large_diff_is_put_together_from_its_files() {
        let json = r#"[{"filename":"src/a.rs","status":"modified","additions":1,"deletions":1,"patch":"@@ -1,2 +1,2 @@\n fn a() {}\n-old\n+new"},
            {"filename":"img.png","status":"added","additions":0,"deletions":0},
            {"filename":"src/b.rs","previous_filename":"src/old_b.rs","status":"renamed","patch":"@@ -3 +3 @@\n-x\n+y"}]"#;
        let files: Vec<FilePatch> = read_pages(json.as_bytes()).unwrap();
        let split = split_diff(&diff_of_patches(&files));
        assert_eq!(split.len(), 3);
        assert_eq!(
            (
                split[0].path.as_str(),
                split[0].additions,
                split[0].deletions,
                split[0].hunks.len()
            ),
            ("src/a.rs", 1, 1, 1)
        );
        assert_eq!(split[0].hunks[0].lines[2].new_no, Some(2));
        assert_eq!(
            (split[1].status, split[1].hunks.len()),
            (FileStatus::Added, 0)
        );
        assert_eq!(
            (
                split[2].status,
                split[2].old_path.as_deref(),
                split[2].path.as_str()
            ),
            (FileStatus::Renamed, Some("src/old_b.rs"), "src/b.rs")
        );
        assert!(too_large(
            "could not find pull request diff: HTTP 406: Sorry, the diff exceeded the maximum number of lines (20000)"
        ));
        assert!(!too_large("HTTP 404: Not Found"));
    }

    #[test]
    fn a_long_diff_is_cut_at_a_line() {
        let whole = capped("one\ntwo\n".into(), 100);
        assert_eq!(
            (whole.text.as_str(), whole.cut, whole.bytes),
            ("one\ntwo\n", false, 8)
        );
        let cut = capped("+one\n+two\n+three\n".into(), 12);
        assert_eq!(
            (cut.text.as_str(), cut.cut, cut.bytes),
            ("+one\n+two\n", true, 17)
        );
        // The cap may fall inside a character.
        let cut = capped("+a\n+ñandú\n".into(), 5);
        assert_eq!((cut.text.as_str(), cut.cut), ("+a\n", true));
        assert_eq!(capped("no newline at all".into(), 4).text, "");
    }

    #[test]
    fn checks_are_counted_once_each() {
        let sum = checks_summary(&pull());
        let shape: Vec<(&str, &str, CheckState)> = sum
            .checks
            .iter()
            .map(|c| (c.name.as_str(), c.workflow.as_str(), c.state))
            .collect();
        assert_eq!(
            shape,
            [
                // The rerun that passed replaces the run that failed.
                ("Clippy", "CI", CheckState::Passed),
                ("Tests", "CI", CheckState::Running),
                ("Windows", "CI", CheckState::Skipped),
                // Same name, another workflow: another check.
                ("Tests", "Nightly", CheckState::Failed),
                ("license/cla", "", CheckState::Passed),
                ("deploy/preview", "", CheckState::Running),
            ]
        );
        assert_eq!(
            (
                sum.passed,
                sum.failed,
                sum.running,
                sum.skipped,
                sum.total()
            ),
            (2, 1, 2, 1, 6)
        );
        assert_eq!(
            sum.checks[0].url,
            "https://github.com/ArakunHQ/arakun/actions/runs/1/job/2"
        );
        assert_eq!(sum.checks[4].url, "https://cla.example/41");
        assert_eq!(sum.names(CheckState::Running), ["Tests", "deploy/preview"]);
        assert_eq!(sum.overall(), Some(CheckState::Failed));
        let only = |passed, running| ChecksSummary {
            passed,
            running,
            ..Default::default()
        };
        assert_eq!(only(3, 1).overall(), Some(CheckState::Running));
        assert_eq!(only(3, 0).overall(), Some(CheckState::Passed));
        assert_eq!(only(0, 0).overall(), None);

        // The later run wins wherever GitHub lists it.
        let mut p = pull();
        p.status_check_rollup.swap(0, 1);
        assert_eq!(checks_summary(&p).checks[0].state, CheckState::Passed);

        let state = |json: &str| read::<Check>(json.as_bytes()).unwrap().normalised().state;
        assert_eq!(
            state(r#"{"name": "a", "status": "QUEUED"}"#),
            CheckState::Running
        );
        assert_eq!(
            state(r#"{"name": "a", "status": "COMPLETED", "conclusion": "NEUTRAL"}"#),
            CheckState::Skipped
        );
        assert_eq!(
            state(r#"{"name": "a", "status": "COMPLETED", "conclusion": "CANCELLED"}"#),
            CheckState::Failed
        );
        assert_eq!(
            state(r#"{"name": "a", "status": "COMPLETED", "conclusion": "ACTION_REQUIRED"}"#),
            CheckState::Failed
        );
        assert_eq!(
            state(r#"{"context": "a", "state": "ERROR"}"#),
            CheckState::Failed
        );
        assert_eq!(
            state(r#"{"context": "a", "state": "EXPECTED"}"#),
            CheckState::Running
        );
        assert_eq!(checks_summary(&Pull::default()), ChecksSummary::default());
    }

    #[test]
    fn blockers_say_why_it_cannot_merge() {
        assert_eq!(
            blockers(&pull()),
            [
                Blocker::ChangesRequested(vec!["Karlo".into()]),
                Blocker::ChecksFailing(vec!["Tests".into()]),
                Blocker::ChecksRunning(vec!["Tests".into(), "deploy/preview".into()]),
            ]
        );
        assert_eq!(
            blockers_text(&blockers(&pull())),
            "Changes requested by Karlo · Checks failing: Tests · Checks still running: Tests, deploy/preview"
        );

        assert_eq!(blockers(&ready()), []);
        assert_eq!(blockers_text(&[]), "Ready to merge");

        let with = |change: fn(&mut Pull)| {
            let mut p = ready();
            change(&mut p);
            blockers(&p)
        };
        assert_eq!(with(|p| p.is_draft = true), [Blocker::Draft]);
        assert_eq!(
            with(|p| p.merge_state_status = "DRAFT".into()),
            [Blocker::Draft]
        );
        assert_eq!(
            with(|p| p.mergeable = "CONFLICTING".into()),
            [Blocker::Conflicts]
        );
        assert_eq!(
            with(|p| p.merge_state_status = "DIRTY".into()),
            [Blocker::Conflicts]
        );
        assert_eq!(
            with(|p| p.review_decision = "REVIEW_REQUIRED".into()),
            [Blocker::ReviewRequired]
        );
        assert_eq!(
            with(|p| p.merge_state_status = "BEHIND".into()),
            [Blocker::BehindBase]
        );
        // Blocked with nothing to show for it: a rule this can't see.
        assert_eq!(
            with(|p| p.merge_state_status = "BLOCKED".into()),
            [Blocker::Rule]
        );
        assert_eq!(
            with(|p| p.mergeable = "UNKNOWN".into()),
            [Blocker::Computing]
        );
        // Blocked for a reason already named isn't said twice.
        assert_eq!(
            with(|p| {
                p.merge_state_status = "BLOCKED".into();
                p.review_decision = "REVIEW_REQUIRED".into();
            }),
            [Blocker::ReviewRequired]
        );
        assert_eq!(
            with(|p| {
                p.is_draft = true;
                p.mergeable = "CONFLICTING".into();
                p.merge_state_status = "BEHIND".into();
            }),
            [Blocker::Draft, Blocker::Conflicts, Blocker::BehindBase]
        );
        // Changes requested, but the review that asked is gone from the list.
        let gone = with(|p| {
            p.review_decision = "CHANGES_REQUESTED".into();
            p.latest_reviews.clear();
        });
        assert_eq!(blockers_text(&gone), "Changes requested");

        // What was true while it was open no longer matters.
        assert_eq!(with(|p| p.state = "MERGED".into()), [Blocker::Merged]);
        let mut closed = pull();
        closed.state = "CLOSED".into();
        assert_eq!(blockers(&closed), [Blocker::Closed]);
    }

    #[test]
    fn waiting_on_names_whose_move_it_is() {
        let you = |why: &str| WaitingOn::You(why.into());
        // Karlo asked for changes: Rik has to move, and for Rik that is "you".
        assert_eq!(waiting_on(&pull(), ME), WaitingOn::Author);
        assert_eq!(waiting_on(&pull(), "RIK"), you("Karlo requested changes"));
        // A requested review comes before everything else.
        assert_eq!(waiting_on(&pull(), "Ana"), you("your review is requested"));

        let with = |me: &str, change: fn(&mut Pull)| {
            let mut p = ready();
            change(&mut p);
            waiting_on(&p, me)
        };
        assert_eq!(with(ME, |_| {}), WaitingOn::Nobody);
        assert_eq!(with("rik", |_| {}), WaitingOn::Nobody);
        assert_eq!(
            with("rik", |p| p.is_draft = true),
            you("it is still a draft")
        );
        assert_eq!(with(ME, |p| p.is_draft = true), WaitingOn::Author);
        assert_eq!(
            with("rik", |p| p.mergeable = "CONFLICTING".into()),
            you("it conflicts with main")
        );
        assert_eq!(
            with("rik", |p| p.merge_state_status = "BEHIND".into()),
            you("it is behind main")
        );
        assert_eq!(
            with("rik", |p| p.status_check_rollup =
                pull().status_check_rollup),
            you("checks are failing: Tests")
        );
        assert_eq!(
            with(ME, |p| p.status_check_rollup = pull().status_check_rollup),
            WaitingOn::Author
        );

        // Review comments nobody answered are the author's to answer.
        assert_eq!(
            with("rik", |p| p.inline = pull().inline),
            you("1 review comment is unanswered")
        );
        assert_eq!(
            with("rik", |p| {
                p.inline = pull().inline;
                p.inline.retain(|c| c.id != 102);
            }),
            you("2 review comments are unanswered")
        );
        // Answered by the author, or resolved: not waiting on them.
        assert_eq!(
            with("rik", |p| p.inline = pull().inline[..2].to_vec()),
            WaitingOn::Nobody
        );
        assert_eq!(
            with("rik", |p| {
                p.inline = pull().inline;
                p.inline[2].resolved = Some(true);
            }),
            WaitingOn::Nobody
        );

        let reviewers = |p: &mut Pull| p.review_requests = pull().review_requests;
        assert_eq!(
            with(ME, reviewers),
            WaitingOn::Reviewers(vec!["ana".into(), "ArakunHQ/core".into()])
        );
        assert_eq!(
            with("rik", reviewers),
            WaitingOn::Reviewers(vec!["ana".into(), "ArakunHQ/core".into()])
        );
        assert_eq!(
            with(ME, |p| p.review_decision = "REVIEW_REQUIRED".into()),
            WaitingOn::Reviewers(vec![])
        );
        assert_eq!(
            with(ME, |p| p.status_check_rollup[0].status =
                "IN_PROGRESS".into()),
            WaitingOn::Checks
        );
        assert_eq!(with(ME, |p| p.state = "MERGED".into()), WaitingOn::Nobody);

        let mut closed = pull();
        closed.state = "CLOSED".into();
        assert_eq!(waiting_on(&closed, "ana"), WaitingOn::Nobody);
        // An empty login is nobody, even next to a deleted author.
        let mut ghost = pull();
        ghost.author.login.clear();
        assert_eq!(waiting_on(&ghost, ""), WaitingOn::Author);
    }

    #[test]
    fn my_review_and_what_came_after_it() {
        let p = pull();
        let mine = my_review(&p, ME).unwrap();
        assert_eq!(
            (mine.id.as_str(), mine.state.as_str()),
            ("PRR_1", "CHANGES_REQUESTED")
        );
        assert_eq!(my_verdict(&p, ME).unwrap().id, "PRR_1");
        assert!(!my_review_dismissed(&p, ME));
        // Written on the first commit: the second is what is new.
        assert_eq!(
            new_since_my_review(&p, ME)
                .iter()
                .map(|c| c.short_oid())
                .collect::<Vec<_>>(),
            ["bbbbbbb"]
        );

        assert_eq!(my_review(&p, "ana"), None);
        assert!(new_since_my_review(&p, "ana").is_empty());
        assert_eq!(my_review(&p, ""), None);

        let review = |id: &str, state: &str, at: &str, oid: &str| Review {
            id: id.into(),
            author: Actor {
                login: "karlo".into(),
                ..Default::default()
            },
            submitted_at: at.into(),
            state: state.into(),
            commit: CommitRef { oid: oid.into() },
            ..Default::default()
        };

        // An answer to a thread after approving: the last look moves, the verdict stays.
        let mut p = pull();
        p.reviews = vec![
            review("A", "APPROVED", "2026-11-14T11:00:00Z", "aaaaaaa1111111"),
            review("B", "COMMENTED", "2026-11-14T12:30:00Z", "bbbbbbb2222222"),
            review("C", "PENDING", "2026-11-14T12:45:00Z", "bbbbbbb2222222"),
        ];
        assert_eq!(my_review(&p, ME).unwrap().id, "B");
        assert_eq!(my_verdict(&p, ME).unwrap().id, "A");
        assert!(new_since_my_review(&p, ME).is_empty());

        // A rule dropped the approval when the second commit arrived.
        p.reviews = vec![review(
            "A",
            "DISMISSED",
            "2026-11-14T11:00:00Z",
            "aaaaaaa1111111",
        )];
        assert!(my_review_dismissed(&p, ME));
        assert_eq!(new_since_my_review(&p, ME).len(), 1);

        // After a rebase the reviewed commit is gone: the dates decide.
        p.reviews = vec![review("A", "APPROVED", "2026-11-14T11:00:00Z", "0ld0ld0")];
        assert_eq!(
            new_since_my_review(&p, ME)
                .iter()
                .map(|c| c.short_oid())
                .collect::<Vec<_>>(),
            ["bbbbbbb"]
        );
        p.reviews = vec![review("A", "APPROVED", "2026-11-14T09:00:00Z", "")];
        assert_eq!(new_since_my_review(&p, ME).len(), 2);
    }

    #[test]
    fn pushes_group_by_author_in_a_row() {
        let ev = |author: &str, commit: bool| Event {
            at: 0,
            author: author.into(),
            body: String::new(),
            kind: if commit {
                EventKind::Commit {
                    oid: "a".into(),
                    headline: "h".into(),
                }
            } else {
                EventKind::Comment { url: String::new() }
            },
        };
        let events = [
            ev("rik", true),
            ev("rik", true),
            ev("ana", true),
            ev("rik", false),
            ev("rik", true),
            ev("rik", false),
            ev("rik", false),
        ];
        assert_eq!(group_pushes(&events), [0..2, 2..3, 3..4, 4..5, 5..6, 6..7]);
        assert!(group_pushes(&[]).is_empty());
    }

    #[test]
    fn reviewers_once_each_with_where_they_stand() {
        let p: Pull = serde_json::from_str(
            r#"{"number":1,"author":{"login":"rik"},
            "latestReviews":[{"author":{"login":"ana"},"state":"APPROVED"},
                {"author":{"login":"bo"},"state":"CHANGES_REQUESTED"},
                {"author":{"login":"rik"},"state":"COMMENTED"},
                {"author":{"login":"cy"},"state":"DISMISSED"}],
            "reviewRequests":[{"login":"Bo"},{"slug":"core"}]}"#,
        )
        .unwrap();
        assert_eq!(
            reviewers(&p),
            [
                ("ana".to_string(), ReviewerState::Approved),
                ("bo".to_string(), ReviewerState::Requested),
                ("core".to_string(), ReviewerState::Requested),
            ]
        );
    }

    #[test]
    fn bots_are_known_by_name() {
        assert!(is_bot("github-actions"));
        assert!(is_bot("renovate[bot]"));
        assert!(is_bot("Dependabot"));
        assert!(!is_bot("rikvisser-dev"));
        assert!(!is_bot(""));
    }

    #[test]
    fn the_timeline_merges_everything_in_order() {
        let events = timeline(&pull());
        let shape: Vec<(&str, String)> = events
            .iter()
            .map(|e| {
                let what = match &e.kind {
                    EventKind::Comment { .. } => "comment".to_string(),
                    EventKind::Review { state, threads } => {
                        format!("review {state} {}", threads.len())
                    }
                    EventKind::Thread(t) => format!("thread {}", t.path),
                    EventKind::Commit { headline, .. } => format!("commit {headline}"),
                };
                (e.author.as_str(), what)
            })
            .collect();
        assert_eq!(
            shape,
            [
                (
                    "rik",
                    "commit fix(terminal): keep the tab name on resize".to_string()
                ),
                ("rik", "comment".to_string()),
                ("Karlo", "review CHANGES_REQUESTED 1".to_string()),
                // Rik's empty review only carried his answer: it is under Karlo's thread.
                ("", "review COMMENTED 0".to_string()),
                ("", "thread src/terminal/tests.rs".to_string()),
                ("Rik V", "commit test the resize".to_string()),
            ]
        );
        assert!(events.windows(2).all(|w| w[0].at <= w[1].at));
        assert_eq!(events[0].body, "The name was read before the resize.");
        assert_eq!(events[2].body, "One thing.");

        let EventKind::Review { threads, .. } = &events[2].kind else {
            panic!("{:?}", events[2].kind);
        };
        let thread = &threads[0];
        assert_eq!(
            (
                thread.id(),
                thread.path.as_str(),
                thread.line,
                thread.resolved
            ),
            (101, "src/terminal.rs", Some(42), Some(false))
        );
        assert!(
            thread
                .diff_hunk
                .starts_with("@@ -38,6 +38,8 @@ fn resize()")
        );
        let said: Vec<(&str, &str)> = thread
            .comments
            .iter()
            .map(|c| (c.user.login.as_str(), c.body.as_str()))
            .collect();
        assert_eq!(
            said,
            [
                ("Karlo", "This reads the name too early."),
                ("rik", "Moved it after the resize.")
            ]
        );

        let EventKind::Thread(lone) = &events[4].kind else {
            panic!("{:?}", events[4].kind);
        };
        assert_eq!((lone.id(), lone.line, lone.resolved), (103, Some(7), None));
        assert_eq!(events[4].body, "Name the test after what it checks.");

        // Without the threads query no comment knows its review: the threads
        // stand alone, and nothing is lost.
        let mut p = pull();
        for c in &mut p.inline {
            c.review = None;
        }
        let events = timeline(&p);
        let threads: Vec<u64> = events
            .iter()
            .filter_map(|e| {
                if let EventKind::Thread(t) = &e.kind {
                    Some(t.id())
                } else {
                    None
                }
            })
            .collect();
        assert_eq!(threads, [101, 103]);
        assert_eq!(
            events
                .iter()
                .filter(|e| matches!(e.kind, EventKind::Review { .. }))
                .count(),
            2
        );

        assert!(timeline(&Pull::default()).is_empty());
    }

    #[test]
    fn answers_join_the_thread_they_answer() {
        let c = |id: u64, reply: Option<u64>, at: &str| InlineComment {
            id,
            path: format!("f{id}"),
            in_reply_to_id: reply,
            created_at: at.into(),
            ..Default::default()
        };
        // Out of order, an answer to an answer, and an answer to a deleted comment.
        let inline = [
            c(4, Some(2), "2026-11-14T10:04:00Z"),
            c(3, Some(1), "2026-11-14T10:03:00Z"),
            c(2, Some(1), "2026-11-14T10:02:00Z"),
            c(1, None, "2026-11-14T10:01:00Z"),
            c(6, Some(99), "2026-11-14T10:06:00Z"),
            c(5, None, "2026-11-14T10:05:00Z"),
        ];
        let ids: Vec<Vec<u64>> = threads(&inline)
            .iter()
            .map(|t| t.comments.iter().map(|c| c.id).collect())
            .collect();
        assert_eq!(ids, [vec![1, 2, 3, 4], vec![5], vec![6]]);
        assert_eq!(threads(&inline)[0].path, "f1");
    }

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

    #[test]
    fn actions_build_the_arguments_gh_expects() {
        let (pr, issue) = (item(ItemKind::Pull), item(ItemKind::Issue));
        let args = |cmd: Result<Cmd, Problem>| cmd.unwrap().args.join(" ");
        let body = "Looks good; `$(rm -rf ~)` \"quotes\"\nand a second line";

        let cmd = comment_cmd(&pr, body).unwrap();
        assert_eq!(
            cmd.args.join(" "),
            "pr comment 41 -R ArakunHQ/arakun --body-file -"
        );
        // The body is stdin, word for word, and in no argument.
        assert_eq!(cmd.stdin.as_deref(), Some(body));
        assert_eq!(
            args(comment_cmd(&issue, "hi")),
            "issue comment 41 -R ArakunHQ/arakun --body-file -"
        );

        let cmd = review_cmd(&pr, Verdict::RequestChanges, body, "").unwrap();
        assert_eq!(
            cmd.args.join(" "),
            "pr review 41 -R ArakunHQ/arakun --request-changes --body-file -"
        );
        assert_eq!(cmd.stdin.as_deref(), Some(body));
        assert_eq!(
            args(review_cmd(&pr, Verdict::Comment, "note", "")),
            "pr review 41 -R ArakunHQ/arakun --comment --body-file -"
        );
        // An approval names the commit it is for.
        let cmd = review_cmd(&pr, Verdict::Approve, "LGTM", SHA).unwrap();
        assert_eq!(
            cmd.args.join(" "),
            format!(
                "api -X POST repos/ArakunHQ/arakun/pulls/41/reviews -f event=APPROVE -f commit_id={SHA} -F body=@- --jq .html_url"
            )
        );
        assert_eq!(cmd.stdin.as_deref(), Some("LGTM"));
        let bare = review_cmd(&pr, Verdict::Approve, "  ", SHA).unwrap();
        assert_eq!(
            (bare.args.join(" "), bare.stdin),
            (
                format!(
                    "api -X POST repos/ArakunHQ/arakun/pulls/41/reviews -f event=APPROVE -f commit_id={SHA} --jq .html_url"
                ),
                None
            )
        );
        // Without the commit that was read, or with something that isn't one, no approval.
        assert!(review_cmd(&pr, Verdict::Approve, "LGTM", "").is_err());
        assert!(review_cmd(&pr, Verdict::Approve, "LGTM", &SHA[..7]).is_err());
        assert!(review_cmd(&pr, Verdict::Approve, "LGTM", "-f event=COMMENT").is_err());

        let cmd = reply_cmd(&pr, 101, body).unwrap();
        assert_eq!(
            cmd.args.join(" "),
            "api -X POST repos/ArakunHQ/arakun/pulls/41/comments/101/replies -F body=@- --jq .html_url"
        );
        assert_eq!(cmd.stdin.as_deref(), Some(body));

        assert_eq!(
            args(merge_cmd(&pr, MergeMethod::Squash, SHA)),
            format!("pr merge 41 -R ArakunHQ/arakun --squash --match-head-commit {SHA}")
        );
        assert_eq!(
            args(merge_cmd(&pr, MergeMethod::Merge, SHA)),
            format!("pr merge 41 -R ArakunHQ/arakun --merge --match-head-commit {SHA}")
        );
        assert_eq!(
            args(merge_cmd(&pr, MergeMethod::Rebase, SHA)),
            format!("pr merge 41 -R ArakunHQ/arakun --rebase --match-head-commit {SHA}")
        );
        // Without the commit the user saw, or with something that isn't one, no merge.
        assert!(merge_cmd(&pr, MergeMethod::Squash, "").is_err());
        assert!(merge_cmd(&pr, MergeMethod::Squash, "--admin").is_err());
        assert!(merge_cmd(&pr, MergeMethod::Squash, &SHA[..7]).is_err());
        let moved = Problem::Other(
            "GraphQL: Head branch was modified. Review and try the merge again. (mergePullRequest)"
                .into(),
        );
        assert!(
            merge_problem(moved, SHA)
                .text()
                .starts_with("New commits were pushed after 0123456")
        );
        let other = Problem::Other("Pull request is not mergeable".into());
        assert_eq!(merge_problem(other.clone(), SHA), other);

        let who = ["ana".to_string(), "ArakunHQ/core".to_string()];
        assert_eq!(
            args(request_review_cmd(&pr, &who)),
            "pr edit 41 -R ArakunHQ/arakun --add-reviewer ana,ArakunHQ/core"
        );

        assert_eq!(
            state_cmd(&pr, "close").args.join(" "),
            "pr close 41 -R ArakunHQ/arakun"
        );
        assert_eq!(
            state_cmd(&issue, "reopen").args.join(" "),
            "issue reopen 41 -R ArakunHQ/arakun"
        );
        assert_eq!(state_cmd(&issue, "close").stdin, None);

        // Refused before gh: nothing to say, nobody to ask, or not a pull request.
        assert!(comment_cmd(&pr, " \n").is_err());
        assert!(review_cmd(&pr, Verdict::RequestChanges, "", "").is_err());
        assert!(review_cmd(&pr, Verdict::Comment, "", "").is_err());
        assert!(reply_cmd(&pr, 101, "").is_err());
        assert!(request_review_cmd(&pr, &[]).is_err());
        assert!(request_review_cmd(&pr, &["--assignee x".to_string()]).is_err());
        assert!(review_cmd(&issue, Verdict::Approve, "", SHA).is_err());
        assert!(reply_cmd(&issue, 1, "x").is_err());
        assert!(merge_cmd(&issue, MergeMethod::Squash, SHA).is_err());
        assert!(request_review_cmd(&issue, &who).is_err());
    }

    #[test]
    fn a_read_only_arakun_never_starts_gh() {
        let pr = item(ItemKind::Pull);
        let never = |_: &Cmd| -> Result<Vec<u8>, Problem> { panic!("gh was started") };
        let cmds = [
            comment_cmd(&pr, "hi"),
            review_cmd(&pr, Verdict::Approve, "", SHA),
            reply_cmd(&pr, 1, "hi"),
            merge_cmd(&pr, MergeMethod::Squash, SHA),
            request_review_cmd(&pr, &["ana".to_string()]),
            Ok(state_cmd(&pr, "close")),
            Ok(state_cmd(&pr, "reopen")),
            // Even one that would be refused anyway says it is read-only.
            comment_cmd(&pr, ""),
        ];
        for cmd in cmds {
            let refused = act_with(true, cmd, never).unwrap_err();
            assert!(refused_read_only(&refused), "{refused:?}");
            assert!(refused.text().contains("read-only"), "{refused:?}");
        }

        // Otherwise gh runs with the command, and its answer comes back trimmed.
        let ran = act_with(false, comment_cmd(&pr, "hi"), |cmd| {
            assert_eq!(
                (cmd.args[1].as_str(), cmd.stdin.as_deref()),
                ("comment", Some("hi"))
            );
            Ok(b"https://github.com/ArakunHQ/arakun/pull/41#issuecomment-9\n".to_vec())
        });
        assert_eq!(
            ran.as_deref(),
            Ok("https://github.com/ArakunHQ/arakun/pull/41#issuecomment-9")
        );
        // A command that can't be built never reaches gh either.
        assert!(!refused_read_only(
            &act_with(false, comment_cmd(&pr, ""), never).unwrap_err()
        ));
        assert!(!refused_read_only(&Problem::SignedOut));
    }

    #[test]
    fn merge_methods_follow_the_repository() {
        let repo = serde_json::json!({"allow_squash_merge": true, "allow_merge_commit": false, "allow_rebase_merge": true});
        assert_eq!(
            merge_methods(&repo),
            [MergeMethod::Squash, MergeMethod::Rebase]
        );
        // Without push rights GitHub leaves the settings out.
        assert_eq!(merge_methods(&serde_json::json!({"full_name": "o/r"})), []);
        assert!(allowed_merge_methods("../user").is_err());
    }

    #[test]
    fn drafts_are_kept_one_per_pull_request() {
        let dir = std::env::temp_dir().join(format!("arakun-drafts-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let pr = item(ItemKind::Pull);
        let draft = Draft {
            repo: "ArakunHQ/arakun".into(),
            number: 41,
            verdict: Verdict::RequestChanges,
            body: "The name is read too early.\n\n- `src/terminal.rs:42`".into(),
            author: "claude".into(),
            head_sha: "bbbbbbb2222222".into(),
            created: 1_794_000_000,
        };
        assert_eq!(load_draft_in(&dir, &pr), None);
        save_draft_in(&dir, &draft).unwrap();
        assert_eq!(load_draft_in(&dir, &pr), Some(draft.clone()));
        assert_eq!(draft.item(), pr);
        // GitHub's names ignore case; another number is another draft.
        assert_eq!(
            load_draft_in(
                &dir,
                &ItemRef {
                    repo: "arakunhq/ARAKUN".into(),
                    ..pr.clone()
                }
            ),
            Some(draft.clone())
        );
        assert_eq!(
            load_draft_in(
                &dir,
                &ItemRef {
                    number: 42,
                    ..pr.clone()
                }
            ),
            None
        );

        // A second draft replaces the first.
        let again = Draft {
            verdict: Verdict::Approve,
            body: "Fixed.".into(),
            ..draft.clone()
        };
        save_draft_in(&dir, &again).unwrap();
        assert_eq!(load_draft_in(&dir, &pr), Some(again.clone()));
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 1);

        // The stored form names the verdict the way the command line does.
        let text = std::fs::read_to_string(draft_file(&dir, "ArakunHQ/arakun", 41)).unwrap();
        assert!(text.contains("\"verdict\": \"approve\""), "{text}");
        assert!(draft_file(&dir, "ArakunHQ/arakun", 41).ends_with("arakunhq_arakun#41.json"));

        // Once posted, only the draft that was posted goes.
        delete_draft_if_in(&dir, &draft).unwrap();
        assert_eq!(load_draft_in(&dir, &pr), Some(again.clone()));
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 1);
        delete_draft_if_in(&dir, &again).unwrap();
        assert_eq!(load_draft_in(&dir, &pr), None);
        delete_draft_if_in(&dir, &again).unwrap();
        save_draft_in(&dir, &again).unwrap();

        delete_draft_in(&dir, &pr).unwrap();
        assert_eq!(load_draft_in(&dir, &pr), None);
        delete_draft_in(&dir, &pr).unwrap();

        // Nothing that isn't a pull request's name becomes a file.
        assert!(
            save_draft_in(
                &dir,
                &Draft {
                    repo: "../../etc".into(),
                    ..draft.clone()
                }
            )
            .is_err()
        );
        assert!(
            save_draft_in(
                &dir,
                &Draft {
                    number: 0,
                    ..draft.clone()
                }
            )
            .is_err()
        );
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 0);
        let _ = std::fs::remove_dir_all(&dir);

        // Stale once the head moves.
        let p = pull();
        assert!(!draft_is_stale(&draft, &p));
        assert!(draft_is_stale(
            &Draft {
                head_sha: "aaaaaaa1111111".into(),
                ..draft.clone()
            },
            &p
        ));
        assert!(!draft_is_stale(
            &Draft {
                head_sha: String::new(),
                ..draft
            },
            &p
        ));
    }

    #[test]
    fn a_draft_approves_only_the_commit_it_was_written_for() {
        let p = pull();
        let draft = Draft {
            repo: "ArakunHQ/arakun".into(),
            number: 41,
            verdict: Verdict::Approve,
            body: "LGTM".into(),
            author: "Claude".into(),
            head_sha: p.head_ref_oid.clone(),
            created: 1,
        };
        assert_eq!(draft_posts_as(&draft, &p), Verdict::Approve);
        let old = Draft {
            head_sha: "aaaaaaa1111111".into(),
            ..draft.clone()
        };
        assert_eq!(draft_posts_as(&old, &p), Verdict::Comment);
        let unknown = Draft {
            head_sha: String::new(),
            ..draft.clone()
        };
        assert_eq!(draft_posts_as(&unknown, &p), Verdict::Comment);
        // The others say what they say on any commit; the view warns that it's old.
        for v in [Verdict::RequestChanges, Verdict::Comment] {
            assert_eq!(
                draft_posts_as(
                    &Draft {
                        verdict: v,
                        ..old.clone()
                    },
                    &p
                ),
                v
            );
        }
    }

    #[test]
    fn the_review_prompt_asks_for_a_draft_and_nothing_else() {
        let p = review_prompt(
            &item(ItemKind::Pull),
            "fix(terminal): keep the tab name on resize",
        );
        assert!(p.starts_with("Review the pull request \"fix(terminal): keep the tab name on resize\" (ArakunHQ/arakun#41)"), "{p}");
        assert!(
            p.contains("URL: https://github.com/ArakunHQ/arakun/pull/41\n"),
            "{p}"
        );
        assert!(
            p.contains(
                "the description, the full diff, the existing reviews and comments, and the checks"
            ),
            "{p}"
        );
        assert!(
            p.contains("`gh pr diff https://github.com/ArakunHQ/arakun/pull/41`"),
            "{p}"
        );
        assert!(
            p.contains("`gh api repos/ArakunHQ/arakun/pulls/41/comments`"),
            "{p}"
        );
        assert!(
            p.contains("Verify what it claims against the code where you can"),
            "{p}"
        );
        assert!(
            p.contains("\narakun github draft ArakunHQ/arakun#41 --verdict <approve|request-changes|comment> --head <that commit> --body-file - --agent <your name> <<'EOF'\n"),
            "{p}"
        );
        assert!(
            p.contains("First note the commit you are reviewing: `gh pr view https://github.com/ArakunHQ/arakun/pull/41 --json headRefOid --jq .headRefOid`."),
            "{p}"
        );
        assert!(p.ends_with("Never post a comment or a review, approve, request changes, merge or close anything on GitHub yourself."), "{p}");
    }

    #[test]
    fn catch_up_and_summary_prompts_only_read() {
        let p = pull();
        let it = item(ItemKind::Pull);
        let new = new_since_my_review(&p, ME);
        let text = catch_up_prompt(&it, &p.title, "2h ago", &new);
        assert!(text.starts_with("Catch me up on the pull request \"fix(terminal): keep the tab name on resize\" (ArakunHQ/arakun#41)"), "{text}");
        assert!(text.contains("I last reviewed it 2h ago. These commits arrived after my review:\n- bbbbbbb test the resize\n"), "{text}");
        assert!(
            text.contains("`gh pr diff https://github.com/ArakunHQ/arakun/pull/41`"),
            "{text}"
        );
        assert!(text.ends_with(HANDS_OFF), "{text}");
        let none = catch_up_prompt(&it, &p.title, "2h ago", &[]);
        assert!(none.contains("No commits arrived since then"), "{none}");
        let never = catch_up_prompt(&it, &p.title, "", &[]);
        assert!(never.contains("I have not reviewed it yet."), "{never}");

        let sum = summarize_prompt(&it, &p.title);
        assert!(
            sum.starts_with(
                "Summarize the pull request \"fix(terminal): keep the tab name on resize\""
            ),
            "{sum}"
        );
        assert!(sum.contains("still open or waiting on someone"), "{sum}");
        assert!(sum.ends_with(HANDS_OFF), "{sum}");
        let issue = summarize_prompt(&item(ItemKind::Issue), "Windows build stuck");
        assert!(
            issue.contains("the issue \"Windows build stuck\"")
                && issue.contains(
                    "`gh issue view https://github.com/ArakunHQ/arakun/issues/41 --comments`"
                ),
            "{issue}"
        );
        assert!(HANDS_OFF.contains(
            "Do not post a comment or a review, approve, request changes, merge or close anything"
        ));
    }

    #[test]
    fn draft_arguments_make_a_draft() {
        let args = parse_draft_args(&strings(&[
            "o/r#7",
            "--verdict",
            "approve",
            "--body-file",
            "x.md",
            "--agent",
            "Claude",
        ]))
        .unwrap();
        let d = args.draft("Looks good.".into(), "d0e6c32aaaa".into(), 42);
        assert_eq!(
            d,
            Draft {
                repo: "o/r".into(),
                number: 7,
                verdict: Verdict::Approve,
                body: "Looks good.".into(),
                author: "Claude".into(),
                head_sha: "d0e6c32aaaa".into(),
                created: 42
            }
        );
        let anonymous = parse_draft_args(&strings(&[
            "o/r#7",
            "--verdict",
            "comment",
            "--body-file",
            "-",
        ]))
        .unwrap();
        assert_eq!(anonymous.draft(String::new(), String::new(), 0).author, "");
    }

    #[test]
    fn an_approval_draft_needs_the_commit_the_agent_read() {
        let parse = |args: &[&str]| parse_draft_args(&strings(args)).unwrap();
        let later = || Ok::<_, String>("f".repeat(40));
        let approve = parse(&["o/r#7", "--verdict", "approve", "--body-file", "-"]);
        // The head at save time may be a push the agent never saw.
        let err = approve.reviewed_head(later).unwrap_err();
        assert!(err.starts_with("an approval needs --head <sha>"), "{err}");
        let pinned = parse(&[
            "o/r#7",
            "--verdict",
            "approve",
            "--body-file",
            "-",
            "--head",
            SHA,
        ]);
        assert_eq!(pinned.reviewed_head(later), Ok(SHA.to_string()));
        // A comment only uses the head for the "new commits" hint.
        let comment = parse(&["o/r#7", "--verdict", "comment", "--body-file", "-"]);
        assert_eq!(comment.reviewed_head(later), Ok("f".repeat(40)));
    }

    #[test]
    fn draft_arguments_are_parsed() {
        let parse = |args: &[&str]| parse_draft_args_on(&strings(args), "github.com");
        let pr = ItemRef {
            repo: "ArakunHQ/arakun".into(),
            number: 41,
            kind: ItemKind::Pull,
        };
        assert_eq!(
            parse(&[
                "ArakunHQ/arakun#41",
                "--verdict",
                "request-changes",
                "--body-file",
                "/tmp/review.md"
            ]),
            Ok(DraftArgs {
                item: pr.clone(),
                verdict: Verdict::RequestChanges,
                body_file: "/tmp/review.md".into(),
                agent: None,
                head: None
            })
        );
        // Any order, `--flag=value`, the address instead of the short form, stdin.
        assert_eq!(
            parse(&[
                "--body-file=-",
                "--agent",
                "codex",
                "--verdict=approve",
                &format!("--head={}", SHA.to_uppercase()),
                "https://github.com/ArakunHQ/arakun/pull/41"
            ]),
            Ok(DraftArgs {
                item: pr,
                verdict: Verdict::Approve,
                body_file: "-".into(),
                agent: Some("codex".into()),
                head: Some(SHA.into())
            })
        );
        assert_eq!(
            parse(&["o/r#1", "--verdict", "comment", "--body-file", "a=b.md"])
                .unwrap()
                .body_file,
            PathBuf::from("a=b.md")
        );

        let err = |args: &[&str]| parse(args).unwrap_err();
        assert!(err(&[]).starts_with("missing the pull request"));
        assert!(err(&["o/r#1", "--body-file", "f"]).starts_with("missing --verdict"));
        assert!(err(&["o/r#1", "--verdict", "approve"]).starts_with("missing --body-file"));
        assert!(
            err(&["o/r#1", "--verdict", "lgtm", "--body-file", "f"])
                .starts_with("`lgtm` is not a verdict")
        );
        assert!(err(&["o/r#1", "--verdict"]).starts_with("`--verdict` needs a value"));
        assert!(err(&["o/r#1", "--verdict="]).starts_with("`--verdict` needs a value"));
        assert!(err(&["o/r#1", "--post"]).starts_with("unknown option `--post`"));
        assert!(err(&["o/r#1", "--head", "d0e6c32"]).contains("not a full commit SHA"));
        assert!(err(&["o/r#1", "--head", "--agent"]).contains("not a full commit SHA"));
        assert!(err(&["o/r#1", "o/r#2"]).starts_with("unexpected argument `o/r#2`"));
        assert!(err(&["arakun 41"]).starts_with("`arakun 41` is not a pull request"));
        assert!(
            err(&[
                "https://github.com/o/r/issues/3",
                "--verdict",
                "comment",
                "--body-file",
                "f"
            ])
            .contains("is an issue")
        );
        {
            assert!(
                err(&[
                    "https://ghe.corp/o/r/pull/7",
                    "--verdict",
                    "comment",
                    "--body-file",
                    "f"
                ])
                .contains("is not on github.com")
            );
        }
        assert!(err(&[]).contains(DRAFT_USAGE));
        for v in [Verdict::Approve, Verdict::RequestChanges, Verdict::Comment] {
            assert_eq!(Verdict::from_name(v.name()), Some(v));
        }
    }

    /// Reads a real pull request and a real issue of the public repository
    /// through gh. Nothing is written.
    #[test]
    #[ignore]
    fn reads_a_real_pull_request_and_issue() {
        let fail = |p: Problem| -> ! { panic!("{}", p.text()) };
        let me = me().unwrap_or_else(|p| fail(p));
        assert_eq!(Ok(&me), super::me().as_ref());
        println!("signed in as {me}");

        let pr = ItemRef::from_url("https://github.com/ArakunHQ/arakun/pull/34").unwrap();
        let pull = fetch_pull(&pr).unwrap_or_else(|p| fail(p));
        assert_eq!(pull.number, 34);
        assert!(
            !pull.title.is_empty()
                && !pull.author.login.is_empty()
                && !pull.head_ref_oid.is_empty(),
            "{pull:?}"
        );
        assert!(!pull.commits.is_empty() && !pull.files.is_empty());
        assert!(
            pull.commits
                .iter()
                .all(|c| secs(&c.committed_date) > 0 && !c.author().is_empty())
        );
        println!(
            "#{} {} [{}] by {} ({} -> {})",
            pull.number,
            pull.title,
            pull.state,
            pull.author.login,
            pull.head_ref_name,
            pull.base_ref_name
        );
        println!(
            "+{} -{} in {} files, {} commits",
            pull.additions,
            pull.deletions,
            pull.changed_files,
            pull.commits.len()
        );
        println!(
            "mergeable={} mergeStateStatus={} reviewDecision={}",
            pull.mergeable, pull.merge_state_status, pull.review_decision
        );

        let checks = checks_summary(&pull);
        assert_eq!(
            checks.passed + checks.failed + checks.running + checks.skipped,
            checks.total()
        );
        assert!(checks.checks.iter().all(|c| !c.name.is_empty()));
        println!(
            "checks: {} passed, {} failed, {} running, {} skipped (of {} entries)",
            checks.passed,
            checks.failed,
            checks.running,
            checks.skipped,
            pull.status_check_rollup.len()
        );
        for c in &checks.checks {
            println!("  {:?} {} [{}] {}", c.state, c.name, c.workflow, c.url);
        }
        println!("blockers: {}", blockers_text(&blockers(&pull)));
        println!("waiting on: {:?}", waiting_on(&pull, &me));
        println!(
            "my review: {:?}",
            my_review(&pull, &me).map(|r| (&r.state, &r.submitted_at))
        );
        println!(
            "my verdict: {:?}, dismissed: {}",
            my_verdict(&pull, &me).map(|r| &r.state),
            my_review_dismissed(&pull, &me)
        );
        println!(
            "new since my review: {:?}",
            new_since_my_review(&pull, &me)
                .iter()
                .map(|c| c.short_oid())
                .collect::<Vec<_>>()
        );
        println!(
            "inline comments: {}, threads: {}, unanswered: {}",
            pull.inline.len(),
            threads(&pull.inline).len(),
            unanswered_threads(&pull, &pull.author.login)
        );

        let events = timeline(&pull);
        assert!(events.len() >= pull.commits.len());
        assert!(events.windows(2).all(|w| w[0].at <= w[1].at));
        assert!(events.iter().all(|e| e.at > 0));
        for e in &events {
            let what = match &e.kind {
                EventKind::Comment { .. } => "comment".to_string(),
                EventKind::Review { state, threads } => {
                    format!("review {state}, {} threads", threads.len())
                }
                EventKind::Thread(t) => format!("thread on {}", t.path),
                EventKind::Commit { oid, headline } => {
                    format!("commit {} {headline}", oid.get(..7).unwrap_or(oid))
                }
            };
            println!("  {} {} {what}", e.at, e.author);
        }

        let diff = fetch_diff(&pr).unwrap_or_else(|p| fail(p));
        let files = split_diff(&diff.text);
        println!(
            "diff: {} bytes, cut: {}, {} files",
            diff.bytes,
            diff.cut,
            files.len()
        );
        for f in &files {
            println!(
                "  {:?} {} +{} -{} ({} hunks)",
                f.status,
                f.path,
                f.additions,
                f.deletions,
                f.hunks.len()
            );
        }
        // The diff and `gh pr view` must agree on what changed.
        assert!(!diff.cut);
        assert_eq!(files.len(), pull.files.len());
        for (split, listed) in files.iter().zip(&pull.files) {
            assert_eq!(
                (split.path.as_str(), split.additions, split.deletions),
                (listed.path.as_str(), listed.additions, listed.deletions)
            );
        }
        assert_eq!(
            files.iter().map(|f| f.additions).sum::<u32>(),
            pull.additions
        );

        let methods = allowed_merge_methods(&pr.repo).unwrap_or_else(|p| fail(p));
        println!("merge methods: {methods:?}");

        // A pull request with a comment on its diff: the comment knows its
        // thread's state and sits under the review it came with.
        let with_inline =
            fetch_pull(&ItemRef::parse("ArakunHQ/arakun#29").unwrap()).unwrap_or_else(|p| fail(p));
        assert!(!with_inline.inline.is_empty());
        for c in &with_inline.inline {
            assert!(
                c.id > 0 && !c.path.is_empty() && !c.user.login.is_empty(),
                "{c:?}"
            );
            assert!(c.resolved.is_some() && c.outdated.is_some(), "{c:?}");
            assert!(
                with_inline
                    .reviews
                    .iter()
                    .any(|r| Some(&r.id) == c.review.as_ref()),
                "{c:?}"
            );
            println!(
                "inline {} {}:{:?} by {} resolved={:?} review={:?}",
                c.id,
                c.path,
                c.shown_line(),
                c.user.login,
                c.resolved,
                c.review
            );
        }
        let under_reviews: usize = timeline(&with_inline)
            .iter()
            .map(|e| {
                if let EventKind::Review { threads, .. } = &e.kind {
                    threads.len()
                } else {
                    0
                }
            })
            .sum();
        assert_eq!(under_reviews, threads(&with_inline.inline).len());
        println!(
            "#29: {}, waiting on {:?}",
            blockers_text(&blockers(&with_inline)),
            waiting_on(&with_inline, &me)
        );

        let issue_ref = ItemRef::from_url("https://github.com/ArakunHQ/arakun/issues/33").unwrap();
        let issue = fetch_issue(&issue_ref).unwrap_or_else(|p| fail(p));
        assert_eq!(issue.number, 33);
        assert!(
            !issue.title.is_empty() && !issue.author.login.is_empty() && !issue.body.is_empty()
        );
        let events = issue_timeline(&issue);
        assert_eq!(events.len(), issue.comments.len());
        println!(
            "issue #{} {} [{}] by {}: {} comments",
            issue.number,
            issue.title,
            issue.state,
            issue.author.login,
            events.len()
        );
        for e in &events {
            println!(
                "  {} {}: {}",
                e.at,
                e.author,
                e.body.lines().next().unwrap_or_default()
            );
        }
    }
}
