//! Everything that happened on a pull request or issue, oldest first, and its review threads.

use std::collections::HashMap;

use super::*;

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
    /// The line of today's diff it is on, and whether that is the removed
    /// side; None once the code under it changed.
    pub place: Option<(u32, bool)>,
    /// The thread's node id: what resolving it needs.
    pub node: Option<String>,
    pub comments: Vec<InlineComment>,
}

impl Thread {
    /// The id to answer the thread with (`reply`).
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
                    place: comment.line.map(|l| (l, comment.side == "LEFT")),
                    node: comment.thread.clone(),
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::github::pr::fixtures::*;

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

        // Where a thread sits in today's diff, and which thread it is.
        let placed = InlineComment {
            id: 7,
            line: Some(12),
            side: "LEFT".into(),
            thread: Some("PRRT_7".into()),
            ..Default::default()
        };
        let outdated = InlineComment {
            id: 8,
            original_line: Some(3),
            ..Default::default()
        };
        let t = threads(&[placed, outdated]);
        assert_eq!(
            (t[0].place, t[0].node.as_deref()),
            (Some((12, true)), Some("PRRT_7"))
        );
        // Once the code under it changed it has no line to show under.
        assert_eq!((t[1].place, t[1].line), (None, Some(3)));
    }
}
