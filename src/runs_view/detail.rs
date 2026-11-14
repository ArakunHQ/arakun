//! A run's page: what happened on the left, its steps, logs, annotations and
//! artifacts on the right.

use std::sync::Arc;

use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, App, Bounds, Context, FontWeight, Hsla, InteractiveElement as _, IntoElement,
    ParentElement as _, PathBuilder, Pixels, SharedString, StatefulInteractiveElement as _,
    Styled as _, Window, canvas, div, point, px, uniform_list,
};

use super::{
    AMBER, DetailState, GREEN, Page, RED, RunsEvent, RunsView, empty, header_button, hex,
    mono_chip, soft, state_color, state_icon,
};
use crate::ci::{self, Annotation, Detail, Job, Level, LineKind, LogLine, Run, State};
use crate::model::{self, Status};
use crate::runs_view::LinkedSession;
use crate::theme;
use crate::ui::{self, icon};

const LEFT_W: f32 = 300.;

impl RunsView {
    pub(super) fn render_detail(
        &mut self,
        run: &Run,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let detail = self.detail_load(run.id, cx);
        let ready = match &detail {
            Some(DetailState::Ready(d)) => Some(d.clone()),
            _ => None,
        };
        div()
            .flex()
            .flex_col()
            .size_full()
            .child(self.render_bar(run, cx))
            .child(
                div()
                    .flex()
                    .flex_1()
                    .min_h_0()
                    .child(self.render_side(run, ready.as_deref(), cx))
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .flex_1()
                            .min_w_0()
                            .child(self.render_pages(run, ready.as_deref(), cx))
                            .child(match detail {
                                None | Some(DetailState::Loading) => div()
                                    .flex()
                                    .flex_1()
                                    .items_center()
                                    .justify_center()
                                    .child(ui::spinner("run-loading", 18., theme::mix(0.4)))
                                    .into_any_element(),
                                Some(DetailState::Failed(e)) => {
                                    empty("alert-circle", "Couldn't read this run", e.as_ref())
                                }
                                Some(DetailState::Ready(d)) => match self.page {
                                    Page::Steps => self.render_steps(run, &d, cx),
                                    Page::Logs => self.render_logs(&d, window, cx),
                                    Page::Annotations => self.render_annotations(run, &d, cx),
                                    Page::Artifacts => self.render_artifacts(run, &d, cx),
                                },
                            }),
                    ),
            )
            .into_any_element()
    }

    fn render_bar(&self, run: &Run, cx: &mut Context<Self>) -> impl IntoElement {
        let state = run.state();
        let root = self.root.clone();
        let acting = root
            .as_ref()
            .and_then(|r| self.ci.read(cx).repo(r))
            .is_some_and(|r| r.acting.contains(&run.id));
        let url = run.url.clone();
        let fix = self.sessions.get(&run.id).and_then(|s| s.first()).cloned();
        let earlier = fix
            .is_none()
            .then(|| self.branch_fix_session(run, cx))
            .flatten();
        let run_id = run.id;
        div()
            .flex()
            .flex_none()
            .items_center()
            .gap(px(8.))
            .h(px(44.))
            .px(px(16.))
            .border_b_1()
            .border_color(theme::stroke())
            .child(
                div()
                    .id("run-crumb")
                    .px(px(4.))
                    .rounded(px(4.))
                    .text_color(theme::text_subtle())
                    .cursor_pointer()
                    .hover(|s| s.text_color(theme::content()))
                    .child("Runs")
                    .on_click(cx.listener(|this, _, _, cx| this.select(None, cx))),
            )
            .child(icon("arrow-right01", 12.).text_color(theme::text_faint()))
            .child(
                div()
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(format!("#{}", run.number)),
            )
            .child(div().flex_1())
            .child(
                header_button("run-github", "link-square02", "GitHub", None)
                    .on_click(move |_, _, cx| cx.open_url(&url)),
            )
            .child(if acting {
                div()
                    .flex()
                    .items_center()
                    .gap(px(6.))
                    .h(px(ui::BUTTON_H))
                    .px(px(10.))
                    .text_size(px(ui::TEXT_SM))
                    .text_color(theme::text_subtle())
                    .child(ui::spinner("run-acting", 13., theme::mix(0.5)))
                    .child(if state.done() {
                        "Starting…"
                    } else {
                        "Cancelling…"
                    })
                    .into_any_element()
            } else if !state.done() {
                header_button("run-cancel", "cancel-circle", "Cancel", None)
                    .on_click(cx.listener(|this, _, _, cx| this.act(true, cx)))
                    .into_any_element()
            } else {
                header_button(
                    "run-rerun",
                    "refresh01",
                    if state == State::Failed {
                        "Re-run failed"
                    } else {
                        "Re-run"
                    },
                    Some("R"),
                )
                .on_click(cx.listener(|this, _, _, cx| this.act(false, cx)))
                .into_any_element()
            })
            .when_some(earlier.filter(|_| state == State::Failed), |d, s| {
                d.child(
                    header_button(
                        "run-send-fix",
                        "message01",
                        format!("Send to “{}”", s.title),
                        None,
                    )
                    .on_click(cx.listener(move |_, _, _, cx| {
                        cx.emit(RunsEvent::SendFailure {
                            project: s.project,
                            env: s.env,
                            id: s.id.clone(),
                            run: run_id,
                        })
                    })),
                )
            })
            .when(state == State::Failed, |d| {
                d.child(match fix {
                    Some(s) => header_button("run-open-fix", "sparkles", "Open fix session", None)
                        .on_click(cx.listener(move |_, _, _, cx| {
                            cx.emit(RunsEvent::OpenSession {
                                project: s.project,
                                env: s.env,
                                id: s.id.clone(),
                            })
                        }))
                        .into_any_element(),
                    None => ui::button_with(
                        "run-fix",
                        Some("sparkles"),
                        "Fix with Droid",
                        ui::ButtonKind::Primary,
                        true,
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .justify_center()
                            .min_w(px(16.))
                            .h(px(16.))
                            .rounded(px(ui::RADIUS_SM))
                            .bg(gpui::hsla(0., 0., 0., 0.12))
                            .text_size(px(ui::TEXT_XS))
                            .child("F"),
                    )
                    .on_click(cx.listener(|this, _, _, cx| this.fix(cx)))
                    .into_any_element(),
                })
            })
    }

    /// The summary, its facts, and the other runs of the branch.
    fn render_side(
        &self,
        run: &Run,
        detail: Option<&Detail>,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let state = run.state();
        let repo = self.root.as_ref().and_then(|r| self.ci.read(cx).repo(r));
        let prev = repo.and_then(|r| r.previous(run)).cloned();
        let failed_job = detail.and_then(Detail::failed_job);
        let failed_step = failed_job.and_then(Job::failed_step);
        let header_text = match failed_job {
            Some(j) => format!("#{} · {} · {}", run.number, run.workflow_name, j.name),
            None => format!("#{} · {}", run.number, run.workflow_name),
        };

        let mut summary = div()
            .flex()
            .flex_wrap()
            .items_center()
            .gap_y(px(4.))
            .text_size(px(ui::TEXT_SM))
            .line_height(px(20.))
            .text_color(theme::text_subtle());
        let word = |t: String| div().child(t);
        match (state, failed_step, failed_job) {
            (State::Failed, Some(step), Some(job)) => {
                summary = summary
                    .child(mono_chip(step.title().to_string()))
                    .child(word(" failed in the ".into()))
                    .child(
                        div()
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(theme::text_muted())
                            .child(job.name.clone()),
                    )
                    .child(word(" job.".into()));
                let code: Vec<&Annotation> = detail
                    .map(|d| {
                        d.annotations
                            .iter()
                            .filter(|a| a.in_code() && a.level == Level::Failure)
                            .collect()
                    })
                    .unwrap_or_default();
                if let Some(first) = code.first() {
                    let n = code.len();
                    summary = summary
                        .child(word(format!(
                            " {n} error{} in ",
                            if n == 1 { "" } else { "s" }
                        )))
                        .child(mono_chip(first.path.clone()))
                        .when(code.iter().any(|a| a.path != first.path), |d| {
                            d.child(word(" and more".into()))
                        });
                }
            }
            (State::Failed, ..) => summary = summary.child(word("This run failed.".into())),
            (State::Passed, ..) => summary = summary.child(word("Every job passed.".into())),
            (State::Running, ..) => summary = summary.child(word("Running now.".into())),
            (State::Queued, ..) => summary = summary.child(word("Waiting for a runner.".into())),
            (State::Cancelled, ..) => {
                summary = summary.child(word("This run was cancelled.".into()))
            }
            (State::Skipped, ..) => summary = summary.child(word("Nothing ran.".into())),
        }
        if let Some(p) = &prev {
            let text = match (state, p.state()) {
                (State::Failed, State::Passed) => {
                    format!(" The previous run on {} passed.", run.head_branch)
                }
                (State::Failed, State::Failed) => " The previous run failed too.".to_string(),
                (State::Passed, State::Failed) => format!(" Fixed since #{}.", p.number),
                _ => String::new(),
            };
            if !text.is_empty() {
                summary = summary.child(word(text));
            }
        }

        let regression = match (state, &prev) {
            (State::Failed, Some(p)) if p.state() == State::Passed => {
                Some((true, format!("Yes · after #{}", p.number)))
            }
            (State::Failed, Some(_)) => {
                let since = repo.map_or(run.number, |r| {
                    let mut first = run.number;
                    let mut cur = run.clone();
                    while let Some(p) = r.previous(&cur) {
                        if p.state() != State::Failed {
                            break;
                        }
                        first = p.number;
                        cur = p.clone();
                    }
                    first
                });
                Some((false, format!("No · failing since #{since}")))
            }
            _ => None,
        };
        let runner = detail
            .and_then(|d| d.jobs.iter().find(|j| !j.labels.is_empty()))
            .map(|j| j.labels.join(", "));
        let commit = detail.and_then(|d| d.commit.clone());

        let kv = |label: &str, value: AnyElement| {
            div()
                .flex()
                .items_center()
                .min_h(px(26.))
                .child(
                    div()
                        .w(px(84.))
                        .flex_none()
                        .text_color(theme::text_subtle())
                        .child(label.to_string()),
                )
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(6.))
                        .flex_1()
                        .min_w_0()
                        .child(value),
                )
        };
        let with_icon = |name: &str, color: Hsla, text: String| {
            div()
                .flex()
                .items_center()
                .gap(px(6.))
                .min_w_0()
                .child(icon(name, 13.).text_color(color))
                .child(div().truncate().child(text))
                .into_any_element()
        };
        let facts = div()
            .flex()
            .flex_col()
            .px(px(20.))
            .py(px(12.))
            .text_size(px(ui::TEXT_SM))
            .border_b_1()
            .border_color(theme::stroke())
            .child(kv(
                "Status",
                div()
                    .flex()
                    .items_center()
                    .gap(px(6.))
                    .child(div().size(px(8.)).rounded_full().bg(state_color(state)))
                    .child(state.label())
                    .when(run.attempt > 1, |d| {
                        d.child(
                            div()
                                .text_color(theme::text_subtle())
                                .child(format!("· attempt {}", run.attempt)),
                        )
                    })
                    .when(run.passed_on_retry(), |d| {
                        d.child(div().text_color(hex(AMBER)).child("· flaky"))
                    })
                    .into_any_element(),
            ))
            .child(kv(
                if run.is_deploy() { "Tag" } else { "Branch" },
                with_icon(
                    if run.is_deploy() {
                        "tag01"
                    } else {
                        "git-branch"
                    },
                    theme::mix(0.5),
                    run.head_branch.clone(),
                ),
            ))
            .child(kv(
                "Commit",
                div()
                    .flex()
                    .items_center()
                    .gap(px(6.))
                    .min_w_0()
                    .child(icon("git-commit", 13.).text_color(theme::text_subtle()))
                    .child(
                        div()
                            .flex_none()
                            .font_family(theme::FONT_MONO)
                            .text_size(px(ui::TEXT_XS))
                            .child(run.short_sha().to_string()),
                    )
                    .when_some(
                        commit
                            .as_ref()
                            .map(|c| c.1.clone())
                            .filter(|a| !a.is_empty()),
                        |d, a| {
                            d.child(
                                div()
                                    .truncate()
                                    .text_color(theme::text_subtle())
                                    .child(format!("· {a}")),
                            )
                        },
                    )
                    .into_any_element(),
            ))
            .child(kv(
                "Trigger",
                with_icon(
                    "flash",
                    hex(AMBER),
                    match detail.and_then(|d| d.actor.clone()) {
                        Some(by) => format!("{} · {by}", run.trigger()),
                        None => run.trigger().to_string(),
                    },
                ),
            ))
            .when_some(run.duration(), |d, s| {
                d.child(kv(
                    "Duration",
                    with_icon("clock01", theme::mix(0.5), ci::duration_label(s)),
                ))
            })
            .when_some(runner, |d, r| {
                d.child(kv("Runner", with_icon("computer", theme::mix(0.5), r)))
            })
            .when_some(regression, |d, (yes, text)| {
                d.child(kv(
                    "Regression",
                    with_icon(
                        "rotate-ccw",
                        if yes { hex(RED) } else { theme::mix(0.5) },
                        text,
                    ),
                ))
            });

        let since = detail.and_then(|d| d.since_green.clone()).map(|g| {
            let gid = g.run;
            let heading = div()
                .id("since-green")
                .flex()
                .items_center()
                .gap(px(6.))
                .px(px(10.))
                .pb(px(6.))
                .text_size(px(ui::TEXT_XS))
                .text_color(theme::text_subtle())
                .cursor_pointer()
                .hover(|s| s.text_color(theme::content()))
                .child(div().size(px(6.)).rounded_full().bg(hex(GREEN)))
                .child(format!("Since #{} passed", g.number))
                .on_click(cx.listener(move |this, _, _, cx| this.select(Some(gid), cx)));
            let body: AnyElement = if g.commits.is_empty() && !g.partial {
                div()
                    .px(px(10.))
                    .text_size(px(ui::TEXT_SM))
                    .line_height(px(19.))
                    .text_color(theme::text_muted())
                    .child(format!(
                        "The same commit passed in #{}. Nothing changed, so this failure is likely flaky; re-run it.",
                        g.number
                    ))
                    .into_any_element()
            } else {
                let n = g.commits.len();
                div()
                    .flex()
                    .flex_col()
                    .children(g.commits.iter().take(8).map(|(sha, title)| {
                        div()
                            .flex()
                            .items_center()
                            .gap(px(8.))
                            .h(px(24.))
                            .px(px(10.))
                            .text_size(px(ui::TEXT_SM))
                            .child(
                                div()
                                    .flex_none()
                                    .font_family(theme::FONT_MONO)
                                    .text_size(px(ui::TEXT_XS))
                                    .text_color(theme::text_subtle())
                                    .child(sha.get(..7).unwrap_or(sha).to_string()),
                            )
                            .child(div().flex_1().min_w_0().truncate().text_color(theme::text_muted()).child(title.clone()))
                    }))
                    .when(n > 8 || g.partial, |d| {
                        d.child(
                            div()
                                .px(px(10.))
                                .pt(px(2.))
                                .text_size(px(ui::TEXT_XS))
                                .text_color(theme::text_faint())
                                .child(if g.partial && n == 0 {
                                    "Those commits aren't in the local clone; fetch to see them.".to_string()
                                } else {
                                    format!("{}{} more", n.saturating_sub(8), if g.partial { "+" } else { "" })
                                }),
                        )
                    })
                    .into_any_element()
            };
            div()
                .flex()
                .flex_col()
                .px(px(10.))
                .py(px(10.))
                .border_b_1()
                .border_color(theme::stroke())
                .child(heading)
                .child(body)
        });

        let durations = repo
            .map(|r| r.durations(&run.workflow_name))
            .unwrap_or_default();
        let trend = (durations.len() >= 3).then(|| {
            let avg = durations.iter().map(|(_, s, _)| *s).sum::<u64>() / durations.len() as u64;
            let here = durations.iter().position(|(id, ..)| *id == run.id);
            let label = match (run.duration(), here) {
                (Some(d), Some(_)) if avg > 0 && d > avg * 13 / 10 => (
                    format!("{}% slower than usual", (d * 100 / avg).saturating_sub(100)),
                    hex(AMBER),
                ),
                (Some(d), Some(_)) if avg > 0 && d * 10 < avg * 8 => (
                    format!("{}% faster than usual", 100 - d * 100 / avg),
                    hex(GREEN),
                ),
                _ => (
                    format!("usually {}", ci::duration_label(avg)),
                    theme::mix(0.45),
                ),
            };
            div()
                .flex()
                .flex_col()
                .gap(px(8.))
                .px(px(20.))
                .py(px(12.))
                .border_b_1()
                .border_color(theme::stroke())
                .child(
                    div()
                        .flex()
                        .items_center()
                        .text_size(px(ui::TEXT_XS))
                        .child(
                            div()
                                .flex_1()
                                .text_color(theme::text_subtle())
                                .child(format!("Duration · last {} runs", durations.len())),
                        )
                        .child(div().text_color(label.1).child(label.0)),
                )
                .child(sparkline(durations, run.id))
        });

        let history: Vec<Run> = repo
            .map(|r| {
                r.runs
                    .iter()
                    .filter(|x| {
                        x.head_branch == run.head_branch && x.is_deploy() == run.is_deploy()
                    })
                    .take(8)
                    .cloned()
                    .collect()
            })
            .unwrap_or_default();
        let current = run.id;
        let history_list = div()
            .flex()
            .flex_col()
            .px(px(10.))
            .py(px(10.))
            .child(
                div()
                    .px(px(10.))
                    .pb(px(6.))
                    .text_size(px(ui::TEXT_XS))
                    .text_color(theme::text_subtle())
                    .child(if run.is_deploy() {
                        "Deploys"
                    } else {
                        "This branch"
                    }),
            )
            .children(history.into_iter().enumerate().map(|(i, r)| {
                let on = r.id == current;
                let id = r.id;
                div()
                    .id(("run-history", i))
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .h(px(28.))
                    .px(px(10.))
                    .rounded(px(8.))
                    .text_size(px(ui::TEXT_SM))
                    .cursor_pointer()
                    .when(on, |d| d.bg(theme::selection()))
                    .when(!on, |d| d.hover(|s| s.bg(theme::hover())))
                    .child(
                        div()
                            .size(px(6.))
                            .rounded_full()
                            .flex_none()
                            .bg(state_color(r.state())),
                    )
                    .child(
                        div()
                            .flex_none()
                            .text_color(theme::text_faint())
                            .child(format!("#{}", r.number)),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_color(if on {
                                theme::content()
                            } else {
                                theme::mix(0.7)
                            })
                            .child(r.display_title.clone()),
                    )
                    .child(
                        div()
                            .flex_none()
                            .text_size(px(ui::TEXT_XS))
                            .text_color(theme::text_faint())
                            .child(model::age(r.created())),
                    )
                    .on_click(cx.listener(move |this, _, _, cx| this.select(Some(id), cx)))
            }));

        div()
            .id("run-side")
            .flex()
            .flex_col()
            .w(px(LEFT_W))
            .flex_none()
            .h_full()
            .overflow_y_scroll()
            .border_r_1()
            .border_color(theme::stroke())
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(8.))
                    .px(px(20.))
                    .pt(px(20.))
                    .pb(px(16.))
                    .border_b_1()
                    .border_color(theme::stroke())
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .justify_center()
                            .size(px(32.))
                            .rounded(px(12.))
                            .bg(soft_state(state, 0.14))
                            .border_1()
                            .border_color(soft_state(state, 0.3))
                            .child(state_icon("run-side-state", state, 16.)),
                    )
                    .child(
                        div()
                            .mt(px(4.))
                            .font_family(theme::FONT_MONO)
                            .text_size(px(ui::TEXT_XS))
                            .text_color(theme::text_subtle())
                            .child(header_text),
                    )
                    .child(
                        div()
                            .text_size(px(ui::TEXT_TITLE))
                            .line_height(px(26.))
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(run.display_title.clone()),
                    )
                    .child(summary),
            )
            .child(facts)
            .children(since)
            .children(trend)
            .child(history_list)
    }

    fn render_pages(
        &self,
        run: &Run,
        detail: Option<&Detail>,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let annotations = detail.map_or(0, |d| d.annotations.len());
        let artifacts = detail.map_or(0, |d| d.artifacts.len());
        let (done, total) = detail.map_or((0, 0), |d| {
            let steps = d.jobs.iter().flat_map(|j| &j.steps);
            (
                steps.clone().filter(|s| s.state() == State::Passed).count(),
                steps.count(),
            )
        });
        let pages = [
            (Page::Steps, "Steps", None),
            (Page::Logs, "Logs", None),
            (Page::Annotations, "Annotations", Some(annotations)),
            (Page::Artifacts, "Artifacts", Some(artifacts)),
        ];
        let failing = run.state() == State::Failed;
        div()
            .flex()
            .flex_none()
            .items_center()
            .gap(px(4.))
            .h(px(44.))
            .px(px(16.))
            .border_b_1()
            .border_color(theme::stroke())
            .children(
                pages
                    .into_iter()
                    .enumerate()
                    .map(|(i, (page, label, count))| {
                        let on = page == self.page;
                        div()
                            .id(("run-page", i))
                            .flex()
                            .items_center()
                            .gap(px(6.))
                            .h(px(26.))
                            .px(px(10.))
                            .rounded(px(8.))
                            .text_size(px(ui::TEXT_SM))
                            .font_weight(FontWeight::MEDIUM)
                            .cursor_pointer()
                            .map(|d| {
                                if on {
                                    d.bg(theme::selection_strong()).text_color(theme::content())
                                } else {
                                    d.text_color(theme::text_subtle())
                                        .hover(|s| s.text_color(theme::content()))
                                }
                            })
                            .child(label)
                            .when_some(count.filter(|n| *n > 0), |d, n| {
                                d.child(
                                    div()
                                        .px(px(6.))
                                        .rounded(px(4.))
                                        .text_size(px(ui::TEXT_XS))
                                        .bg(if failing && page == Page::Annotations {
                                            soft(RED, 0.18)
                                        } else {
                                            theme::mix(0.08)
                                        })
                                        .text_color(if failing && page == Page::Annotations {
                                            hex(RED)
                                        } else {
                                            theme::mix(0.6)
                                        })
                                        .child(n.to_string()),
                                )
                            })
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.page = page;
                                cx.notify();
                            }))
                    }),
            )
            .child(div().flex_1())
            .when(total > 0, |d| {
                d.child(
                    div()
                        .text_size(px(ui::TEXT_XS))
                        .text_color(theme::text_faint())
                        .child(format!("{done} of {total} steps passed")),
                )
            })
    }

    fn render_steps(&self, run: &Run, d: &Arc<Detail>, cx: &mut Context<Self>) -> AnyElement {
        let mut items: Vec<AnyElement> = Vec::new();
        let failure = d.failure.as_ref();
        for (ji, job) in d.jobs.iter().enumerate() {
            let js = job.state();
            let open =
                self.expanded.contains(&job.id) || matches!(js, State::Failed | State::Running);
            let dur = job.duration().map(ci::duration_label);
            let job_id = job.id;
            if !open {
                let label = match (js, &dur) {
                    (State::Passed, Some(t)) => format!("Job passed · {t}"),
                    (s, Some(t)) => format!("{} · {t}", s.label()),
                    (s, None) => s.label().to_string(),
                };
                items.push(
                    timeline_item(
                        state_color(js),
                        div()
                            .id(("job-fold", ji))
                            .cursor_pointer()
                            .child(item_label(label, theme::mix(0.45)))
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap(px(6.))
                                    .child(div().text_size(px(ui::TEXT_LG)).child(job.name.clone()))
                                    .when(!job.steps.is_empty(), |d| {
                                        d.child(
                                            div()
                                                .text_size(px(ui::TEXT_XS))
                                                .text_color(theme::text_faint())
                                                .child(format!("{} steps", job.steps.len())),
                                        )
                                    }),
                            )
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.expanded.insert(job_id);
                                cx.notify();
                            }))
                            .into_any_element(),
                    )
                    .into_any_element(),
                );
                continue;
            }
            let failed_at = job.steps.iter().position(|s| s.state() == State::Failed);
            let mut passed_run = 0usize;
            let flush = |items: &mut Vec<AnyElement>, n: &mut usize| {
                if *n > 0 {
                    items.push(
                        timeline_item(
                            hex(GREEN),
                            div()
                                .child(item_label(
                                    format!(
                                        "{} · {n} step{} passed",
                                        job.name,
                                        if *n == 1 { "" } else { "s" }
                                    ),
                                    theme::mix(0.45),
                                ))
                                .into_any_element(),
                        )
                        .into_any_element(),
                    );
                    *n = 0;
                }
            };
            let fully_open = self.expanded.contains(&job.id);
            for (si, step) in job.steps.iter().enumerate() {
                let ss = step.state();
                let near_failure = failed_at.is_some_and(|f| si + 1 == f);
                if ss == State::Passed && !fully_open && !near_failure {
                    passed_run += 1;
                    continue;
                }
                flush(&mut items, &mut passed_run);
                if failed_at.is_some_and(|f| si > f) && ss == State::Skipped && !fully_open {
                    let rest = job.steps[si..]
                        .iter()
                        .filter(|s| s.state() == State::Skipped)
                        .count();
                    items.push(
                        timeline_item(
                            theme::mix(0.25),
                            div()
                                .child(item_label(
                                    format!(
                                        "{rest} later step{} skipped",
                                        if rest == 1 { "" } else { "s" }
                                    ),
                                    theme::mix(0.35),
                                ))
                                .into_any_element(),
                        )
                        .into_any_element(),
                    );
                    break;
                }
                let time = step.duration().map(ci::clock);
                let label = match ss {
                    State::Failed => format!("Step {} failed", step.number),
                    State::Running => format!("{} · Step {} · running", job.name, step.number),
                    _ => format!("{} · Step {}", job.name, step.number),
                };
                let label = match &time {
                    Some(t) if ss != State::Running => format!("{label} · {t}"),
                    _ => label,
                };
                let color = if ss == State::Failed {
                    hex(RED)
                } else {
                    theme::mix(0.45)
                };
                let mut body = div().flex().flex_col().gap(px(10.)).child(
                    div().child(item_label(label, color)).child(
                        div()
                            .text_size(px(ui::TEXT_LG))
                            .child(step.title().to_string()),
                    ),
                );
                if ss == State::Failed
                    && let Some(f) = failure.filter(|f| f.job == job.id)
                {
                    body = body.child(log_box(
                        ("fail-log", ji),
                        &f.lines,
                        window_copy_text(&f.lines),
                    ));
                    for (ai, a) in d
                        .annotations
                        .iter()
                        .enumerate()
                        .filter(|(_, a)| a.in_code() && a.job == job.name)
                        .take(3)
                    {
                        body = body.child(self.code_card(ji * 100 + ai, run, a, cx));
                    }
                }
                items.push(
                    timeline_item(state_color(ss), body.into_any_element()).into_any_element(),
                );
            }
            flush(&mut items, &mut passed_run);
            if fully_open && js.done() {
                items.push(
                    div()
                        .id(("job-fold-up", ji))
                        .pl(px(22.))
                        .pb(px(16.))
                        .text_size(px(ui::TEXT_XS))
                        .text_color(theme::text_faint())
                        .cursor_pointer()
                        .hover(|s| s.text_color(theme::content()))
                        .child(format!("Fold {}", job.name))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.expanded.remove(&job_id);
                            cx.notify();
                        }))
                        .into_any_element(),
                );
            } else if js == State::Failed {
                items.push(
                    div()
                        .id(("job-unfold", ji))
                        .pl(px(22.))
                        .pb(px(16.))
                        .text_size(px(ui::TEXT_XS))
                        .text_color(theme::text_faint())
                        .cursor_pointer()
                        .hover(|s| s.text_color(theme::content()))
                        .child(format!("Show every step of {}", job.name))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.expanded.insert(job_id);
                            cx.notify();
                        }))
                        .into_any_element(),
                );
            }
        }
        if let Some(list) = self.sessions.get(&run.id) {
            for (i, s) in list.iter().enumerate() {
                items.push(self.fix_item(i, run, s, cx));
            }
        }
        if items.is_empty() {
            return empty(
                "activity01",
                "No jobs yet",
                "GitHub hasn't started this run's jobs.",
            );
        }
        div()
            .id("run-steps")
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .track_scroll(&self.detail_scroll)
            .px(px(24.))
            .pt(px(16.))
            .pb(px(24.))
            .child(div().max_w(px(860.)).children(items))
            .into_any_element()
    }

    fn fix_item(
        &self,
        i: usize,
        run: &Run,
        s: &LinkedSession,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let (color, label, line) = match s.status {
            Status::Working => (
                hex(AMBER),
                "Fix started",
                format!("{} is fixing it", s.agent.label()),
            ),
            Status::NeedsInput => (
                hex(AMBER),
                "Fix waiting",
                format!("{} needs your input", s.agent.label()),
            ),
            Status::Error => (
                hex(RED),
                "Fix stopped",
                "The session stopped with an error".to_string(),
            ),
            Status::Idle => (
                hex(GREEN),
                "Fix ready",
                format!("{} finished; review the changes", s.agent.label()),
            ),
        };
        let (project, env, id) = (s.project, s.env, s.id.clone());
        let ship = (s.status == Status::Idle && run.state() == State::Failed && !run.is_deploy())
            .then(|| {
                let (id, branch) = (s.id.clone(), run.head_branch.clone());
                header_button(("fix-ship", i), "cloud-upload", "Commit & push", None).on_click(
                    cx.listener(move |_, _, _, cx| {
                        cx.emit(RunsEvent::Ship {
                            project,
                            env,
                            id: id.clone(),
                            branch: branch.clone(),
                        })
                    }),
                )
            });
        timeline_item(
            color,
            div()
                .flex()
                .flex_col()
                .gap(px(10.))
                .child(
                    div()
                        .child(item_label(
                            format!("{label} · {}", model::age(s.updated)),
                            color,
                        ))
                        .child(div().text_size(px(ui::TEXT_LG)).child(line)),
                )
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(10.))
                        .px(px(12.))
                        .py(px(10.))
                        .rounded(px(12.))
                        .border_1()
                        .border_color(theme::border())
                        .bg(theme::fill_raised())
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .justify_center()
                                .size(px(26.))
                                .rounded(px(8.))
                                .bg(theme::fill_hover())
                                .child(icon("sparkles", 13.).text_color(theme::text_muted())),
                        )
                        .child(
                            div()
                                .flex()
                                .flex_col()
                                .flex_1()
                                .min_w_0()
                                .child(
                                    div()
                                        .truncate()
                                        .text_size(px(ui::TEXT_MD))
                                        .font_weight(FontWeight::MEDIUM)
                                        .child(s.title.clone()),
                                )
                                .child(
                                    div()
                                        .text_size(px(ui::TEXT_XS))
                                        .text_color(theme::text_subtle())
                                        .child(format!(
                                            "{} · {}",
                                            s.agent.label(),
                                            status_label(s.status)
                                        )),
                                ),
                        )
                        .children(ship)
                        .child(
                            header_button(("fix-open", i), "message01", "Open session", None)
                                .on_click(cx.listener(move |_, _, _, cx| {
                                    cx.emit(RunsEvent::OpenSession {
                                        project,
                                        env,
                                        id: id.clone(),
                                    })
                                })),
                        ),
                )
                .into_any_element(),
        )
        .into_any_element()
    }

    /// The newest fix session of an earlier run on the same branch.
    fn branch_fix_session(&self, run: &Run, cx: &App) -> Option<LinkedSession> {
        let repo = self.ci.read(cx).repo(self.root.as_ref()?)?;
        self.sessions
            .iter()
            .filter(|(id, _)| {
                **id != run.id
                    && repo
                        .run(**id)
                        .is_some_and(|r| r.head_branch == run.head_branch)
            })
            .flat_map(|(_, list)| list.iter())
            .max_by_key(|s| s.updated)
            .cloned()
    }

    /// The annotated lines of a file, with the problem under them.
    fn code_card(
        &self,
        key: usize,
        run: &Run,
        a: &Annotation,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let color = level_color(a.level);
        let (branch, path) = (run.head_branch.clone(), a.path.clone());
        let loc = format!("{}:{}", a.path, a.line);
        let copied = crate::agent::was_copied(&loc);
        let copy = loc.clone();
        let mut lines = div()
            .flex()
            .flex_col()
            .py(px(6.))
            .font_family(theme::FONT_MONO)
            .text_size(px(ui::TEXT_SM));
        for (n, text) in &a.snippet {
            let hit = (a.line..=a.end_line).contains(n);
            lines = lines.child(
                div()
                    .flex()
                    .min_h(px(20.))
                    .items_center()
                    .when(hit, |d| d.bg(soft_level(a.level, 0.12)))
                    .child(
                        div()
                            .w(px(2.))
                            .h_full()
                            .min_h(px(20.))
                            .when(hit, |d| d.bg(color)),
                    )
                    .child(
                        div()
                            .w(px(44.))
                            .flex_none()
                            .pr(px(10.))
                            .text_right()
                            .text_color(if hit { color } else { theme::mix(0.3) })
                            .child(n.to_string()),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_color(theme::text_muted())
                            .child(text.replace('\t', "    ")),
                    ),
            );
        }
        div()
            .flex()
            .flex_col()
            .rounded(px(12.))
            .border_1()
            .border_color(theme::border())
            .bg(theme::fill_raised())
            .overflow_hidden()
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .h(px(32.))
                    .px(px(12.))
                    .border_b_1()
                    .border_color(theme::border_subtle())
                    .child(icon("file-code", 13.).text_color(theme::text_subtle()))
                    .child(
                        div()
                            .font_family(theme::FONT_MONO)
                            .text_size(px(ui::TEXT_SM))
                            .child(a.path.clone()),
                    )
                    .child(
                        div()
                            .font_family(theme::FONT_MONO)
                            .text_size(px(ui::TEXT_SM))
                            .text_color(theme::text_faint())
                            .child(format!(":{}", a.line)),
                    )
                    .child(div().flex_1())
                    .child(
                        ui::icon_button(("code-open", key), "link-square02", 13.).on_click(
                            cx.listener(move |_, _, _, cx| {
                                cx.emit(RunsEvent::OpenFile {
                                    branch: branch.clone(),
                                    path: path.clone(),
                                })
                            }),
                        ),
                    )
                    .child(
                        ui::icon_button(
                            ("code-copy", key),
                            if copied { "tick02" } else { "copy01" },
                            13.,
                        )
                        .on_click(move |_, window, cx| {
                            crate::agent::copy_with_feedback(&copy, window, cx)
                        }),
                    ),
            )
            .when(!a.snippet.is_empty(), |d| d.child(lines))
            .child(
                div()
                    .flex()
                    .items_start()
                    .gap(px(8.))
                    .mx(px(10.))
                    .mb(px(10.))
                    .when(a.snippet.is_empty(), |d| d.mt(px(10.)))
                    .px(px(10.))
                    .py(px(8.))
                    .rounded(px(8.))
                    .bg(soft_level(a.level, 0.10))
                    .text_size(px(ui::TEXT_SM))
                    .text_color(color)
                    .child(
                        div()
                            .pt(px(2.))
                            .child(icon("alert-circle", 13.).text_color(color)),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .when(!a.title.is_empty(), |d| {
                                d.child(
                                    div()
                                        .font_weight(FontWeight::SEMIBOLD)
                                        .child(a.title.clone()),
                                )
                            })
                            .child(a.message.clone()),
                    ),
            )
            .into_any_element()
    }

    fn render_logs(
        &mut self,
        d: &Arc<Detail>,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        if d.jobs.is_empty() {
            return empty("file01", "No logs yet", "The jobs haven't started.");
        }
        let job = self
            .log_job
            .filter(|j| d.jobs.iter().any(|x| x.id == *j))
            .or_else(|| d.failed_job().map(|j| j.id))
            .unwrap_or(d.jobs[0].id);
        if let Some(root) = self.root.clone() {
            self.ci.update(cx, |ci, cx| ci.load_log(&root, job, cx));
        }
        let load = self
            .root
            .as_ref()
            .and_then(|r| self.ci.read(cx).repo(r))
            .and_then(|r| match r.log(job)? {
                ci::Load::Loading => Some(Err(None)),
                ci::Load::Ready(l) => Some(Ok(l.clone())),
                ci::Load::Failed(e) => Some(Err(Some(e.clone()))),
            });
        let pills = div()
            .flex()
            .flex_none()
            .flex_wrap()
            .gap(px(6.))
            .px(px(16.))
            .py(px(10.))
            .border_b_1()
            .border_color(theme::stroke())
            .children(d.jobs.iter().enumerate().map(|(i, j)| {
                let on = j.id == job;
                let id = j.id;
                div()
                    .id(("log-job", i))
                    .flex()
                    .items_center()
                    .gap(px(6.))
                    .h(px(24.))
                    .px(px(8.))
                    .rounded(px(8.))
                    .border_1()
                    .border_color(if on {
                        theme::mix(0.18)
                    } else {
                        theme::mix(0.08)
                    })
                    .when(on, |d| d.bg(theme::fill_hover()))
                    .text_size(px(ui::TEXT_SM))
                    .cursor_pointer()
                    .hover(|s| s.bg(theme::fill_hover()))
                    .child(div().size(px(6.)).rounded_full().bg(state_color(j.state())))
                    .child(j.name.clone())
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.log_job = Some(id);
                        cx.notify();
                    }))
            }));
        let body = match load {
            None | Some(Err(None)) => div()
                .flex()
                .flex_1()
                .items_center()
                .justify_center()
                .child(ui::spinner("log-loading", 16., theme::mix(0.4)))
                .into_any_element(),
            Some(Err(Some(e))) => empty("alert-circle", "Couldn't read the log", e.as_ref()),
            Some(Ok(lines)) => {
                let rows = lines.clone();
                uniform_list(
                    "run-log",
                    rows.len(),
                    move |range: std::ops::Range<usize>, _, _| {
                        range
                            .map(|ix| {
                                let l = &rows[ix];
                                div()
                                    .flex()
                                    .h(px(19.))
                                    .px(px(16.))
                                    .font_family(theme::FONT_MONO)
                                    .text_size(px(ui::TEXT_SM))
                                    .child(
                                        div()
                                            .w(px(48.))
                                            .flex_none()
                                            .text_right()
                                            .pr(px(12.))
                                            .text_color(theme::text_faint())
                                            .child((ix + 1).to_string()),
                                    )
                                    .child(log_text(l).truncate())
                                    .into_any_element()
                            })
                            .collect::<Vec<_>>()
                    },
                )
                .track_scroll(&self.log_scroll)
                .flex_1()
                .min_h_0()
                .py(px(8.))
                .into_any_element()
            }
        };
        div()
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .child(pills)
            .child(body)
            .into_any_element()
    }

    fn render_annotations(&self, run: &Run, d: &Arc<Detail>, cx: &mut Context<Self>) -> AnyElement {
        if d.annotations.is_empty() {
            return empty(
                "checkmark-circle01",
                "No annotations",
                "Nothing pointed at the code in this run.",
            );
        }
        let cards: Vec<AnyElement> = d
            .annotations
            .iter()
            .enumerate()
            .map(|(i, a)| {
                if a.in_code() {
                    return div()
                        .flex()
                        .flex_col()
                        .gap(px(6.))
                        .child(item_label(
                            format!("{} · {}", a.job, level_label(a.level)),
                            level_color(a.level),
                        ))
                        .child(self.code_card(10_000 + i, run, a, cx))
                        .into_any_element();
                }
                div()
                    .flex()
                    .items_start()
                    .gap(px(10.))
                    .px(px(12.))
                    .py(px(10.))
                    .rounded(px(12.))
                    .border_1()
                    .border_color(theme::border())
                    .bg(theme::fill_raised())
                    .child(
                        div().pt(px(2.)).child(
                            icon(
                                if a.level == Level::Failure {
                                    "cancel-circle"
                                } else {
                                    "alert-circle"
                                },
                                14.,
                            )
                            .text_color(level_color(a.level)),
                        ),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap(px(2.))
                            .flex_1()
                            .min_w_0()
                            .child(
                                div()
                                    .text_size(px(ui::TEXT_XS))
                                    .text_color(theme::text_subtle())
                                    .child(format!("{} · {}", a.job, level_label(a.level))),
                            )
                            .child(div().text_size(px(ui::TEXT_MD)).child(a.message.clone())),
                    )
                    .into_any_element()
            })
            .collect();
        div()
            .id("run-annotations")
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .px(px(24.))
            .py(px(16.))
            .child(
                div()
                    .max_w(px(860.))
                    .flex()
                    .flex_col()
                    .gap(px(12.))
                    .children(cards),
            )
            .into_any_element()
    }

    fn render_artifacts(&self, run: &Run, d: &Arc<Detail>, _cx: &mut Context<Self>) -> AnyElement {
        if d.artifacts.is_empty() {
            return empty(
                "archive02",
                "No artifacts",
                "This run didn't upload any files.",
            );
        }
        let url = format!("{}#artifacts", run.url);
        div()
            .id("run-artifacts")
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .px(px(16.))
            .py(px(12.))
            .children(d.artifacts.iter().enumerate().map(|(i, a)| {
                let url = url.clone();
                div()
                    .id(("artifact", i))
                    .flex()
                    .items_center()
                    .gap(px(10.))
                    .h(px(40.))
                    .px(px(10.))
                    .rounded(px(12.))
                    .cursor_pointer()
                    .hover(|s| s.bg(theme::hover()))
                    .when(a.expired, |d| d.opacity(0.5))
                    .child(icon("zip01", 15.).text_color(theme::text_subtle()))
                    .child(div().flex_1().min_w_0().truncate().child(a.name.clone()))
                    .child(
                        div()
                            .text_size(px(ui::TEXT_XS))
                            .text_color(theme::text_subtle())
                            .child(if a.expired {
                                "Expired".to_string()
                            } else {
                                ci::size_label(a.size_in_bytes)
                            }),
                    )
                    .child(icon("download04", 14.).text_color(theme::text_subtle()))
                    .on_click(move |_, _, cx| cx.open_url(&url))
            }))
            .into_any_element()
    }
}

