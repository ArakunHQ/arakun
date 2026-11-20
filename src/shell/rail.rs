//! The project rail on the far left: search, views and projects with their environments.

use super::*;
use crate::servers_view::Reach;

/// Height of an environment row under its project, and the space between rows.
const ENV_ROW_H: f32 = 28.;
const ENV_GAP: f32 = 2.;

const NEW_SESSION_TIP: &str = "A session in its own environment. Alt+click to pick the branch";
const NEW_ENV_TIP: &str =
    "New session in its own environment (Ctrl+Shift+N). Alt+click for options";

/// The one thing an environment row says at its end, the most urgent first.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum EnvSignal {
    NeedsYou,
    CiFailed,
    /// The pull request wants changes: what it says.
    Review(&'static str),
    /// Lines added and removed.
    Changes(u32, u32),
    Nothing,
}

/// What GitHub says about an environment's branch.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(super) struct GitHubState {
    /// The failed run, when CI on the branch fails.
    pub failed_run: Option<u64>,
    /// CI on the branch is still going.
    pub ci_running: bool,
    /// The pull request needs changes: what it says, and its address.
    pub review: Option<(&'static str, String)>,
    /// The branch's open pull request.
    pub pr: Option<PrLink>,
}

/// An open pull request as a row links to it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct PrLink {
    pub number: u64,
    pub title: String,
    pub url: String,
    pub draft: bool,
}

impl PrLink {
    /// The chip's tooltip: what the pull request is, and that a click opens it.
    fn tip(&self) -> String {
        let draft = if self.draft {
            "Draft pull request"
        } else {
            "Pull request"
        };
        format!("{draft} #{}: {}", self.number, self.title)
    }
}

impl GitHubState {
    /// A red build or a pull request that wants changes waits on the user.
    pub(super) fn needs_you(&self) -> bool {
        self.failed_run.is_some() || self.review.is_some()
    }

    fn of(ci: Option<ci::State>, failed_run: Option<u64>, pr: Option<&crate::ci::Pr>) -> Self {
        let review = pr.and_then(|pr| {
            let what = if pr.review() == crate::ci::Review::ChangesRequested {
                "Changes requested"
            } else if pr.conflicts() {
                "Conflicts"
            } else {
                return None;
            };
            Some((what, pr.url.clone()))
        });
        Self {
            failed_run: failed_run.filter(|_| ci == Some(ci::State::Failed)),
            ci_running: ci.is_some_and(|s| !s.done()),
            review,
            pr: pr.map(|pr| PrLink {
                number: pr.number,
                title: pr.title.clone(),
                url: pr.url.clone(),
                draft: pr.is_draft,
            }),
        }
    }
}

/// An environment's state from its sessions and GitHub together: CI that
/// fails or a review that wants changes needs the user; CI that runs counts
/// as work going on.
pub(super) fn env_status(sessions: Status, github: &GitHubState) -> Status {
    if sessions == Status::NeedsInput || github.needs_you() {
        Status::NeedsInput
    } else if sessions == Status::Working || github.ci_running {
        Status::Working
    } else {
        // Keeps a session's error.
        sessions
    }
}

/// A row says one thing so the list stays easy to scan: a session that needs
/// the user beats a red build, which beats a review, which beats the diff.
pub(super) fn env_signal(
    sessions: Status,
    github: &GitHubState,
    changes: Option<(u32, u32)>,
) -> EnvSignal {
    if sessions == Status::NeedsInput {
        EnvSignal::NeedsYou
    } else if github.failed_run.is_some() {
        EnvSignal::CiFailed
    } else if let Some((what, _)) = &github.review {
        EnvSignal::Review(what)
    } else {
        match changes {
            Some((a, r)) if a + r > 0 => EnvSignal::Changes(a, r),
            _ => EnvSignal::Nothing,
        }
    }
}

/// The diff as `+a -r`, the removals left out when there are none.
fn diff_label(a: u32, r: u32) -> gpui::Div {
    div()
        .flex()
        .gap(crate::ui::sp(3.))
        .text_size(px(ui::TEXT_XS))
        .font_family(theme::FONT_MONO)
        .child(div().text_color(theme::diff_add()).child(format!("+{a}")))
        .when(r > 0, |d| {
            d.child(div().text_color(theme::diff_del()).child(format!("-{r}")))
        })
}

/// The + at the end of a rail heading. Faint until the pointer is on it, so
/// a column of headings doesn't read as a column of buttons.
pub(super) fn heading_button(
    id: impl Into<gpui::ElementId>,
    tip: &'static str,
) -> gpui::Stateful<gpui::Div> {
    div()
        .id(id)
        .group("rail-heading-add")
        .flex()
        .flex_none()
        .items_center()
        .justify_center()
        .size(px(22.))
        .rounded(crate::ui::rad(6.))
        .cursor_pointer()
        .hover(|s| s.bg(theme::fill_selected()))
        .tooltip(ui::tip(tip))
        .child(hover_icon(
            "add01",
            14.,
            theme::text_subtle(),
            theme::content(),
            "rail-heading-add",
        ))
}

/// An environment whose worktree is still being made: there at once, with a
/// spinner, so a click on + answers right away.
fn pending_env_row(id: usize, name: String) -> gpui::Stateful<gpui::Div> {
    div()
        .id(("env-pending", id))
        .flex()
        .items_center()
        .gap(crate::ui::sp(8.))
        .h(px(ENV_ROW_H))
        .px(crate::ui::sp(8.))
        .rounded(crate::ui::rad(8.))
        .text_size(px(ui::TEXT_MD))
        .text_color(theme::text_muted())
        .child(
            div()
                .flex()
                .flex_none()
                .items_center()
                .justify_center()
                .size(px(14.))
                .child(ui::spinner(
                    ("env-pending-spin", id),
                    12.,
                    theme::text_faint(),
                )),
        )
        .child(div().flex_1().min_w_0().truncate().child(name))
        .child(
            div()
                .flex_none()
                .text_size(px(ui::TEXT_XS))
                .text_color(theme::text_faint())
                .child("Making environment"),
        )
}

/// What a project's environments are doing, so a project you are not looking
/// at can still say it needs you.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct Activity {
    pub needs_you: usize,
    pub working: usize,
}

impl Activity {
    /// From the state of each environment.
    pub(super) fn of(statuses: impl IntoIterator<Item = Status>) -> Self {
        statuses.into_iter().fold(Self::default(), |mut a, s| {
            match s {
                Status::NeedsInput => a.needs_you += 1,
                Status::Working => a.working += 1,
                _ => {}
            }
            a
        })
    }

