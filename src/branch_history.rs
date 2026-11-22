//! The Branches tab's history: the default branch's commits, newest first,
//! with the commits of other branches put between them as lanes. A branch
//! not in the default one yet hangs from the commit it grew from; a merged
//! one leaves the default line at the commit it grew from and comes back at
//! the commit that took it in (for a squash, the branch's own commits are
//! still on its ref). At most [`LANES`] lanes run side by side; a branch that
//! would need another one folds. Integration branches (see
//! [`integration`]) are left out, and merge commits on a lane fold away.

use std::collections::HashMap;

use crate::branches::{Branch, Commit, Map};

/// Lanes beside the default branch's line, and colors for them.
pub const LANES: usize = 3;
/// A branch's newest commits drawn on its lane; older ones are left out, so
/// a long-lived branch doesn't push the default branch's history away.
pub const LANE_COMMITS: usize = 5;
/// Branches that only gather other branches (a nightly build, a backup):
/// they are neither drawn nor counted.
pub const INTEGRATION_PREFIXES: &[&str] = &["nightly/", "backup/"];

/// Where a row's commit comes from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Source {
    /// `map.main_commits[i]`.
    Main(usize),
    /// `map.branches[branch].commits[commit]`.
    Side { branch: usize, commit: usize },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Lane {
    /// Index into `map.branches`.
    pub branch: usize,
    /// 1..=LANES; the default branch is column 0.
    pub column: usize,
    /// 0..LANES: which lane color it wears.
    pub color: usize,
    /// The row it starts at: the commit that merged it, or its newest commit.
    pub top: usize,
    /// The row of the commit it grew from; None when that is below the rows.
    pub bottom: Option<usize>,
    pub merged: bool,
    /// Index in the branch's commits of the newest one drawn: merges are not.
    pub tip: usize,
}

#[derive(Clone, Debug, Default)]
pub struct History {
    pub rows: Vec<Source>,
    pub lanes: Vec<Lane>,
    /// Branches not in the default one that got no lane, newest first.
    pub folded: Vec<usize>,
}

impl History {
    /// The lane a row's commit is on, if it is a branch's own commit.
    pub fn lane_of(&self, row: usize) -> Option<&Lane> {
        match self.rows.get(row)? {
            Source::Side { branch, .. } => self.lanes.iter().find(|l| l.branch == *branch),
            Source::Main(_) => None,
        }
    }

    /// Is `row` the newest drawn commit of a branch not in main yet?
    pub fn is_tip(&self, row: usize) -> bool {
        match self.rows.get(row) {
            Some(Source::Side { commit, .. }) => self
                .lane_of(row)
                .is_some_and(|l| !l.merged && l.tip == *commit),
            _ => false,
        }
    }

    /// The merged lane that comes back into the default line at `row`.
    pub fn merged_at(&self, row: usize) -> Option<&Lane> {
        self.lanes.iter().find(|l| l.merged && l.top == row)
    }
}

/// Is `b` a branch not in the default one yet (nor merged by a squash)?
pub fn not_in_main(b: &Branch) -> bool {
    !b.ghost && !b.contained && !b.pr.as_ref().is_some_and(|p| p.merged()) && b.ahead > 0
}

/// A branch that gathers others rather than holding work of its own: its
/// name starts with one of [`INTEGRATION_PREFIXES`], or its newest commit
/// is a merge and it has merged at least two branches other than `main`.
pub fn integration(b: &Branch, main: &str) -> bool {
    if INTEGRATION_PREFIXES.iter().any(|p| b.name.starts_with(p)) {
        return true;
    }
    let of_main = |c: &Commit| {
        c.title.contains(&format!("'{main}'")) || c.title.contains(&format!("'origin/{main}'"))
    };
    b.commits.first().is_some_and(|c| c.merge)
        && b.commits.iter().filter(|c| c.merge && !of_main(c)).count() >= 2
}

