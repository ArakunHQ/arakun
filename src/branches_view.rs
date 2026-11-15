//! The Branches tab: every live branch of the project as a lane next to the
//! default branch, on a time axis that only spends room where commits happened.
//! Each commit wears the color of the agent that wrote it; each row says what
//! the branch needs (conflicts, failing checks, ready to merge...) with the
//! most urgent first, and a selected branch opens its PR, environment, sessions
//! and commits, with a session to fix conflicts or checks one click away.

use std::path::PathBuf;
use std::time::Duration;

use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, App, AppContext as _, BorderStyle, Bounds, Context, Div, EventEmitter, FocusHandle,
    FontWeight, Hsla, InteractiveElement as _, IntoElement, ParentElement as _, PathBuilder,
    Pixels, Render, ScrollHandle, SharedString, StatefulInteractiveElement as _, Styled as _, Task,
    TextAlign, TextRun, Window, canvas, div, point, px, quad, size,
};

use crate::branches::{self, Branch, Checks, Map, Status, Who};
use crate::input::{InputEvent, TextInput};
use crate::model::{Agent, Status as SessionStatus};
use crate::theme;
use crate::ui::{self, ButtonKind, icon, icon_button};

/// Reopening the tab reloads a map older than this.
const STALE: Duration = Duration::from_secs(60);
/// While the tab is closed the map is read this often, for the rail's badge.
const BACKGROUND: Duration = Duration::from_secs(5 * 60);
const ROW_H: f32 = 34.;
/// Room above the lanes for the day labels.
const AXIS_H: f32 = 22.;
const LABEL_W: f32 = 200.;
const STATUS_W: f32 = 300.;
/// Room per commit on the time axis.
const COL_W: f32 = 18.;
const PAD: f32 = 14.;

const CLAUDE: u32 = 0xD97757;
const DROID: u32 = 0x8B7CF6;

/// An environment of the project, as the shell sees it.
#[derive(Clone)]
pub struct EnvInfo {
    pub project: usize,
    pub env: usize,
    pub path: PathBuf,
    pub name: SharedString,
    pub branch: String,
    pub sessions: Vec<SessionInfo>,
}

#[derive(Clone)]
pub struct SessionInfo {
    pub id: SharedString,
    pub title: SharedString,
    pub agent: Agent,
    pub status: SessionStatus,
}

impl EnvInfo {
    fn working(&self) -> bool {
        self.sessions
            .iter()
            .any(|s| matches!(s.status, SessionStatus::Working | SessionStatus::NeedsInput))
    }
}

pub enum BranchesEvent {
    /// Open a pull request in a browser tab.
    Open(String),
    OpenEnv {
        project: usize,
        env: usize,
    },
    OpenSession {
        project: usize,
        env: usize,
        id: SharedString,
    },
    /// Check `branch` out in a new environment.
    CreateEnv(String),
    /// Start a session of the environment's agent with `prompt`.
    Ask {
        path: PathBuf,
        title: String,
        prompt: String,
    },
    /// A pull request's checks finished since the last read.
    ChecksDone {
        title: String,
        body: String,
        failed: bool,
    },
}

pub struct BranchesView {
    root: Option<PathBuf>,
    envs: Vec<EnvInfo>,
    map: Option<Map>,
    loading: bool,
    loaded_at: Option<std::time::Instant>,
    selected: Option<String>,
    show_idle: bool,
    show_merged: bool,
    /// The field to link a GitHub repository, for a repository without a remote.
    show_repo: bool,
    focus: FocusHandle,
    scroll: ScrollHandle,
    /// `owner/name` of the GitHub repository to read pull requests from.
    repo: gpui::Entity<TextInput>,
    repo_error: Option<String>,
    _repo: gpui::Subscription,
    _load: Option<Task<()>>,
}

impl EventEmitter<BranchesEvent> for BranchesView {}

/// A row of the graph: the default branch, or one branch.
struct Row<'a> {
    branch: Option<&'a Branch>,
    status: Status,
    env: Option<&'a EnvInfo>,
}

impl BranchesView {
    pub fn new(cx: &mut Context<Self>) -> Self {
        let repo = cx.new(|cx| TextInput::new("owner/name", cx));
        let sub = cx.subscribe(&repo, |this, _, ev: &InputEvent, cx| {
            if let InputEvent::Submit = ev {
                this.use_repo(cx);
            }
        });
        Self {
            repo,
            repo_error: None,
            _repo: sub,
            root: None,
            envs: Vec::new(),
            map: None,
            loading: false,
            loaded_at: None,
            selected: None,
            show_idle: false,
            show_merged: false,
            show_repo: false,
            focus: cx.focus_handle(),
            scroll: ScrollHandle::new(),
            _load: None,
        }
    }

    pub fn focus(&self, window: &mut Window, cx: &mut App) {
        window.focus(&self.focus, cx);
    }

    /// The visible project: its main checkout and environments. A new project
    /// drops the old map.
    pub fn set_project(
        &mut self,
        root: Option<PathBuf>,
        envs: Vec<EnvInfo>,
        cx: &mut Context<Self>,
    ) {
        if self.root != root {
            self.root = root;
            self.map = None;
            self.selected = None;
            self.loaded_at = None;
            self._load = None;
            self.loading = false;
            cx.notify();
        }
        self.envs = envs;
    }

    /// Keeps the map of `root` fresh in the background, for the rail's badge.
    pub fn watch(&mut self, root: PathBuf, cx: &mut Context<Self>) {
        if self.root.as_ref() != Some(&root) {
            self.set_project(Some(root), Vec::new(), cx);
        }
        // Checks running somewhere: look again sooner, to tell when they end.
        let every = if self.checks_running() {
            STALE
        } else {
            BACKGROUND
        };
        if self.loaded_at.is_none_or(|t| t.elapsed() > every) {
            self.refresh(cx);
        }
    }

    fn checks_running(&self) -> bool {
        self.map.as_ref().is_some_and(|m| {
            m.branches
                .iter()
                .filter_map(|b| b.pr.as_ref())
                .any(|p| p.open() && p.checks == Checks::Pending)
        })
    }

