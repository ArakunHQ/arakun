//! What a session row or tab offers on right-click (rename, fork, compact,
//! remove from the list) and importing the sessions Droid already has for an
//! environment.

use std::collections::HashSet;
use std::path::PathBuf;

use gpui::prelude::FluentBuilder as _;
use gpui::{
    App, AppContext as _, Context, Entity, EventEmitter, FocusHandle, Focusable,
    InteractiveElement as _, IntoElement, ParentElement as _, Render, SharedString,
    StatefulInteractiveElement as _, Styled as _, Subscription, Window, div, px,
};

use super::{ContextMenu, MenuTarget, Modal, Shell};
use crate::agent::catalog::{self, AgentSession};
use crate::input::{InputEvent, TextInput};
use crate::model::{self, Agent};
use crate::theme;
use crate::ui::{self, ButtonKind, icon};

pub enum SessionDialogEvent {
    Rename(String),
    Remove,
    Import(Vec<AgentSession>),
    Dismissed,
}

// ---------------------------------------------------------------------------

pub struct RenameDialog {
    input: Entity<TextInput>,
    _sub: Subscription,
}

impl EventEmitter<SessionDialogEvent> for RenameDialog {}

impl RenameDialog {
    pub fn new(title: &str, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let input = cx.new(|cx| {
            let mut i = TextInput::new("Session title", cx);
            i.set_text(title.to_string(), cx);
            i.select_all_text(cx);
            i
        });
        let sub = cx.subscribe_in(&input, window, |this: &mut Self, _, ev, _, cx| match ev {
            InputEvent::Submit => this.submit(cx),
            InputEvent::Cancel => cx.emit(SessionDialogEvent::Dismissed),
            InputEvent::Changed => cx.notify(),
            _ => {}
        });
        window.focus(&input.focus_handle(cx), cx);
        Self { input, _sub: sub }
    }

    fn title(&self, cx: &App) -> String {
        self.input.read(cx).text().trim().to_string()
    }

    fn submit(&mut self, cx: &mut Context<Self>) {
        let title = self.title(cx);
        if !title.is_empty() {
            cx.emit(SessionDialogEvent::Rename(title));
        }
    }
}

impl Focusable for RenameDialog {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.input.focus_handle(cx)
    }
}

impl Render for RenameDialog {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let ready = !self.title(cx).is_empty();
        ui::modal_panel("rename-session", 420.)
            .child(ui::modal_header(
                "Rename session",
                None,
                ui::close_button("rename-session-close")
                    .on_click(cx.listener(|_, _, _, cx| cx.emit(SessionDialogEvent::Dismissed))),
            ))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(crate::ui::sp(16.))
                    .p(crate::ui::sp(16.))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .h(px(ui::INPUT_H))
                            .px(crate::ui::sp(10.))
                            .rounded(crate::ui::rad(ui::RADIUS))
                            .border_1()
                            .border_color(theme::border_strong())
                            .bg(theme::background())
                            .text_size(px(ui::TEXT_MD))
                            .line_height(px(20.))
                            .child(self.input.clone()),
                    )
                    .child(
                        div()
                            .flex()
                            .justify_end()
                            .gap(crate::ui::sp(8.))
                            .child(
                                ui::button(
                                    "rename-session-cancel",
                                    "Cancel",
                                    ButtonKind::Ghost,
                                    true,
                                )
                                .on_click(
                                    cx.listener(|_, _, _, cx| {
                                        cx.emit(SessionDialogEvent::Dismissed)
                                    }),
                                ),
                            )
                            .child(
                                ui::button(
                                    "rename-session-submit",
                                    "Rename",
                                    ButtonKind::Primary,
                                    ready,
                                )
                                .on_click(cx.listener(|this, _, _, cx| this.submit(cx))),
                            ),
                    ),
            )
    }
}

// ---------------------------------------------------------------------------

