//! The Branches tab: the history of the default branch, newest first, with
//! each branch as a colored lane beside it (see `branch_history`). A branch
//! not in the default one yet shows at its newest commit with its
//! environment, the live session there and its pull request; a merged one
//! leaves the default line and comes back at the commit that took it in.
//! Pointing at a commit, or moving to it with the arrow keys, shows who wrote
//! it, with which agent, what it changed and its pull request. A selected
//! branch opens its PR, environment, sessions and commits, with a session to
//! fix conflicts or checks one click away.

use std::cell::Cell;
use std::collections::HashMap;
use std::convert::Infallible;
use std::path::PathBuf;
use std::rc::Rc;
use std::time::Duration;

use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, App, AppContext as _, BorderStyle, Bounds, Context, Div, EventEmitter, FocusHandle,
    FontWeight, Hsla, InteractiveElement as _, IntoElement, KeyDownEvent, ParentElement as _,
    PathBuilder, Pixels, Render, Role, ScrollHandle, SharedString, StatefulInteractiveElement as _,
    Styled as _, Task, Window, canvas, div, point, px, quad, size,
};

use crate::agent::AgentKind;
use crate::branch_history::{self, History, People, Source};
use crate::branches::{self, Branch, Checks, Commit, Map, Status, Who};
use crate::input::{InputEvent, TextInput};
use crate::loadable::Loadable;
use crate::model::Status as SessionStatus;
use crate::theme;
use crate::ui::{self, ButtonKind, icon, icon_button};

/// Reopening the tab reloads a map older than this.
const STALE: Duration = Duration::from_secs(60);
/// While the tab is closed the map is read this often, for the rail's badge.
const BACKGROUND: Duration = Duration::from_secs(5 * 60);
/// One commit per row; the lanes are painted on the same grid.
const ROW_H: f32 = 40.;
/// x of the default branch's line, and the room between lanes.
const MAIN_X: f32 = 24.;
const LANE_STEP: f32 = 20.;
/// Rows shown before the rest folds under "N more commits".
const SHOWN_ROWS: usize = 40;
/// Under this width the list drops the author, the PR number, branch names
/// and the `type(scope):` prefix, as in the Workspace panel.
const COMPACT_W: f32 = 640.;
const AUTHOR_W: f32 = 150.;
const TIME_W: f32 = 32.;
const CARD_W: f32 = 340.;
/// About the card's height, to tell if it fits under its row.
const CARD_H: f32 = 240.;

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
    pub agent: AgentKind,
    pub status: SessionStatus,
}

impl EnvInfo {
    /// The live session the branch row shows: one that needs you first, else
    /// one at work.
    fn live_session(&self) -> Option<&SessionInfo> {
        let find = |st: SessionStatus| self.sessions.iter().find(|s| s.status == st);
        find(SessionStatus::NeedsInput).or_else(|| find(SessionStatus::Working))
    }

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
    /// The project's map; reading it never fails, so it has no error.
    map: Loadable<Rc<Map>, Infallible>,
    loaded_at: Option<std::time::Instant>,
    selected: Option<String>,
    /// The row under the pointer, and the one the arrow keys moved to.
    hovered: Option<usize>,
    cursor: Option<usize>,
    /// The arrow keys moved last: the cursor's row shows its card.
    keyed: bool,
    /// Every commit read, not only the first [`SHOWN_ROWS`].
    show_all: bool,
    /// The branches that got no lane, listed above the history.
    show_folded: bool,
    /// The field to link a GitHub repository, for a repository without a remote.
    show_repo: bool,
    /// The width this view got last frame; it decides the layout of this one.
    width: Rc<Cell<f32>>,
    /// Where the history list started last frame, in window pixels, to
    /// scroll the row the arrow keys moved to into view.
    list_y: Rc<Cell<f32>>,
    focus: FocusHandle,
    /// The two folds, reachable with Tab.
    more_focus: FocusHandle,
    folded_focus: FocusHandle,
    scroll: ScrollHandle,
    /// `owner/name` of the GitHub repository to read pull requests from.
    repo: gpui::Entity<TextInput>,
    repo_error: Option<String>,
    _repo: gpui::Subscription,
    /// Who is who among the authors of `map`, worked out with it.
    people: Rc<People>,
    /// GitHub pictures by lower-case login; None while one downloads or
    /// when it failed. Read from disk or GitHub off the UI thread, once.
    avatars: HashMap<String, Option<PathBuf>>,
    _avatars: Option<Task<()>>,
}

impl EventEmitter<BranchesEvent> for BranchesView {}