    /// Every branch read, with its PR's number and title: what Ctrl+K can jump to.
    /// Each branch with how far it is ahead of the default one, for the palette.
    pub fn branch_ahead(&self) -> Vec<(String, u64, u64)> {
        self.map.as_ref().map_or_else(Vec::new, |m| {
            m.branches
                .iter()
                .map(|b| (b.name.clone(), b.ahead, b.behind))
                .collect()
        })
    }

    pub fn branch_list(&self) -> Vec<(String, Option<(u64, String)>)> {
        self.map.as_ref().map_or_else(Vec::new, |m| {
            m.branches
                .iter()
                .map(|b| {
                    (
                        b.name.clone(),
                        b.pr.as_ref().map(|p| (p.number, p.title.clone())),
                    )
                })
                .collect()
        })
    }

    /// Branches that need someone now: conflicts or failing checks.
    pub fn alarms(&self) -> usize {
        let Some(map) = &self.map else { return 0 };
        let now = crate::model::unix_now();
        map.branches
            .iter()
            .filter(|b| {
                let env = self.env_of(&b.name);
                matches!(
                    b.status(env.is_some(), env.is_some_and(EnvInfo::working), now),
                    Status::Conflict | Status::Failing
                )
            })
            .count()
    }

    pub fn refresh_if_stale(&mut self, cx: &mut Context<Self>) {
        if self.loaded_at.is_none_or(|t| t.elapsed() > STALE) {
            self.refresh(cx);
        }
    }

    pub fn refresh(&mut self, cx: &mut Context<Self>) {
        let Some(root) = self.root.clone() else {
            return;
        };
        if self.loading {
            return;
        }
        self.loading = true;
        self.loaded_at = Some(std::time::Instant::now());
        cx.notify();
        self._load = Some(cx.spawn(async move |this, cx| {
            let r = root.clone();
            let map = cx.background_spawn(async move { branches::load(&r) }).await;
            let _ = this.update(cx, |this, cx| {
                if this.root.as_ref() == Some(&root) {
                    if let Some(old) = &this.map {
                        for (title, body, failed) in finished_checks(old, &map) {
                            cx.emit(BranchesEvent::ChecksDone {
                                title,
                                body,
                                failed,
                            });
                        }
                    }
                    this.map = Some(map);
                }
                this.loading = false;
                cx.notify();
            });
        }));
    }

    /// Reads pull requests from the GitHub repository typed in the field.
    fn use_repo(&mut self, cx: &mut Context<Self>) {
        let Some(root) = self.root.clone() else {
            return;
        };
        let text = self.repo.read(cx).text().to_string();
        match branches::set_github_repo(&root, &text) {
            Ok(()) => {
                self.repo_error = None;
                self.refresh(cx);
            }
            Err(e) => self.repo_error = Some(e),
        }
        cx.notify();
    }