/// Lays out the history of `map`. `live(b)` says if a branch not in the
/// default one is worth a lane (inactive ones are not).
pub fn layout(map: &Map, live: impl Fn(&Branch) -> bool) -> History {
    let main = &map.main_commits;
    let row_of: HashMap<&str, usize> = main
        .iter()
        .enumerate()
        .map(|(i, c)| (c.sha.as_str(), i))
        .collect();

    // Each candidate: its branch, the main row that merged it, the main row
    // it grew from, and the slot of each of its commits (it goes just above
    // main row `slot`).
    struct Candidate {
        branch: usize,
        merge: Option<usize>,
        fork: Option<usize>,
        /// (index in the branch's commits, slot, time).
        slots: Vec<(usize, usize, u64)>,
    }
    let mut candidates: Vec<Candidate> = Vec::new();
    let mut folded = Vec::new();
    for (bi, b) in map.branches.iter().enumerate() {
        if b.ghost || b.commits.is_empty() || integration(b, &map.main) {
            continue;
        }
        let merge = b.merged_by.as_deref().and_then(|s| row_of.get(s).copied());
        let open = not_in_main(b);
        if merge.is_none() && !(open && live(b)) {
            continue;
        }
        let fork = b.fork.as_deref().and_then(|s| row_of.get(s).copied());
        let lo = merge.map_or(0, |m| m + 1);
        let hi = fork.unwrap_or(main.len());
        if hi < lo {
            continue;
        }
        let mut slots = Vec::with_capacity(LANE_COMMITS);
        let (mut last_slot, mut last_time) = (lo, u64::MAX);
        let own = b.commits.iter().enumerate().filter(|(_, c)| !c.merge);
        for (k, c) in own.take(LANE_COMMITS) {
            // Above the first main commit in range that is older; a branch
            // keeps its own order even when its times don't.
            let slot = (lo..hi)
                .find(|&j| main[j].time < c.time)
                .unwrap_or(hi)
                .max(last_slot);
            let time = c.time.min(last_time);
            slots.push((k, slot, time));
            (last_slot, last_time) = (slot, time);
        }
        if slots.is_empty() {
            // Only merges: nothing to draw, but an open branch stays listed.
            if merge.is_none() {
                folded.push(bi);
            }
            continue;
        }
        candidates.push(Candidate {
            branch: bi,
            merge,
            fork,
            slots,
        });
    }

    // Sort keys: a branch commit in slot s goes above main row s; among
    // themselves, newest first.
    type Key = (usize, u8, std::cmp::Reverse<u64>, usize, usize);
    let main_key = |j: usize| -> Key { (j, 1, std::cmp::Reverse(0), 0, 0) };
    let top_key = |c: &Candidate| -> Key {
        match c.merge {
            Some(m) => main_key(m),
            None => (
                c.slots[0].1,
                0,
                std::cmp::Reverse(c.slots[0].2),
                c.branch,
                c.slots[0].0,
            ),
        }
    };
    candidates.sort_by_key(top_key);

    // Columns, first come first served: a column is free again below the
    // row its last lane went back into the default line.
    let mut columns: [Option<(Key, usize)>; LANES] = [None; LANES];
    let mut next_color = 0;
    let mut kept: Vec<(usize, usize, usize)> = Vec::new();
    for (ci, c) in candidates.iter().enumerate() {
        let top = top_key(c);
        let bottom = c
            .fork
            .map_or((usize::MAX, 0, std::cmp::Reverse(0), 0, 0), main_key);
        for col in columns.iter_mut() {
            if col.is_some_and(|(end, _)| end <= top) {
                *col = None;
            }
        }
        let Some(col) = columns.iter().position(Option::is_none) else {
            if c.merge.is_none() {
                folded.push(c.branch);
            }
            continue;
        };
        let busy: Vec<usize> = columns.iter().flatten().map(|(_, color)| *color).collect();
        let color = (0..LANES)
            .map(|k| (next_color + k) % LANES)
            .find(|k| !busy.contains(k))
            .unwrap_or(0);
        next_color = (color + 1) % LANES;
        columns[col] = Some((bottom, color));
        kept.push((ci, col + 1, color));
    }

    let mut keyed: Vec<(Key, Source)> = (0..main.len())
        .map(|j| (main_key(j), Source::Main(j)))
        .collect();
    for &(ci, ..) in &kept {
        let c = &candidates[ci];
        for &(k, slot, time) in &c.slots {
            keyed.push((
                (slot, 0, std::cmp::Reverse(time), c.branch, k),
                Source::Side {
                    branch: c.branch,
                    commit: k,
                },
            ));
        }
    }
    keyed.sort_by_key(|(k, _)| *k);
    let rows: Vec<Source> = keyed.into_iter().map(|(_, s)| s).collect();
    let main_row: HashMap<usize, usize> = rows
        .iter()
        .enumerate()
        .filter_map(|(r, s)| match s {
            Source::Main(j) => Some((*j, r)),
            Source::Side { .. } => None,
        })
        .collect();

    let lanes = kept
        .into_iter()
        .map(|(ci, column, color)| {
            let c = &candidates[ci];
            let first = rows
                .iter()
                .position(|s| {
                    *s == Source::Side {
                        branch: c.branch,
                        commit: c.slots[0].0,
                    }
                })
                .unwrap_or(0);
            Lane {
                branch: c.branch,
                column,
                color,
                top: c.merge.map_or(first, |m| main_row[&m]),
                bottom: c.fork.map(|f| main_row[&f]),
                merged: c.merge.is_some(),
                tip: c.slots[0].0,
            }
        })
        .collect();
    folded.sort_by_key(|&b| std::cmp::Reverse(map.branches[b].time));
    History {
        rows,
        lanes,
        folded,
    }
}

