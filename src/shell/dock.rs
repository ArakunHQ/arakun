//! The terminal dock: opening, focusing and closing terminals, and drawing it.

use super::*;

impl Shell {
    pub(super) fn toggle_dock(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.set_dock(!self.dock_open, window, cx);
    }

    /// Sets the whole-UI zoom and relayouts the window at the new density.
    pub(super) fn zoom_to(&mut self, pct: u32, window: &mut Window, cx: &mut Context<Self>) {
        if pct == crate::ui_zoom::pct() {
            return;
        }
        crate::ui_zoom::set_pct(pct);
        // Deferred: the bounds observer (`track_window`) updates this Shell,
        // which is mid-update here, and GPUI aborts on the nested update.
        window.defer(cx, |window, cx| window.bounds_changed(cx));
        self.save(cx);
        cx.notify();
    }

    pub(super) fn set_dock(&mut self, open: bool, window: &mut Window, cx: &mut Context<Self>) {
        self.dock_open = open;
        if open && self.has_projects() {
            if self.env().terminals.is_empty() {
                self.new_terminal(window, cx);
            } else {
                self.focus_terminal(window, cx);
            }
        } else if self.has_projects() {
            window.focus(&self.composer_focus, cx);
        }
        self.save(cx);
        cx.notify();
    }

    /// A visible dock always has a shell in it.
    pub(super) fn ensure_terminal(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.dock_open && self.has_projects() && self.env().terminals.is_empty() {
            self.spawn_terminal(false, window, cx);
        }
    }

    pub(super) fn new_terminal(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.spawn_terminal(true, window, cx);
    }

    pub(super) fn spawn_terminal(
        &mut self,
        focus: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.spawn_terminal_command(None, focus, window, cx);
    }

    /// A terminal in the visible environment, optionally running `command`
    /// instead of an interactive shell.
    pub(super) fn spawn_terminal_command(
        &mut self,
        command: Option<Vec<String>>,
        focus: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.has_projects() {
            return;
        }
        let cwd = self.env().path.clone();
        self.spawn_terminal_in(&cwd, command, focus, window, cx);
    }

    /// Opens `kind`'s sign-in in a terminal. Codex's CLI comes with its adapter,
    /// so before any session ran it is installed first, off the main thread. A
    /// notice says why when nothing can open: terminals need a project.
    pub(super) fn sign_in(
        &mut self,
        kind: crate::agent::AgentKind,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        use crate::island::AlertKind;
        let title = format!("Sign in to {}", kind.label());
        let cmd = crate::agent::login_command(kind);
        if cmd.is_some() || kind != crate::agent::AgentKind::Codex {
            self.run_account_command(&title, "sign-in", kind, cmd, window, cx);
            return;
        }
        if !self.terminal_possible(&title, "sign-in", cx) {
            return;
        }
        let what = match crate::agent::adapter_install(kind) {
            Some(plan) => format!(
                "Installing {} into {} with npm first; the sign-in opens when it is done.",
                plan.package,
                plan.dir.display()
            ),
            None => "Installing Codex first; the sign-in opens when it is done.".to_string(),
        };
        self.notify(AlertKind::Info, &title, &what, None, cx);
        cx.spawn_in(window, async move |this, cx| {
            let installed = cx
                .background_spawn(async move { crate::agent::install_codex() })
                .await;
            let _ = this.update_in(cx, |this, window, cx| {
                // The last project may have closed during the install.
                if installed.is_ok() && !this.terminal_possible(&title, "sign-in", cx) {
                    return;
                }
                let cmd = installed.and_then(|()| {
                    crate::agent::login_command(kind)
                        .ok_or_else(|| anyhow::anyhow!("the codex CLI is still missing"))
                });
                match cmd {
                    Ok(cmd) => this.spawn_terminal_command(Some(cmd), true, window, cx),
                    Err(e) => this.notify(
                        AlertKind::Failure,
                        &title,
                        &format!("Installing Codex failed: {e:#}"),
                        None,
                        cx,
                    ),
                }
            });
        })
        .detach();
    }