    /// Where pull requests can't be read: why, and a field to name the repository.
    fn render_repo(&self, error: &str, cx: &mut Context<Self>) -> Div {
        div()
            .flex()
            .flex_col()
            .gap(px(8.))
            .p(px(12.))
            .rounded(px(ui::RADIUS_LG))
            .border_1()
            .border_color(theme::stroke())
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .text_size(px(ui::TEXT_SM))
                    .child(icon("git-pull-request", 14.).text_color(theme::text_subtle()))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .child(format!("Pull requests unavailable: {error}")),
                    ),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .child(
                        div()
                            .text_size(px(ui::TEXT_SM))
                            .text_color(theme::text_muted())
                            .child("Read them from GitHub repository"),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .w(px(240.))
                            .h(px(ui::BUTTON_H))
                            .px(px(8.))
                            .rounded(px(ui::RADIUS))
                            .border_1()
                            .border_color(theme::border_strong())
                            .font_family(theme::FONT_MONO)
                            .text_size(px(ui::TEXT_SM))
                            .child(self.repo.clone()),
                    )
                    .child(
                        ui::button("branches-repo", "Use", ButtonKind::Outline, true)
                            .on_click(cx.listener(|this, _, _, cx| this.use_repo(cx))),
                    ),
            )
            .when_some(self.repo_error.clone(), |d, e| {
                d.child(
                    div()
                        .text_size(px(ui::TEXT_XS))
                        .text_color(theme::danger())
                        .child(e),
                )
            })
    }

    fn env_of(&self, branch: &str) -> Option<&EnvInfo> {
        self.envs.iter().find(|e| e.branch == branch)
    }

    /// The rows, most urgent first; merged and inactive ones only when shown.
    /// Also how many of each are hidden or shown.
    fn rows<'a>(&'a self, map: &'a Map, now: u64) -> (Vec<Row<'a>>, usize, usize) {
        let mut rows: Vec<Row> = map
            .branches
            .iter()
            .map(|b| {
                let env = self.env_of(&b.name);
                Row {
                    branch: Some(b),
                    status: b.status(env.is_some(), env.is_some_and(EnvInfo::working), now),
                    env,
                }
            })
            .collect();
        rows.sort_by(|a, b| {
            a.status.cmp(&b.status).then_with(|| {
                b.branch
                    .map_or(0, |x| x.time)
                    .cmp(&a.branch.map_or(0, |x| x.time))
            })
        });
        let idle = rows.iter().filter(|r| r.status == Status::Idle).count();
        let merged = rows.iter().filter(|r| r.status == Status::Merged).count();
        let selected = self.selected.as_deref();
        // A selected row stays, so its detail and lane match.
        rows.retain(|r| match r.status {
            Status::Idle => self.show_idle || r.branch.map(|b| b.name.as_str()) == selected,
            Status::Merged => self.show_merged || r.branch.map(|b| b.name.as_str()) == selected,
            _ => true,
        });
        rows.insert(
            0,
            Row {
                branch: None,
                status: Status::Working,
                env: self.env_of(&map.main),
            },
        );
        (rows, merged, idle)
    }

    /// Selects `name` (its row shows even in a hidden group).
    pub fn reveal(&mut self, name: String, cx: &mut Context<Self>) {
        self.selected = Some(name);
        cx.notify();
    }

    fn select(&mut self, name: Option<String>, cx: &mut Context<Self>) {
        self.selected = if self.selected == name { None } else { name };
        cx.notify();
    }

    fn render_summary(&self, map: &Map, now: u64) -> Div {
        let mut counts: Vec<(Status, usize)> = Vec::new();
        for b in &map.branches {
            let env = self.env_of(&b.name);
            let s = b.status(env.is_some(), env.is_some_and(EnvInfo::working), now);
            match counts.iter_mut().find(|(x, _)| *x == s) {
                Some(c) => c.1 += 1,
                None => counts.push((s, 1)),
            }
        }
        counts.sort_by_key(|(s, _)| *s);
        div().flex().flex_wrap().gap(px(8.)).children(
            counts
                .into_iter()
                .filter(|(s, _)| *s != Status::Idle)
                .map(|(s, n)| {
                    div()
                        .flex()
                        .items_center()
                        .gap(px(6.))
                        .h(px(26.))
                        .px(px(10.))
                        .rounded_full()
                        .bg(status_color(s).opacity(0.12))
                        .text_size(px(ui::TEXT_SM))
                        .child(div().size(px(7.)).rounded_full().bg(status_color(s)))
                        .child(div().font_weight(FontWeight::MEDIUM).child(n.to_string()))
                        .child(div().text_color(theme::text_muted()).child(s.label()))
                }),
        )
    }

    fn render_graph(
        &self,
        map: &Map,
        rows: &[Row],
        merged: usize,
        idle: usize,
        now: u64,
        cx: &mut Context<Self>,
    ) -> Div {
        let lanes = Lanes::new(map, rows);
        let height = AXIS_H + rows.len() as f32 * ROW_H;
        let painter = lanes.clone();
        let graph = canvas(
            |_, _, _| {},
            move |bounds, _, window, cx| painter.paint(bounds, window, cx),
        )
        .absolute()
        .top_0()
        .bottom_0()
        .left(px(LABEL_W))
        .right(px(STATUS_W))
        .h(px(height));

        let mut list = div().relative().flex().flex_col().pt(px(AXIS_H));
        for (i, row) in rows.iter().enumerate() {
            list = list.child(self.render_row(map, row, i, now, cx));
        }
        let toggle = |id: &'static str,
                      open: bool,
                      label: String,
                      cx: &mut Context<Self>,
                      flip: fn(&mut Self)| {
            div()
                .id(id)
                .flex()
                .items_center()
                .gap(px(6.))
                .h(px(28.))
                .px(px(12.))
                .text_size(px(ui::TEXT_SM))
                .text_color(theme::text_subtle())
                .cursor_pointer()
                .hover(|s| s.text_color(theme::content()))
                .child(
                    icon(if open { "arrow-up01" } else { "arrow-down01" }, 12.)
                        .text_color(theme::text_subtle()),
                )
                .child(label)
                .on_click(cx.listener(move |this, _, _, cx| {
                    flip(this);
                    cx.notify();
                }))
        };
        let plural = |n: usize| if n == 1 { "branch" } else { "branches" };
        let toggles = div()
            .flex()
            .gap(px(4.))
            .when(merged > 0, |d| {
                let label = if self.show_merged {
                    "Hide merged".to_string()
                } else {
                    format!("{merged} merged this week")
                };
                d.child(toggle(
                    "branches-merged",
                    self.show_merged,
                    label,
                    cx,
                    |this| this.show_merged = !this.show_merged,
                ))
            })
            .when(idle > 0, |d| {
                let label = if self.show_idle {
                    "Hide inactive".to_string()
                } else {
                    format!("{idle} inactive {}", plural(idle))
                };
                d.child(toggle("branches-idle", self.show_idle, label, cx, |this| {
                    this.show_idle = !this.show_idle
                }))
            });
        div()
            .flex()
            .flex_col()
            .rounded(px(ui::RADIUS_LG))
            .bg(theme::fill_raised())
            .overflow_hidden()
            .child(div().relative().child(list).child(graph))
            .when(merged + idle > 0, |d| d.child(toggles))
    }

    fn render_row(
        &self,
        map: &Map,
        row: &Row,
        i: usize,
        now: u64,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let name = row.branch.map_or(map.main.clone(), |b| b.name.clone());
        let selected = self.selected.as_deref() == Some(name.as_str()) && row.branch.is_some();
        let color = row
            .branch
            .map_or(theme::mix(0.45), |_| status_color(row.status));
        let label = div()
            .flex()
            .items_center()
            .gap(px(8.))
            .w(px(LABEL_W))
            .flex_none()
            .pl(px(14.))
            .pr(px(8.))
            .child(div().flex_none().size(px(8.)).rounded_full().bg(color))
            .child(
                div()
                    .min_w_0()
                    .truncate()
                    .font_family(theme::FONT_MONO)
                    .text_size(px(ui::TEXT_SM))
                    .when(row.branch.is_none(), |d| {
                        d.font_weight(FontWeight::SEMIBOLD)
                    })
                    .when(row.branch.is_some_and(|b| b.ghost), |d| {
                        d.text_color(theme::text_subtle())
                    })
                    .child(name.clone()),
            )
            .when_some(row.env, |d, _| {
                d.child(icon("folder01", 11.).text_color(theme::text_subtle()))
            });
        let status = div()
            .flex()
            .items_center()
            .justify_end()
            .gap(px(6.))
            .w(px(STATUS_W))
            .flex_none()
            .pr(px(14.))
            .children(match row.branch {
                None => vec![meta(format!("{} commits shown", map.main_commits.len()))],
                Some(b) => row_status(b, row.status, row.env, now),
            });
        div()
            .id(SharedString::from(format!("branch-row-{i}")))
            .flex()
            .items_center()
            .h(px(ROW_H))
            .when(i > 0, |d| d.border_t_1().border_color(theme::stroke()))
            .when(selected, |d| d.bg(theme::fill_selected()))
            .when(row.branch.is_some() && !selected, |d| {
                d.hover(|s| s.bg(theme::fill_hover()))
            })
            .when(row.branch.is_some(), |d| d.cursor_pointer())
            .child(label)
            .child(div().flex_1())
            .child(status)
            .when(row.branch.is_some(), |d| {
                d.on_click(cx.listener(move |this, _, _, cx| this.select(Some(name.clone()), cx)))
            })
            .into_any_element()
    }

    fn render_detail(&self, map: &Map, b: &Branch, now: u64, cx: &mut Context<Self>) -> Div {
        let env = self.env_of(&b.name);
        let status = b.status(env.is_some(), env.is_some_and(EnvInfo::working), now);
        let mut actions: Vec<AnyElement> = Vec::new();
        if let Some(pr) = &b.pr {
            let url = pr.url.clone();
            actions.push(
                ui::button_with(
                    SharedString::from("branch-pr"),
                    Some("git-pull-request"),
                    format!("PR #{}", pr.number),
                    ButtonKind::Outline,
                    true,
                )
                .on_click(cx.listener(move |_, _, _, cx| cx.emit(BranchesEvent::Open(url.clone()))))
                .into_any_element(),
            );
        }
        match env {
            Some(e) => {
                let (project, envi) = (e.project, e.env);
                actions.push(
                    ui::button_with(
                        SharedString::from("branch-env"),
                        Some("folder-open"),
                        format!("Open {}", e.name),
                        ButtonKind::Outline,
                        true,
                    )
                    .on_click(cx.listener(move |_, _, _, cx| {
                        cx.emit(BranchesEvent::OpenEnv { project, env: envi })
                    }))
                    .into_any_element(),
                );
                if let Some((title, prompt)) = fix_prompt(map, b, status) {
                    let path = e.path.clone();
                    actions.push(
                        ui::button_with(
                            SharedString::from("branch-fix"),
                            Some("sparkles"),
                            title.clone(),
                            ButtonKind::Primary,
                            true,
                        )
                        .on_click(cx.listener(move |_, _, _, cx| {
                            cx.emit(BranchesEvent::Ask {
                                path: path.clone(),
                                title: title.clone(),
                                prompt: prompt.clone(),
                            })
                        }))
                        .into_any_element(),
                    );
                }
            }
            None if !b.ghost && status != Status::Merged => {
                let name = b.name.clone();
                actions.push(
                    ui::button_with(
                        SharedString::from("branch-checkout"),
                        Some("folder-add"),
                        "Check out in a new environment",
                        ButtonKind::Outline,
                        true,
                    )
                    .on_click(cx.listener(move |_, _, _, cx| {
                        cx.emit(BranchesEvent::CreateEnv(name.clone()))
                    }))
                    .into_any_element(),
                );
            }
            None => {}
        }

        let mut facts = Vec::new();
        if !b.ghost && !matches!(status, Status::Merged | Status::Idle) {
            facts.push(format!(
                "{} ahead, {} behind {}",
                b.ahead, b.behind, map.main
            ));
        }
        if let Some(pr) = &b.pr {
            facts.push(format!(
                "PR #{} {} by {}",
                pr.number,
                pr.state.to_lowercase(),
                if pr.author.is_empty() {
                    "?"
                } else {
                    &pr.author
                }
            ));
        }
        if b.time > 0 {
            facts.push(format!("last commit {}", crate::account::ago(b.time, now)));
        }
        if b.ghost {
            facts.push("only on GitHub".into());
        } else if b.remote_only {
            facts.push("only on the remote".into());
        }

        let conflicts = b.conflicts.as_ref().filter(|c| !c.is_empty()).map(|files| {
            div()
                .flex()
                .flex_col()
                .gap(px(4.))
                .p(px(10.))
                .rounded(px(ui::RADIUS))
                .bg(Hsla::from(theme::danger()).opacity(0.08))
                .child(
                    div()
                        .text_size(px(ui::TEXT_SM))
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(theme::danger())
                        .child(format!(
                            "Merging into {} would conflict in {} {}",
                            map.main,
                            files.len(),
                            if files.len() == 1 { "file" } else { "files" }
                        )),
                )
                .children(files.iter().take(8).map(|f| {
                    div()
                        .font_family(theme::FONT_MONO)
                        .text_size(px(ui::TEXT_XS))
                        .text_color(theme::text_muted())
                        .child(f.clone())
                }))
        });

        let sessions = env.map(|e| {
            let (project, envi) = (e.project, e.env);
            div()
                .flex()
                .flex_wrap()
                .gap(px(6.))
                .children(e.sessions.iter().take(6).map(|s| {
                    let id = s.id.clone();
                    div()
                        .id(SharedString::from(format!("branch-session-{}", s.id)))
                        .flex()
                        .items_center()
                        .gap(px(6.))
                        .h(px(26.))
                        .px(px(8.))
                        .max_w(px(260.))
                        .rounded(px(ui::RADIUS))
                        .border_1()
                        .border_color(theme::stroke())
                        .cursor_pointer()
                        .hover(|st| st.bg(theme::fill_hover()))
                        .text_size(px(ui::TEXT_SM))
                        .child(crate::shell::agent_icon(s.agent, 12.))
                        .child(div().min_w_0().truncate().child(s.title.clone()))
                        .when(matches!(s.status, SessionStatus::Working), |d| {
                            d.child(ui::spinner(
                                SharedString::from(format!("bs-spin-{}", s.id)),
                                10.,
                                theme::accent(),
                            ))
                        })
                        .on_click(cx.listener(move |_, _, _, cx| {
                            cx.emit(BranchesEvent::OpenSession {
                                project,
                                env: envi,
                                id: id.clone(),
                            })
                        }))
                }))
        });

        let commits = div().flex().flex_col().children(b.commits.iter().map(|c| {
            div()
                .flex()
                .items_center()
                .gap(px(10.))
                .h(px(28.))
                .text_size(px(ui::TEXT_SM))
                .child(
                    div()
                        .flex_none()
                        .size(px(10.))
                        .rounded_full()
                        .border_2()
                        .border_color(who_color(c.who))
                        .when(!c.pushed, |d| d.border_dashed()),
                )
                .child(
                    div()
                        .flex_none()
                        .w(px(58.))
                        .font_family(theme::FONT_MONO)
                        .text_size(px(ui::TEXT_XS))
                        .text_color(theme::text_subtle())
                        .child(c.short().to_string()),
                )
                .child(div().flex_1().min_w_0().truncate().child(c.title.clone()))
                .child(
                    div()
                        .flex_none()
                        .text_size(px(ui::TEXT_XS))
                        .text_color(theme::text_subtle())
                        .child(match c.who {
                            Who::Claude => format!("Claude · {}", c.author),
                            Who::Droid => format!("Droid · {}", c.author),
                            Who::Person => c.author.clone(),
                        }),
                )
                .when(!c.pushed, |d| d.child(ui::tag("not pushed", theme::warn())))
                .child(
                    div()
                        .flex_none()
                        .w(px(64.))
                        .text_right()
                        .text_size(px(ui::TEXT_XS))
                        .text_color(theme::text_subtle())
                        .child(crate::account::ago(c.time, now)),
                )
        }));

        div()
            .flex()
            .flex_col()
            .gap(px(12.))
            .p(px(16.))
            .rounded(px(ui::RADIUS_LG))
            .border_1()
            .border_color(theme::stroke())
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(10.))
                    .child(div().size(px(10.)).rounded_full().bg(status_color(status)))
                    .child(
                        div()
                            .min_w_0()
                            .truncate()
                            .font_family(theme::FONT_MONO)
                            .text_size(px(ui::TEXT_LG))
                            .font_weight(FontWeight::MEDIUM)
                            .child(b.name.clone()),
                    )
                    .child(ui::tag(status.label(), status_color(status)))
                    .child(div().flex_1())
                    .child(
                        icon_button("branch-close", "cancel01", 12.)
                            .on_click(cx.listener(|this, _, _, cx| this.select(None, cx))),
                    ),
            )
            .when_some(b.pr.as_ref(), |d, pr| {
                d.child(div().text_size(px(ui::TEXT_MD)).child(pr.title.clone()))
            })
            .child(meta(facts.join(" · ")))
            .children(conflicts)
            .when(!actions.is_empty(), |d| {
                d.child(div().flex().flex_wrap().gap(px(8.)).children(actions))
            })
            .children(sessions)
            .when(!b.commits.is_empty(), |d| {
                d.child(meta(format!("Commits not on {}", map.main)))
                    .child(commits)
            })
    }
}

