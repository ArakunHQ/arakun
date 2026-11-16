//! The review pulse: how the user's week of reviews went, from GitHub's own
//! record. A few GraphQL searches read, for every pull request the user was
//! involved in lately, when reviews were asked of whom and who reviewed when.
//! Everything shown is worked out from that; where there is too little, the
//! numbers say so instead of guessing.

use serde_json::Value;

use super::Problem;

const QUERY: &str = "query($q:String!){search(query:$q,type:ISSUE,first:100){nodes{... on PullRequest{\
number state repository{nameWithOwner} author{login} \
timelineItems(itemTypes:[REVIEW_REQUESTED_EVENT,REVIEW_REQUEST_REMOVED_EVENT,PULL_REQUEST_REVIEW],last:100){nodes{__typename \
... on ReviewRequestedEvent{createdAt requestedReviewer{... on User{login}}} \
... on ReviewRequestRemovedEvent{createdAt requestedReviewer{... on User{login}}} \
... on PullRequestReview{submittedAt state author{login}}}}}}}}";

/// What happened on one pull request, as far as reviews go.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Activity {
    pub repo: String,
    pub number: u64,
    pub author: String,
    pub open: bool,
    /// (who, when, true for asked / false for no longer asked), oldest first.
    pub requests: Vec<(String, u64, bool)>,
    /// (who, when), oldest first. Pending reviews are left out.
    pub reviews: Vec<(String, u64)>,
}

fn time(v: &Value, key: &str) -> u64 {
    v.get(key)
        .and_then(Value::as_str)
        .and_then(crate::ci::parse_time)
        .unwrap_or(0)
}

fn login(v: &Value, pointer: &str) -> String {
    v.pointer(pointer)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

/// The answer to `QUERY`, as activities.
pub fn parse(answer: &Value) -> Vec<Activity> {
    let nodes = answer
        .pointer("/data/search/nodes")
        .and_then(Value::as_array);
    let mut out = Vec::new();
    for node in nodes.into_iter().flatten() {
        let Some(number) = node.get("number").and_then(Value::as_u64) else {
            continue;
        };
        let mut a = Activity {
            repo: login(node, "/repository/nameWithOwner"),
            number,
            author: login(node, "/author/login"),
            open: node.get("state").and_then(Value::as_str) == Some("OPEN"),
            ..Default::default()
        };
        let items = node
            .pointer("/timelineItems/nodes")
            .and_then(Value::as_array);
        for item in items.into_iter().flatten() {
            match item.get("__typename").and_then(Value::as_str) {
                Some(kind @ ("ReviewRequestedEvent" | "ReviewRequestRemovedEvent")) => {
                    let who = login(item, "/requestedReviewer/login");
                    if !who.is_empty() {
                        a.requests.push((
                            who,
                            time(item, "createdAt"),
                            kind == "ReviewRequestedEvent",
                        ));
                    }
                }
                Some("PullRequestReview")
                    if item.get("state").and_then(Value::as_str) != Some("PENDING") =>
                {
                    a.reviews
                        .push((login(item, "/author/login"), time(item, "submittedAt")));
                }
                _ => {}
            }
        }
        a.requests.sort_by_key(|r| r.1);
        a.reviews.sort_by_key(|r| r.1);
        out.push(a);
    }
    out
}

/// The searches behind the pulse. `involves:` leaves out pull requests the
/// user was only asked to review or only reviewed, so those are searched too;
/// and a request still open counts however long ago it was last touched.
fn searches(since: &str) -> [String; 3] {
    [
        format!("is:pr involves:@me updated:>={since} archived:false"),
        format!("is:pr reviewed-by:@me updated:>={since} archived:false"),
        "is:pr is:open review-requested:@me archived:false".to_string(),
    ]
}

/// The searches' activities, each pull request once.
fn merge(lists: Vec<Vec<Activity>>) -> Vec<Activity> {
    let mut out: Vec<Activity> = Vec::new();
    for a in lists.into_iter().flatten() {
        let seen = out
            .iter()
            .any(|b| b.number == a.number && b.repo.eq_ignore_ascii_case(&a.repo));
        if !seen {
            out.push(a);
        }
    }
    out
}

fn search(q: &str) -> Result<Vec<Activity>, Problem> {
    let out = super::gh_out(&[
        "api",
        "graphql",
        "-f",
        &format!("query={QUERY}"),
        "-f",
        &format!("q={q}"),
    ])?;
    let value: Value = serde_json::from_slice(&out)
        .map_err(|e| Problem::Other(format!("unexpected answer from gh: {e}")))?;
    Ok(parse(&value))
}

/// Reads the pull requests the user was involved in since `since` (a date,
/// `2026-11-03`), and those whose review still waits on them. One failed
/// search fails the pulse: a part of it would show wrong numbers.
pub fn fetch(since: &str) -> Result<Vec<Activity>, Problem> {
    let qs = searches(since);
    let lists = std::thread::scope(|s| {
        let jobs: Vec<_> = qs.iter().map(|q| s.spawn(move || search(q))).collect();
        jobs.into_iter()
            .map(|j| {
                j.join()
                    .unwrap_or_else(|_| Err(Problem::Other("a search failed".into())))
            })
            .collect::<Result<Vec<_>, _>>()
    })?;
    Ok(merge(lists))
}

/// The week, worked out.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Pulse {
    /// Monday to Sunday: (review requests the user got, reviews the user gave).
    pub days: [(u32, u32); 7],
    /// Average seconds from a request to the user's review, for requests of
    /// this week the user answered. None when there were none.
    pub avg: Option<u64>,
    /// The same for the week before.
    pub prev_avg: Option<u64>,
    /// How many requests of this week went into `avg`.
    pub answered: usize,
    /// Authors waiting on the user's review: (login, seconds of the longest
    /// wait), longest first.
    pub waiting: Vec<(String, u64)>,
}

