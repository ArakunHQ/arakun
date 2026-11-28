//! The text an agent session gets for work on a pull request: review it,
//! catch up on it, address its comments, fix its checks, resolve its
//! conflicts. The GitHub tab builds these, and so can a session delegated
//! on `arakun server`, which has only this crate.
//!
//! The inputs are small on purpose: only what the prompts read, so core does
//! not take on the app's whole GitHub model.

use super::short_sha;

/// The pull request a prompt is about.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PullRequest {
    /// `owner/name`.
    pub repo: String,
    pub number: u64,
    /// Its web address on the host `gh` talks to.
    pub url: String,
    pub title: String,
    /// The branch, as GitHub names it (`headRefName`).
    pub head: String,
    /// The head commit (`headRefOid`).
    pub head_sha: String,
    /// The branch it merges into (`baseRefName`).
    pub base: String,
    /// The branch an agent works on locally: a fork's branch gets a name of
    /// its own, as it is often `main`.
    pub local_branch: String,
}

impl PullRequest {
    /// `owner/repo#12`.
    pub fn label(&self) -> String {
        format!("{}#{}", self.repo, self.number)
    }
}

/// A check that failed, with the end of its failing step's log when it was
/// read.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FailedCheck {
    pub name: String,
    /// The workflow it belongs to; empty for a commit status.
    pub workflow: String,
    pub url: String,
    pub log: Vec<String>,
}

/// A commit that arrived after the user's last review.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct NewCommit {
    pub sha: String,
    pub headline: String,
}

/// An open review thread on a line of the diff.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ReviewThread {
    pub path: String,
    pub line: Option<u32>,
    /// The code under it changed since it was written.
    pub outdated: bool,
    /// The address of its first comment; empty when GitHub gave none.
    pub url: String,
    pub comments: Vec<ThreadComment>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ThreadComment {
    /// The login; empty for a deleted account.
    pub author: String,
    pub body: String,
}

/// The end of every prompt that only reads: the agent reports to the user and
/// leaves GitHub alone.
pub const HANDS_OFF: &str = "Only read and report back to me. Do not post a comment or a review, approve, request changes, \
merge or close anything on GitHub.";

/// How much of one comment goes in the prompt. Bots write pages; the agent
/// reads the rest with `gh`.
pub const COMMENT_IN_PROMPT: usize = 1500;
/// How much of all the threads goes in the prompt.
pub const THREADS_IN_PROMPT: usize = 30_000;

/// The prompt that hands a pull request to an agent for review: read it,
/// check it, leave a draft, and send nothing to GitHub.
pub fn review_prompt(pr: &PullRequest) -> String {
    let (url, repo, n, title) = (&pr.url, &pr.repo, pr.number, &pr.title);
    format!(
        "Review the pull request \"{title}\" ({label}) for me and leave your review as a draft.\nURL: {url}\n\n\
First note the commit you are reviewing: `gh pr view {url} --json headRefOid --jq .headRefOid`. \
Then read it with `gh`: the description, the full diff, the existing reviews and comments, and the checks \
(`gh pr view {url} --comments`, `gh pr diff {url}`, `gh api repos/{repo}/pulls/{n}/comments`, `gh pr checks {url}`). \
Use the URL: this checkout may be another repository. Don't switch this checkout to another branch \
(no `git switch` or `gh pr checkout`): other sessions may be working in it.\n\n\
Verify what it claims against the code where you can: read the files around the change, and build or run the tests \
if this checkout is that repository. Say what you checked and what you could not check.\n\n\
Then save the review as a draft, its text on stdin:\n\n\
arakun github draft {label} --verdict <approve|request-changes|comment> --head <that commit> --body-file - --agent <your name> <<'EOF'\n<the review>\nEOF\n\n\
I read the draft in Arakun and post, edit or discard it myself. Never post a comment or a review, approve, \
request changes, merge or close anything on GitHub yourself.",
        label = pr.label(),
    )
}

