//! What the view can do on GitHub: comment, review, answer threads, merge, close.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::*;
use crate::github::Problem;

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

pub(super) fn invalid(why: &str) -> Problem {
    Problem::Other(why.to_string())
}

pub(super) fn strings(args: &[&str]) -> Vec<String> {
    args.iter().map(|a| a.to_string()).collect()
}

pub(super) fn pulls_only(item: &ItemRef) -> Result<(), Problem> {
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
    // An approval may come with no words.
    let stdin = (!body.trim().is_empty()).then(|| body.to_string());
    let mut args = review_api_args(item, "APPROVE", head, stdin.is_some());
    args.extend(strings(&["--jq", ".html_url"]));
    Ok(Cmd { args, stdin })
}

/// `gh api` posting a review on commit `sha`: the REST endpoint is the one
/// that takes a commit. `-F body=@-` takes the words from stdin, and as text
/// whatever they look like.
fn review_api_args(item: &ItemRef, event: &str, sha: &str, words: bool) -> Vec<String> {
    let path = format!("repos/{}/pulls/{}/reviews", item.repo, item.number);
    let mut args = strings(&[
        "api",
        "-X",
        "POST",
        &path,
        "-f",
        &format!("event={event}"),
        "-f",
        &format!("commit_id={sha}"),
    ]);
    if words {
        args.extend(strings(&["-F", "body=@-"]));
    }
    args
}

/// A review tied to one commit, through the REST API (`gh pr review` can't
/// name a commit): what was read is what gets approved, even if a push lands
/// while the review is sent.
fn review_at_cmd(item: &ItemRef, verdict: Verdict, body: &str, sha: &str) -> Result<Cmd, Problem> {
    pulls_only(item)?;
    if !valid_repo(&item.repo) {
        return Err(invalid("Not a repository name."));
    }
    if !(7..=64).contains(&sha.len()) || !sha.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err(invalid("Not a commit."));
    }
    let event = match verdict {
        Verdict::Approve => "APPROVE",
        Verdict::RequestChanges => "REQUEST_CHANGES",
        Verdict::Comment => "COMMENT",
    };
    let stdin = if verdict == Verdict::Approve && body.trim().is_empty() {
        None
    } else {
        with_body(body)?
    };
    let args = review_api_args(item, event, sha, stdin.is_some());
    Ok(Cmd { args, stdin })
}

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

const UNRESOLVE_THREAD: &str =
    "mutation($id:ID!){unresolveReviewThread(input:{threadId:$id}){thread{id}}}";

/// Marks the thread `node` (`Thread::node`) resolved, or open again.
fn thread_cmd(node: &str, resolve: bool) -> Result<Cmd, Problem> {
    // A node id is base64 and `_`: nothing else goes into the call.
    let ok = !node.is_empty()
        && node
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '=' | '+' | '/'));
    if !ok {
        return Err(invalid(
            "GitHub didn't say which thread this is; reload the pull request.",
        ));
    }
    let query = if resolve {
        RESOLVE_THREAD
    } else {
        UNRESOLVE_THREAD
    };
    Ok(Cmd {
        args: strings(&[
            "api",
            "graphql",
            "-f",
            &format!("query={query}"),
            "-f",
            &format!("id={node}"),
        ]),
        stdin: None,
    })
}

/// A comment on one line of the diff, not sent yet.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LineComment {
    pub path: String,
    pub line: u32,
    /// On a line the change removed (GitHub's `LEFT` side).
    pub left: bool,
    pub body: String,
}

/// One review of commit `sha` that carries `comments`, each on its line.
/// The body is JSON on stdin: the comments are a list, which `-f` can't say.
fn line_review_cmd(item: &ItemRef, sha: &str, comments: &[LineComment]) -> Result<Cmd, Problem> {
    pulls_only(item)?;
    if !valid_repo(&item.repo) {
        return Err(invalid("Not a repository name."));
    }
    if !is_sha(sha) {
        return Err(invalid(
            "Arakun doesn't know which commit these lines are of; reload the diff.",
        ));
    }
    if comments.is_empty() || comments.iter().any(|c| c.body.trim().is_empty()) {
        return Err(invalid("Write something first."));
    }
    let list: Vec<Value> = comments
        .iter()
        .map(|c| {
            serde_json::json!({
                "path": c.path,
                "line": c.line,
                "side": if c.left { "LEFT" } else { "RIGHT" },
                "body": c.body,
            })
        })
        .collect();
    let body = serde_json::json!({"event": "COMMENT", "commit_id": sha, "comments": list});
    let path = format!("repos/{}/pulls/{}/reviews", item.repo, item.number);
    Ok(Cmd {
        args: strings(&[
            "api",
            "-X",
            "POST",
            &path,
            "--input",
            "-",
            "--jq",
            ".html_url",
        ]),
        stdin: Some(body.to_string()),
    })
}

