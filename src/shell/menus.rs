//! Pop-up menus: the environment menu, setting menus and right-click context menus.

use super::*;

/// The model menu: one column, wide enough for seven agents in its strip.
const MODEL_MENU_W: f32 = 340.;
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
                        .child(div().flex_1().min_w_0().truncate().child(
                            if env.branch.is_empty() {
                                env.name.clone()
                            } else {
                                env.branch.clone()
                            },
                        ))
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
                        .child(status_dot(self.env_status(pi, ei, cx)))
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
                        .child("New environment…")
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
        self.effort_hover = None;
        // A server's environment: its models come from the server, which is
        // asked again each time, and the menu fills in as it answers.
        if matches!(target, MenuTarget::ModelMenu)
            && self.has_projects()
            && self.env().remote.is_some()
        {
            let session = match self.strip.tabs.get(self.composer_tab()).map(|t| &t.kind) {
                Some(TabKind::Session { id, .. }) => Some(id.to_string()),
                _ => None,
            };
            let path = self.env().path.clone();
            self.servers_ask_settings(&path, session.as_deref());
        }
        self.menu = Some(ContextMenu { target, at });
        cx.notify();
    }

    /// The permissions chip: Claude's and OpenCode's mode; Droid's autonomy, after
    /// "Spec" when it plans first. True when the agent may act without asking at all.
    pub(super) fn permissions_label(&self, cx: &App) -> Option<(String, bool)> {
        use crate::agent::Setting;
        let current = |s| self.setting_choices(s, cx).map(|(c, _)| c);
        match self.composer_agent() {
            AgentKind::Claude => {
                let label = self.setting_value_label(Setting::Mode, cx)?;
                Some((
                    label,
                    current(Setting::Mode).as_deref() == Some("bypassPermissions"),
                ))
            }
            // Build or Plan; what Build may do without asking is OpenCode's own config.
            AgentKind::OpenCode => self
                .setting_value_label(Setting::Mode, cx)
                .map(|l| (l, false)),
            // Codex's sandbox and approval preset; full access never asks.
            AgentKind::Codex => {
                let label = self.setting_value_label(Setting::Mode, cx)?;
                Some((
                    label,
                    current(Setting::Mode).as_deref() == Some("agent-full-access"),
                ))
            }
            // Devin's mode: Code, Smart, Ask, Plan or Bypass; bypass never asks.
            AgentKind::Devin => {
                let label = self.setting_value_label(Setting::Mode, cx)?;
                Some((label, current(Setting::Mode).as_deref() == Some("bypass")))
            }
            // Cursor's mode: Agent, Plan or Ask. None of them skips asking.
            AgentKind::Cursor => {
                let label = self.setting_value_label(Setting::Mode, cx)?;
                Some((label, false))
            }
            // Antigravity's modes come from agy; what each may do without
            // asking is agy's own setting.
            AgentKind::Antigravity => self
                .setting_value_label(Setting::Mode, cx)
                .map(|l| (l, false)),
            // It asks before commands and edits; `arakun agent new` sessions don't.
            AgentKind::WorkersAi => None,
            AgentKind::Droid => {
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
    fn menu_agent(&self) -> AgentKind {
        self.menu_agent.unwrap_or_else(|| self.composer_agent())
    }

    /// The agents the model menu offers: all of them, unless the session can't
    /// be handed over.
    fn menu_agents(&self) -> Vec<AgentKind> {
        if self.can_switch_agent() {
            AgentKind::ALL.to_vec()
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
        let choices = |agent: AgentKind, s: Setting| -> Vec<MenuKey> {
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
                if self.effort_levels(agent, cx).is_some() {
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
            MenuTarget::RailUsage => AgentKind::WITH_PLAN
                .into_iter()
                .map(MenuKey::Rail)
                .chain([MenuKey::OpenUsage])
                .collect(),
            MenuTarget::TreeSort => crate::tab_layout::Sort::ALL
                .into_iter()
                .map(MenuKey::TreeSort)
                .chain([MenuKey::TreeGroup, MenuKey::TreeArchived])
                .collect(),
            _ => Vec::new(),
        }
    }

    /// Does what a row of a composer or rail menu does.
    pub(super) fn menu_run(&mut self, key: MenuKey, window: &mut Window, cx: &mut Context<Self>) {
        let focused = self
            .menu_cursor
            .and_then(|i| self.menu_keys(cx).get(i).cloned());
        self.menu_run_key(key, window, cx);
        // A pick can add or drop rows (another model may have no reasoning
        // levels), so the cursor follows its action, not its old index.
        if let Some(focused) = focused {
            self.menu_cursor = self
                .menu_keys(cx)
                .iter()
                .position(|k| same_action(k, &focused));
        }
        cx.notify();
    }

    fn menu_run_key(&mut self, key: MenuKey, window: &mut Window, cx: &mut Context<Self>) {
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
            // The tree's menu stays open: the tree changes beside it, and
            // the next row is one click away.
            MenuKey::TreeSort(sort) => self.set_tree_sort(sort, cx),
            MenuKey::TreeGroup => {
                self.drop_pick();
                let layout = crate::tab_layout::current().with(|l| l.group = !l.group);
                self.apply_tab_layout(layout, None, cx);
            }
            MenuKey::TreeArchived => {
                self.drop_pick();
                self.show_settled = !self.show_settled;
            }
        }
    }

    /// `agent`'s reasoning levels, when there are two or more to pick from.
    fn effort_levels(
        &self,
        agent: AgentKind,
        cx: &App,
    ) -> Option<(String, Vec<crate::agent::SettingOption>)> {
        use crate::agent::Setting;
        self.agent_choices(agent, Setting::Effort, cx)
            .filter(|(_, options)| options.len() > 1)
    }

    /// The next (or previous) reasoning level of `agent`, without wrapping.
    fn step_effort(
        &mut self,
        agent: AgentKind,
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
            MenuKey::TreeSort(sort) => self.tree_menu_sort() == Some(*sort),
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
            AgentKind::Claude
            | AgentKind::OpenCode
            | AgentKind::Codex
            | AgentKind::Devin
            | AgentKind::Cursor
            | AgentKind::Antigravity => vec![Setting::Mode],
            AgentKind::WorkersAi => Vec::new(),
            AgentKind::Droid => [Setting::Mode, Setting::Autonomy]
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
            .children(
                AgentKind::WITH_PLAN
                    .into_iter()
                    .enumerate()
                    .map(|(i, agent)| {
                        ui::switch_row(
                            ("rail-usage-opt", i),
                            agent.label(),
                            account_view::rail_shows(agent),
                        )
                        .when(self.menu_focus(i), |d| d.bg(theme::fill_hover()))
                        .on_click(cx.listener(
                            move |this, _, window, cx| {
                                this.menu_run(MenuKey::Rail(agent), window, cx)
                            },
                        ))
                    }),
            )
            .child(ui::menu_divider())
            .child(
                ui::menu_item(
                    "rail-usage-open",
                    "dashboard-square01",
                    "Open Usage",
                    false,
                    true,
                )
                .when(self.menu_focus(AgentKind::WITH_PLAN.len()), |d| {
                    d.bg(theme::fill_hover())
                })
                .on_click(
                    cx.listener(|this, _, window, cx| {
                        this.menu_run(MenuKey::OpenUsage, window, cx)
                    }),
                ),
            )
    }

    /// The tree's menu: the order of its sessions, the two switches for
    /// what it lists, and which projects the order goes to. Settings has the
    /// order and the grouping too, and the end of the tree shows the archived
    /// sessions as well. On Select projects the list of projects opens beside it.
    pub(super) fn render_tree_sort_menu(&self, cx: &mut Context<Self>) -> impl IntoElement {
        use crate::tab_layout::Sort;
        let layout = crate::tab_layout::current();
        let orders = Sort::ALL.len();
        let shown = self.tree_menu_sort();
        let select = self.tree_sort_select && layout.group;
        let picked = self.tree_sort_picked().len();
        let menu = ui::menu_panel("tree-sort-menu", 240.)
            .child(ui::menu_heading("Order sessions by"))
            .children(Sort::ALL.into_iter().enumerate().map(|(i, sort)| {
                ui::choice_row(("tree-sort-order", i), sort.label(), shown == Some(sort))
                    .child(div().flex_1().child(sort.label()))
                    .child(
                        div()
                            .flex_none()
                            .text_size(px(ui::TEXT_XS))
                            .text_color(theme::text_subtle())
                            .child(sort.hint()),
                    )
                    .when(self.menu_focus(i), |d| d.bg(theme::fill_hover()))
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.menu_run(MenuKey::TreeSort(sort), window, cx)
                    }))
            }))
            .child(ui::menu_divider())
            .child(
                ui::switch_row("tree-sort-group", "Group by project", layout.group)
                    .when(self.menu_focus(orders), |d| d.bg(theme::fill_hover()))
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.menu_run(MenuKey::TreeGroup, window, cx)
                    })),
            )
            .child(
                ui::switch_row("tree-sort-archived", "Show archived", self.show_settled)
                    .when(self.menu_focus(orders + 1), |d| d.bg(theme::fill_hover()))
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.menu_run(MenuKey::TreeArchived, window, cx)
                    })),
            )
            // Without project headings there is one list, so one order.
            .when(layout.group, |d| {
                let label = if select && picked > 0 {
                    format!("{picked} project{}", if picked == 1 { "" } else { "s" })
                } else {
                    "Select projects…".to_string()
                };
                d.child(ui::menu_divider())
                    .child(ui::menu_heading("Applies to"))
                    .child(
                        div().px(crate::ui::sp(4.)).pb(crate::ui::sp(4.)).child(
                            ui::segmented()
                                .w_full()
                                .child(
                                    ui::segment("tree-sort-all", None, "All projects", !select)
                                        .flex_1()
                                        .justify_center()
                                        .on_click(cx.listener(|this, _, _, cx| {
                                            this.set_tree_sort_scope(false, cx)
                                        })),
                                )
                                .child(
                                    ui::segment("tree-sort-select", None, label, select)
                                        .flex_1()
                                        .justify_center()
                                        .on_click(cx.listener(|this, _, _, cx| {
                                            this.set_tree_sort_scope(true, cx)
                                        })),
                                ),
                        ),
                    )
            });
        div()
            .flex()
            .items_start()
            .gap(crate::ui::sp(4.))
            .child(menu)
            .when(select, |d| d.child(self.render_tree_sort_projects(cx)))
    }

    /// The projects Applies to › Select projects… can give an order of
    /// their own, each with a tick.
    fn render_tree_sort_projects(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let all = crate::tab_layout::current().sort;
        let picked = self.tree_sort_picked();
        let note = match (picked.len(), self.tree_menu_sort()) {
            (0, _) => "Tick the projects that get the order picked.".to_string(),
            (n, Some(sort)) => format!(
                "{n} project{} use{} {}. The rest stay on {}.",
                if n == 1 { "" } else { "s" },
                if n == 1 { "s" } else { "" },
                sort.label(),
                all.label()
            ),
            (n, None) => format!(
                "{n} projects have orders of their own. The rest stay on {}.",
                all.label()
            ),
        };
        ui::menu_panel("tree-sort-projects", 230.)
            .child(ui::menu_heading("Select projects"))
            .children(self.projects.iter().enumerate().map(|(pi, project)| {
                let on = project.sort.is_some();
                let logo: Hsla = gpui::rgb(project.logo).into();
                div()
                    .id(("tree-sort-project", pi))
                    .role(gpui::Role::MenuItemCheckBox)
                    .aria_label(project.name.clone())
                    .aria_toggled(ui::toggled(on))
                    .flex()
                    .items_center()
                    .gap(crate::ui::sp(10.))
                    .h(px(ui::MENU_ROW_H))
                    .px(crate::ui::sp(8.))
                    .rounded(crate::ui::rad(ui::RADIUS))
                    .text_size(px(ui::TEXT_MD))
                    .text_color(theme::text())
                    .cursor_pointer()
                    .map(ui::focusable)
                    .hover(|d| d.bg(theme::fill_hover()))
                    .child(ui::check_box(on))
                    .child(
                        div()
                            .flex()
                            .flex_none()
                            .items_center()
                            .justify_center()
                            .size(px(14.))
                            .child(ui::project_icon(project.icon, 1.25, logo)),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .child(project.name.clone()),
                    )
                    .on_click(
                        cx.listener(move |this, _, _, cx| this.toggle_tree_sort_project(pi, cx)),
                    )
            }))
            .child(ui::menu_divider())
            .child(
                div()
                    .px(crate::ui::sp(8.))
                    .py(crate::ui::sp(4.))
                    .text_size(px(ui::TEXT_XS))
                    .text_color(theme::text_subtle())
                    .child(note),
            )
    }

    /// One column: the shown agent's models, its reasoning level and fast
    /// mode, and the agents in a strip at the bottom. The menu opens upward,
    /// so the strip sits by the chip and stays put while the height follows
    /// the agent. Picking another agent's model moves the composer to it.
    pub(super) fn render_model_menu(&self, cx: &mut Context<Self>) -> impl IntoElement {
        use crate::agent::Setting;
        let current = self.composer_agent();
        let shown = self.menu_agent();
        let agents = self.menu_agents();
        let in_session = self.composer_in_session();
        let models = self.choice_rows(shown, Setting::Model, false, 0, cx);
        let n_models = models.len();
        let effort = self.effort_levels(shown, cx);
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

        let heading = div()
            .flex()
            .items_center()
            .justify_between()
            .pr(crate::ui::sp(8.))
            .child(ui::menu_heading(&format!("{} models", shown.label())))
            .when(shown != current, |d| {
                d.child(
                    div()
                        .pt(crate::ui::sp(3.))
                        .text_size(px(ui::TEXT_XS))
                        .text_color(theme::text_faint())
                        .child("Picking one switches agent"),
                )
            });

        let models = if n_models == 0 {
            div()
                .px(crate::ui::sp(8.))
                .py(crate::ui::sp(6.))
                .text_size(px(ui::TEXT_SM))
                .text_color(theme::text_subtle())
                .child(format!(
                    "{}'s models show here after its first session.",
                    shown.label()
                ))
                .into_any_element()
        } else {
            div()
                .id("model-list")
                .flex()
                .flex_col()
                .gap(crate::ui::sp(1.))
                .max_h(px(MODEL_LIST_MAX_H))
                .min_h(px(0.))
                .overflow_y_scroll()
                .track_scroll(&self.menu_scroll)
                .children(models)
                .into_any_element()
        };

        let effort_row = effort.map(|(now, options)| {
            self.render_effort_row(shown, &now, options, self.menu_focus(n_models), cx)
        });

        // Fast mode and reset share the last row: both are small and rarely used.
        let settings_row = div()
            .flex()
            .items_center()
            .gap(crate::ui::sp(4.))
            .when_some(fast, |d, on| {
                d.child(
                    div()
                        .id("model-fast")
                        .role(gpui::Role::Switch)
                        .aria_label(Setting::Fast.label())
                        .aria_toggled(crate::ui::toggled(on))
                        .flex()
                        .items_center()
                        .gap(crate::ui::sp(8.))
                        .h(px(ui::MENU_ROW_H))
                        .px(crate::ui::sp(8.))
                        .rounded(ui::menu_row_radius())
                        .cursor_pointer()
                        .map(crate::ui::focusable)
                        .text_size(px(ui::TEXT_MD))
                        .text_color(theme::text())
                        .hover(|d| d.bg(theme::fill_hover()))
                        .when(self.menu_focus(fast_ix), |d| d.bg(theme::fill_hover()))
                        .child(icon("flash", 13.).text_color(if on {
                            theme::text()
                        } else {
                            theme::text_subtle()
                        }))
                        .child(Setting::Fast.label())
                        .child(ui::switch(on))
                        .tooltip(ui::tip("Faster output, at a higher price"))
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.menu_run(MenuKey::Fast(shown, on), window, cx)
                        })),
                )
            })
            .child(div().flex_1())
            .child(
                ui::menu_item("model-reset", "undo", "Reset", false, true)
                    .text_size(px(ui::TEXT_SM))
                    .when(self.menu_focus(reset_ix), |d| d.bg(theme::fill_hover()))
                    .tooltip(ui::tip("Back to the default model, reasoning and speed"))
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.menu_run(MenuKey::Reset(shown), window, cx)
                    })),
            );

        let agent_strip = (agents.len() > 1).then(|| {
            div()
                .flex()
                .items_center()
                .gap(crate::ui::sp(2.))
                .px(crate::ui::sp(2.))
                .children(agents.into_iter().map(|agent| {
                    let tip = match self.agent_value_label(agent, Setting::Model, cx) {
                        Some(m) => format!("{} · {m}", agent.label()),
                        None => agent.label().to_string(),
                    };
                    div()
                        .id(("model-agent", agent as usize))
                        .role(gpui::Role::Tab)
                        .aria_label(agent.label())
                        .aria_selected(agent == shown)
                        .relative()
                        .flex()
                        .flex_1()
                        .items_center()
                        .justify_center()
                        .h(px(ui::MENU_ROW_H))
                        .rounded(ui::menu_row_radius())
                        .cursor_pointer()
                        .hover(|d| d.bg(theme::fill_hover()))
                        .when(agent == shown, |d| d.bg(theme::fill_selected()))
                        .child(agent.icon(16.))
                        // The agent the composer uses now: a dot under its icon.
                        .when(agent == current, |d| {
                            d.child(
                                div()
                                    .absolute()
                                    .bottom(px(1.))
                                    .size(px(3.))
                                    .rounded_full()
                                    .bg(theme::text_muted()),
                            )
                        })
                        .tooltip(ui::tip_text(tip))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            if this.menu_agent() != agent {
                                this.menu_agent = Some(agent);
                                this.menu_cursor = None;
                                this.menu_scroll.scroll_to_item(0);
                                cx.notify();
                            }
                        }))
                }))
        });

        ui::menu_panel("model-menu", MODEL_MENU_W)
            .child(heading)
            .child(models)
            .child(ui::menu_divider())
            .children(effort_row)
            .child(settings_row)
            .when(shown != current, |d| {
                d.child(
                    ui::menu_item("model-switch", "arrow-right01", switch_label, false, true)
                        .when(self.menu_focus(reset_ix + 1), |d| d.bg(theme::fill_hover()))
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.menu_run(MenuKey::Agent(shown), window, cx)
                        })),
                )
            })
            .when_some(agent_strip, |d, strip| {
                d.child(ui::menu_divider()).child(strip)
            })
    }

    /// The reasoning level: Auto and Off as small buttons, then a bar per
    /// level, lit up to the current one, and its name. The pointer previews a
    /// level before the click: the bars light up to it and the label names it.
    fn render_effort_row(
        &self,
        agent: AgentKind,
        now: &str,
        options: Vec<crate::agent::SettingOption>,
        focused: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        use crate::agent::Setting;
        let (zeros, levels): (Vec<_>, Vec<_>) =
            options.into_iter().partition(|o| effort_is_zero(&o.id));
        let lit = levels.iter().position(|o| o.id == now);
        // A hover left over from another agent's levels matches nothing here.
        let hovered = self
            .effort_hover
            .as_deref()
            .and_then(|h| zeros.iter().chain(&levels).find(|o| o.id == h));
        let shown = match hovered {
            Some(o) => levels.iter().position(|l| l.id == o.id),
            None => lit,
        };
        let label = match hovered {
            Some(o) => Some(effort_short(&o.id, &o.name)),
            None => lit.map(|i| effort_short(&levels[i].id, &levels[i].name)),
        };
        let previewing = hovered.is_some_and(|o| o.id != now);
        let hover = |id: String| {
            cx.listener(move |this: &mut Self, over: &bool, _, cx| {
                if *over {
                    this.effort_hover = Some(id.clone());
                } else if this.effort_hover.as_deref() == Some(id.as_str()) {
                    // The next bar's enter can come before this one's leave.
                    this.effort_hover = None;
                } else {
                    return;
                }
                cx.notify();
            })
        };
        let zero_buttons: Vec<_> = zeros
            .iter()
            .enumerate()
            .map(|(i, o)| {
                let enabled = o.disabled.is_none();
                let on = o.id == now;
                div()
                    .id(("model-effort-zero", i))
                    .role(gpui::Role::MenuItemRadio)
                    .aria_label(SharedString::from(o.name.clone()))
                    .aria_toggled(crate::ui::toggled(on))
                    .flex_none()
                    .mr(crate::ui::sp(3.))
                    .px(crate::ui::sp(6.))
                    .py(crate::ui::sp(1.))
                    .rounded(crate::ui::rad(ui::RADIUS_SM))
                    .text_size(px(ui::TEXT_SM))
                    .text_color(if on {
                        theme::text()
                    } else {
                        theme::text_subtle()
                    })
                    .when(on, |d| d.bg(theme::fill_selected()))
                    .when(!enabled, |d| d.opacity(0.4))
                    .when(enabled && !on, |d| {
                        d.cursor_pointer()
                            .hover(|s| s.bg(theme::fill_hover()).text_color(theme::text()))
                    })
                    .child(effort_short(&o.id, &o.name))
                    // Only a dimmed level needs a tooltip, for its reason: the
                    // label names the others on hover.
                    .when_some(o.disabled.clone(), |d, why| {
                        d.aria_description(why.clone()).tooltip(ui::tip_text(why))
                    })
                    .when(enabled, |d| {
                        let key = MenuKey::Choice(agent, Setting::Effort, o.id.clone(), true);
                        d.on_hover(hover(o.id.clone())).on_click(cx.listener(
                            move |this, _, window, cx| this.menu_run(key.clone(), window, cx),
                        ))
                    })
                    .into_any_element()
            })
            .collect();
        let bars: Vec<_> = levels
            .iter()
            .enumerate()
            .map(|(i, o)| {
                let enabled = o.disabled.is_none();
                let on = shown.is_some_and(|l| i <= l);
                // While previewing, the bars the pick would add are dimmer
                // than the ones already lit.
                let fill = if !on {
                    theme::fill_selected()
                } else if previewing && !lit.is_some_and(|l| i <= l) {
                    theme::text_subtle()
                } else {
                    theme::active()
                };
                div()
                    .id(("model-effort", i))
                    .role(gpui::Role::MenuItemRadio)
                    .aria_label(SharedString::from(o.name.clone()))
                    .aria_toggled(crate::ui::toggled(o.id == now))
                    .when_some(o.disabled.clone(), |d, why| d.aria_description(why))
                    // No gap between bars: the whole row height and width
                    // takes the pointer, so a level is easy to hit.
                    .flex_1()
                    .h_full()
                    .px(crate::ui::sp(1.5))
                    .flex()
                    .items_center()
                    .child(
                        div()
                            .w_full()
                            .h(px(6.))
                            .rounded_full()
                            .bg(fill)
                            .when(!enabled, |d| d.opacity(0.4)),
                    )
                    .when(enabled, |d| {
                        let key = MenuKey::Choice(agent, Setting::Effort, o.id.clone(), true);
                        d.cursor_pointer()
                            .on_hover(hover(o.id.clone()))
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.menu_run(key.clone(), window, cx)
                            }))
                    })
                    .when_some(o.disabled.clone(), |d, why| d.tooltip(ui::tip_text(why)))
                    .into_any_element()
            })
            .collect();
        let has_bars = !bars.is_empty();
        div()
            .flex()
            .items_center()
            .gap(crate::ui::sp(8.))
            .h(px(ui::MENU_ROW_H))
            .px(crate::ui::sp(8.))
            .rounded(ui::menu_row_radius())
            .text_size(px(ui::TEXT_MD))
            .text_color(theme::text())
            .when(focused, |d| d.bg(theme::fill_hover()))
            .child(div().flex_none().child("Reasoning"))
            .child(
                div()
                    .flex_1()
                    .h_full()
                    .flex()
                    .items_center()
                    .children(zero_buttons)
                    .children(bars),
            )
            .when(has_bars, |d| {
                d.child(
                    div()
                        .flex_none()
                        .w(px(40.))
                        .text_right()
                        .text_size(px(ui::TEXT_SM))
                        .text_color(if previewing {
                            theme::text_subtle()
                        } else {
                            theme::text_muted()
                        })
                        .children(label),
                )
            })
            .into_any_element()
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
        agent: AgentKind,
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
                // Plain children don't reach screen readers, so the row says
                // what it shows next to its name: the description and the
                // note, which for an unavailable choice is the reason.
                let mut spoken: Vec<&str> = Vec::new();
                for text in [description.as_deref(), note.as_deref()]
                    .into_iter()
                    .flatten()
                {
                    if !spoken.contains(&text) {
                        spoken.push(text);
                    }
                }
                let spoken = (!spoken.is_empty()).then(|| spoken.join(". "));
                ui::choice_row(
                    ("choice", s as usize * 1000 + i),
                    o.name.clone(),
                    o.id == current,
                )
                .when(self.menu_focus(offset + i), |d| d.bg(theme::fill_hover()))
                .when(!enabled, |d| d.opacity(0.45).cursor_default())
                // gpui has no disabled state to give screen readers; the
                // reason, part of the note, says the choice is out of reach.
                .when_some(spoken, |d, text| d.aria_description(text))
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
                // The main checkout is not in the list while no session is
                // there. A project can have one here and one on each server
                // that shares its origin: each gets its own entry, by machine.
                let checkouts: Vec<(usize, String)> = self
                    .projects
                    .get(pi)
                    .map(|p| {
                        p.environments
                            .iter()
                            .enumerate()
                            .filter(|(_, e)| e.is_checkout)
                            .map(|(ei, e)| {
                                let label = match &e.remote {
                                    Some(r) => format!(
                                        "Open main checkout on {}",
                                        crate::redact::text(&r.machine)
                                    ),
                                    None => "Open main checkout".to_string(),
                                };
                                (ei, label)
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                panel = panel.child(
                    ui::menu_item("cm-new-env", "git-branch", "New environment…", false, true)
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.open_create_env(pi, window, cx)
                        })),
                );
                for (ei, label) in checkouts {
                    panel = panel.child(
                        ui::menu_item(("cm-checkout", ei), "folder01", label, false, true)
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.menu = None;
                                this.select_env(pi, ei, window, cx)
                            })),
                    );
                }
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
                    .child(
                        ui::menu_item("cm-icon", "paint-board", "Edit icon…", false, true)
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.open_icon_editor(pi, window, cx)
                            })),
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
            MenuTarget::TreeSession(pi, ei, si) => return self.render_tree_menu(pi, ei, si, cx),
            MenuTarget::Picked => return self.render_pick_menu(cx),
            MenuTarget::TreePicked => return self.render_tree_pick_menu(cx),
            MenuTarget::TreeSort => return self.render_tree_sort_menu(cx).into_any_element(),
            MenuTarget::MoreTabs => return self.render_more_menu(cx),
            MenuTarget::Sessions => return self.render_sessions_menu(cx),
            MenuTarget::Help => return self.render_help_menu(cx),
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
            // A server's folder: nothing here acts on it but the Computers tab.
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
                        this.servers_new_env(pi, ei, None, cx)
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
                            ui::menu_item("cm-servers", "computer", "Open Computers", false, true)
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
                            "archive02",
                            "Archive",
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
                let archived = env.is_some_and(|e| e.archived());
                let can_archive = env.is_some_and(|e| {
                    e.sessions
                        .iter()
                        .any(|s| !s.settled && s.status != Status::Working)
                });
                let reveal = path.clone();
                let archive = path.clone();
                panel = panel
                    .child(if archived {
                        ui::menu_item("cm-restore", "undo", "Restore", false, true).on_click(
                            cx.listener(move |this, _, _, cx| {
                                this.menu = None;
                                this.restore_sessions(&archive, None, cx);
                            }),
                        )
                    } else {
                        ui::menu_item("cm-archive", "archive02", "Archive", false, can_archive)
                            .tooltip(ui::tip(
                                "Hide its sessions and resolve its pull request's review threads",
                            ))
                            .when(can_archive, |d| {
                                d.on_click(cx.listener(move |this, _, _, cx| {
                                    this.menu = None;
                                    this.archive_env(&archive, cx);
                                }))
                            })
                    })
                    .child(div().my(px(4.)).h(px(1.)).bg(theme::stroke()))
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