pub struct RemoveDialog {
    title: SharedString,
    agent: Agent,
    busy: bool,
    focus: FocusHandle,
}

impl EventEmitter<SessionDialogEvent> for RemoveDialog {}

impl RemoveDialog {
    pub fn new(
        title: SharedString,
        agent: Agent,
        busy: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let focus = cx.focus_handle();
        window.focus(&focus, cx);
        Self {
            title,
            agent,
            busy,
            focus,
        }
    }
}

impl Focusable for RemoveDialog {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for RemoveDialog {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let kept = match self.agent {
            Agent::Droid => "Droid keeps its own copy, so it can be imported again later.",
            Agent::Claude => "Claude Code keeps its own copy of the conversation.",
            Agent::OpenCode => "OpenCode keeps its own copy of the conversation.",
            Agent::WorkersAi => "arakun keeps the conversation the model saw until then.",
        };
        ui::modal_panel("remove-session", 420.)
            .track_focus(&self.focus)
            .on_key_down(cx.listener(|_, ev: &gpui::KeyDownEvent, _, cx| match ev.keystroke.key.as_str() {
                "escape" => cx.emit(SessionDialogEvent::Dismissed),
                "enter" => cx.emit(SessionDialogEvent::Remove),
                _ => {}
            }))
            .child(ui::modal_header(
                "Remove session from Arakun?",
                None,
                ui::close_button("remove-session-close")
                    .on_click(cx.listener(|_, _, _, cx| cx.emit(SessionDialogEvent::Dismissed))),
            ))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(crate::ui::sp(14.))
                    .p(crate::ui::sp(16.))
                    .text_size(px(ui::TEXT_MD))
                    .line_height(px(19.))
                    .child(
                        div()
                            .p(crate::ui::sp(12.))
                            .rounded(crate::ui::rad(12.))
                            .border_1()
                            .border_color(theme::border())
                            .bg(theme::fill_hover())
                            .text_color(theme::content())
                            .child(self.title.clone()),
                    )
                    .child(div().text_color(theme::text_muted()).child(format!(
                        "It leaves the sessions list and Arakun's saved transcript is deleted. {kept}"
                    )))
                    .when(self.busy, |d| {
                        d.child(
                            div()
                                .text_color(theme::warn())
                                .child("The agent is still working; removing stops it."),
                        )
                    })
                    .child(
                        div()
                            .flex()
                            .justify_end()
                            .gap(crate::ui::sp(8.))
                            .child(
                                ui::button("remove-session-cancel", "Cancel", ButtonKind::Ghost, true)
                                    .on_click(cx.listener(|_, _, _, cx| cx.emit(SessionDialogEvent::Dismissed))),
                            )
                            .child(
                                ui::button("remove-session-submit", "Remove", ButtonKind::Danger, true)
                                    .on_click(cx.listener(|_, _, _, cx| cx.emit(SessionDialogEvent::Remove))),
                            ),
                    ),
            )
    }
}

// ---------------------------------------------------------------------------

pub struct ImportDialog {
    path: PathBuf,
    loading: bool,
    found: Vec<AgentSession>,
    picked: HashSet<usize>,
    focus: FocusHandle,
}

impl EventEmitter<SessionDialogEvent> for ImportDialog {}

impl ImportDialog {
    /// Lists the agents' sessions for `path`, minus those in `known` (their session ids).
    pub fn new(
        path: PathBuf,
        known: HashSet<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let focus = cx.focus_handle();
        window.focus(&focus, cx);
        let p = path.clone();
        cx.spawn(async move |this, cx| {
            let found = cx
                .background_spawn(async move {
                    let mut found = catalog::agent_sessions(&p);
                    found.retain(|s| !known.contains(&s.id));
                    found
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                this.loading = false;
                this.found = found;
                cx.notify();
            });
        })
        .detach();
        Self {
            path,
            loading: true,
            found: Vec::new(),
            picked: HashSet::new(),
            focus,
        }
    }