fn status_label(s: Status) -> &'static str {
    match s {
        Status::Working => "Working",
        Status::NeedsInput => "Needs input",
        Status::Error => "Error",
        Status::Idle => "Done",
    }
}

fn level_color(l: Level) -> Hsla {
    match l {
        Level::Failure => hex(RED),
        Level::Warning => hex(AMBER),
        Level::Notice => theme::mix(0.6),
    }
}

fn soft_level(l: Level, alpha: f32) -> Hsla {
    match l {
        Level::Failure => soft(RED, alpha),
        Level::Warning => soft(AMBER, alpha),
        Level::Notice => theme::mix(alpha * 0.6),
    }
}

fn soft_state(s: State, alpha: f32) -> Hsla {
    match s {
        State::Passed => soft(GREEN, alpha),
        State::Failed => soft(RED, alpha),
        State::Running | State::Queued => soft(AMBER, alpha),
        _ => theme::mix(alpha * 0.6),
    }
}

fn level_label(l: Level) -> &'static str {
    match l {
        Level::Failure => "Error",
        Level::Warning => "Warning",
        Level::Notice => "Notice",
    }
}

fn item_label(text: impl Into<SharedString>, color: Hsla) -> gpui::Div {
    div()
        .text_size(px(ui::TEXT_XS))
        .line_height(px(16.))
        .text_color(color)
        .child(text.into())
}

