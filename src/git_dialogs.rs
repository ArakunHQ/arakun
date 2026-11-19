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

use crate::agent::AgentKind;
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
        .p(crate::ui::sp(10.))
        .rounded(crate::ui::rad(12.))
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
                .background_spawn(crate::guard::task(async move {
                    git::commit(&dir, &message, all)
                }))
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
        let files = if self.files == 1 {
            "1 file".to_string()
        } else {
            format!("{} files", self.files)
        };
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
        let commit_button =
            |id: &'static str, all: bool, kind: ButtonKind, cx: &mut Context<Self>| {
                ui::button(id, label(all, busy), kind, ready)
                    .when(busy == Some(all), |d| {
                        let color = if kind == ButtonKind::Primary {
                            theme::background()
                        } else {
                            theme::content()
                        };
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
                    .gap(crate::ui::sp(16.))
                    .p(crate::ui::sp(16.))
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap(crate::ui::sp(6.))
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
                                    .px(crate::ui::sp(10.))
                                    .rounded(crate::ui::rad(ui::RADIUS))
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
                            .gap(crate::ui::sp(8.))
                            .child(
                                ui::button(
                                    "commit-cancel",
                                    "Cancel",
                                    ButtonKind::Ghost,
                                    self.busy.is_none(),
                                )
                                .on_click(cx.listener(
                                    |this, _, _, cx| {
                                        if this.busy.is_none() {
                                            cx.emit(GitDialogEvent::Dismissed)
                                        }
                                    },
                                )),
                            )
                            .map(|d| {
                                if has_staged {
                                    d.child(commit_button(
                                        "commit-all",
                                        true,
                                        ButtonKind::Ghost,
                                        cx,
                                    ))
                                    .child(commit_button(
                                        "commit-staged",
                                        false,
                                        ButtonKind::Primary,
                                        cx,
                                    ))
                                } else {
                                    d.child(commit_button(
                                        "commit-all",
                                        true,
                                        ButtonKind::Primary,
                                        cx,
                                    ))
                                }
                            }),
                    ),
            )
    }
}

// ---------------------------------------------------------------------------

pub enum PullRequestEvent {
    /// The pull request was opened: its address (empty when gh named none).
    Created(String),
    Dismissed,
}

/// Opening a pull request for the checked-out branch: the title and where it
/// merges, with the description GitHub's own form would start with.
pub struct PullRequestDialog {
    dir: PathBuf,
    remote: String,
    branch: String,
    /// The branch has commits GitHub doesn't: they are pushed first.
    needs_push: bool,
    plan: git::PrPlan,
    desc: Description,
    /// The agent the "Write with" button asks; None when none can answer here.
    writer: Option<AgentKind>,
    /// The agent is writing.
    writing: bool,
    title: Entity<TextInput>,
    base: Entity<TextInput>,
    draft: bool,
    /// Some(on the web) while it is being opened.
    busy: Option<bool>,
    /// The base whose commits are being read for the description.
    planning: Option<String>,
    error: Option<SharedString>,
    _subs: Vec<Subscription>,
}

impl EventEmitter<PullRequestEvent> for PullRequestDialog {}

impl PullRequestDialog {
    pub fn new(
        dir: PathBuf,
        remote: String,
        branch: String,
        needs_push: bool,
        plan: git::PrPlan,
        writer: Option<AgentKind>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let title = cx.new(|cx| {
            let mut input = TextInput::new("What the pull request does", cx);
            input.set_text(plan.title.clone(), cx);
            input
        });
        let base = cx.new(|cx| {
            let mut input = TextInput::new("main", cx).mono();
            input.set_text(plan.base.clone(), cx);
            input
        });
        let follow = |this: &mut Self,
                      input: &Entity<TextInput>,
                      ev: &InputEvent,
                      window: &mut Window,
                      cx: &mut Context<Self>| match ev {
            InputEvent::Submit => this.submit(false, cx),
            // Tab: from the title to "Merge into" and back.
            InputEvent::Complete => {
                let other = if *input == this.title {
                    &this.base
                } else {
                    &this.title
                };
                window.focus(&other.focus_handle(cx), cx);
            }
            InputEvent::Cancel => {
                if this.busy.is_none() {
                    cx.emit(PullRequestEvent::Dismissed);
                }
            }
            InputEvent::Changed => {
                this.error = None;
                if *input == this.base {
                    this.refresh_plan(cx);
                }
                cx.notify();
            }
            _ => {}
        };
        let subs = vec![
            cx.subscribe_in(&title, window, follow),
            cx.subscribe_in(&base, window, follow),
        ];
        window.focus(&title.focus_handle(cx), cx);
        Self {
            dir,
            remote,
            branch,
            needs_push,
            desc: Description::new(&plan.base, plan.body.clone()),
            plan,
            writer,
            writing: false,
            title,
            base,
            draft: true,
            busy: None,
            planning: None,
            error: None,
            _subs: subs,
        }
    }