    /// Opens `kind`'s sign-out in a terminal, or says why it can't.
    pub(super) fn sign_out(
        &mut self,
        kind: crate::agent::AgentKind,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let title = format!("Sign out of {}", kind.label());
        let cmd = crate::agent::logout_command(kind);
        self.run_account_command(&title, "sign-out", kind, cmd, window, cx);
    }

    /// Runs one of `kind`'s account commands in a terminal. `cmd` is `None`
    /// when the agent's CLI wasn't found. `what` names the command in the
    /// notice shown when it can't run.
    pub(super) fn run_account_command(
        &mut self,
        title: &str,
        what: &str,
        kind: crate::agent::AgentKind,
        cmd: Option<Vec<String>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.terminal_possible(title, what, cx) {
            return;
        }
        match cmd {
            Some(cmd) => self.spawn_terminal_command(Some(cmd), true, window, cx),
            None => {
                let body = format!("{} was not found. Install it first.", kind.label());
                self.notify(crate::island::AlertKind::Failure, title, &body, None, cx);
            }
        }
    }

    /// Whether a terminal can open: they need a project. If not, a notice
    /// titled `title` says so; `what` names what would have run.
    fn terminal_possible(&mut self, title: &str, what: &str, cx: &mut Context<Self>) -> bool {
        if self.has_projects() {
            return true;
        }
        let body = format!("Open a project first: the {what} runs in one of its terminals.");
        self.notify(crate::island::AlertKind::Warning, title, &body, None, cx);
        false
    }