/// A row of the Steps timeline: its dot, the line down to the next row, and the content.
fn timeline_item(dot: Hsla, body: AnyElement) -> gpui::Div {
    div()
        .flex()
        .gap(px(12.))
        .child(
            div()
                .relative()
                .flex_none()
                .w(px(10.))
                .child(
                    div()
                        .absolute()
                        .left(px(4.5))
                        .top(px(14.))
                        .bottom(px(-2.))
                        .w(px(1.))
                        .bg(theme::fill_selected()),
                )
                .child(
                    div()
                        .absolute()
                        .left(px(2.))
                        .top(px(4.))
                        .size(px(6.))
                        .rounded_full()
                        .bg(dot),
                ),
        )
        .child(div().flex_1().min_w_0().pb(px(16.)).child(body))
}

fn log_text(l: &LogLine) -> gpui::Div {
    let (color, text): (Hsla, String) = match l.kind {
        LineKind::Group => (theme::mix(0.85), format!("▸ {}", l.text)),
        LineKind::Setup => (theme::mix(0.35), l.text.clone()),
        LineKind::Command => (theme::mix(0.5), format!("$ {}", l.text)),
        LineKind::Error => (hex(RED), l.text.clone()),
        LineKind::Warning => (hex(AMBER), l.text.clone()),
        LineKind::Notice => (theme::accent(), l.text.clone()),
        LineKind::Plain => (theme::mix(0.72), l.text.clone()),
    };
    div().flex_1().min_w_0().text_color(color).child(text)
}