/// "#677" from a squash title "perf(cloudflare): parse… (#677)", and the
/// title without it.
pub fn split_pr(title: &str) -> (&str, Option<u64>) {
    let t = title.trim_end();
    if let Some(open) = t.rfind(" (#")
        && let Some(n) = t[open + 3..].strip_suffix(')')
        && let Ok(n) = n.parse()
    {
        return (&t[..open], Some(n));
    }
    (t, None)
}

/// The title without its Conventional Commits prefix ("perf(usage): ").
pub fn without_type(title: &str) -> &str {
    let Some((head, rest)) = title.split_once(": ") else {
        return title;
    };
    let kind = head.trim_end_matches('!');
    let kind = match kind.split_once('(') {
        Some((k, scope)) if scope.ends_with(')') => k,
        Some(_) => return title,
        None => kind,
    };
    if !kind.is_empty() && kind.chars().all(|c| c.is_ascii_lowercase()) && !rest.is_empty() {
        rest
    } else {
        title
    }
}

/// "now", "4m", "3h", "2d", "5w", "3mo", "2y": the age column.
pub fn short_ago(t: u64, now: u64) -> String {
    match now.saturating_sub(t) {
        0..60 => "now".into(),
        s @ 60..3600 => format!("{}m", s / 60),
        s @ 3600..86_400 => format!("{}h", s / 3600),
        s @ 86_400..1_209_600 => format!("{}d", s / 86_400),
        s @ 1_209_600..5_184_000 => format!("{}w", s / 604_800),
        s @ 5_184_000..31_536_000 => format!("{}mo", s / 2_592_000),
        s => format!("{}y", s / 31_536_000),
    }
}

/// "just now", "1 minute ago", "49 minutes ago", "3 days ago": the card.
pub fn long_ago(t: u64, now: u64) -> String {
    let s = now.saturating_sub(t);
    let (n, unit) = match s {
        0..60 => return "just now".into(),
        60..3600 => (s / 60, "minute"),
        3600..86_400 => (s / 3600, "hour"),
        86_400..1_209_600 => (s / 86_400, "day"),
        1_209_600..5_184_000 => (s / 604_800, "week"),
        5_184_000..31_536_000 => (s / 2_592_000, "month"),
        _ => (s / 31_536_000, "year"),
    };
    format!("{n} {unit}{} ago", if n == 1 { "" } else { "s" })
}

/// One person behind several author names and emails.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Person {
    /// The fullest name they signed with ("Ana Lima" over "ana").
    pub name: String,
    /// Their GitHub login, when an email or a pull request gives it.
    pub login: Option<String>,
}