    /// The most urgent state, for a dot.
    pub(super) fn status(self) -> Status {
        if self.needs_you > 0 {
            Status::NeedsInput
        } else if self.working > 0 {
            Status::Working
        } else {
            Status::Idle
        }
    }

    /// "Needs you" beats a count of what runs; nothing when all is idle.
    pub(super) fn label(self) -> Option<String> {
        if self.needs_you > 0 {
            Some("Needs you".into())
        } else if self.working > 0 {
            Some(format!("{} running", self.working))
        } else {
            None
        }
    }
}

/// Environments in the other projects that run or wait on the user, those
/// waiting first, then in sidebar order. `statuses[p][e]` is each
/// environment's state; the result is `(project, env, status)`.
pub(super) fn away_envs(statuses: &[Vec<Status>], open: usize) -> Vec<(usize, usize, Status)> {
    let mut away: Vec<_> = statuses
        .iter()
        .enumerate()
        .filter(|(pi, _)| *pi != open)
        .flat_map(|(pi, envs)| {
            envs.iter()
                .enumerate()
                .filter(|(_, s)| matches!(s, Status::NeedsInput | Status::Working))
                .map(move |(ei, s)| (pi, ei, *s))
        })
        .collect();
    // Stable: the sidebar order holds within each group.
    away.sort_by_key(|(_, _, s)| *s != Status::NeedsInput);
    away
}

/// What a worktree's row is called: its thread, the newest when it holds
/// more than one (from before one thread per worktree), with how many more.
/// `None` for the main checkout, a scratch place known by its branch, and for
/// a worktree with no thread yet. `sessions` is `(title, last activity)`;
/// the last value is the index of the one named, which a click opens.
pub(super) fn thread_label(
    is_checkout: bool,
    sessions: &[(&str, u64)],
) -> Option<(String, usize, usize)> {
    if is_checkout {
        return None;
    }
    let (i, (title, _)) = sessions
        .iter()
        .enumerate()
        .filter(|(_, (t, _))| !t.trim().is_empty())
        .max_by_key(|(_, (_, updated))| *updated)?;
    Some((title.to_string(), sessions.len() - 1, i))
}

/// A worktree is often named after its project (`beta-fix-login`); next to
/// the project's name that part only takes room.
fn short_env_name(project: &str, env: &str) -> String {
    env.strip_prefix(project)
        .and_then(|rest| rest.strip_prefix(['-', '_', '/']))
        .filter(|rest| !rest.is_empty())
        .unwrap_or(env)
        .to_string()
}

/// The shape of a status mark. Each state has its own, so it reads without
/// its color (WCAG 1.4.1): someone who can't tell red from green still sees
/// a check or a cross.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Shape {
    /// Nothing going on.
    Dot,
    /// Under way.
    Ring,
    Check,
    Cross,
    /// Waits on you.
    Alert,
}

impl Shape {
    fn of_status(status: Status) -> Self {
        match status {
            Status::Idle => Self::Dot,
            Status::Working => Self::Ring,
            Status::NeedsInput => Self::Alert,
            Status::Error => Self::Cross,
        }
    }

    fn of_ci(state: ci::State) -> Self {
        match state {
            ci::State::Passed => Self::Check,
            ci::State::Failed => Self::Cross,
            ci::State::Running | ci::State::Queued => Self::Ring,
            ci::State::Cancelled | ci::State::Skipped => Self::Dot,
        }
    }
}

/// A status mark in a 10 px box, so marks of every shape line up.
fn mark(shape: Shape, color: Hsla) -> gpui::Div {
    let glyph = |name| ui::icon(name, 10.).text_color(color);
    let inner = match shape {
        Shape::Dot => div()
            .size(px(6.))
            .rounded_full()
            .bg(color)
            .into_any_element(),
        Shape::Ring => div()
            .size(px(7.))
            .rounded_full()
            .border_1()
            .border_color(color)
            .into_any_element(),
        Shape::Check => glyph("mark-check").into_any_element(),
        Shape::Cross => glyph("mark-cross").into_any_element(),
        Shape::Alert => glyph("mark-alert").into_any_element(),
    };
    div()
        .flex()
        .flex_none()
        .items_center()
        .justify_center()
        .size(px(10.))
        .child(inner)
}

pub(super) fn status_dot(status: Status) -> impl IntoElement {
    let color = match status {
        Status::Idle => theme::mix(0.25),
        Status::Working => theme::working(),
        Status::NeedsInput => theme::needs_you(),
        Status::Error => theme::failed().into(),
    };
    mark(Shape::of_status(status), color)
}

/// A run state: a check, a cross, or a ring while it runs.
pub(super) fn ci_mark(state: ci::State) -> gpui::Div {
    let color: Hsla = match state {
        ci::State::Passed => theme::passed().into(),
        ci::State::Failed => theme::failed().into(),
        ci::State::Running | ci::State::Queued => theme::attention().into(),
        _ => theme::mix(0.3),
    };
    mark(Shape::of_ci(state), color)
}

impl Shell {
    /// What GitHub says about an environment's branch; nothing when its
    /// project's runs are not read.
    pub(super) fn env_github(&self, pi: usize, ei: usize, cx: &App) -> GitHubState {
        let Some(p) = self.projects.get(pi) else {
            return GitHubState::default();
        };
        let Some(env) = p.environments.get(ei) else {
            return GitHubState::default();
        };
        let Some(repo) = self.ci.read(cx).repo(&p.path) else {
            return GitHubState::default();
        };
        let ci = repo.branch_state(&env.branch);
        let pr = repo.pr_of(&env.branch);
        GitHubState::of(ci.map(|(s, _)| s), ci.map(|(_, r)| r.id), pr)
    }

    /// An environment's state for its dot, its sessions and GitHub together.
    pub(super) fn env_status(&self, pi: usize, ei: usize, cx: &App) -> Status {
        let Some(env) = self.projects.get(pi).and_then(|p| p.environments.get(ei)) else {
            return Status::Idle;
        };
        env_status(env.aggregate_status(), &self.env_github(pi, ei, cx))
    }

    /// What a project's environments are doing, GitHub included.
    pub(super) fn project_activity(&self, pi: usize, cx: &App) -> Activity {
        let n = self.projects.get(pi).map_or(0, |p| p.environments.len());
        Activity::of((0..n).map(|ei| self.env_status(pi, ei, cx)))
    }