    fn submit(&mut self, cx: &mut Context<Self>) {
        if self.picked.is_empty() {
            return;
        }
        let chosen = self
            .found
            .iter()
            .enumerate()
            .filter(|(i, _)| self.picked.contains(i))
            .map(|(_, s)| s.clone())
            .collect();
        cx.emit(SessionDialogEvent::Import(chosen));
    }
}

impl Focusable for ImportDialog {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

fn checkbox(on: bool) -> impl IntoElement {
    div()
        .flex()
        .flex_none()
        .items_center()
        .justify_center()
        .size(px(16.))
        .rounded(crate::ui::rad(4.))
        .border_1()
        .border_color(if on {
            theme::content()
        } else {
            theme::mix(0.25)
        })
        .when(on, |d| {
            d.bg(theme::content())
                .child(icon("tick02", 12.).text_color(theme::background()))
        })
}

impl Render for ImportDialog {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let n = self.picked.len();
        let all = !self.found.is_empty() && n == self.found.len();
        let body: gpui::AnyElement = if self.loading {
            div()
                .flex()
                .items_center()
                .gap(crate::ui::sp(8.))
                .py(crate::ui::sp(20.))
                .justify_center()
                .text_size(px(ui::TEXT_MD))
                .text_color(theme::text_subtle())
                .child(ui::spinner("import-spin", 14., theme::mix(0.5)))
                .child("Looking for sessions…")
                .into_any_element()
        } else if self.found.is_empty() {
            div()
                .py(crate::ui::sp(20.))
                .text_center()
                .text_size(px(ui::TEXT_MD))
                .text_color(theme::text_subtle())
                .child("Droid and Claude Code have no other sessions for this folder.")
                .into_any_element()
        } else {
            div()
                .id("import-list")
                .flex()
                .flex_col()
                .max_h(px(320.))
                .min_h_0()
                .overflow_y_scroll()
                .p(crate::ui::sp(4.))
                .rounded(crate::ui::rad(12.))
                .border_1()
                .border_color(theme::border())
                .children(self.found.iter().enumerate().map(|(i, s)| {
                    let on = self.picked.contains(&i);
                    div()
                        .id(("import-row", i))
                        .flex()
                        .items_center()
                        .gap(crate::ui::sp(10.))
                        .h(px(32.))
                        .px(crate::ui::sp(8.))
                        .rounded(crate::ui::rad(8.))
                        .cursor_pointer()
                        .hover(|st| st.bg(theme::selection_hover()))
                        .when(on, |d| d.bg(theme::selection()))
                        .child(checkbox(on))
                        .child(s.agent.icon(12.))
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .truncate()
                                .text_size(px(ui::TEXT_MD))
                                .text_color(theme::text_muted())
                                .child(s.title.clone()),
                        )
                        .child(
                            div()
                                .flex_none()
                                .text_size(px(ui::TEXT_XS))
                                .text_color(theme::text_subtle())
                                .child(model::age(s.updated)),
                        )
                        .on_click(cx.listener(move |this, _, _, cx| {
                            if !this.picked.remove(&i) {
                                this.picked.insert(i);
                            }
                            cx.notify();
                        }))
                }))
                .into_any_element()
        };
        ui::modal_panel("import-sessions", 480.)
            .track_focus(&self.focus)
            .on_key_down(cx.listener(|this, ev: &gpui::KeyDownEvent, _, cx| {
                match ev.keystroke.key.as_str() {
                    "escape" => cx.emit(SessionDialogEvent::Dismissed),
                    "enter" => this.submit(cx),
                    _ => {}
                }
            }))
            .child(ui::modal_header(
                "Import sessions",
                Some(
                    format!(
                        "Sessions Droid and Claude Code have for {}",
                        model::pretty_path(&self.path)
                    )
                    .into(),
                ),
                ui::close_button("import-close")
                    .on_click(cx.listener(|_, _, _, cx| cx.emit(SessionDialogEvent::Dismissed))),
            ))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .min_h_0()
                    .gap(crate::ui::sp(14.))
                    .p(crate::ui::sp(16.))
                    .child(body)
                    .child(
                        div()
                            .flex()
                            .flex_none()
                            .items_center()
                            .gap(crate::ui::sp(8.))
                            .when(!self.found.is_empty(), |d| {
                                d.child(
                                    ui::button(
                                        "import-all",
                                        if all { "Select none" } else { "Select all" },
                                        ButtonKind::Ghost,
                                        true,
                                    )
                                    .on_click(cx.listener(
                                        move |this, _, _, cx| {
                                            this.picked = if all {
                                                HashSet::new()
                                            } else {
                                                (0..this.found.len()).collect()
                                            };
                                            cx.notify();
                                        },
                                    )),
                                )
                            })
                            .child(div().flex_1())
                            .child(
                                ui::button("import-cancel", "Cancel", ButtonKind::Ghost, true)
                                    .on_click(cx.listener(|_, _, _, cx| {
                                        cx.emit(SessionDialogEvent::Dismissed)
                                    })),
                            )
                            .child(
                                ui::button(
                                    "import-submit",
                                    match n {
                                        0 | 1 => "Import session".to_string(),
                                        n => format!("Import {n} sessions"),
                                    },
                                    ButtonKind::Primary,
                                    n > 0,
                                )
                                .on_click(cx.listener(|this, _, _, cx| this.submit(cx))),
                            ),
                    ),
            )
    }
}

