//! The title bar: back and forward, the one row of tabs and the window buttons.

use super::*;

impl Shell {
    pub(super) fn render_titlebar(
        &self,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let a11y = window.is_a11y_active();
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
                (
                    "title-panel",
                    "layout-align-right",
                    self.panel_shown(),
                    false,
                ),
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
        // Back/forward through the tab history; the rail has
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
        let strip_tabs = &self.strip;
        let layout = crate::tab_layout::current();
        // Pinned tabs carry their project's icon, so no run of tabs needs a label.
        let pinned_bar = layout.pinned_bar();
        // While the sidebar is hidden, its tree is a menu here.
        if layout.tree() && !self.rail_shown() {
            strip = strip.child(self.render_sessions_button(cx));
        }
        let in_other_pane = self.split_other();
        let shell = cx.entity();
        let lifted = self.lifted_tab(cx);
        let session_lifted = self.lifted_session(cx).is_some();
        // The line between two tabs where a dragged tab or session would land;
        // none where a tab would land in its own place.
        let drop_gap = if session_lifted {
            self.tab_drop(cx)
        } else {
            lifted
                .and_then(|id| Some((strip_tabs.position(id)?, self.tab_drop(cx)?)))
                .filter(|(from, gap)| model::drop_index(*from, *gap).is_some())
                .map(|(_, gap)| gap)
        };
        let here = self.projects.get(self.active_project);
        let mut order = strip_tabs.shown(layout.bar_shows, |t| {
            here.is_some_and(|p| t.env.as_deref().is_some_and(|e| p.env_index(e).is_some()))
        });
        // Pinned tabs fit the bar as measured last frame: left of the island,
        // then right of it, narrower, then under "N more".
        let area = self.bar_area.get();
        let (left_room, right_room) = pinned::split_room(
            (area.origin.x.into(), area.size.width.into()),
            self.bar_notch(window),
        );
        let fit = (pinned_bar && area.size.width > px(0.)).then(|| {
            let fit = pinned::fit_tabs(order.len(), left_room, right_room);
            pinned::bring_into_view(&mut order, fit.shown(), strip_tabs.active_tab);
            fit
        });
        let more: Vec<u64> = match fit {
            Some(fit) if fit.shown() < order.len() => order
                .split_off(fit.shown())
                .into_iter()
                .map(|i| strip_tabs.tabs[i].id)
                .collect(),
            _ => Vec::new(),
        };
        *self.bar_more.borrow_mut() = more.clone();
        // The space the island takes, after the tabs left of it and "N more"
        // when it goes there.
        let notch_gap = fit.zip(right_room).map(|(fit, right)| {
            let more = if fit.more_left && !more.is_empty() {
                pinned::MORE_W + 2.
            } else {
                0.
            };
            let used = fit.left as f32 * (fit.width + 2.) + more;
            (f32::from(area.size.width) - right - used - 2.).max(0.)
        });
        let mut more_button = (!more.is_empty()).then(|| {
            let n = more.len();
            div()
                .id("tab-more")
                .map(|d| ui::labelled(d, gpui::Role::Button, format!("{n} more pinned tabs")))
                .flex()
                .flex_none()
                .items_center()
                .gap(crate::ui::sp(4.))
                .h(px(ui::TAB_H))
                .min_w(px(pinned::MORE_W))
                .px(crate::ui::sp(8.))
                .rounded(crate::ui::rad(8.))
                .cursor_pointer()
                .text_size(px(ui::TEXT_MD))
                .text_color(theme::text_subtle())
                .hover(|s| s.bg(theme::hover()).text_color(theme::content()))
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .child(format!("{n} more"))
                .child(icon("arrow-down01", 12.))
                .on_click(cx.listener(|this, ev: &gpui::ClickEvent, _, cx| {
                    let at = ev.position();
                    this.menu = Some(ContextMenu {
                        target: MenuTarget::MoreTabs,
                        at: point(at.x - px(120.), px(44.)),
                    });
                    cx.notify();
                }))
                .into_any_element()
        });

        // Tabs past the bar's width scroll instead of running under the title and the
        // window buttons. Pinned tabs fit instead, and the part of the bar
        // they leave empty moves the window.
        let bar_area = self.bar_area.clone();
        let mut list = div()
            .id("tab-list")
            .role(gpui::Role::TabList)
            .relative()
            .flex()
            .items_center()
            .min_w_0()
            .h_full()
            .gap(crate::ui::sp(2.))
            .overflow_x_scroll()
            .track_scroll(&self.tab_scroll)
            .on_drag_move::<drag::DraggedTab>(cx.listener(
                |this, ev: &gpui::DragMoveEvent<drag::DraggedTab>, _, cx| {
                    this.bar_drag_out(ev.bounds, ev.event.position, cx)
                },
            ))
            .when(pinned_bar, |d| {
                d.flex_1()
                    .on_mouse_down(MouseButton::Left, |ev, window, _| {
                        crate::ui::drag_window(ev, window)
                    })
                    .child(
                        canvas(
                            move |b, window, _| {
                                if bar_area.get() != b {
                                    bar_area.set(b);
                                    window.refresh();
                                }
                            },
                            |_, _, _, _| {},
                        )
                        .absolute()
                        .size_full(),
                    )
                    // A session row dropped on the bar pins, at the end
                    // unless a gap is marked.
                    .on_drag_move::<drag::DraggedSession>(cx.listener(
                        |this, ev: &gpui::DragMoveEvent<drag::DraggedSession>, _, cx| {
                            this.bar_drag_out(ev.bounds, ev.event.position, cx)
                        },
                    ))
                    .drag_over::<drag::DraggedSession>(|s, _, _, _| s.bg(theme::hover()))
                    .on_drop(cx.listener(|this, d: &drag::DraggedSession, _, cx| {
                        this.drop_session_on_bar(d, None, cx)
                    }))
            });
        let active_id = strip_tabs.active().map(|t| t.id);
        if self.tab_revealed.replace(active_id) != active_id
            && let Some(pos) = order.iter().position(|&i| i == strip_tabs.active_tab)
        {
            self.tab_scroll.scroll_to_item(pos);
        }
        // With tabs of several environments in the row, the first tab of each
        // run says which environment the run works in.
        let mut envs = order
            .iter()
            .filter_map(|&i| strip_tabs.tabs[i].env.as_ref());
        let labelled = !layout.tree()
            && envs
                .next()
                .is_some_and(|first| envs.any(|other| other != first));
        let projects = self
            .projects
            .iter()
            .filter(|p| {
                strip_tabs
                    .tabs
                    .iter()
                    .any(|t| t.env.as_deref().is_some_and(|e| p.env_index(e).is_some()))
            })
            .count();
        let shown = order.len();
        for (pos, &i) in order.iter().enumerate() {
            let tab = &strip_tabs.tabs[i];
            // The tab shown after this one: a gap right of this tab is the one
            // before it.
            let next = order.get(pos + 1).copied().unwrap_or(i + 1);
            let last = pos + 1 == shown;
            if let Some(gap) = notch_gap.filter(|_| fit.is_some_and(|f| f.left == pos)) {
                if fit.is_some_and(|f| f.more_left) {
                    list = list.children(more_button.take());
                }
                list = list.child(div().flex_none().w(px(gap)));
            }
            let starts_run = labelled
                && pos
                    .checked_sub(1)
                    .is_none_or(|b| strip_tabs.tabs[order[b]].env != tab.env);
            let run_label: Option<SharedString> =
                tab.env
                    .as_deref()
                    .filter(|_| starts_run)
                    .map(|path| match self.find_env(path) {
                        Some(env) if projects < 2 => env.name.clone(),
                        _ => self.env_label(path).into(),
                    });
            let dragged = drag::DraggedTab::new(tab.id, tab.title.clone());
            let shell = shell.clone();
            let active = i == strip_tabs.active_tab;
            let shown_beside = in_other_pane == Some(i);
            // A pinned tab can always be closed: that unpins it.
            let closable = self.can_close_tab(i) || (pinned_bar && tab.pinned);
            let tab_icon = self.tab_lead(tab, cx);
            let group: SharedString = format!("tab-{i}").into();
            // One child per tab, so `tab_scroll` finds a tab by its index.
            let mut slot = div()
                .flex()
                .flex_none()
                .items_center()
                .gap(crate::ui::sp(2.));
            if starts_run && pos > 0 {
                slot = slot.child(
                    div()
                        .flex_none()
                        .w(px(1.))
                        .h(px(16.))
                        .mx(px(4.))
                        .bg(theme::fill_selected()),
                );
            }
            if let Some(label) = run_label {
                slot = slot.child(
                    div()
                        .flex_none()
                        .max_w(px(140.))
                        .truncate()
                        .px(crate::ui::sp(4.))
                        .text_size(px(ui::TEXT_XS))
                        .text_color(theme::text_faint())
                        .child(crate::redact::text(&label).into_owned()),
                );
            }
            let title = crate::redact::text(&tab.title).into_owned();
            let close_label = format!("Close {title}");
            list = list.child(
                slot.child(
                    ui::tab(("tab", i), title, active)
                        .group(group.clone())
                        .when_some(fit, |d, fit| {
                            d.flex_none()
                                .w(px(fit.width))
                                .min_w(px(fit.width))
                                .max_w(px(fit.width))
                        })
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
                        // A middle click closes the tab, as in a browser.
                        .when(closable, |d| {
                            d.on_mouse_down(MouseButton::Middle, |_, _, cx| cx.stop_propagation())
                                .on_mouse_up(
                                    MouseButton::Middle,
                                    cx.listener(move |this, _, window, cx| {
                                        cx.stop_propagation();
                                        this.close_tab(i, window, cx)
                                    }),
                                )
                        })
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
                                    .map(|d| {
                                        ui::labelled(d, gpui::Role::Button, close_label.clone())
                                    })
                                    .absolute()
                                    .right(px(4.))
                                    .top(px(ui::TAB_CLOSE_TOP))
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .size(px(20.))
                                    .rounded(crate::ui::rad(4.))
                                    // Hidden elements register no click listeners,
                                    // so keep it pressable for a screen reader.
                                    .when(!a11y, |d| {
                                        d.invisible().group_hover(group.clone(), |s| s.visible())
                                    })
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
                        .on_drag(dragged, move |d, _, _, cx| {
                            shell.update(cx, |this, cx| this.start_tab_drag(d, cx))
                        })
                        // A dragged tab or session row marks the gap it would
                        // land in, on the side of this tab's middle the
                        // pointer is on.
                        .on_drag_move::<drag::DraggedTab>(cx.listener(
                            move |this, ev: &gpui::DragMoveEvent<drag::DraggedTab>, _, cx| {
                                this.bar_drag_over(i, next, ev.bounds, ev.event.position, cx)
                            },
                        ))
                        .on_drop(cx.listener(move |this, d: &drag::DraggedTab, _, cx| {
                            this.drop_tab_at(d.id, i, cx)
                        }))
                        .when(pinned_bar, |d| {
                            d.on_drag_move::<drag::DraggedSession>(cx.listener(
                                move |this,
                                      ev: &gpui::DragMoveEvent<drag::DraggedSession>,
                                      _,
                                      cx| {
                                    this.bar_drag_over(i, next, ev.bounds, ev.event.position, cx)
                                },
                            ))
                            .on_drop(cx.listener(
                                move |this, d: &drag::DraggedSession, _, cx| {
                                    cx.stop_propagation();
                                    this.drop_session_on_bar(d, Some(i), cx)
                                },
                            ))
                        })
                        .when(drop_gap == Some(i), |d| d.child(drop_line(true, pos == 0)))
                        .when(drop_gap == Some(i + 1) && last, |d| {
                            d.child(drop_line(false, true))
                        })
                        .when(lifted == Some(tab.id), |d| d.opacity(0.4)),
                ),
            );
        }
        // No tab right of the island: "N more" still keeps clear of it.
        if let Some(gap) = notch_gap.filter(|_| fit.is_some_and(|f| f.left >= shown)) {
            if fit.is_some_and(|f| f.more_left) {
                list = list.children(more_button.take());
            } else if more_button.is_some() {
                list = list.child(div().flex_none().w(px(gap)));
            }
        }
        list = list.children(more_button.take());
        strip = strip.when(pinned_bar, |d| d.flex_1()).child(list);

        strip = strip.child(
            div()
                .flex()
                .flex_none()
                .gap(crate::ui::sp(2.))
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .child(
                    icon_button("tab-new-session", "add01", 14., "New session (Ctrl+T)")
                        .on_click(cx.listener(|this, _, window, cx| this.new_session(window, cx))),
                )
                .child(
                    icon_button(
                        "tab-new-browser",
                        "globe",
                        14.,
                        "New browser tab (Ctrl+Shift+B)",
                    )
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
                        if in_other_pane.is_some() {
                            "Close split"
                        } else {
                            "Split right (Ctrl+D)"
                        },
                    )
                    .on_click(cx.listener(|this, _, window, cx| {
                        if this.split_other().is_some() {
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
                    // Pinned tabs take the bar; the empty part of it moves the window too.
                    .when(crate::tab_layout::current().pinned_bar(), |d| d.flex_none())
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
                                .on_click(|_, window, cx| crate::background::close(window, cx)),
                        ),
                )
            })
    }

    /// The icon a tab starts with: its kind's, or for a session with
    /// sessions in the sidebar, its state's mark and its project's icon.
    pub(super) fn tab_lead(&self, tab: &Tab, cx: &App) -> AnyElement {
        let layout = crate::tab_layout::current();
        if layout.tree()
            && let Some(state) = self.tab_state(tab, cx)
        {
            return div()
                .flex()
                .items_center()
                .gap(crate::ui::sp(6.))
                .children(state.mark(layout.state))
                .children(self.tab_project_icon(tab))
                .into_any_element();
        }
        self.tab_icon(tab, cx)
    }

    /// The icon of the project a tab works in.
    pub(super) fn tab_project_icon(&self, tab: &Tab) -> Option<AnyElement> {
        let env = tab.env.as_deref()?;
        let p = self.projects.iter().find(|p| p.env_index(env).is_some())?;
        let logo: gpui::Hsla = gpui::rgb(p.logo).into();
        Some(
            div()
                .flex()
                .flex_none()
                .items_center()
                .justify_center()
                .size(px(14.))
                .child(ui::project_icon(p.icon, 1.25, logo))
                .into_any_element(),
        )
    }

    /// The icon of a tab's kind.
    pub(super) fn tab_icon(&self, tab: &Tab, cx: &App) -> AnyElement {
        match &tab.kind {
            TabKind::Session { agent, .. } => agent_icon(*agent, 14.).into_any_element(),
            TabKind::NewSession => agent_icon(self.tab_env(tab).agent, 14.).into_any_element(),
            TabKind::Browser { url } => {
                let page = tab.browser.as_ref().map(|b| b.view.read(cx));
                // A tab whose page is not running, or has no icon yet, shows the icon its
                // site had last time.
                let favicon = page
                    .and_then(|p| p.favicon())
                    .or_else(|| crate::browser::favicons::cached(url, cx));
                match (page.is_some_and(|p| p.is_loading()), favicon) {
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
            TabKind::Doop => icon("paint-board", 14.)
                .text_color(theme::content())
                .into_any_element(),
            TabKind::Phone => icon("signal-full02", 14.)
                .text_color(theme::content())
                .into_any_element(),
            TabKind::Servers => icon("computer", 14.)
                .text_color(theme::content())
                .into_any_element(),
            TabKind::Teams => icon("ungroup-items", 14.)
                .text_color(theme::content())
                .into_any_element(),
            TabKind::Usage => icon("dashboard-square01", 14.)
                .text_color(theme::content())
                .into_any_element(),
            TabKind::Gallery => icon("paint-board", 14.)
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
        }
    }
}

/// Where a dragged tab would land: a line on the left edge of the tab after
/// the gap, or on the right edge of the last tab. Between two tabs it sits in
/// the gap; at an end of the bar, inside the tab, as the list clips past it.
pub(super) fn drop_line(left: bool, end: bool) -> gpui::Div {
    let at = px(if end { 0. } else { -2. });
    div()
        .absolute()
        .top(px(4.))
        .bottom(px(4.))
        .w(px(2.))
        .rounded_full()
        .bg(theme::active())
        .map(|d| if left { d.left(at) } else { d.right(at) })
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