    pub(super) fn render_rail(&self, window: &Window, cx: &mut Context<Self>) -> impl IntoElement {
        let inbox = self.inbox_items();
        let mood = if inbox.iter().any(|i| i.status == Status::NeedsInput) {
            crate::mascot::Mood::Waiting
        } else if inbox.iter().any(|i| i.status == Status::Working) {
            crate::mascot::Mood::Working
        } else {
            crate::mascot::Mood::Idle
        };
        self.rail_mascot.update(cx, |m, _| m.set_mood(mood));
        let header = div()
            .flex()
            .items_center()
            .flex_none()
            .h(px(ui::BAR_H))
            // The raccoon lines up with the list below; the buttons at the end
            // sit as far from the side as from the top.
            .pl(crate::ui::sp(14.).max(crate::ui::traffic_lights_inset(window) - px(gap())))
            .pr(ui::icon_inset())
            .gap(crate::ui::sp(2.))
            .when(
                !self.sidebar.has(&crate::sidebar::Piece::Mascot)
                    && self.rail_shows_raccoon(&inbox, cx),
                |d| d.child(self.rail_mascot.clone()),
            )
            .child(
                div()
                    .flex_1()
                    .h_full()
                    .on_mouse_down(MouseButton::Left, |ev, window, _| {
                        crate::ui::drag_window(ev, window)
                    })
                    .when(cfg!(windows), |d| {
                        d.window_control_area(WindowControlArea::Drag)
                    }),
            )
            .children(
                [
                    (
                        "back",
                        "arrow-left01",
                        self.has_projects() && self.can_go_back(),
                        true,
                    ),
                    (
                        "forward",
                        "arrow-right01",
                        self.has_projects() && self.can_go_forward(),
                        false,
                    ),
                ]
                .map(|(id, name, enabled, go_back)| {
                    if enabled {
                        icon_button(id, name, 14.)
                            .tooltip(ui::tip(if go_back {
                                "Back (Alt+Left)"
                            } else {
                                "Forward (Alt+Right)"
                            }))
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.tab_history(go_back, window, cx)
                            }))
                            .into_any_element()
                    } else {
                        div()
                            .id(id)
                            .flex()
                            .flex_none()
                            .items_center()
                            .justify_center()
                            .size(px(24.))
                            .child(icon(name, 14.).text_color(theme::text_faint()))
                            .into_any_element()
                    }
                }),
            )
            .when(self.has_projects(), |d| {
                d.child(
                    icon_button("rail-hide", "sidebar-left", 14.)
                        .tooltip(ui::tip("Hide sidebar (Ctrl+B)"))
                        .on_click(cx.listener(|this, _, _, cx| this.toggle_rail(cx))),
                )
            })
            .child(
                div()
                    .id("panel-toggle")
                    .flex()
                    .items_center()
                    .justify_center()
                    .size(px(24.))
                    .rounded(crate::ui::rad(8.))
                    .cursor_pointer()
                    .tooltip(ui::tip("Workspace panel"))
                    .hover(|s| s.bg(theme::fill_selected()))
                    .child(
                        icon("layout-align-right", 14.).text_color(if self.panel_shown() {
                            theme::content()
                        } else {
                            theme::mix(0.45)
                        }),
                    )
                    .on_click(cx.listener(|this, _, _, cx| this.toggle_panel(cx))),
            );

        let narrow = self.rail_narrow();
        let card = self.side_card();
        let width = self.rail_width();
        card.id("rail-card")
            .map(ui::stop_window_move)
            .flex()
            .flex_col()
            .flex_none()
            .w(px(width))
            .h_full()
            .when(narrow, |d| {
                d.child(self.render_rail_narrow_header(window, cx))
            })
            .when(!narrow, |d| d.child(header))
            .when(!narrow && self.arranging(), |d| {
                d.child(self.render_arranging_bar(cx))
            })
            .child(
                // Below the header everything scrolls together once the
                // window is too short for the fixed sections and the
                // environments' least height (`RAIL_SCROLL_MIN_H`).
                div()
                    .id("rail-body")
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .when(!narrow, |d| d.children(self.render_switcher_offer(cx)))
                    .children(self.render_pinned_block(cx))
                    .children(self.render_rail_sections(cx)),
            )
            .pb(crate::ui::sp(4.))
    }

    /// Offers the Switcher to someone whose layout predates it, once: "Try it"
    /// swaps it in and asks whether to keep it, "Go back" restores theirs.
    fn render_switcher_offer(&self, cx: &mut Context<Self>) -> Option<gpui::Div> {
        let trying = self.sidebar.trial_of.is_some();
        if !trying && !self.sidebar.offers_switcher() {
            return None;
        }
        let (title, body, yes, no) = if trying {
            (
                "This is the new sidebar",
                "Change it any time in Customize sidebar.",
                "Keep it",
                "Go back",
            )
        } else {
            (
                "Try the new sidebar",
                "One project at a time with its environments, and the views as icons.",
                "Try it",
                "No thanks",
            )
        };
        let yes = ui::button("switcher-yes", yes, ui::ButtonKind::Primary, true).on_click(
            cx.listener(move |this, _, _, cx| {
                if trying {
                    this.change_sidebar(cx, |s| s.trial_of = None);
                } else {
                    this.change_sidebar(cx, |s| *s = s.as_switcher());
                }
            }),
        );
        let no = ui::button("switcher-no", no, ui::ButtonKind::Ghost, true).on_click(cx.listener(
            move |this, _, _, cx| match this.sidebar.trial_of.clone() {
                Some(old) => this.change_sidebar(cx, |s| *s = *old),
                None => this.change_sidebar(cx, |s| s.switcher_offered = true),
            },
        ));
        Some(
            div()
                .flex()
                .flex_col()
                .flex_none()
                .gap(crate::ui::sp(8.))
                .mx(crate::ui::sp(8.))
                .mb(crate::ui::sp(8.))
                .p(crate::ui::sp(12.))
                .rounded(crate::ui::rad(10.))
                .border_1()
                .border_color(theme::stroke())
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .gap(crate::ui::sp(2.))
                        .child(
                            div()
                                .text_size(px(ui::TEXT_MD))
                                .font_weight(FontWeight::SEMIBOLD)
                                .text_color(theme::text())
                                .child(title),
                        )
                        .child(
                            div()
                                .text_size(px(ui::TEXT_SM))
                                .text_color(theme::text_subtle())
                                .child(body),
                        ),
                )
                .child(div().flex().gap(crate::ui::sp(6.)).child(yes).child(no)),
        )
    }

    /// What the rail, the workspace panel and the subagents column sit in,
    /// as the rail's style says: on the window itself (Flush, the default),
    /// in a card, or in a card lifted off the window.
    pub(super) fn side_card(&self) -> gpui::Div {
        match self.sidebar.style.variant {
            crate::sidebar::Variant::Card => ui::card().bg(theme::sidebar()),
            crate::sidebar::Variant::Flush => div().overflow_hidden(),
            crate::sidebar::Variant::Floating => ui::card().bg(theme::sidebar()).shadow_lg(),
        }
    }

    /// The top of the rail narrowed to icons: room for the traffic lights,
    /// then the button that widens it again.
    fn render_rail_narrow_header(
        &self,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let lights = crate::ui::traffic_lights_inset(window) > px(0.);
        div()
            .flex()
            .flex_col()
            .flex_none()
            .items_center()
            .child(
                div()
                    .w_full()
                    .h(px(if lights { 40. } else { 8. }))
                    .on_mouse_down(MouseButton::Left, |ev, window, _| {
                        crate::ui::drag_window(ev, window)
                    }),
            )
            .child(
                icon_button("rail-widen", "sidebar-left", 14.)
                    .tooltip(ui::tip("Show sidebar (Ctrl+B)"))
                    .on_click(cx.listener(|this, _, _, cx| this.toggle_rail(cx))),
            )
    }

    /// Projects, with the environments of the open one.
    /// With `only_open`, the others wait in a dropdown on the open one.
    pub(super) fn render_projects_block(
        &self,
        show_envs: bool,
        show_changes: bool,
        only_open: bool,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let mut projects = div().flex().flex_col().gap(crate::ui::sp(4.));
        for (pi, project) in self.projects.iter().enumerate() {
            let selected = pi == self.active_project;
            if only_open && !selected {
                continue;
            }
            let (added, removed) = project
                .environments
                .iter()
                .flat_map(|e| e.changes.iter())
                .fold((0, 0), |(a, r), c| (a + c.added, r + c.removed));
            let art = ui::project_art(project.art);
            let logo: Hsla = gpui::rgb(project.logo).into();
            projects = projects.child(
                div()
                    .id(("project", pi))
                    .flex()
                    .items_center()
                    .gap(crate::ui::sp(8.))
                    .h(px(32.))
                    .px(crate::ui::sp(8.))
                    .rounded(crate::ui::rad(8.))
                    .cursor_pointer()
                    .text_size(px(ui::TEXT_LG))
                    .font_weight(FontWeight::MEDIUM)
                    .when(selected, |d| {
                        d.bg(theme::selection_strong()).text_color(theme::content())
                    })
                    .when(!selected, |d| {
                        d.text_color(theme::text_muted())
                            .hover(|s| s.bg(theme::hover()).text_color(theme::content()))
                    })
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .justify_center()
                            .size(px(16.))
                            .child(ui::pixel_art(art, 1.5, logo)),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .child(project.name.clone()),
                    )
                    .when(!selected, |d| {
                        let status = self.project_activity(pi, cx).status();
                        d.when(status != Status::Idle, |d| d.child(status_dot(status)))
                    })
                    // The env rows below repeat these totals; keep them here
                    // only while they are hidden (project collapsed).
                    .when(show_changes && added + removed > 0 && !selected, |d| {
                        d.child(
                            div()
                                .flex()
                                .gap(crate::ui::sp(4.))
                                .text_size(px(ui::TEXT_XS))
                                .font_weight(FontWeight::SEMIBOLD)
                                .child(
                                    div()
                                        .text_color(theme::diff_add())
                                        .child(format!("+{added}")),
                                )
                                .child(
                                    div()
                                        .text_color(theme::diff_del())
                                        .child(format!("-{removed}")),
                                ),
                        )
                    })
                    .when(only_open && self.projects.len() > 1, |d| {
                        d.child(icon("unfold-more", 14.).text_color(theme::text_subtle()))
                    })
                    .on_click(cx.listener(move |this, ev: &gpui::ClickEvent, window, cx| {
                        if only_open {
                            this.menu = Some(ContextMenu {
                                target: MenuTarget::ProjectSwitch,
                                at: ev.position() + point(px(8.), px(0.)),
                            });
                            cx.notify();
                        } else {
                            this.select_project(pi, window, cx)
                        }
                    }))
                    .on_mouse_down(
                        MouseButton::Right,
                        cx.listener(move |this, ev: &gpui::MouseDownEvent, _, cx| {
                            cx.stop_propagation();
                            this.menu = Some(ContextMenu {
                                target: MenuTarget::Project(pi),
                                at: ev.position,
                            });
                            cx.notify();
                        }),
                    ),
            );
            if selected && show_envs {
                // The open project alone: its environments get a heading
                // with the + on it. Otherwise they indent under the project.
                let mut envs = div()
                    .flex()
                    .flex_col()
                    .gap(crate::ui::sp(ENV_GAP))
                    .when(!only_open, |d| d.ml(px(10.)));
                if only_open {
                    envs = envs.child(self.render_env_heading(pi, cx));
                }
                let active_above = self.active_list_shown();
                for ei in self.listed_envs(pi) {
                    envs = envs.child(self.render_env_row(
                        pi,
                        ei,
                        &project.environments[ei],
                        show_changes,
                        active_above,
                        cx,
                    ));
                }
                let archived = self.archived_count(pi);
                if archived > 0 {
                    envs = envs.child(
                        super::panel::archived_toggle(
                            ("rail-archived", pi),
                            archived,
                            "",
                            self.show_settled,
                        )
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.show_settled = !this.show_settled;
                            this.drop_pick();
                            cx.notify();
                        })),
                    );
                }
                if !self.pick_bar_under_active() {
                    envs = envs.children(self.render_pick_bar(cx));
                }
                for (i, name) in self
                    .pending_envs
                    .iter()
                    .filter(|p| p.repo == project.path)
                    .map(|p| p.name.clone())
                    .enumerate()
                {
                    envs = envs.child(pending_env_row(pi * 1000 + i, name));
                }
                if !only_open {
                    envs = envs.child(
                        div()
                            .id(("env-add-row", pi))
                            .group("env-add-row")
                            .flex()
                            .items_center()
                            .gap(crate::ui::sp(8.))
                            .h(px(ENV_ROW_H))
                            .pl(crate::ui::sp(8.))
                            .pr(crate::ui::sp(8.))
                            .rounded(crate::ui::rad(8.))
                            .cursor_pointer()
                            .text_size(px(ui::TEXT_MD))
                            .text_color(theme::text_faint())
                            .hover(|s| s.bg(theme::hover()).text_color(theme::text_muted()))
                            .tooltip(ui::tip(NEW_ENV_TIP))
                            .child(hover_icon(
                                "add01",
                                14.,
                                theme::text_faint(),
                                theme::mix(0.7),
                                "env-add-row",
                            ))
                            .child("New session")
                            .on_click(cx.listener(
                                move |this, ev: &gpui::ClickEvent, window, cx| {
                                    this.new_env_click(pi, ev, window, cx)
                                },
                            )),
                    );
                }
                projects = projects.child(envs);
                // The Active list already shows what runs elsewhere.
                if only_open && !active_above {
                    projects = projects.children(self.render_elsewhere(cx));
                }
            }
        }
        if self.projects.is_empty() {
            projects = projects.child(
                div()
                    .id("project-add-empty")
                    .group("project-add-empty")
                    .flex()
                    .items_center()
                    .justify_center()
                    .gap(crate::ui::sp(6.))
                    .h(px(32.))
                    .rounded(crate::ui::rad(8.))
                    .border_1()
                    .border_dashed()
                    .border_color(theme::border_strong())
                    .cursor_pointer()
                    .text_size(px(ui::TEXT_SM))
                    .text_color(theme::text_subtle())
                    .hover(|s| s.bg(theme::fill_hover()).text_color(theme::content()))
                    .child(hover_icon(
                        "add01",
                        14.,
                        theme::text_faint(),
                        theme::content(),
                        "project-add-empty",
                    ))
                    .child("Add project")
                    .on_click(cx.listener(|this, _, window, cx| this.open_picker(window, cx))),
            );
        }

        div().id("rail-projects").child(projects)
    }

    /// Every project, from the open one when the rail shows only that.
    pub(super) fn render_project_switch_menu(&self, cx: &mut Context<Self>) -> AnyElement {
        let mut panel = ui::menu_panel("project-switch-menu", 240.)
            .min_h_0()
            .overflow_y_scroll();
        for (pi, project) in self.projects.iter().enumerate() {
            let selected = pi == self.active_project;
            let (added, removed) = project
                .environments
                .iter()
                .flat_map(|e| e.changes.iter())
                .fold((0, 0), |(a, r), c| (a + c.added, r + c.removed));
            let logo: Hsla = gpui::rgb(project.logo).into();
            let activity = (!selected).then(|| self.project_activity(pi, cx));
            let label = activity.and_then(Activity::label);
            let label_none = label.is_none();
            panel = panel.child(
                div()
                    .id(("project-switch", pi))
                    .flex()
                    .items_center()
                    .gap(crate::ui::sp(8.))
                    .h(px(30.))
                    .px(crate::ui::sp(8.))
                    .rounded(crate::ui::rad(6.))
                    .cursor_pointer()
                    .text_size(px(ui::TEXT_MD))
                    .when(selected, |d| d.bg(theme::selection()))
                    .hover(|s| s.bg(theme::hover()))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .justify_center()
                            .size(px(16.))
                            .child(ui::pixel_art(ui::project_art(project.art), 1.5, logo)),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .child(project.name.clone()),
                    )
                    .when_some(label.zip(activity), |d, (label, a)| {
                        d.child(
                            div()
                                .flex()
                                .items_center()
                                .gap(crate::ui::sp(6.))
                                .text_size(px(ui::TEXT_XS))
                                .text_color(if a.needs_you > 0 {
                                    theme::needs_you()
                                } else {
                                    theme::text_subtle()
                                })
                                .child(status_dot(a.status()))
                                .child(label),
                        )
                    })
                    // What waits or runs matters more than the diff.
                    .when(label_none && added + removed > 0, |d| {
                        d.child(
                            div()
                                .flex()
                                .gap(crate::ui::sp(4.))
                                .text_size(px(ui::TEXT_XS))
                                .font_weight(FontWeight::SEMIBOLD)
                                .child(
                                    div()
                                        .text_color(theme::diff_add())
                                        .child(format!("+{added}")),
                                )
                                .child(
                                    div()
                                        .text_color(theme::diff_del())
                                        .child(format!("-{removed}")),
                                ),
                        )
                    })
                    .when(selected, |d| {
                        d.child(icon("tick02", 14.).text_color(theme::text_subtle()))
                    })
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.menu = None;
                        this.select_project(pi, window, cx)
                    })),
            );
        }
        panel
            .child(ui::menu_divider())
            .child(
                ui::menu_item("project-switch-add", "add01", "Add project…", false, true).on_click(
                    cx.listener(|this, _, window, cx| {
                        this.menu = None;
                        this.open_picker(window, cx)
                    }),
                ),
            )
            .into_any_element()
    }
}

