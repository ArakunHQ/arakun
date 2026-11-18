//! The Workspace panel: its Sessions, Explorer and Changes tabs.

use super::*;

impl Shell {
    pub(super) fn render_panel(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let env = self.env();
        let title: SharedString = if env.is_checkout {
            "Workspace".into()
        } else {
            env.branch.clone()
        };

        let header = div()
            .flex()
            .flex_none()
            .items_center()
            .h(px(ui::BAR_H))
            // The switcher's highlight and the buttons sit as far from the sides as
            // from the top.
            .pl(ui::icon_inset())
            .pr(ui::icon_inset())
            .gap(crate::ui::sp(4.))
            .child(
                div()
                    .id("env-switcher")
                    .flex()
                    .items_center()
                    .gap(crate::ui::sp(8.))
                    .h(px(ui::ICON_BUTTON))
                    .px(crate::ui::sp(6.))
                    .rounded(ui::nested(ui::RADIUS_XL, ui::ICON_INSET))
                    .min_w_0()
                    .text_size(px(ui::TEXT_LG))
                    .font_weight(FontWeight::MEDIUM)
                    .cursor_pointer()
                    .hover(|s| s.bg(theme::fill_selected()))
                    .when(self.env_menu_open, |d| d.bg(theme::fill_selected()))
                    .child(div().min_w_0().truncate().child(title))
                    .child(icon("unfold-more", 14.).text_color(theme::text_subtle()))
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.env_menu_open = !this.env_menu_open;
                        this.env_menu_at = None;
                        cx.notify();
                    })),
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
            .child(
                icon_button("panel-search", "search01", 14.)
                    .tooltip(ui::tip("Search sessions (Ctrl+K)"))
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.panel_tab = PanelTab::Sessions;
                        this.env_menu_open = false;
                        let field = this.session_query.clone();
                        window.focus(&field.focus_handle(cx), cx);
                        field.update(cx, |f, cx| f.select_all_text(cx));
                        cx.notify();
                    })),
            )
            .child(
                icon_button("panel-new", "add01", 14.)
                    .tooltip(ui::tip("New session (Ctrl+T)"))
                    .on_click(cx.listener(|this, _, window, cx| this.new_session(window, cx))),
            );

        let changed_files = env.changes.len();
        let tabs = div()
            .flex()
            .flex_none()
            .items_center()
            .h(px(40.))
            .px(crate::ui::sp(8.))
            .child(
                div()
                    .flex()
                    .flex_1()
                    // Or its labels' full widths hold it open past the panel.
                    .min_w_0()
                    .items_center()
                    .h(px(28.))
                    .p(crate::ui::sp(2.))
                    .gap(crate::ui::sp(2.))
                    .rounded(crate::ui::rad(8.))
                    .bg(theme::fill_hover())
                    .children(
                        [PanelTab::Sessions, PanelTab::Explorer, PanelTab::Changes]
                            .into_iter()
                            .map(|tab| {
                                let active = tab == self.panel_tab;
                                let label = match tab {
                                    PanelTab::Sessions => "Sessions",
                                    PanelTab::Explorer => "Explorer",
                                    PanelTab::Changes => "Changes",
                                };
                                div()
                                    .id(label)
                                    .flex()
                                    // Sized from their labels, so a tab with a
                                    // count gets the room for it. On a narrow
                                    // panel the labels shorten rather than push
                                    // the last tab out.
                                    .flex_auto()
                                    .min_w_0()
                                    .items_center()
                                    .justify_center()
                                    .gap(crate::ui::sp(6.))
                                    .h_full()
                                    .px(crate::ui::sp(8.))
                                    // 2 px inside an 8 px track: parallel to it.
                                    .rounded(ui::nested(ui::RADIUS, 2.))
                                    .text_size(px(ui::TEXT_SM))
                                    .line_height(px(12.))
                                    .cursor_pointer()
                                    .when(active, |d| {
                                        d.bg(theme::selection()).text_color(theme::content())
                                    })
                                    .when(!active, |d| {
                                        d.text_color(theme::text_subtle()).hover(|s| {
                                            s.bg(theme::hover()).text_color(theme::content())
                                        })
                                    })
                                    .child(div().min_w_0().truncate().child(label))
                                    // The line counts are inside the tab; its label only says how many files.
                                    .when(tab == PanelTab::Changes && changed_files > 0, |d| {
                                        d.child(
                                            div()
                                                .flex()
                                                .flex_none()
                                                .items_center()
                                                .h(px(16.))
                                                .px(crate::ui::sp(4.))
                                                .rounded(crate::ui::rad(crate::ui::RADIUS_SM))
                                                .bg(theme::fill_selected())
                                                .text_size(px(crate::ui::TEXT_XS))
                                                .line_height(px(16.))
                                                .font_weight(FontWeight::MEDIUM)
                                                .text_color(theme::text_muted())
                                                .child(changed_files.to_string()),
                                        )
                                    })
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.panel_tab = tab;
                                        match tab {
                                            PanelTab::Explorer => {
                                                this.ensure_explorer(cx);
                                                this.refresh_visible(cx);
                                            }
                                            PanelTab::Changes => this.refresh_visible(cx),
                                            PanelTab::Sessions => {}
                                        }
                                        cx.notify();
                                    }))
                            }),
                    ),
            );

        let body: AnyElement = match self.panel_tab {
            PanelTab::Sessions => self.render_sessions(env, cx).into_any_element(),
            PanelTab::Explorer => self.render_explorer(cx).into_any_element(),
            PanelTab::Changes => self.render_changes(cx).into_any_element(),
        };

        self.side_card()
            .id("panel-card")
            .map(ui::stop_window_move)
            .flex()
            .flex_col()
            .flex_none()
            .w(px(self.panel_width()))
            .h_full()
            .child(header)
            .child(tabs)
            .child(body)
    }

    pub(super) fn render_sessions(
        &self,
        env: &Environment,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let search = div()
            .flex()
            .flex_none()
            .items_center()
            .h(px(36.))
            .px(crate::ui::sp(8.))
            .gap(crate::ui::sp(4.))
            .child(
                div()
                    .flex()
                    .flex_1()
                    .min_w_0()
                    .items_center()
                    .h(px(28.))
                    .pl(crate::ui::sp(8.))
                    .gap(crate::ui::sp(8.))
                    .child(icon("search01", 12.).text_color(theme::text_subtle()))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_size(px(ui::TEXT_SM))
                            .child(self.session_query.clone()),
                    ),
            )
            .when_some(self.session_filter, |d, agent| {
                d.child(
                    div()
                        .text_size(px(ui::TEXT_XS))
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(theme::text())
                        .child(agent.label()),
                )
            })
            .child(
                icon_button("session-import", "download04", 12.)
                    .tooltip(ui::tip("Import sessions"))
                    .on_click(
                        cx.listener(|this, _, window, cx| this.open_import_dialog(window, cx)),
                    ),
            )
            .child(
                icon_button("session-filter", "filter", 12.)
                    .tooltip(ui::tip("Filter by agent"))
                    .when(self.session_filter.is_some(), |d| {
                        d.bg(theme::fill_selected())
                    })
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.session_filter = match this.session_filter {
                            None => Some(Agent::Droid),
                            Some(Agent::Droid) => Some(Agent::Claude),
                            Some(Agent::Claude) => Some(Agent::OpenCode),
                            Some(Agent::OpenCode) => Some(Agent::Codex),
                            Some(Agent::Codex) => Some(Agent::WorkersAi),
                            Some(Agent::WorkersAi) => None,
                        };
                        cx.notify();
                    })),
            );
        let query = self.session_query.read(cx).text().to_lowercase();
        let filter = self.session_filter;
        let matches = |s: &model::Session| {
            filter.is_none_or(|a| s.agent == a)
                && (query.is_empty() || s.title.to_lowercase().contains(query.trim()))
        };
        let shown = env.sessions.iter().filter(|s| matches(s)).count();

        let list: AnyElement = if env.sessions.is_empty() {
            div()
                .flex()
                .flex_col()
                .flex_1()
                .items_center()
                .px(crate::ui::sp(24.))
                .py(crate::ui::sp(40.))
                .gap(crate::ui::sp(12.))
                .child(
                    div()
                        .text_size(px(ui::TEXT_MD))
                        .text_center()
                        .text_color(theme::text_subtle())
                        .child("No sessions yet"),
                )
                .child(
                    ui::button_with(
                        "sessions-import",
                        Some("circle-arrow-down01"),
                        "Import sessions",
                        ui::ButtonKind::Outline,
                        true,
                    )
                    .on_click(
                        cx.listener(|this, _, window, cx| this.open_import_dialog(window, cx)),
                    ),
                )
                .into_any_element()
        } else {
            let active_id = self.active_session_id();
            div()
                .id("session-list")
                .flex()
                .flex_col()
                .flex_1()
                .min_h_0()
                .overflow_y_scroll()
                .p(crate::ui::sp(6.))
                .gap(crate::ui::sp(2.))
                .when(shown == 0, |d| {
                    d.child(
                        div()
                            .py(crate::ui::sp(24.))
                            .text_center()
                            .text_size(px(ui::TEXT_SM))
                            .text_color(theme::text_faint())
                            .child("No sessions match"),
                    )
                })
                .children(
                    env.sessions
                        .iter()
                        .enumerate()
                        .filter(|(_, s)| matches(s))
                        .map(|(i, s)| {
                            let active = active_id.as_ref() == Some(&s.id);
                            let id = s.id.clone();
                            let menu_id = s.id.clone();
                            let lifted = self.lifted_session(cx) == Some(&s.id);
                            let dragged = drag::DraggedSession::new(
                                env.path.clone(),
                                s.id.clone(),
                                s.title.clone(),
                                s.agent,
                            );
                            let shell = cx.entity();
                            let card =
                                session_card(i, s, active, &self.project().name, &env.branch)
                                    .on_click(cx.listener(move |this, _, window, cx| {
                                        this.open_session(id.clone(), window, cx)
                                    }))
                                    .on_mouse_down(
                                        MouseButton::Right,
                                        cx.listener(
                                            move |this, ev: &gpui::MouseDownEvent, _, cx| {
                                                this.open_session_menu(
                                                    menu_id.clone(),
                                                    ev.position,
                                                    cx,
                                                )
                                            },
                                        ),
                                    )
                                    .on_drag(dragged, move |d, offset, _, cx| {
                                        shell.update(cx, |this, cx| {
                                            this.start_session_drag(d, offset, cx)
                                        })
                                    })
                                    .drag_over::<drag::DraggedSession>(|s, _, _, _| {
                                        s.bg(theme::active().opacity(0.10))
                                    })
                                    .on_drop(cx.listener(
                                        move |this, d: &drag::DraggedSession, _, cx| {
                                            this.drop_session_on_row(d, i, cx)
                                        },
                                    ))
                                    .when(lifted, |d| d.opacity(0.3));
                            self.sliding_card(i, &s.id, card)
                        }),
                )
                .into_any_element()
        };

        div()
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .child(search)
            .child(list)
    }
}