// ---------------------------------------------------------------------------

impl Shell {
    pub(super) fn open_session_menu(
        &mut self,
        id: SharedString,
        at: gpui::Point<gpui::Pixels>,
        cx: &mut Context<Self>,
    ) {
        if !self.has_projects() || self.env().session_index(&id).is_none() {
            return;
        }
        self.menu = Some(ContextMenu {
            target: MenuTarget::Session(self.env().session_index(&id).unwrap_or(0)),
            at,
        });
        cx.notify();
    }

    pub(super) fn render_session_menu(
        &self,
        index: usize,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let panel = ui::menu_panel("session-menu", 220.)
            .min_h_0()
            .overflow_y_scroll();
        self.session_menu_items(index, panel, cx).into_any_element()
    }

    /// Rename, fork, compact and remove for the session at `index`, appended to `panel`.
    pub(super) fn session_menu_items(
        &self,
        index: usize,
        mut panel: gpui::Stateful<gpui::Div>,
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let env = self.env();
        let Some(s) = env.sessions.get(index) else {
            return panel;
        };
        let id = s.id.clone();
        let live = s.live.as_ref().map(|l| l.thread.read(cx));
        let busy = live.is_some_and(|t| t.is_busy());
        // Fork and compact need a session the agent knows, and for Droid its native protocol.
        let known = live.is_some_and(|t| t.session_id().is_some())
            || s.meta.as_ref().is_some_and(|m| m.session_id.is_some());
        let can_compact = known && (s.agent == Agent::Claude || catalog::droid_native());
        let can_fork = can_compact && live.is_none_or(|t| t.supports_fork());
        let (rename, fork, compact, remove) = (id.clone(), id.clone(), id.clone(), id);
        panel = panel
            .child(
                ui::menu_item("sm-rename", "pencil-edit02", "Rename…", false, true).on_click(
                    cx.listener(move |this, _, window, cx| {
                        this.open_rename_dialog(rename.clone(), window, cx)
                    }),
                ),
            )
            .child(
                ui::menu_item("sm-fork", "git-branch", "Fork", false, can_fork).when(
                    can_fork,
                    |d| {
                        d.on_click(cx.listener(move |this, _, window, cx| {
                            this.menu = None;
                            this.session_action(fork.clone(), true, window, cx)
                        }))
                    },
                ),
            )
            .child(
                ui::menu_item(
                    "sm-compact",
                    "archive02",
                    if busy {
                        "Compact (wait for the turn)"
                    } else {
                        "Compact"
                    },
                    false,
                    can_compact && !busy,
                )
                .when(can_compact && !busy, |d| {
                    d.on_click(cx.listener(move |this, _, window, cx| {
                        this.menu = None;
                        this.session_action(compact.clone(), false, window, cx)
                    }))
                }),
            )
            .child(div().my(px(4.)).h(px(1.)).bg(theme::stroke()))
            .child(
                ui::menu_item("sm-remove", "delete02", "Remove from list…", true, true).on_click(
                    cx.listener(move |this, _, window, cx| {
                        this.open_remove_dialog(remove.clone(), window, cx)
                    }),
                ),
            );
        panel
    }

