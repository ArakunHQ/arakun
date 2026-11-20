//! The review queue: everything that waits on the user or that they wait on,
//! gathered from two places. The inbox alone misses what was already read on
//! github.com, so searches add the pull requests whose review is asked of the
//! user, the user's own open pull requests, what is assigned to them or
//! mentions them, and what closed today. Each entry is then placed in a group
//! by what is known of it: its details when they were read, otherwise where it
//! was found.

use std::collections::HashMap;

use serde::Deserialize;

use super::pr::{self, ItemKind, ItemRef, WaitingOn};
use super::{Kind, Notification, Problem};

// ---------------------------------------------------------------------------
// Searches

/// Why a search found an item.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Why {
    /// An open pull request whose review is asked of the user.
    ReviewRequested,
    /// An open pull request the user opened.
    Authored,
    /// An open issue or pull request assigned to the user.
    Assigned,
    /// An open issue or pull request that mentions the user, lately.
    Mentioned,
    /// Closed or merged today, with the user involved.
    ClosedToday,
    /// An open pull request the user reviewed today.
    ReviewedToday,
}

impl Why {
    pub const ALL: [Why; 6] = [
        Why::ReviewRequested,
        Why::Authored,
        Why::Assigned,
        Why::Mentioned,
        Why::ClosedToday,
        Why::ReviewedToday,
    ];
}

/// How many items one search asks for.
pub(super) const SEARCH_LIMIT: usize = 50;
const PULL_FIELDS: &str =
    "number,title,repository,author,state,isDraft,createdAt,updatedAt,closedAt,url";
const ISSUE_FIELDS: &str =
    "number,title,repository,author,state,isPullRequest,createdAt,updatedAt,closedAt,url";

#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct FoundRepo {
    pub name_with_owner: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct FoundAuthor {
    pub login: String,
}

/// One result of `gh search prs` or `gh search issues`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Found {
    pub number: u64,
    pub title: String,
    pub repository: FoundRepo,
    pub author: FoundAuthor,
    /// `open`, `closed` or `merged`, in lower case.
    pub state: String,
    pub is_draft: bool,
    /// Only `gh search issues` says it; a pull request search leaves it None.
    pub is_pull_request: Option<bool>,
    pub created_at: String,
    pub updated_at: String,
    pub closed_at: String,
    pub url: String,
}

impl Found {
    pub fn item(&self, why: Why) -> Option<ItemRef> {
        let pull = match why {
            Why::ReviewRequested | Why::Authored | Why::ReviewedToday | Why::ClosedToday => true,
            Why::Assigned | Why::Mentioned => self.is_pull_request.unwrap_or(false),
        };
        let kind = if pull {
            ItemKind::Pull
        } else {
            ItemKind::Issue
        };
        let item = ItemRef::from_url(&self.url).map(|mut i| {
            i.kind = kind;
            i
        });
        item.or_else(|| {
            let parsed = ItemRef::parse(&format!(
                "{}#{}",
                self.repository.name_with_owner, self.number
            ))?;
            Some(ItemRef { kind, ..parsed })
        })
    }
}

/// The arguments of one search. `today` and `since` are dates
/// (`2026-11-14`): the start of the local day, and how far back mentions count.
pub fn search_args(why: Why, today: &str, since: &str) -> Vec<String> {
    let mut args: Vec<String> = match why {
        Why::ReviewRequested => vec![
            "prs".into(),
            "--review-requested=@me".into(),
            "--state=open".into(),
        ],
        Why::Authored => vec!["prs".into(), "--author=@me".into(), "--state=open".into()],
        Why::Assigned => vec![
            "issues".into(),
            "--include-prs".into(),
            "--assignee=@me".into(),
            "--state=open".into(),
        ],
        Why::Mentioned => {
            vec![
                "issues".into(),
                "--include-prs".into(),
                "--mentions=@me".into(),
                "--state=open".into(),
                format!("--updated=>={since}"),
            ]
        }
        Why::ClosedToday => vec![
            "prs".into(),
            "--involves=@me".into(),
            "--state=closed".into(),
            format!("--closed=>={today}"),
        ],
        Why::ReviewedToday => vec![
            "prs".into(),
            "--reviewed-by=@me".into(),
            "--state=open".into(),
            format!("--updated=>={today}"),
        ],
    };
    let fields = if args[0] == "issues" {
        ISSUE_FIELDS
    } else {
        PULL_FIELDS
    };
    let mut all = vec!["search".to_string()];
    all.append(&mut args);
    all.extend([
        "--archived=false".into(),
        "--limit".into(),
        SEARCH_LIMIT.to_string(),
        "--json".into(),
        fields.into(),
    ]);
    all
}

pub fn parse_found(bytes: &[u8]) -> Result<Vec<Found>, Problem> {
    let value: serde_json::Value = serde_json::from_slice(bytes)
        .map_err(|e| Problem::Other(format!("unexpected answer from gh: {e}")))?;
    serde_json::from_value(strip_nulls(value))
        .map_err(|e| Problem::Other(format!("unexpected answer from gh: {e}")))
}

fn strip_nulls(value: serde_json::Value) -> serde_json::Value {
    use serde_json::Value;
    match value {
        Value::Object(map) => Value::Object(
            map.into_iter()
                .filter(|(_, v)| !v.is_null())
                .map(|(k, v)| (k, strip_nulls(v)))
                .collect(),
        ),
        Value::Array(list) => Value::Array(
            list.into_iter()
                .filter(|v| !v.is_null())
                .map(strip_nulls)
                .collect(),
        ),
        other => other,
    }
}

/// Every search, at once. What failed is left out, and the first failure is
/// returned with the rest, so one bad search doesn't empty the queue.
/// What every search found, which ones failed, and the first failure.
pub type Searches = (Vec<(Why, Found)>, Vec<Why>, Option<Problem>);