/// What one row is, worked out once per frame for its text, its card and
/// what a click does.
struct RowInfo<'a> {
    commit: &'a Commit,
    /// On a branch's lane (dimmer), not on the default branch.
    side: bool,
    /// The newest commit of a branch not in the default one yet.
    tip: Option<&'a Branch>,
    /// The merged branch that comes back into the default line here.
    merged: Option<&'a Branch>,
    /// The branch a click selects and the card names.
    branch: Option<&'a Branch>,
    pr: Option<&'a branches::Pr>,
    /// The PR number from a squash title, when its PR wasn't read.
    pr_number: Option<u64>,
    /// The title without " (#N)".
    title: &'a str,
    lane: Option<Hsla>,
    tag: Option<&'a str>,
    /// The author's name, one per person, and their GitHub login.
    author: &'a str,
    login: Option<&'a str>,
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
            map: Loadable::default(),
            loaded_at: None,
            selected: None,
            hovered: None,
            cursor: None,
            keyed: false,
            show_all: false,
            show_folded: false,
            show_repo: false,
            people: Rc::default(),
            avatars: HashMap::new(),
            _avatars: None,
            width: Rc::new(Cell::new(1200.)),
            list_y: Rc::new(Cell::new(0.)),
            focus: cx.focus_handle(),
            more_focus: cx.focus_handle().tab_index(0).tab_stop(true),
            folded_focus: cx.focus_handle().tab_index(0).tab_stop(true),
            scroll: ScrollHandle::new(),
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
            self.map.reset();
            self.selected = None;
            self.hovered = None;
            self.cursor = None;
            self.show_all = false;
            self.loaded_at = None;
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
        self.map.data.as_ref().is_some_and(|m| {
            m.branches
                .iter()
                .filter_map(|b| b.pr.as_ref())
                .any(|p| p.open() && p.checks == Checks::Pending)
        })
    }

    /// Each branch with how far it is ahead of the default one, for the palette.
    pub fn branch_ahead(&self) -> Vec<(String, u64, u64)> {
        self.map.data.as_ref().map_or_else(Vec::new, |m| {
            m.branches
                .iter()
                .map(|b| (b.name.clone(), b.ahead, b.behind))
                .collect()
        })
    }

    /// Every branch read, with its PR's number and title: what Ctrl+K can jump to.
    pub fn branch_list(&self) -> Vec<(String, Option<(u64, String)>)> {
        self.map.data.as_ref().map_or_else(Vec::new, |m| {
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
        let Some(map) = &self.map.data else { return 0 };
        let now = crate::model::unix_now();
        map.branches
            .iter()
            .filter(|b| matches!(self.status_of(b, now), Status::Conflict | Status::Failing))
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
        if self.map.loading() {
            return;
        }
        self.loaded_at = Some(std::time::Instant::now());
        let r = root.clone();
        self.map.start(
            cx,
            |this| &mut this.map,
            async move {
                let map = branches::load(&r);
                let people = branch_history::people(&map);
                Some((map, people))
            },
            move |this, got, cx| {
                // A read that panicked still lands, as an empty map.
                let (map, people) = got.unwrap_or_default();
                if this.root.as_ref() != Some(&root) {
                    return;
                }
                if let Some(old) = &this.map.data {
                    for (title, body, failed) in finished_checks(old, &map) {
                        cx.emit(BranchesEvent::ChecksDone {
                            title,
                            body,
                            failed,
                        });
                    }
                }
                this.map.settle(Ok(Rc::new(map)));
                this.people = Rc::new(people);
                this.fetch_avatars(cx);
                // Rows moved: the card would describe another commit.
                this.hovered = None;
                this.cursor = None;
            },
        );
    }
    /// Pictures of the people with a GitHub login that aren't asked for yet:
    /// from the GitHub tab's cache on disk, else downloaded, in the
    /// background. A render only reads what arrived.
    fn fetch_avatars(&mut self, cx: &mut Context<Self>) {
        let want: Vec<String> = self
            .people
            .logins()
            .into_iter()
            .filter(|l| !self.avatars.contains_key(l))
            .collect();
        if want.is_empty() || self._avatars.is_some() {
            return;
        }
        for l in &want {
            self.avatars.insert(l.clone(), None);
        }
        self._avatars = Some(cx.spawn(async move |this, cx| {
            let got = cx
                .background_spawn(crate::guard::task_or_default(async move {
                    let dir = crate::github::avatars::dir();
                    let one = |login: String| {
                        let path = match crate::github::avatars::cached(&dir, &login) {
                            Some((path, false)) => Some(path),
                            // Due again: a failed download keeps the old one.
                            Some((path, true)) => crate::github::avatars::download(&dir, &login)
                                .ok()
                                .or(Some(path)),
                            None => crate::github::avatars::download(&dir, &login).ok(),
                        };
                        (login, path)
                    };
                    // Four at a time, like the GitHub tab.
                    let size = want.len().div_ceil(4).max(1);
                    std::thread::scope(|sc| {
                        let jobs: Vec<_> = want
                            .chunks(size)
                            .map(|part| {
                                let one = &one;
                                sc.spawn(move || part.iter().cloned().map(one).collect::<Vec<_>>())
                            })
                            .collect();
                        jobs.into_iter()
                            .flat_map(|j| j.join().unwrap_or_default())
                            .collect::<Vec<_>>()
                    })
                }))
                .await;
            let _ = this.update(cx, |this, cx| {
                this._avatars = None;
                for (login, path) in got {
                    this.avatars.insert(login, path);
                }
                // People who showed up while these were read.
                this.fetch_avatars(cx);
                cx.notify();
            });
        }));
    }

    fn picture(&self, login: Option<&str>) -> Option<&std::path::Path> {
        self.avatars.get(login?)?.as_deref()
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

    /// Where pull requests can't be read: why, and a field to name the
    /// repository. When `gh` is missing or signed out, the button that fixes
    /// that instead: no repository name helps then.
    fn render_repo(&self, error: &str, cx: &mut Context<Self>) -> Div {
        let fix = crate::github_view::gh_fix("branches-gh-fix", error);
        let gh_problem = fix.is_some();
        let card = div()
            .flex()
            .flex_col()
            .gap(crate::ui::sp(8.))
            .p(crate::ui::sp(12.))
            .rounded(crate::ui::rad(ui::RADIUS_LG))
            .border_1()
            .border_color(theme::stroke())
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(crate::ui::sp(8.))
                    .text_size(px(ui::TEXT_SM))
                    .child(icon("git-pull-request", 14.).text_color(theme::text_subtle()))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .child(format!("Pull requests unavailable: {error}")),
                    )
                    .children(fix),
            );
        if gh_problem {
            return card;
        }
        card.child(
            div()
                .flex()
                .items_center()
                .gap(crate::ui::sp(8.))
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
                        .px(crate::ui::sp(8.))
                        .rounded(crate::ui::rad(ui::RADIUS))
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

    fn status_of(&self, b: &Branch, now: u64) -> Status {
        let env = self.env_of(&b.name);
        b.status(env.is_some(), env.is_some_and(EnvInfo::working), now)
    }

    /// Selects `name`; its detail opens above the history.
    pub fn reveal(&mut self, name: String, cx: &mut Context<Self>) {
        self.selected = Some(name);
        cx.notify();
    }

    fn select(&mut self, name: Option<String>, cx: &mut Context<Self>) {
        self.selected = if self.selected == name { None } else { name };
        cx.notify();
    }

    /// Inactive branches get no lane: they would only push the live ones aside.
    fn history(&self, map: &Map, now: u64) -> History {
        branch_history::layout(map, |b| self.status_of(b, now) != Status::Idle)
    }

    /// How many rows show: the first [`SHOWN_ROWS`], and far enough for every
    /// lane that starts in them to come back into the default line.
    fn shown_rows(&self, h: &History) -> usize {
        if self.show_all {
            return h.rows.len();
        }
        let lanes_end = h
            .lanes
            .iter()
            .filter(|l| l.top < SHOWN_ROWS)
            .filter_map(|l| l.bottom)
            .max()
            .map_or(0, |b| b + 1);
        SHOWN_ROWS.max(lanes_end).min(h.rows.len())
    }

    /// Selects the branch of row `r`, the way a click on it does.
    fn activate(&mut self, r: usize, cx: &mut Context<Self>) {
        let Some(map) = self.map.data.clone() else {
            return;
        };
        let h = self.history(&map, crate::model::unix_now());
        if r >= h.rows.len() {
            return;
        }
        let people = self.people.clone();
        let info = row_info(&map, &h, &pr_index(&map), &HashMap::new(), &people, r);
        if let Some(b) = info.branch {
            self.select(Some(b.name.clone()), cx);
        }
    }

    fn on_key(&mut self, ev: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let k = &ev.keystroke;
        // Keys typed in the repository field are text, not moves.
        if k.modifiers.control
            || k.modifiers.alt
            || k.modifiers.platform
            || !self.focus.is_focused(window)
        {
            return;
        }
        let Some(map) = self.map.data.clone() else {
            return;
        };
        let h = self.history(&map, crate::model::unix_now());
        let n = self.shown_rows(&h);
        if n == 0 {
            return;
        }
        let at = self.cursor.or(self.hovered);
        match k.key.as_str() {
            "down" | "j" => self.cursor = Some(at.map_or(0, |r| (r + 1).min(n - 1))),
            "up" | "k" => self.cursor = Some(at.map_or(0, |r| r.saturating_sub(1))),
            "home" => self.cursor = Some(0),
            "end" => self.cursor = Some(n - 1),
            "enter" | "space" => {
                if let Some(r) = at {
                    self.activate(r, cx);
                }
            }
            "escape" if self.selected.is_some() => self.selected = None,
            "escape" => self.cursor = None,
            _ => return,
        }
        self.hovered = None;
        self.keyed = true;
        if let Some(r) = self.cursor {
            self.scroll_to_row(r);
        }
        cx.stop_propagation();
        cx.notify();
    }

    /// Scrolls just enough for row `r` and the card under it to show.
    fn scroll_to_row(&self, r: usize) {
        let view = self.scroll.bounds();
        let (top, bottom) = (f32::from(view.top()), f32::from(view.bottom()));
        let y = self.list_y.get() + r as f32 * ROW_H;
        let mut offset = self.scroll.offset();
        if y < top {
            offset.y += px(top - y);
        } else if y + ROW_H > bottom {
            offset.y -= px(y + ROW_H - bottom);
        }
        self.scroll.set_offset(offset);
    }

    fn render_header(
        &self,
        map: Option<&Map>,
        compact: bool,
        now: u64,
        cx: &mut Context<Self>,
    ) -> Div {
        let open = map.map_or(0, |m| {
            m.branches
                .iter()
                .filter(|b| {
                    branch_history::not_in_main(b) && !branch_history::integration(b, &m.main)
                })
                .count()
        });
        let no_remote = map.is_some_and(|m| m.prs_error.as_deref() == Some(branches::NO_REMOTE));
        div()
            .flex()
            .flex_none()
            .items_center()
            .gap(crate::ui::sp(10.))
            .h(px(ui::TOOLBAR_H))
            .px(crate::ui::sp(16.))
            .border_b_1()
            .border_color(theme::stroke())
            .when(!compact, |d| {
                d.child(
                    div()
                        .flex_none()
                        .text_size(px(ui::TEXT_LG))
                        .font_weight(FontWeight::SEMIBOLD)
                        .child("History"),
                )
            })
            .when_some(map, |d, m| {
                d.child(
                    div()
                        .flex()
                        .flex_none()
                        .items_center()
                        .gap(crate::ui::sp(6.))
                        .h(px(26.))
                        .px(crate::ui::sp(8.))
                        .rounded(crate::ui::rad(ui::RADIUS))
                        .border_1()
                        .border_color(theme::border())
                        .child(icon("git-branch", 12.).text_color(theme::text_muted()))
                        .child(
                            div()
                                .font_family(theme::FONT_MONO)
                                .text_size(px(ui::TEXT_SM))
                                .child(m.main.clone()),
                        ),
                )
                .child(
                    div()
                        .flex_none()
                        .font_family(theme::FONT_MONO)
                        .text_size(px(ui::TEXT_SM))
                        .text_color(theme::text_subtle())
                        .child(m.main_total.to_string()),
                )
                // Set by hand for a clone without a GitHub remote.
                .when_some(m.github.clone().filter(|_| !compact), |d, g| {
                    d.child(
                        div()
                            .min_w_0()
                            .truncate()
                            .text_size(px(ui::TEXT_SM))
                            .text_color(theme::text_faint())
                            .child(format!("PRs from {g}")),
                    )
                })
            })
            .child(div().flex_1())
            .when(no_remote && !compact, |d| {
                d.child(
                    div()
                        .id("branches-link-repo")
                        .flex_none()
                        .text_size(px(ui::TEXT_SM))
                        .text_color(theme::text_faint())
                        .cursor_pointer()
                        .hover(|s| s.text_color(theme::content()))
                        .child("No GitHub remote · Link a repo")
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.show_repo = !this.show_repo;
                            cx.notify();
                        })),
                )
            })
            .when(open > 0, |d| {
                let text = match (compact, open) {
                    (true, n) => format!("{n} not in main"),
                    (false, 1) => "1 branch not in main".to_string(),
                    (false, n) => format!("{n} branches not in main"),
                };
                d.child(
                    div()
                        .flex_none()
                        .text_size(px(ui::TEXT_SM))
                        .text_color(theme::text_subtle())
                        .child(text),
                )
            })
            .child(ui::refresh_control(
                "branches-refresh",
                self.map.loading(),
                map.filter(|_| !compact)
                    .map(|m| crate::account::ago(m.fetched, now)),
                cx.listener(|this, _, _, cx| this.refresh(cx)),
            ))
    }

    /// A fold that opens with a click, or with Enter or Space once Tab
    /// reached it (GPUI turns those keys on a focused element into a click).
    #[allow(clippy::too_many_arguments)]
    fn disclosure(
        &self,
        id: &'static str,
        focus: &FocusHandle,
        open: bool,
        label: String,
        lead: f32,
        height: f32,
        cx: &mut Context<Self>,
        flip: fn(&mut Self),
    ) -> gpui::Stateful<Div> {
        ui::labelled(div().id(id), Role::Button, label.clone())
            .aria_expanded(open)
            .track_focus(focus)
            .flex()
            .flex_none()
            .items_center()
            .gap(crate::ui::sp(6.))
            .h(px(height))
            .rounded(crate::ui::rad(ui::RADIUS))
            .text_size(px(ui::TEXT_SM))
            .text_color(theme::text_subtle())
            .cursor_pointer()
            .hover(|s| s.text_color(theme::content()))
            .focus_visible(|s| s.text_color(theme::content()).bg(theme::fill_hover()))
            .child(div().flex_none().w(px(lead)))
            .child(
                icon(if open { "arrow-up01" } else { "arrow-down01" }, 12.)
                    .text_color(theme::text_subtle()),
            )
            .child(label)
            .on_click(cx.listener(move |this, _, _, cx| {
                flip(this);
                cx.notify();
            }))
    }

    /// The branches not in the default one that got no lane: a fold above
    /// the history, opened into a row per branch.
    fn render_folded(
        &self,
        map: &Map,
        h: &History,
        gutter: f32,
        now: u64,
        cx: &mut Context<Self>,
    ) -> Option<Div> {
        let n = h.folded.len();
        if n == 0 {
            return None;
        }
        let label = if n == 1 {
            "1 more branch not in main".to_string()
        } else {
            format!("{n} more branches not in main")
        };
        let toggle = self.disclosure(
            "history-folded",
            &self.folded_focus,
            self.show_folded,
            label,
            gutter - 18.,
            32.,
            cx,
            |this| this.show_folded = !this.show_folded,
        );
        let rows = self.show_folded.then(|| {
            div().flex().flex_col().children(h.folded.iter().map(|&bi| {
                let b = &map.branches[bi];
                let env = self.env_of(&b.name);
                let status = self.status_of(b, now);
                let name = b.name.clone();
                let selected = self.selected.as_deref() == Some(b.name.as_str());
                ui::labelled(
                    div().id(SharedString::from(format!("history-folded-{bi}"))),
                    Role::Button,
                    format!("{}, {}", b.name, status.label()),
                )
                .flex()
                .items_center()
                .gap(crate::ui::sp(8.))
                .h(px(32.))
                .pr(crate::ui::sp(12.))
                .rounded(crate::ui::rad(ui::RADIUS))
                .cursor_pointer()
                .when(selected, |d| d.bg(theme::fill_selected()))
                .hover(|s| s.bg(theme::fill_hover()))
                .child(div().flex_none().w(px(gutter)))
                .child(
                    div()
                        .flex_none()
                        .size(px(6.))
                        .rounded_full()
                        .bg(status_color(status)),
                )
                .child(
                    div()
                        .min_w_0()
                        .truncate()
                        .font_family(theme::FONT_MONO)
                        .text_size(px(ui::TEXT_SM))
                        .child(b.name.clone()),
                )
                .when_some(env, |d, e| {
                    d.child(icon("folder01", 11.).text_color(theme::text_subtle()))
                        .child(meta(e.name.clone()))
                })
                .child(div().flex_1())
                .children(row_status(b, status, env, now))
                .on_click(cx.listener(move |this, _, _, cx| this.select(Some(name.clone()), cx)))
            }))
        });
        Some(
            div()
                .flex()
                .flex_col()
                .pb(crate::ui::sp(4.))
                .child(toggle)
                .children(rows),
        )
    }

    fn render_history(
        &self,
        map: &Map,
        h: &History,
        shown: usize,
        compact: bool,
        now: u64,
        cx: &mut Context<Self>,
    ) -> Div {
        let by_pr = pr_index(map);
        let tags = tag_index(map);
        let gutter = gutter(h, shown);
        let active = self
            .hovered
            .or(self.cursor.filter(|_| self.keyed))
            .filter(|r| *r < shown);
        let more = h.rows.len() - shown;

        let mut list = div().relative().flex().flex_col();
        let mut card = None;
        for r in 0..shown {
            let info = row_info(map, h, &by_pr, &tags, &self.people, r);
            if active == Some(r) {
                card = Some(self.render_card(map, &info, now));
            }
            list =
                list.child(self.render_row(&info, r, active == Some(r), compact, gutter, now, cx));
        }
        if more > 0 {
            let label = if more == 1 {
                "1 more commit".to_string()
            } else {
                format!("{more} more commits")
            };
            list = list.child(self.disclosure(
                "history-more",
                &self.more_focus,
                false,
                label,
                gutter,
                ROW_H,
                cx,
                |this| this.show_all = true,
            ));
        }
        let rows = shown + usize::from(more > 0);
        let graph = Graph::new(map, h, shown, more > 0);
        let list_y = self.list_y.clone();
        list = list.child(
            canvas(
                |_, _, _| {},
                move |bounds, _, window, _| {
                    list_y.set(f32::from(bounds.origin.y));
                    graph.paint(bounds, window);
                },
            )
            .absolute()
            .top_0()
            .left_0()
            .w(px(gutter))
            .h(px(rows as f32 * ROW_H)),
        );
        if let (Some(r), Some(card)) = (active, card) {
            // Under its row, unless the view ends before the card would:
            // then above it, where there is room.
            let view = self.scroll.bounds();
            let row_y = self.list_y.get() + r as f32 * ROW_H;
            let below = row_y + ROW_H + CARD_H <= f32::from(view.bottom());
            let above = row_y - CARD_H >= f32::from(view.top());
            let card = if below || !above {
                card.top(px((r + 1) as f32 * ROW_H + 2.))
            } else {
                card.bottom(px((rows - r) as f32 * ROW_H + 2.))
            };
            let card = if compact {
                card.left(px(gutter)).right(px(8.))
            } else {
                card.w(px(CARD_W)).right(px(AUTHOR_W + TIME_W + 40.))
            };
            list = list.child(card);
        }
        list
    }

    #[allow(clippy::too_many_arguments)]
    fn render_row(
        &self,
        info: &RowInfo,
        r: usize,
        active: bool,
        compact: bool,
        gutter: f32,
        now: u64,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let c = info.commit;
        let title = if compact {
            branch_history::without_type(info.title)
        } else {
            info.title
        };
        let number = info.pr.map(|p| p.number).or(info.pr_number);
        let mut tags: Vec<AnyElement> = Vec::new();
        if let Some(b) = info.tip {
            if !compact {
                tags.push(branch_label(b.name.clone(), info.lane));
                if let Some(e) = self.env_of(&b.name) {
                    tags.push(
                        div()
                            .flex()
                            .flex_none()
                            .items_center()
                            .gap(crate::ui::sp(4.))
                            .child(icon("folder01", 12.).text_color(theme::text_subtle()))
                            .child(
                                div()
                                    .font_family(theme::FONT_MONO)
                                    .text_size(px(ui::TEXT_XS))
                                    .text_color(theme::text_muted())
                                    .child(e.name.clone()),
                            )
                            .into_any_element(),
                    );
                    if let Some(s) = e.live_session() {
                        tags.push(
                            div()
                                .flex()
                                .flex_none()
                                .items_center()
                                .gap(crate::ui::sp(4.))
                                .child(s.agent.icon(12.))
                                .child(
                                    div()
                                        .text_size(px(ui::TEXT_SM))
                                        .text_color(session_color(s.status))
                                        .child(session_label(s.status)),
                                )
                                .into_any_element(),
                        );
                    }
                }
            }
            if b.pr.as_ref().is_some_and(|p| p.open() && p.draft) {
                tags.push(ui::tag("Draft", theme::pr_draft()).into_any_element());
            }
            let status = self.status_of(b, now);
            if matches!(status, Status::Conflict | Status::Failing) {
                tags.push(ui::tag(status.label(), theme::danger()).into_any_element());
            }
        }
        if let (Some(b), false) = (info.merged, compact) {
            tags.push(branch_label(format!("merged {}", b.name), info.lane));
        }
        if let Some(t) = info.tag {
            tags.push(ui::tag(t.to_string(), theme::content()).into_any_element());
        }

        ui::labelled(
            div().id(SharedString::from(format!("history-row-{r}"))),
            Role::Button,
            row_label(info, now),
        )
        .aria_selected(active)
        .flex()
        .items_center()
        .gap(crate::ui::sp(8.))
        .h(px(ROW_H))
        .flex_none()
        .pr(crate::ui::sp(12.))
        .rounded(crate::ui::rad(ui::RADIUS))
        .when(active, |d| d.bg(theme::fill_hover()))
        .when(info.branch.is_some(), |d| d.cursor_pointer())
        .child(div().flex_none().w(px(gutter)))
        .child(
            div()
                .min_w_0()
                .flex_shrink(1.)
                .truncate()
                .text_size(px(ui::TEXT_MD))
                .text_color(if info.side {
                    theme::text_subtle()
                } else {
                    theme::content()
                })
                .child(title.to_string()),
        )
        .when_some(number.filter(|_| !compact), |d, n| {
            d.child(
                div()
                    .flex_none()
                    .font_family(theme::FONT_MONO)
                    .text_size(px(ui::TEXT_XS))
                    .text_color(theme::text_faint())
                    .child(format!("#{n}")),
            )
        })
        .children(tags)
        .child(div().flex_1().min_w(px(8.)))
        .child(
            div()
                .flex()
                .flex_none()
                .items_center()
                .gap(crate::ui::sp(8.))
                .when(!compact, |d| d.w(px(AUTHOR_W)))
                .when(info.side, |d| d.opacity(0.7))
                .child(face(c, info.author, self.picture(info.login), 20.))
                .when(!compact, |d| {
                    d.child(
                        div()
                            .min_w_0()
                            .truncate()
                            .text_size(px(ui::TEXT_SM))
                            .text_color(theme::text_subtle())
                            .child(info.author.to_string()),
                    )
                }),
        )
        .child(
            div()
                .flex_none()
                .w(px(TIME_W))
                .text_right()
                .text_size(px(ui::TEXT_SM))
                .text_color(theme::text_subtle())
                .child(branch_history::short_ago(c.time, now)),
        )
        .on_hover(cx.listener(move |this, hovered: &bool, _, cx| {
            if *hovered && this.hovered != Some(r) {
                this.hovered = Some(r);
                this.keyed = false;
                cx.notify();
            } else if !*hovered && this.hovered == Some(r) {
                this.hovered = None;
                cx.notify();
            }
        }))
        .on_click(cx.listener(move |this, _, window, cx| {
            window.focus(&this.focus, cx);
            this.cursor = Some(r);
            this.keyed = false;
            this.activate(r, cx);
        }))
        .into_any_element()
    }

    /// What pointing at a commit tells: who wrote it and with which agent,
    /// its sha, what it changed, when, its branch and its pull request.
    fn render_card(&self, map: &Map, info: &RowInfo, now: u64) -> Div {
        let c = info.commit;
        let fact = |label: &'static str, value: AnyElement| {
            div()
                .flex()
                .items_center()
                .gap(crate::ui::sp(12.))
                .text_size(px(ui::TEXT_SM))
                .child(
                    div()
                        .flex_none()
                        .w(px(92.))
                        .text_color(theme::text_subtle())
                        .child(label),
                )
                .child(
                    div()
                        .min_w_0()
                        .flex()
                        .items_center()
                        .gap(crate::ui::sp(6.))
                        .child(value),
                )
        };
        let mono = |text: String| {
            div()
                .truncate()
                .font_family(theme::FONT_MONO)
                .child(text)
                .into_any_element()
        };
        let branch = info.branch.map_or(map.main.clone(), |b| b.name.clone());
        let pr = info.pr.or(info.branch.and_then(|b| b.pr.as_ref()));
        div()
            .absolute()
            .flex()
            .flex_col()
            .gap(crate::ui::sp(10.))
            .p(crate::ui::sp(14.))
            .rounded(crate::ui::rad(ui::RADIUS_LG))
            .bg(theme::dialog())
            .border_1()
            .border_color(theme::border())
            .shadow_lg()
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(crate::ui::sp(10.))
                    .child(face(c, info.author, self.picture(info.login), 28.))
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .min_w_0()
                            .child(
                                div()
                                    .truncate()
                                    .text_size(px(ui::TEXT_MD))
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .child(info.author.to_string()),
                            )
                            .when_some(c.coauthor.clone(), |d, who| {
                                d.child(
                                    div()
                                        .truncate()
                                        .text_size(px(ui::TEXT_SM))
                                        .text_color(theme::text_subtle())
                                        .child(format!("Co-authored with {who}")),
                                )
                            }),
                    ),
            )
            .child(div().h(px(1.)).bg(theme::border_subtle()))
            .child(fact("Commit", mono(c.short().to_string())))
            .when_some(c.stat, |d, s| {
                d.child(fact(
                    "Changed",
                    div()
                        .flex()
                        .gap(crate::ui::sp(6.))
                        .child(format!(
                            "{} {}",
                            s.files,
                            if s.files == 1 { "file" } else { "files" }
                        ))
                        .child(
                            div()
                                .text_color(theme::diff_add())
                                .child(format!("+{}", s.added)),
                        )
                        .child(
                            div()
                                .text_color(theme::diff_del())
                                .child(format!("−{}", s.removed)),
                        )
                        .into_any_element(),
                ))
            })
            .child(fact(
                "Committed",
                div()
                    .child(branch_history::long_ago(c.time, now))
                    .into_any_element(),
            ))
            .child(fact("Branch", mono(branch)))
            .when_some(pr, |d, p| {
                let (word, color): (&str, Hsla) = if p.merged() {
                    ("Merged", theme::pr_merged().into())
                } else if p.open() && p.draft {
                    ("Draft", theme::pr_draft().into())
                } else if p.open() {
                    ("Open", theme::pr_open().into())
                } else {
                    ("Closed", theme::pr_closed().into())
                };
                d.child(fact(
                    "Pull request",
                    ui::tag(format!("#{} {word}", p.number), color).into_any_element(),
                ))
            })
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
                .gap(crate::ui::sp(4.))
                .p(crate::ui::sp(10.))
                .rounded(crate::ui::rad(ui::RADIUS))
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

        let sessions =
            env.map(|e| {
                let (project, envi) = (e.project, e.env);
                div().flex().flex_wrap().gap(crate::ui::sp(6.)).children(
                    e.sessions.iter().take(6).map(|s| {
                        let id = s.id.clone();
                        div()
                            .id(SharedString::from(format!("branch-session-{}", s.id)))
                            .flex()
                            .items_center()
                            .gap(crate::ui::sp(6.))
                            .h(px(26.))
                            .px(crate::ui::sp(8.))
                            .max_w(px(260.))
                            .rounded(crate::ui::rad(ui::RADIUS))
                            .border_1()
                            .border_color(theme::stroke())
                            .cursor_pointer()
                            .hover(|st| st.bg(theme::fill_hover()))
                            .text_size(px(ui::TEXT_SM))
                            .child(s.agent.icon(12.))
                            .child(div().min_w_0().truncate().child(s.title.clone()))
                            .when(matches!(s.status, SessionStatus::Working), |d| {
                                d.child(ui::spinner(10., theme::working()))
                            })
                            .on_click(cx.listener(move |_, _, _, cx| {
                                cx.emit(BranchesEvent::OpenSession {
                                    project,
                                    env: envi,
                                    id: id.clone(),
                                })
                            }))
                    }),
                )
            });

        let commits = div().flex().flex_col().children(b.commits.iter().map(|c| {
            div()
                .flex()
                .items_center()
                .gap(crate::ui::sp(10.))
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
            .gap(crate::ui::sp(12.))
            .p(crate::ui::sp(16.))
            .rounded(crate::ui::rad(ui::RADIUS_LG))
            .border_1()
            .border_color(theme::stroke())
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(crate::ui::sp(10.))
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
                        icon_button("branch-close", "cancel01", 12., "Close")
                            .on_click(cx.listener(|this, _, _, cx| this.select(None, cx))),
                    ),
            )
            .when_some(b.pr.as_ref(), |d, pr| {
                d.child(div().text_size(px(ui::TEXT_MD)).child(pr.title.clone()))
            })
            .child(meta(facts.join(" · ")))
            .children(conflicts)
            .when(!actions.is_empty(), |d| {
                d.child(
                    div()
                        .flex()
                        .flex_wrap()
                        .gap(crate::ui::sp(8.))
                        .children(actions),
                )
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
        && let Some(s) = e.live_session()
    {
        out.push(
            div()
                .flex()
                .items_center()
                .gap(crate::ui::sp(4.))
                .child(s.agent.icon(12.))
                .child(ui::spinner(10., session_color(s.status)))
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

/// A live session's spinner: amber when it waits for you, blue while it works.
fn session_color(s: SessionStatus) -> Hsla {
    match s {
        SessionStatus::NeedsInput => theme::needs_you(),
        _ => theme::working(),
    }
}

fn status_color(s: Status) -> Hsla {
    match s {
        Status::Conflict | Status::Failing => theme::danger().into(),
        Status::Ready => theme::success().into(),
        Status::Review => theme::pr_open().into(),
        Status::Working => theme::working(),
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

/// Which branch has which pull request, by number.
fn pr_index(map: &Map) -> HashMap<u64, usize> {
    map.branches
        .iter()
        .enumerate()
        .filter_map(|(i, b)| b.pr.as_ref().map(|p| (p.number, i)))
        .collect()
}

/// Version tags ("v0.14.1") by the commit they point at.
fn tag_index(map: &Map) -> HashMap<&str, &str> {
    map.tags
        .iter()
        .filter(|(_, t)| {
            t.strip_prefix('v')
                .is_some_and(|v| v.starts_with(|c: char| c.is_ascii_digit()))
        })
        .map(|(sha, t)| (sha.as_str(), t.as_str()))
        .collect()
}

/// The room left of the titles: the default line and the lanes in use.
fn gutter(h: &History, shown: usize) -> f32 {
    let columns = h
        .lanes
        .iter()
        .filter(|l| l.top < shown)
        .map(|l| l.column)
        .max()
        .unwrap_or(0);
    MAIN_X + columns as f32 * LANE_STEP + 18.
}

fn row_info<'a>(
    map: &'a Map,
    h: &History,
    by_pr: &HashMap<u64, usize>,
    tags: &HashMap<&str, &'a str>,
    people: &'a People,
    r: usize,
) -> RowInfo<'a> {
    let (commit, side, tip) = match h.rows[r] {
        Source::Main(j) => (&map.main_commits[j], false, false),
        Source::Side { branch, commit } => {
            (&map.branches[branch].commits[commit], true, h.is_tip(r))
        }
    };
    let (title, pr_number) = branch_history::split_pr(&commit.title);
    let lane = h.lane_of(r).or_else(|| h.merged_at(r));
    let lane_branch = lane.map(|l| &map.branches[l.branch]);
    let tip = lane
        .filter(|l| tip && !l.merged)
        .map(|l| &map.branches[l.branch]);
    let merged = h.merged_at(r).map(|l| &map.branches[l.branch]);
    // A main commit's branch: by its PR number, else the branch it merged
    // (a merge commit, or a branch with no lane).
    let pr_branch = pr_number
        .and_then(|n| by_pr.get(&n))
        .map(|&i| &map.branches[i])
        .or_else(|| {
            map.branches
                .iter()
                .find(|b| !side && b.merged_by.as_deref() == Some(commit.sha.as_str()))
        });
    let pr = match (side, tip) {
        (_, Some(b)) => b.pr.as_ref(),
        (true, None) => None,
        (false, None) => pr_branch.and_then(|b| b.pr.as_ref()),
    };
    RowInfo {
        commit,
        side,
        tip,
        merged,
        branch: lane_branch.or(pr_branch),
        pr,
        pr_number: pr_number.filter(|_| !side),
        title,
        lane: lane.map(|l| theme::lane(l.color)),
        tag: tags.get(commit.sha.as_str()).copied(),
        author: people.name(commit),
        login: people.of(commit).and_then(|p| p.login.as_deref()),
    }
}

/// What a screen reader says for a row: the card's facts in one line.
fn row_label(info: &RowInfo, now: u64) -> String {
    let c = info.commit;
    let mut out = format!("{}, {}", info.title, info.author);
    if let Some(who) = &c.coauthor {
        out.push_str(&format!(" with {who}"));
    }
    out.push_str(&format!(", {}", branch_history::long_ago(c.time, now)));
    if let Some(b) = info.branch {
        out.push_str(&format!(", branch {}", b.name));
    }
    if let Some(n) = info.pr.map(|p| p.number).or(info.pr_number) {
        out.push_str(&format!(", pull request #{n}"));
    }
    if let Some(s) = c.stat {
        out.push_str(&format!(
            ", {} {} changed, {} added, {} removed",
            s.files,
            if s.files == 1 { "file" } else { "files" },
            s.added,
            s.removed
        ));
    }
    out
}

/// The agent that wrote a commit with its author: from the author or the
/// Co-Authored-By trailer.
fn agent_of(c: &Commit) -> Option<AgentKind> {
    match c.who {
        Who::Claude => return Some(AgentKind::Claude),
        Who::Droid => return Some(AgentKind::Droid),
        Who::Person => {}
    }
    let who = c.coauthor.as_deref()?.to_ascii_lowercase();
    [
        ("codex", AgentKind::Codex),
        ("openai", AgentKind::Codex),
        ("cursor", AgentKind::Cursor),
        ("devin", AgentKind::Devin),
        ("opencode", AgentKind::OpenCode),
    ]
    .into_iter()
    .find(|(name, _)| who.contains(name))
    .map(|(_, a)| a)
}

/// The author's avatar, with the mark of the agent that wrote it with them.
fn face(c: &Commit, name: &str, picture: Option<&std::path::Path>, size: f32) -> Div {
    let b = (size * 0.6).round();
    div()
        .relative()
        .flex_none()
        .size(px(size))
        .child(ui::avatar_image(name, picture, size))
        .when_some(agent_of(c), |d, agent| {
            d.child(
                div()
                    .absolute()
                    .right(px(-b * 0.3))
                    .bottom(px(-b * 0.3))
                    .flex()
                    .items_center()
                    .justify_center()
                    .size(px(b))
                    .rounded_full()
                    .bg(theme::background())
                    .child(agent.icon(b * 0.72)),
            )
        })
}

/// A branch name with its lane's dot.
fn branch_label(text: String, lane: Option<Hsla>) -> AnyElement {
    div()
        .flex()
        .flex_none()
        .items_center()
        .gap(crate::ui::sp(6.))
        .font_family(theme::FONT_MONO)
        .text_size(px(ui::TEXT_XS))
        .text_color(theme::text_subtle())
        .when_some(lane, |d, c| {
            d.child(div().flex_none().size(px(6.)).rounded_full().bg(c))
        })
        .child(text)
        .into_any_element()
}

fn session_label(s: SessionStatus) -> &'static str {
    match s {
        SessionStatus::NeedsInput => "Needs you",
        _ => "Working",
    }
}

/// The lanes and dots of the shown rows, copied out so the canvas can paint
/// them later.
#[derive(Clone)]
struct Graph {
    /// Rows painted, the fold row included.
    rows: usize,
    fold: bool,
    first_main: Option<usize>,
    lanes: Vec<LanePath>,
    dots: Vec<GraphDot>,
    line: Hsla,
    ground: Hsla,
}

#[derive(Clone)]
struct LanePath {
    x: f32,
    color: Hsla,
    top: usize,
    bottom: Option<usize>,
    merged: bool,
}

#[derive(Clone, Copy, PartialEq)]
enum DotKind {
    Main,
    /// A commit of the default branch that took a lane in: a ring.
    Merge,
    Tip,
    Side,
}

#[derive(Clone)]
struct GraphDot {
    row: usize,
    x: f32,
    kind: DotKind,
    color: Hsla,
    pushed: bool,
}

fn lane_x(column: usize) -> f32 {
    MAIN_X + column as f32 * LANE_STEP
}

impl Graph {
    fn new(map: &Map, h: &History, shown: usize, fold: bool) -> Self {
        let lanes: Vec<LanePath> = h
            .lanes
            .iter()
            .filter(|l| l.top < shown)
            .map(|l| LanePath {
                x: lane_x(l.column),
                color: if l.merged {
                    theme::lane(l.color).opacity(0.6)
                } else {
                    theme::lane(l.color)
                },
                top: l.top,
                bottom: l.bottom.filter(|b| *b < shown),
                merged: l.merged,
            })
            .collect();
        let dots = (0..shown)
            .map(|r| match h.rows[r] {
                Source::Main(_) => GraphDot {
                    row: r,
                    x: MAIN_X,
                    kind: if h.merged_at(r).is_some() {
                        DotKind::Merge
                    } else {
                        DotKind::Main
                    },
                    color: theme::text_muted(),
                    pushed: true,
                },
                Source::Side { branch, commit } => {
                    let lane = h.lane_of(r);
                    let c = &map.branches[branch].commits[commit];
                    GraphDot {
                        row: r,
                        x: lane.map_or(MAIN_X, |l| lane_x(l.column)),
                        kind: if h.is_tip(r) {
                            DotKind::Tip
                        } else {
                            DotKind::Side
                        },
                        color: lane.map_or(theme::text_muted(), |l| theme::lane(l.color)),
                        pushed: c.pushed,
                    }
                }
            })
            .collect();
        Self {
            rows: shown + usize::from(fold),
            fold,
            first_main: h.rows[..shown]
                .iter()
                .position(|s| matches!(s, Source::Main(_))),
            lanes,
            dots,
            line: theme::text_faint(),
            ground: theme::background(),
        }
    }

    fn paint(&self, bounds: Bounds<Pixels>, window: &mut Window) {
        let o = bounds.origin;
        let at = |x: f32, y: f32| point(o.x + px(x), o.y + px(y));
        let y = |row: usize| row as f32 * ROW_H + ROW_H / 2.;
        let shown = self.rows - usize::from(self.fold);
        let end = self.rows as f32 * ROW_H;
        let stroke = |from: (f32, f32), to: (f32, f32), color: Hsla, window: &mut Window| {
            let mut p = PathBuilder::stroke(px(2.));
            p.move_to(at(from.0, from.1));
            if from.0 == to.0 {
                p.line_to(at(to.0, to.1));
            } else {
                // Leaves straight up or down and arrives the same way.
                let mid = (from.1 + to.1) / 2.;
                p.cubic_bezier_to(at(to.0, to.1), at(from.0, mid), at(to.0, mid));
            }
            if let Ok(path) = p.build() {
                window.paint_path(path, color);
            }
        };

        // The default branch: one neutral line from its newest shown commit
        // down, dotted through the fold.
        if let Some(first) = self.first_main {
            let solid_end = if self.fold { y(shown - 1) } else { end };
            if solid_end > y(first) {
                stroke((MAIN_X, y(first)), (MAIN_X, solid_end), self.line, window);
            }
            if self.fold {
                let mut d = y(shown - 1) + 8.;
                while d < end - 4. {
                    stroke((MAIN_X, d), (MAIN_X, d + 2.), self.line, window);
                    d += 6.;
                }
            }
        }

        for l in &self.lanes {
            let start = if l.merged {
                stroke((MAIN_X, y(l.top)), (l.x, y(l.top + 1)), l.color, window);
                y(l.top + 1)
            } else {
                y(l.top)
            };
            let stop = match l.bottom {
                Some(b) => y(b.saturating_sub(1)),
                None => end,
            };
            if stop > start {
                stroke((l.x, start), (l.x, stop), l.color, window);
            }
            if let Some(b) = l.bottom {
                stroke(
                    (l.x, y(b.saturating_sub(1)).max(start)),
                    (MAIN_X, y(b)),
                    l.color,
                    window,
                );
            }
        }

        for d in &self.dots {
            // A commit not pushed yet is drawn faint.
            let alpha = if d.pushed { 1. } else { 0.45 };
            let (r, fill, ring) = match d.kind {
                DotKind::Main => (4.5, d.color, self.ground),
                DotKind::Merge => (5., self.ground, theme::content()),
                DotKind::Tip => (5., d.color.opacity(alpha), self.ground),
                DotKind::Side => (4., self.ground, d.color.opacity(alpha)),
            };
            let c = at(d.x, y(d.row));
            window.paint_quad(quad(
                Bounds::new(
                    point(c.x - px(r), c.y - px(r)),
                    size(px(r * 2.), px(r * 2.)),
                ),
                px(r),
                fill,
                px(2.),
                ring,
                BorderStyle::Solid,
            ));
        }
    }
}

impl Render for BranchesView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let now = crate::model::unix_now();
        // The width this view got last frame decides the layout of this one.
        let (cell, id) = (self.width.clone(), cx.entity_id());
        let measure = canvas(
            |_, _, _| {},
            move |bounds: Bounds<Pixels>, _, _: &mut Window, cx: &mut App| {
                let now = f32::from(bounds.size.width);
                if (now - cell.get()).abs() > 0.5 {
                    cell.set(now);
                    // A notify while the window paints marks no new frame, so
                    // send it once this one is drawn.
                    cx.defer(move |cx| cx.notify(id));
                }
            },
        )
        .absolute()
        .size_full();
        let compact = self.width.get() < COMPACT_W;
        let map = self.map.data.clone();
        let header = self.render_header(map.as_deref(), compact, now, cx);

        let detail = map.as_ref().and_then(|m| {
            let b = m.branch(self.selected.as_deref()?)?;
            Some(
                div()
                    .id("branches-detail")
                    .flex_none()
                    .max_h(px(360.))
                    .overflow_y_scroll()
                    .px(crate::ui::sp(8.))
                    .pt(crate::ui::sp(8.))
                    .child(self.render_detail(m, b, now, cx)),
            )
        });

        let body: AnyElement = match map.as_deref() {
            None if self.root.is_none() => ui::empty_state(
                "git-branch",
                "No project",
                "Open a project to see its history.",
                None,
            )
            .into_any_element(),
            None => div()
                .pt(crate::ui::sp(60.))
                .child(ui::loading("Reading history…"))
                .into_any_element(),
            Some(map) if map.main.is_empty() => ui::empty_state(
                "git-branch",
                "No default branch",
                "This project has no main or master branch to compare with.",
                None,
            )
            .into_any_element(),
            Some(map) => {
                let h = self.history(map, now);
                let shown = self.shown_rows(&h);
                let gutter = gutter(&h, shown);
                div()
                    .flex()
                    .flex_col()
                    // No remote is not a failure: a quiet link in the header opens the field.
                    .when_some(
                        map.prs_error
                            .clone()
                            .filter(|e| e != branches::NO_REMOTE || self.show_repo),
                        |d, e| d.child(div().pb(crate::ui::sp(8.)).child(self.render_repo(&e, cx))),
                    )
                    .children(self.render_folded(map, &h, gutter, now, cx))
                    .child(self.render_history(map, &h, shown, compact, now, cx))
                    .into_any_element()
            }
        };
        div()
            .track_focus(&self.focus)
            .on_key_down(cx.listener(Self::on_key))
            .relative()
            .flex()
            .flex_col()
            .size_full()
            .bg(theme::background())
            .font_family(theme::FONT_SANS)
            .text_color(theme::content())
            .child(measure)
            .child(header)
            .children(detail)
            .child(
                div()
                    .id("branches-body")
                    .track_scroll(&self.scroll)
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .child(div().w_full().p(crate::ui::sp(8.)).child(body)),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn commit(sha: &str, time: u64) -> Commit {
        Commit {
            sha: sha.into(),
            time,
            author: "Ana".into(),
            email: "ana@example.com".into(),
            merge: false,
            title: String::new(),
            who: Who::Person,
            coauthor: None,
            stat: None,
            pushed: true,
        }
    }

    #[test]
    fn a_session_that_needs_you_is_not_drawn_as_working() {
        assert_eq!(session_color(SessionStatus::NeedsInput), theme::needs_you());
        assert_eq!(session_color(SessionStatus::Working), theme::working());
        assert_eq!(session_label(SessionStatus::NeedsInput), "Needs you");
    }

    #[test]
    fn a_branch_shows_the_session_that_needs_you_first() {
        let session = |id: &str, status| SessionInfo {
            id: id.to_string().into(),
            title: SharedString::default(),
            agent: AgentKind::Claude,
            status,
        };
        let mut env = EnvInfo {
            project: 0,
            env: 0,
            path: PathBuf::new(),
            name: SharedString::default(),
            branch: String::new(),
            sessions: vec![
                session("idle", SessionStatus::Idle),
                session("busy", SessionStatus::Working),
                session("asks", SessionStatus::NeedsInput),
            ],
        };
        assert_eq!(env.live_session().map(|s| s.id.as_ref()), Some("asks"));
        env.sessions.pop();
        assert_eq!(env.live_session().map(|s| s.id.as_ref()), Some("busy"));
        env.sessions.pop();
        assert!(env.live_session().is_none());
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
    fn the_agent_mark_comes_from_the_author_or_the_trailer() {
        let mut c = commit("a", 0);
        assert_eq!(agent_of(&c), None);
        c.who = Who::Claude;
        assert_eq!(agent_of(&c), Some(AgentKind::Claude));
        c.who = Who::Person;
        c.coauthor = Some("Codex".into());
        assert_eq!(agent_of(&c), Some(AgentKind::Codex));
        c.coauthor = Some("cubic".into());
        assert_eq!(agent_of(&c), None);
    }

    #[test]
    fn a_row_knows_its_branch_pr_and_tag() {
        let mut main = commit("m1", 100);
        main.title = "perf(usage): read less (#672)".into();
        let mut side = commit("s1", 90);
        side.title = "perf(usage): first try".into();
        let map = Map {
            main: "main".into(),
            main_commits: vec![main, commit("m0", 50)],
            tags: vec![
                ("m0".into(), "v0.14.1".into()),
                ("m0".into(), "nightly".into()),
            ],
            branches: vec![Branch {
                name: "perf/usage".into(),
                commits: vec![side],
                fork: Some("m0".into()),
                merged_by: Some("m1".into()),
                pr: Some(branches::Pr {
                    number: 672,
                    title: String::new(),
                    url: String::new(),
                    state: "MERGED".into(),
                    draft: false,
                    review: String::new(),
                    mergeable: String::new(),
                    checks: Checks::None,
                    author: String::new(),
                    merged_at: 0,
                    updated_at: 0,
                }),
                ..Default::default()
            }],
            ..Default::default()
        };
        let h = branch_history::layout(&map, |_| true);
        let (by_pr, tags) = (pr_index(&map), tag_index(&map));
        let people = branch_history::people(&map);
        let merge = row_info(&map, &h, &by_pr, &tags, &people, 0);
        assert_eq!(merge.title, "perf(usage): read less");
        assert_eq!(merge.pr.map(|p| p.number), Some(672));
        assert_eq!(merge.merged.map(|b| b.name.as_str()), Some("perf/usage"));
        assert!(!merge.side && merge.tip.is_none());
        let side = row_info(&map, &h, &by_pr, &tags, &people, 1);
        assert!(side.side && side.tip.is_none() && side.pr.is_none());
        assert_eq!(side.branch.map(|b| b.name.as_str()), Some("perf/usage"));
        let last = row_info(&map, &h, &by_pr, &tags, &people, 2);
        assert_eq!(last.tag, Some("v0.14.1"));
        assert_eq!(gutter(&h, 3), MAIN_X + LANE_STEP + 18.);
        let label = row_label(&merge, 100);
        assert!(label.contains("branch perf/usage") && label.contains("#672"));
    }
}