    /// Reads the commits and the description again for the base typed in
    /// "Merge into": in a stack the default base's would list the commits of
    /// the pull requests below too. A title the user changed stays.
    fn refresh_plan(&mut self, cx: &mut Context<Self>) {
        let base = self.base.read(cx).text().trim().to_string();
        if base.is_empty() || base == self.plan.base {
            self.planning = None;
            // Back on the base read last: what the agent wrote for another
            // base meanwhile doesn't describe it.
            self.desc.planned(&self.plan.base, self.plan.body.clone());
            return;
        }
        self.planning = Some(base.clone());
        let (dir, remote, branch) = (self.dir.clone(), self.remote.clone(), self.branch.clone());
        cx.spawn(async move |this, cx| {
            let onto = base.clone();
            let plan = cx
                .background_spawn(crate::guard::task_or_default(async move {
                    git::pr_plan_onto(&dir, &remote, &branch, &onto)
                }))
                .await;
            let _ = this.update(cx, |this, cx| {
                if this.planning.as_deref() == Some(base.as_str()) {
                    this.planning = None;
                }
                // Typed on since, or no such branch: the plan shown stays.
                let (Some(plan), true) = (plan, this.base.read(cx).text().trim() == base) else {
                    return;
                };
                if this.title.read(cx).text() == this.plan.title {
                    let title = plan.title.clone();
                    this.title.update(cx, |input, cx| input.set_text(title, cx));
                }
                this.desc.planned(&plan.base, plan.body.clone());
                this.plan = plan;
                cx.notify();
            });
        })
        .detach();
    }

    /// Opens the pull request here, or (`web`) GitHub's form for it, where
    /// the description can be written at length.
    fn submit(&mut self, web: bool, cx: &mut Context<Self>) {
        let title = self.title.read(cx).text().trim().to_string();
        let base = self.base.read(cx).text().trim().to_string();
        // What the agent is writing would arrive after the pull request went out.
        if self.busy.is_some() || self.writing || (!web && (title.is_empty() || base.is_empty())) {
            return;
        }
        // The description shown is still the one of the base before: it
        // would go out with this pull request.
        if !web && self.planning.is_some() {
            self.error = Some(
                format!("Still reading the commits for {base}. Try again in a moment.").into(),
            );
            cx.notify();
            return;
        }
        // No plan for this base: this clone doesn't have the branch (not
        // fetched, or misspelled), so the description shown is another
        // base's. GitHub's own form reads it there.
        if !web && base != self.plan.base {
            self.error = Some(
                format!(
                    "This clone has no {}/{base} to read the commits from. Fetch it, or open GitHub's form.",
                    self.remote
                )
                .into(),
            );
            cx.notify();
            return;
        }
        // Reads and the agent finish in any order: whatever the order, a
        // description of another base's commits doesn't go out.
        if !web && !self.desc.describes(&base) {
            self.error = Some(
                format!(
                    "The description is still the one for {}. Try again in a moment.",
                    self.desc.base
                )
                .into(),
            );
            cx.notify();
            return;
        }
        self.busy = Some(web);
        self.error = None;
        cx.notify();
        let (dir, remote, branch) = (self.dir.clone(), self.remote.clone(), self.branch.clone());
        let (draft, body) = (self.draft, self.desc.body.clone());
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_spawn(crate::guard::task(async move {
                    // The push state below is the checkout's: of another
                    // branch it would say nothing about this one.
                    let on = git::current_branch(&dir);
                    if on.as_deref() != Some(branch.as_str()) {
                        anyhow::bail!(
                            "The folder is on {} now. Check out {branch} to create its pull request.",
                            on.as_deref().unwrap_or("no branch")
                        );
                    }
                    // Read again: a commit made while the dialog was open
                    // would stay behind, and gh pushes nothing with --head.
                    // Not knowing is no reason to skip the push.
                    let Some(needs_push) = git::needs_push(&dir, &remote) else {
                        anyhow::bail!(
                            "Couldn't read the branch's state, so nothing was pushed or created."
                        );
                    };
                    if needs_push {
                        // A read-only Arakun sends nothing, the branch included.
                        if crate::github::read_only() {
                            anyhow::bail!("{}", crate::github::pr::READ_ONLY);
                        }
                        git::push(&dir, &remote, &branch)?;
                    }
                    // After the push: on a fork the head names its owner.
                    let head = git::pr_head(&dir, &remote, &branch);
                    if web {
                        git::pr_create_web(&dir, &head, &base, &title).map(|()| None)
                    } else {
                        git::pr_create(&dir, &head, &base, &title, &body, draft).map(Some)
                    }
                }))
                .await;
            let _ = this.update(cx, |this, cx| {
                this.busy = None;
                match result {
                    Ok(Some(url)) => cx.emit(PullRequestEvent::Created(url)),
                    // The browser has it now.
                    Ok(None) => cx.emit(PullRequestEvent::Dismissed),
                    Err(e) => {
                        // The push may have gone through: the next try skips it
                        // only when it did, and pushing twice is harmless.
                        this.error = Some(e.to_string().into());
                        cx.notify();
                    }
                }
            });
        })
        .detach();
    }
}

