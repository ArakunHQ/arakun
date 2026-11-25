//! The local servers that run: what the status bar's counter and the
//! sidebar's Running piece count, and the list both of them open.

use std::path::Path;

use super::*;

use crate::running::{Scope, Server, Source, TerminalRoot};
use crate::ui::Type;

const WIDTH: f32 = 440.;
const MAX_H: f32 = 560.;
const PORT_W: f32 = 54.;
/// A look walks every process of the machine, so not on every poll.
const EVERY: Duration = Duration::from_secs(10);

impl Shell {
    /// What Arakun knows before it looks: its terminals and the folders of
    /// the environments on this machine.
    fn running_scope(&self, cx: &App) -> Scope {
        let mut scope = Scope {
            app: std::process::id(),
            terminals: Vec::new(),
            envs: Vec::new(),
        };
        for env in self.projects.iter().flat_map(|p| &p.environments) {
            // What runs in a server's environment runs on the server.
            if env.remote.is_some() {
                continue;
            }
            scope.envs.push(env.path.clone());
            for tab in &env.terminals {
                scope.terminals.push(TerminalRoot {
                    pid: tab.terminal.read(cx).pid(),
                    env: env.path.clone(),
                    number: tab.number,
                });
            }
        }
        scope
    }

    /// Looks again, off the main thread. `now` skips the wait between looks:
    /// the list just opened, or a server was just stopped.
    pub(super) fn refresh_running(&mut self, now: bool, cx: &mut Context<Self>) {
        if self.running_scanning {
            // The look under way may have started before what `now` is about.
            self.running_again |= now;
            return;
        }
        if !now && self.running_checked.is_some_and(|t| t.elapsed() < EVERY) {
            return;
        }
        let scope = self.running_scope(cx);
        self.running_scanning = true;
        cx.spawn(async move |this, cx| {
            let found = cx
                .background_spawn(crate::guard::task_or_default(async move {
                    crate::running::scan(&scope)
                }))
                .await;
            let _ = this.update(cx, |this, cx| {
                this.running_scanning = false;
                this.running_checked = Some(Instant::now());
                if this.running != found {
                    this.running = found;
                    cx.notify();
                }
                if std::mem::take(&mut this.running_again) {
                    this.refresh_running(true, cx);
                }
            });
        })
        .detach();
    }

    /// How many servers run and the memory they hold together.
    pub(super) fn running_total(&self) -> (usize, u64) {
        (
            self.running.len(),
            self.running.iter().map(|s| s.memory).sum(),
        )
    }

    pub(super) fn running_open(&self) -> bool {
        matches!(
            self.menu,
            Some(ContextMenu {
                target: MenuTarget::Running,
                ..
            })
        )
    }

    pub(super) fn open_running(&mut self, at: gpui::Point<Pixels>, cx: &mut Context<Self>) {
        self.refresh_running(true, cx);
        self.menu = Some(ContextMenu {
            target: MenuTarget::Running,
            at,
        });
        cx.notify();
    }

    /// The project and environment a server's folder is.
    fn running_place(&self, env: &Path) -> Option<(usize, usize)> {
        self.projects.iter().enumerate().find_map(|(pi, p)| {
            p.environments
                .iter()
                .position(|e| e.path == env)
                .map(|ei| (pi, ei))
        })
    }

    fn stop_running(&mut self, pid: u32, started: u64, cx: &mut Context<Self>) {
        // Out of the list at once; the next look says whether it really went.
        self.running.retain(|s| s.pid != pid);
        cx.notify();
        cx.spawn(async move |this, cx| {
            cx.background_spawn(crate::guard::task_or_default(async move {
                crate::running::stop(pid, started)
            }))
            .await;
            // A moment for the system to take the port back.
            cx.background_executor()
                .timer(Duration::from_millis(400))
                .await;
            let _ = this.update(cx, |this, cx| this.refresh_running(true, cx));
        })
        .detach();
    }

    /// Shows the dock terminal a server runs in, switching to its environment.
    fn show_running_terminal(
        &mut self,
        env: &Path,
        number: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some((pi, ei)) = self.running_place(env) else {
            return;
        };
        self.menu = None;
        if pi != self.active_project || self.projects[pi].active_env != ei {
            self.select_env(pi, ei, window, cx);
        }
        let Some(i) = self.env().terminals.iter().position(|t| t.number == number) else {
            return;
        };
        self.set_dock(true, window, cx);
        self.select_terminal(i, window, cx);
    }