/// What a branch's row says at its right: environment, agents, PR, checks, ahead/behind.
fn row_status(b: &Branch, status: Status, env: Option<&EnvInfo>, now: u64) -> Vec<AnyElement> {
    let mut out: Vec<AnyElement> = Vec::new();
    if let Some(e) = env
        && let Some(s) = e
            .sessions
            .iter()
            .find(|s| matches!(s.status, SessionStatus::Working | SessionStatus::NeedsInput))
    {
        out.push(
            div()
                .flex()
                .items_center()
                .gap(px(4.))
                .child(crate::shell::agent_icon(s.agent, 12.))
                .child(ui::spinner(
                    SharedString::from(format!("bw-{}", s.id)),
                    10.,
                    theme::accent(),
                ))
                .into_any_element(),
        );
    } else {
        for w in b
            .authors()
            .into_iter()
            .filter(|w| *w != Who::Person)
            .take(2)
        {
            out.push(
                div()
                    .size(px(8.))
                    .rounded_full()
                    .bg(who_color(w))
                    .into_any_element(),
            );
        }
    }
    if !b.ghost
        && status != Status::Merged
        && status != Status::Idle
        && (b.ahead > 0 || b.behind > 0)
    {
        out.push(meta(format!("↑{} ↓{}", b.ahead, b.behind)));
    }
    if let Some(pr) = &b.pr {
        let (name, color) = match pr.checks {
            Checks::Failed => ("cancel-circle", theme::danger().into()),
            Checks::Passed => ("checkmark-circle02", theme::success().into()),
            Checks::Pending => ("loading03", theme::warn().into()),
            Checks::None => ("", theme::mix(0.4)),
        };
        if pr.open() && !name.is_empty() {
            out.push(icon(name, 12.).text_color(color).into_any_element());
        }
        out.push(
            ui::tag(
                format!("#{}", pr.number),
                if pr.merged() {
                    Hsla::from(theme::pr_merged())
                } else if pr.open() {
                    theme::pr_open().into()
                } else {
                    theme::mix(0.45)
                },
            )
            .into_any_element(),
        );
    }
    out.push(ui::tag(status.label(), status_color(status)).into_any_element());
    if status == Status::Idle {
        out.push(meta(crate::account::ago(b.time, now)));
    }
    out
}

