//! The Runs tab's front page: the branches on the left, and the chosen one drawn
//! against main with each commit's CI result and the commit that broke it.

use crate::ui::Typography as _;
use std::sync::Arc;

use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, App, BorderStyle, Bounds, Context, FontWeight, Hsla, InteractiveElement as _,
    IntoElement, ParentElement as _, PathBuilder, Pixels, SharedString,
    StatefulInteractiveElement as _, Styled as _, Window, canvas, div, point, px, quad, size,
};

use super::{
    AMBER, GREEN, RED, RunsEvent, RunsView, empty, header_button, hex, soft, state_color,
    state_icon,
};
use crate::ci::graph::{Graph, Lane};
use crate::ci::{Load, Pr, Repo, Review, Run, State};
use crate::model;
use crate::theme;
use crate::ui::{self, icon};

const ROW_H: f32 = 46.;
const MAIN_X: f32 = 16.;
const BRANCH_X: f32 = 38.;
const GUTTER: f32 = 58.;
pub const LANE: u32 = theme::LINK;
/// An orange apart from the red of a failed run.
const CULPRIT: u32 = 0xf88a34;

/// A branch of the list: its name, CI state, last run time and PR number.
type BranchRow = (String, Option<State>, Option<u64>, Option<u64>);

/// What CI says about one row of the graph.
#[derive(Clone)]
struct Node {
    lane: Lane,
    state: Option<State>,
    pushed: bool,
    culprit: bool,
}

/// The commit that broke the branch: the oldest failing commit since the last
/// green one, and how many untested commits sit between them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Culprit {
    pub row: usize,
    pub untested: usize,
    /// A green commit was found below it.
    pub after_green: bool,
}

/// Walks the lane newest to oldest; only a lane whose newest decided result is a
/// failure has a culprit.
pub fn find_culprit(states: &[(Lane, Option<State>)], lane: Lane) -> Option<Culprit> {
    let mut culprit: Option<usize> = None;
    let mut untested = 0;
    for (i, (l, s)) in states.iter().enumerate() {
        if *l != lane {
            continue;
        }
        match s {
            Some(State::Failed) => {
                culprit = Some(i);
                untested = 0;
            }
            Some(State::Passed) => {
                return culprit.map(|row| Culprit {
                    row,
                    untested,
                    after_green: true,
                });
            }
            _ => {
                if culprit.is_some() {
                    untested += 1;
                }
            }
        }
    }
    culprit.map(|row| Culprit {
        row,
        untested,
        after_green: false,
    })
}

impl RunsView {
    /// The branches to pick from: the environments' first, then the ones CI ran on.
    fn graph_branches(&self, repo: &Repo) -> (Vec<String>, Vec<String>) {
        let mut mine: Vec<String> = Vec::new();
        for b in &self.branches {
            if b.as_ref() != "detached HEAD" && !mine.iter().any(|m| m == b.as_ref()) {
                mine.push(b.to_string());
            }
        }
        let mut other: Vec<String> = Vec::new();
        for r in repo.runs.iter().filter(|r| !r.is_deploy()) {
            if !mine.contains(&r.head_branch) && !other.contains(&r.head_branch) && other.len() < 10
            {
                other.push(r.head_branch.clone());
            }
        }
        (mine, other)
    }

    pub(super) fn render_graph_page(&self, cx: &mut Context<Self>) -> AnyElement {
        let Some(root) = self.root.clone() else {
            return empty(
                "activity01",
                "No project",
                "Add a project to see its GitHub Actions runs.",
            );
        };
        let (mine, other) = match self.ci.read(cx).repo(&root) {
            Some(repo) => self.graph_branches(repo),
            None => return empty("activity01", "No project", ""),
        };
        let branch = self
            .graph_branch
            .clone()
            .filter(|b| mine.contains(b) || other.contains(b))
            .or_else(|| self.current_branch.clone().filter(|b| mine.contains(b)))
            .or_else(|| mine.first().cloned())
            .or_else(|| other.first().cloned());
        let Some(branch) = branch else {
            return empty(
                "git-branch",
                "No branches yet",
                "Branches show here once CI runs on them.",
            );
        };
        self.ci
            .update(cx, |ci, cx| ci.load_graph(&root, &branch, cx));
        div()
            .flex()
            .flex_1()
            .min_h_0()
            .gap(px(16.))
            .px(px(24.))
            .pb(px(20.))
            .child(self.render_branch_list(&mine, &other, &branch, cx))
            .child(self.render_graph_card(&root, &branch, cx))
            .into_any_element()
    }

