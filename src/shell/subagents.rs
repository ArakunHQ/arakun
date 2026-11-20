//! The Subagents panel: the subagents of the session on screen, beside the
//! chat, and the transcript of the one opened. It sits between the chat and
//! the Workspace panel, with its own width and its own open flag.

use super::*;
use crate::agent::{SubagentState, Thread, ThreadView, ThreadViewEvent};
use crate::ui::{Tone, Type, Typography as _};

/// A subagent's transcript open in the panel.
pub(super) struct SubagentView {
    /// The session tab it belongs to.
    session: SharedString,
    id: String,
    view: Entity<ThreadView>,
    _events: Subscription,
}

/// What a row of the list shows.
struct Row {
    id: String,
    name: String,
    task: String,
    state: SubagentState,
    secs: u64,
}

fn tone(state: SubagentState) -> Tone {
    match state {
        SubagentState::Running => Tone::Info,
        SubagentState::Completed => Tone::Success,
        SubagentState::Failed => Tone::Danger,
        SubagentState::Cancelled => Tone::Neutral,
    }
}

fn fmt_secs(secs: u64) -> String {
    match secs {
        s if s < 60 => format!("{s}s"),
        s if s < 3600 => format!("{}m {:02}s", s / 60, s % 60),
        s => format!("{}h {:02}m", s / 3600, s % 3600 / 60),
    }
}

impl Shell {
    pub(super) fn toggle_subagents(&mut self, cx: &mut Context<Self>) {
        // Left out for room, a click brings it back in the panel's place.
        if self.subagents_open && self.side_crowded.get()[1] {
            self.side_prefer = SideCard::Subagents;
            cx.notify();
            return;
        }
        self.subagents_open = !self.subagents_open;
        self.subagents_dismissed = !self.subagents_open;
        if self.subagents_open {
            self.side_prefer = SideCard::Subagents;
        }
        self.save(cx);
        self.sync_subagent_tick(cx);
        cx.notify();
    }

    /// Opens the panel on the list, or on `subagent` of session `session`.
    pub(super) fn open_subagent(
        &mut self,
        session: SharedString,
        subagent: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.subagents_open {
            self.subagents_open = true;
            self.save(cx);
        }
        self.side_prefer = SideCard::Subagents;
        match subagent {
            Some(id) => self.show_subagent(session, &id, window, cx),
            None => self.subagent_view = None,
        }
        self.sync_subagent_tick(cx);
        cx.notify();
    }

    /// A new subagent in a session: the panel opens for the one on screen,
    /// unless the user closed it.
    pub(super) fn subagent_spawned(&mut self, session: &str, cx: &mut Context<Self>) {
        let active = self
            .active_session_id()
            .is_some_and(|a| a.as_ref() == session);
        if active && !self.subagents_open && !self.subagents_dismissed {
            self.subagents_open = true;
            self.save(cx);
        }
        self.sync_subagent_tick(cx);
        cx.notify();
    }

    fn session_thread(&self, session: &str) -> Option<Entity<Thread>> {
        let env = self.env();
        env.sessions[env.session_index(session)?]
            .live
            .as_ref()
            .map(|l| l.thread.clone())
    }

    fn show_subagent(
        &mut self,
        session: SharedString,
        id: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(thread) = self.session_thread(&session) else {
            return;
        };
        let Some((id, child)) = thread
            .read(cx)
            .subagent(id)
            .map(|s| (s.id.clone(), s.thread.clone()))
        else {
            return;
        };
        if self
            .subagent_view
            .as_ref()
            .is_some_and(|v| v.session == session && v.id == id)
        {
            return;
        }
        let view = cx.new(|cx| ThreadView::new(child, window, cx));
        let (env, sid) = (self.env().path.clone(), session.clone());
        let events = cx.subscribe_in(
            &view,
            window,
            move |this, _, ev: &ThreadViewEvent, window, cx| match ev {
                ThreadViewEvent::OpenFile(file, line) => {
                    this.open_file_beside(&env, file, *line, window, cx)
                }
                ThreadViewEvent::OpenSubagent(id) => {
                    this.open_subagent(sid.clone(), id.clone(), window, cx)
                }
                ThreadViewEvent::AddNote { .. } => {}
            },
        );
        self.subagent_view = Some(SubagentView {
            session,
            id,
            view,
            _events: events,
        });
    }

