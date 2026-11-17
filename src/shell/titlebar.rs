//! The title bar: back and forward, the tabs of the environment and the window buttons.

use super::*;

impl Shell {
    pub(super) fn render_titlebar(
        &self,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let env = self.env();
        // The tabs take what they need and the centred title what is left.
        let mut strip = div()
            .flex()
            .min_w_0()
            .items_center()
            .h_full()
            // The first tab sits as far from the card's side as from its top.
            .pl(ui::bar_inset().max(ui::traffic_lights_inset(window) - px(self.titlebar_x())))
            .pr(ui::icon_inset())
            .gap(crate::ui::sp(2.))
            // Presses that land in the gaps around a tab stay here: aiming at a
            // tab and missing by a pixel must not slide the window away.
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation());
        // With the rail hidden its toggles move here: one brings the rail back,
        // the other the workspace panel.
        if !self.rail_shown() {
            for (id, icon_name, lit, rail) in [
                ("title-rail", "sidebar-left", false, true),
                ("title-panel", "layout-align-right", self.panel_open, false),
            ] {
                strip = strip.child(
                    div()
                        .id(id)
                        .flex()
                        .flex_none()
                        .items_center()
                        .justify_center()
                        .size(px(24.))
                        .mr(px(2.))
                        .rounded(crate::ui::rad(8.))
                        .cursor_pointer()
                        .hover(|s| s.bg(theme::hover()))
                        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                        .on_click(cx.listener(move |this, _, _, cx| {
                            if rail {
                                this.toggle_rail(cx)
                            } else {
                                this.toggle_panel(cx)
                            }
                        }))
                        .tooltip(ui::tip(if rail {
                            "Show sidebar (Ctrl+B)"
                        } else {
                            "Workspace panel"
                        }))
                        .child(icon(icon_name, 14.).text_color(if lit {
                            theme::content()
                        } else {
                            theme::mix(0.7)
                        })),
                );
            }
            strip = strip.child(
                div()
                    .flex_none()
                    .w(px(1.))
                    .h(px(16.))
                    .mx(px(4.))
                    .bg(theme::fill_selected()),
            );
        }
        // Back/forward through the tab history of this environment; the rail has
        // its own pair while it shows.
        let (back, fwd) = (self.can_go_back(), self.can_go_forward());
        let history = [
            ("tab-back", "arrow-left01", back, true),
            ("tab-fwd", "arrow-right01", fwd, false),
        ];
        for (id, icon_name, enabled, go_back) in history.into_iter().filter(|_| !self.rail_shown())
        {
            strip = strip.child(
                div()
                    .id(id)
                    .flex()
                    .flex_none()
                    .items_center()
                    .justify_center()
                    .size(px(24.))
                    .mr(px(2.))
                    .rounded(crate::ui::rad(8.))
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .when(enabled, |d| {
                        d.cursor_pointer()
                            .hover(|s| s.bg(theme::hover()))
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.tab_history(go_back, window, cx)
                            }))
                    })
                    .tooltip(ui::tip(if go_back {
                        "Back (Alt+Left)"
                    } else {
                        "Forward (Alt+Right)"
                    }))
                    .child(icon(icon_name, 14.).text_color(theme::mix(if enabled {
                        0.7
                    } else {
                        0.3
                    }))),
            );
        }
        let in_other_pane = env.split_other();
        let shell = cx.entity();
        let lifted = self.lifted_tab(cx);
        // Tabs past the bar's width scroll instead of running under the title and the
        // window buttons.
        let mut list = div()
            .id("tab-list")
            .flex()
            .items_center()
            .min_w_0()
            .h_full()
            .gap(crate::ui::sp(2.))
            .overflow_x_scroll()
            .track_scroll(&self.tab_scroll);
        let active_id = env.tabs.get(env.active_tab).map(|t| t.id);
        if self.tab_revealed.replace(active_id) != active_id {
            self.tab_scroll.scroll_to_item(env.active_tab);
        }
        for (i, tab) in env.tabs.iter().enumerate() {
            let dragged = drag::DraggedTab::new(tab.id, tab.title.clone());
            let shell = shell.clone();
            let active = i == env.active_tab;
            let shown_beside = in_other_pane == Some(i);
            let closable = env.tabs.len() > 1 || !matches!(tab.kind, TabKind::NewSession);
            let tab_icon: AnyElement = match &tab.kind {
                TabKind::Session { agent, .. } => agent_icon(*agent, 14.).into_any_element(),
                TabKind::NewSession => agent_icon(env.agent, 14.).into_any_element(),
                TabKind::Browser { .. } => {
                    let page = tab.browser.as_ref().map(|b| b.view.read(cx));
                    match (
                        page.is_some_and(|p| p.is_loading()),
                        page.and_then(|p| p.favicon()),
                    ) {
                        (true, _) => icon("loading03", 14.)
                            .text_color(theme::text_subtle())
                            .into_any_element(),
                        (false, Some(f)) => img(f).size(px(14.)).into_any_element(),
                        (false, None) => icon("globe", 14.)
                            .text_color(theme::content())
                            .into_any_element(),
                    }
                }
                TabKind::File { .. } => {
                    let (name, color) = file_icon(&tab.title);
                    icon(name, 14.).text_color(color).into_any_element()
                }
                TabKind::Diff { .. } => icon("file-diff", 14.)
                    .text_color(theme::content())
                    .into_any_element(),
                TabKind::Notes => icon("file01", 14.)
                    .text_color(theme::content())
                    .into_any_element(),
                TabKind::Memory => icon("ai-idea", 14.)
                    .text_color(theme::content())
                    .into_any_element(),
                #[cfg(feature = "coderlabs")]
                TabKind::Tasks => icon("checkmark-circle02", 14.)
                    .text_color(theme::content())
                    .into_any_element(),
                TabKind::Runs => icon("activity01", 14.)
                    .text_color(theme::content())
                    .into_any_element(),
                TabKind::Branches => icon("git-branch", 14.)
                    .text_color(theme::content())
                    .into_any_element(),
                TabKind::GitHub => icon("github", 14.)
                    .text_color(theme::content())
                    .into_any_element(),
                TabKind::Cloudflare => icon("internet", 14.)
                    .text_color(theme::content())
                    .into_any_element(),
                #[cfg(feature = "team")]
                TabKind::Team => icon("ungroup-items", 14.)
                    .text_color(theme::content())
                    .into_any_element(),
                #[cfg(feature = "coderlabs")]
                TabKind::Site => icon("globe", 14.)
                    .text_color(theme::content())
                    .into_any_element(),
                TabKind::PostHog => icon("chart-breakout-square", 14.)
                    .text_color(theme::content())
                    .into_any_element(),
                TabKind::Phone => icon("signal-full02", 14.)
                    .text_color(theme::content())
                    .into_any_element(),
                TabKind::Servers => icon("computer", 14.)
                    .text_color(theme::content())
                    .into_any_element(),
                TabKind::Usage => icon("dashboard-square01", 14.)
                    .text_color(theme::content())
                    .into_any_element(),
                TabKind::Gallery => icon("paint-board", 14.)
                    .text_color(theme::content())
                    .into_any_element(),
                TabKind::SidebarBuilder => icon("layout-align-left", 14.)
                    .text_color(theme::content())
                    .into_any_element(),
                TabKind::Isometric => icon("cube", 14.)
                    .text_color(theme::content())
                    .into_any_element(),
                #[cfg(feature = "coderlabs")]
                TabKind::Canvas => icon("artboard", 14.)
                    .text_color(theme::content())
                    .into_any_element(),
                TabKind::Extensions => icon("store01", 14.)
                    .text_color(theme::content())
                    .into_any_element(),
            };
            let group: SharedString = format!("tab-{i}").into();
            list = list.child(
                ui::tab(("tab", i), active)
                    .group(group.clone())
                    // The close button keeps its size; only the air beside it scales.
                    .when(closable, |d| {
                        d.pr(crate::ui::sp(4.) + px(ui::TAB_CLOSE_ROOM))
                    })
                    // The tab on screen in the other pane.
                    .when(shown_beside, |d| {
                        d.border_1()
                            .border_color(theme::border_strong())
                            .text_color(theme::text_muted())
                    })
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .on_mouse_down(
                        MouseButton::Right,
                        cx.listener(move |this, ev: &gpui::MouseDownEvent, _, cx| {
                            cx.stop_propagation();
                            this.tab_menu(i, ev.position, cx)
                        }),
                    )
                    .child(
                        div()
                            .flex_none()
                            .when(!active, |d| d.opacity(0.55))
                            .child(tab_icon),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .when(tab.preview, |d| d.italic())
                            // A terminal's title is often `user@host:~/path`.
                            .child(crate::redact::text(&tab.title).into_owned()),
                    )
                    .when(closable, |d| {
                        d.child(
                            div()
                                .id(("tab-close", i))
                                .absolute()
                                .right(px(4.))
                                .top(px(ui::TAB_CLOSE_TOP))
                                .flex()
                                .items_center()
                                .justify_center()
                                .size(px(20.))
                                .rounded(crate::ui::rad(4.))
                                .invisible()
                                .group_hover(group.clone(), |s| s.visible())
                                .hover(|s| s.bg(theme::fill_selected()))
                                .child(icon("cancel01", 12.).text_color(theme::text_subtle()))
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    cx.stop_propagation();
                                    this.close_tab(i, window, cx)
                                })),
                        )
                    })
                    .on_click(cx.listener(move |this, ev: &gpui::ClickEvent, window, cx| {
                        this.select_tab(i, window, cx);
                        // A double click keeps a preview tab.
                        if ev.click_count() >= 2 {
                            this.pin_preview(cx);
                        }
                    }))
                    .on_drag(dragged, move |d, offset, _, cx| {
                        shell.update(cx, |this, cx| this.start_tab_drag(d, offset, cx))
                    })
                    .when(lifted == Some(tab.id), |d| d.opacity(0.4)),
            );
        }
        strip = strip.child(list);

        strip = strip.child(
            div()
                .flex()
                .flex_none()
                .gap(crate::ui::sp(2.))
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .child(
                    icon_button("tab-new-session", "add01", 14.)
                        .tooltip(ui::tip("New session (Ctrl+T)"))
                        .on_click(cx.listener(|this, _, window, cx| this.new_session(window, cx))),
                )
                .child(
                    icon_button("tab-new-browser", "globe", 14.)
                        .tooltip(ui::tip("New browser tab (Ctrl+Shift+B)"))
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.open_browser_tab(String::new(), window, cx)
                        })),
                )
                .child(
                    icon_button(
                        "tab-split",
                        if in_other_pane.is_some() {
                            "square"
                        } else {
                            "layout-two-column"
                        },
                        14.,
                    )
                    .tooltip(ui::tip(if in_other_pane.is_some() {
                        "Close split"
                    } else {
                        "Split right (Ctrl+D)"
                    }))
                    .on_click(cx.listener(|this, _, window, cx| {
                        if this.env().split_other().is_some() {
                            this.close_split(cx)
                        } else {
                            this.split_with(None, model::SplitDir::Right, window, cx)
                        }
                    })),
                ),
        );

        let maximized = window.is_maximized();

        div()
            .flex()
            .flex_none()
            .items_center()
            .h(px(40.))
            // Windows moves the window through the native hit test (HTCAPTION):
            // `start_window_move` does nothing there. Whatever in the bar takes a
            // click stops the mousedown, or the press would start a move instead.
            .when(cfg!(windows), |d| {
                d.window_control_area(WindowControlArea::Drag)
            })
            .child(strip)
            // Empty drag space; the project and branch already show in the rail and
            // dock. On Linux this is the only part of the bar that moves the
            // window, and it keeps a floor so a crowded tab strip never leaves
            // the window unmovable.
            .child(
                div()
                    .flex_1()
                    .min_w(px(48.))
                    .h_full()
                    .on_mouse_down(MouseButton::Left, |ev, window, _| {
                        crate::ui::drag_window(ev, window)
                    }),
            )
            // Whoever shares the screen sees at a glance that it is safe to.
            .when(crate::redact::on(), |d| {
                d.child(
                    div()
                        .id("title-redacted")
                        .flex_none()
                        .mr(crate::ui::sp(6.))
                        .cursor_pointer()
                        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                        .tooltip(ui::tip(
                            "Redacted mode: emails, keys and codes are hidden. Click or Ctrl+Shift+H to show them.",
                        ))
                        .on_click(cx.listener(|this, _, _, cx| this.toggle_redacted(cx)))
                        .child(ui::state_pill("view-off", "Redacted", theme::attention())),
                )
            })
            // macOS draws its own traffic lights at the left.
            .when(cfg!(not(target_os = "macos")), |d| {
                d.child(
                    div()
                        .flex()
                        .flex_none()
                        .items_center()
                        .when(cfg!(windows), |d| {
                            d.h_full().border_l_1().border_color(theme::stroke())
                        })
                        .when(cfg!(not(windows)), |d| {
                            d.gap(crate::ui::sp(2.))
                                .pr(crate::ui::sp(8.))
                                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                        })
                        .child(
                            window_button("win-min", "minus-sign", 14., false)
                                .on_click(|_, window, _| window.minimize_window()),
                        )
                        .child(max_button(maximized))
                        .child(
                            window_button("win-close", "cancel01", 14., true)
                                .on_click(|_, _, cx| cx.quit()),
                        ),
                )
            })
    }
}

