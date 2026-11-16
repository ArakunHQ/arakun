//! Pop-up menus: the environment menu, setting menus and right-click context menus.

use super::*;

impl Shell {
    pub(super) fn render_env_menu(
        &self,
        viewport_h: f32,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let project = self.project();
        let pi = self.active_project;
        let at = self.env_menu_at;
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
                                plural(env.tabs.len(), "tab"),
                                plural(env.terminals.len(), "terminal")
                            )),
                    )
                    .child(status_dot(env.aggregate_status()))
                    .on_click(
                        cx.listener(move |this, _, window, cx| this.select_env(pi, ei, window, cx)),
                    )
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
                    .on_click(
                        cx.listener(move |this, _, window, cx| {
                            this.open_create_env(pi, window, cx)
                        }),
                    ),
            )
    }

    // ---- composer menus: agent, model and permissions -------------------------

    pub(super) fn open_composer_menu(
        &mut self,
        target: MenuTarget,
        at: gpui::Point<Pixels>,
        cx: &mut Context<Self>,
    ) {
        self.menu_sub = None;
        self.menu_in_sub = false;
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

    /// The rows the keyboard walks in the open menu, top to bottom: the
    /// submenu's choices when the keys went into it, else the menu's own rows.
    pub(super) fn menu_keys(&self, cx: &App) -> Vec<MenuKey> {
        use crate::agent::{AgentKind, Setting};
        let Some(menu) = &self.menu else {
            return Vec::new();
        };
        let choices = |s: Setting| -> Vec<MenuKey> {
            self.setting_choices(s, cx)
                .map(|(_, options)| {
                    options
                        .into_iter()
                        .map(|o| MenuKey::Choice(s, o.id.clone(), o.disabled.is_none()))
                        .collect()
                })
                .unwrap_or_default()
        };
        match menu.target {
            MenuTarget::ModelMenu => match self.menu_sub.filter(|_| self.menu_in_sub) {
                Some(s) => choices(s),
                None => {
                    let mut keys: Vec<MenuKey> = [Setting::Model, Setting::Effort]
                        .into_iter()
                        .filter(|s| self.setting_value_label(*s, cx).is_some())
                        .map(MenuKey::Sub)
                        .collect();
                    if let Some((current, _)) = self.setting_choices(Setting::Fast, cx) {
                        keys.push(MenuKey::Fast(current == "on"));
                    }
                    keys.push(MenuKey::Reset);
                    keys
                }
            },
            MenuTarget::PermissionMenu => self
                .permission_sections(cx)
                .into_iter()
                .flat_map(choices)
                .collect(),
            MenuTarget::AgentMenu => model::Agent::ALL.into_iter().map(MenuKey::Agent).collect(),
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
        match key {
            MenuKey::Choice(s, id, enabled) => {
                if enabled {
                    self.menu = None;
                    self.choose_setting(s, &id, cx);
                }
            }
            MenuKey::Sub(s) => {
                self.menu_sub = Some(s);
                self.menu_in_sub = true;
                let (current, options) = self.setting_choices(s, cx).unwrap_or_default();
                self.menu_cursor = Some(options.iter().position(|o| o.id == current).unwrap_or(0));
            }
            MenuKey::Fast(on) => self.choose_setting(
                crate::agent::Setting::Fast,
                if on { "off" } else { "on" },
                cx,
            ),
            MenuKey::Reset => {
                self.menu = None;
                self.reset_settings(cx);
            }
            MenuKey::Agent(agent) if self.composer_in_session() => {
                self.menu = None;
                self.continue_with(agent, window, cx);
            }
            MenuKey::Agent(agent) => {
                self.menu = None;
                self.env_mut().agent = agent;
                model::set_default_agent(agent);
                self.save(cx);
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
        let usable = |k: &MenuKey| !matches!(k, MenuKey::Choice(_, _, false));
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
            "right" => match self.menu_cursor.and_then(|i| keys.get(i)) {
                Some(k @ MenuKey::Sub(_)) => self.menu_run(k.clone(), window, cx),
                _ => return true,
            },
            "left" if self.menu_in_sub => {
                let sub = self.menu_sub;
                self.menu_in_sub = false;
                self.menu_cursor = sub.and_then(|s| {
                    self.menu_keys(cx)
                        .iter()
                        .position(|k| *k == MenuKey::Sub(s))
                });
            }
            "enter" | "space" => {
                if let Some(k) = self.menu_cursor.and_then(|i| keys.get(i)).cloned() {
                    self.menu_run(k, window, cx);
                }
            }
            _ => return false,
        }
        if self.menu_in_sub
            && let Some(i) = self.menu_cursor
        {
            self.menu_scroll.scroll_to_item(i);
        }
        cx.notify();
        true
    }

    fn menu_key_is_current(&self, k: &MenuKey, cx: &App) -> bool {
        match k {
            MenuKey::Choice(s, id, _) => {
                self.setting_choices(*s, cx).is_some_and(|(c, _)| c == *id)
            }
            MenuKey::Agent(a) => self.composer_agent() == *a,
            _ => false,
        }
    }

    /// Whether the keyboard is on row `i` of the menu's own rows (`sub` false)
    /// or of its submenu (`sub` true).
    fn menu_focus(&self, i: usize, sub: bool) -> bool {
        self.menu_in_sub == sub && self.menu_cursor == Some(i)
    }

    fn permission_sections(&self, cx: &App) -> Vec<crate::agent::Setting> {
        use crate::agent::Setting;
        match self.composer_agent() {
            model::Agent::Claude | model::Agent::OpenCode | model::Agent::Codex => {
                vec![Setting::Mode]
            }
            model::Agent::WorkersAi => Vec::new(),
            model::Agent::Droid => [Setting::Mode, Setting::Autonomy]
                .into_iter()
                .filter(|s| self.setting_choices(*s, cx).is_some())
                .collect(),
        }
    }

    pub(super) fn render_agent_menu(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let current = self.composer_agent();
        let heading = if self.composer_in_session() {
            "Continue with"
        } else {
            "Agent"
        };
        ui::menu_panel("agent-menu", 220.)
            .child(ui::menu_heading(heading))
            .children(model::Agent::ALL.into_iter().enumerate().map(|(i, agent)| {
                ui::choice_row(("agent-opt", agent as usize), agent == current)
                    .when(self.menu_focus(i, false), |d| d.bg(theme::fill_hover()))
                    .child(agent_icon(agent, 14.))
                    .child(div().flex_1().min_w_0().truncate().child(agent.label()))
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.menu_run(MenuKey::Agent(agent), window, cx)
                    }))
            }))
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
                .when(self.menu_focus(i, false), |d| d.bg(theme::fill_hover()))
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
                .when(self.menu_focus(AgentKind::ALL.len(), false), |d| {
                    d.bg(theme::fill_hover())
                })
                .on_click(
                    cx.listener(|this, _, window, cx| {
                        this.menu_run(MenuKey::OpenUsage, window, cx)
                    }),
                ),
            )
    }

    /// Model, reasoning and fast mode; hovering a row (or → on it) shows its
    /// choices beside the menu.
    pub(super) fn render_model_menu(&self, cx: &mut Context<Self>) -> impl IntoElement {
        use crate::agent::Setting;
        const WIDTH: f32 = 250.;
        let rows: Vec<(Setting, String)> = [Setting::Model, Setting::Effort]
            .into_iter()
            .filter_map(|s| Some((s, self.setting_value_label(s, cx)?)))
            .collect();
        let n_rows = rows.len();
        let fast = self
            .setting_choices(Setting::Fast, cx)
            .map(|(c, _)| c == "on");
        let reset_ix = n_rows + usize::from(fast.is_some());
        let open = self.menu_sub.filter(|s| rows.iter().any(|(r, _)| r == s));
        let leave_sub = |this: &mut Self, hovered: &bool, cx: &mut Context<Self>| {
            if *hovered && this.menu_sub.is_some() {
                this.menu_sub = None;
                this.menu_in_sub = false;
                cx.notify();
            }
        };
        ui::menu_panel("model-menu", WIDTH)
            .relative()
            .children(rows.into_iter().enumerate().map(|(i, (s, value))| {
                let open_here = open == Some(s);
                ui::submenu_row(
                    ("model-row", s as usize),
                    s.label(),
                    value,
                    open_here || self.menu_focus(i, false),
                )
                .on_hover(cx.listener(move |this, hovered: &bool, _, cx| {
                    if *hovered && this.menu_sub != Some(s) {
                        this.menu_sub = Some(s);
                        this.menu_in_sub = false;
                        cx.notify();
                    }
                }))
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.menu_run(MenuKey::Sub(s), window, cx)
                }))
            }))
            .when_some(fast, |d, on| {
                d.child(
                    ui::switch_row("model-fast", Setting::Fast.label(), on)
                        .when(self.menu_focus(n_rows, false), |d| {
                            d.bg(theme::fill_hover())
                        })
                        .on_hover(cx.listener(move |this, hovered: &bool, _, cx| {
                            leave_sub(this, hovered, cx)
                        }))
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.menu_run(MenuKey::Fast(on), window, cx)
                        })),
                )
            })
            .child(ui::menu_divider())
            .child(
                ui::menu_item("model-reset", "undo", "Reset to default", false, true)
                    .when(self.menu_focus(reset_ix, false), |d| {
                        d.bg(theme::fill_hover())
                    })
                    .on_hover(
                        cx.listener(move |this, hovered: &bool, _, cx| {
                            leave_sub(this, hovered, cx)
                        }),
                    )
                    .on_click(
                        cx.listener(|this, _, window, cx| {
                            this.menu_run(MenuKey::Reset, window, cx)
                        }),
                    ),
            )
            .when_some(open, |d, s| {
                d.child(
                    div()
                        .absolute()
                        .left(px(WIDTH + 4.))
                        .bottom(px(-1.))
                        .child(self.render_choice_list(s, false, cx)),
                )
            })
    }

    /// Who decides what the agent may do: Claude's modes, or Droid's mode and
    /// autonomy, each choice with what it means.
    pub(super) fn render_permission_menu(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let sections = self.permission_sections(cx);
        let last = sections.len().saturating_sub(1);
        let mut offset = 0;
        let mut panel = ui::menu_panel("permission-menu", 320.);
        for (i, s) in sections.into_iter().enumerate() {
            let rows = self.choice_rows(s, true, offset, false, cx);
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

    /// A setting's choices as their own little menu, beside the model menu.
    fn render_choice_list(
        &self,
        s: crate::agent::Setting,
        describe: bool,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        use crate::agent::Setting;
        let width = if s == Setting::Model { 290. } else { 200. };
        ui::menu_panel(("choice-list", s as usize), width)
            .max_h(px(400.))
            .min_h_0()
            .overflow_y_scroll()
            .track_scroll(&self.menu_scroll)
            .children(self.choice_rows(s, describe, 0, true, cx))
    }

    /// The choices of a setting as radio rows; `offset` is where they start in
    /// the keyboard's order, `sub` whether they are a submenu's.
    fn choice_rows(
        &self,
        s: crate::agent::Setting,
        describe: bool,
        offset: usize,
        sub: bool,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        use crate::agent::Setting;
        let (current, options) = self.setting_choices(s, cx).unwrap_or_default();
        options
            .into_iter()
            .enumerate()
            .map(|(i, o)| {
                let enabled = o.disabled.is_none();
                let key = MenuKey::Choice(s, o.id.clone(), enabled);
                // Claude's "Default (recommended)" model says which model it is.
                let note = o.disabled.clone().or(o.detail.clone()).or_else(|| {
                    (s == Setting::Model && o.id == "default")
                        .then(|| o.description.clone())
                        .flatten()
                });
                let description = o.description.filter(|_| describe);
                ui::choice_row(("choice", s as usize * 1000 + i), o.id == current)
                    .when(self.menu_focus(offset + i, sub), |d| {
                        d.bg(theme::fill_hover())
                    })
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
                panel = panel
                    .child(
                        ui::menu_item("cm-new-env", "git-branch", "New environment…", false, true)
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.open_create_env(pi, window, cx)
                            })),
                    )
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
            MenuTarget::AgentMenu => return self.render_agent_menu(cx).into_any_element(),
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