impl Pulse {
    /// The change from last week's average, in percent: negative is faster.
    /// None without both weeks.
    pub fn change(&self) -> Option<i64> {
        let (now, before) = (self.avg?, self.prev_avg?);
        (before > 0).then(|| ((now as f64 - before as f64) / before as f64 * 100.).round() as i64)
    }
}

fn same(a: &str, b: &str) -> bool {
    !a.is_empty() && a.eq_ignore_ascii_case(b)
}

/// The pulse of `me`'s week. `days` are the starts of Monday … next Monday
/// (local), `prev_start` the start of the Monday before; `now` the time now.
pub fn pulse(acts: &[Activity], me: &str, days: &[u64; 8], prev_start: u64, now: u64) -> Pulse {
    let day_of = |t: u64| (0..7).find(|&d| t >= days[d] && t < days[d + 1]);
    let mut p = Pulse::default();
    let (mut sum, mut prev_sum, mut prev_n) = (0u64, 0u64, 0u64);
    let mut waits: Vec<(String, u64)> = Vec::new();
    for a in acts {
        if same(&a.author, me) {
            continue;
        }
        let mine: Vec<u64> = a
            .reviews
            .iter()
            .filter(|(who, _)| same(who, me))
            .map(|r| r.1)
            .collect();
        for &at in &mine {
            if let Some(d) = day_of(at) {
                p.days[d].1 += 1;
            }
        }
        let asks: Vec<&(String, u64, bool)> = a
            .requests
            .iter()
            .filter(|(who, _, _)| same(who, me))
            .collect();
        for (i, (_, at, asked)) in asks.iter().enumerate() {
            if !asked {
                continue;
            }
            if let Some(d) = day_of(*at) {
                p.days[d].0 += 1;
            }
            // Answered by the first review after it, unless the request was taken back first.
            let withdrawn = asks[i + 1..].iter().find(|r| !r.2).map(|r| r.1);
            let answer = mine
                .iter()
                .copied()
                .find(|&r| r >= *at)
                .filter(|&r| withdrawn.is_none_or(|w| r <= w));
            match answer {
                Some(r) if *at >= days[0] && *at < days[7] => {
                    sum += r - at;
                    p.answered += 1;
                }
                Some(r) if *at >= prev_start && *at < days[0] => {
                    prev_sum += r - at;
                    prev_n += 1;
                }
                None if a.open && withdrawn.is_none() && i + 1 == asks.len() => {
                    let wait = now.saturating_sub(*at);
                    match waits
                        .iter_mut()
                        .find(|(who, _)| who.eq_ignore_ascii_case(&a.author))
                    {
                        Some(w) => w.1 = w.1.max(wait),
                        None => waits.push((a.author.clone(), wait)),
                    }
                }
                _ => {}
            }
        }
    }
    p.avg = (p.answered > 0).then(|| sum / p.answered as u64);
    p.prev_avg = (prev_n > 0).then(|| prev_sum / prev_n);
    waits.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    p.waiting = waits;
    p
}