/// A full commit SHA, as GitHub gives `headRefOid`.
pub(super) fn is_sha(s: &str) -> bool {
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

/// Runs an action unless this Arakun is read-only, and returns what gh printed.
/// The refusal comes before anything else, so a read-only Arakun never starts gh.
pub(super) fn act_with(
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
    act_with(crate::github::read_only(), cmd, |cmd| {
        gh_run(cmd, crate::platform::GIT_REMOTE)
    })
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

/// Sends a review of commit `sha`. An approval may have an empty body.
pub fn review_at(item: &ItemRef, verdict: Verdict, body: &str, sha: &str) -> Result<(), Problem> {
    act(review_at_cmd(item, verdict, body, sha)).map(|_| ())
}

/// Answers a thread on the diff (`Thread::id`); returns the answer's address.
pub fn reply(item: &ItemRef, comment_id: u64, body: &str) -> Result<String, Problem> {
    act(reply_cmd(item, comment_id, body))
}

/// Resolves a thread on the diff, or opens it again.
pub fn set_thread_resolved(node: &str, resolve: bool) -> Result<(), Problem> {
    act(thread_cmd(node, resolve)).map(|_| ())
}

/// Sends the comments written on lines of commit `sha` as one review.
pub fn review_lines(item: &ItemRef, sha: &str, comments: &[LineComment]) -> Result<(), Problem> {
    act(line_review_cmd(item, sha, comments)).map(|_| ())
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
    let out = crate::github::gh_out(&["api", &format!("repos/{repo}")])?;
    Ok(merge_methods(
        &serde_json::from_slice(&out).map_err(unexpected)?,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::github::pr::fixtures::*;

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

        let sha = "40a1b2c3d4e5f60718293a4b5c6d7e8f90a1b2c3";
        let cmd = review_at_cmd(&pr, Verdict::RequestChanges, body, sha).unwrap();
        assert_eq!(
            cmd.args.join(" "),
            format!(
                "api -X POST repos/ArakunHQ/arakun/pulls/41/reviews -f event=REQUEST_CHANGES -f commit_id={sha} -F body=@-"
            )
        );
        assert_eq!(cmd.stdin.as_deref(), Some(body));
        let bare = review_at_cmd(&pr, Verdict::Approve, "", sha).unwrap();
        assert_eq!(
            (bare.args.join(" "), bare.stdin),
            (
                format!(
                    "api -X POST repos/ArakunHQ/arakun/pulls/41/reviews -f event=APPROVE -f commit_id={sha}"
                ),
                None
            )
        );
        // Only a commit goes into the field, and only a pull request is reviewed.
        assert!(review_at_cmd(&pr, Verdict::Approve, "", "main; rm").is_err());
        assert!(review_at_cmd(&pr, Verdict::Approve, "", "").is_err());
        assert!(review_at_cmd(&issue, Verdict::Approve, "", sha).is_err());
        assert!(review_at_cmd(&pr, Verdict::RequestChanges, " ", sha).is_err());

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
    fn threads_resolve_and_lines_take_one_review() {
        let pr = item(ItemKind::Pull);
        let cmd = thread_cmd("PRRT_kwDOabc-_1", true).unwrap();
        assert_eq!(cmd.args[..3], ["api", "graphql", "-f"]);
        assert!(cmd.args[3].contains("resolveReviewThread"), "{cmd:?}");
        assert_eq!(cmd.args[5], "id=PRRT_kwDOabc-_1");
        let open = thread_cmd("PRRT_1", false).unwrap();
        assert!(open.args[3].contains("unresolveReviewThread"), "{open:?}");
        // No id (GitHub didn't say), or one that isn't an id: nothing is sent.
        assert!(thread_cmd("", true).is_err());
        assert!(thread_cmd("PRRT_1 -f query=x", true).is_err());

        let line = |path: &str, line, left, body: &str| LineComment {
            path: path.into(),
            line,
            left,
            body: body.into(),
        };
        let body = "Why `$(rm -rf ~)`?\n\"quoted\"";
        let comments = [
            line("src/a.rs", 12, false, body),
            line("src/b.rs", 3, true, "gone"),
        ];
        let cmd = line_review_cmd(&pr, SHA, &comments).unwrap();
        assert_eq!(
            cmd.args.join(" "),
            "api -X POST repos/ArakunHQ/arakun/pulls/41/reviews --input - --jq .html_url"
        );
        // The words travel as JSON on stdin, whatever they look like.
        let sent: Value = serde_json::from_str(cmd.stdin.as_deref().unwrap()).unwrap();
        assert_eq!(
            sent,
            serde_json::json!({"event": "COMMENT", "commit_id": SHA, "comments": [
                {"path": "src/a.rs", "line": 12, "side": "RIGHT", "body": body},
                {"path": "src/b.rs", "line": 3, "side": "LEFT", "body": "gone"},
            ]})
        );
        assert!(line_review_cmd(&pr, SHA, &[]).is_err());
        assert!(line_review_cmd(&pr, "abc", &comments).is_err());
        assert!(line_review_cmd(&pr, SHA, &[line("a", 1, false, " ")]).is_err());
        assert!(line_review_cmd(&item(ItemKind::Issue), SHA, &comments).is_err());
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
            rerun_cmd(&pr, 7),
            thread_cmd("PRRT_1", true),
            line_review_cmd(
                &pr,
                SHA,
                &[LineComment {
                    path: "a.rs".into(),
                    line: 1,
                    left: false,
                    body: "hi".into(),
                }],
            ),
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
}