pub fn search_all(today: &str, since: &str) -> Searches {
    let results: Vec<(Why, Result<Vec<Found>, Problem>)> = std::thread::scope(|s| {
        let handles: Vec<_> = Why::ALL
            .into_iter()
            .map(|why| {
                let args = search_args(why, today, since);
                (
                    why,
                    s.spawn(move || {
                        let refs: Vec<&str> = args.iter().map(String::as_str).collect();
                        super::gh_out(&refs).and_then(|out| parse_found(&out))
                    }),
                )
            })
            .collect();
        handles
            .into_iter()
            .map(|(why, h)| {
                (
                    why,
                    h.join()
                        .unwrap_or_else(|_| Err(Problem::Other("a search failed".into()))),
                )
            })
            .collect()
    });
    let mut found = Vec::new();
    let mut failed = Vec::new();
    let mut problem = None;
    for (why, result) in results {
        match result {
            Ok(list) => found.extend(list.into_iter().map(|f| (why, f))),
            Err(p) => {
                failed.push(why);
                problem.get_or_insert(p);
            }
        }
    }
    (found, failed, problem)
}

/// The searches' results after a read: what each search found now, and for
/// a search that failed, what it found last time. A failed search must not
/// empty its part of the queue (review requests already read on GitHub would
/// vanish until the next good read).
pub fn merge_found(
    old: Vec<(Why, Found)>,
    new: Vec<(Why, Found)>,
    failed: &[Why],
) -> Vec<(Why, Found)> {
    let mut out: Vec<(Why, Found)> = old
        .into_iter()
        .filter(|(why, _)| failed.contains(why))
        .collect();
    out.extend(new.into_iter().filter(|(why, _)| !failed.contains(why)));
    out
}

// ---------------------------------------------------------------------------
// Entries

/// One thing in the queue: a pull request or issue, or a notification about
/// something else (a CI run, a release).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    /// None for a notification that is not about a pull request or issue.
    pub item: Option<ItemRef>,
    pub title: String,
    /// Empty when only a notification named it.
    pub author: String,
    /// ISO 8601: the newest activity any source knows of.
    pub updated_at: String,
    pub closed_at: String,
    /// What a search said of its state: `open`, `closed`, `merged`, or empty.
    pub state: String,
    pub why: Vec<Why>,
    /// Its notification, read or not.
    pub notification: Option<Notification>,
}

impl Entry {
    /// `owner/repo#12` in lower case, or the notification's id: what tells
    /// two entries apart.
    pub fn key(&self) -> String {
        match (&self.item, &self.notification) {
            (Some(item), _) => key_of(item),
            (None, Some(n)) => format!("thread:{}", n.id),
            (None, None) => String::new(),
        }
    }

    pub fn unread(&self) -> bool {
        self.notification.as_ref().is_some_and(|n| n.unread)
    }

    pub fn updated(&self) -> u64 {
        crate::ci::parse_time(&self.updated_at).unwrap_or(0)
    }

    pub fn repo(&self) -> String {
        match (&self.item, &self.notification) {
            (Some(item), _) => item.repo.clone(),
            (None, Some(n)) => n.repository.full_name.clone(),
            _ => String::new(),
        }
    }
}

pub fn key_of(item: &ItemRef) -> String {
    format!("{}#{}", item.repo.to_ascii_lowercase(), item.number)
}

/// The notifications and the search results as one list, each item once.
pub fn gather(notifications: &[Notification], found: &[(Why, Found)]) -> Vec<Entry> {
    let mut out: Vec<Entry> = Vec::new();
    let mut at: HashMap<String, usize> = HashMap::new();
    for (why, f) in found {
        let Some(item) = f.item(*why) else { continue };
        let key = key_of(&item);
        match at.get(&key) {
            Some(&i) => {
                let e = &mut out[i];
                if !e.why.contains(why) {
                    e.why.push(*why);
                }
                if f.updated_at > e.updated_at {
                    e.updated_at = f.updated_at.clone();
                }
            }
            None => {
                at.insert(key, out.len());
                out.push(Entry {
                    item: Some(item),
                    title: f.title.clone(),
                    author: f.author.login.clone(),
                    updated_at: f.updated_at.clone(),
                    closed_at: f.closed_at.clone(),
                    state: f.state.clone(),
                    why: vec![*why],
                    notification: None,
                });
            }
        }
    }
    for n in notifications {
        let item = pr::ItemRef::from_notification(n);
        let key = item
            .as_ref()
            .map(key_of)
            .unwrap_or_else(|| format!("thread:{}", n.id));
        match at.get(&key) {
            Some(&i) => {
                let e = &mut out[i];
                if n.updated_at > e.updated_at {
                    e.updated_at = n.updated_at.clone();
                }
                e.notification = Some(n.clone());
            }
            None => {
                at.insert(key, out.len());
                out.push(Entry {
                    item,
                    title: n.subject.title.clone(),
                    author: String::new(),
                    updated_at: n.updated_at.clone(),
                    closed_at: String::new(),
                    state: String::new(),
                    why: Vec::new(),
                    notification: Some(n.clone()),
                });
            }
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Placing

/// What was read of an item.
#[derive(Clone, Debug)]
pub enum Details {
    Pull(Box<pr::Pull>),
    Issue(Box<pr::Issue>),
}

impl Details {
    pub fn updated_at(&self) -> &str {
        match self {
            Details::Pull(p) => &p.updated_at,
            Details::Issue(i) => &i.updated_at,
        }
    }

    /// A mark of the comments on the diff and what can be done with each:
    /// it moves when a thread is answered, resolved or opened again, or its
    /// id arrives after a read that failed, which the item's date doesn't
    /// always do. Counts alone miss one thread resolving while another opens.
    pub fn threads_mark(&self) -> u64 {
        use std::hash::{Hash as _, Hasher as _};
        let mut mark = std::collections::hash_map::DefaultHasher::new();
        if let Details::Pull(p) = self {
            for c in &p.inline {
                (c.id, c.resolved, c.outdated, &c.thread).hash(&mut mark);
            }
        }
        mark.finish()
    }
}

/// The groups of the queue, in the order they show.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Group {
    NeedsReview,
    Mentions,
    Yours,
    /// Open issues assigned to the user.
    Issues,
    Ready,
    #[default]
    Other,
    Done,
}

impl Group {
    pub const ALL: [Group; 7] = [
        Group::NeedsReview,
        Group::Mentions,
        Group::Yours,
        Group::Issues,
        Group::Ready,
        Group::Other,
        Group::Done,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Group::NeedsReview => "Needs your review",
            Group::Mentions => "Mentions",
            Group::Yours => "Your pull requests",
            Group::Issues => "Assigned issues",
            Group::Ready => "Ready to merge",
            Group::Other => "Other",
            Group::Done => "Done today",
        }
    }
}

/// Whose move it is, for the bar above the queue.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Lane {
    OnYou,
    OnOthers,
    Ready,
    Done,
    /// Counted nowhere: a notification that asks nothing of anyone.
    #[default]
    None,
}