/// The pull requests whose checks were running in `old` and are done in `new`.
fn finished_checks(old: &Map, new: &Map) -> Vec<(String, String, bool)> {
    let prs = |m: &Map| {
        m.branches
            .iter()
            .filter_map(|b| b.pr.clone())
            .collect::<Vec<_>>()
    };
    let before = prs(old);
    prs(new)
        .into_iter()
        .filter(|p| matches!(p.checks, Checks::Passed | Checks::Failed))
        .filter(|p| {
            before
                .iter()
                .any(|b| b.number == p.number && b.checks == Checks::Pending)
        })
        .map(|p| {
            let failed = p.checks == Checks::Failed;
            let title = format!(
                "Checks {} on #{}",
                if failed { "failed" } else { "passed" },
                p.number
            );
            (title, p.title, failed)
        })
        .collect()
}

/// The session that would move a branch forward, when one would.
fn fix_prompt(map: &Map, b: &Branch, status: Status) -> Option<(String, String)> {
    match status {
        Status::Conflict => {
            let files = b
                .conflicts
                .as_ref()
                .map(|f| f.join(", "))
                .unwrap_or_default();
            Some((
                "Resolve conflicts".into(),
                format!(
                    "This branch (`{}`) is {} commits ahead and {} behind `{}`, and merging it would conflict{}.\n\nBring `{}` into this branch (merge it, unless the branch's history says rebases are the norm), resolve every conflict keeping the intent of both sides, run the project's checks, and commit. Don't push or force-push without asking me.",
                    b.name,
                    b.ahead,
                    b.behind,
                    map.main_ref,
                    if files.is_empty() {
                        String::new()
                    } else {
                        format!(" in {files}")
                    },
                    map.main_ref
                ),
            ))
        }
        Status::Failing => {
            let n = b.pr.as_ref()?.number;
            Some((
                "Fix failing checks".into(),
                format!(
                    "The checks of PR #{n} (branch `{}`) are failing. Read them with `gh pr checks {n}` and the failed job logs (`gh run view --log-failed`), fix the cause in this branch, run the same checks locally, and commit. Don't push without asking me.",
                    b.name
                ),
            ))
        }
        _ if b.behind > 20 => Some((
            format!("Update with {}", map.main),
            format!(
                "This branch (`{}`) is {} commits behind `{}`. Bring those changes in (merge, unless rebases are the norm here), make sure everything still builds and the tests pass, and commit. Don't push without asking me.",
                b.name, b.behind, map.main_ref
            ),
        )),
        _ => None,
    }
}