/// On Windows the caption buttons are left to the native hit test: a mousedown
/// that reaches gpui unhandled arms Windows' own button tracking, which also
/// gives Windows 11's snap layouts on the maximize button. Linux stops it so a
/// click doesn't start a window move.
pub(super) fn caption_buttons() -> gpui::Div {
    div().flex().h_full().when(cfg!(not(windows)), |d| {
        d.on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
    })
}

/// gpui's `zoom_window` only maximizes on Windows, so there the native
/// HTMAXBUTTON path toggles maximize and restore instead.
pub(super) fn max_button(maximized: bool) -> gpui::Stateful<gpui::Div> {
    window_button(
        "win-max",
        if maximized { "copy01" } else { "square" },
        12.,
        false,
    )
    .when(cfg!(not(windows)), |d| {
        d.on_click(|_, window, _| window.zoom_window())
    })
}

pub(super) fn window_button(
    id: &'static str,
    icon_name: &str,
    size: f32,
    close: bool,
) -> gpui::Stateful<gpui::Div> {
    div()
        .id(id)
        .group(id)
        .flex()
        .items_center()
        .justify_center()
        .cursor_pointer()
        .when(cfg!(windows), |d| d.w(px(40.)).h_full())
        .when(cfg!(not(windows)), |d| {
            d.size(px(28.)).rounded(crate::ui::rad(8.))
        })
        // The hit test stops here, so Windows sees the button and not the
        // title bar's drag area around it.
        .when(cfg!(windows), |d| {
            d.occlude().window_control_area(match id {
                "win-min" => WindowControlArea::Min,
                "win-max" => WindowControlArea::Max,
                _ => WindowControlArea::Close,
            })
        })
        .map(|d| {
            if close {
                d.hover(|s| s.bg(theme::danger()))
            } else {
                d.hover(|s| s.bg(theme::fill_selected()))
            }
        })
        .child(hover_icon(
            icon_name,
            size,
            theme::text_subtle(),
            if close {
                gpui::white()
            } else {
                theme::content()
            },
            id,
        ))
}
