//! The prompts that hand a pull request or an issue to an agent.

use arakun_core::github::prompts::{self, HANDS_OFF};

use super::*;

/// The prompt that hands a pull request's failed checks to an agent.
/// `failed` are the checks with the end of their log, when it was read.
pub fn fix_checks_prompt(
    item: &ItemRef,
    title: &str,
    pull: &Pull,
    failed: &[(&CheckRun, Option<&[crate::ci::LogLine]>)],
) -> String {
    let failed: Vec<prompts::FailedCheck> = failed
        .iter()
        .map(|(c, log)| prompts::FailedCheck {
            name: c.name.clone(),
            workflow: c.workflow.clone(),
            url: c.url.clone(),
            log: log
                .unwrap_or_default()
                .iter()
                .map(|l| l.text.clone())
                .collect(),
        })
        .collect();
    prompts::fix_checks_prompt(&prompt_pull(item, title, Some(pull)), &failed)
}

/// What the prompts in core read of a pull request. Without `pull`, only
/// the prompts that name no branch can use it.
fn prompt_pull(item: &ItemRef, title: &str, pull: Option<&Pull>) -> prompts::PullRequest {
    let mut pr = prompts::PullRequest {
        repo: item.repo.clone(),
        number: item.number,
        url: item.url(),
        title: title.to_string(),
        ..Default::default()
    };
    if let Some(pull) = pull {
        pr.head = pull.head_ref_name.clone();
        pr.head_sha = pull.head_ref_oid.clone();
        pr.base = pull.base_ref_name.clone();
        pr.local_branch = pull.local_branch();
    }
    pr
}

/// The pull request an agent works on, for the shell to start it in the
/// environment on its branch.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PullTarget {
    pub repo: String,
    pub number: u64,
    /// The local branch: [`Pull::local_branch`].
    pub branch: String,
    /// The branch is in `repo` itself, so the local one can follow it.
    pub track: bool,
}

impl PullTarget {
    pub fn of(item: &ItemRef, pull: &Pull) -> Self {
        Self {
            repo: item.repo.clone(),
            number: item.number,
            branch: pull.local_branch(),
            track: !pull.is_cross_repository,
        }
    }
}

/// Whether "Resolve conflicts with agent" has something to do.
pub fn has_conflicts(pull: &Pull) -> bool {
    pull.open() && conflicting(pull)
}

/// The prompt behind "Resolve conflicts with agent": bring the base branch
/// into the pull request's branch and settle the conflicts, without pushing.
pub fn conflicts_prompt(item: &ItemRef, title: &str, pull: &Pull) -> String {
    prompts::conflicts_prompt(&prompt_pull(item, title, Some(pull)))
}

/// The open review threads an agent should address: not marked resolved
/// (a thread GitHub said nothing about counts as open, as in
/// `unanswered_threads`).
pub fn open_threads(pull: &Pull) -> Vec<Thread> {
    threads(&pull.inline)
        .into_iter()
        .filter(|t| t.resolved != Some(true))
        .collect()
}

/// The prompt behind "Address comments with agent": the open review threads,
/// each fixed in the checkout or answered with a reason. The agent neither
/// replies nor resolves on GitHub: the user does that after reading its
/// report.
pub fn comments_prompt(item: &ItemRef, title: &str, pull: &Pull, open: &[Thread]) -> String {
    let open: Vec<prompts::ReviewThread> = open
        .iter()
        .map(|t| prompts::ReviewThread {
            path: t.path.clone(),
            line: t.line,
            outdated: t.outdated == Some(true),
            url: t
                .comments
                .first()
                .map(|c| c.html_url.clone())
                .unwrap_or_default(),
            comments: t
                .comments
                .iter()
                .map(|c| prompts::ThreadComment {
                    author: c.user.login.clone(),
                    body: c.body.clone(),
                })
                .collect(),
        })
        .collect();
    prompts::comments_prompt(
        &prompt_pull(item, title, Some(pull)),
        &open,
        &requested_changes(pull),
    )
}

/// The prompt that hands a pull request to an agent for review: read it,
/// check it, leave a draft, and send nothing to GitHub.
pub fn review_prompt(item: &ItemRef, title: &str) -> String {
    prompts::review_prompt(&prompt_pull(item, title, None))
}

/// The prompt behind "Work on it": an agent does what an issue asks, in the
/// checkout, and leaves git and GitHub to the user.
pub fn issue_prompt(item: &ItemRef, title: &str) -> String {
    let (url, repo) = (item.url(), &item.repo);
    format!(
        "Work on the issue \"{title}\" ({label}).\nURL: {url}\n\n\
Read it with `gh issue view {url} --comments`. Use the URL: this checkout may be another repository. Check with \
`git remote -v`, and if it isn't {repo}, say so and stop.\n\n\
Find what it asks for in the code and make the change in this checkout. Run the tests that cover it, or say why \
you could not. Then tell me what you changed and what is left.\n\n\
Don't commit or push, and don't comment on the issue, close it or open a pull request: I do that myself.",
        label = item.label(),
    )
}