fn status_color(s: Status) -> Hsla {
    match s {
        Status::Conflict | Status::Failing => theme::danger().into(),
        Status::Ready => theme::success().into(),
        Status::Review => theme::pr_open().into(),
        Status::Working => theme::accent(),
        Status::Local => theme::warn().into(),
        Status::Merged => theme::pr_merged().into(),
        Status::Idle => theme::mix(0.35),
    }
}

fn who_color(w: Who) -> Hsla {
    match w {
        Who::Claude => gpui::rgb(CLAUDE).into(),
        Who::Droid => gpui::rgb(DROID).into(),
        Who::Person => theme::mix(0.55),
    }
}

fn meta(text: impl Into<SharedString>) -> AnyElement {
    div()
        .text_size(px(ui::TEXT_XS))
        .text_color(theme::text_subtle())
        .truncate()
        .child(text.into())
        .into_any_element()
}

/// What the canvas needs, copied out of the rows so it can paint later.
#[derive(Clone)]
struct Lanes {
    /// Every commit time drawn, newest first; a commit's column is its index.
    times: Vec<u64>,
    main: Vec<Dot>,
    rows: Vec<Lane>,
}

#[derive(Clone)]
struct Dot {
    time: u64,
    who: Who,
    pushed: bool,
}

#[derive(Clone)]
struct Lane {
    color: Hsla,
    /// Newest first.
    dots: Vec<Dot>,
    /// Time of the commit on main it grew from (0: older than the view).
    fork: Option<u64>,
    /// Time of the commit on main that took it in.
    merge: Option<u64>,
    ghost: bool,
    faded: bool,
}

impl Lanes {
    fn new(map: &Map, rows: &[Row]) -> Self {
        let main_time = |sha: &str| {
            map.main_commits
                .iter()
                .find(|c| c.sha == sha)
                .map(|c| c.time)
        };
        let dot = |c: &branches::Commit| Dot {
            time: c.time,
            who: c.who,
            pushed: c.pushed,
        };
        let lanes: Vec<Lane> = rows
            .iter()
            .skip(1)
            .filter_map(|r| r.branch.map(|b| (b, r.status)))
            .map(|(b, status)| Lane {
                color: status_color(status),
                dots: b.commits.iter().map(dot).collect(),
                fork: b.fork.as_deref().map(|s| main_time(s).unwrap_or(0)),
                merge: b.merged_by.as_deref().and_then(main_time),
                ghost: b.ghost,
                faded: matches!(status, Status::Idle | Status::Merged),
            })
            .collect();
        let mut times: Vec<u64> = map
            .main_commits
            .iter()
            .map(|c| c.time)
            .chain(lanes.iter().flat_map(|l| l.dots.iter().map(|d| d.time)))
            .collect();
        times.sort_unstable_by(|a, b| b.cmp(a));
        times.dedup();
        Self {
            times,
            main: map.main_commits.iter().map(dot).collect(),
            rows: lanes,
        }
    }

    /// x of a time: its column from the right edge; None when older than what fits.
    fn x(&self, time: u64, right: f32, columns: usize) -> Option<f32> {
        let i = self.times.iter().position(|t| *t <= time)?;
        (i < columns).then_some(right - i as f32 * COL_W)
    }

