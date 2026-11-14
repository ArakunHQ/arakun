//! The Runs tab: the GitHub Actions runs of the visible project (CI on every
//! branch, and deploys), and a run's page with its steps, logs, annotations and
//! artifacts, from where a failure goes to an agent.

mod detail;
mod graph;

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, App, Context, ElementId, Entity, EventEmitter, FocusHandle, Focusable, FontWeight, Hsla,
    InteractiveElement as _, IntoElement, KeyDownEvent, ParentElement as _, Render, ScrollHandle, SharedString,
    StatefulInteractiveElement as _, Styled as _, Subscription, UniformListScrollHandle, Window, div, px, rgb, rgba,
};

use crate::ci::{self, Ci, Load, Run, State};
use crate::model::{self, Agent, Status};
use crate::theme;
use crate::ui::{self, icon};

/// An arakun session linked to something it was started from (a run it fixes, a task).
#[derive(Clone)]
pub struct LinkedSession {
    pub project: usize,
    pub env: usize,
    pub id: SharedString,
    pub title: SharedString,
    pub agent: Agent,
    pub status: Status,
    /// Unix seconds.
    pub updated: u64,
}

pub const GREEN: u32 = theme::SUCCESS;
pub const RED: u32 = theme::ERROR;
pub const AMBER: u32 = theme::ATTENTION;

pub enum RunsEvent {
    /// Hand the failed run to an agent.
    Fix { root: PathBuf, run: u64 },
    OpenSession { project: usize, env: usize, id: SharedString },
    /// A file of the repository, relative to its root, at a line.
    OpenFile { branch: String, path: String },
    /// Revert a commit in the environment of `branch`.
    Revert { branch: String, sha: String },
    /// Push the environment on `branch` and follow its CI.
    Push { branch: String },
    /// Open the branch's pull request (`url`), or GitHub's page to open one.
    OpenPr { branch: String, url: Option<String> },
    /// Have a fix session commit and push, and follow its CI.
    Ship { project: usize, env: usize, id: SharedString, branch: String },
    /// Send a newer failure to the session that fixed an older one.
    SendFailure { project: usize, env: usize, id: SharedString, run: u64 },
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    Graph,
    List,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Filter {
    All,
    Failed,
    Deploys,
    Mine,
}

impl Filter {
    const ALL: [Filter; 4] = [Filter::All, Filter::Mine, Filter::Failed, Filter::Deploys];

