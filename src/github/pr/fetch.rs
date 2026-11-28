//! Reading a pull request, an issue, their review threads and the diff from GitHub.

use std::collections::HashMap;
use std::sync::OnceLock;

use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::Value;

use super::*;
use crate::github::Problem;

const PULL_FIELDS: &str = "number,title,body,state,isDraft,author,baseRefName,headRefName,headRefOid,isCrossRepository,\
mergeable,mergeStateStatus,reviewDecision,additions,deletions,changedFiles,createdAt,updatedAt,closedAt,mergedAt,url,labels,assignees,\
reviewRequests,latestReviews,reviews,comments,commits,statusCheckRollup,files";
const ISSUE_FIELDS: &str =
    "number,title,body,state,author,labels,assignees,comments,createdAt,updatedAt,closedAt,url";

/// Resolution and review of each inline comment: REST doesn't give them.
const THREADS_QUERY: &str = "query($owner:String!,$name:String!,$number:Int!,$after:String){repository(owner:$owner,name:$name){\
pullRequest(number:$number){reviewThreads(first:100,after:$after){pageInfo{hasNextPage endCursor} \
nodes{id isResolved isOutdated comments(first:100){nodes{databaseId pullRequestReview{id}}}}}}}}";

/// Pages of 100 threads read for one pull request, at most: 2,000 threads.
const THREADS_PAGES: usize = 20;

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

pub(super) fn unexpected(e: impl std::fmt::Display) -> Problem {
    Problem::Other(format!("unexpected answer from gh: {e}"))
}

pub(super) fn read<T: DeserializeOwned>(bytes: &[u8]) -> Result<T, Problem> {
    let value: Value = serde_json::from_slice(bytes).map_err(unexpected)?;
    serde_json::from_value(without_nulls(value)).map_err(unexpected)
}

/// The items of every page of a `gh api --paginate` answer. gh joins the pages
/// of a list into one, but older versions wrote one list after the other.
pub(super) fn read_pages<T: DeserializeOwned>(bytes: &[u8]) -> Result<Vec<T>, Problem> {
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
    /// The thread's node id: what resolving it needs.
    thread: Option<String>,
}

/// Comment id → the state of its thread, from the answer to `THREADS_QUERY`.
fn thread_states(answer: &Value) -> HashMap<u64, ThreadState> {
    let mut states = HashMap::new();
    let threads = answer
        .pointer("/data/repository/pullRequest/reviewThreads/nodes")
        .and_then(Value::as_array);
    for thread in threads.into_iter().flatten() {
        let flag = |key: &str| thread.get(key).and_then(Value::as_bool).unwrap_or(false);
        let node = thread.get("id").and_then(Value::as_str);
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
                    thread: node.map(str::to_string),
                },
            );
        }
    }
    states
}

/// Where the next page of threads starts, when the answer says there is one.
fn threads_next_page(answer: &Value) -> Option<String> {
    let page = answer.pointer("/data/repository/pullRequest/reviewThreads/pageInfo")?;
    page.get("hasNextPage")?
        .as_bool()?
        .then(|| page.get("endCursor")?.as_str().map(str::to_string))
        .flatten()
}

fn fetch_inline(item: &ItemRef) -> Result<Vec<InlineComment>, Problem> {
    let path = format!(
        "repos/{}/pulls/{}/comments?per_page=100",
        item.repo, item.number
    );
    read_pages(&crate::github::gh_out(&["api", &path, "--paginate"])?)
}

fn fetch_thread_states(item: &ItemRef) -> Result<HashMap<u64, ThreadState>, Problem> {
    let (owner, name) = item.repo.split_once('/').unwrap_or((&item.repo, ""));
    let mut states = HashMap::new();
    let mut after: Option<String> = None;
    // GitHub gives 100 threads a page; a thread past the first page would
    // have no state, and so no Resolve.
    for _ in 0..THREADS_PAGES {
        let mut args = strings(&[
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
        ]);
        if let Some(cursor) = &after {
            args.extend(["-f".to_string(), format!("after={cursor}")]);
        }
        let args: Vec<&str> = args.iter().map(String::as_str).collect();
        let answer: Value =
            serde_json::from_slice(&crate::github::gh_out(&args)?).map_err(unexpected)?;
        states.extend(thread_states(&answer));
        after = threads_next_page(&answer);
        if after.is_none() {
            break;
        }
    }
    if after.is_some() {
        // The threads past the limit show without Resolve; say why.
        eprintln!(
            "arakun: {}#{} has more than {} review threads; the rest were not read",
            item.repo,
            item.number,
            THREADS_PAGES * 100
        );
    }
    Ok(states)
}

