//! One pull request or issue, read and acted on through `gh`: what the native
//! view shows (checks, what blocks the merge, who it waits on, the timeline, the
//! diff) and what it can do (comment, review, merge). Every call here blocks;
//! run them off the main thread. Reviews written by an agent are kept as drafts
//! on disk: nothing reaches GitHub until the user posts it.

mod actions;
mod checks;
mod diff;
mod drafts;
mod fetch;
mod host;
mod prompts;
mod review;
mod timeline;
mod types;

pub use actions::*;
pub use checks::*;
pub use diff::*;
pub use drafts::*;
pub use fetch::*;
pub use host::*;
pub use prompts::*;
pub use review::*;
pub use timeline::*;
pub use types::*;

/// What the tests of every part read: a pull request as `gh` gives it.
#[cfg(test)]
mod fixtures {
    use super::*;

    pub(super) const ME: &str = "karlo";

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

    pub(super) fn pull() -> Pull {
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
    pub(super) fn ready() -> Pull {
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
    pub(super) const SHA: &str = "0123456789abcdef0123456789abcdef01234567";

    pub(super) fn item(kind: ItemKind) -> ItemRef {
        ItemRef {
            repo: "ArakunHQ/arakun".into(),
            number: 41,
            kind,
        }
    }
}