/// The prompt behind "Catch me up": what changed since the user last
/// reviewed. `new` are the commits that arrived after that review, oldest
/// first; `since` says when it was ("2h ago"), empty when they never reviewed.
pub fn catch_up_prompt(pr: &PullRequest, since: &str, new: &[NewCommit]) -> String {
    let (url, repo, n, title) = (&pr.url, &pr.repo, pr.number, &pr.title);
    let mut p = format!(
        "Catch me up on the pull request \"{title}\" ({label}).\nURL: {url}\n\n",
        label = pr.label()
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
            p.push_str(&format!("- {} {}\n", short_sha(&c.sha), c.headline));
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

/// The prompt that hands a pull request's failed checks to an agent.
pub fn fix_checks_prompt(pr: &PullRequest, failed: &[FailedCheck]) -> String {
    let (url, title) = (&pr.url, &pr.title);
    let mut p = format!(
        "The checks of the pull request \"{title}\" ({label}) failed on branch `{branch}` at {sha}.\nURL: {url}\n\nFailed checks:\n",
        label = pr.label(),
        branch = pr.head,
        sha = short_sha(&pr.head_sha),
    );
    for c in failed {
        let name = if c.workflow.is_empty() {
            c.name.clone()
        } else {
            format!("{} ({})", c.name, c.workflow)
        };
        if c.url.is_empty() {
            p.push_str(&format!("- {name}\n"));
        } else {
            p.push_str(&format!("- {name}: {}\n", c.url));
        }
    }
    for c in failed.iter().filter(|c| !c.log.is_empty()) {
        p.push_str(&format!(
            "\nEnd of the failing step's log of {}:\n```\n",
            c.name
        ));
        for l in &c.log {
            p.push_str(l);
            p.push('\n');
        }
        p.push_str("```\n");
    }
    p.push_str(&format!(
        "\nRead the rest with `gh` (`gh pr checks {url}`, `gh run view --job <job id> --log-failed -R {repo}`; \
the job id ends each check's address).\n\n{checkout}\n\n\
Find the cause and fix it in this checkout. Run the same command locally to confirm it passes, then summarize \
what changed. Don't push.",
        repo = pr.repo,
        checkout = checkout_step(pr),
    ));
    p
}

/// The prompt behind "Resolve conflicts with agent": bring the base branch
/// into the pull request's branch and settle the conflicts, without pushing.
pub fn conflicts_prompt(pr: &PullRequest) -> String {
    let (url, title) = (&pr.url, &pr.title);
    let (base, branch) = (&pr.base, &pr.local_branch);
    format!(
        "The pull request \"{title}\" ({label}) conflicts with its base branch `{base}`.\nURL: {url}\n\n\
{checkout}\n\n\
Fetch the base (`git fetch <remote> {base}`) and merge `<remote>/{base}` into `{branch}`; don't rebase, so nobody has \
to force-push. Settle each conflict by keeping what both sides meant: read the pull request \
(`gh pr view {url}`) and the commits on `{base}` that touched the same lines to understand each side. When the \
two can't both hold, keep the base's behavior, and tell me what of the pull request you dropped.\n\n\
Build and run the tests that cover the files that conflicted, then commit the merge. Tell me which files \
conflicted and how you settled each. Don't push.",
        label = pr.label(),
        checkout = checkout_step(pr),
    )
}

/// The prompt behind "Address comments with agent": the open review threads,
/// each fixed in the checkout or answered with a reason. The agent neither
/// replies nor resolves on GitHub: the user does that after reading its
/// report. `requested_changes` are the reviewers whose latest review asked
/// for changes.
pub fn comments_prompt(
    pr: &PullRequest,
    open: &[ReviewThread],
    requested_changes: &[String],
) -> String {
    let (url, repo, n, title) = (&pr.url, &pr.repo, pr.number, &pr.title);
    let branch = &pr.head;
    let mut p = format!(
        "Address the open review comments on the pull request \"{title}\" ({label}), branch `{branch}` at {sha}.\n\
URL: {url}\n\nOpen threads ({count}):\n",
        label = pr.label(),
        sha = short_sha(&pr.head_sha),
        count = open.len(),
    );
    let mut room = THREADS_IN_PROMPT;
    let mut left_out = 0;
    for (i, t) in open.iter().enumerate() {
        let mut block = format!("\n{}. `{}`", i + 1, t.path);
        if let Some(line) = t.line {
            block.push_str(&format!(" line {line}"));
        }
        if t.outdated {
            block.push_str(" (outdated: the code under it changed since)");
        }
        if !t.url.is_empty() {
            block.push_str(&format!(", {}", t.url));
        }
        block.push('\n');
        for c in &t.comments {
            let who = if c.author.is_empty() {
                "someone"
            } else {
                &c.author
            };
            let body = clip_comment(c.body.trim());
            block.push_str(&format!("   {who}: {}\n", body.replace('\n', "\n   ")));
        }
        if block.len() > room {
            left_out = open.len() - i;
            break;
        }
        room -= block.len();
        p.push_str(&block);
    }
    if left_out > 0 {
        p.push_str(&format!(
            "\n{left_out} more not shown here: read them with `gh api repos/{repo}/pulls/{n}/comments`.\n"
        ));
    }
    if !requested_changes.is_empty() {
        p.push_str(&format!(
            "\n{} requested changes; read their review too (`gh pr view {url} --comments`).\n",
            requested_changes.join(", ")
        ));
    }
    p.push_str(&format!(
        "\nRead any comment cut short above in full with `gh api repos/{repo}/pulls/{n}/comments`.\n\n{checkout}\n\n\
For each thread, check the comment against the code. Fix it in this checkout when it is right; when it is wrong \
or not worth it, don't change the code and give the reason. Build and run the tests that cover what you changed.\n\n\
Then list every thread by its number with what you did, or the reason you didn't, in a sentence I can post as \
the reply. Don't commit, push, reply to or resolve anything on GitHub: I do that myself.",
        checkout = checkout_step(pr),
    ));
    p
}

/// The start of a comment, cut at a character boundary.
fn clip_comment(body: &str) -> String {
    if body.len() <= COMMENT_IN_PROMPT {
        return body.to_string();
    }
    let mut end = COMMENT_IN_PROMPT;
    while !body.is_char_boundary(end) {
        end -= 1;
    }
    format!("{} […cut]", &body[..end])
}

/// How an agent gets onto a pull request's branch before working on it. Arakun
/// starts it in the environment on the local branch, so this mostly brings in
/// the latest commits. When it couldn't (no project of that repository), the
/// agent switches only when that leaves every other worktree alone.
fn checkout_step(pr: &PullRequest) -> String {
    format!(
        "This checkout may be another repository or another branch: check with `git remote -v` and \
`git branch --show-current`. If no remote points at {repo}, say so and stop. Use the remote that points at \
{repo} (in a fork checkout that is often `upstream`, not `origin`) wherever `<remote>` appears below. Fetch the \
pull request's current head with `git fetch <remote> refs/pull/{number}/head` (it works for branches in forks \
too); if that fails, say so and stop. If it isn't on `{branch}`, switch to it when the working tree is clean (`git switch -c {branch} FETCH_HEAD` \
if it doesn't exist here), else say so and stop. If git says `{branch}` is checked out in another worktree, \
don't touch that worktree or use `--ignore-other-worktrees`: say where it is and stop. Then run \
`git merge --ff-only FETCH_HEAD` so the branch has the pull request's latest commits; if that fails, the local \
branch has diverged from the pull request: say so and stop.",
        repo = pr.repo,
        number = pr.number,
        branch = pr.local_branch,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    const URL: &str = "https://github.com/ArakunHQ/arakun/pull/41";

    fn pr() -> PullRequest {
        PullRequest {
            repo: "ArakunHQ/arakun".into(),
            number: 41,
            url: URL.into(),
            title: "fix(terminal): keep the tab name on resize".into(),
            head: "fix/tab-name".into(),
            head_sha: "0123456789abcdef0123456789abcdef01234567".into(),
            base: "main".into(),
            local_branch: "fix/tab-name".into(),
        }
    }

    fn thread(path: &str, line: u32, url: &str, comments: &[(&str, &str)]) -> ReviewThread {
        ReviewThread {
            path: path.into(),
            line: Some(line),
            outdated: false,
            url: url.into(),
            comments: comments
                .iter()
                .map(|(author, body)| ThreadComment {
                    author: (*author).into(),
                    body: (*body).into(),
                })
                .collect(),
        }
    }

    #[test]
    fn the_review_prompt_asks_for_a_draft_and_nothing_else() {
        let p = review_prompt(&pr());
        assert!(p.starts_with("Review the pull request \"fix(terminal): keep the tab name on resize\" (ArakunHQ/arakun#41)"), "{p}");
        assert!(p.contains(&format!("URL: {URL}\n")), "{p}");
        assert!(
            p.contains(
                "the description, the full diff, the existing reviews and comments, and the checks"
            ),
            "{p}"
        );
        assert!(p.contains(&format!("`gh pr diff {URL}`")), "{p}");
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
            p.contains(&format!("First note the commit you are reviewing: `gh pr view {URL} --json headRefOid --jq .headRefOid`.")),
            "{p}"
        );
        assert!(p.ends_with("Never post a comment or a review, approve, request changes, merge or close anything on GitHub yourself."), "{p}");
    }

    #[test]
    fn the_catch_up_prompt_only_reads() {
        let new = [NewCommit {
            sha: "bbbbbbbbbbbbbbbb".into(),
            headline: "test the resize".into(),
        }];
        let text = catch_up_prompt(&pr(), "2h ago", &new);
        assert!(text.starts_with("Catch me up on the pull request \"fix(terminal): keep the tab name on resize\" (ArakunHQ/arakun#41)"), "{text}");
        assert!(text.contains("I last reviewed it 2h ago. These commits arrived after my review:\n- bbbbbbb test the resize\n"), "{text}");
        assert!(text.contains(&format!("`gh pr diff {URL}`")), "{text}");
        assert!(text.ends_with(HANDS_OFF), "{text}");
        let none = catch_up_prompt(&pr(), "2h ago", &[]);
        assert!(none.contains("No commits arrived since then"), "{none}");
        let never = catch_up_prompt(&pr(), "", &[]);
        assert!(never.contains("I have not reviewed it yet."), "{never}");
        assert!(HANDS_OFF.contains(
            "Do not post a comment or a review, approve, request changes, merge or close anything"
        ));
    }

    #[test]
    fn the_fix_checks_prompt_names_each_check_and_its_log() {
        let mut pr = pr();
        pr.head = "pr/resize".into();
        pr.local_branch = "pr/resize".into();
        let here = "https://github.com/ArakunHQ/arakun/actions/runs";
        let failed = [
            FailedCheck {
                name: "Clippy".into(),
                workflow: "CI".into(),
                url: format!("{here}/7/job/70"),
                log: vec!["error: unused variable `x`".into()],
            },
            FailedCheck {
                name: "Tests".into(),
                workflow: "CI".into(),
                url: format!("{here}/7/job/71"),
                log: vec![],
            },
            FailedCheck {
                name: "deploy/preview".into(),
                ..Default::default()
            },
        ];
        let prompt = fix_checks_prompt(&pr, &failed);
        assert!(
            prompt.contains("failed on branch `pr/resize` at 0123456"),
            "{prompt}"
        );
        assert!(
            prompt.contains(&format!("- Clippy (CI): {here}/7/job/70")),
            "{prompt}"
        );
        assert!(prompt.contains("- deploy/preview\n"), "{prompt}");
        assert!(
            prompt.contains("log of Clippy:\n```\nerror: unused variable `x`\n```"),
            "{prompt}"
        );
        assert!(!prompt.contains("log of Tests"), "{prompt}");
        assert!(
            prompt.contains("`git switch -c pr/resize FETCH_HEAD`"),
            "{prompt}"
        );
        assert!(prompt.ends_with("Don't push."), "{prompt}");
    }

    #[test]
    fn the_conflicts_prompt_merges_the_base_without_pushing() {
        let prompt = conflicts_prompt(&pr());
        assert!(prompt.contains("base branch `main`"), "{prompt}");
        assert!(
            prompt.contains("merge `<remote>/main` into `fix/tab-name`"),
            "{prompt}"
        );
        // In a fork checkout `origin` is the fork: its pull 41 is another one.
        assert!(
            prompt.contains("remote that points at ArakunHQ/arakun"),
            "{prompt}"
        );
        // A stale local branch would leave the newest conflicts unmerged.
        assert!(
            prompt.contains("`git fetch <remote> refs/pull/41/head`"),
            "{prompt}"
        );
        assert!(
            prompt.contains("`git merge --ff-only FETCH_HEAD`"),
            "{prompt}"
        );
        assert!(prompt.ends_with("Don't push."), "{prompt}");
    }

    #[test]
    fn a_forks_branch_is_worked_on_under_its_local_name() {
        let mut pr = pr();
        pr.head = "main".into();
        pr.local_branch = "pr-41".into();
        let prompt = comments_prompt(&pr, &[], &[]);
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
        let open = [
            thread(
                "src/terminal.rs",
                42,
                "https://github.com/ArakunHQ/arakun/pull/41#discussion_r101",
                &[
                    ("Karlo", "This reads the name too early."),
                    ("rik", "Moved it after the resize."),
                ],
            ),
            thread("src/terminal/tests.rs", 7, "", &[("", "Name the test")]),
        ];
        let prompt = comments_prompt(&pr(), &open, &["Karlo".into()]);
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
        let mut old = open[0].clone();
        old.outdated = true;
        let prompt = comments_prompt(&pr(), &[old], &[]);
        assert!(
            prompt.contains("line 42 (outdated: the code under it changed since), https://"),
            "{prompt}"
        );
        assert!(!prompt.contains("requested changes"), "{prompt}");
    }

    #[test]
    fn long_comments_and_many_threads_are_cut() {
        let long = "é".repeat(COMMENT_IN_PROMPT);
        let one = thread("src/a.rs", 1, "", &[("Karlo", &long)]);
        let prompt = comments_prompt(&pr(), std::slice::from_ref(&one), &[]);
        assert!(prompt.contains("[…cut]"), "{prompt}");

        let many: Vec<ReviewThread> = (0..100).map(|_| one.clone()).collect();
        let prompt = comments_prompt(&pr(), &many, &[]);
        assert!(prompt.len() < THREADS_IN_PROMPT + 3000, "{}", prompt.len());
        assert!(prompt.contains("more not shown here"), "{prompt}");
    }
}