/// One page of review threads, with where the next page starts.
const OPEN_THREADS_QUERY: &str = "query($owner:String!,$name:String!,$number:Int!,$after:String){repository(owner:$owner,name:$name){\
pullRequest(number:$number){reviewThreads(first:100,after:$after){pageInfo{hasNextPage endCursor} nodes{id isResolved}}}}}";

pub(super) const RESOLVE_THREAD: &str =
    "mutation($id:ID!){resolveReviewThread(input:{threadId:$id}){thread{id}}}";

/// The ids of the threads in one answer to `OPEN_THREADS_QUERY` that aren't
/// resolved, and the cursor of the next page when there is one.
fn open_thread_ids(answer: &Value) -> (Vec<String>, Option<String>) {
    let threads = answer.pointer("/data/repository/pullRequest/reviewThreads");
    let ids = threads
        .and_then(|t| t.get("nodes"))
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|t| {
            !t.get("isResolved")
                .and_then(Value::as_bool)
                .unwrap_or(false)
        })
        .filter_map(|t| t.get("id").and_then(Value::as_str).map(str::to_string))
        .collect();
    let next = threads
        .and_then(|t| t.get("pageInfo"))
        .filter(|p| p.get("hasNextPage").and_then(Value::as_bool) == Some(true))
        .and_then(|p| p.get("endCursor"))
        .and_then(Value::as_str)
        .map(str::to_string);
    (ids, next)
}

/// Resolves every review thread still open on a merged pull request at
/// `host` (`github.com`, or an Enterprise host). Returns how many it resolved;
/// the first failure stops it. Each `gh` call has [`GH_TIMEOUT`], so a hung one
/// can't hold up the next check.
pub fn resolve_open_threads(host: &str, item: &ItemRef) -> Result<usize, Problem> {
    resolve_open_threads_with(host, item, crate::github::read_only(), |args| {
        crate::github::gh_run(&std::env::temp_dir(), args, None, GH_TIMEOUT)
    })
}

const GH_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

/// The most pages of threads read: 10,000 threads. One with more fails rather
/// than claim the rest are resolved.
const THREAD_PAGES_MAX: usize = 100;

fn resolve_open_threads_with(
    host: &str,
    item: &ItemRef,
    read_only: bool,
    mut run: impl FnMut(&[&str]) -> Result<Vec<u8>, Problem>,
) -> Result<usize, Problem> {
    // A test build shares the person's real gh session: it must not change
    // threads on GitHub.
    if read_only {
        return Ok(0);
    }
    let (owner, name) = item.repo.split_once('/').unwrap_or((&item.repo, ""));
    let mut resolved = 0;
    let mut after: Option<String> = None;
    for _ in 0..THREAD_PAGES_MAX {
        let mut args = vec![
            "api".to_string(),
            "graphql".into(),
            "--hostname".into(),
            host.into(),
            "-f".into(),
            format!("query={OPEN_THREADS_QUERY}"),
            "-f".into(),
            format!("owner={owner}"),
            "-f".into(),
            format!("name={name}"),
            "-F".into(),
            format!("number={}", item.number),
        ];
        if let Some(a) = &after {
            args.extend(["-f".into(), format!("after={a}")]);
        }
        let args: Vec<&str> = args.iter().map(String::as_str).collect();
        let out = run(&args)?;
        let (page, next) = open_thread_ids(&serde_json::from_slice(&out).map_err(unexpected)?);
        // Resolved page by page: a resolved thread keeps its place in the
        // list, so the cursor still points to the next page.
        for id in &page {
            run(&[
                "api",
                "graphql",
                "--hostname",
                host,
                "-f",
                &format!("query={RESOLVE_THREAD}"),
                "-f",
                &format!("id={id}"),
            ])?;
            resolved += 1;
        }
        match next {
            Some(n) => after = Some(n),
            None => return Ok(resolved),
        }
    }
    Err(Problem::Other(format!(
        "{}#{} has more than {} review threads",
        item.repo,
        item.number,
        THREAD_PAGES_MAX * 100
    )))
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
        let pull = crate::github::gh_out(&[
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
                comment.thread = state.thread.clone();
            }
        }
    }
    Ok(pull)
}

