//! Dialogs of the Changes panel: writing a commit, and confirming a destructive or
//! far-reaching git action (discarding a file, merging into the main checkout).

use std::path::PathBuf;

use anyhow::Result;
use gpui::prelude::FluentBuilder as _;
use gpui::{
    App, AppContext as _, Context, Entity, EventEmitter, FocusHandle, Focusable, Hsla,
    InteractiveElement as _, IntoElement, ParentElement as _, Render, SharedString,
    StatefulInteractiveElement as _, Styled as _, Subscription, Window, div, px,
};

use crate::git;
use crate::input::{InputEvent, TextInput};
use crate::theme;
use crate::ui::{self, ButtonKind, icon};

pub enum GitDialogEvent {
    Done,
    Dismissed,
}

fn error_box(e: SharedString) -> impl IntoElement {
    div()
        .id("git-dialog-error")
        .max_h(px(160.))
        .overflow_y_scroll()
        .p(px(10.))
        .rounded(px(12.))
        .bg(gpui::rgba(0xef44441a))
        .text_size(px(ui::TEXT_SM))
        .line_height(px(17.))
        .font_family(theme::FONT_MONO)
        .text_color(theme::error())
        .child(e)
}

// ---------------------------------------------------------------------------

pub struct CommitDialog {
    dir: PathBuf,
    branch: SharedString,
    files: usize,
    staged: usize,
    input: Entity<TextInput>,
    /// Some(all) while a commit runs.
    busy: Option<bool>,
    error: Option<SharedString>,
    _sub: Subscription,
}

impl EventEmitter<GitDialogEvent> for CommitDialog {}

impl CommitDialog {
    pub fn new(
        dir: PathBuf,
        branch: SharedString,
        files: usize,
        staged: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let input = cx.new(|cx| TextInput::new("Describe what changed", cx));
        let sub = cx.subscribe_in(&input, window, |this: &mut Self, _, ev, _, cx| match ev {
            InputEvent::Submit => {
                let all = this.staged == 0;
                this.submit(all, cx)
            }
            InputEvent::Cancel => {
                if this.busy.is_none() {
                    cx.emit(GitDialogEvent::Dismissed);
                }
            }
            InputEvent::Changed => {
                this.error = None;
                cx.notify();
            }
            _ => {}
        });
        window.focus(&input.focus_handle(cx), cx);
        Self {
            dir,
            branch,
            files,
            staged,
            input,
            busy: None,
            error: None,
            _sub: sub,
        }
    }

    fn message(&self, cx: &App) -> String {
        self.input.read(cx).text().trim().to_string()
    }

    fn submit(&mut self, all: bool, cx: &mut Context<Self>) {
        let message = self.message(cx);
        if self.busy.is_some() || message.is_empty() {
            return;
        }
        self.busy = Some(all);
        self.error = None;
        cx.notify();
        let dir = self.dir.clone();
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_spawn(async move { git::commit(&dir, &message, all) })
                .await;
            let _ = this.update(cx, |this, cx| {
                this.busy = None;
                match result {
                    Ok(()) => cx.emit(GitDialogEvent::Done),
                    Err(e) => {
                        this.error = Some(e.to_string().into());
                        cx.notify();
                    }
                }
            });
        })
        .detach();
    }
}

impl Focusable for CommitDialog {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.input.focus_handle(cx)
    }
}