    /// Keeps the elapsed times of running subagents counting while the panel shows them.
    pub(super) fn sync_subagent_tick(&mut self, cx: &mut Context<Self>) {
        if self.subagent_tick.is_some() || !self.subagents_running(cx) {
            return;
        }
        self.subagent_tick = Some(cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(Duration::from_secs(1)).await;
                let going = this.update(cx, |this, cx| {
                    let going = this.subagents_running(cx);
                    if going {
                        cx.notify();
                    } else {
                        this.subagent_tick = None;
                    }
                    going
                });
                if !matches!(going, Ok(true)) {
                    break;
                }
            }
        }));
    }

    fn subagents_running(&self, cx: &App) -> bool {
        self.subagents_open
            && self.has_projects()
            && self
                .active_thread(cx)
                .is_some_and(|t| t.read(cx).running_subagents() > 0)
    }

    pub(super) fn render_subagents(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let session = self.active_session_id();
        let thread = self.active_thread(cx);
        let opened = self
            .subagent_view
            .as_ref()
            .filter(|v| Some(&v.session) == session.as_ref())
            .and_then(|v| {
                let t = thread.as_ref()?.read(cx);
                let s = t.subagent(&v.id)?;
                Some((v.view.clone(), s.name.clone(), s.state, s.elapsed_secs()))
            });

        let header = div()
            .flex()
            .flex_none()
            .items_center()
            .h(px(40.))
            .pl(crate::ui::sp(if opened.is_some() { 6. } else { 12. }))
            .pr(crate::ui::sp(6.))
            .gap(crate::ui::sp(6.))
            .map(|d| match &opened {
                Some((_, name, state, _)) => d
                    .child(
                        icon_button("subagent-back", "arrow-left01", 14., "All subagents")
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.subagent_view = None;
                                cx.notify();
                            })),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_size(px(ui::TEXT_LG))
                            .font_weight(FontWeight::MEDIUM)
                            .child(name.clone()),
                    )
                    .child(ui::badge(state.label(), tone(*state))),
                None => {
                    let count = thread.as_ref().map_or(0, |t| t.read(cx).subagents().len());
                    d.child(
                        div()
                            .text_size(px(ui::TEXT_LG))
                            .font_weight(FontWeight::MEDIUM)
                            .child("Subagents"),
                    )
                    .when(count > 0, |d| {
                        d.child(ui::text(
                            Type::Caption,
                            theme::text_subtle(),
                            count.to_string(),
                        ))
                    })
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
                }
            })
            .child(
                icon_button("subagents-close", "cancel01", 14., "Close subagents panel")
                    .on_click(cx.listener(|this, _, _, cx| this.toggle_subagents(cx))),
            );

        let body: AnyElement = match (&opened, &thread) {
            (Some((view, ..)), _) => gpui::AnyView::from(view.clone())
                .cached(gpui::StyleRefinement::default().size_full())
                .into_any_element(),
            (None, None) => ui::empty_state(
                "bot",
                "No session",
                "Open a session to see the subagents it runs.",
                None,
            )
            .into_any_element(),
            (None, Some(thread)) => {
                // Newest first: the one just started is the one to look at.
                let rows: Vec<Row> = thread
                    .read(cx)
                    .subagents()
                    .iter()
                    .rev()
                    .map(|s| Row {
                        id: s.id.clone(),
                        name: s.name.clone(),
                        task: s
                            .task
                            .lines()
                            .find(|l| !l.trim().is_empty())
                            .unwrap_or("")
                            .trim()
                            .to_string(),
                        state: s.state,
                        secs: s.elapsed_secs(),
                    })
                    .collect();
                if rows.is_empty() {
                    ui::empty_state(
                        "bot",
                        "No subagents yet",
                        "When the agent hands work to a subagent, it shows here with its own transcript while you go on.",
                        None,
                    )
                    .into_any_element()
                } else {
                    let sid = session.clone().unwrap_or_default();
                    div()
                        .id("subagent-list")
                        .flex()
                        .flex_col()
                        .gap(crate::ui::sp(2.))
                        .p(crate::ui::sp(6.))
                        .overflow_y_scroll()
                        .children(
                            rows.into_iter()
                                .enumerate()
                                .map(|(i, row)| self.subagent_row(i, row, &sid, cx)),
                        )
                        .into_any_element()
                }
            }
        };

        self.side_card()
            .id("subagents-card")
            .map(ui::stop_window_move)
            .flex()
            .flex_col()
            .flex_none()
            .w(px(self.subagents_width()))
            .h_full()
            .child(header)
            .child(div().flex_1().min_h_0().child(body))
    }

    fn subagent_row(
        &self,
        i: usize,
        row: Row,
        session: &SharedString,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let Row {
            id,
            name,
            task,
            state,
            secs,
        } = row;
        let session = session.clone();
        let running = state.is_running();
        div()
            .id(("subagent-row", i))
            .flex()
            .items_start()
            .gap(crate::ui::sp(8.))
            .px(crate::ui::sp(8.))
            .py(crate::ui::sp(8.))
            .rounded(crate::ui::rad(8.))
            .cursor_pointer()
            .hover(|d| d.bg(theme::fill_hover()))
            .child(
                div().flex_none().pt(crate::ui::sp(6.)).child(
                    div()
                        .size(px(7.))
                        .rounded_full()
                        .bg(tone(state).color())
                        .when(!running, |d| d.opacity(0.6)),
                ),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_w_0()
                    .gap(crate::ui::sp(2.))
                    .child(
                        div()
                            .min_w_0()
                            .truncate()
                            .ty(Type::Label)
                            .text_color(theme::text())
                            .child(name.clone()),
                    )
                    .when(!task.is_empty() && task != name, |d| {
                        d.child(
                            div()
                                .min_w_0()
                                .truncate()
                                .ty(Type::Caption)
                                .text_color(theme::text_subtle())
                                .child(task),
                        )
                    }),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_none()
                    .items_end()
                    .gap(crate::ui::sp(2.))
                    .child(ui::badge(state.label(), tone(state)))
                    .child(ui::text(Type::Caption, theme::text_faint(), fmt_secs(secs))),
            )
            .on_click(cx.listener(move |this, _, window, cx| {
                this.open_subagent(session.clone(), Some(id.clone()), window, cx)
            }))
            .into_any_element()
    }
}