pub(super) fn session_card(
    i: usize,
    s: &model::Session,
    active: bool,
    project: &SharedString,
    branch: &SharedString,
) -> gpui::Stateful<gpui::Div> {
    let (status, color): (SharedString, Hsla) = match s.status {
        Status::Idle => (model::age(s.updated), theme::mix(0.45)),
        Status::Error => ("Error".into(), theme::diff_del().into()),
        Status::Working => ("Working...".into(), theme::working()),
        Status::NeedsInput => ("Need approval".into(), theme::needs_you()),
    };
    div()
        .id(("session", i))
        .flex()
        .flex_col()
        .px(crate::ui::sp(10.))
        .py(crate::ui::sp(8.))
        .rounded(crate::ui::rad(8.))
        .border_1()
        .cursor_pointer()
        .map(|d| match s.status {
            Status::NeedsInput => d
                .bg(theme::fill_hover())
                .border_dashed()
                .border_color(theme::mix(0.30)),
            _ => d.border_color(gpui::transparent_black()),
        })
        .when(active, |d| d.bg(theme::selection()))
        .when(!active, |d| d.hover(|s| s.bg(theme::hover())))
        .child(
            div()
                .flex()
                .items_center()
                .gap(crate::ui::sp(6.))
                .child(agent_icon(s.agent, 14.))
                .child(
                    div()
                        .flex_1()
                        .text_size(px(ui::TEXT_XS))
                        .text_color(theme::text_subtle())
                        .child(s.agent.label()),
                )
                .child(
                    div()
                        .text_size(px(ui::TEXT_XS))
                        .text_color(color)
                        .child(status),
                ),
        )
        .child(
            div()
                .mt(px(4.))
                .text_size(px(ui::TEXT_MD))
                .font_weight(FontWeight::SEMIBOLD)
                .truncate()
                .child(s.title.clone()),
        )
        .child(
            div()
                .mt(px(4.))
                .flex()
                .items_center()
                .gap(crate::ui::sp(4.))
                .text_size(px(ui::TEXT_XS))
                .text_color(theme::text_subtle())
                .child(icon("git-branch", 12.).text_color(theme::text_subtle()))
                .child(
                    div()
                        .flex_1()
                        .truncate()
                        .child(format!("{project}/{branch}")),
                )
                .when_some(s.meta.as_ref().and_then(|m| m.pr.clone()), |d, url| {
                    let number = url.rsplit('/').next().unwrap_or_default().to_string();
                    d.child(
                        div()
                            .id(("session-pr", i))
                            .flex()
                            .items_center()
                            .gap(crate::ui::sp(3.))
                            .px(crate::ui::sp(4.))
                            .rounded(crate::ui::rad(4.))
                            .text_color(theme::text_subtle())
                            .hover(|s| s.bg(theme::fill_selected()).text_color(theme::content()))
                            .child(icon("git-pull-request", 11.).text_color(theme::text_faint()))
                            .child(format!("#{number}"))
                            .on_click(move |_, _, cx| {
                                cx.stop_propagation();
                                cx.open_url(&url);
                            }),
                    )
                })
                .child(
                    div()
                        .text_color(theme::diff_add())
                        .child(format!("+{}", s.added)),
                )
                .child(
                    div()
                        .text_color(theme::diff_del())
                        .child(format!("-{}", s.removed)),
                ),
        )
}

pub(super) fn panel_empty(text: &str) -> gpui::Div {
    div()
        .px(crate::ui::sp(16.))
        .py(crate::ui::sp(12.))
        .text_size(px(ui::TEXT_SM))
        .line_height(px(18.))
        .text_color(theme::text_subtle())
        .child(text.to_string())
}