impl Render for CommitDialog {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let ready = !self.message(cx).is_empty() && self.busy.is_none();
        let has_staged = self.staged > 0;
        let files = if self.files == 1 { "1 file".to_string() } else { format!("{} files", self.files) };
        let what = if has_staged {
            format!("{} of {files} staged", self.staged)
        } else {
            format!("{files} changed")
        };
        let label = |all: bool, busy: Option<bool>| -> &'static str {
            match (all, busy) {
                (true, Some(true)) | (false, Some(false)) => "Committing…",
                (true, _) => "Commit all",
                (false, _) => "Commit staged",
            }
        };
        let busy = self.busy;
        let commit_button = |id: &'static str, all: bool, kind: ButtonKind, cx: &mut Context<Self>| {
            ui::button(id, label(all, busy), kind, ready)
                .when(busy == Some(all), |d| {
                    let color = if kind == ButtonKind::Primary { theme::background() } else { theme::content() };
                    d.child(ui::spinner(("commit-spin", all as usize), 12., color))
                })
                .on_click(cx.listener(move |this, _, _, cx| this.submit(all, cx)))
        };

        ui::modal_panel("commit", 440.)
            .child(ui::modal_header(
                "Commit changes",
                Some(format!("{what} on {}", self.branch).into()),
                ui::close_button("commit-close").on_click(cx.listener(|this, _, _, cx| {
                    if this.busy.is_none() {
                        cx.emit(GitDialogEvent::Dismissed)
                    }
                })),
            ))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(16.))
                    .p(px(16.))
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap(px(6.))
                            .child(
                                div()
                                    .text_size(px(ui::TEXT_SM))
                                    .text_color(theme::text_muted())
                                    .child("Message"),
                            )
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .h(px(ui::INPUT_H))
                                    .px(px(10.))
                                    .rounded(px(ui::RADIUS))
                                    .border_1()
                                    .border_color(theme::border_strong())
                                    .bg(theme::background())
                                    .text_size(px(ui::TEXT_MD))
                                    .line_height(px(20.))
                                    .child(self.input.clone()),
                            ),
                    )
                    .when_some(self.error.clone(), |d, e| d.child(error_box(e)))
                    .child(
                        div()
                            .flex()
                            .justify_end()
                            .gap(px(8.))
                            .child(
                                ui::button("commit-cancel", "Cancel", ButtonKind::Ghost, self.busy.is_none())
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        if this.busy.is_none() {
                                            cx.emit(GitDialogEvent::Dismissed)
                                        }
                                    })),
                            )
                            .map(|d| {
                                if has_staged {
                                    d.child(commit_button("commit-all", true, ButtonKind::Ghost, cx))
                                        .child(commit_button("commit-staged", false, ButtonKind::Primary, cx))
                                } else {
                                    d.child(commit_button("commit-all", true, ButtonKind::Primary, cx))
                                }
                            }),
                    ),
            )
    }
}

// ---------------------------------------------------------------------------

type Action = Box<dyn FnOnce() -> Result<()> + Send>;

/// A yes/no dialog that runs its action in the background and keeps the error on
/// screen if it fails.
pub struct ConfirmDialog {
    title: SharedString,
    body: SharedString,
    details: Vec<(&'static str, Hsla, SharedString)>,
    confirm: SharedString,
    danger: bool,
    /// Why the action can't run right now; the confirm button stays disabled.
    blocked: Option<SharedString>,
    action: Option<Action>,
    busy: bool,
    error: Option<SharedString>,
    focus: FocusHandle,
}

impl EventEmitter<GitDialogEvent> for ConfirmDialog {}

impl ConfirmDialog {
    pub fn new(
        title: impl Into<SharedString>,
        body: impl Into<SharedString>,
        confirm: impl Into<SharedString>,
        danger: bool,
        action: impl FnOnce() -> Result<()> + Send + 'static,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let focus = cx.focus_handle();
        window.focus(&focus, cx);
        Self {
            title: title.into(),
            body: body.into(),
            details: Vec::new(),
            confirm: confirm.into(),
            danger,
            blocked: None,
            action: Some(Box::new(action)),
            busy: false,
            error: None,
            focus,
        }
    }

    pub fn detail(mut self, icon_name: &'static str, color: Hsla, text: impl Into<SharedString>) -> Self {
        self.details.push((icon_name, color, text.into()));
        self
    }

    pub fn blocked(mut self, why: Option<String>) -> Self {
        self.blocked = why.map(Into::into);
        self
    }