/// The prompt behind "Catch me up": what changed since the user last
/// reviewed. `new` are the commits that arrived after that review, oldest
/// first; `since` says when it was ("2h ago"), empty when they never reviewed.
pub fn catch_up_prompt(item: &ItemRef, title: &str, since: &str, new: &[&Commit]) -> String {
    let new: Vec<prompts::NewCommit> = new
        .iter()
        .map(|c| prompts::NewCommit {
            sha: c.oid.clone(),
            headline: c.message_headline.clone(),
        })
        .collect();
    prompts::catch_up_prompt(&prompt_pull(item, title, None), since, &new)
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::github::pr::fixtures::*;

    #[test]
    fn the_review_prompt_names_the_pull_request() {
        // The address follows GH_HOST, so a run on GitHub Enterprise passes too.
        let url = item(ItemKind::Pull).url();
        let p = review_prompt(
            &item(ItemKind::Pull),
            "fix(terminal): keep the tab name on resize",
        );
        assert!(p.starts_with("Review the pull request \"fix(terminal): keep the tab name on resize\" (ArakunHQ/arakun#41)"), "{p}");
        assert!(p.contains(&format!("URL: {url}\n")), "{p}");
    }

    #[test]
    fn the_issue_prompt_changes_code_and_nothing_on_github() {
        let p = issue_prompt(&item(ItemKind::Issue), "Tab name is lost");
        assert!(p.starts_with("Work on the issue \"Tab name is lost\" (ArakunHQ/arakun#41)."));
        assert!(
            p.contains("gh issue view https://github.com/ArakunHQ/arakun/issues/41 --comments")
        );
        assert!(p.contains("if it isn't ArakunHQ/arakun, say so and stop"));
        assert!(p.contains("Don't commit or push"));
        assert!(p.contains("don't comment on the issue, close it or open a pull request"));
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
            text.contains(&format!("`gh pr diff {}`", it.url())),
            "{text}"
        );
        assert!(text.ends_with(HANDS_OFF), "{text}");

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
                && issue.contains(&format!(
                    "`gh issue view {} --comments`",
                    item(ItemKind::Issue).url()
                )),
            "{issue}"
        );
    }

    #[test]
    fn conflicts_go_to_an_agent_only_when_there_are_some() {
        let mut p = pull();
        assert!(!has_conflicts(&p));
        p.mergeable = "CONFLICTING".into();
        assert!(has_conflicts(&p));
        let prompt = conflicts_prompt(&item(ItemKind::Pull), "Keep the tab name", &p);
        assert!(prompt.contains("base branch `main`"), "{prompt}");
        assert!(
            prompt.contains("merge `<remote>/main` into `fix/tab-name`"),
            "{prompt}"
        );
        p.state = "CLOSED".into();
        assert!(!has_conflicts(&p));
    }

    #[test]
    fn a_forks_branch_gets_a_local_name_of_its_own() {
        let mut p = pull();
        let it = item(ItemKind::Pull);
        let on = PullTarget::of(&it, &p);
        assert_eq!((on.branch.as_str(), on.track), ("fix/tab-name", true));
        // A fork's `main` must not land on the clone's own main.
        p.is_cross_repository = true;
        p.head_ref_name = "main".into();
        let on = PullTarget::of(&it, &p);
        assert_eq!((on.branch.as_str(), on.track), ("pr-41", false));
        let prompt = comments_prompt(&it, "Keep the tab name", &p, &open_threads(&p));
        assert!(prompt.contains("branch `main` at"), "{prompt}");
        assert!(
            prompt.contains("`git switch -c pr-41 FETCH_HEAD`"),
            "{prompt}"
        );
        // Another environment's branch stays where it is.
        assert!(
            prompt.contains("checked out in another worktree"),
            "{prompt}"
        );
    }

    #[test]
    fn open_threads_go_to_an_agent_with_their_comments() {
        let mut p = pull();
        let open = open_threads(&p);
        // The resolved-false thread and the one GitHub said nothing about.
        assert_eq!(open.len(), 2);
        let prompt = comments_prompt(&item(ItemKind::Pull), "Keep the tab name", &p, &open);
        assert!(prompt.contains("Open threads (2):"), "{prompt}");
        assert!(
            prompt.contains(
                "1. `src/terminal.rs` line 42, https://github.com/ArakunHQ/arakun/pull/41#discussion_r101\n   Karlo: This reads the name too early.\n   rik: Moved it after the resize."
            ),
            "{prompt}"
        );
        assert!(
            prompt.contains("2. `src/terminal/tests.rs` line 7\n   someone: Name the test"),
            "{prompt}"
        );
        assert!(prompt.contains("Karlo requested changes"), "{prompt}");
        assert!(prompt.contains("I do that myself."), "{prompt}");
        // The project is often on main: the agent switches to the branch
        // instead of stopping.
        assert!(
            prompt.contains("`git switch -c fix/tab-name FETCH_HEAD`"),
            "{prompt}"
        );
        assert!(
            prompt.contains("`git fetch <remote> refs/pull/41/head`"),
            "{prompt}"
        );

        // A resolved thread is not the agent's to address.
        for c in &mut p.inline[..2] {
            c.resolved = Some(true);
        }
        assert_eq!(open_threads(&p).len(), 1);
    }
}
