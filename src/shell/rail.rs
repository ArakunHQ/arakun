//! The project rail on the far left: search, views and projects with their environments.

use super::*;

/// Height of an environment row under its project, and the space between rows.
const ENV_ROW_H: f32 = 28.;
const ENV_GAP: f32 = 2.;

pub(super) fn status_dot(status: Status) -> impl IntoElement {
    let color = match status {
        Status::Idle => theme::mix(0.25),
        Status::Working => theme::working(),
        Status::NeedsInput => theme::needs_you(),
        Status::Error => theme::diff_del().into(),
    };
    div().size(px(6.)).rounded_full().bg(color).flex_none()
}

/// A run state as a small dot; work in progress pulses as a ring.
pub(super) fn ci_mark(state: ci::State) -> gpui::Div {
    let color: Hsla = match state {
        ci::State::Passed => theme::success().into(),
        ci::State::Failed => theme::error().into(),
        ci::State::Running | ci::State::Queued => theme::attention().into(),
        _ => theme::mix(0.3),
    };
    if state.done() {
        div().size(px(6.)).rounded_full().bg(color).flex_none()
    } else {
        div()
            .size(px(7.))
            .rounded_full()
            .border_1()
            .border_color(color)
            .flex_none()
    }
}

impl Shell {
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
                        icon("layout-align-right", 14.).text_color(if self.panel_open {
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
            .children(self.render_pinned_block(cx))
            .children(self.render_rail_sections(cx))
            .pb(crate::ui::sp(4.))
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
                        d.opacity(0.65).hover(|s| s.bg(theme::hover()).opacity(1.))
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
                // Envs nest under their project like steps in the chat: a spine
                // down from the project icon that curves into each row.
                let mut envs = div()
                    .flex()
                    .flex_col()
                    .gap(crate::ui::sp(ENV_GAP))
                    .ml(px(10.));
                for (ei, env) in project.environments.iter().enumerate() {
                    let active = ei == project.active_env;
                    envs = envs.child(ui::tree_item(
                        false,
                        ENV_ROW_H / 2.,
                        ENV_GAP,
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
                            .pl(crate::ui::sp(8.))
                            .pr(crate::ui::sp(8.))
                            .rounded(crate::ui::rad(8.))
                            .cursor_pointer()
                            .text_size(px(ui::TEXT_MD))
                            .text_color(if active {
                                theme::content()
                            } else {
                                theme::mix(0.5)
                            })
                            .when(active, |d| d.bg(theme::selection()))
                            .when(!active, |d| {
                                d.hover(|s| s.bg(theme::hover()).text_color(theme::content()))
                            })
                            .child(
                                icon(
                                    if env.is_checkout {
                                        "folder01"
                                    } else {
                                        "git-branch"
                                    },
                                    14.,
                                )
                                .text_color(if active {
                                    theme::mix(0.7)
                                } else {
                                    theme::mix(0.4)
                                }),
                            )
                            .child(div().flex_1().min_w_0().truncate().child(env.name.clone()))
                            .when_some(
                                Some(env.diff_totals()).filter(|(a, r)| show_changes && a + r > 0),
                                |d, (a, r)| {
                                    d.child(
                                        div()
                                            .flex()
                                            .gap(crate::ui::sp(3.))
                                            .text_size(px(ui::TEXT_XS))
                                            .font_weight(FontWeight::SEMIBOLD)
                                            .child(
                                                div()
                                                    .text_color(theme::diff_add())
                                                    .child(format!("+{a}")),
                                            )
                                            .when(r > 0, |d| {
                                                d.child(
                                                    div()
                                                        .text_color(theme::diff_del())
                                                        .child(format!("-{r}")),
                                                )
                                            }),
                                    )
                                },
                            )
                            .when_some(self.env_ci(pi, ei, cx), |d, (state, run)| {
                                let id = run.id;
                                d.child(
                                    div()
                                        .id(("env-ci", pi * 1000 + ei))
                                        .flex()
                                        .items_center()
                                        .justify_center()
                                        .size(px(14.))
                                        .rounded(crate::ui::rad(4.))
                                        .hover(|s| s.bg(theme::fill_selected()))
                                        .child(ci_mark(state))
                                        .on_click(cx.listener(move |this, _, window, cx| {
                                            cx.stop_propagation();
                                            this.select_env(pi, ei, window, cx);
                                            this.open_runs_tab(Some(id), window, cx);
                                        })),
                                )
                            })
                            .child(status_dot(env.aggregate_status()))
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.select_env(pi, ei, window, cx)
                            }))
                            .into_any_element(),
                    ));
                }
                envs = envs.child(ui::tree_item(
                    true,
                    ENV_ROW_H / 2.,
                    ENV_GAP,
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
                        .child(hover_icon(
                            "add01",
                            14.,
                            theme::text_faint(),
                            theme::mix(0.7),
                            "env-add-row",
                        ))
                        .child("New environment")
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.open_create_env(pi, window, cx)
                        }))
                        .into_any_element(),
                ));
                projects = projects.child(envs);
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
                    .when(added + removed > 0, |d| {
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