    fn submit(&mut self, cx: &mut Context<Self>) {
        if self.busy || self.blocked.is_some() {
            return;
        }
        let Some(action) = self.action.take() else {
            return;
        };
        self.busy = true;
        self.error = None;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result = cx.background_spawn(async move { action() }).await;
            let _ = this.update(cx, |this, cx| {
                this.busy = false;
                match result {
                    Ok(()) => cx.emit(GitDialogEvent::Done),
                    // A failed action may have changed things; offer closing, not retrying.
                    Err(e) => {
                        this.error = Some(e.to_string().into());
                        cx.notify();
                    }
                }
            });
        })
        .detach();
    }
}

impl Focusable for ConfirmDialog {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for ConfirmDialog {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let enabled = !self.busy && self.blocked.is_none() && self.action.is_some();
        let kind = if self.danger { ButtonKind::Danger } else { ButtonKind::Primary };
        let spin_color: Hsla = if self.danger { theme::error().into() } else { theme::background() };
        ui::modal_panel("git-confirm", 420.)
            .track_focus(&self.focus)
            .on_key_down(cx.listener(|this, ev: &gpui::KeyDownEvent, _, cx| {
                match ev.keystroke.key.as_str() {
                    "escape" if !this.busy => cx.emit(GitDialogEvent::Dismissed),
                    "enter" => this.submit(cx),
                    _ => {}
                }
            }))
            .child(ui::modal_header(
                self.title.clone(),
                None,
                ui::close_button("git-confirm-close").on_click(cx.listener(|this, _, _, cx| {
                    if !this.busy {
                        cx.emit(GitDialogEvent::Dismissed)
                    }
                })),
            ))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(16.))
                    .p(px(16.))
                    .text_size(px(ui::TEXT_MD))
                    .line_height(px(19.))
                    .child(div().text_color(theme::text_muted()).child(self.body.clone()))
                    .when(!self.details.is_empty(), |d| {
                        d.child(
                            div()
                                .flex()
                                .flex_col()
                                .gap(px(8.))
                                .p(px(12.))
                                .rounded(px(12.))
                                .border_1()
                                .border_color(theme::border())
                                .bg(theme::fill_hover())
                                .text_size(px(ui::TEXT_SM))
                                .text_color(theme::text_muted())
                                .children(self.details.iter().map(|(name, color, text)| {
                                    div()
                                        .flex()
                                        .items_start()
                                        .gap(px(10.))
                                        .child(div().pt(px(2.)).child(icon(name, 14.).text_color(*color)))
                                        .child(div().flex_1().min_w_0().child(text.clone()))
                                })),
                        )
                    })
                    .when_some(self.blocked.clone(), |d, why| {
                        d.child(
                            div()
                                .flex()
                                .items_start()
                                .gap(px(10.))
                                .text_size(px(ui::TEXT_SM))
                                .text_color(theme::warn())
                                .child(div().pt(px(2.)).child(icon("alert-circle", 14.).text_color(theme::warn())))
                                .child(div().flex_1().min_w_0().child(why)),
                        )
                    })
                    .when_some(self.error.clone(), |d, e| d.child(error_box(e)))
                    .child(
                        div()
                            .flex()
                            .justify_end()
                            .gap(px(8.))
                            .child(
                                ui::button(
                                    "git-confirm-cancel",
                                    if self.action.is_none() && !self.busy { "Close" } else { "Cancel" },
                                    ButtonKind::Ghost,
                                    !self.busy,
                                )
                                .on_click(cx.listener(|this, _, _, cx| {
                                    if !this.busy {
                                        cx.emit(GitDialogEvent::Dismissed)
                                    }
                                })),
                            )
                            .when(self.action.is_some() || self.busy, |d| {
                                d.child(
                                    ui::button("git-confirm-ok", self.confirm.clone(), kind, enabled)
                                        .when(self.busy, |d| d.child(ui::spinner("git-confirm-spin", 12., spin_color)))
                                        .on_click(cx.listener(|this, _, _, cx| this.submit(cx))),
                                )
                            }),
                    ),
            )
    }
}