impl Shell {
    /// A click on a + for a new environment: at once, or with Alt the dialog
    /// that asks for a branch and what to start from.
    pub(super) fn new_env_click(
        &mut self,
        pi: usize,
        ev: &gpui::ClickEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if ev.modifiers().alt {
            self.open_create_env(pi, window, cx)
        } else {
            self.draft_new_session(pi, window, cx)
        }
    }

    /// While the rail shows one project: the environments of the others that
    /// wait on the user or run, so work there is not out of sight. Nothing
    /// when all of them are idle.
    fn render_elsewhere(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let statuses: Vec<Vec<Status>> = self
            .projects
            .iter()
            .enumerate()
            .map(|(pi, p)| {
                (0..p.environments.len())
                    .map(|ei| self.env_status(pi, ei, cx))
                    .collect()
            })
            .collect();
        let away = away_envs(&statuses, self.active_project);
        if away.is_empty() {
            return None;
        }
        let mut list = div().flex().flex_col().gap(crate::ui::sp(ENV_GAP)).child(
            div()
                .flex()
                .items_center()
                .h(px(28.))
                .mt(crate::ui::sp(6.))
                .pl(crate::ui::sp(8.))
                .text_size(px(ui::TEXT_SM))
                .text_color(theme::text_subtle())
                .child("Other projects"),
        );
        for (pi, ei, status) in away {
            let project = &self.projects[pi];
            let env = &project.environments[ei];
            let needs_you = status == Status::NeedsInput;
            let logo: Hsla = gpui::rgb(project.logo).into();
            list = list.child(
                div()
                    .id(("env-away", pi * 1000 + ei))
                    .flex()
                    .items_center()
                    .gap(crate::ui::sp(8.))
                    .h(px(ENV_ROW_H))
                    .px(crate::ui::sp(8.))
                    .rounded(crate::ui::rad(8.))
                    .cursor_pointer()
                    .text_size(px(ui::TEXT_MD))
                    .text_color(if needs_you {
                        theme::content()
                    } else {
                        theme::text_muted()
                    })
                    .hover(|s| s.bg(theme::hover()).text_color(theme::content()))
                    .tooltip(ui::tip_text(format!("{}/{}", project.name, env.name)))
                    .child(
                        div()
                            .flex()
                            .flex_none()
                            .items_center()
                            .justify_center()
                            .size(px(14.))
                            .child(ui::pixel_art(ui::project_art(project.art), 1., logo)),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_1()
                            .min_w_0()
                            .gap(crate::ui::sp(4.))
                            .child(
                                div()
                                    .flex_none()
                                    .max_w(px(96.))
                                    .truncate()
                                    .text_color(theme::text_faint())
                                    .child(project.name.clone()),
                            )
                            .child(
                                div()
                                    .min_w_0()
                                    .truncate()
                                    .child(short_env_name(&project.name, &env.name)),
                            ),
                    )
                    .child(
                        div()
                            .flex_none()
                            .flex()
                            .items_center()
                            .when(needs_you, |d| {
                                d.text_size(px(ui::TEXT_XS))
                                    .text_color(theme::needs_you())
                                    .child("Needs you")
                            })
                            .when(!needs_you, |d| d.child(status_dot(status))),
                    )
                    .on_mouse_down(
                        MouseButton::Right,
                        cx.listener(move |this, ev: &gpui::MouseDownEvent, _, cx| {
                            cx.stop_propagation();
                            this.menu = Some(ContextMenu {
                                target: MenuTarget::Env(pi, ei),
                                at: ev.position,
                            });
                            cx.notify();
                        }),
                    )
                    .on_click(
                        cx.listener(move |this, _, window, cx| this.select_env(pi, ei, window, cx)),
                    ),
            );
        }
        Some(list.into_any_element())
    }