/// The row's round icon: what kind of thing it is, in which state.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Mark {
    Pull,
    Issue,
    Mention,
    Merge,
    Done,
    Closed,
    Ci,
    #[default]
    Other,
}

/// An entry in its place, with what its row says.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Placed {
    pub group: Group,
    pub lane: Lane,
    pub mark: Mark,
    /// Why it is here, under the title after the repository and number:
    /// `rik`, `approved by you · 1 check passed`.
    pub sub: String,
    /// Whose it is: the login its avatar shows. Empty when unknown.
    pub author: String,
    /// A pull request's lines added and removed, once read.
    pub changes: Option<(u32, u32)>,
    /// How a pull request's checks stand, when it has any.
    pub checks: Option<pr::CheckState>,
}

fn closed_since(at: &str, day_start: u64) -> bool {
    crate::ci::parse_time(at).is_some_and(|t| t >= day_start)
}

/// Where an entry goes, seen by `me`. None for something closed before today:
/// nothing is left to do with it. `day_start` is the start of the local day,
/// in unix seconds.
pub fn place(e: &Entry, details: Option<&Details>, me: &str, day_start: u64) -> Option<Placed> {
    let mut p = place_only(e, details, me, day_start)?;
    match details {
        Some(Details::Pull(pull)) => {
            p.author = pull.author.login.clone();
            p.changes = Some((pull.additions, pull.deletions));
            p.checks = pr::checks_summary(pull).overall();
        }
        Some(Details::Issue(i)) => p.author = i.author.login.clone(),
        None => p.author = e.author.clone(),
    }
    Some(p)
}