/// Who wrote what, by email: a person signs locally as `ana` and on GitHub's
/// squash merges as `Ana Lima`, and both are one person.
#[derive(Clone, Debug, Default)]
pub struct People {
    /// Lower-case email to index in `people`.
    by_email: HashMap<String, usize>,
    people: Vec<Person>,
}

impl People {
    pub fn of(&self, c: &Commit) -> Option<&Person> {
        self.by_email
            .get(&c.email.to_ascii_lowercase())
            .map(|&i| &self.people[i])
    }

    /// The name to show for a commit's author.
    pub fn name<'a>(&'a self, c: &'a Commit) -> &'a str {
        self.of(c).map_or(c.author.as_str(), |p| p.name.as_str())
    }

    pub fn logins(&self) -> Vec<String> {
        let mut out: Vec<String> = self.people.iter().filter_map(|p| p.login.clone()).collect();
        out.sort();
        out.dedup();
        out
    }
}

/// The login in a GitHub noreply email: `123+ana@users.noreply.github.com`
/// or the older `ana@users.noreply.github.com`.
pub fn noreply_login(email: &str) -> Option<String> {
    let local = email
        .to_ascii_lowercase()
        .strip_suffix("@users.noreply.github.com")?
        .to_string();
    let login = local.split_once('+').map_or(local.as_str(), |(_, l)| l);
    (!login.is_empty()).then(|| login.to_string())
}

/// Lower-case letters and digits only: "Ana Lima" → "analima".
fn squash(name: &str) -> String {
    name.chars()
        .filter(char::is_ascii_alphanumeric)
        .map(|c| c.to_ascii_lowercase())
        .collect()
}

/// Could `name` be the person behind `login`? Their login holds the name or
/// the other way round ("karlo", "KarloAldrete"), or they signed with it from
/// that login's noreply email. Keeps a collaborator's commits on someone
/// else's pull request from taking that person's login.
fn fits(name: &str, login: &str, known: &HashMap<String, Vec<String>>) -> bool {
    let (n, l) = (squash(name), squash(login));
    !n.is_empty()
        && (l.contains(&n)
            || n.contains(&l)
            || known.get(login).is_some_and(|names| names.contains(&n)))
}

/// "Ana Lima" over "ana": a name with a space first, then the longer one.
fn fuller(a: &str, b: &str) -> bool {
    (a.contains(' '), a.len()) > (b.contains(' '), b.len())
}