    /// For an environment on a server: which machine, dimmed while it cannot
    /// be reached.
    fn render_machine(&self, env: &Environment, id: usize) -> Option<AnyElement> {
        let remote = env.remote.as_ref()?;
        let reach = self.server_reach(&remote.server);
        let note = match &reach {
            Some(Reach::Connected) => None,
            Some(Reach::Away(why) | Reach::Refused(why)) => Some(why.clone()),
            Some(Reach::Connecting) | None => Some("Connecting…".to_string()),
        };
        let machine = crate::redact::text(&remote.machine).into_owned();
        Some(
            div()
                .id(("env-machine", id))
                .flex()
                .flex_none()
                .items_center()
                .gap(crate::ui::sp(4.))
                .max_w(px(96.))
                .text_size(px(ui::TEXT_XS))
                .text_color(theme::text_faint())
                .when(note.is_some(), |d| d.opacity(0.6))
                .tooltip(ui::tip_text(match note {
                    Some(why) => format!("On {machine}: {why}"),
                    None => format!("On {machine}"),
                }))
                .child(icon("computer", 12.).text_color(theme::text_faint()))
                .child(div().min_w_0().truncate().child(machine))
                .into_any_element(),
        )
    }

    /// "Sessions", with the + that makes one in a new worktree, over the open project's list.
    fn render_env_heading(&self, pi: usize, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .flex()
            .flex_col()
            .gap(crate::ui::sp(ENV_GAP))
            .child(
                div()
                    .flex()
                    .items_center()
                    .h(px(28.))
                    .mt(crate::ui::sp(6.))
                    .pl(crate::ui::sp(8.))
                    .text_size(px(ui::TEXT_SM))
                    .text_color(theme::text_subtle())
                    .child(div().flex_1().min_w_0().truncate().child("Sessions")),
            )
            .child(
                div()
                    .id(("env-add", pi))
                    .group("env-add")
                    .flex()
                    .items_center()
                    .gap(crate::ui::sp(8.))
                    .h(px(ENV_ROW_H))
                    .px(crate::ui::sp(8.))
                    .rounded(crate::ui::rad(8.))
                    .cursor_pointer()
                    .border_1()
                    .border_color(theme::border())
                    .text_size(px(ui::TEXT_MD))
                    .text_color(theme::text_muted())
                    .hover(|s| s.bg(theme::hover()).text_color(theme::content()))
                    .tooltip(ui::tip(NEW_SESSION_TIP))
                    .child(hover_icon(
                        "add01",
                        14.,
                        theme::text_muted(),
                        theme::content(),
                        "env-add",
                    ))
                    .child(div().flex_1().min_w_0().truncate().child("New session"))
                    .child(
                        div()
                            .flex_none()
                            .text_size(px(ui::TEXT_XS))
                            .text_color(theme::text_faint())
                            .child(crate::platform::shortcut_label("Ctrl+T")),
                    )
                    .on_click(cx.listener(move |this, ev: &gpui::ClickEvent, window, cx| {
                        if ev.modifiers().alt {
                            this.open_create_env(pi, window, cx)
                        } else {
                            this.new_session(window, cx)
                        }
                    })),
            )
    }