    fn label(self) -> &'static str {
        match self {
            Self::All => "All",
            Self::Mine => "My branches",
            Self::Failed => "Failed",
            Self::Deploys => "Deploys",
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Page {
    Steps,
    Logs,
    Annotations,
    Artifacts,
}

pub struct RunsView {
    ci: Entity<Ci>,
    root: Option<PathBuf>,
    project_name: SharedString,
    /// Branches checked out in the project's environments.
    branches: Vec<SharedString>,
    /// The branch of the visible environment.
    current_branch: Option<String>,
    mode: Mode,
    /// The branch drawn on the graph, once picked.
    graph_branch: Option<String>,
    /// The commit whose Revert was clicked once, waiting for the second click.
    revert_confirm: Option<String>,
    /// The outcome of the last action of the graph (text, is an error).
    notice: Option<(SharedString, bool)>,
    selected: Option<u64>,
    page: Page,
    log_job: Option<u64>,
    filter: Filter,
    /// Jobs whose steps are unfolded on the Steps page.
    expanded: HashSet<u64>,
    /// Run id → the sessions started to fix it.
    sessions: HashMap<u64, Vec<LinkedSession>>,
    /// Pull request URL → the session that opened it.
    pr_sessions: HashMap<String, LinkedSession>,
    list_scroll: ScrollHandle,
    detail_scroll: ScrollHandle,
    log_scroll: UniformListScrollHandle,
    focus: FocusHandle,
    _sub: Subscription,
}

impl EventEmitter<RunsEvent> for RunsView {}

pub fn hex(c: u32) -> Hsla {
    rgb(c).into()
}

pub fn soft(c: u32, alpha: f32) -> Hsla {
    rgba((c << 8) | (alpha.clamp(0., 1.) * 255.).round() as u32).into()
}

pub fn state_color(s: State) -> Hsla {
    match s {
        State::Passed => hex(GREEN),
        State::Failed => hex(RED),
        State::Running | State::Queued => hex(AMBER),
        State::Cancelled | State::Skipped => theme::mix(0.35),
    }
}

/// The run's status glyph; work in progress spins.
pub fn state_icon(id: impl Into<ElementId>, s: State, size: f32) -> AnyElement {
    match s {
        State::Running => ui::spinner(id, size, hex(AMBER)).into_any_element(),
        State::Passed => icon("checkmark-circle01", size).text_color(hex(GREEN)).into_any_element(),
        State::Failed => icon("cancel-circle", size).text_color(hex(RED)).into_any_element(),
        State::Queued => icon("clock01", size).text_color(hex(AMBER)).into_any_element(),
        State::Cancelled => icon("minus-sign", size).text_color(theme::text_faint()).into_any_element(),
        State::Skipped => icon("circle-dashed", size).text_color(theme::text_faint()).into_any_element(),
    }
}

pub fn mono_chip(text: impl Into<SharedString>) -> gpui::Div {
    div()
        .flex_none()
        .px(px(6.))
        .rounded(px(ui::RADIUS_SM))
        .bg(theme::fill_hover())
        .font_family(theme::FONT_MONO)
        .text_size(px(ui::TEXT_XS))
        .text_color(theme::text_muted())
        .child(text.into())
}

/// A small bordered button of the run header.
pub fn header_button(id: impl Into<ElementId>, icon_name: &str, label: impl Into<SharedString>, key: Option<&str>) -> gpui::Stateful<gpui::Div> {
    ui::button_with(id, Some(icon_name), label, ui::ButtonKind::Outline, true).when_some(key, |d, k| d.child(keycap(k)))
}

pub fn keycap(k: &str) -> gpui::Div {
    div()
        .flex()
        .items_center()
        .justify_center()
        .min_w(px(16.))
        .h(px(16.))
        .px(px(4.))
        .rounded(px(ui::RADIUS_SM))
        .bg(theme::fill_selected())
        .text_size(px(ui::TEXT_XS))
        .text_color(theme::text_subtle())
        .child(k.to_string())
}

impl RunsView {
    pub fn new(ci: Entity<Ci>, cx: &mut Context<Self>) -> Self {
        let sub = cx.observe(&ci, |_, _, cx| cx.notify());
        Self {
            ci,
            root: None,
            project_name: SharedString::default(),
            branches: Vec::new(),
            current_branch: None,
            mode: Mode::Graph,
            graph_branch: None,
            revert_confirm: None,
            notice: None,
            selected: None,
            page: Page::Steps,
            log_job: None,
            filter: Filter::All,
            expanded: HashSet::new(),
            sessions: HashMap::new(),
            pr_sessions: HashMap::new(),
            list_scroll: ScrollHandle::new(),
            detail_scroll: ScrollHandle::new(),
            log_scroll: UniformListScrollHandle::new(),
            focus: cx.focus_handle(),
            _sub: sub,
        }
    }

    /// The project the tab shows, and the branches of its environments.
    pub fn set_project(
        &mut self,
        root: Option<PathBuf>,
        name: SharedString,
        branches: Vec<SharedString>,
        current: Option<String>,
    ) {
        if self.root != root {
            self.root = root;
            self.selected = None;
            self.expanded.clear();
            self.graph_branch = None;
            self.revert_confirm = None;
            self.notice = None;
        }
        if self.current_branch != current {
            // Switching environments shows the new one's branch.
            self.graph_branch = None;
        }
        self.project_name = name;
        self.branches = branches;
        self.current_branch = current;
    }

    /// The outcome of an action started from the graph.
    pub fn set_notice(&mut self, text: impl Into<SharedString>, error: bool, cx: &mut Context<Self>) {
        self.notice = Some((text.into(), error));
        cx.notify();
    }

    pub fn set_sessions(&mut self, sessions: HashMap<u64, Vec<LinkedSession>>, prs: HashMap<String, LinkedSession>) {
        self.sessions = sessions;
        self.pr_sessions = prs;
    }

    pub fn focus(&self, window: &mut Window, cx: &mut App) {
        window.focus(&self.focus, cx);
    }

    /// Shows a run's page (None: the list).
    pub fn select(&mut self, id: Option<u64>, cx: &mut Context<Self>) {
        if self.selected != id {
            self.page = Page::Steps;
            self.log_job = None;
            self.detail_scroll = ScrollHandle::new();
        }
        self.selected = id;
        if let (Some(root), Some(id)) = (self.root.clone(), id) {
            self.ci.update(cx, |ci, cx| ci.load_detail(&root, id, cx));
        }
        cx.notify();
    }

    fn refresh(&mut self, cx: &mut Context<Self>) {
        if let Some(root) = self.root.clone() {
            self.ci.update(cx, |ci, cx| ci.refresh(&root, cx));
        }
    }

    fn on_key(&mut self, ev: &KeyDownEvent, _: &mut Window, cx: &mut Context<Self>) {
        let k = &ev.keystroke;
        if k.modifiers.control || k.modifiers.alt || k.modifiers.platform {
            return;
        }
        match k.key.as_str() {
            "escape" if self.selected.is_some() => self.select(None, cx),
            "r" if self.selected.is_some() => self.act(false, cx),
            "f" if self.selected.is_some() => self.fix(cx),
            _ => return,
        }
        cx.stop_propagation();
    }

    fn act(&mut self, cancel: bool, cx: &mut Context<Self>) {
        let (Some(root), Some(id)) = (self.root.clone(), self.selected) else {
            return;
        };
        self.ci.update(cx, |ci, cx| ci.act(&root, id, cancel, cx));
    }

    fn fix(&mut self, cx: &mut Context<Self>) {
        let (Some(root), Some(id)) = (self.root.clone(), self.selected) else {
            return;
        };
        let failed = self.ci.read(cx).repo(&root).and_then(|r| r.run(id)).is_some_and(|r| r.state() == State::Failed);
        if failed {
            cx.emit(RunsEvent::Fix { root, run: id });
        }
    }

    fn shown_runs<'a>(&self, runs: &'a [Run]) -> Vec<&'a Run> {
        runs.iter()
            .filter(|r| match self.filter {
                Filter::All => true,
                Filter::Failed => r.state() == State::Failed,
                Filter::Deploys => r.is_deploy(),
                Filter::Mine => self.branches.iter().any(|b| b.as_ref() == r.head_branch),
            })
            .collect()
    }