/// A reasoning level as short as the meter's label can hold.
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

/// Whether two menu rows do the same thing, whatever state they show now.
fn same_action(a: &MenuKey, b: &MenuKey) -> bool {
    match (a, b) {
        (MenuKey::Choice(a1, s1, id1, _), MenuKey::Choice(a2, s2, id2, _)) => {
            a1 == a2 && s1 == s2 && id1 == id2
        }
        (MenuKey::Fast(a1, _), MenuKey::Fast(a2, _)) => a1 == a2,
        _ => a == b,
    }
}

/// Whether a reasoning level is the bottom of the meter, lighting no bars.
fn effort_is_zero(id: &str) -> bool {
    matches!(id, "default" | "auto" | "none" | "off")
}

/// Whether picking `agent`'s choice of `s` closes the menu. Reasoning stays
/// open to compare levels, and so does a model of the agent in use: the
/// reasoning row sits below the models and may change with them, so it comes
/// next. Another agent's model switches the agent or hands the session over.
fn closes_on_pick(s: crate::agent::Setting, agent: AgentKind, composer_agent: AgentKind) -> bool {
    use crate::agent::Setting;
    match s {
        Setting::Effort => false,
        Setting::Model => agent != composer_agent,
        _ => true,
    }
}

/// Where the Help menu's Documentation row goes.
const DOCS_URL: &str = "https://arakun.dev/docs";