    /// The status bar's counter, there only while something runs.
    pub(super) fn running_chip(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let (count, memory) = self.running_total();
        if count == 0 {
            return None;
        }
        let open = self.running_open();
        Some(
            div()
                .id("status-running")
                .flex()
                .flex_none()
                .items_center()
                .gap(crate::ui::sp(6.))
                .h(px(ui::DOCK_TAB_H))
                .px(crate::ui::sp(6.))
                .rounded(ui::nested(ui::RADIUS_XL, ui::DOCK_INSET))
                .cursor_pointer()
                .when(open, |d| {
                    d.bg(theme::fill_selected()).text_color(theme::content())
                })
                .hover(|s| s.bg(theme::fill_selected()).text_color(theme::content()))
                // Open, the tooltip would sit over the list's own title.
                .when(!open, |d| d.tooltip(ui::tip("Local servers that run")))
                .child(icon("server-stack01", 12.).text_color(theme::text_subtle()))
                .child(format!("{count} running"))
                .child(
                    div()
                        .font_family(theme::FONT_MONO)
                        .text_color(theme::text_faint())
                        .child(memory_label(memory)),
                )
                .on_click(cx.listener(|this, ev: &gpui::ClickEvent, _, cx| {
                    this.open_running(ev.position(), cx)
                }))
                .into_any_element(),
        )
    }

    pub(super) fn render_running_menu(&self, cx: &mut Context<Self>) -> AnyElement {
        let (count, memory) = self.running_total();
        let now = model::unix_now();
        let mut list = div()
            .id("running-scroll")
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .px(crate::ui::sp(4.))
            .pb(crate::ui::sp(4.));
        // By project, in the order the sidebar lists them.
        for (pi, project) in self.projects.iter().enumerate() {
            let rows: Vec<(usize, &Server)> = self
                .running
                .iter()
                .filter_map(|s| {
                    let (p, ei) = self.running_place(&s.env)?;
                    (p == pi).then_some((ei, s))
                })
                .collect();
            if rows.is_empty() {
                continue;
            }
            let total = rows.iter().map(|(_, s)| s.memory).sum();
            list = list.child(project_heading(project, total));
            for (ei, server) in rows {
                list = list.child(self.running_row(pi, ei, server, now, cx));
            }
        }
        if count == 0 {
            list = list.child(
                ui::empty_state(
                    "server-stack01",
                    "Nothing is running",
                    "A server that a terminal or an agent starts in one of your projects shows here.",
                    None,
                )
                .py(crate::ui::sp(24.)),
            );
        }
        let header = div()
            .flex()
            .flex_none()
            .items_center()
            .gap(crate::ui::sp(8.))
            .px(crate::ui::sp(14.))
            .h(px(44.))
            .border_b_1()
            .border_color(theme::border_subtle())
            .child(ui::text(Type::Heading, theme::text(), "Running"))
            .child(div().flex_1())
            .when(count > 0, |d| {
                d.child(
                    ui::text(
                        Type::Caption,
                        theme::text_faint(),
                        format!(
                            "{count} server{} · {}",
                            if count == 1 { "" } else { "s" },
                            memory_label(memory)
                        ),
                    )
                    .font_family(theme::FONT_MONO),
                )
            });
        ui::menu_panel("running-menu", WIDTH)
            .max_h(px(MAX_H))
            .min_h_0()
            .p_0()
            .gap_0()
            .child(header)
            .child(list)
            .into_any_element()
    }