    fn render_list(&self, cx: &mut Context<Self>) -> AnyElement {
        let ci = self.ci.read(cx);
        let repo = self.root.as_ref().and_then(|r| ci.repo(r));
        let busy = repo.is_some_and(|r| r.busy);
        let list = match repo {
            _ if Ci::disabled() => List::Off,
            None => List::NoProject,
            Some(r) if r.not_github => List::NotGitHub,
            Some(r) if !r.loaded => List::Loading(r.error.clone()),
            Some(r) => {
                let mut overview = Vec::new();
                if let Some(d) = r.latest_deploy() {
                    overview.push(("cloud-upload", "Production", d.head_branch.clone(), d.clone()));
                }
                let mut seen = HashSet::new();
                for b in &self.branches {
                    if seen.insert(b.clone())
                        && let Some((_, run)) = r.branch_state(b)
                    {
                        overview.push(("git-branch", "Branch", b.to_string(), run.clone()));
                    }
                }
                List::Ready {
                    runs: self.shown_runs(&r.runs).into_iter().cloned().collect(),
                    error: r.error.clone(),
                    overview,
                }
            }
        };
        let seg = ui::segmented().children(Filter::ALL.into_iter().enumerate().map(|(i, f)| {
            ui::segment(("runs-filter", i), None, f.label(), f == self.filter).on_click(cx.listener(move |this, _, _, cx| {
                this.filter = f;
                cx.notify();
            }))
        }));
        let modes = ui::segmented().children(
            [(Mode::Graph, "git-branch", "Graph"), (Mode::List, "left-to-right-list-bullet", "List")].into_iter().map(
                |(m, icon_name, label)| {
                    ui::segment(SharedString::from(format!("runs-mode-{label}")), Some(icon_name), label, m == self.mode)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.mode = m;
                            cx.notify();
                        }))
                },
            ),
        );
        let header = div()
            .flex()
            .flex_none()
            .items_start()
            .gap(px(12.))
            .px(px(ui::PAGE_PAD))
            .pt(px(ui::PAGE_PAD))
            .pb(px(16.))
            .child(
                ui::page_title("Runs", Some(format!("{} · GitHub Actions", self.project_name).into_any_element()))
                    .flex_1(),
            )
            .child(
                div()
                    .flex()
                    .flex_none()
                    .items_center()
                    .gap(px(8.))
                    .h(px(ui::BUTTON_H))
                    .when(self.mode == Mode::List, |d| d.child(seg))
                    .child(modes)
                    .child(ui::refresh_control("runs-refresh", busy, None, cx.listener(|this, _, _, cx| this.refresh(cx)))),
            );

        let body: AnyElement = match list {
            List::Off => empty("activity01", "Runs are turned off", "ARAKUN_CI=off is set."),
            List::NoProject => empty("activity01", "No project", "Add a project to see its GitHub Actions runs."),
            List::NotGitHub => empty(
                "git-branch",
                "Not on GitHub",
                "Runs show for repositories with a GitHub remote.",
            ),
            List::Loading(error) => match error {
                Some(e) => empty("alert-circle", "Couldn't read the runs", e.as_ref()),
                None => div()
                    .flex()
                    .flex_1()
                    .items_center()
                    .justify_center()
                    .child(ui::spinner("runs-loading", 18., theme::mix(0.4)))
                    .into_any_element(),
            },
            List::Ready { .. } if self.mode == Mode::Graph => self.render_graph_page(cx),
            List::Ready { runs, error, overview } => {
                let mut list = div()
                    .id("runs-list")
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .track_scroll(&self.list_scroll)
                    .px(px(16.))
                    .pb(px(24.))
                    .child(self.render_overview(overview, cx));
                if let Some(e) = error {
                    list = list.child(
                        div()
                            .mx(px(8.))
                            .mb(px(8.))
                            .text_size(px(ui::TEXT_SM))
                            .text_color(hex(RED))
                            .child(e.clone()),
                    );
                }
                if runs.is_empty() {
                    list = list.child(
                        div()
                            .px(px(8.))
                            .py(px(24.))
                            .text_size(px(ui::TEXT_MD))
                            .text_color(theme::text_subtle())
                            .child(match self.filter {
                                Filter::Failed => "Nothing failed recently.",
                                Filter::Deploys => "No deploys in the recent runs.",
                                Filter::Mine => "No runs on the branches of your environments.",
                                Filter::All => "No runs yet.",
                            }),
                    );
                }
                for (ix, run) in runs.iter().enumerate() {
                    list = list.child(self.render_row(ix, run, cx));
                }
                list.into_any_element()
            }
        };
        div().flex().flex_col().size_full().child(header).child(body).into_any_element()
    }

    /// Production and the branch of each environment, at a glance.
    fn render_overview(&self, overview: Vec<(&'static str, &'static str, String, Run)>, cx: &mut Context<Self>) -> AnyElement {
        let cards: Vec<AnyElement> = overview
            .into_iter()
            .enumerate()
            .map(|(i, (icon_name, kind, name, run))| self.overview_card(("ov-card", i), icon_name, kind, name, &run, cx))
            .collect();
        if cards.is_empty() {
            return div().into_any_element();
        }
        div()
            .flex()
            .flex_wrap()
            .gap(px(8.))
            .px(px(8.))
            .pb(px(16.))
            .children(cards)
            .into_any_element()
    }

    fn overview_card(
        &self,
        id: impl Into<ElementId>,
        icon_name: &str,
        kind: &str,
        name: String,
        run: &Run,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let state = run.state();
        let rid = run.id;
        div()
            .id(id)
            .flex()
            .flex_col()
            .gap(px(4.))
            .w(px(220.))
            .px(px(12.))
            .py(px(10.))
            .rounded(px(ui::RADIUS_LG))
            .border_1()
            .border_color(if state == State::Failed { soft(RED, 0.35) } else { theme::mix(0.08) })
            .bg(theme::fill_raised())
            .cursor_pointer()
            .hover(|s| s.bg(theme::fill_hover()))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(6.))
                    .text_size(px(ui::TEXT_XS))
                    .text_color(theme::text_subtle())
                    .child(icon(icon_name, 12.).text_color(theme::text_subtle()))
                    .child(div().min_w_0().truncate().child(kind.to_string()))
                    .child(div().ml_auto().flex_none().child(model::age(run.created()))),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(6.))
                    .child(div().size(px(8.)).rounded_full().flex_none().bg(state_color(state)))
                    .child(div().flex_1().min_w_0().truncate().text_size(px(ui::TEXT_MD)).font_weight(FontWeight::MEDIUM).child(name)),
            )
            .child(
                div()
                    .text_size(px(ui::TEXT_XS))
                    .text_color(theme::text_subtle())
                    .truncate()
                    .child(format!("{} · #{} · {}", state.label(), run.number, run.workflow_name)),
            )
            .on_click(cx.listener(move |this, _, _, cx| this.select(Some(rid), cx)))
            .into_any_element()
    }

    fn render_row(&self, ix: usize, run: &Run, cx: &mut Context<Self>) -> AnyElement {
        let state = run.state();
        let id = run.id;
        let mine = self.branches.iter().any(|b| b.as_ref() == run.head_branch);
        let fixing = self.sessions.contains_key(&id);
        div()
            .id(("run-row", ix))
            .flex()
            .items_center()
            .gap(px(12.))
            .h(px(52.))
            .px(px(8.))
            .rounded(px(12.))
            .cursor_pointer()
            .hover(|s| s.bg(theme::hover()))
            .child(div().flex().items_center().justify_center().size(px(18.)).child(state_icon(("run-state", ix), state, 16.)))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_w_0()
                    .gap(px(2.))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(8.))
                            .child(div().min_w_0().truncate().text_size(px(ui::TEXT_MD)).font_weight(FontWeight::MEDIUM).child(run.display_title.clone()))
                            .when(fixing, |d| {
                                d.child(
                                    div()
                                        .flex()
                                        .flex_none()
                                        .items_center()
                                        .gap(px(4.))
                                        .px(px(6.))
                                        .h(px(18.))
                                        .rounded(px(4.))
                                        .bg(soft(AMBER, 0.12))
                                        .text_size(px(ui::TEXT_XS))
                                        .text_color(hex(AMBER))
                                        .child(icon("sparkles", 11.).text_color(hex(AMBER)))
                                        .child("Fixing"),
                                )
                            })
                            .when(run.passed_on_retry(), |d| {
                                d.child(
                                    div()
                                        .flex_none()
                                        .px(px(6.))
                                        .h(px(18.))
                                        .flex()
                                        .items_center()
                                        .rounded(px(4.))
                                        .bg(soft(AMBER, 0.12))
                                        .text_size(px(ui::TEXT_XS))
                                        .text_color(hex(AMBER))
                                        .child("Flaky · passed on retry"),
                                )
                            }),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(6.))
                            .min_w_0()
                            .text_size(px(ui::TEXT_XS))
                            .text_color(theme::text_subtle())
                            .child(div().flex_none().child(format!("#{} · {} · {}", run.number, run.workflow_name, run.trigger())))
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap(px(4.))
                                    .min_w_0()
                                    .px(px(6.))
                                    .rounded(px(4.))
                                    .bg(if mine { theme::mix(0.08) } else { theme::mix(0.04) })
                                    .text_color(if mine { theme::mix(0.75) } else { theme::mix(0.5) })
                                    .child(icon(if run.is_deploy() { "tag01" } else { "git-branch" }, 11.).text_color(theme::text_subtle()))
                                    .child(div().truncate().child(run.head_branch.clone())),
                            ),
                    ),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .items_end()
                    .flex_none()
                    .gap(px(2.))
                    .text_size(px(ui::TEXT_XS))
                    .text_color(theme::text_subtle())
                    .child(model::age(run.created()))
                    .when_some(run.duration(), |d, s| d.child(ci::duration_label(s))),
            )
            .on_click(cx.listener(move |this, _, _, cx| this.select(Some(id), cx)))
            .into_any_element()
    }
}