    fn paint(&self, bounds: Bounds<Pixels>, window: &mut Window, cx: &mut App) {
        let o = bounds.origin;
        let w = f32::from(bounds.size.width);
        let right = w - PAD;
        let columns = (((w - 2. * PAD) / COL_W).floor() as usize).max(1);
        let left = right - (columns.saturating_sub(1)) as f32 * COL_W;
        let y = |row: usize| AXIS_H + row as f32 * ROW_H + ROW_H / 2.;
        let at = |x: f32, yy: f32| point(o.x + px(x), o.y + px(yy));
        let line = |from: gpui::Point<Pixels>,
                    to: gpui::Point<Pixels>,
                    curve: bool,
                    color: Hsla,
                    width: f32,
                    window: &mut Window| {
            let mut p = PathBuilder::stroke(px(width));
            p.move_to(from);
            if curve {
                let dx = (to.x - from.x) * 0.5;
                p.cubic_bezier_to(to, point(from.x + dx, from.y), point(to.x - dx, to.y));
            } else {
                p.line_to(to);
            }
            if let Ok(path) = p.build() {
                window.paint_path(path, color);
            }
        };

        // Down from main at `x`, round the corner, along the lane to `to_x`; or the
        // mirror image, along the lane and up into main.
        let elbow = |x: f32, lane_y: f32, to_x: f32, color: Hsla, window: &mut Window| {
            let r = (ROW_H * 0.45).min((to_x - x).abs().max(1.));
            let dir = if to_x >= x { 1. } else { -1. };
            let k = r * 0.552;
            let mut p = PathBuilder::stroke(px(2.));
            p.move_to(at(x, y(0)));
            p.line_to(at(x, lane_y - r));
            p.cubic_bezier_to(
                at(x + dir * r, lane_y),
                at(x, lane_y - r + k),
                at(x + dir * (r - k), lane_y),
            );
            p.line_to(at(to_x, lane_y));
            if let Ok(path) = p.build() {
                window.paint_path(path, color);
            }
        };

        self.paint_days(o, right, columns, window, cx);

        // The default branch: one line across, a dot per commit.
        let main_color = theme::mix(0.4);
        let main_xs: Vec<f32> = self
            .main
            .iter()
            .filter_map(|d| self.x(d.time, right, columns))
            .collect();
        let main_left = if self.main.len() > main_xs.len() {
            left
        } else {
            main_xs.last().copied().unwrap_or(right)
        };
        line(
            at(main_left, y(0)),
            at(main_xs.first().copied().unwrap_or(right), y(0)),
            false,
            main_color,
            2.,
            window,
        );

        for (i, lane) in self.rows.iter().enumerate() {
            let row_y = y(i + 1);
            let color = if lane.faded {
                lane.color.opacity(0.45)
            } else {
                lane.color
            };
            let xs: Vec<f32> = lane
                .dots
                .iter()
                .filter_map(|d| self.x(d.time, right, columns))
                .collect();
            // A lane with no commit in view still shows where it is: a stub at its merge, or at the right edge.
            let (newest, oldest) = match (xs.first(), xs.last()) {
                (Some(n), Some(o)) => (*n, if lane.dots.len() > xs.len() { left } else { *o }),
                _ => match (
                    lane.merge.and_then(|t| self.x(t, right, columns)),
                    lane.fork
                        .filter(|f| *f > 0)
                        .and_then(|t| self.x(t, right, columns)),
                ) {
                    (Some(m), _) => (m - COL_W, m - COL_W),
                    // No commits of its own: it points at a commit of main.
                    (None, Some(f)) if lane.dots.is_empty() && !lane.ghost => {
                        (f + COL_W, f + COL_W)
                    }
                    (None, _) if lane.dots.is_empty() => (right, right),
                    (None, _) => continue,
                },
            };
            if newest > oldest {
                line(
                    at(oldest, row_y),
                    at(newest, row_y),
                    false,
                    color,
                    2.,
                    window,
                );
            }
            // Grew from main: a curve down from the fork commit to its first commit.
            if let Some(fork) = lane.fork {
                let fx = if fork == 0 {
                    None
                } else {
                    self.x(fork, right, columns)
                };
                match fx {
                    Some(fx) if fx < oldest => elbow(fx, row_y, oldest, color, window),
                    _ if oldest > left => line(
                        at(left, row_y),
                        at(oldest, row_y),
                        false,
                        color.opacity(0.4),
                        2.,
                        window,
                    ),
                    _ => {}
                }
            }
            if let Some(mx) = lane.merge.and_then(|t| self.x(t, right, columns)) {
                elbow(mx, row_y, newest, color, window);
            }
            for (d, x) in lane.dots.iter().zip(xs.iter()) {
                dot(
                    window,
                    at(*x, row_y),
                    who_color(d.who),
                    d.pushed,
                    if lane.faded { 0.5 } else { 1. },
                );
            }
            if lane.dots.is_empty() {
                // Only known from GitHub: a hollow dashed ring.
                dot(window, at(newest, row_y), color, false, 0.8);
            }
        }
        for d in &self.main {
            if let Some(x) = self.x(d.time, right, columns) {
                let c = if d.who == Who::Person {
                    main_color
                } else {
                    who_color(d.who)
                };
                let r = 3.5;
                window.paint_quad(quad(
                    Bounds::new(
                        point(o.x + px(x - r), o.y + px(y(0) - r)),
                        size(px(r * 2.), px(r * 2.)),
                    ),
                    px(r),
                    c,
                    px(0.),
                    gpui::transparent_black(),
                    BorderStyle::Solid,
                ));
            }
        }
    }

    /// A label where the day changes, over a faint guide line.
    fn paint_days(
        &self,
        o: gpui::Point<Pixels>,
        right: f32,
        columns: usize,
        window: &mut Window,
        cx: &mut App,
    ) {
        let font_size = px(ui::TEXT_XS);
        let mut last_day = None;
        let mut last_x = f32::MAX;
        for (i, t) in self.times.iter().take(columns).enumerate() {
            let day = t / 86_400;
            if last_day == Some(day) {
                continue;
            }
            last_day = Some(day);
            let x = right - i as f32 * COL_W;
            // Leave room between labels.
            if last_x - x < 52. {
                continue;
            }
            last_x = x;
            let text: SharedString = crate::cloudflare::date(*t)
                .get(5..)
                .unwrap_or_default()
                .to_string()
                .into();
            let run = TextRun {
                len: text.len(),
                font: gpui::font(theme::FONT_SANS),
                color: theme::text_faint(),
                background_color: None,
                underline: None,
                strikethrough: None,
            };
            let shaped = window
                .text_system()
                .shape_line(text, font_size, &[run], None);
            let tx = x - f32::from(shaped.width);
            let _ = shaped.paint(
                point(o.x + px(tx.max(0.)), o.y + px(4.)),
                px(14.),
                TextAlign::Left,
                None,
                window,
                cx,
            );
        }
    }
}

fn dot(window: &mut Window, c: gpui::Point<Pixels>, ring: Hsla, pushed: bool, alpha: f32) {
    let r = 5.;
    window.paint_quad(quad(
        Bounds::new(
            point(c.x - px(r), c.y - px(r)),
            size(px(r * 2.), px(r * 2.)),
        ),
        px(r),
        theme::fill_raised(),
        px(2.),
        ring.opacity(alpha),
        if pushed {
            BorderStyle::Solid
        } else {
            BorderStyle::Dashed
        },
    ));
}