fn window_copy_text(lines: &[LogLine]) -> String {
    lines
        .iter()
        .map(|l| {
            if l.kind == LineKind::Group {
                format!("$ {}", l.text.strip_prefix("Run ").unwrap_or(&l.text))
            } else {
                l.text.clone()
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// The end of the failing step's output, as a terminal would show it.
fn log_box(id: impl Into<gpui::ElementId>, lines: &[LogLine], copy: String) -> AnyElement {
    let copied = crate::agent::was_copied(&copy);
    div()
        .relative()
        .flex()
        .flex_col()
        .px(px(12.))
        .py(px(10.))
        .rounded(px(12.))
        .border_1()
        .border_color(theme::border_subtle())
        .bg(theme::surface(0.065))
        .font_family(theme::FONT_MONO)
        .text_size(px(ui::TEXT_SM))
        .line_height(px(19.))
        .children(lines.iter().map(|l| {
            let (color, text): (Hsla, String) = match l.kind {
                LineKind::Group => (
                    theme::mix(0.6),
                    format!("$ {}", l.text.strip_prefix("Run ").unwrap_or(&l.text)),
                ),
                LineKind::Error => (hex(RED), format!("Error: {}", l.text)),
                LineKind::Warning => (hex(AMBER), l.text.clone()),
                _ if l.text.contains("error") => (hex(RED).opacity(0.75), l.text.clone()),
                _ => (theme::mix(0.7), l.text.clone()),
            };
            div().text_color(color).child(text)
        }))
        .child(
            div().absolute().top(px(6.)).right(px(6.)).child(
                ui::icon_button(id, if copied { "tick02" } else { "copy01" }, 13.).on_click(
                    move |_, window, cx| crate::agent::copy_with_feedback(&copy, window, cx),
                ),
            ),
        )
        .into_any_element()
}

/// Durations of a workflow's runs, oldest first, as a line; failures are red
/// dots and `current` a larger one.
fn sparkline(points: Vec<(u64, u64, State)>, current: u64) -> impl IntoElement {
    canvas(
        |_, _, _| {},
        move |bounds: Bounds<Pixels>, _, window: &mut Window, _: &mut App| {
            let max = points.iter().map(|(_, s, _)| *s).max().unwrap_or(1).max(1) as f32;
            let (w, h) = (f32::from(bounds.size.width), f32::from(bounds.size.height));
            let pad = 4.;
            let step = if points.len() > 1 {
                (w - pad * 2.) / (points.len() - 1) as f32
            } else {
                0.
            };
            let at = |i: usize, secs: u64| {
                point(
                    bounds.origin.x + px(pad + i as f32 * step),
                    bounds.origin.y + px(pad + (h - pad * 2.) * (1. - secs as f32 / max)),
                )
            };
            let mut path = PathBuilder::stroke(px(1.5));
            for (i, (_, secs, _)) in points.iter().enumerate() {
                if i == 0 {
                    path.move_to(at(i, *secs));
                } else {
                    path.line_to(at(i, *secs));
                }
            }
            if let Ok(p) = path.build() {
                window.paint_path(p, theme::mix(0.3));
            }
            for (i, (id, secs, state)) in points.iter().enumerate() {
                let here = *id == current;
                if *state != State::Failed && !here {
                    continue;
                }
                let r = if here { 3.5 } else { 2.5 };
                let c = at(i, *secs);
                let color = if *state == State::Failed {
                    hex(RED)
                } else {
                    theme::content()
                };
                window.paint_quad(
                    gpui::fill(
                        Bounds::new(
                            point(c.x - px(r), c.y - px(r)),
                            gpui::size(px(r * 2.), px(r * 2.)),
                        ),
                        color,
                    )
                    .corner_radii(px(r)),
                );
            }
        },
    )
    .w_full()
    .h(px(36.))
}