/// Groups the authors of `map` into people. An email belongs to a login when
/// it is that login's noreply address, or when most of its commits are in
/// that login's pull requests (on the PR's branch, or its squash on main)
/// under a name that [`fits`] the login; emails of one login are one person,
/// and an email with no login is one too.
pub fn people(map: &Map) -> People {
    let by_pr: HashMap<u64, &str> = map
        .branches
        .iter()
        .filter_map(|b| b.pr.as_ref())
        .filter(|p| !p.author.is_empty())
        .map(|p| (p.number, p.author.as_str()))
        .collect();
    // The names each login signed with from its noreply email.
    let mut known: HashMap<String, Vec<String>> = HashMap::new();
    let all = map
        .main_commits
        .iter()
        .chain(map.branches.iter().flat_map(|b| b.commits.iter()));
    for c in all {
        if let Some(login) = noreply_login(&c.email) {
            known.entry(login).or_default().push(squash(&c.author));
        }
    }
    let mut sure: HashMap<String, String> = HashMap::new();
    let mut votes: HashMap<String, HashMap<String, usize>> = HashMap::new();
    let mut names: HashMap<String, String> = HashMap::new();
    let mut see = |c: &Commit, pr_author: Option<&str>| {
        let email = c.email.to_ascii_lowercase();
        if email.is_empty() {
            return;
        }
        let name = names.entry(email.clone()).or_default();
        if fuller(&c.author, name) {
            *name = c.author.clone();
        }
        if let Some(login) = noreply_login(&email) {
            sure.insert(email, login);
        } else if let Some(login) = pr_author
            && fits(&c.author, &login.to_ascii_lowercase(), &known)
        {
            *votes
                .entry(email)
                .or_default()
                .entry(login.to_ascii_lowercase())
                .or_default() += 1;
        }
    };
    for c in &map.main_commits {
        let pr = split_pr(&c.title).1.and_then(|n| by_pr.get(&n).copied());
        see(c, pr);
    }
    for b in &map.branches {
        let pr =
            b.pr.as_ref()
                .map(|p| p.author.as_str())
                .filter(|a| !a.is_empty());
        for c in &b.commits {
            see(c, pr);
        }
    }

    let mut out = People::default();
    let mut by_login: HashMap<String, usize> = HashMap::new();
    let mut emails: Vec<&String> = names.keys().collect();
    emails.sort();
    for email in emails {
        let login = sure.get(email).cloned().or_else(|| {
            votes
                .get(email)?
                .iter()
                .max_by_key(|(login, n)| (**n, std::cmp::Reverse((*login).clone())))
                .map(|(login, _)| login.clone())
        });
        let name = &names[email];
        let i = match login
            .as_ref()
            .and_then(|l| by_login.get(&l.to_ascii_lowercase()))
        {
            Some(&i) => {
                if fuller(name, &out.people[i].name) {
                    out.people[i].name = name.clone();
                }
                i
            }
            None => {
                out.people.push(Person {
                    name: name.clone(),
                    login: login.clone(),
                });
                let i = out.people.len() - 1;
                if let Some(l) = login {
                    by_login.insert(l.to_ascii_lowercase(), i);
                }
                i
            }
        };
        out.by_email.insert(email.clone(), i);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::branches::{Commit, Who};

    fn commit(sha: &str, time: u64) -> Commit {
        Commit {
            sha: sha.into(),
            time,
            author: String::new(),
            email: String::new(),
            merge: false,
            title: String::new(),
            who: Who::Person,
            coauthor: None,
            stat: None,
            pushed: true,
        }
    }

    fn branch(name: &str, commits: Vec<Commit>, fork: &str) -> Branch {
        Branch {
            name: name.into(),
            time: commits.first().map_or(0, |c| c.time),
            ahead: commits.len() as u64,
            commits,
            fork: Some(fork.into()),
            ..Default::default()
        }
    }

    fn map(branches: Vec<Branch>) -> Map {
        Map {
            main: "main".into(),
            main_commits: vec![
                commit("m4", 400),
                commit("m3", 300),
                commit("m2", 200),
                commit("m1", 100),
            ],
            branches,
            ..Default::default()
        }
    }

    #[test]
    fn an_open_branch_hangs_above_the_commit_it_grew_from() {
        let m = map(vec![branch("b", vec![commit("b1", 350)], "m3")]);
        let h = layout(&m, |_| true);
        assert_eq!(
            h.rows,
            vec![
                Source::Main(0),
                Source::Side {
                    branch: 0,
                    commit: 0
                },
                Source::Main(1),
                Source::Main(2),
                Source::Main(3),
            ]
        );
        assert_eq!(
            h.lanes,
            vec![Lane {
                branch: 0,
                column: 1,
                color: 0,
                top: 1,
                bottom: Some(2),
                merged: false,
                tip: 0,
            }]
        );
    }

    #[test]
    fn a_new_branch_tip_goes_above_main_even_when_older() {
        // Forked from the newest main commit, but its commit is older: it
        // can't go below the commit it grew from.
        let m = map(vec![branch("b", vec![commit("b1", 50)], "m4")]);
        let h = layout(&m, |_| true);
        assert_eq!(
            h.rows[0],
            Source::Side {
                branch: 0,
                commit: 0
            }
        );
        assert_eq!(h.lanes[0].bottom, Some(1));
    }

    #[test]
    fn a_squashed_branch_leaves_and_comes_back_into_main() {
        let mut b = branch("b", vec![commit("b2", 260), commit("b1", 220)], "m2");
        b.merged_by = Some("m4".into());
        b.contained = false;
        let h = layout(&map(vec![b]), |_| false);
        let side = |commit| Source::Side { branch: 0, commit };
        assert_eq!(
            h.rows,
            vec![
                Source::Main(0),
                Source::Main(1),
                side(0),
                side(1),
                Source::Main(2),
                Source::Main(3),
            ]
        );
        let lane = &h.lanes[0];
        assert_eq!((lane.top, lane.bottom, lane.merged), (0, Some(4), true));
        assert_eq!(h.merged_at(0).map(|l| l.branch), Some(0));
        assert_eq!(h.lane_of(3).map(|l| l.branch), Some(0));
    }

    #[test]
    fn a_fourth_branch_side_by_side_folds() {
        let branches = (0..4)
            .map(|i| {
                branch(
                    &format!("b{i}"),
                    vec![commit(&format!("c{i}"), 500 + i)],
                    "m4",
                )
            })
            .collect();
        let h = layout(&map(branches), |_| true);
        assert_eq!(h.lanes.len(), LANES);
        // The newest three get lanes, each its own column and color.
        let mut got: Vec<(usize, usize, usize)> = h
            .lanes
            .iter()
            .map(|l| (l.branch, l.column, l.color))
            .collect();
        got.sort();
        assert_eq!(got, vec![(1, 3, 2), (2, 2, 1), (3, 1, 0)]);
        assert_eq!(h.folded, vec![0]);
        // The folded branch's commit is not drawn.
        assert!(!h.rows.contains(&Source::Side {
            branch: 0,
            commit: 0
        }));
    }

    #[test]
    fn a_column_is_free_again_below_its_lane() {
        let mut merged = branch("old", vec![commit("o1", 150)], "m1");
        merged.merged_by = Some("m2".into());
        let branches = vec![
            branch("a", vec![commit("a1", 500)], "m4"),
            branch("b", vec![commit("b1", 501)], "m4"),
            branch("c", vec![commit("c1", 502)], "m4"),
            merged,
        ];
        let h = layout(&map(branches), |_| true);
        assert_eq!(h.lanes.len(), 4);
        assert!(h.folded.is_empty());
        let old = h.lanes.iter().find(|l| l.branch == 3).unwrap();
        assert_eq!(old.column, 1);
    }

    #[test]
    fn inactive_and_merged_branches_get_no_lane_of_their_own() {
        let idle = branch("idle", vec![commit("i1", 350)], "m3");
        let mut gone = branch("gone", vec![commit("g1", 350)], "m3");
        gone.merged_by = Some("not-in-view".into());
        gone.pr = None;
        gone.contained = true;
        let h = layout(&map(vec![idle, gone]), |b| b.name != "idle");
        assert!(h.lanes.is_empty());
        assert!(h.folded.is_empty());
        assert_eq!(h.rows.len(), 4);
    }

    #[test]
    fn a_long_branch_draws_only_its_newest_commits() {
        let commits = (0..9).map(|i| commit(&format!("c{i}"), 390 - i)).collect();
        let h = layout(&map(vec![branch("long", commits, "m3")]), |_| true);
        assert_eq!(h.rows.len(), 4 + LANE_COMMITS);
        assert_eq!(h.lanes[0].bottom, Some(1 + LANE_COMMITS));
    }

    #[test]
    fn titles_lose_their_pr_number_and_type() {
        assert_eq!(
            split_pr("perf(cloudflare): parse the config (#677)"),
            ("perf(cloudflare): parse the config", Some(677))
        );
        assert_eq!(split_pr("fix: a (#x)"), ("fix: a (#x)", None));
        assert_eq!(split_pr("plain title"), ("plain title", None));
        assert_eq!(without_type("perf(usage): read it"), "read it");
        assert_eq!(without_type("build: add a profile"), "add a profile");
        assert_eq!(without_type("feat(phone)!: drop v1"), "drop v1");
        assert_eq!(without_type("Merge branch 'x': y"), "Merge branch 'x': y");
        assert_eq!(without_type("no prefix here"), "no prefix here");
    }

    #[test]
    fn ages_read_short_in_rows_and_long_in_the_card() {
        let now = 10_000_000;
        assert_eq!(short_ago(now - 5, now), "now");
        assert_eq!(short_ago(now - 240, now), "4m");
        assert_eq!(short_ago(now - 3 * 3600, now), "3h");
        assert_eq!(short_ago(now - 86_400, now), "1d");
        assert_eq!(short_ago(now - 21 * 86_400, now), "3w");
        assert_eq!(short_ago(now - 90 * 86_400, now), "3mo");
        assert_eq!(short_ago(now + 50, now), "now");
        assert_eq!(long_ago(now - 30, now), "just now");
        assert_eq!(long_ago(now - 60, now), "1 minute ago");
        assert_eq!(long_ago(now - 49 * 60, now), "49 minutes ago");
        assert_eq!(long_ago(now - 2 * 86_400, now), "2 days ago");
    }

    #[test]
    fn integration_branches_get_no_lane_and_merges_fold() {
        let merge = |sha: &str, title: &str, time| Commit {
            merge: true,
            title: title.into(),
            ..commit(sha, time)
        };
        let nightly = branch("nightly/local-1", vec![commit("n1", 350)], "m3");
        let gather = branch(
            "gather",
            vec![
                merge(
                    "g2",
                    "Merge remote-tracking branch 'origin/a' into gather",
                    360,
                ),
                merge(
                    "g1",
                    "Merge remote-tracking branch 'origin/b' into gather",
                    355,
                ),
                commit("g0", 350),
            ],
            "m3",
        );
        assert!(integration(&nightly, "main") && integration(&gather, "main"));
        let feature = branch(
            "feature",
            vec![
                merge("f2", "Merge branch 'main' into feature", 360),
                commit("f1", 350),
            ],
            "m3",
        );
        assert!(!integration(&feature, "main"));
        let h = layout(&map(vec![nightly, gather, feature]), |_| true);
        assert_eq!(h.lanes.len(), 1);
        assert_eq!((h.lanes[0].branch, h.lanes[0].tip), (2, 1));
        // The merge on the lane is folded; its own commit is the tip.
        assert!(!h.rows.contains(&Source::Side {
            branch: 2,
            commit: 0
        }));
        assert!(h.is_tip(1));
    }

    #[test]
    fn one_person_signs_with_several_names_and_emails() {
        let signed = |sha: &str, author: &str, email: &str, title: &str| Commit {
            author: author.into(),
            email: email.into(),
            title: title.into(),
            ..commit(sha, 100)
        };
        let pr = |n: u64, author: &str| crate::branches::Pr {
            number: n,
            title: String::new(),
            url: String::new(),
            state: "OPEN".into(),
            draft: false,
            review: String::new(),
            mergeable: String::new(),
            checks: crate::branches::Checks::None,
            author: author.into(),
            merged_at: 0,
            updated_at: 0,
        };
        let mut m = map(vec![Branch {
            pr: Some(pr(7, "AnaLima")),
            ..branch(
                "ana/fix",
                vec![signed("b1", "ana", "ana@home.test", "fix: x")],
                "m4",
            )
        }]);
        m.main_commits = vec![
            signed(
                "m2",
                "Ana Lima",
                "1+AnaLima@users.noreply.github.com",
                "fix: y (#6)",
            ),
            signed("m1", "Bo", "bo@work.test", "chore: z"),
        ];
        let p = people(&m);
        let local = &m.branches[0].commits[0];
        assert_eq!(p.name(local), "Ana Lima");
        assert_eq!(p.of(local), p.of(&m.main_commits[0]));
        assert_eq!(
            p.of(local).and_then(|x| x.login.as_deref()),
            Some("analima")
        );
        assert_eq!(p.name(&m.main_commits[1]), "Bo");
        assert_eq!(p.of(&m.main_commits[1]).and_then(|x| x.login.clone()), None);
        assert_eq!(p.logins(), vec!["analima".to_string()]);
        // A collaborator's commit on Ana's pull request keeps its own identity.
        m.branches[0]
            .commits
            .push(signed("b0", "Rik Visser", "rik@work.test", "fix: w"));
        let p = people(&m);
        assert_eq!(p.name(&m.branches[0].commits[1]), "Rik Visser");
        assert_eq!(
            p.of(&m.branches[0].commits[1])
                .and_then(|x| x.login.clone()),
            None
        );
        assert_eq!(
            noreply_login("dependabot[bot]@users.noreply.github.com").as_deref(),
            Some("dependabot[bot]")
        );
        assert_eq!(noreply_login("ana@home.test"), None);
    }
}