    /// Runs `command` in a new terminal of `env_path`; it prints `[exit N]`
    /// when done and keeps the output on screen until Enter opens a shell,
    /// even when the command calls `exit` or is stopped with Ctrl+C. The
    /// command runs at lower priority; the shell left afterwards doesn't.
    /// Finished runs nobody opened are pruned first. Returns the terminal's index.
    pub(super) fn run_in_terminal(
        &mut self,
        env_path: &std::path::Path,
        command: &str,
        focus: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<usize> {
        self.prune_finished_runs(env_path, command, cx);
        let cmd = crate::platform::run_then_shell(command);
        let i = self.spawn_terminal_in(env_path, Some(cmd), focus, window, cx)?;
        let tab = self.find_env_mut(env_path)?.terminals.get_mut(i)?;
        tab.command = Some(command.into());
        Some(i)
    }

    /// Closes the terminals of finished runs still waiting for Enter: those of
    /// `command` (the new run replaces them) and all but the newest
    /// `KEEP_FINISHED_RUNS` others, so repeated runs don't pile up. One whose
    /// shell was opened stays, and so does one on screen unless the new run
    /// replaces it. Focus doesn't move.
    fn prune_finished_runs(
        &mut self,
        env_path: &std::path::Path,
        command: &str,
        cx: &mut Context<Self>,
    ) {
        const KEEP_FINISHED_RUNS: usize = 4;
        let Some(env) = self.find_env(env_path) else {
            return;
        };
        let finished: Vec<(gpui::EntityId, bool)> = env
            .terminals
            .iter()
            .filter(|t| {
                t.command.is_some()
                    && super::commands::run_waiting(&t.terminal.read(cx).recent_text(4))
            })
            .map(|t| {
                (
                    t.terminal.entity_id(),
                    t.command.as_deref() == Some(command),
                )
            })
            .filter(|&(id, same)| same || !self.terminal_on_screen(env_path, id))
            .collect();
        let others = finished.iter().filter(|(_, same)| !same).count();
        let mut extra = others.saturating_sub(KEEP_FINISHED_RUNS);
        let close: Vec<gpui::EntityId> = finished
            .into_iter()
            .filter_map(|(id, same)| {
                if !same && extra == 0 {
                    return None;
                }
                extra -= usize::from(!same);
                Some(id)
            })
            .collect();
        if close.is_empty() {
            return;
        }
        let env = self.find_env_mut(env_path).expect("env looked up above");
        let active = env
            .terminals
            .get(env.active_terminal)
            .map(|t| t.terminal.entity_id());
        env.terminals.retain(|t| {
            let keep = !close.contains(&t.terminal.entity_id());
            if !keep {
                t.terminal.read(cx).kill();
            }
            keep
        });
        env.active_terminal = active
            .and_then(|a| {
                env.terminals
                    .iter()
                    .position(|t| t.terminal.entity_id() == a)
            })
            .unwrap_or(env.terminals.len().saturating_sub(1));
        cx.notify();
    }

    /// A terminal in any environment; the dock only opens for the visible one.
    pub(super) fn spawn_terminal_in(
        &mut self,
        env_path: &std::path::Path,
        command: Option<Vec<String>>,
        focus: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<usize> {
        let visible = self.has_projects() && self.env().path == env_path;
        let cwd = env_path.to_path_buf();
        let terminal = match Terminal::spawn(cwd.clone(), command, cx) {
            Ok(t) => t,
            Err(e) => {
                eprintln!(
                    "arakun: could not start a shell in {}: {e:#}",
                    cwd.display()
                );
                return None;
            }
        };
        let view = cx.new(|cx| TerminalView::new(terminal.clone(), window, cx));
        let events = cx.subscribe_in(
            &terminal,
            window,
            move |this, terminal, ev: &TerminalEvent, window, cx| match ev {
                TerminalEvent::Exited => {
                    let id = terminal.entity_id();
                    this.close_terminal(&cwd, id, window, cx);
                    this.reload_file_tabs(&cwd, cx);
                }
                TerminalEvent::Activity => {
                    let id = terminal.entity_id();
                    let on_screen = this.terminal_on_screen(&cwd, id);
                    if let Some(env) = this.find_env_mut(&cwd)
                        && let Some(tab) = env
                            .terminals
                            .iter_mut()
                            .find(|t| t.terminal.entity_id() == id)
                        && !on_screen
                        && !tab.unseen
                    {
                        tab.unseen = true;
                        cx.notify();
                    }
                }
                TerminalEvent::TitleChanged | TerminalEvent::Bell => cx.notify(),
            },
        );
        let env = self.find_env_mut(env_path)?;
        env.terminals_opened += 1;
        env.terminals.push(TermTab {
            terminal,
            view,
            number: env.terminals_opened,
            command: None,
            unseen: !visible,
            _events: events,
        });
        env.active_terminal = env.terminals.len() - 1;
        let index = env.active_terminal;
        if visible {
            self.dock_open = true;
            if focus {
                self.focus_terminal(window, cx);
            }
        }
        cx.notify();
        Some(index)
    }

    pub(super) fn terminal_on_screen(
        &self,
        env_path: &std::path::Path,
        id: gpui::EntityId,
    ) -> bool {
        self.dock_open
            && self.has_projects()
            && self.env().path == env_path
            && self
                .env()
                .terminals
                .get(self.env().active_terminal)
                .is_some_and(|t| t.terminal.entity_id() == id)
    }

    pub(super) fn focus_terminal(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let env = self.env_mut();
        let i = env.active_terminal;
        if let Some(tab) = env.terminals.get_mut(i) {
            tab.unseen = false;
            let handle = tab.view.focus_handle(cx);
            window.focus(&handle, cx);
        }
    }

    pub(super) fn select_terminal(
        &mut self,
        index: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.env_mut().active_terminal = index;
        self.focus_terminal(window, cx);
        cx.notify();
    }

    pub(super) fn close_terminal(
        &mut self,
        env_path: &std::path::Path,
        id: gpui::EntityId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let visible = self.has_projects() && self.env().path == env_path;
        let Some(env) = self.find_env_mut(env_path) else {
            return;
        };
        let Some(i) = env
            .terminals
            .iter()
            .position(|t| t.terminal.entity_id() == id)
        else {
            return;
        };
        let tab = env.terminals.remove(i);
        tab.terminal.read(cx).kill();
        if env.active_terminal > i || env.active_terminal >= env.terminals.len() {
            env.active_terminal = env.active_terminal.saturating_sub(1);
        }
        let empty = env.terminals.is_empty();
        if visible {
            if empty {
                self.dock_open = false;
                self.save(cx);
                window.focus(&self.composer_focus, cx);
            } else {
                self.focus_terminal(window, cx);
            }
        }
        cx.notify();
    }

    pub(super) fn render_dock(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let env = self.env();
        let dock_open = self.dock_open;
        let env_path = env.path.clone();
        let (added, removed) = env.diff_totals();
        let path = env.path.clone();
        let mut tabs = div()
            .id("term-tabs")
            .flex()
            .flex_1()
            .min_w_0()
            .items_center()
            .h_full()
            .gap(crate::ui::sp(2.))
            .overflow_x_scroll();
        for (i, tab) in env.terminals.iter().enumerate() {
            let active = i == env.active_terminal;
            let group: SharedString = format!("term-{i}").into();
            let terminal = tab.terminal.read(cx);
            let title = terminal.title();
            let exited = terminal.has_exited().is_some();
            let id = tab.terminal.entity_id();
            let close_path = env_path.clone();
            tabs = tabs.child(
                ui::tab(("term", i), active)
                    .w(px(DOCK_TAB_W))
                    .h(px(ui::DOCK_TAB_H))
                    .group(group.clone())
                    .pr(crate::ui::sp(4.) + px(ui::TAB_CLOSE_ROOM))
                    .child(icon("computer-terminal01", 14.).text_color(if active {
                        theme::content()
                    } else {
                        theme::mix(0.5)
                    }))
                    .child(
                        div()
                            .flex_none()
                            .text_size(px(ui::TEXT_XS))
                            .text_color(theme::text_faint())
                            .child(tab.number.to_string()),
                    )
                    .child(div().flex_1().min_w_0().truncate().child(title))
                    .when(tab.unseen && !active, |d| {
                        d.child(
                            div()
                                .flex_none()
                                .size(px(6.))
                                .rounded_full()
                                .bg(theme::text()),
                        )
                    })
                    .when(exited, |d| d.opacity(0.6))
                    .child(
                        div()
                            .id(("term-close", i))
                            .absolute()
                            .right(px(4.))
                            .top(px(DOCK_TAB_CLOSE_TOP))
                            .flex()
                            .items_center()
                            .justify_center()
                            .size(px(20.))
                            .rounded(crate::ui::rad(4.))
                            .when(!active, |d| {
                                d.invisible().group_hover(group.clone(), |s| s.visible())
                            })
                            .hover(|s| s.bg(theme::fill_selected()))
                            .child(icon("cancel01", 12.).text_color(theme::text_subtle()))
                            .on_click(cx.listener(move |this, _, window, cx| {
                                cx.stop_propagation();
                                this.close_terminal(&close_path, id, window, cx);
                            })),
                    )
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.select_terminal(i, window, cx);
                    })),
            );
        }