pub fn empty(icon_name: &str, title: &str, text: &str) -> AnyElement {
    ui::empty_state(icon_name, title, text, None).flex_1().justify_center().into_any_element()
}

impl Focusable for RunsView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for RunsView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let run = match (&self.root, self.selected) {
            (Some(root), Some(id)) => self.ci.read(cx).repo(root).and_then(|r| r.run(id)).cloned(),
            _ => None,
        };
        if self.selected.is_some() && run.is_none() {
            // Gone from the recent list, or another project is in front.
            let loaded = self.root.as_ref().and_then(|r| self.ci.read(cx).repo(r)).is_some_and(|r| r.loaded);
            if loaded {
                self.selected = None;
            }
        }
        if let (Some(root), Some(run)) = (self.root.clone(), &run) {
            let id = run.id;
            self.ci.update(cx, |ci, cx| {
                ci.load_detail(&root, id, cx);
                ci.poll_detail(&root, id, cx);
            });
        }
        let content = match run {
            Some(run) => self.render_detail(&run, window, cx),
            None => self.render_list(cx),
        };
        div()
            .id("runs-view")
            .track_focus(&self.focus)
            .on_key_down(cx.listener(Self::on_key))
            .flex()
            .flex_col()
            .size_full()
            .overflow_hidden()
            .bg(theme::background())
            .font_family(theme::FONT_SANS)
            .text_size(px(ui::TEXT_MD))
            .text_color(theme::content())
            .child(content)
    }
}

impl RunsView {
    fn detail_load(&self, id: u64, cx: &App) -> Option<DetailState> {
        let root = self.root.as_ref()?;
        let repo = self.ci.read(cx).repo(root)?;
        Some(match repo.detail(id)? {
            Load::Loading => DetailState::Loading,
            Load::Ready(d) => DetailState::Ready(d.clone()),
            Load::Failed(e) => DetailState::Failed(e.clone()),
        })
    }
}

enum List {
    Off,
    NoProject,
    NotGitHub,
    Loading(Option<SharedString>),
    Ready {
        runs: Vec<Run>,
        error: Option<SharedString>,
        overview: Vec<(&'static str, &'static str, String, Run)>,
    },
}

enum DetailState {
    Loading,
    Ready(std::sync::Arc<ci::Detail>),
    Failed(SharedString),
}