/// The pull request's description, who wrote it, and the base whose
/// commits it describes. A reading of the commits for a new base and the
/// agent can finish in either order; the base decides which text is shown.
#[derive(Debug)]
struct Description {
    /// What goes out: the plan's, or what the agent wrote.
    body: String,
    /// Who wrote the description shown, once an agent did.
    written_by: Option<AgentKind>,
    /// The base the description was read or written for.
    base: String,
}

impl Description {
    fn new(base: &str, body: String) -> Self {
        Self {
            body,
            written_by: None,
            base: base.trim().to_string(),
        }
    }

    /// A reading of the commits onto `base` found `body`.
    fn planned(&mut self, base: &str, body: String) {
        // What the agent wrote for this base stays: the user asked for it.
        if self.written_by.is_some() && self.describes(base) {
            return;
        }
        *self = Self::new(base, body);
    }

    fn wrote(&mut self, base: &str, agent: AgentKind, body: String) {
        *self = Self::new(base, body);
        self.written_by = Some(agent);
    }

    fn describes(&self, base: &str) -> bool {
        written_for(&self.base, base)
    }
}

/// Whether what an agent wrote for the base `asked` still fits the base in
/// the dialog `now`.
fn written_for(asked: &str, now: &str) -> bool {
    asked.trim() == now.trim()
}