impl Render for BranchesView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let now = crate::model::unix_now();
        let header = div()
            .flex()
            .flex_none()
            .items_center()
            .gap(px(8.))
            .h(px(ui::TOOLBAR_H))
            .px(px(12.))
            .border_b_1()
            .border_color(theme::stroke())
            .text_size(px(ui::TEXT_SM))
            .text_color(theme::text_subtle())
            .child(icon("git-branch", 14.).text_color(theme::text_subtle()))
            .child(div().flex_none().child("Branches"))
            .when_some(self.map.as_ref(), |d, m| {
                let from = m
                    .github
                    .as_ref()
                    .map_or(String::new(), |g| format!(" · PRs from {g}"));
                d.child(
                    div()
                        .min_w_0()
                        .truncate()
                        .text_color(theme::text_faint())
                        .child(format!("against {}{from}", m.main_ref)),
                )
            })
            .child(div().flex_1())
            .when(
                self.map
                    .as_ref()
                    .is_some_and(|m| m.prs_error.as_deref() == Some(branches::NO_REMOTE)),
                |d| {
                    d.child(
                        div()
                            .id("branches-link-repo")
                            .flex_none()
                            .text_color(theme::text_faint())
                            .cursor_pointer()
                            .hover(|s| s.text_color(theme::content()))
                            .child("No GitHub remote · Link a repo")
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.show_repo = !this.show_repo;
                                cx.notify();
                            })),
                    )
                },
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(10.))
                    .text_size(px(ui::TEXT_XS))
                    .child(legend(gpui::rgb(CLAUDE).into(), "Claude"))
                    .child(legend(gpui::rgb(DROID).into(), "Droid"))
                    .child(legend(theme::mix(0.55), "Person")),
            )
            .child(ui::refresh_control(
                "branches-refresh",
                self.loading,
                self.map
                    .as_ref()
                    .map(|m| crate::account::ago(m.fetched, now)),
                cx.listener(|this, _, _, cx| this.refresh(cx)),
            ));

        let body: AnyElement = match self.map.clone() {
            None if self.root.is_none() => ui::empty_state(
                "git-branch",
                "No project",
                "Open a project to see its branches.",
                None,
            )
            .into_any_element(),
            None => div()
                .pt(px(60.))
                .child(ui::loading("branches-first", "Reading branches…"))
                .into_any_element(),
            Some(map) if map.main.is_empty() => ui::empty_state(
                "git-branch",
                "No default branch",
                "This project has no main or master branch to compare with.",
                None,
            )
            .into_any_element(),
            Some(map) => {
                let (rows, merged, idle) = self.rows(&map, now);
                let detail = self
                    .selected
                    .as_deref()
                    .and_then(|n| map.branch(n))
                    .map(|b| self.render_detail(&map, b, now, cx));
                div()
                    .flex()
                    .flex_col()
                    .gap(px(16.))
                    .child(self.render_summary(&map, now))
                    // No remote is not a failure: a quiet link in the header opens the field.
                    .when_some(
                        map.prs_error
                            .clone()
                            .filter(|e| e != branches::NO_REMOTE || self.show_repo),
                        |d, e| d.child(self.render_repo(&e, cx)),
                    )
                    .child(self.render_graph(&map, &rows, merged, idle, now, cx))
                    .children(detail)
                    .when(map.branches.is_empty(), |d| {
                        d.child(meta(format!("No branches besides {}.", map.main)))
                    })
                    .into_any_element()
            }
        };
        div()
            .track_focus(&self.focus)
            .flex()
            .flex_col()
            .size_full()
            .bg(theme::background())
            .font_family(theme::FONT_SANS)
            .text_color(theme::content())
            .child(header)
            .child(
                div()
                    .id("branches-body")
                    .track_scroll(&self.scroll)
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .child(
                        div()
                            .w_full()
                            .max_w(px(1200.))
                            .mx_auto()
                            .p(px(ui::PAGE_PAD))
                            .child(body),
                    ),
            )
    }
}

fn legend(color: Hsla, name: &'static str) -> Div {
    div()
        .flex()
        .items_center()
        .gap(px(4.))
        .child(div().size(px(7.)).rounded_full().bg(color))
        .child(name)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::branches::Commit;

    fn commit(sha: &str, time: u64) -> Commit {
        Commit {
            sha: sha.into(),
            time,
            author: String::new(),
            title: String::new(),
            who: Who::Person,
            pushed: true,
        }
    }

    #[test]
    fn checks_that_finished_are_told() {
        let pr = |n: u64, checks: Checks| branches::Pr {
            number: n,
            title: format!("PR {n}"),
            url: String::new(),
            state: "OPEN".into(),
            draft: false,
            review: String::new(),
            mergeable: String::new(),
            checks,
            author: String::new(),
            merged_at: 0,
            updated_at: 0,
        };
        let map = |prs: Vec<branches::Pr>| Map {
            branches: prs
                .into_iter()
                .map(|p| Branch {
                    name: format!("b{}", p.number),
                    pr: Some(p),
                    ..Default::default()
                })
                .collect(),
            ..Default::default()
        };
        let old = map(vec![
            pr(1, Checks::Pending),
            pr(2, Checks::Pending),
            pr(3, Checks::Passed),
        ]);
        let new = map(vec![
            pr(1, Checks::Passed),
            pr(2, Checks::Failed),
            pr(3, Checks::Failed),
            pr(4, Checks::Failed),
        ]);
        assert_eq!(
            finished_checks(&old, &new),
            vec![
                ("Checks passed on #1".to_string(), "PR 1".to_string(), false),
                ("Checks failed on #2".to_string(), "PR 2".to_string(), true)
            ]
        );
    }

    #[test]
    fn time_axis_only_spends_room_on_commits() {
        let map = Map {
            main: "main".into(),
            main_commits: vec![commit("m3", 900), commit("m2", 500), commit("m1", 100)],
            branches: vec![Branch {
                name: "b".into(),
                commits: vec![commit("b1", 600)],
                fork: Some("m2".into()),
                time: 600,
                ahead: 1,
                ..Default::default()
            }],
            ..Default::default()
        };
        let view_rows = [
            Row {
                branch: None,
                status: Status::Working,
                env: None,
            },
            Row {
                branch: Some(&map.branches[0]),
                status: Status::Working,
                env: None,
            },
        ];
        let lanes = Lanes::new(&map, &view_rows);
        assert_eq!(lanes.times, vec![900, 600, 500, 100]);
        // Columns from the right edge, one per commit, however far apart in time.
        assert_eq!(lanes.x(900, 100., 10), Some(100.));
        assert_eq!(lanes.x(600, 100., 10), Some(100. - COL_W));
        assert_eq!(lanes.x(100, 100., 3), None);
        assert_eq!(lanes.rows[0].fork, Some(500));
    }
}