/// `place` without what the row draws beside its text.
fn place_only(e: &Entry, details: Option<&Details>, me: &str, day_start: u64) -> Option<Placed> {
    let num = e
        .item
        .as_ref()
        .map(|i| format!("#{}", i.number))
        .unwrap_or_default();
    let reason = e
        .notification
        .as_ref()
        .map(|n| n.reason.as_str())
        .unwrap_or("");
    let mentioned = e.why.contains(&Why::Mentioned) || matches!(reason, "mention" | "team_mention");
    let assigned = e.why.contains(&Why::Assigned) || reason == "assign";
    let mine = |login: &str| !login.is_empty() && login.eq_ignore_ascii_case(me);
    let join = |parts: &[&str]| {
        parts
            .iter()
            .filter(|p| !p.is_empty())
            .copied()
            .collect::<Vec<_>>()
            .join(" · ")
    };

    match details {
        Some(Details::Pull(p)) => {
            let author = p.author.login.as_str();
            if !p.open() {
                let at = if p.merged() {
                    &p.merged_at
                } else {
                    &p.closed_at
                };
                if !closed_since(at, day_start) {
                    return None;
                }
                let what = if p.merged() { "merged" } else { "closed" };
                let mark = if p.merged() { Mark::Done } else { Mark::Closed };
                return Some(Placed {
                    group: Group::Done,
                    lane: Lane::Done,
                    mark,
                    sub: join(&[what, author]),
                    ..Default::default()
                });
            }
            let waiting = pr::waiting_on(p, me);
            let blockers = pr::blockers(p);
            let verdict = pr::my_verdict(p, me);
            let approved_by_me = verdict.is_some_and(|r| r.state == "APPROVED");
            if let WaitingOn::You(why) = &waiting {
                if mine(author) {
                    return Some(Placed {
                        group: Group::Yours,
                        lane: Lane::OnYou,
                        mark: Mark::Pull,
                        sub: join(&[why]),
                        ..Default::default()
                    });
                }
                return Some(Placed {
                    group: Group::NeedsReview,
                    lane: Lane::OnYou,
                    mark: Mark::Pull,
                    sub: join(&[author]),
                    ..Default::default()
                });
            }
            // `review-requested:@me` also finds requests to the user's teams,
            // which the details only name by team. While the search still
            // finds it and a team is still asked, it waits on the user.
            let team_asked = p
                .review_requests
                .iter()
                .any(|r| r.login.is_empty() && !r.who().is_empty());
            if e.why.contains(&Why::ReviewRequested) && team_asked && !mine(author) {
                return Some(Placed {
                    group: Group::NeedsReview,
                    lane: Lane::OnYou,
                    mark: Mark::Pull,
                    sub: join(&[author, "your team's review is requested"]),
                    ..Default::default()
                });
            }
            if blockers.is_empty() && (mine(author) || approved_by_me) {
                let checks = pr::checks_summary(p);
                let by = if approved_by_me {
                    "approved by you".to_string()
                } else {
                    approval_text(p)
                };
                let passed = match checks.passed {
                    0 => String::new(),
                    1 => "1 check passed".into(),
                    n => format!("{n} checks passed"),
                };
                return Some(Placed {
                    group: Group::Ready,
                    lane: Lane::Ready,
                    mark: Mark::Merge,
                    sub: join(&[&by, &passed]),
                    ..Default::default()
                });
            }
            if mine(author) {
                return Some(Placed {
                    group: Group::Yours,
                    lane: Lane::OnOthers,
                    mark: Mark::Pull,
                    sub: join(&[&waiting_text(&waiting)]),
                    ..Default::default()
                });
            }
            if mentioned || assigned {
                let asked = asked_text(assigned, author, me);
                return Some(Placed {
                    group: Group::Mentions,
                    lane: Lane::OnYou,
                    mark: Mark::Mention,
                    sub: join(&[&asked]),
                    ..Default::default()
                });
            }
            if let Some(v) = verdict {
                let gave = if v.state == "APPROVED" {
                    "you approved"
                } else {
                    "you asked for changes"
                };
                let at = crate::ci::parse_time(&v.submitted_at).unwrap_or(0);
                let group = if at >= day_start {
                    Group::Done
                } else {
                    Group::Other
                };
                let mark = if group == Group::Done {
                    Mark::Done
                } else {
                    Mark::Pull
                };
                return Some(Placed {
                    group,
                    lane: Lane::OnOthers,
                    mark,
                    sub: join(&[gave, &waiting_text(&waiting)]),
                    ..Default::default()
                });
            }
            Some(Placed {
                group: Group::Other,
                lane: Lane::None,
                mark: Mark::Pull,
                sub: join(&[author, &waiting_text(&waiting)]),
                ..Default::default()
            })
        }
        Some(Details::Issue(i)) => {
            let author = i.author.login.as_str();
            if !i.open() {
                if !closed_since(&i.closed_at, day_start) {
                    return None;
                }
                return Some(Placed {
                    group: Group::Done,
                    lane: Lane::Done,
                    mark: Mark::Closed,
                    sub: join(&["issue", "closed"]),
                    ..Default::default()
                });
            }
            let assigned = assigned || i.assignees.iter().any(|a| mine(&a.login));
            if assigned {
                // Work to do, not a question to answer: a group of its own.
                let labels: Vec<&str> = i.labels.iter().map(|l| l.name.as_str()).collect();
                return Some(Placed {
                    group: Group::Issues,
                    lane: Lane::OnYou,
                    mark: Mark::Issue,
                    sub: join(&["assigned to you", &labels.join(", ")]),
                    ..Default::default()
                });
            }
            if mentioned {
                return Some(Placed {
                    group: Group::Mentions,
                    lane: Lane::OnYou,
                    mark: Mark::Mention,
                    sub: join(&["issue", &asked_text(false, author, me)]),
                    ..Default::default()
                });
            }
            Some(Placed {
                group: Group::Other,
                lane: Lane::None,
                mark: Mark::Issue,
                sub: join(&["issue", author]),
                ..Default::default()
            })
        }
        None => place_unread(e, &num, reason, mentioned, assigned, me, day_start, &join),
    }
}

/// Why a mention row asks for the user. `author` is the item's author, not
/// whoever wrote the mention, so on the user's own item it would name them.
fn asked_text(assigned: bool, author: &str, me: &str) -> String {
    if assigned {
        "assigned to you".into()
    } else if author.is_empty() || author.eq_ignore_ascii_case(me) {
        "mentioned you".into()
    } else {
        format!("{author} mentioned you")
    }
}

/// Where an entry goes before its details are read: by where it was found.
fn place_unread(
    e: &Entry,
    num: &str,
    reason: &str,
    mentioned: bool,
    assigned: bool,
    me: &str,
    day_start: u64,
    join: &dyn Fn(&[&str]) -> String,
) -> Option<Placed> {
    let author = e.author.as_str();
    let issue = e.item.as_ref().is_some_and(|i| i.kind == ItemKind::Issue);
    let what = if issue { "issue" } else { "" };
    if e.why.contains(&Why::ClosedToday) || matches!(e.state.as_str(), "closed" | "merged") {
        if !closed_since(&e.closed_at, day_start) {
            return None;
        }
        let merged = e.state == "merged";
        let mark = if merged { Mark::Done } else { Mark::Closed };
        return Some(Placed {
            group: Group::Done,
            lane: Lane::Done,
            mark,
            sub: join(&[if merged { "merged" } else { "closed" }, author]),
            ..Default::default()
        });
    }
    if e.why.contains(&Why::ReviewRequested) || reason == "review_requested" {
        return Some(Placed {
            group: Group::NeedsReview,
            lane: Lane::OnYou,
            mark: Mark::Pull,
            sub: join(&[author]),
            ..Default::default()
        });
    }
    if issue && assigned {
        return Some(Placed {
            group: Group::Issues,
            lane: Lane::OnYou,
            mark: Mark::Issue,
            sub: join(&["assigned to you"]),
            ..Default::default()
        });
    }
    if mentioned || assigned {
        let asked = asked_text(assigned, author, me);
        return Some(Placed {
            group: Group::Mentions,
            lane: Lane::OnYou,
            mark: Mark::Mention,
            sub: join(&[what, &asked]),
            ..Default::default()
        });
    }
    if e.why.contains(&Why::Authored) || reason == "author" && !issue && e.item.is_some() {
        return Some(Placed {
            group: Group::Yours,
            lane: Lane::OnOthers,
            mark: Mark::Pull,
            sub: join(&["your pull request"]),
            ..Default::default()
        });
    }
    if e.why.contains(&Why::ReviewedToday) {
        return Some(Placed {
            group: Group::Done,
            lane: Lane::OnOthers,
            mark: Mark::Done,
            sub: join(&["you reviewed it today"]),
            ..Default::default()
        });
    }
    // Only an unread notification keeps something here that no search found.
    if !e.unread() {
        return None;
    }
    let mark = match (
        e.item.as_ref().map(|i| i.kind),
        e.notification.as_ref().map(Notification::kind),
    ) {
        (Some(ItemKind::Pull), _) => Mark::Pull,
        (Some(ItemKind::Issue), _) => Mark::Issue,
        (None, Some(Kind::Ci)) => Mark::Ci,
        _ => Mark::Other,
    };
    let label = super::reason_label(reason);
    let noun = e
        .notification
        .as_ref()
        .map(|n| n.kind().noun())
        .unwrap_or("");
    Some(Placed {
        group: Group::Other,
        lane: Lane::None,
        mark,
        sub: join(&[if num.is_empty() { noun } else { what }, &label]),
        ..Default::default()
    })
}