impl PullRequestDialog {
    /// "Write with <agent>": the agent reads the branch's commits and diff and
    /// writes the title and the description. Nothing goes to GitHub; the
    /// user reads both before creating.
    fn write_with_ai(&mut self, cx: &mut Context<Self>) {
        let Some(agent) = self.writer else { return };
        let base = self.base.read(cx).text().trim().to_string();
        if self.writing || self.busy.is_some() || base.is_empty() {
            return;
        }
        self.writing = true;
        self.error = None;
        cx.notify();
        let (dir, remote, branch) = (self.dir.clone(), self.remote.clone(), self.branch.clone());
        // Both fields stay editable while it writes.
        let (for_base, old_title) = (base.clone(), self.title.read(cx).text().to_string());
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_spawn(crate::guard::task(async move {
                    let input = crate::pr_writer::read(&dir, &remote, &base, &branch);
                    if input.commits.is_empty() && input.diff.trim().is_empty() {
                        anyhow::bail!(
                            "Nothing to write from: git found no commits on {branch} that {remote}/{base} doesn't have."
                        );
                    }
                    crate::pr_writer::write(agent, &input)
                }))
                .await;
            let _ = this.update(cx, |this, cx| {
                this.writing = false;
                let now = (
                    this.base.read(cx).text().trim().to_string(),
                    this.title.read(cx).text().to_string(),
                );
                match result {
                    // It describes the commits of another base than the one
                    // the pull request would now merge into.
                    Ok(_) if !written_for(&for_base, &now.0) => {
                        this.error = Some(
                            format!(
                                "\"Merge into\" changed while {} wrote. Write it again for {}.",
                                agent.label(),
                                now.0
                            )
                            .into(),
                        )
                    }
                    Ok((title, body)) => {
                        // A title typed meanwhile is the user's.
                        if now.1 == old_title {
                            this.title.update(cx, |i, cx| i.set_text(title, cx));
                        }
                        this.desc.wrote(&for_base, agent, body);
                    }
                    Err(e) => {
                        this.error = Some(format!("{} couldn't write it: {e}", agent.label()).into())
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// The button beside the description's label.
    fn write_button(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let can = self.writer.is_some() && !self.writing && self.busy.is_none();
        // The writer can be the agent picked for naming branches, not this
        // environment's: its name on the button says where the diff goes
        // before the click sends it.
        let label: SharedString = match (self.writing, self.desc.written_by, self.writer) {
            (true, _, _) => "Writing…".into(),
            (false, Some(_), _) => "Write again".into(),
            (false, None, Some(agent)) => format!("Write with {}", agent.label()).into(),
            (false, None, None) => "Write with AI".into(),
        };
        let tip = match self.writer {
            Some(agent) => format!(
                "Sends the branch's commits and diff to {}, which writes the title and description",
                agent.label()
            ),
            None => "Needs Claude Code: as this environment's agent, or picked for naming branches"
                .to_string(),
        };
        ui::button_with(
            "pull-request-write",
            (!self.writing).then_some("sparkles"),
            label,
            ButtonKind::Outline,
            can,
        )
        .when(self.writing, |d| {
            d.child(ui::spinner("pull-request-writing", 12., theme::content()))
        })
        .tooltip(ui::tip_text(tip))
        .when(can, |d| {
            d.on_click(cx.listener(|this, _, _, cx| this.write_with_ai(cx)))
        })
    }
}

impl Focusable for PullRequestDialog {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.title.focus_handle(cx)
    }
}

/// A labeled text box of a dialog.
fn labeled(label: &'static str, input: &Entity<TextInput>) -> gpui::Div {
    div()
        .flex()
        .flex_col()
        .gap(crate::ui::sp(6.))
        .child(ui::text(ui::Type::Label, theme::text_muted(), label))
        .child(
            ui::field()
                .bg(theme::background())
                .child(div().flex_1().min_w_0().child(input.clone())),
        )
}

impl Render for PullRequestDialog {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        use ui::Typography as _;
        let free = self.busy.is_none();
        let ready = free
            && !self.writing
            && !self.title.read(cx).text().trim().is_empty()
            && !self.base.read(cx).text().trim().is_empty();
        let commits = match self.plan.commits {
            0 => String::new(),
            1 => "1 commit · ".into(),
            n => format!("{n} commits · "),
        };
        let about = format!(
            "{commits}{}{}",
            self.branch,
            if self.needs_push {
                " is pushed first"
            } else {
                ""
            }
        );
        let body_label = match self.desc.written_by {
            Some(agent) => format!("Description (written by {})", agent.label()),
            None if self.plan.template => "Description (the repository's template)".to_string(),
            None => "Description (from the commits)".to_string(),
        };
        let body = if self.desc.body.trim().is_empty() {
            ui::text(
                ui::Type::Caption,
                theme::text_subtle(),
                "Empty. Write it on GitHub once the pull request is open.",
            )
        } else {
            div()
                .ty(ui::Type::Mono)
                .text_color(theme::text_muted())
                .child(self.desc.body.clone())
        };
        let create = if self.draft {
            "Create draft"
        } else {
            "Create pull request"
        };
        ui::modal_panel("pull-request", 520.)
            .child(ui::modal_header(
                "Create pull request",
                Some(about.into()),
                ui::close_button("pull-request-close").on_click(cx.listener(|this, _, _, cx| {
                    if this.busy.is_none() {
                        cx.emit(PullRequestEvent::Dismissed)
                    }
                })),
            ))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(crate::ui::sp(16.))
                    .p(crate::ui::sp(16.))
                    .child(labeled("Title", &self.title))
                    .child(labeled("Merge into", &self.base))
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap(crate::ui::sp(6.))
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap(crate::ui::sp(8.))
                                    .child(
                                        ui::text(ui::Type::Label, theme::text_muted(), body_label)
                                            .flex_1()
                                            .min_w_0()
                                            .truncate(),
                                    )
                                    .child(self.write_button(cx)),
                            )
                            .child(
                                div()
                                    .id("pull-request-body")
                                    .max_h(px(180.))
                                    .overflow_y_scroll()
                                    .px(crate::ui::sp(10.))
                                    .py(crate::ui::sp(8.))
                                    .rounded(crate::ui::rad(ui::RADIUS))
                                    .border_1()
                                    .border_color(theme::border())
                                    .bg(theme::fill_raised())
                                    .child(body),
                            )
                            .child(ui::text(
                                ui::Type::Caption,
                                theme::text_faint(),
                                "Edit it on GitHub afterwards, or open GitHub's form to write it first.",
                            )),
                    )
                    .child(
                        ui::switch_row("pull-request-draft", "Open as a draft", self.draft)
                            .cursor_pointer()
                            .on_click(cx.listener(|this, _, _, cx| {
                                if this.busy.is_none() {
                                    this.draft = !this.draft;
                                    cx.notify();
                                }
                            })),
                    )
                    .when_some(self.error.clone(), |d, e| d.child(error_box(e)))
                    .child(
                        div()
                            .flex()
                            .flex_wrap()
                            .justify_end()
                            .gap(crate::ui::sp(8.))
                            .child(
                                ui::button(
                                    "pull-request-web",
                                    if self.busy == Some(true) {
                                        "Opening…"
                                    } else {
                                        "Open GitHub's form"
                                    },
                                    ButtonKind::Ghost,
                                    // Nothing opens while the agent writes.
                                    free && !self.writing,
                                )
                                .on_click(cx.listener(|this, _, _, cx| this.submit(true, cx))),
                            )
                            .child(div().flex_1())
                            .child(
                                ui::button("pull-request-cancel", "Cancel", ButtonKind::Ghost, free)
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        if this.busy.is_none() {
                                            cx.emit(PullRequestEvent::Dismissed)
                                        }
                                    })),
                            )
                            .child(
                                ui::button(
                                    "pull-request-create",
                                    if self.busy == Some(false) {
                                        "Creating…"
                                    } else {
                                        create
                                    },
                                    ButtonKind::Primary,
                                    ready,
                                )
                                .when(self.busy == Some(false), |d| {
                                    d.child(ui::spinner(
                                        "pull-request-spin",
                                        12.,
                                        theme::background(),
                                    ))
                                })
                                .on_click(cx.listener(|this, _, _, cx| this.submit(false, cx))),
                            ),
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

