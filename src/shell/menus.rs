//! Pop-up menus: the environment menu, setting menus and right-click context menus.

use super::*;

/// The model menu: the agent column beside the model's options.
const MODEL_MENU_W: f32 = 500.;
const AGENT_COLUMN_W: f32 = 170.;
/// Claude offers some twenty models; past this the list scrolls.
const MODEL_LIST_MAX_H: f32 = 280.;

impl Shell {
    pub(super) fn render_env_menu(
        &self,
        viewport_h: f32,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let project = self.project();
        let pi = self.active_project;
        let at = self.env_menu_at;
        let from = if at.is_some() {
            ui::motion::From::Below
        } else {
            ui::motion::From::Above
        };
        let menu =
            div()
                .id("env-menu")
                .absolute()
                .map(|d| match at {
                    Some(p) => d
                        .left(p.x)
                        .bottom(px(viewport_h) - p.y)
                        .max_h((p.y - px(12.)).max(px(120.))),
                    None => d
                        .top(px(gap() + 38.))
                        .right(px(gap() + 6.))
                        .max_h(px((viewport_h - gap() - 46.).max(120.))),
                })
                .w(px(280.))
                .overflow_y_scroll()
                .p(crate::ui::sp(4.))
                .flex()
                .flex_col()
                .gap(crate::ui::sp(1.))
                .bg(theme::dialog())
                .border_1()
                .border_color(theme::border())
                .rounded(crate::ui::rad(ui::MENU_RADIUS))
                .shadow_xl()
                .occlude()
                .child(
                    div()
                        .px(crate::ui::sp(8.))
                        .pt(crate::ui::sp(4.))
                        .pb(crate::ui::sp(6.))
                        .text_size(px(ui::TEXT_XS))
                        .text_color(theme::text_subtle())
                        .child(format!("Environments in {}", project.name)),
                )
                .children(project.environments.iter().enumerate().map(|(ei, env)| {
                    let active = ei == project.active_env;
                    div()
                        .id(("env-menu-item", ei))
                        .flex()
                        .items_center()
                        .gap(crate::ui::sp(8.))
                        .h(px(32.))
                        .px(crate::ui::sp(8.))
                        .rounded(ui::menu_row_radius())
                        .cursor_pointer()
                        .text_size(px(ui::TEXT_MD))
                        .when(active, |d| d.bg(theme::selection()))
                        .hover(|s| s.bg(theme::selection_hover()))
                        .child(
                            icon(
                                if env.is_checkout {
                                    "folder01"
                                } else {
                                    "git-branch"
                                },
                                14.,
                            )
                            .text_color(theme::text_subtle()),
                        )
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .truncate()
                                .child(env.branch.clone()),
                        )
                        .child(
                            div()
                                .text_size(px(ui::TEXT_XS))
                                .text_color(theme::text_faint())
                                .child(format!(
                                    "{} · {}",
                                    plural(
                                        self.strip
                                            .tabs
                                            .iter()
                                            .filter(|t| t.is_in(&env.path))
                                            .count(),
                                        "tab"
                                    ),
                                    plural(env.terminals.len(), "terminal")
                                )),
                        )
                        .child(status_dot(env.aggregate_status()))
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.select_env(pi, ei, window, cx)
                        }))
                }))
                .child(div().my(px(4.)).h(px(1.)).bg(theme::stroke()))
                .child(
                    div()
                        .id("env-new")
                        .flex()
                        .items_center()
                        .gap(crate::ui::sp(8.))
                        .h(px(32.))
                        .px(crate::ui::sp(8.))
                        .rounded(ui::menu_row_radius())
                        .cursor_pointer()
                        .text_size(px(ui::TEXT_MD))
                        .text_color(theme::text_subtle())
                        .hover(|s| s.bg(theme::selection_hover()).text_color(theme::content()))
                        .child(icon("add01", 14.).text_color(theme::text_subtle()))
                        .child("New environment (worktree)…")
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.open_create_env(pi, window, cx)
                        })),
                );
        // Placed by its bottom when it opens above the anchor, where a top
        // margin would not move it.
        menu.with_animation(
            "env-menu-in",
            gpui::Animation::new(ui::motion::FAST),
            move |d, t| {
                let (opacity, offset) = ui::motion::enter_at(t, from, 4.);
                let d = d.opacity(opacity);
                match from {
                    ui::motion::From::Below => d.mb(px(-offset)),
                    ui::motion::From::Above => d.mt(px(offset)),
                }
            },
        )
    }

    // ---- composer menus: agent, model and permissions -------------------------

    pub(super) fn open_composer_menu(
        &mut self,
        target: MenuTarget,
        at: gpui::Point<Pixels>,
        cx: &mut Context<Self>,
    ) {
        self.menu_agent = None;
        self.menu_scroll.scroll_to_item(0);
        self.menu_cursor = None;
        self.menu = Some(ContextMenu { target, at });
        cx.notify();
    }

    /// The permissions chip: Claude's and OpenCode's mode; Droid's autonomy, after
    /// "Spec" when it plans first. True when the agent may act without asking at all.
    pub(super) fn permissions_label(&self, cx: &App) -> Option<(String, bool)> {
        use crate::agent::Setting;
        let current = |s| self.setting_choices(s, cx).map(|(c, _)| c);
        match self.composer_agent() {
            model::Agent::Claude => {
                let label = self.setting_value_label(Setting::Mode, cx)?;
                Some((
                    label,
                    current(Setting::Mode).as_deref() == Some("bypassPermissions"),
                ))
            }
            // Build or Plan; what Build may do without asking is OpenCode's own config.
            model::Agent::OpenCode => self
                .setting_value_label(Setting::Mode, cx)
                .map(|l| (l, false)),
            // Codex's sandbox and approval preset; full access never asks.
            model::Agent::Codex => {
                let label = self.setting_value_label(Setting::Mode, cx)?;
                Some((
                    label,
                    current(Setting::Mode).as_deref() == Some("agent-full-access"),
                ))
            }
            // Devin's mode: Code, Smart, Ask, Plan or Bypass; bypass never asks.
            model::Agent::Devin => {
                let label = self.setting_value_label(Setting::Mode, cx)?;
                Some((label, current(Setting::Mode).as_deref() == Some("bypass")))
            }
            // It asks before commands and edits; `arakun agent new` sessions don't.
            model::Agent::WorkersAi => None,
            model::Agent::Droid => {
                let Some(autonomy) = self.setting_value_label(Setting::Autonomy, cx) else {
                    // Droid over ACP: its modes carry the autonomy.
                    return self
                        .setting_value_label(Setting::Mode, cx)
                        .map(|l| (l, false));
                };
                let level = current(Setting::Autonomy).unwrap_or_default();
                let auto = if level == "off" {
                    autonomy
                } else {
                    format!("Auto {autonomy}")
                };
                let label = if current(Setting::Mode).as_deref() == Some("spec") {
                    format!("Spec · {auto}")
                } else {
                    auto
                };
                Some((label, level == "high"))
            }
        }
    }

    // ---- what the composer and rail menus can do, for clicks and keys ---------

    /// The agent whose models the model menu shows.
    fn menu_agent(&self) -> model::Agent {
        self.menu_agent.unwrap_or_else(|| self.composer_agent())
    }

    /// The agents the model menu offers: all of them, unless the session can't
    /// be handed over.
    fn menu_agents(&self) -> Vec<model::Agent> {
        if self.can_switch_agent() {
            model::Agent::ALL.to_vec()
        } else {
            vec![self.composer_agent()]
        }
    }

    /// The rows the keyboard walks in the open menu, top to bottom. In the
    /// model menu these are the hovered agent's; ← and → change the agent.
    pub(super) fn menu_keys(&self, cx: &App) -> Vec<MenuKey> {
        use crate::agent::{AgentKind, Setting};
        let Some(menu) = &self.menu else {
            return Vec::new();
        };
        let choices = |agent: model::Agent, s: Setting| -> Vec<MenuKey> {
            self.agent_choices(agent, s, cx)
                .map(|(_, options)| {
                    options
                        .into_iter()
                        .map(|o| MenuKey::Choice(agent, s, o.id.clone(), o.disabled.is_none()))
                        .collect()
                })
                .unwrap_or_default()
        };
        match menu.target {
            MenuTarget::ModelMenu => {
                let agent = self.menu_agent();
                let mut keys = choices(agent, Setting::Model);
                if self.agent_choices(agent, Setting::Effort, cx).is_some() {
                    keys.push(MenuKey::Effort(agent));
                }
                if let Some((current, _)) = self.agent_choices(agent, Setting::Fast, cx) {
                    keys.push(MenuKey::Fast(agent, current == "on"));
                }
                keys.push(MenuKey::Reset(agent));
                // Enter can move to the agent even before it has models.
                if agent != self.composer_agent() {
                    keys.push(MenuKey::Agent(agent));
                }
                keys
            }
            MenuTarget::PermissionMenu => {
                let agent = self.composer_agent();
                self.permission_sections(cx)
                    .into_iter()
                    .flat_map(|s| choices(agent, s))
                    .collect()
            }
            MenuTarget::RailUsage => AgentKind::ALL
                .into_iter()
                .map(MenuKey::Rail)
                .chain([MenuKey::OpenUsage])
                .collect(),
            _ => Vec::new(),
        }
    }

    /// Does what a row of a composer or rail menu does.
    pub(super) fn menu_run(&mut self, key: MenuKey, window: &mut Window, cx: &mut Context<Self>) {
        use crate::agent::Setting;
        match key {
            MenuKey::Choice(agent, s, id, enabled) => {
                if enabled {
                    if closes_on_pick(s, agent, self.composer_agent()) {
                        self.menu = None;
                    }
                    self.pick_setting(agent, s, &id, window, cx);
                }
            }
            MenuKey::Effort(agent) => self.step_effort(agent, true, window, cx),
            MenuKey::Fast(agent, on) => {
                self.pick_setting(
                    agent,
                    Setting::Fast,
                    if on { "off" } else { "on" },
                    window,
                    cx,
                );
            }
            MenuKey::Reset(agent) => {
                self.menu = None;
                self.reset_settings(agent, cx);
            }
            MenuKey::Agent(agent) => {
                self.menu = None;
                self.switch_agent(agent, window, cx);
            }
            MenuKey::Rail(agent) => {
                account_view::set_rail_shows(agent, !account_view::rail_shows(agent));
                self.save(cx);
            }
            MenuKey::OpenUsage => {
                self.menu = None;
                self.open_usage_tab(window, cx);
            }
        }
        cx.notify();
    }

    /// The next (or previous) reasoning level of `agent`, without wrapping.
    fn step_effort(
        &mut self,
        agent: model::Agent,
        up: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        use crate::agent::Setting;
        let Some((current, options)) = self.agent_choices(agent, Setting::Effort, cx) else {
            return;
        };
        let usable: Vec<&str> = options
            .iter()
            .filter(|o| o.disabled.is_none())
            .map(|o| o.id.as_str())
            .collect();
        let at = usable.iter().position(|id| *id == current);
        let next = match (at, up) {
            (None, _) => usable.first(),
            (Some(i), true) => usable.get(i + 1),
            (Some(i), false) => i.checked_sub(1).and_then(|i| usable.get(i)),
        };
        if let Some(id) = next.map(|id| id.to_string()) {
            self.pick_setting(agent, Setting::Effort, &id, window, cx);
        }
    }

    /// Arrows, Enter and Space in a composer or rail menu. True when the key
    /// was the menu's.
    pub(super) fn menu_key(
        &mut self,
        key: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let keys = self.menu_keys(cx);
        if keys.is_empty() {
            return false;
        }
        let model_menu = matches!(
            self.menu.as_ref().map(|m| m.target),
            Some(MenuTarget::ModelMenu)
        );
        let usable = |k: &MenuKey| !matches!(k, MenuKey::Choice(_, _, _, false));
        let step = |from: Option<usize>, down: bool| -> Option<usize> {
            let n = keys.len();
            let mut i = from.map_or(if down { n - 1 } else { 0 }, |i| i.min(n - 1));
            for _ in 0..n {
                i = if down { (i + 1) % n } else { (i + n - 1) % n };
                if usable(&keys[i]) {
                    return Some(i);
                }
            }
            None
        };
        match key {
            "down" | "up" => {
                // The first press lands on the current choice, if there is one.
                self.menu_cursor = match self.menu_cursor {
                    Some(i) => step(Some(i), key == "down"),
                    None => keys
                        .iter()
                        .position(|k| self.menu_key_is_current(k, cx))
                        .or_else(|| step(None, key == "down")),
                };
            }
            "home" => self.menu_cursor = step(None, true),
            "end" => self.menu_cursor = step(None, false),
            "left" | "right" if model_menu => {
                let right = key == "right";
                match self.menu_cursor.and_then(|i| keys.get(i)) {
                    Some(MenuKey::Effort(agent)) => {
                        self.step_effort(*agent, right, window, cx);
                    }
                    // Anywhere else they page through the agents.
                    _ => {
                        let agents = self.menu_agents();
                        let n = agents.len();
                        let at = agents
                            .iter()
                            .position(|a| *a == self.menu_agent())
                            .unwrap_or(0);
                        self.menu_agent = Some(
                            agents[if right {
                                (at + 1) % n
                            } else {
                                (at + n - 1) % n
                            }],
                        );
                        self.menu_cursor = None;
                        self.menu_scroll.scroll_to_item(0);
                    }
                }
            }
            "right" => return true,
            "enter" | "space" => {
                if let Some(k) = self.menu_cursor.and_then(|i| keys.get(i)).cloned() {
                    self.menu_run(k, window, cx);
                }
            }
            _ => return false,
        }
        if model_menu
            && let Some(i) = self.menu_cursor
            && matches!(keys.get(i), Some(MenuKey::Choice(..)))
        {
            self.menu_scroll.scroll_to_item(i);
        }
        cx.notify();
        true
    }

    fn menu_key_is_current(&self, k: &MenuKey, cx: &App) -> bool {
        match k {
            MenuKey::Choice(agent, s, id, _) => self
                .agent_choices(*agent, *s, cx)
                .is_some_and(|(c, _)| c == *id),
            _ => false,
        }
    }

    /// Whether the keyboard is on row `i` of the menu, in `menu_keys` order.
    fn menu_focus(&self, i: usize) -> bool {
        self.menu_cursor == Some(i)
    }

    fn permission_sections(&self, cx: &App) -> Vec<crate::agent::Setting> {
        use crate::agent::Setting;
        match self.composer_agent() {
            model::Agent::Claude
            | model::Agent::OpenCode
            | model::Agent::Codex
            | model::Agent::Devin => vec![Setting::Mode],
            model::Agent::WorkersAi => Vec::new(),
            model::Agent::Droid => [Setting::Mode, Setting::Autonomy]
                .into_iter()
                .filter(|s| self.setting_choices(*s, cx).is_some())
                .collect(),
        }
    }

    /// Which providers the rail's usage card shows; the Usage tab keeps them all.
    pub(super) fn render_rail_usage_menu(&self, cx: &mut Context<Self>) -> impl IntoElement {
        use crate::agent::AgentKind;
        ui::menu_panel("rail-usage-menu", 220.)
            .child(ui::menu_heading("Show in sidebar"))
            .children(AgentKind::ALL.into_iter().enumerate().map(|(i, agent)| {
                ui::switch_row(
                    ("rail-usage-opt", i),
                    agent.label(),
                    account_view::rail_shows(agent),
                )
                .when(self.menu_focus(i), |d| d.bg(theme::fill_hover()))
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.menu_run(MenuKey::Rail(agent), window, cx)
                }))
            }))
            .child(ui::menu_divider())
            .child(
                ui::menu_item(
                    "rail-usage-open",
                    "dashboard-square01",
                    "Open Usage",
                    false,
                    true,
                )
                .when(self.menu_focus(AgentKind::ALL.len()), |d| {
                    d.bg(theme::fill_hover())
                })
                .on_click(
                    cx.listener(|this, _, window, cx| {
                        this.menu_run(MenuKey::OpenUsage, window, cx)
                    }),
                ),
            )
    }

    /// Agents on the left; on the right the hovered agent's models, reasoning
    /// and fast mode. Picking another agent's model moves the composer to it.
    pub(super) fn render_model_menu(&self, cx: &mut Context<Self>) -> impl IntoElement {
        use crate::agent::Setting;
        let current = self.composer_agent();
        let shown = self.menu_agent();
        let agents = self.menu_agents();
        let switchable = agents.len() > 1;
        let in_session = self.composer_in_session();
        let models = self.choice_rows(shown, Setting::Model, false, 0, cx);
        let n_models = models.len();
        let effort = self.agent_choices(shown, Setting::Effort, cx);
        let fast_ix = n_models + usize::from(effort.is_some());
        let fast = self
            .agent_choices(shown, Setting::Fast, cx)
            .map(|(c, _)| c == "on");
        let reset_ix = fast_ix + usize::from(fast.is_some());
        let switch_label = if in_session {
            format!("Continue with {}", shown.label())
        } else {
            format!("Use {}", shown.label())
        };

        let agent_column = div()
            .flex_none()
            .w(px(AGENT_COLUMN_W))
            .flex()
            .flex_col()
            .gap(crate::ui::sp(1.))
            .child(ui::menu_heading(if in_session {
                "Continue with"
            } else {
                "Agent"
            }))
            .children(agents.into_iter().map(|agent| {
                let model = self.agent_value_label(agent, Setting::Model, cx);
                div()
                    .id(("model-agent", agent as usize))
                    .flex()
                    .items_center()
                    .gap(crate::ui::sp(8.))
                    .min_h(px(ui::MENU_ROW_H))
                    .py(crate::ui::sp(4.))
                    .px(crate::ui::sp(8.))
                    .rounded(ui::menu_row_radius())
                    .cursor_pointer()
                    .text_size(px(ui::TEXT_MD))
                    .text_color(theme::text())
                    .when(agent == shown, |d| d.bg(theme::fill_hover()))
                    .when(agent == current, |d| d.bg(theme::fill_selected()))
                    .child(agent_icon(agent, 14.))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .child(div().truncate().child(agent.label()))
                            .when_some(model, |d, m| {
                                d.child(
                                    div()
                                        .truncate()
                                        .text_size(px(ui::TEXT_XS))
                                        .text_color(theme::text_subtle())
                                        .child(m),
                                )
                            }),
                    )
                    .when(agent == current, |d| {
                        d.child(
                            icon("tick02", 12.)
                                .flex_none()
                                .text_color(theme::text_subtle()),
                        )
                    })
                    .on_hover(cx.listener(move |this, hovered: &bool, _, cx| {
                        if *hovered && this.menu_agent() != agent {
                            this.menu_agent = Some(agent);
                            this.menu_cursor = None;
                            cx.notify();
                        }
                    }))
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.menu_run(MenuKey::Agent(agent), window, cx)
                    }))
            }));

        let empty = n_models == 0;
        let options = div()
            .flex_1()
            .min_w_0()
            .flex()
            .flex_col()
            .gap(crate::ui::sp(1.))
            .child(ui::menu_heading(if shown == current {
                "Model"
            } else {
                "Model · switches agent"
            }))
            .when(empty, |d| {
                d.child(
                    div()
                        .px(crate::ui::sp(8.))
                        .py(crate::ui::sp(6.))
                        .text_size(px(ui::TEXT_SM))
                        .text_color(theme::text_subtle())
                        .child(format!(
                            "{}'s models show here after its first session.",
                            shown.label()
                        )),
                )
            })
            .when(!empty, |d| {
                d.child(
                    div()
                        .id("model-list")
                        .flex()
                        .flex_col()
                        .gap(crate::ui::sp(1.))
                        .max_h(px(MODEL_LIST_MAX_H))
                        .min_h(px(0.))
                        .overflow_y_scroll()
                        .track_scroll(&self.menu_scroll)
                        .children(models),
                )
            })
            .when_some(effort, |d, (now, levels)| {
                d.child(ui::menu_heading("Reasoning")).child(
                    div().px(crate::ui::sp(4.)).pb(crate::ui::sp(2.)).child(
                        ui::segmented()
                            .w_full()
                            .when(self.menu_focus(n_models), |d| {
                                d.border_color(theme::border_strong())
                            })
                            .children(levels.into_iter().enumerate().map(|(i, o)| {
                                let enabled = o.disabled.is_none();
                                let key =
                                    MenuKey::Choice(shown, Setting::Effort, o.id.clone(), enabled);
                                ui::segment(
                                    ("model-effort", i),
                                    None,
                                    effort_short(&o.id, &o.name),
                                    o.id == now,
                                )
                                .flex_1()
                                .min_w_0()
                                .justify_center()
                                .px(crate::ui::sp(4.))
                                .when_some(effort_tip(&o.id), |d, tip| d.tooltip(ui::tip(tip)))
                                .when(!enabled, |d| d.opacity(0.45).cursor_default())
                                .when(enabled, |d| {
                                    d.on_click(cx.listener(move |this, _, window, cx| {
                                        this.menu_run(key.clone(), window, cx)
                                    }))
                                })
                            })),
                    ),
                )
            })
            .when_some(fast, |d, on| {
                d.child(
                    ui::switch_row("model-fast", Setting::Fast.label(), on)
                        .when(self.menu_focus(fast_ix), |d| d.bg(theme::fill_hover()))
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.menu_run(MenuKey::Fast(shown, on), window, cx)
                        })),
                )
            })
            .child(ui::menu_divider())
            .child(
                ui::menu_item("model-reset", "undo", "Reset to default", false, true)
                    .when(self.menu_focus(reset_ix), |d| d.bg(theme::fill_hover()))
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.menu_run(MenuKey::Reset(shown), window, cx)
                    })),
            )
            .when(shown != current, |d| {
                d.child(
                    ui::menu_item("model-switch", "arrow-right01", switch_label, false, true)
                        .when(self.menu_focus(reset_ix + 1), |d| d.bg(theme::fill_hover()))
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.menu_run(MenuKey::Agent(shown), window, cx)
                        })),
                )
            });

        // The menu opens upward, so a height that followed the hovered agent's
        // options would move the agents under the cursor and hover another.
        // With more than one agent the agent column sets the height, and the
        // options fit in it, the model list scrolling.
        let options = if switchable {
            div()
                .relative()
                .flex_1()
                .min_w_0()
                .self_stretch()
                .child(options.absolute().top_0().left_0().right_0().bottom_0())
        } else {
            options
        };

        ui::menu_panel("model-menu", MODEL_MENU_W)
            .flex_row()
            .items_start()
            .gap(crate::ui::sp(4.))
            .child(agent_column)
            .child(
                div()
                    .flex_none()
                    .w(px(1.))
                    .self_stretch()
                    .my(crate::ui::sp(4.))
                    .bg(theme::border_subtle()),
            )
            .child(options)
    }

    /// Who decides what the agent may do: Claude's modes, or Droid's mode and
    /// autonomy, each choice with what it means.
    pub(super) fn render_permission_menu(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let sections = self.permission_sections(cx);
        let agent = self.composer_agent();
        let last = sections.len().saturating_sub(1);
        let mut offset = 0;
        let mut panel = ui::menu_panel("permission-menu", 320.);
        for (i, s) in sections.into_iter().enumerate() {
            let rows = self.choice_rows(agent, s, true, offset, cx);
            offset += rows.len();
            panel = panel.child(
                div()
                    .flex()
                    .flex_col()
                    .gap(crate::ui::sp(1.))
                    .child(ui::menu_heading(s.label()))
                    .children(rows)
                    .when(i < last, |d| d.child(ui::menu_divider())),
            );
        }
        panel
    }

    /// `agent`'s choices of a setting as radio rows; `offset` is where they
    /// start in the keyboard's order.
    fn choice_rows(
        &self,
        agent: model::Agent,
        s: crate::agent::Setting,
        describe: bool,
        offset: usize,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        use crate::agent::Setting;
        let (current, options) = self.agent_choices(agent, s, cx).unwrap_or_default();
        options
            .into_iter()
            .enumerate()
            .map(|(i, o)| {
                let enabled = o.disabled.is_none();
                let key = MenuKey::Choice(agent, s, o.id.clone(), enabled);
                // Claude's "Default (recommended)" model says which model it is.
                let note = o.disabled.clone().or(o.detail.clone()).or_else(|| {
                    (s == Setting::Model && o.id == "default")
                        .then(|| o.description.clone())
                        .flatten()
                });
                let description = o.description.filter(|_| describe);
                ui::choice_row(("choice", s as usize * 1000 + i), o.id == current)
                    .when(self.menu_focus(offset + i), |d| d.bg(theme::fill_hover()))
                    .when(!enabled, |d| d.opacity(0.45).cursor_default())
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .child(div().truncate().child(o.name))
                            .when_some(description, |d, text| {
                                d.child(
                                    div()
                                        .text_size(px(ui::TEXT_XS))
                                        .text_color(theme::text_subtle())
                                        .child(text),
                                )
                            }),
                    )
                    .when_some(note, |d, note| {
                        d.child(
                            div()
                                .flex_none()
                                .max_w(px(130.))
                                .truncate()
                                .text_size(px(ui::TEXT_XS))
                                .text_color(theme::text_subtle())
                                .child(note),
                        )
                    })
                    .when(enabled, |d| {
                        d.on_click(cx.listener(move |this, _, window, cx| {
                            this.menu_run(key.clone(), window, cx)
                        }))
                    })
                    .into_any_element()
            })
            .collect()
    }

    pub(super) fn render_context_menu(
        &self,
        menu: &ContextMenu,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let mut panel = ui::menu_panel("context-menu", 220.)
            .min_h_0()
            .overflow_y_scroll();
        match menu.target {
            MenuTarget::Inbox => return self.render_inbox(cx).into_any_element(),
            MenuTarget::Project(pi) => {
                let path = self
                    .projects
                    .get(pi)
                    .map(|p| p.path.clone())
                    .unwrap_or_default();
                let reveal = path.clone();
                // The main checkout is not in the list while no session is there.
                let checkout = self
                    .projects
                    .get(pi)
                    .and_then(|p| p.environments.iter().position(|e| e.is_checkout));
                panel = panel
                    .child(
                        ui::menu_item("cm-new-env", "git-branch", "New environment…", false, true)
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.open_create_env(pi, window, cx)
                            })),
                    )
                    .when_some(checkout, |d, ei| {
                        d.child(
                            ui::menu_item(
                                "cm-checkout",
                                "folder01",
                                "Open main checkout",
                                false,
                                true,
                            )
                            .on_click(cx.listener(
                                move |this, _, window, cx| {
                                    this.menu = None;
                                    this.select_env(pi, ei, window, cx)
                                },
                            )),
                        )
                    })
                    .child(
                        ui::menu_item(
                            "cm-reveal",
                            "folder-open",
                            "Open in file manager",
                            false,
                            true,
                        )
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.menu = None;
                            open_external(&reveal);
                            cx.notify();
                        })),
                    )
                    .child(
                        ui::menu_item("cm-copy", "copy01", "Copy path", false, true).on_click(
                            cx.listener(move |this, _, _, cx| {
                                this.menu = None;
                                cx.write_to_clipboard(gpui::ClipboardItem::new_string(
                                    path.display().to_string(),
                                ));
                                cx.notify();
                            }),
                        ),
                    )
                    .child(
                        ui::menu_item(
                            "cm-setup",
                            "computer-terminal01",
                            "Setup script…",
                            false,
                            true,
                        )
                        .on_click(cx.listener(
                            move |this, _, window, cx| {
                                this.open_project_command(pi, false, None, window, cx)
                            },
                        )),
                    )
                    .child(
                        ui::menu_item(
                            "cm-check",
                            "checkmark-circle02",
                            "Check command…",
                            false,
                            true,
                        )
                        .on_click(cx.listener(
                            move |this, _, window, cx| {
                                this.open_project_command(pi, true, None, window, cx)
                            },
                        )),
                    )
                    .child(div().my(px(4.)).h(px(1.)).bg(theme::stroke()))
                    .child(
                        ui::menu_item(
                            "cm-remove",
                            "cancel-circle",
                            "Remove from Arakun",
                            true,
                            true,
                        )
                        .on_click(cx.listener(
                            move |this, _, window, cx| this.remove_project(pi, window, cx),
                        )),
                    );
            }
            MenuTarget::Automations => {
                if self.has_projects() {
                    let count = self.project().automations.len();
                    for ai in 0..count {
                        let automation = self.project().automations[ai].clone();
                        let id: SharedString = format!("cm-auto-{ai}").into();
                        let when = match automation.next_run(model::unix_now()) {
                            Some(next) => crate::schedule::when(next, model::unix_now()),
                            None => automation.schedule.describe(),
                        };
                        panel = panel.child(
                            ui::menu_item(
                                id,
                                if automation.agent.is_some() {
                                    "play"
                                } else {
                                    "flash"
                                },
                                format!("{}  ·  {when}", automation.name),
                                false,
                                true,
                            )
                            .on_click(cx.listener(
                                move |this, _, window, cx| {
                                    this.menu = None;
                                    this.run_automation(this.active_project, ai, true, window, cx);
                                },
                            )),
                        );
                    }
                    if count > 0 {
                        panel = panel.child(div().my(px(4.)).h(px(1.)).bg(theme::stroke()));
                    }
                    panel = panel
                        .child(
                            ui::menu_item("cm-auto-new", "add01", "New automation…", false, true)
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.open_automations(true, window, cx)
                                })),
                        )
                        .child(
                            ui::menu_item(
                                "cm-auto-manage",
                                "clock01",
                                "Manage automations…",
                                false,
                                true,
                            )
                            .on_click(cx.listener(
                                |this, _, window, cx| this.open_automations(false, window, cx),
                            )),
                        );
                }
            }
            MenuTarget::ModelMenu => return self.render_model_menu(cx).into_any_element(),
            MenuTarget::PermissionMenu => {
                return self.render_permission_menu(cx).into_any_element();
            }
            MenuTarget::RailUsage => return self.render_rail_usage_menu(cx).into_any_element(),
            MenuTarget::Session(i) => return self.render_session_menu(i, cx),
            MenuTarget::Tab(i) => return self.render_tab_menu(i, cx),
            #[cfg(feature = "coderlabs")]
            MenuTarget::TaskProject => return self.render_task_project_menu(cx),
            MenuTarget::RailBlock(i, j) => return self.render_rail_block_menu(i, j, cx),
            MenuTarget::RailSection(i) => return self.render_rail_section_menu(i, cx),
            MenuTarget::ProjectSwitch => return self.render_project_switch_menu(cx),
            MenuTarget::Change(i) => {
                let change = self
                    .has_projects()
                    .then(|| self.env().changes.get(i).cloned())
                    .flatten();
                let Some(change) = change else {
                    return panel.into_any_element();
                };
                let (open, toggle, discard) = (change.clone(), change.clone(), change.clone());
                let staged = change.fully_staged();
                panel = panel
                    .child(
                        ui::menu_item("cm-open-diff", "file-diff", "Open diff", false, true)
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.menu = None;
                                this.open_doc_tab(
                                    TabKind::Diff {
                                        path: open.path.clone(),
                                        untracked: open.kind == git::ChangeKind::Untracked,
                                    },
                                    window,
                                    cx,
                                )
                            })),
                    )
                    .child(
                        ui::menu_item(
                            "cm-toggle-stage",
                            if staged { "minus-sign" } else { "add01" },
                            if staged { "Unstage" } else { "Stage" },
                            false,
                            true,
                        )
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.menu = None;
                            this.toggle_stage(toggle.clone(), cx);
                        })),
                    )
                    .child(div().my(px(4.)).h(px(1.)).bg(theme::stroke()))
                    .child(
                        ui::menu_item("cm-discard", "undo", "Discard changes…", true, true)
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.confirm_discard(discard.clone(), window, cx)
                            })),
                    );
            }
            // A server's folder: nothing here acts on it but the Servers tab.
            MenuTarget::Env(pi, ei)
                if self
                    .projects
                    .get(pi)
                    .and_then(|p| p.environments.get(ei))
                    .is_some_and(|e| e.remote.is_some()) =>
            {
                let server = self.projects[pi].environments[ei]
                    .remote
                    .as_ref()
                    .map(|r| r.server.clone())
                    .unwrap_or_default();
                let machine = self.projects[pi].environments[ei]
                    .remote
                    .as_ref()
                    .map(|r| r.machine.to_string())
                    .unwrap_or_default();
                panel = panel.child(
                    ui::menu_item(
                        "cm-server-new-env",
                        "add01",
                        format!("New environment on {machine}"),
                        false,
                        true,
                    )
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.menu = None;
                        this.servers_new_env(pi, ei, cx)
                    })),
                );
                panel =
                    panel
                        .child(
                            ui::menu_item(
                                "cm-server-add-place",
                                "folder01",
                                "Clone or add a folder on this server…",
                                false,
                                true,
                            )
                            .on_click(cx.listener(
                                move |this, _, window, cx| {
                                    this.menu = None;
                                    this.servers_add_place(&server, window, cx)
                                },
                            )),
                        )
                        .child(
                            ui::menu_item("cm-servers", "computer", "Open Servers", false, true)
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.open_servers_tab(window, cx)
                                })),
                        );
                let env = &self.projects[pi].environments[ei];
                let can_settle = env
                    .sessions
                    .iter()
                    .any(|s| matches!(s.status, Status::NeedsInput | Status::Error));
                let deletable = !env.is_checkout;
                let settle = env.path.clone();
                panel = panel
                    .child(
                        ui::menu_item(
                            "cm-server-settle",
                            "checkmark-circle02",
                            "Settle threads",
                            false,
                            can_settle,
                        )
                        .when(can_settle, |d| {
                            d.on_click(cx.listener(move |this, _, _, cx| {
                                this.menu = None;
                                this.servers_settle_env(&settle, cx);
                                cx.notify();
                            }))
                        }),
                    )
                    .child(div().my(px(4.)).h(px(1.)).bg(theme::stroke()))
                    .child(
                        ui::menu_item(
                            "cm-server-delete",
                            "delete02",
                            if deletable {
                                "Delete environment…"
                            } else {
                                "Main checkout can't be deleted"
                            },
                            true,
                            deletable,
                        )
                        .when(deletable, |d| {
                            d.on_click(cx.listener(move |this, _, window, cx| {
                                this.servers_delete_env(pi, ei, window, cx)
                            }))
                        }),
                    );
            }
            MenuTarget::Env(pi, ei) => {
                let env = self.projects.get(pi).and_then(|p| p.environments.get(ei));
                let path = env.map(|e| e.path.clone()).unwrap_or_default();
                let deletable = env.is_some_and(|e| !e.is_checkout);
                let reveal = path.clone();
                panel = panel
                    .child(
                        ui::menu_item(
                            "cm-reveal",
                            "folder-open",
                            "Open in file manager",
                            false,
                            true,
                        )
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.menu = None;
                            open_external(&reveal);
                            cx.notify();
                        })),
                    )
                    .child(
                        ui::menu_item("cm-copy", "copy01", "Copy path", false, true).on_click(
                            cx.listener(move |this, _, _, cx| {
                                this.menu = None;
                                cx.write_to_clipboard(gpui::ClipboardItem::new_string(
                                    path.display().to_string(),
                                ));
                                cx.notify();
                            }),
                        ),
                    )
                    .child(div().my(px(4.)).h(px(1.)).bg(theme::stroke()))
                    .child(
                        ui::menu_item(
                            "cm-delete",
                            "delete02",
                            if deletable {
                                "Delete environment…"
                            } else {
                                "Main checkout can't be deleted"
                            },
                            true,
                            deletable,
                        )
                        .when(deletable, |d| {
                            d.on_click(cx.listener(move |this, _, window, cx| {
                                this.open_delete_env(pi, ei, window, cx)
                            }))
                        }),
                    );
            }
        }
        panel.into_any_element()
    }
}

