//! What blocks the merge, whose move it is, and where each reviewer and the user stand.

use super::*;

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

pub(super) fn conflicting(pull: &Pull) -> bool {
    pull.mergeable == "CONFLICTING" || pull.merge_state_status == "DIRTY"
}

/// Who asked for changes and still stands by it.
pub(super) fn requested_changes(pull: &Pull) -> Vec<String> {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::github::pr::fixtures::*;

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
}