    pub fn detail(
        mut self,
        icon_name: &'static str,
        color: Hsla,
        text: impl Into<SharedString>,
    ) -> Self {
        self.details.push((icon_name, color, text.into()));
        self
    }

    pub fn blocked(mut self, why: Option<String>) -> Self {
        self.blocked = why.map(Into::into);
        self
    }

    /// The action is running; whoever opened it waits for its event.
    pub fn busy(&self) -> bool {
        self.busy
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
            let result = cx
                .background_spawn(crate::guard::task(async move { action() }))
                .await;
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
        let kind = if self.danger {
            ButtonKind::Danger
        } else {
            ButtonKind::Primary
        };
        let spin_color: Hsla = if self.danger {
            theme::error().into()
        } else {
            theme::background()
        };
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
                    // Lets a long details list shrink and scroll on short
                    // windows instead of pushing the buttons out of the panel.
                    .min_h_0()
                    .gap(crate::ui::sp(16.))
                    .p(crate::ui::sp(16.))
                    .text_size(px(ui::TEXT_MD))
                    .line_height(px(19.))
                    .child(
                        div()
                            .flex_none()
                            .text_color(theme::text_muted())
                            .child(self.body.clone()),
                    )
                    .when(!self.details.is_empty(), |d| {
                        d.child(
                            div()
                                .id("git-confirm-details")
                                .flex()
                                .flex_col()
                                .min_h_0()
                                .overflow_y_scroll()
                                .gap(crate::ui::sp(8.))
                                .p(crate::ui::sp(12.))
                                .rounded(crate::ui::rad(12.))
                                .border_1()
                                .border_color(theme::border())
                                .bg(theme::fill_hover())
                                .text_size(px(ui::TEXT_SM))
                                .text_color(theme::text_muted())
                                .children(self.details.iter().map(|(name, color, text)| {
                                    div()
                                        .flex()
                                        .flex_none()
                                        .items_start()
                                        .gap(crate::ui::sp(10.))
                                        .child(
                                            div()
                                                .pt(crate::ui::sp(2.))
                                                .child(icon(name, 14.).text_color(*color)),
                                        )
                                        .child(div().flex_1().min_w_0().child(text.clone()))
                                })),
                        )
                    })
                    .when_some(self.blocked.clone(), |d, why| {
                        d.child(
                            div()
                                .flex()
                                .flex_none()
                                .items_start()
                                .gap(crate::ui::sp(10.))
                                .text_size(px(ui::TEXT_SM))
                                .text_color(theme::warn())
                                .child(
                                    div()
                                        .pt(crate::ui::sp(2.))
                                        .child(icon("alert-circle", 14.).text_color(theme::warn())),
                                )
                                .child(div().flex_1().min_w_0().child(why)),
                        )
                    })
                    .when_some(self.error.clone(), |d, e| d.child(error_box(e)))
                    .child(
                        div()
                            .flex()
                            .flex_none()
                            .justify_end()
                            .gap(crate::ui::sp(8.))
                            .child(
                                ui::button(
                                    "git-confirm-cancel",
                                    if self.action.is_none() && !self.busy {
                                        "Close"
                                    } else {
                                        "Cancel"
                                    },
                                    ButtonKind::Ghost,
                                    !self.busy,
                                )
                                .on_click(cx.listener(
                                    |this, _, _, cx| {
                                        if !this.busy {
                                            cx.emit(GitDialogEvent::Dismissed)
                                        }
                                    },
                                )),
                            )
                            .when(self.action.is_some() || self.busy, |d| {
                                d.child(
                                    ui::button(
                                        "git-confirm-ok",
                                        self.confirm.clone(),
                                        kind,
                                        enabled,
                                    )
                                    .when(self.busy, |d| {
                                        d.child(ui::spinner("git-confirm-spin", 12., spin_color))
                                    })
                                    .on_click(cx.listener(|this, _, _, cx| this.submit(cx))),
                                )
                            }),
                    ),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const AGENT: AgentKind = AgentKind::Claude;

    #[test]
    fn a_read_for_another_base_replaces_the_description() {
        let mut d = Description::new("main", "main's".into());
        d.planned("release", "release's".into());
        assert_eq!(d.body, "release's");
        assert!(d.describes("release"));
    }

    #[test]
    fn the_agent_answer_stays_when_a_read_for_its_base_lands() {
        let mut d = Description::new("main", "main's".into());
        d.wrote("release", AGENT, "agent".into());
        d.planned("release", "release's".into());
        assert_eq!(d.body, "agent");
        assert_eq!(d.written_by, Some(AGENT));
    }

    #[test]
    fn the_agent_answer_goes_when_a_read_for_another_base_lands() {
        let mut d = Description::new("main", "main's".into());
        d.wrote("main", AGENT, "agent".into());
        d.planned("release", "release's".into());
        assert_eq!(d.body, "release's");
        assert_eq!(d.written_by, None);
    }

    #[test]
    fn going_back_to_the_read_base_restores_its_description() {
        // The agent wrote for release before release's commits were read;
        // the plan is still main's.
        let mut d = Description::new("main", "main's".into());
        d.wrote("release", AGENT, "agent".into());
        assert!(!d.describes("main"));
        d.planned("main", "main's".into());
        assert_eq!(d.body, "main's");
        assert!(d.describes("main"));
    }
}