    fn running_row(
        &self,
        pi: usize,
        ei: usize,
        server: &Server,
        now: u64,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let project = &self.projects[pi];
        let env = &project.environments[ei];
        let port = server.ports.first().copied().unwrap_or_default();
        let ports = match server.ports.len() {
            0 | 1 => format!(":{port}"),
            n => format!(":{port} +{}", n - 1),
        };
        // Where in the environment it runs, when not at its root.
        let folder = server
            .cwd
            .as_deref()
            .and_then(|cwd| cwd.strip_prefix(&env.path).ok())
            .filter(|rest| !rest.as_os_str().is_empty())
            .map(|rest| format!("{}/", rest.to_string_lossy().replace('\\', "/")));
        let (source_icon, source) = match server.source {
            Source::Terminal(number) => ("computer-terminal01", format!("Terminal {number}")),
            Source::Agent => ("bot", "Agent".to_string()),
            Source::Outside => ("app-window", "Outside Arakun".to_string()),
        };
        // One environment needs no name: the project's heading says it all.
        let source = if project.environments.len() > 1 {
            format!("{source} · {}", env.name)
        } else {
            source
        };
        let meta = format!(
            "{} · {}",
            memory_label(server.memory),
            crate::branch_history::short_ago(server.started, now)
        );
        let id = server.pid as usize;
        let (pid, started) = (server.pid, server.started);
        let terminal = match server.source {
            Source::Terminal(number) => Some((server.env.clone(), number)),
            _ => None,
        };
        let actions = div()
            .absolute()
            .top_0()
            .bottom_0()
            .right_0()
            .flex()
            .items_center()
            .gap(crate::ui::sp(2.))
            .invisible()
            .group_hover("running-row", |s| s.visible())
            .child(
                icon_button(("running-open", id), "globe", 13., "Open in the browser").on_click(
                    cx.listener(move |this, _, window, cx| {
                        this.menu = None;
                        this.open_url(format!("http://localhost:{port}"), window, cx);
                        cx.notify();
                    }),
                ),
            )
            .children(terminal.map(|(env, number)| {
                icon_button(
                    ("running-terminal", id),
                    "computer-terminal01",
                    13.,
                    "Show its terminal",
                )
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.show_running_terminal(&env, number, window, cx);
                    cx.notify();
                }))
            }))
            .child(
                icon_button(("running-stop", id), "square", 13., "Stop").on_click(cx.listener(
                    move |this, _, _, cx| {
                        this.stop_running(pid, started, cx);
                    },
                )),
            );
        div()
            .id(("running-row", id))
            .group("running-row")
            .flex()
            .items_center()
            .gap(crate::ui::sp(10.))
            .px(crate::ui::sp(8.))
            .py(crate::ui::sp(6.))
            .rounded(crate::ui::rad(ui::RADIUS))
            .hover(|s| s.bg(theme::fill_hover()))
            .child(
                div()
                    .w(px(PORT_W))
                    .flex_none()
                    .font_family(theme::FONT_MONO)
                    .text_size(px(ui::TEXT_SM))
                    .text_color(theme::text())
                    .child(ports),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_w_0()
                    .gap(crate::ui::sp(1.))
                    .child(
                        div()
                            .flex()
                            .items_baseline()
                            .gap(crate::ui::sp(6.))
                            .min_w_0()
                            .child(
                                div()
                                    .min_w_0()
                                    .truncate()
                                    .text_size(px(ui::TEXT_MD))
                                    .text_color(theme::text_muted())
                                    .child(server.command.clone()),
                            )
                            .children(folder.map(|f| {
                                ui::text(Type::Caption, theme::text_faint(), f)
                                    .flex_none()
                                    .font_family(theme::FONT_MONO)
                            })),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(crate::ui::sp(6.))
                            .min_w_0()
                            .text_size(px(ui::TEXT_XS))
                            .text_color(theme::text_faint())
                            .child(icon(source_icon, 12.).text_color(theme::text_faint()))
                            .child(div().min_w_0().truncate().child(source)),
                    ),
            )
            .child(
                div()
                    .relative()
                    .flex()
                    .flex_none()
                    .items_center()
                    .justify_end()
                    // Room for the three buttons that take its place.
                    .min_w(px(80.))
                    .h(px(24.))
                    .child(
                        ui::text(Type::Caption, theme::text_faint(), meta)
                            .font_family(theme::FONT_MONO)
                            .group_hover("running-row", |s| s.invisible()),
                    )
                    .child(actions),
            )
    }
}

fn project_heading(project: &Project, memory: u64) -> impl IntoElement {
    let logo: Hsla = gpui::rgb(project.logo).into();
    div()
        .flex()
        .items_center()
        .gap(crate::ui::sp(6.))
        .px(crate::ui::sp(8.))
        .pt(crate::ui::sp(10.))
        .pb(crate::ui::sp(4.))
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
                .text_size(px(ui::TEXT_SM))
                .font_weight(FontWeight::MEDIUM)
                .text_color(theme::text())
                .child(project.name.clone()),
        )
        .child(
            ui::text(Type::Caption, theme::text_faint(), memory_label(memory))
                .font_family(theme::FONT_MONO),
        )
}

/// "95 MB", "1.5 GB": the memory a server holds, as a task manager says it.
pub(super) fn memory_label(bytes: u64) -> String {
    const MB: f64 = 1024. * 1024.;
    let mb = bytes as f64 / MB;
    if mb < 1000. {
        format!("{:.0} MB", mb.max(1.))
    } else {
        format!("{:.1} GB", mb / 1024.)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn memory_reads_in_megabytes_until_it_is_about_a_gigabyte() {
        assert_eq!(memory_label(0), "1 MB");
        assert_eq!(memory_label(95 * 1024 * 1024), "95 MB");
        assert_eq!(memory_label(640 * 1024 * 1024), "640 MB");
        assert_eq!(memory_label(1000 * 1024 * 1024), "1.0 GB");
        assert_eq!(memory_label(1536 * 1024 * 1024), "1.5 GB");
    }
}