    fn session_dialog<D>(
        &mut self,
        dialog: Entity<D>,
        on_event: impl Fn(&mut Shell, &SessionDialogEvent, &mut Window, &mut Context<Shell>) + 'static,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) where
        D: EventEmitter<SessionDialogEvent> + Render,
    {
        let sub = cx.subscribe_in(
            &dialog,
            window,
            move |this, _, ev: &SessionDialogEvent, window, cx| {
                this.close_modal(window, cx);
                on_event(this, ev, window, cx);
            },
        );
        self.modal = Some((Modal::Session(dialog.into()), sub));
        cx.notify();
    }

    fn open_rename_dialog(
        &mut self,
        id: SharedString,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.menu = None;
        let env = self.env();
        let Some(i) = env.session_index(&id) else {
            return;
        };
        let title = env.sessions[i].title.to_string();
        let dialog = cx.new(|cx| RenameDialog::new(&title, window, cx));
        self.session_dialog(
            dialog,
            move |this, ev, _, cx| {
                if let SessionDialogEvent::Rename(title) = ev {
                    this.rename_session(&id, title, cx);
                }
            },
            window,
            cx,
        );
    }

    fn open_remove_dialog(
        &mut self,
        id: SharedString,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.menu = None;
        let env = self.env();
        let Some(i) = env.session_index(&id) else {
            return;
        };
        let s = &env.sessions[i];
        let (title, agent) = (s.title.clone(), s.agent);
        let busy = s.live.as_ref().is_some_and(|l| l.thread.read(cx).is_busy());
        let dialog = cx.new(|cx| RemoveDialog::new(title, agent, busy, window, cx));
        self.session_dialog(
            dialog,
            move |this, ev, window, cx| {
                if let SessionDialogEvent::Remove = ev {
                    this.remove_session(&id, window, cx);
                }
            },
            window,
            cx,
        );
    }

    pub(super) fn open_import_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.menu = None;
        if !self.has_projects() {
            return;
        }
        let path = self.env().path.clone();
        let known = self.known_agent_ids();
        let dialog = cx.new(|cx| ImportDialog::new(path, known, window, cx));
        self.session_dialog(
            dialog,
            |this, ev, _, cx| {
                if let SessionDialogEvent::Import(found) = ev {
                    this.import_agent_sessions(found.clone(), cx);
                }
            },
            window,
            cx,
        );
    }

    /// Right-click on a tab: split and close actions, plus the session menu for session tabs.
    pub(super) fn tab_menu(
        &mut self,
        index: usize,
        at: gpui::Point<gpui::Pixels>,
        cx: &mut Context<Self>,
    ) {
        if !self.has_projects() || index >= self.env().tabs.len() {
            return;
        }
        // Below the title bar, so the menu doesn't cover the tab it is about.
        let at = gpui::point(at.x, at.y.max(gpui::px(crate::ui::BAR_H + 2.)));
        self.menu = Some(ContextMenu {
            target: MenuTarget::Tab(index),
            at,
        });
        cx.notify();
    }
}