    fn render_branch_list(
        &self,
        mine: &[String],
        other: &[String],
        current: &str,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let ci = self.ci.read(cx);
        let repo = self.root.as_ref().and_then(|r| ci.repo(r));
        let rows: Vec<BranchRow> = mine
            .iter()
            .chain(other)
            .map(|b| {
                let st = repo.and_then(|r| r.branch_state(b));
                let pr = repo.and_then(|r| r.pr_of(b)).map(|p| p.number);
                (
                    b.clone(),
                    st.map(|(s, _)| s),
                    st.map(|(_, r)| r.created()),
                    pr,
                )
            })
            .collect();
        let deploy = repo.and_then(Repo::latest_deploy).cloned();
        let row = |i: usize, (b, state, at, pr): BranchRow, cx: &mut Context<Self>| {
            let on = b == current;
            let pick = b.clone();
            div()
                .id(("graph-branch", i))
                .flex()
                .items_center()
                .gap(px(8.))
                .h(px(30.))
                .px(px(10.))
                .rounded(px(8.))
                .text_size(px(ui::TEXT_SM))
                .cursor_pointer()
                .when(on, |d| d.bg(theme::selection_strong()))
                .when(!on, |d| d.hover(|s| s.bg(theme::hover())))
                .child(match state {
                    Some(s) => div()
                        .size(px(8.))
                        .rounded_full()
                        .flex_none()
                        .bg(state_color(s)),
                    None => div()
                        .size(px(8.))
                        .rounded_full()
                        .flex_none()
                        .border_1()
                        .border_color(theme::mix(0.3)),
                })
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .text_color(if on {
                            theme::content()
                        } else {
                            theme::mix(0.75)
                        })
                        .child(b),
                )
                .when_some(pr, |d, n| {
                    d.child(
                        div()
                            .flex_none()
                            .font_family(theme::FONT_MONO)
                            .text_size(px(ui::TEXT_XS))
                            .text_color(theme::text_subtle())
                            .child(format!("#{n}")),
                    )
                })
                .when_some(at, |d, t| {
                    d.child(
                        div()
                            .flex_none()
                            .text_size(px(ui::TEXT_XS))
                            .text_color(theme::text_faint())
                            .child(model::age(t)),
                    )
                })
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.graph_branch = Some(pick.clone());
                    this.revert_confirm = None;
                    this.notice = None;
                    cx.notify();
                }))
                .into_any_element()
        };
        let heading = |t: &str| {
            div()
                .px(px(10.))
                .pt(px(10.))
                .pb(px(4.))
                .text_size(px(ui::TEXT_XS))
                .text_color(theme::text_subtle())
                .child(t.to_string())
        };
        let n_mine = mine.len();
        let mut list = div()
            .id("graph-branches")
            .flex()
            .flex_col()
            .w(px(240.))
            .flex_none()
            .h_full()
            .overflow_y_scroll();
        if let Some(d) = deploy {
            let (id, state) = (d.id, d.state());
            list = list.child(heading("Production")).child(
                div()
                    .id("graph-deploy")
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .h(px(30.))
                    .px(px(10.))
                    .rounded(px(8.))
                    .text_size(px(ui::TEXT_SM))
                    .cursor_pointer()
                    .hover(|s| s.bg(theme::hover()))
                    .child(icon("cloud-upload", 13.).text_color(state_color(state)))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .child(d.head_branch.clone()),
                    )
                    .child(
                        div()
                            .flex_none()
                            .text_size(px(ui::TEXT_XS))
                            .text_color(theme::text_faint())
                            .child(model::age(d.created())),
                    )
                    .on_click(cx.listener(move |this, _, _, cx| this.select(Some(id), cx))),
            );
        }
        let mut rows = rows.into_iter().enumerate();
        if n_mine > 0 {
            list = list.child(heading("Your environments"));
            for (i, r) in rows.by_ref().take(n_mine) {
                list = list.child(row(i, r, cx));
            }
        }
        let rest: Vec<_> = rows.collect();
        if !rest.is_empty() {
            list = list.child(heading("Other branches"));
            for (i, r) in rest {
                list = list.child(row(i, r, cx));
            }
        }
        list.into_any_element()
    }

    fn render_graph_card(
        &self,
        root: &std::path::Path,
        branch: &str,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let ci = self.ci.read(cx);
        let Some(repo) = ci.repo(root) else {
            return div().into_any_element();
        };
        let graph: Result<Arc<Graph>, Option<SharedString>> = match repo.graph(branch) {
            Some(Load::Ready(g)) => Ok(g.clone()),
            Some(Load::Failed(e)) => Err(Some(e.clone())),
            _ => Err(None),
        };
        let is_main = repo
            .graph(branch)
            .is_some_and(|g| matches!(g, Load::Ready(g) if g.branch == g.main));
        let pr_chip = match repo.pr_of(branch) {
            Some(pr) => Some(pr_chip(pr, branch, self.pr_sessions.get(&pr.url), cx)),
            None if !is_main && matches!(repo.graph(branch), Some(Load::Ready(_))) => {
                let b = branch.to_string();
                Some(
                    header_button("graph-open-pr", "git-pull-request", "Open PR", None)
                        .on_click(cx.listener(move |_, _, _, cx| {
                            cx.emit(RunsEvent::OpenPr {
                                branch: b.clone(),
                                url: None,
                            })
                        }))
                        .into_any_element(),
                )
            }
            None => None,
        };
        let card = div()
            .flex()
            .flex_col()
            .flex_1()
            .min_w_0()
            .max_w(px(760.))
            .h_full()
            .rounded(px(16.))
            .border_1()
            .border_color(theme::border())
            .bg(theme::fill_raised())
            .overflow_hidden()
            .child(
                div()
                    .flex()
                    .flex_none()
                    .items_center()
                    .gap(px(8.))
                    .h(px(44.))
                    .px(px(16.))
                    .border_b_1()
                    .border_color(theme::border_subtle())
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .justify_center()
                            .size(px(24.))
                            .rounded(px(8.))
                            .bg(soft(LANE, 0.16))
                            .child(icon("git-branch", 13.).text_color(hex(LANE))),
                    )
                    .child(div().text_color(theme::text_subtle()).child("Branches"))
                    .child(icon("arrow-right01", 11.).text_color(theme::text_faint()))
                    .child(
                        div()
                            .font_weight(FontWeight::SEMIBOLD)
                            .truncate()
                            .child(branch.to_string()),
                    )
                    .child(div().flex_1())
                    .children(pr_chip),
            );
        let graph = match graph {
            Ok(g) => g,
            Err(None) => {
                return card
                    .child(
                        div()
                            .flex()
                            .flex_1()
                            .items_center()
                            .justify_center()
                            .child(ui::spinner("graph-loading", 16., theme::mix(0.4))),
                    )
                    .into_any_element();
            }
            Err(Some(e)) => {
                return card
                    .child(empty("git-branch", "Couldn't draw this branch", e.as_ref()))
                    .into_any_element();
            }
        };

        let lane = if graph.branch == graph.main {
            Lane::Main
        } else {
            Lane::Branch
        };
        let states: Vec<(Lane, Option<State>)> = graph
            .rows
            .iter()
            .map(|c| (c.lane, repo.commit_state(&c.sha).map(|(s, _)| s)))
            .collect();
        let culprit = find_culprit(&states, lane);
        let runs: Vec<Option<Run>> = graph
            .rows
            .iter()
            .map(|c| repo.commit_state(&c.sha).map(|(_, r)| r.clone()))
            .collect();
        let tags: Vec<Vec<Run>> = graph
            .rows
            .iter()
            .map(|c| repo.deploys_of(&c.sha).cloned().collect())
            .collect();
        let tip = graph.rows.iter().position(|c| c.lane == lane);
        let tip_state = tip.and_then(|i| states[i].1);
        let expecting = repo.expecting(branch);
        // The newest commit on main that a deploy that passed shipped.
        let deployed_row = (lane == Lane::Main)
            .then(|| {
                tags.iter()
                    .position(|t| t.iter().any(|d| d.state() == State::Passed))
            })
            .flatten();
        let undeployed = deployed_row.map(|d| {
            graph.rows[..d]
                .iter()
                .filter(|c| c.lane == Lane::Main)
                .count()
        });
        let unpushed = graph
            .rows
            .iter()
            .filter(|c| c.lane == lane && !c.pushed)
            .count();
        let failing_run = repo
            .branch_state(branch)
            .filter(|(s, _)| *s == State::Failed)
            .map(|(_, r)| r.id);
        let nodes: Vec<Node> = graph
            .rows
            .iter()
            .enumerate()
            .map(|(i, c)| Node {
                lane: c.lane,
                state: states[i].1,
                pushed: c.pushed,
                culprit: culprit.is_some_and(|k| k.row == i),
            })
            .collect();

        let (big, big_color, small): (String, Hsla, String) = match (culprit, tip) {
            (Some(k), _) => (
                graph.rows[k.row].short().to_string(),
                hex(RED),
                match (k.untested, k.after_green) {
                    (0, true) => format!("broke {branch}; the commit before it passed"),
                    (0, false) => format!("broke {branch}"),
                    (n, _) => format!(
                        "broke {branch}, or one of the {n} untested commit{} before it",
                        if n == 1 { "" } else { "s" }
                    ),
                },
            ),
            (None, Some(t)) => match tip_state {
                _ if expecting && !matches!(tip_state, Some(State::Running | State::Queued)) => (
                    "Waiting for CI".into(),
                    hex(AMBER),
                    "Pushed from Arakun; GitHub hasn't started its run yet".into(),
                ),
                Some(State::Passed) if undeployed.is_some_and(|n| n > 0) => {
                    let n = undeployed.unwrap_or(0);
                    let tag = deployed_row
                        .and_then(|d| tags[d].first())
                        .map(|r| r.head_branch.clone())
                        .unwrap_or_default();
                    (
                        format!("{n} commit{}", if n == 1 { "" } else { "s" }),
                        hex(AMBER),
                        format!("on {branch} and green, not in production yet ({tag} is live)"),
                    )
                }
                Some(State::Passed) if undeployed == Some(0) => (
                    "In production".into(),
                    hex(GREEN),
                    format!("{} is what's live", graph.rows[t].short()),
                ),
                Some(State::Passed) => (
                    "All green".into(),
                    theme::content(),
                    format!("{branch} passes CI at {}", graph.rows[t].short()),
                ),
                Some(State::Running | State::Queued) => (
                    "Checking".into(),
                    hex(AMBER),
                    format!("CI is running on {}", graph.rows[t].short()),
                ),
                _ if !graph.rows[t].pushed => (
                    "Not pushed".into(),
                    theme::mix(0.7),
                    "Push the branch to run CI on it".into(),
                ),
                _ => (
                    "No CI yet".into(),
                    theme::mix(0.7),
                    format!("No run has checked {} yet", graph.rows[t].short()),
                ),
            },
            (None, None) => (
                "No commits".into(),
                theme::mix(0.7),
                format!("{branch} has nothing on top of {}", graph.main),
            ),
        };

        let n = graph.rows.len();
        let paint_nodes = nodes.clone();
        let (fork, merge) = (graph.fork, graph.merge);
        let lanes = canvas(
            |_, _, _| {},
            move |bounds: Bounds<Pixels>, _, window: &mut Window, _: &mut App| {
                paint_lanes(bounds, &paint_nodes, fork, merge, window)
            },
        )
        .absolute()
        .top_0()
        .left_0()
        .w(px(GUTTER))
        .h(px(n as f32 * ROW_H));

        let mut rows = div().relative().flex().flex_col().child(lanes);
        for (i, c) in graph.rows.iter().enumerate() {
            let node = &nodes[i];
            let run = runs[i].clone();
            let on_lane = c.lane == lane;
            rows = rows.child(
                div()
                    .id(("graph-row", i))
                    .flex()
                    .items_center()
                    .gap(px(10.))
                    .h(px(ROW_H))
                    .pl(px(GUTTER))
                    .pr(px(16.))
                    .when(run.is_some(), |d| {
                        d.cursor_pointer().hover(|s| s.bg(theme::fill_raised()))
                    })
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .flex_1()
                            .min_w_0()
                            .gap(px(1.))
                            .child(
                                div()
                                    .truncate()
                                    .text_size(px(ui::TEXT_MD))
                                    .when(node.culprit, |d| d.font_weight(FontWeight::SEMIBOLD))
                                    .text_color(if node.culprit {
                                        theme::content()
                                    } else if !c.pushed || !on_lane {
                                        theme::mix(0.6)
                                    } else {
                                        theme::mix(0.85)
                                    })
                                    .child(c.title.clone()),
                            )
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap(px(6.))
                                    .child(
                                        div()
                                            .font_family(theme::FONT_MONO)
                                            .text_size(px(ui::TEXT_XS))
                                            .text_color(if node.culprit {
                                                hex(RED)
                                            } else {
                                                theme::mix(0.4)
                                            })
                                            .child(c.short().to_string()),
                                    )
                                    .when(node.culprit, |d| {
                                        d.child(ui::tag("culprit", hex(CULPRIT)))
                                    })
                                    .when(!c.pushed, |d| {
                                        d.child(ui::tag("not pushed", hex(0x9198a1)))
                                    })
                                    .when(
                                        c.lane == Lane::Main
                                            && deployed_row.is_some_and(|d| i < d)
                                            && on_lane,
                                        |d| d.child(ui::tag("not deployed", hex(AMBER))),
                                    )
                                    .when(run.as_ref().is_some_and(Run::passed_on_retry), |d| {
                                        d.child(ui::tag("flaky", hex(AMBER)))
                                    })
                                    .children(tags[i].iter().take(2).map(|t| {
                                        div()
                                            .flex()
                                            .items_center()
                                            .gap(px(4.))
                                            .px(px(6.))
                                            .h(px(16.))
                                            .rounded(px(4.))
                                            .bg(theme::fill_hover())
                                            .text_size(px(ui::TEXT_XS))
                                            .text_color(theme::text_muted())
                                            .child(
                                                icon("cloud-upload", 10.)
                                                    .text_color(state_color(t.state())),
                                            )
                                            .child(t.head_branch.clone())
                                    }))
                                    .when_some(run.as_ref(), |d, r| {
                                        d.child(
                                            div()
                                                .text_size(px(ui::TEXT_XS))
                                                .text_color(theme::text_faint())
                                                .child(format!(
                                                    "#{} · {}",
                                                    r.number, r.workflow_name
                                                )),
                                        )
                                    }),
                            ),
                    )
                    .child(
                        div()
                            .flex_none()
                            .text_size(px(ui::TEXT_XS))
                            .text_color(theme::text_faint())
                            .child(model::age(c.time)),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_none()
                            .items_center()
                            .justify_center()
                            .size(px(18.))
                            .child(match node.state {
                                Some(s) => state_icon(("graph-state", i), s, 15.),
                                None => div()
                                    .size(px(12.))
                                    .rounded_full()
                                    .border_1()
                                    .border_color(theme::border_strong())
                                    .into_any_element(),
                            }),
                    )
                    .when_some(run.map(|r| r.id), |d, id| {
                        d.on_click(cx.listener(move |this, _, _, cx| this.select(Some(id), cx)))
                    }),
            );
        }

        let revert = culprit
            .filter(|_| lane == Lane::Branch && self.branches.iter().any(|b| b.as_ref() == branch))
            .map(|k| graph.rows[k.row].sha.clone());
        let confirm = revert
            .as_ref()
            .is_some_and(|s| self.revert_confirm.as_ref() == Some(s));
        let footer = div()
            .flex()
            .flex_none()
            .items_center()
            .gap(px(12.))
            .h(px(44.))
            .px(px(16.))
            .border_t_1()
            .border_color(theme::border_subtle())
            .font_family(theme::FONT_MONO)
            .text_size(px(ui::TEXT_XS))
            .text_color(theme::text_subtle())
            .child(legend(theme::mix(0.35), graph.main.clone()))
            .when(lane == Lane::Branch, |d| {
                d.child(legend(hex(LANE), branch.to_string()))
            })
            .child(div().flex_1())
            .when_some(self.notice.clone(), |d, (text, error)| {
                d.child(
                    div()
                        .min_w_0()
                        .truncate()
                        .font_family(theme::FONT_SANS)
                        .text_color(if error { hex(RED) } else { hex(GREEN) })
                        .child(text),
                )
            })
            .when(
                unpushed > 0 && self.branches.iter().any(|b| b.as_ref() == branch),
                |d| {
                    let b = branch.to_string();
                    d.child(
                        header_button(
                            "graph-push",
                            "cloud-upload",
                            format!(
                                "Push {unpushed} commit{}",
                                if unpushed == 1 { "" } else { "s" }
                            ),
                            None,
                        )
                        .font_family(theme::FONT_SANS)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.revert_confirm = None;
                            cx.emit(RunsEvent::Push { branch: b.clone() });
                        })),
                    )
                },
            )
            .when_some(failing_run, |d, id| {
                d.child(
                    header_button("graph-fix", "sparkles", "Fix with Droid", None)
                        .font_family(theme::FONT_SANS)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            if let Some(root) = this.root.clone() {
                                cx.emit(RunsEvent::Fix { root, run: id });
                            }
                        })),
                )
            })
            .when_some(revert, |d, sha| {
                let short = sha.get(..7).unwrap_or(&sha).to_string();
                let b = branch.to_string();
                d.child(
                    div()
                        .id("graph-revert")
                        .flex()
                        .items_center()
                        .gap(px(6.))
                        .h(px(26.))
                        .px(px(8.))
                        .rounded(px(8.))
                        .font_family(theme::FONT_SANS)
                        .text_size(px(ui::TEXT_SM))
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(hex(CULPRIT))
                        .when(confirm, |d| d.bg(soft(CULPRIT, 0.14)))
                        .cursor_pointer()
                        .hover(|s| s.bg(soft(CULPRIT, 0.12)))
                        .child(icon("undo", 13.).text_color(hex(CULPRIT)))
                        .child(if confirm {
                            format!("Revert {short} in {b}?")
                        } else {
                            format!("Revert {short}")
                        })
                        .on_click(cx.listener(move |this, _, _, cx| {
                            if this.revert_confirm.as_ref() == Some(&sha) {
                                this.revert_confirm = None;
                                cx.emit(RunsEvent::Revert {
                                    branch: b.clone(),
                                    sha: sha.clone(),
                                });
                            } else {
                                this.revert_confirm = Some(sha.clone());
                            }
                            cx.notify();
                        })),
                )
            });

        card.child(
            div()
                .id("graph-body")
                .flex()
                .flex_col()
                .flex_1()
                .min_h_0()
                .overflow_y_scroll()
                .child(
                    div()
                        .flex()
                        .items_baseline()
                        .gap(px(10.))
                        .px(px(16.))
                        .pt(px(16.))
                        .pb(px(10.))
                        .child(
                            div()
                                .ty(ui::Type::Title)
                                .font_family(theme::FONT_MONO)
                                .font_weight(FontWeight::MEDIUM)
                                .text_color(big_color)
                                .child(big),
                        )
                        .child(
                            div()
                                .min_w_0()
                                .text_size(px(ui::TEXT_SM))
                                .text_color(theme::text_subtle())
                                .child(small),
                        ),
                )
                .child(div().pb(px(8.)).child(rows)),
        )
        .child(footer)
        .into_any_element()
    }
}