/// Who approved, for a ready pull request that isn't the user's approval.
fn approval_text(p: &pr::Pull) -> String {
    let who: Vec<&str> = p
        .latest_reviews
        .iter()
        .filter(|r| r.state == "APPROVED")
        .map(|r| r.author.login.as_str())
        .collect();
    match who.as_slice() {
        [] => "no review needed".into(),
        [one] => format!("approved by {one}"),
        [one, rest @ ..] => format!("approved by {one} +{}", rest.len()),
    }
}

/// "waiting on ana, Core" in a row's line.
pub fn waiting_text(w: &WaitingOn) -> String {
    match w {
        WaitingOn::You(why) => format!("on you: {why}"),
        WaitingOn::Author => "waiting on the author".into(),
        WaitingOn::Reviewers(who) if who.is_empty() => "needs a reviewer".into(),
        WaitingOn::Reviewers(who) => format!("waiting on {}", who.join(", ")),
        WaitingOn::Checks => "waiting on checks".into(),
        WaitingOn::Nobody => "nothing pending".into(),
    }
}

/// The queue laid out: each group's rows in order, and the lane counts.
#[derive(Clone, Debug, Default)]
pub struct Queue {
    pub rows: Vec<(Entry, Placed)>,
    pub on_you: usize,
    pub on_others: usize,
    pub ready: usize,
    pub done: usize,
}

impl Queue {
    pub fn group(&self, g: Group) -> impl Iterator<Item = &(Entry, Placed)> {
        self.rows.iter().filter(move |(_, p)| p.group == g)
    }

    /// Rows in the order they show, the done ones last.
    pub fn tracked(&self) -> usize {
        self.rows.len()
    }
}

/// Every entry placed, then sorted: groups in their order; what waits on the
/// user longest first, what is done newest first.
pub fn build(
    entries: &[Entry],
    details: &HashMap<String, Details>,
    me: &str,
    day_start: u64,
) -> Queue {
    let mut q = Queue::default();
    for e in entries {
        let Some(p) = place(e, details.get(&e.key()), me, day_start) else {
            continue;
        };
        match p.lane {
            Lane::OnYou => q.on_you += 1,
            Lane::OnOthers => q.on_others += 1,
            Lane::Ready => q.ready += 1,
            Lane::Done => q.done += 1,
            Lane::None => {}
        }
        q.rows.push((e.clone(), p));
    }
    // Done counts what finished today, whichever lane it came from.
    q.done = q
        .rows
        .iter()
        .filter(|(_, p)| p.group == Group::Done)
        .count();
    q.on_others = q
        .rows
        .iter()
        .filter(|(_, p)| p.lane == Lane::OnOthers && p.group != Group::Done)
        .count();
    q.rows.sort_by(|(a, pa), (b, pb)| {
        pa.group.cmp(&pb.group).then_with(|| match pa.group {
            Group::Done | Group::Other => b.updated_at.cmp(&a.updated_at),
            _ => a.updated_at.cmp(&b.updated_at),
        })
    });
    q
}

/// The local day of unix time `t`, as `2026-11-14`.
pub fn day_key(t: u64) -> String {
    match crate::platform::local_time(t) {
        Some(l) => format!("{:04}-{:02}-{:02}", l.year, l.month + 1, l.day),
        None => String::new(),
    }
}

/// The start of the local day `days_back` days before the one of `now`.
pub fn day_start(now: u64, days_back: i32) -> u64 {
    crate::platform::local_time(now)
        .and_then(|l| crate::platform::from_local(l.year, l.month, l.day - days_back, 0, 0))
        .unwrap_or_else(|| (now / 86_400).saturating_sub(days_back.max(0) as u64) * 86_400)
}

#[cfg(test)]
mod tests {
    use super::super::{Owner, Repository, Subject};
    use super::*;

    #[test]
    fn the_threads_mark_moves_with_what_can_be_done_on_a_thread() {
        let comment = |id: u64, resolved: Option<bool>, thread: Option<&str>| pr::InlineComment {
            id,
            resolved,
            thread: thread.map(str::to_string),
            ..Default::default()
        };
        let mark = |inline: Vec<pr::InlineComment>| {
            Details::Pull(Box::new(pr::Pull {
                inline,
                ..Default::default()
            }))
            .threads_mark()
        };
        let read = |a, b| vec![comment(1, a, Some("T1")), comment(2, b, Some("T2"))];
        assert_eq!(
            mark(read(Some(false), Some(true))),
            mark(read(Some(false), Some(true)))
        );
        // The thread ids arrive after a read that failed: same counts.
        let before = vec![comment(1, None, None), comment(2, None, None)];
        assert_ne!(mark(before), mark(read(Some(false), Some(false))));
        // One thread resolves while the other opens again: same counts too.
        assert_ne!(
            mark(read(Some(true), Some(false))),
            mark(read(Some(false), Some(true)))
        );
        // An answer is one more comment.
        let mut answered = read(Some(false), Some(false));
        answered.push(comment(3, Some(false), Some("T1")));
        assert_ne!(mark(answered), mark(read(Some(false), Some(false))));
    }