/// A reasoning level as short as a segment can hold.
fn effort_short(id: &str, name: &str) -> String {
    match id {
        "default" | "auto" => "Auto".into(),
        "none" | "off" => "Off".into(),
        "minimal" => "Min".into(),
        "low" => "Low".into(),
        "medium" => "Med".into(),
        "high" => "High".into(),
        "xhigh" => "XHigh".into(),
        "max" => "Max".into(),
        "ultra" => "Ultra".into(),
        _ => name.to_string(),
    }
}

/// What a reasoning level's segment says on hover.
fn effort_tip(id: &str) -> Option<&'static str> {
    Some(match id {
        "default" | "auto" => "The model's own default",
        "none" | "off" => "No reasoning",
        "minimal" => "Minimal reasoning",
        "low" => "Low reasoning",
        "medium" => "Medium reasoning",
        "high" => "High reasoning",
        "xhigh" => "Extra high reasoning",
        "max" => "Maximum reasoning",
        "ultra" => "Ultra reasoning",
        _ => return None,
    })
}

/// Whether picking `agent`'s choice of `s` closes the menu. Reasoning stays
/// open to compare levels, and so does a model of the agent in use: the
/// reasoning row sits below the models and may change with them, so it comes
/// next. Another agent's model switches the agent or hands the session over.
fn closes_on_pick(
    s: crate::agent::Setting,
    agent: model::Agent,
    composer_agent: model::Agent,
) -> bool {
    use crate::agent::Setting;
    match s {
        Setting::Effort => false,
        Setting::Model => agent != composer_agent,
        _ => true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::Setting;

    #[test]
    fn model_menu_stays_open_for_the_same_agent() {
        let (claude, codex) = (model::Agent::Claude, model::Agent::Codex);
        assert!(!closes_on_pick(Setting::Model, claude, claude));
        assert!(closes_on_pick(Setting::Model, codex, claude));
        assert!(!closes_on_pick(Setting::Effort, claude, claude));
        assert!(!closes_on_pick(Setting::Effort, codex, claude));
        assert!(closes_on_pick(Setting::Mode, claude, claude));
    }

    #[test]
    fn effort_levels_fit_a_segment() {
        for id in [
            "default", "none", "minimal", "low", "medium", "high", "xhigh", "max", "ultra",
        ] {
            assert!(effort_short(id, id).len() <= 5, "{id}");
            assert!(effort_tip(id).is_some(), "{id}");
        }
        assert_eq!(effort_short("turbo", "Turbo"), "Turbo");
        assert_eq!(effort_tip("turbo"), None);
    }
}