/// The branch's pull request: its number, review and conflicts; opens on GitHub.
fn pr_chip(
    pr: &Pr,
    branch: &str,
    session: Option<&super::LinkedSession>,
    cx: &Context<RunsView>,
) -> AnyElement {
    let (label, color): (_, Hsla) = match pr.review() {
        Review::Draft => ("Draft", theme::pr_draft().into()),
        Review::Approved => ("Approved", hex(GREEN)),
        Review::ChangesRequested => ("Changes requested", hex(RED)),
        Review::Required => ("Review required", hex(AMBER)),
        Review::None => ("Open", theme::pr_open().into()),
    };
    let (pr_icon, pr_color) = if pr.is_draft {
        ("git-pull-request-draft", theme::pr_draft())
    } else {
        ("git-pull-request", theme::pr_open())
    };
    let (b, url) = (branch.to_string(), pr.url.clone());
    div()
        .id("graph-pr")
        .flex()
        .flex_none()
        .items_center()
        .gap(px(6.))
        .h(px(26.))
        .px(px(8.))
        .rounded(px(8.))
        .border_1()
        .border_color(theme::border())
        .text_size(px(ui::TEXT_SM))
        .cursor_pointer()
        .hover(|s| s.bg(theme::fill_hover()))
        .child(icon(pr_icon, 13.).text_color(pr_color))
        .child(
            div()
                .font_family(theme::FONT_MONO)
                .text_color(theme::text_subtle())
                .child(format!("#{}", pr.number)),
        )
        .child(div().text_color(color).child(label))
        .when(pr.conflicts(), |d| d.child(ui::tag("conflicts", hex(RED))))
        .on_click(cx.listener(move |_, _, _, cx| {
            cx.emit(RunsEvent::OpenPr {
                branch: b.clone(),
                url: Some(url.clone()),
            })
        }))
        .when_some(session.cloned(), |d, s| {
            let title = s.title.clone();
            d.child(
                div()
                    .id("graph-pr-session")
                    .flex()
                    .items_center()
                    .gap(px(4.))
                    .pl(px(6.))
                    .ml(px(2.))
                    .border_l_1()
                    .border_color(theme::border())
                    .max_w(px(180.))
                    .text_color(theme::text_subtle())
                    .hover(|st| st.text_color(theme::content()))
                    .child(icon("chatting01", 12.).text_color(theme::text_subtle()))
                    .child(div().min_w_0().truncate().child(title))
                    .on_click(cx.listener(move |_, _, _, cx| {
                        cx.stop_propagation();
                        cx.emit(RunsEvent::OpenSession {
                            project: s.project,
                            env: s.env,
                            id: s.id.clone(),
                        })
                    })),
            )
        })
        .into_any_element()
}