/// A span as the queue writes it: `1h 10m`, `26m`, `3d 4h`, `now`.
pub fn span(secs: u64) -> String {
    let (m, h, d) = (secs / 60, secs / 3600, secs / 86_400);
    match secs {
        0..60 => "now".into(),
        60..3600 => format!("{m}m"),
        3600..86_400 if m % 60 == 0 => format!("{h}h"),
        3600..86_400 => format!("{h}h {}m", m % 60),
        _ if h % 24 == 0 => format!("{d}d"),
        _ => format!("{d}d {}h", h % 24),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ANSWER: &str = r#"{"data":{"search":{"nodes":[
      {"number":51,"state":"OPEN","repository":{"nameWithOwner":"ArakunHQ/arakun"},"author":{"login":"rik"},
       "timelineItems":{"nodes":[
         {"__typename":"ReviewRequestedEvent","createdAt":"2026-11-10T10:00:00Z","requestedReviewer":{"login":"Karlo"}},
         {"__typename":"ReviewRequestedEvent","createdAt":"2026-11-10T10:00:00Z","requestedReviewer":{"login":"dani"}},
         {"__typename":"PullRequestReview","submittedAt":"2026-11-10T10:30:00Z","state":"CHANGES_REQUESTED","author":{"login":"karlo"}},
         {"__typename":"PullRequestReview","submittedAt":"2026-11-10T11:00:00Z","state":"PENDING","author":{"login":"karlo"}}]}},
      {"number":55,"state":"OPEN","repository":{"nameWithOwner":"ArakunHQ/arakun"},"author":{"login":"rik"},
       "timelineItems":{"nodes":[
         {"__typename":"ReviewRequestedEvent","createdAt":"2026-11-14T10:00:00Z","requestedReviewer":{"login":"karlo"}}]}},
      {"number":56,"state":"OPEN","repository":{"nameWithOwner":"ArakunHQ/arakun"},"author":{"login":"dani"},
       "timelineItems":{"nodes":[
         {"__typename":"ReviewRequestedEvent","createdAt":"2026-11-14T11:00:00Z","requestedReviewer":{"login":"karlo"}}]}},
      {"number":57,"state":"OPEN","repository":{"nameWithOwner":"ArakunHQ/arakun"},"author":{"login":"dani"},
       "timelineItems":{"nodes":[
         {"__typename":"ReviewRequestedEvent","createdAt":"2026-11-13T11:00:00Z","requestedReviewer":{"login":"karlo"}},
         {"__typename":"ReviewRequestRemovedEvent","createdAt":"2026-11-13T12:00:00Z","requestedReviewer":{"login":"karlo"}}]}},
      {"number":40,"state":"MERGED","repository":{"nameWithOwner":"ArakunHQ/arakun"},"author":{"login":"rik"},
       "timelineItems":{"nodes":[
         {"__typename":"ReviewRequestedEvent","createdAt":"2026-11-05T10:00:00Z","requestedReviewer":{"login":"karlo"}},
         {"__typename":"PullRequestReview","submittedAt":"2026-11-05T12:00:00Z","state":"APPROVED","author":{"login":"karlo"}}]}},
      {"number":30,"state":"OPEN","repository":{"nameWithOwner":"k/app"},"author":{"login":"karlo"},
       "timelineItems":{"nodes":[{"__typename":"PullRequestReview","submittedAt":"2026-11-12T10:00:00Z","state":"APPROVED","author":{"login":"rik"}}]}},
      {}
    ]}}}"#;

    fn t(s: &str) -> u64 {
        crate::ci::parse_time(s).unwrap()
    }

    #[test]
    fn the_answer_is_read() {
        let acts = parse(&serde_json::from_str(ANSWER).unwrap());
        assert_eq!(acts.len(), 6);
        assert_eq!(
            (
                acts[0].repo.as_str(),
                acts[0].number,
                acts[0].author.as_str(),
                acts[0].open
            ),
            ("ArakunHQ/arakun", 51, "rik", true)
        );
        assert_eq!(acts[0].requests.len(), 2);
        // The pending review is not one.
        assert_eq!(
            acts[0].reviews,
            [("karlo".to_string(), t("2026-11-10T10:30:00Z"))]
        );
        assert!(!acts[3].requests[1].2);
        assert!(!acts[4].open);
    }

    #[test]
    fn the_week_is_worked_out_from_real_events() {
        let acts = parse(&serde_json::from_str(ANSWER).unwrap());
        // Monday 2026-11-09 … Monday 2026-11-16, in UTC for the test.
        let monday = t("2026-11-09T00:00:00Z");
        let days: [u64; 8] = std::array::from_fn(|i| monday + i as u64 * 86_400);
        let now = t("2026-11-14T12:10:00Z");
        let p = pulse(&acts, "Karlo", &days, monday - 7 * 86_400, now);
        // Tuesday: one request, one review. Friday: one request taken back. Saturday: two requests.
        assert_eq!(
            p.days,
            [(0, 0), (1, 1), (0, 0), (0, 0), (1, 0), (2, 0), (0, 0)]
        );
        assert_eq!((p.avg, p.answered), (Some(30 * 60), 1));
        assert_eq!(p.prev_avg, Some(2 * 3600));
        assert_eq!(p.change(), Some(-75));
        // Both still wait; the withdrawn one doesn't; the user's own pull is not theirs to review.
        assert_eq!(
            p.waiting,
            [
                ("rik".to_string(), 2 * 3600 + 600),
                ("dani".to_string(), 3600 + 600)
            ]
        );
    }

    #[test]
    fn too_little_data_gives_no_numbers() {
        let p = pulse(&[], "karlo", &[0, 1, 2, 3, 4, 5, 6, 7], 0, 10);
        assert_eq!((p.avg, p.prev_avg, p.change()), (None, None, None));
        assert!(p.waiting.is_empty());
    }

    #[test]
    fn spans_read_short() {
        assert_eq!(span(20), "now");
        assert_eq!(span(26 * 60), "26m");
        assert_eq!(span(70 * 60), "1h 10m");
        assert_eq!(span(2 * 3600), "2h");
        assert_eq!(span(3 * 86_400 + 4 * 3600), "3d 4h");
        assert_eq!(span(2 * 86_400), "2d");
    }

    #[test]
    fn the_pulse_also_reads_review_only_and_old_open_requests() {
        let qs = searches("2026-11-03");
        assert_eq!(
            qs[0],
            "is:pr involves:@me updated:>=2026-11-03 archived:false"
        );
        assert_eq!(
            qs[1],
            "is:pr reviewed-by:@me updated:>=2026-11-03 archived:false"
        );
        // No date: a request from three weeks ago still waits.
        assert_eq!(qs[2], "is:pr is:open review-requested:@me archived:false");
        let act = |repo: &str, number| Activity {
            repo: repo.into(),
            number,
            ..Default::default()
        };
        let merged = merge(vec![
            vec![act("o/r", 1), act("o/r", 2)],
            vec![act("O/R", 2), act("o/r", 3)],
            vec![act("o/r", 1)],
        ]);
        assert_eq!(merged, vec![act("o/r", 1), act("o/r", 2), act("o/r", 3)]);
    }
}