        // The dock's header doubles as the status strip: branch, path and CI
        // chips on the left; terminal controls on the right.
        let header = div()
            .flex()
            .flex_none()
            .items_center()
            .h(px(ui::DOCK_BAR_H))
            // Chips and icon buttons are as tall as the dock's tabs, so one
            // inset for this 32 px header matches the sides to the top.
            .pl(ui::dock_inset())
            .pr(ui::dock_inset())
            .gap(crate::ui::sp(6.))
            .text_size(px(ui::TEXT_XS))
            .text_color(theme::text_subtle())
            .child(
                div()
                    .id("status-branch")
                    .flex()
                    .items_center()
                    .gap(crate::ui::sp(6.))
                    .h(px(ui::DOCK_TAB_H))
                    .px(crate::ui::sp(6.))
                    .rounded(ui::nested(ui::RADIUS_XL, ui::DOCK_INSET))
                    .cursor_pointer()
                    .hover(|s| s.bg(theme::fill_selected()).text_color(theme::content()))
                    .child(icon("git-branch", 12.).text_color(theme::text_subtle()))
                    .child(env.branch.clone())
                    .when(added + removed > 0, |d| {
                        d.child(
                            div()
                                .text_color(theme::diff_add())
                                .child(format!("+{added}")),
                        )
                        .child(
                            div()
                                .text_color(theme::diff_del())
                                .child(format!("-{removed}")),
                        )
                    })
                    .on_click(cx.listener(|this, _, _, cx| this.show_changes(cx))),
            )
            .when(
                env.sync.ahead + env.sync.behind > 0 || env.pr.is_some(),
                |d| {
                    let (ahead, behind) = (env.sync.ahead, env.sync.behind);
                    let pr = env.pr.clone();
                    let branch = env.branch.to_string();
                    d.child(
                        div()
                            .id("status-sync")
                            .flex()
                            .items_center()
                            .gap(crate::ui::sp(6.))
                            .h(px(ui::DOCK_TAB_H))
                            .px(crate::ui::sp(6.))
                            .rounded(ui::nested(ui::RADIUS_XL, ui::DOCK_INSET))
                            .cursor_pointer()
                            .hover(|s| s.bg(theme::fill_selected()).text_color(theme::content()))
                            .tooltip(ui::tip("Show in Branches"))
                            .when(ahead > 0, |d| d.child(format!("↑{ahead}")))
                            .when(behind > 0, |d| d.child(format!("↓{behind}")))
                            .when_some(pr, |d, pr| {
                                let color: Hsla = match pr.state.as_str() {
                                    "MERGED" => theme::pr_merged().into(),
                                    "OPEN" if pr.is_draft => theme::mix(0.55),
                                    "OPEN" => theme::pr_open().into(),
                                    _ => theme::mix(0.45),
                                };
                                d.child(icon("git-pull-request", 12.).text_color(color))
                                    .child(div().text_color(color).child(format!("#{}", pr.number)))
                            })
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.reveal_branch(branch.clone(), window, cx)
                            })),
                    )
                },
            )
            .child(
                div()
                    .id("status-path")
                    .flex()
                    .items_center()
                    .gap(crate::ui::sp(6.))
                    .h(px(ui::DOCK_TAB_H))
                    .px(crate::ui::sp(6.))
                    .rounded(ui::nested(ui::RADIUS_XL, ui::DOCK_INSET))
                    .cursor_pointer()
                    .font_family(theme::FONT_MONO)
                    .text_color(theme::text_faint())
                    .hover(|s| s.bg(theme::fill_selected()).text_color(theme::content()))
                    .map(|d| {
                        let full = path.display().to_string();
                        if crate::agent::was_copied(&full) {
                            d.child(icon("tick02", 12.).text_color(theme::content()))
                                .child("Copied path")
                        } else {
                            d.child(pretty_path(&path))
                        }
                    })
                    .on_click(move |_, window, cx| {
                        crate::agent::copy_with_feedback(&path.display().to_string(), window, cx)
                    }),
            )
            .map(|d| {
                let (branch_ci, deploy) = self.status_ci(cx);
                d.when_some(branch_ci, |d, (state, run)| {
                    let id = run.id;
                    let color: Hsla = match state {
                        ci::State::Failed => theme::error().into(),
                        ci::State::Running | ci::State::Queued => theme::attention().into(),
                        _ => theme::mix(0.55),
                    };
                    d.child(
                        div()
                            .id("status-ci")
                            .flex()
                            .items_center()
                            .gap(crate::ui::sp(6.))
                            .h(px(ui::DOCK_TAB_H))
                            .px(crate::ui::sp(6.))
                            .rounded(ui::nested(ui::RADIUS_XL, ui::DOCK_INSET))
                            .cursor_pointer()
                            .text_color(color)
                            .when(state == ci::State::Failed, |d| d.bg(gpui::rgba(0xe5484d1f)))
                            .hover(|s| s.bg(theme::fill_selected()))
                            .child(ci_mark(state))
                            .child(runs::ci_chip_label(state, &run))
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.open_runs_tab(Some(id), window, cx)
                            })),
                    )
                })
                .when_some(deploy, |d, run| {
                    let (id, state) = (run.id, run.state());
                    d.child(
                        div()
                            .id("status-deploy")
                            .flex()
                            .items_center()
                            .gap(crate::ui::sp(6.))
                            .h(px(ui::DOCK_TAB_H))
                            .px(crate::ui::sp(6.))
                            .rounded(ui::nested(ui::RADIUS_XL, ui::DOCK_INSET))
                            .cursor_pointer()
                            .hover(|s| s.bg(theme::fill_selected()).text_color(theme::content()))
                            .child(icon("cloud-upload", 12.).text_color(theme::text_subtle()))
                            .child(format!("Production {}", run.head_branch))
                            .child(ci_mark(state))
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.open_runs_tab(Some(id), window, cx)
                            })),
                    )
                })
            })
            .when(crate::ui_zoom::pct() != 100, |d| {
                d.child(
                    div()
                        .id("status-zoom")
                        .flex()
                        .items_center()
                        .gap(crate::ui::sp(6.))
                        .h(px(ui::DOCK_TAB_H))
                        .px(crate::ui::sp(6.))
                        .rounded(ui::nested(ui::RADIUS_XL, ui::DOCK_INSET))
                        .cursor_pointer()
                        .hover(|s| s.bg(theme::fill_selected()).text_color(theme::content()))
                        .tooltip(ui::tip("Reset zoom (Ctrl+0)"))
                        .child(icon("search01", 12.).text_color(theme::text_subtle()))
                        .child(format!("{}%", crate::ui_zoom::pct()))
                        .on_click(cx.listener(|this, _, window, cx| this.zoom_to(100, window, cx))),
                )
            })
            .when(dock_open, |d| {
                d.child(tabs).child(
                    div()
                        .flex()
                        .gap(crate::ui::sp(2.))
                        .child(
                            icon_button("term-add", "add01", 14.)
                                .tooltip(ui::tip("New terminal (Ctrl+Shift+T)"))
                                .on_click(
                                    cx.listener(|this, _, window, cx| {
                                        this.new_terminal(window, cx)
                                    }),
                                ),
                        )
                        .child(
                            icon_button("term-hide", "arrow-down01", 14.)
                                .tooltip(ui::tip("Hide terminal (Ctrl+J)"))
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.set_dock(false, window, cx)
                                })),
                        ),
                )
            })
            .when(!dock_open, |d| {
                d.child(div().flex_1()).child(
                    div()
                        .id("status-terminal")
                        .flex()
                        .items_center()
                        .gap(crate::ui::sp(6.))
                        .h(px(ui::DOCK_TAB_H))
                        .px(crate::ui::sp(6.))
                        .rounded(ui::nested(ui::RADIUS_XL, ui::DOCK_INSET))
                        .cursor_pointer()
                        .text_color(theme::text_faint())
                        .hover(|s| s.bg(theme::fill_selected()).text_color(theme::content()))
                        .child(icon("computer-terminal01", 14.).text_color(theme::text_faint()))
                        .child("Terminal")
                        .on_click(
                            cx.listener(|this, _, window, cx| this.set_dock(true, window, cx)),
                        ),
                )
            });

        let body: AnyElement = match env.terminals.get(env.active_terminal) {
            Some(tab) => div()
                .flex_1()
                .min_h_0()
                .pl(crate::ui::sp(10.))
                .pt(crate::ui::sp(6.))
                .child(tab.view.clone())
                .into_any_element(),
            None => panel_empty("No terminals in this environment yet.").into_any_element(),
        };

        let handle = div()
            .id("dock-resize")
            .absolute()
            .top(px(-3.))
            .left_0()
            .right_0()
            .h(px(6.))
            .cursor_row_resize()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, _, cx| {
                    this.dock_drag = true;
                    cx.stop_propagation();
                    cx.notify();
                }),
            );

        // Open, the terminal is a sheet like the center. Closed, only its
        // header is left, as the status line on the window itself.
        let card = if dock_open {
            ui::card().bg(theme::sheet())
        } else {
            div()
        };
        card.id("dock-card")
            .map(ui::stop_window_move)
            .relative()
            .flex()
            .flex_col()
            .flex_none()
            // Open, the user's height; closed, as tall as its header, from the
            // layout itself: a fixed number here pushed the chips to the bottom.
            .when(dock_open, |d| d.h(px(self.dock_height)))
            // Always there, so the content doesn't move by a pixel when a
            // drag starts; seen only while dragging.
            .border_1()
            .border_color(if self.dock_drag {
                theme::border_strong()
            } else {
                gpui::transparent_black()
            })
            .child(header)
            .when(dock_open, |d| d.child(body).child(handle))
    }
}