fn legend(color: Hsla, name: String) -> gpui::Div {
    div()
        .flex()
        .items_center()
        .gap(px(6.))
        .child(div().w(px(12.)).h(px(2.)).rounded(px(4.)).bg(color))
        .child(name)
}

fn lane_x(lane: Lane) -> f32 {
    match lane {
        Lane::Main => MAIN_X,
        Lane::Branch => BRANCH_X,
    }
}

/// The lines of main and the branch, the curves where the branch leaves and
/// rejoins main, then a ring per commit on top.
fn paint_lanes(
    bounds: Bounds<Pixels>,
    nodes: &[Node],
    fork: Option<usize>,
    merge: Option<usize>,
    window: &mut Window,
) {
    let o = bounds.origin;
    let at = |x: f32, row: usize| point(o.x + px(x), o.y + px(row as f32 * ROW_H + ROW_H / 2.));
    let main_color = theme::mix(0.28);
    let lane_color: Hsla = hex(LANE);
    let rows_of = |lane: Lane| {
        nodes
            .iter()
            .enumerate()
            .filter(move |(_, n)| n.lane == lane)
            .map(|(i, _)| i)
    };
    let stroke = |from: gpui::Point<Pixels>,
                  to: gpui::Point<Pixels>,
                  curve: bool,
                  color: Hsla,
                  window: &mut Window| {
        let mut p = PathBuilder::stroke(px(1.5));
        p.move_to(from);
        if curve {
            let dy = (to.y - from.y) * 0.55;
            p.cubic_bezier_to(to, point(from.x, from.y + dy), point(to.x, to.y - dy));
        } else {
            p.line_to(to);
        }
        if let Ok(path) = p.build() {
            window.paint_path(path, color);
        }
    };
    if let (Some(first), Some(last)) = (rows_of(Lane::Main).next(), rows_of(Lane::Main).next_back())
        && first != last
    {
        stroke(
            at(MAIN_X, first),
            at(MAIN_X, last),
            false,
            main_color,
            window,
        );
    }
    if let (Some(top), Some(bottom)) = (
        rows_of(Lane::Branch).next(),
        rows_of(Lane::Branch).next_back(),
    ) {
        if top != bottom {
            stroke(
                at(BRANCH_X, top),
                at(BRANCH_X, bottom),
                false,
                lane_color,
                window,
            );
        }
        if let Some(f) = fork {
            stroke(
                at(BRANCH_X, bottom),
                at(MAIN_X, f),
                true,
                lane_color,
                window,
            );
        }
        if let Some(m) = merge {
            stroke(at(BRANCH_X, top), at(MAIN_X, m), true, lane_color, window);
        }
    }
    for (i, n) in nodes.iter().enumerate() {
        let c = at(lane_x(n.lane), i);
        let ring = match n.state {
            Some(State::Failed) => hex(RED),
            _ if n.lane == Lane::Branch => lane_color,
            _ => theme::mix(0.45),
        };
        if n.culprit {
            let r = 10.;
            window.paint_quad(quad(
                Bounds::new(
                    point(c.x - px(r), c.y - px(r)),
                    size(px(r * 2.), px(r * 2.)),
                ),
                px(r),
                soft(RED, 0.22),
                px(0.),
                gpui::transparent_black(),
                BorderStyle::Solid,
            ));
        }
        let r = 5.5;
        window.paint_quad(quad(
            Bounds::new(
                point(c.x - px(r), c.y - px(r)),
                size(px(r * 2.), px(r * 2.)),
            ),
            px(r),
            theme::background(),
            px(1.75),
            ring,
            if n.pushed {
                BorderStyle::Solid
            } else {
                BorderStyle::Dashed
            },
        ));
        if n.state == Some(State::Failed) {
            let d = 2.25;
            window.paint_quad(quad(
                Bounds::new(
                    point(c.x - px(d), c.y - px(d)),
                    size(px(d * 2.), px(d * 2.)),
                ),
                px(d),
                hex(RED),
                px(0.),
                gpui::transparent_black(),
                BorderStyle::Solid,
            ));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const B: Lane = Lane::Branch;
    const M: Lane = Lane::Main;

    #[test]
    fn the_culprit_is_the_oldest_failure_since_green() {
        let f = Some(State::Failed);
        let p = Some(State::Passed);
        // merge (main, failed), two failing branch commits, an untested one, then green.
        let rows = [(M, f), (B, f), (B, None), (B, f), (B, None), (B, p), (M, p)];
        assert_eq!(
            find_culprit(&rows, B),
            Some(Culprit {
                row: 3,
                untested: 1,
                after_green: true
            })
        );
        // Green at the tip: nothing to blame.
        assert_eq!(find_culprit(&[(B, p), (B, f)], B), None);
        // Running at the tip doesn't hide an older failure.
        assert_eq!(
            find_culprit(&[(B, Some(State::Running)), (B, f)], B),
            Some(Culprit {
                row: 1,
                untested: 0,
                after_green: false
            })
        );
        assert_eq!(
            find_culprit(&[(M, f), (M, p)], M),
            Some(Culprit {
                row: 0,
                untested: 0,
                after_green: true
            })
        );
        assert_eq!(find_culprit(&[(B, None)], B), None);
    }
}