impl Shell {
    /// The rail's Help: the feedback form, by kind, and the docs.
    pub(super) fn render_help_menu(&self, cx: &mut Context<Self>) -> AnyElement {
        let feedback = [
            ("help-feedback", "message01", "Send feedback…", None),
            ("help-bug", "alert-circle", "Report a bug…", Some("bug")),
            (
                "help-feature",
                "ai-idea",
                "Request a feature…",
                Some("feature"),
            ),
        ];
        let mut panel = ui::menu_panel("help-menu", 220.);
        for (id, icon_name, label, kind) in feedback {
            panel = panel.child(ui::menu_item(id, icon_name, label, false, true).on_click(
                cx.listener(move |this, _, window, cx| {
                    this.menu = None;
                    this.send_feedback_as(kind, window, cx);
                }),
            ));
        }
        panel
            .child(div().my(px(4.)).h(px(1.)).bg(theme::stroke()))
            .child(
                ui::menu_item("help-docs", "link-square02", "Documentation", false, true).on_click(
                    cx.listener(|this, _, window, cx| {
                        this.menu = None;
                        this.open_page(DOCS_URL.into(), window, cx);
                    }),
                ),
            )
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::Setting;

    #[test]
    fn model_menu_stays_open_for_the_same_agent() {
        let (claude, codex) = (AgentKind::Claude, AgentKind::Codex);
        assert!(!closes_on_pick(Setting::Model, claude, claude));
        assert!(closes_on_pick(Setting::Model, codex, claude));
        assert!(!closes_on_pick(Setting::Effort, claude, claude));
        assert!(!closes_on_pick(Setting::Effort, codex, claude));
        assert!(closes_on_pick(Setting::Mode, claude, claude));
    }

    #[test]
    fn effort_levels_fit_the_label() {
        for id in [
            "default", "none", "minimal", "low", "medium", "high", "xhigh", "max", "ultra",
        ] {
            assert!(effort_short(id, id).len() <= 5, "{id}");
        }
        assert_eq!(effort_short("turbo", "Turbo"), "Turbo");
    }

    #[test]
    fn the_cursor_follows_the_action_not_its_state() {
        use crate::agent::Setting;
        let c = AgentKind::Claude;
        let pick = |id: &str, on| MenuKey::Choice(c, Setting::Effort, id.into(), on);
        assert!(same_action(&pick("high", true), &pick("high", false)));
        assert!(!same_action(&pick("high", true), &pick("low", true)));
        assert!(same_action(
            &MenuKey::Fast(c, true),
            &MenuKey::Fast(c, false)
        ));
        assert!(!same_action(&MenuKey::Effort(c), &MenuKey::Reset(c)));
    }

    #[test]
    fn auto_and_off_light_no_bars() {
        for id in ["default", "auto", "none", "off"] {
            assert!(effort_is_zero(id), "{id}");
        }
        for id in ["minimal", "low", "high", "max"] {
            assert!(!effort_is_zero(id), "{id}");
        }
    }
}