    const ME: &str = "KarloAldrete";
    const DAY: u64 = 1_763_078_400; // 2025-11-14T00:00:00Z

    const SEARCH: &str = r#"[{"author":{"id":"U_1","is_bot":false,"login":"rikvisser-dev","type":"User","url":"https://github.com/rikvisser-dev"},
      "createdAt":"2026-11-14T22:21:33Z","isDraft":false,"number":55,"repository":{"name":"arakun","nameWithOwner":"ArakunHQ/arakun"},
      "state":"open","title":"ci: run clippy and tests on macos","updatedAt":"2026-11-14T22:22:11Z","url":"https://github.com/ArakunHQ/arakun/pull/55"},
      {"author":null,"number":16,"repository":{"name":"Habitae","nameWithOwner":"KarloAldrete/Habitae"},"isPullRequest":false,
      "state":"open","title":"landing review","updatedAt":"2026-06-14T05:01:21Z","url":"https://github.com/KarloAldrete/Habitae/issues/16","closedAt":null}]"#;

    #[test]
    fn mentions_never_name_the_user() {
        assert_eq!(asked_text(false, "rik", ME), "rik mentioned you");
        assert_eq!(asked_text(false, "karloaldrete", ME), "mentioned you");
        assert_eq!(asked_text(false, "", ME), "mentioned you");
        assert_eq!(asked_text(true, ME, ME), "assigned to you");
    }

    fn found(why: Why, repo: &str, n: u64, state: &str, pull: Option<bool>) -> (Why, Found) {
        let page = if pull == Some(false) {
            "issues"
        } else {
            "pull"
        };
        (
            why,
            Found {
                number: n,
                title: format!("title {n}"),
                repository: FoundRepo {
                    name_with_owner: repo.into(),
                },
                author: FoundAuthor {
                    login: "rik".into(),
                },
                state: state.into(),
                is_pull_request: pull,
                updated_at: "2025-11-14T10:00:00Z".into(),
                url: format!("https://github.com/{repo}/{page}/{n}"),
                ..Default::default()
            },
        )
    }

    fn note(id: &str, reason: &str, kind: &str, url: Option<&str>) -> Notification {
        Notification {
            id: id.into(),
            unread: true,
            reason: reason.into(),
            updated_at: "2025-11-14T12:00:00Z".into(),
            subject: Subject {
                title: format!("note {id}"),
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
        }
    }

    #[test]
    fn search_results_are_read() {
        let list = parse_found(SEARCH.as_bytes()).unwrap();
        assert_eq!(list.len(), 2);
        assert_eq!(list[0].repository.name_with_owner, "ArakunHQ/arakun");
        assert_eq!(list[0].author.login, "rikvisser-dev");
        assert_eq!(
            list[0].item(Why::ReviewRequested).unwrap().kind,
            ItemKind::Pull
        );
        assert_eq!(list[1].author.login, "");
        let issue = list[1].item(Why::Mentioned).unwrap();
        assert_eq!(
            (issue.repo.as_str(), issue.number, issue.kind),
            ("KarloAldrete/Habitae", 16, ItemKind::Issue)
        );
    }

    #[test]
    fn searches_ask_gh_for_the_right_things() {
        let args = search_args(Why::ReviewRequested, "2026-11-14", "2026-10-15");
        assert_eq!(
            &args[..4],
            ["search", "prs", "--review-requested=@me", "--state=open"]
        );
        assert!(args.ends_with(&["--json".into(), PULL_FIELDS.into()]));
        let args = search_args(Why::Mentioned, "2026-11-14", "2026-10-15");
        assert!(
            args.contains(&"--include-prs".into())
                && args.contains(&"--updated=>=2026-10-15".into()),
            "{args:?}"
        );
        assert!(args.ends_with(&["--json".into(), ISSUE_FIELDS.into()]));
        let args = search_args(Why::ClosedToday, "2026-11-14", "2026-10-15");
        assert!(
            args.contains(&"--closed=>=2026-11-14".into())
                && args.contains(&"--involves=@me".into()),
            "{args:?}"
        );
        for why in Why::ALL {
            assert!(search_args(why, "a", "b").contains(&"--archived=false".into()));
        }
    }

    #[test]
    fn sources_join_into_one_entry_per_item() {
        let list = [
            found(Why::ReviewRequested, "ArakunHQ/arakun", 34, "open", None),
            found(Why::Mentioned, "arakunhq/arakun", 34, "open", Some(true)),
            found(Why::Assigned, "ArakunHQ/arakun", 33, "open", Some(false)),
        ];
        let notes = [
            note(
                "1",
                "review_requested",
                "PullRequest",
                Some("https://api.github.com/repos/ArakunHQ/arakun/pulls/34"),
            ),
            note("2", "ci_activity", "CheckSuite", None),
        ];
        let entries = gather(&notes, &list);
        assert_eq!(entries.len(), 3);
        assert_eq!(entries[0].why, [Why::ReviewRequested, Why::Mentioned]);
        assert_eq!(
            entries[0].notification.as_ref().map(|n| n.id.as_str()),
            Some("1")
        );
        // The notification is newer than the search's answer.
        assert_eq!(entries[0].updated_at, "2025-11-14T12:00:00Z");
        assert_eq!(entries[1].item.as_ref().unwrap().kind, ItemKind::Issue);
        assert_eq!(entries[2].key(), "thread:2");
        assert!(entries[2].item.is_none());
    }

    #[test]
    fn assigned_issues_have_a_group_of_their_own() {
        let entries = gather(
            &[],
            &[
                found(Why::Assigned, "o/r", 1, "open", Some(false)),
                found(Why::Mentioned, "o/r", 2, "open", Some(false)),
                // An assigned pull request is still a question for the user.
                found(Why::Assigned, "o/r", 3, "open", Some(true)),
            ],
        );
        let place_of = |e: &Entry, d: Option<&Details>| {
            place(e, d, ME, DAY).map(|p| (p.group, p.lane, p.mark, p.sub))
        };
        // Before its details are read, by the search that found it.
        assert_eq!(
            place_of(&entries[0], None),
            Some((
                Group::Issues,
                Lane::OnYou,
                Mark::Issue,
                "assigned to you".into()
            ))
        );
        assert_eq!(
            place_of(&entries[1], None).map(|p| p.0),
            Some(Group::Mentions)
        );
        assert_eq!(
            place_of(&entries[2], None).map(|p| p.0),
            Some(Group::Mentions)
        );

        let issue = |state: &str, assignee: &str, labels: &[&str]| {
            Details::Issue(Box::new(pr::Issue {
                state: state.into(),
                author: pr::Actor {
                    login: "ana".into(),
                    ..Default::default()
                },
                assignees: vec![pr::Actor {
                    login: assignee.into(),
                    ..Default::default()
                }],
                labels: labels
                    .iter()
                    .map(|l| pr::Label {
                        name: l.to_string(),
                        ..Default::default()
                    })
                    .collect(),
                closed_at: "2020-01-01T00:00:00Z".into(),
                ..Default::default()
            }))
        };
        // The details say who has it, whatever case the login is in.
        assert_eq!(
            place_of(
                &entries[1],
                Some(&issue("OPEN", &ME.to_uppercase(), &["bug", "terminal"]))
            ),
            Some((
                Group::Issues,
                Lane::OnYou,
                Mark::Issue,
                "assigned to you · bug, terminal".into()
            ))
        );
        // Mentioned on someone else's issue: a mention, as before.
        assert_eq!(
            place_of(&entries[1], Some(&issue("OPEN", "ana", &[]))).map(|p| (p.0, p.3)),
            Some((Group::Mentions, "issue · ana mentioned you".into()))
        );
        // Closed before today: nothing is left to do with it.
        assert_eq!(place_of(&entries[0], Some(&issue("CLOSED", ME, &[]))), None);
        assert_eq!(Group::ALL.iter().position(|g| *g == Group::Issues), Some(3));
    }

    #[test]
    fn before_details_an_entry_goes_where_it_was_found() {
        let place_of = |e: &Entry| place(e, None, ME, DAY).map(|p| (p.group, p.lane));
        let entries = gather(
            &[
                note("9", "ci_activity", "CheckSuite", None),
                note("8", "subscribed", "Release", None),
            ],
            &[
                found(Why::ReviewRequested, "o/r", 1, "open", None),
                found(Why::Mentioned, "o/r", 2, "open", Some(false)),
                found(Why::Authored, "o/r", 3, "open", None),
                found(Why::ReviewedToday, "o/r", 4, "open", None),
            ],
        );
        assert_eq!(
            place_of(&entries[0]),
            Some((Group::NeedsReview, Lane::OnYou))
        );
        assert_eq!(place_of(&entries[1]), Some((Group::Mentions, Lane::OnYou)));
        assert_eq!(
            place(&entries[1], None, ME, DAY).unwrap().sub,
            "issue · rik mentioned you"
        );
        assert_eq!(place_of(&entries[2]), Some((Group::Yours, Lane::OnOthers)));
        assert_eq!(place_of(&entries[3]), Some((Group::Done, Lane::OnOthers)));
        assert_eq!(place_of(&entries[4]), Some((Group::Other, Lane::None)));
        assert_eq!(place(&entries[4], None, ME, DAY).unwrap().mark, Mark::Ci);

        // Closed today is done; closed before today is gone.
        let mut closed =
            gather(&[], &[found(Why::ClosedToday, "o/r", 5, "merged", None)]).remove(0);
        closed.closed_at = "2025-11-14T09:00:00Z".into();
        assert_eq!(place_of(&closed), Some((Group::Done, Lane::Done)));
        closed.closed_at = "2025-11-13T09:00:00Z".into();
        assert_eq!(place_of(&closed), None);

        // A read notification that no search found is not kept.
        let mut read = note(
            "7",
            "subscribed",
            "Issue",
            Some("https://api.github.com/repos/o/r/issues/9"),
        );
        read.unread = false;
        assert_eq!(place_of(&gather(&[read], &[]).remove(0)), None);
    }

    fn pull(json: &str) -> Details {
        let mut value: serde_json::Value = serde_json::from_str(
            r#"{"number":34,"title":"t","state":"OPEN","author":{"login":"rik"},"baseRefName":"main","headRefName":"x",
            "mergeable":"MERGEABLE","mergeStateStatus":"CLEAN","reviewDecision":"","additions":487,"deletions":0,
            "updatedAt":"2025-11-14T10:00:00Z"}"#,
        )
        .unwrap();
        let extra: serde_json::Value = serde_json::from_str(json).unwrap();
        for (k, v) in extra.as_object().unwrap() {
            value[k] = v.clone();
        }
        Details::Pull(Box::new(serde_json::from_value(value).unwrap()))
    }

    #[test]
    fn details_decide_the_group() {
        let e = gather(&[], &[found(Why::ReviewRequested, "o/r", 34, "open", None)]).remove(0);
        let at =
            |d: &Details| place(&e, Some(d), ME, DAY).map(|p| (p.group, p.lane, p.mark, p.sub));

        let asked = pull(
            r#"{"reviewRequests":[{"login":"karloaldrete"}],"reviewDecision":"REVIEW_REQUIRED","mergeStateStatus":"BLOCKED"}"#,
        );
        assert_eq!(
            at(&asked),
            Some((Group::NeedsReview, Lane::OnYou, Mark::Pull, "rik".into()))
        );

        let approved = pull(
            r#"{"reviewDecision":"APPROVED","reviews":[{"author":{"login":"KarloAldrete"},"state":"APPROVED","submittedAt":"2025-11-13T10:00:00Z"}],
            "statusCheckRollup":[{"name":"ci","status":"COMPLETED","conclusion":"SUCCESS"}]}"#,
        );
        assert_eq!(
            at(&approved),
            Some((
                Group::Ready,
                Lane::Ready,
                Mark::Merge,
                "approved by you · 1 check passed".into()
            ))
        );

        let mine = pull(
            r#"{"author":{"login":"KarloAldrete"},"reviewRequests":[{"login":"ana"}],"reviewDecision":"REVIEW_REQUIRED"}"#,
        );
        assert_eq!(
            at(&mine),
            Some((
                Group::Yours,
                Lane::OnOthers,
                Mark::Pull,
                "waiting on ana".into()
            ))
        );

        // Asked of a team the user is in: the search says so, the details
        // only name the team.
        let team = pull(
            r#"{"reviewRequests":[{"slug":"core","name":"Core"}],"reviewDecision":"REVIEW_REQUIRED","mergeStateStatus":"BLOCKED"}"#,
        );
        assert_eq!(
            at(&team),
            Some((
                Group::NeedsReview,
                Lane::OnYou,
                Mark::Pull,
                "rik · your team's review is requested".into()
            ))
        );
        // Without the search's word (someone else's team), it doesn't wait on you.
        let other = gather(&[], &[found(Why::ReviewedToday, "o/r", 34, "open", None)]).remove(0);
        assert_eq!(
            place(&other, Some(&team), ME, DAY).map(|p| p.lane),
            Some(Lane::None)
        );

        let mine_conflicts =
            pull(r#"{"author":{"login":"KarloAldrete"},"mergeable":"CONFLICTING"}"#);
        assert_eq!(
            at(&mine_conflicts).map(|p| (p.0, p.1)),
            Some((Group::Yours, Lane::OnYou))
        );

        let changes_today = pull(
            r#"{"reviewDecision":"CHANGES_REQUESTED","latestReviews":[{"author":{"login":"KarloAldrete"},"state":"CHANGES_REQUESTED"}],
            "reviews":[{"author":{"login":"KarloAldrete"},"state":"CHANGES_REQUESTED","submittedAt":"2025-11-14T11:00:00Z"}]}"#,
        );
        assert_eq!(
            at(&changes_today).map(|p| (p.0, p.1)),
            Some((Group::Done, Lane::OnOthers))
        );

        let merged = pull(
            r#"{"state":"MERGED","mergedAt":"2025-11-14T08:00:00Z","closedAt":"2025-11-14T08:00:00Z"}"#,
        );
        assert_eq!(
            at(&merged).map(|p| (p.0, p.1, p.3)),
            Some((Group::Done, Lane::Done, "merged · rik".into()))
        );
        // What the row draws beside the text comes with it.
        let row = place(&e, Some(&approved), ME, DAY).unwrap();
        assert_eq!(row.author, "rik");
        assert_eq!(row.changes, Some((487, 0)));
        assert_eq!(row.checks, Some(pr::CheckState::Passed));
        assert_eq!(place(&e, None, ME, DAY).unwrap().changes, None);
        let merged_before = pull(r#"{"state":"MERGED","mergedAt":"2025-11-12T08:00:00Z"}"#);
        assert_eq!(at(&merged_before), None);
    }

    #[test]
    fn the_queue_counts_lanes_and_sorts_groups() {
        let entries = gather(
            &[],
            &[
                found(Why::ReviewRequested, "o/r", 1, "open", None),
                found(Why::Authored, "o/r", 2, "open", None),
                found(Why::Mentioned, "o/r", 3, "open", Some(false)),
            ],
        );
        let mut older = entries[0].clone();
        older.item.as_mut().unwrap().number = 9;
        older.updated_at = "2025-11-13T10:00:00Z".into();
        let mut all = entries.clone();
        all.push(older);
        let q = build(&all, &HashMap::new(), ME, DAY);
        assert_eq!(
            (q.on_you, q.on_others, q.ready, q.done, q.tracked()),
            (3, 1, 0, 0, 4)
        );
        let order: Vec<u64> = q
            .rows
            .iter()
            .map(|(e, _)| e.item.as_ref().unwrap().number)
            .collect();
        // Waiting longest first within a group.
        assert_eq!(order, [9, 1, 3, 2]);
        assert_eq!(q.group(Group::NeedsReview).count(), 2);
    }

    #[test]
    fn a_failed_search_keeps_what_it_found_last_time() {
        let old = vec![
            found(Why::ReviewRequested, "o/r", 1, "open", Some(true)),
            found(Why::Authored, "o/r", 2, "open", Some(true)),
        ];
        let new = vec![found(Why::Authored, "o/r", 3, "open", Some(true))];
        let merged = merge_found(old.clone(), new.clone(), &[Why::ReviewRequested]);
        // The review request stays; the authored search moved on.
        assert_eq!(merged, vec![old[0].clone(), new[0].clone()]);
        // When every search worked, only the new results count.
        assert_eq!(merge_found(old.clone(), new.clone(), &[]), new);
        // When all failed, nothing changes.
        assert_eq!(merge_found(old.clone(), Vec::new(), &Why::ALL), old);
    }
}