    /// One environment: its state as a dot (a folder for the checkout while
    /// nothing runs there), its name, and one signal at the end.
    fn render_env_row(
        &self,
        pi: usize,
        ei: usize,
        env: &Environment,
        show_changes: bool,
        active_above: bool,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let active = self.projects.get(pi).is_some_and(|p| p.active_env == ei);
        let picked = self.is_picked(&env.path);
        let sessions = env.aggregate_status();
        let github = self.env_github(pi, ei, cx);
        let status = env_status(sessions, &github);
        // The diff of the open row; the others show theirs on hover.
        let changes = show_changes.then(|| env.diff_totals());
        let signal = env_signal(sessions, &github, changes);
        // The sessions in the Active list are named there; this row names the rest.
        let named: Vec<&model::Session> = env
            .sessions
            .iter()
            .filter(|s| !active_above || !super::active::is_active(s.shown_status()))
            .collect();
        let titles: Vec<(&str, u64)> = named
            .iter()
            .map(|s| (s.title.as_ref(), s.updated))
            .collect();
        let label = thread_label(env.is_checkout, &titles);
        // A click opens the session the row is named after: going to the
        // worktree would show the tab it showed last, maybe another session's.
        let opens = label.as_ref().map(|(_, _, i)| named[*i].id.clone());
        let lead: AnyElement = if picked {
            icon("tick02", 14.)
                .text_color(theme::content())
                .into_any_element()
        } else if env.archived() && status == Status::Idle {
            icon("archive02", 14.)
                .text_color(theme::text_faint())
                .into_any_element()
        } else if env.is_checkout && status == Status::Idle {
            icon("folder01", 14.)
                .text_color(if active {
                    theme::text_muted()
                } else {
                    theme::text_faint()
                })
                .into_any_element()
        } else {
            div()
                .flex()
                .items_center()
                .justify_center()
                .size(px(14.))
                .child(status_dot(status))
                .into_any_element()
        };
        let end: Option<AnyElement> = match signal {
            EnvSignal::NeedsYou => Some(
                div()
                    .text_size(px(ui::TEXT_XS))
                    .text_color(theme::needs_you())
                    .child("Needs you")
                    .into_any_element(),
            ),
            EnvSignal::CiFailed => github.failed_run.map(|id| {
                div()
                    .id(("env-ci", pi * 1000 + ei))
                    .px(crate::ui::sp(4.))
                    .rounded(crate::ui::rad(4.))
                    .text_size(px(ui::TEXT_XS))
                    .text_color(theme::error())
                    .hover(|s| s.bg(theme::fill_selected()))
                    .tooltip(ui::tip("Open the failed run"))
                    .child("CI failed")
                    .on_click(cx.listener(move |this, _, window, cx| {
                        cx.stop_propagation();
                        this.select_env(pi, ei, window, cx);
                        this.open_runs_tab(Some(id), window, cx);
                    }))
                    .into_any_element()
            }),
            EnvSignal::Review(what) => github.review.clone().map(|(_, url)| {
                div()
                    .id(("env-review", pi * 1000 + ei))
                    .px(crate::ui::sp(4.))
                    .rounded(crate::ui::rad(4.))
                    .text_size(px(ui::TEXT_XS))
                    .text_color(theme::needs_you())
                    .hover(|s| s.bg(theme::fill_selected()))
                    .tooltip(ui::tip("Open the pull request"))
                    .child(what)
                    .on_click(cx.listener(move |this, _, window, cx| {
                        cx.stop_propagation();
                        this.select_env(pi, ei, window, cx);
                        this.open_url(url.clone(), window, cx);
                    }))
                    .into_any_element()
            }),
            EnvSignal::Changes(a, r) => Some(
                diff_label(a, r)
                    .when(!active, |d| {
                        d.invisible().group_hover("env-row", |s| s.visible())
                    })
                    .into_any_element(),
            ),
            EnvSignal::Nothing => None,
        };
        // The branch's pull request by number, so a row says which one it is.
        let pr = github.pr.clone().map(|pr| {
            let url = pr.url.clone();
            div()
                .id(("env-pr", pi * 1000 + ei))
                .flex_none()
                .px(crate::ui::sp(4.))
                .rounded(crate::ui::rad(4.))
                .text_size(px(ui::TEXT_XS))
                .text_color(theme::text_faint())
                .hover(|s| s.bg(theme::fill_selected()).text_color(theme::content()))
                .tooltip(ui::tip_text(pr.tip()))
                .child(format!("#{}", pr.number))
                .on_click(cx.listener(move |this, _, window, cx| {
                    cx.stop_propagation();
                    this.select_env(pi, ei, window, cx);
                    this.open_url(url.clone(), window, cx);
                }))
        });
        div()
            .id(("env", pi * 1000 + ei))
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(move |this, ev: &gpui::MouseDownEvent, _, cx| {
                    cx.stop_propagation();
                    this.menu = Some(ContextMenu {
                        target: MenuTarget::Env(pi, ei),
                        at: ev.position,
                    });
                    cx.notify();
                }),
            )
            .group("env-row")
            .flex()
            .items_center()
            .gap(crate::ui::sp(8.))
            .h(px(ENV_ROW_H))
            .px(crate::ui::sp(8.))
            .rounded(crate::ui::rad(8.))
            .cursor_pointer()
            .text_size(px(ui::TEXT_MD))
            .text_color(if active || status == Status::NeedsInput {
                theme::content()
            } else {
                theme::text_muted()
            })
            // Picked rows are set apart from the open one, which may not be picked.
            .when(active && !picked, |d| d.bg(theme::selection()))
            .when(picked, |d| {
                d.bg(theme::selection_strong()).text_color(theme::content())
            })
            .when(!active && !picked, |d| {
                d.hover(|s| s.bg(theme::hover()).text_color(theme::content()))
            })
            .child(lead)
            .when_some(label.clone(), |d, (title, more, _)| {
                let tip = match env.branch.as_ref() {
                    "" => title.to_string(),
                    branch => format!("{title} · {branch}"),
                };
                d.tooltip(ui::tip_text(tip))
                    .child(div().flex_1().min_w_0().truncate().child(title))
                    .when(more > 0, |d| {
                        d.child(
                            div()
                                .flex_none()
                                .text_size(px(ui::TEXT_XS))
                                .text_color(theme::text_faint())
                                .child(format!("+{more}")),
                        )
                    })
            })
            .when(label.is_none(), |d| {
                // A worktree made for a new session, still on its random branch:
                // the first prompt names both.
                let fresh = env.placeholder.is_some() && env.sessions.is_empty();
                d.when(fresh, |d| {
                    d.tooltip(ui::tip_text(env.branch.clone()))
                        .child(div().flex_1().min_w_0().truncate().child("New session"))
                })
                .when(!fresh, |d| {
                    d.child(div().flex_1().min_w_0().truncate().child(env.name.clone()))
                })
            })
            .children(pr)
            .children(self.render_machine(env, pi * 1000 + ei))
            .children(end.map(|e| div().flex_none().child(e)))
            .on_click(cx.listener(move |this, ev: &gpui::ClickEvent, window, cx| {
                this.click_env(pi, ei, opens.clone(), ev.modifiers(), window, cx)
            }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn other_projects_list_what_waits_first() {
        use Status::*;
        let statuses = vec![
            vec![Working, NeedsInput],
            vec![Idle, Working, Error],
            vec![Working, NeedsInput, Idle],
        ];
        // The open project (0) is left out, and so are idle and failed rows.
        assert_eq!(
            away_envs(&statuses, 0),
            vec![(2, 1, NeedsInput), (1, 1, Working), (2, 0, Working)]
        );
        assert!(away_envs(&[vec![Working]], 0).is_empty());
    }

    #[test]
    fn a_worktree_is_called_by_its_thread() {
        assert_eq!(
            thread_label(false, &[("Fix login", 5)]),
            Some(("Fix login".into(), 0, 0))
        );
        // From before one thread per worktree: the newest, and how many more.
        assert_eq!(
            thread_label(false, &[("Old", 1), ("New", 9), ("Mid", 4)]),
            Some(("New".into(), 2, 1))
        );
        assert_eq!(
            thread_label(false, &[("", 9), ("Named", 2)]),
            Some(("Named".into(), 1, 1))
        );
        // No thread yet, or the main checkout: known by the branch.
        assert_eq!(thread_label(false, &[]), None);
        assert_eq!(thread_label(true, &[("Fix login", 5)]), None);
    }

    #[test]
    fn the_project_name_is_not_said_twice() {
        assert_eq!(short_env_name("beta", "beta-fix-login"), "fix-login");
        assert_eq!(short_env_name("beta", "beta_new"), "new");
        assert_eq!(
            short_env_name("beta", "env-sleek-plover"),
            "env-sleek-plover"
        );
        assert_eq!(short_env_name("beta", "betamax"), "betamax");
        assert_eq!(short_env_name("beta", "beta-"), "beta-");
    }

    #[test]
    fn a_project_says_needs_you_before_what_runs() {
        use Status::*;
        let a = Activity::of([Working, NeedsInput, Working, Idle]);
        assert_eq!(a.status(), NeedsInput);
        assert_eq!(a.label().as_deref(), Some("Needs you"));
        let a = Activity::of([Working, Working, Idle]);
        assert_eq!(a.label().as_deref(), Some("2 running"));
        assert_eq!(Activity::of([Idle, Error]).label(), None);
    }

    fn failing() -> GitHubState {
        GitHubState {
            failed_run: Some(7),
            ..Default::default()
        }
    }

    fn wants(what: &'static str) -> GitHubState {
        GitHubState {
            review: Some((what, "https://github.com/o/r/pull/1".into())),
            ..Default::default()
        }
    }

    #[test]
    fn a_row_says_the_most_urgent_thing() {
        let diff = Some((12, 3));
        let quiet = GitHubState::default();
        assert_eq!(
            env_signal(Status::NeedsInput, &failing(), diff),
            EnvSignal::NeedsYou
        );
        assert_eq!(
            env_signal(Status::Working, &failing(), diff),
            EnvSignal::CiFailed
        );
        assert_eq!(
            env_signal(Status::Working, &wants("Changes requested"), diff),
            EnvSignal::Review("Changes requested")
        );
        assert_eq!(
            env_signal(Status::Working, &quiet, diff),
            EnvSignal::Changes(12, 3)
        );
        assert_eq!(
            env_signal(Status::Idle, &quiet, Some((0, 0))),
            EnvSignal::Nothing
        );
        // Changes turned off in the builder.
        assert_eq!(env_signal(Status::Idle, &quiet, None), EnvSignal::Nothing);
    }

    #[test]
    fn github_can_need_you_or_keep_a_row_running() {
        let running = GitHubState {
            ci_running: true,
            ..Default::default()
        };
        assert_eq!(env_status(Status::Idle, &running), Status::Working);
        assert_eq!(env_status(Status::Idle, &failing()), Status::NeedsInput);
        assert_eq!(
            env_status(Status::Working, &wants("Conflicts")),
            Status::NeedsInput
        );
        assert_eq!(
            env_status(Status::Idle, &GitHubState::default()),
            Status::Idle
        );
        assert_eq!(
            env_status(Status::Error, &GitHubState::default()),
            Status::Error
        );
    }

    #[test]
    fn a_pull_request_says_what_it_wants() {
        let pr = |decision: &str, mergeable: &str| crate::ci::Pr {
            number: 1,
            title: "t".into(),
            head_ref_name: "b".into(),
            url: "u".into(),
            is_draft: false,
            review_decision: Some(decision.into()),
            mergeable: Some(mergeable.into()),
        };
        let of = |p| GitHubState::of(None, None, Some(&p)).review.map(|(w, _)| w);
        assert_eq!(
            of(pr("CHANGES_REQUESTED", "MERGEABLE")),
            Some("Changes requested")
        );
        assert_eq!(of(pr("APPROVED", "CONFLICTING")), Some("Conflicts"));
        assert_eq!(of(pr("APPROVED", "MERGEABLE")), None);
        // A failed run counts only while the branch's state is failed.
        let passed = GitHubState::of(Some(ci::State::Passed), Some(3), None);
        assert_eq!(passed.failed_run, None);
        assert!(!passed.ci_running);
        assert!(GitHubState::of(Some(ci::State::Running), Some(3), None).ci_running);
    }

    #[test]
    fn a_row_knows_its_pull_request() {
        let pr: crate::ci::Pr = serde_json::from_value(serde_json::json!({
            "number": 513,
            "title": "Reach buttons with Tab",
            "headRefName": "pr/focus",
            "url": "https://github.com/o/r/pull/513",
            "isDraft": true,
        }))
        .unwrap();
        let link = GitHubState::of(None, None, Some(&pr)).pr.unwrap();
        assert_eq!(link.number, 513);
        assert_eq!(link.url, "https://github.com/o/r/pull/513");
        assert_eq!(
            link.tip(),
            "Draft pull request #513: Reach buttons with Tab"
        );
        // A pull request alone needs nothing from the user.
        assert!(!GitHubState::of(None, None, Some(&pr)).needs_you());
        assert_eq!(GitHubState::of(None, None, None).pr, None);
    }

    #[test]
    fn every_state_has_its_own_shape() {
        // Color alone must not tell these apart.
        let session = [
            Status::Idle,
            Status::Working,
            Status::NeedsInput,
            Status::Error,
        ]
        .map(Shape::of_status);
        for (i, a) in session.iter().enumerate() {
            assert!(!session[i + 1..].contains(a), "{a:?} is used twice");
        }
        let (passed, failed, running) = (
            Shape::of_ci(ci::State::Passed),
            Shape::of_ci(ci::State::Failed),
            Shape::of_ci(ci::State::Running),
        );
        assert!(passed != failed && failed != running && passed != running);
    }
}