/// The commit at the head of a pull request now: what a draft is written for.
pub fn head_sha(item: &ItemRef) -> Result<String, Problem> {
    let number = item.number.to_string();
    let out = crate::github::gh_out(&[
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
    let out = crate::github::gh_out(&[
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
    read(&crate::github::gh_out(&[
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
    match crate::github::gh_out(&["pr", "diff", &number, "-R", &item.repo]) {
        Ok(out) => Ok(capped(String::from_utf8_lossy(&out).into_owned(), DIFF_CAP)),
        Err(Problem::Other(e)) if too_large(&e) => {
            let path = format!(
                "repos/{}/pulls/{}/files?per_page=100",
                item.repo, item.number
            );
            let files: Vec<FilePatch> =
                read_pages(&crate::github::gh_out(&["api", &path, "--paginate"])?)?;
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
    let out = crate::github::gh_out(&["api", "user", "--jq", ".login"])?;
    let login = String::from_utf8_lossy(&out).trim().to_string();
    if login.is_empty() {
        return Err(Problem::Other("gh did not say who is signed in".into()));
    }
    Ok(ME.get_or_init(|| login).clone())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::github::pr::fixtures::*;

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
    fn only_open_threads_are_resolved_on_every_page() {
        let page = |nodes: &str, next: Option<&str>| {
            let info = match next {
                Some(c) => format!(r#"{{"hasNextPage": true, "endCursor": "{c}"}}"#),
                None => r#"{"hasNextPage": false, "endCursor": null}"#.to_string(),
            };
            format!(
                r#"{{"data": {{"repository": {{"pullRequest": {{"reviewThreads": {{"pageInfo": {info}, "nodes": [{nodes}]}}}}}}}}}}"#
            )
        };
        let one = page(
            r#"{"id": "T1", "isResolved": true}, {"id": "T2", "isResolved": false}"#,
            Some("c1"),
        );
        let two = page(r#"{"id": "T3"}"#, None);
        let item = ItemRef::parse("o/r#5").unwrap();
        let mut calls: Vec<Vec<String>> = Vec::new();
        let n = resolve_open_threads_with("ghe.corp", &item, false, |args| {
            calls.push(args.iter().map(|a| a.to_string()).collect());
            let after = args.contains(&"after=c1");
            Ok(match calls.len() {
                1 => one.clone().into_bytes(),
                3 if after => two.clone().into_bytes(),
                _ => b"{}".to_vec(),
            })
        })
        .unwrap();
        assert_eq!(n, 2);
        // Each page read, then its open threads resolved, all on the pull
        // request's host.
        assert_eq!(calls.len(), 4);
        assert!(calls.iter().all(|c| c[2..4] == ["--hostname", "ghe.corp"]));
        assert!(calls[1].iter().any(|a| a == "id=T2"));
        assert!(calls[3].iter().any(|a| a == "id=T3"));
        assert!(
            open_thread_ids(&serde_json::json!({"data": null}))
                .0
                .is_empty()
        );
    }

    #[test]
    fn too_many_pages_fail_instead_of_claiming_success() {
        let item = ItemRef::parse("o/r#5").unwrap();
        let endless = r#"{"data": {"repository": {"pullRequest": {"reviewThreads": {"pageInfo": {"hasNextPage": true, "endCursor": "c"}, "nodes": []}}}}}"#;
        let mut calls = 0;
        let done = resolve_open_threads_with("github.com", &item, false, |_| {
            calls += 1;
            Ok(endless.as_bytes().to_vec())
        });
        assert!(done.is_err());
        assert_eq!(calls, THREAD_PAGES_MAX);
    }

    #[test]
    fn a_test_build_resolves_nothing() {
        let item = ItemRef::parse("o/r#5").unwrap();
        let mut ran = false;
        let n = resolve_open_threads_with("github.com", &item, true, |_| {
            ran = true;
            Ok(Vec::new())
        });
        assert_eq!(n, Ok(0));
        assert!(!ran);
    }

    #[test]
    fn thread_states_come_from_the_graphql_answer() {
        let answer: Value = serde_json::from_str(
            r#"{"data": {"repository": {"pullRequest": {"reviewThreads": {"nodes": [
              {"id": "PRRT_1", "isResolved": true, "isOutdated": false, "comments": {"nodes": [
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
        // Every comment of a thread names it; a thread without an id names none.
        assert_eq!(states[&102].thread.as_deref(), Some("PRRT_1"));
        assert_eq!(states[&103].thread, None);
        assert!(thread_states(&serde_json::json!({"errors": [{"message": "nope"}]})).is_empty());
        // The 101st thread is on a second page.
        let page = |more: bool| {
            serde_json::json!({"data": {"repository": {"pullRequest": {"reviewThreads": {
                "pageInfo": {"hasNextPage": more, "endCursor": "abc"}, "nodes": []}}}}})
        };
        assert_eq!(threads_next_page(&page(true)).as_deref(), Some("abc"));
        assert_eq!(threads_next_page(&page(false)), None);
        assert_eq!(threads_next_page(&answer), None);
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
