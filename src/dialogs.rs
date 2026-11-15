//! "New environment" and "Delete environment" dialogs.

use std::path::PathBuf;

use gpui::prelude::FluentBuilder as _;
use gpui::{
    App, AppContext as _, Context, Entity, EventEmitter, FocusHandle, Focusable, FontWeight,
    InteractiveElement as _, IntoElement, ParentElement as _, Render, SharedString,
    StatefulInteractiveElement as _, Styled as _, Subscription, Window, div, px,
};

use crate::env_name;
use crate::git;
use crate::input::{InputEvent, TextInput};
use crate::model::{pretty_path, workspaces_root, worktree_path};
use crate::theme;
use crate::ui::{self, ButtonKind, icon};

pub enum DialogEvent {
    /// `auto_name`: the branch kept the random name the dialog filled in.
    Created {
        path: PathBuf,
        branch: String,
        auto_name: bool,
    },
    Deleted(PathBuf),
    /// A project command (setup script, check) was saved; None clears it.
    Command(Option<String>),
    Dismissed,
}

// ---------------------------------------------------------------------------

pub struct CreateEnvDialog {
    repo: PathBuf,
    project: SharedString,
    /// Project setup script, shown so the user knows it will run.
    setup: Option<String>,
    input: Entity<TextInput>,
    /// The random name filled in; a branch created with it gets renamed after
    /// the first prompt.
    suggested: String,
    /// Who renames a branch that keeps `suggested`; None when naming is off.
    namer: Option<crate::agent::AgentKind>,
    branches: git::Branches,
    /// The local branches are read. Until then a random name could match one, which
    /// would be checked out and later renamed.
    branches_loaded: bool,
    /// None = current HEAD.
    base: Option<String>,
    base_open: bool,
    busy: bool,
    error: Option<SharedString>,
    _sub: Subscription,
}

impl EventEmitter<DialogEvent> for CreateEnvDialog {}

impl CreateEnvDialog {
    pub fn new(
        repo: PathBuf,
        project: SharedString,
        setup: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let suggested = env_name::random(|n| folder_taken(&project, n));
        // Selected, so typing a name of your own replaces it.
        let input = cx.new(|cx| {
            let mut i = TextInput::new("feature/my-task", cx).mono();
            i.set_text(suggested.clone(), cx);
            i.select_all_text(cx);
            i
        });
        let sub = cx.subscribe_in(&input, window, |this: &mut Self, _, ev, _, cx| match ev {
            InputEvent::Submit => this.submit(cx),
            InputEvent::Cancel => {
                if this.base_open {
                    this.base_open = false;
                    cx.notify();
                } else if !this.busy {
                    cx.emit(DialogEvent::Dismissed);
                }
            }
            InputEvent::Changed => {
                this.error = None;
                cx.notify();
            }
            _ => {}
        });
        window.focus(&input.focus_handle(cx), cx);
        let r = repo.clone();
        cx.spawn(async move |this, cx| {
            let branches = cx
                .background_spawn(async move { git::branches(&r).unwrap_or_default() })
                .await;
            let _ = this.update(cx, |this, cx| {
                this.branches = branches;
                this.branches_loaded = true;
                let untouched = this.name(cx) == this.suggested;
                if untouched && this.branches.local.contains(&this.suggested) {
                    let project = this.project.clone();
                    let local = &this.branches.local;
                    this.suggested = env_name::random(|n| {
                        local.iter().any(|b| b == n) || folder_taken(&project, n)
                    });
                    let name = this.suggested.clone();
                    this.input.update(cx, |i, cx| {
                        i.set_text(name, cx);
                        i.select_all_text(cx);
                    });
                }
                cx.notify();
            });
        })
        .detach();
        Self {
            repo,
            project,
            setup,
            input,
            suggested,
            namer: env_name::namer().agent,
            branches: git::Branches::default(),
            branches_loaded: false,
            base: None,
            base_open: false,
            busy: false,
            error: None,
            _sub: sub,
        }
    }

    fn name(&self, cx: &App) -> String {
        self.input.read(cx).text().trim().to_string()
    }

    fn submit(&mut self, cx: &mut Context<Self>) {
        let name = self.name(cx);
        if self.busy || name.is_empty() || !self.branches_loaded {
            return;
        }
        if name.contains(char::is_whitespace) || !git::branch_name_ok(&self.repo, &name) {
            self.error = Some(format!("\"{name}\" is not a valid branch name").into());
            cx.notify();
            return;
        }
        self.busy = true;
        self.error = None;
        cx.notify();
        let repo = self.repo.clone();
        let path = worktree_path(&self.project, &name);
        let base = self.base.clone();
        // An existing branch is the user's, whatever it is called.
        let auto_name = name == self.suggested && !self.branches.local.contains(&name);
        cx.spawn(async move |this, cx| {
            let target = path.clone();
            let branch = name.clone();
            let result = cx
                .background_spawn(async move {
                    git::add_worktree(&repo, &target, &name, base.as_deref())
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                this.busy = false;
                match result {
                    Ok(()) => cx.emit(DialogEvent::Created {
                        path,
                        branch,
                        auto_name,
                    }),
                    Err(e) => {
                        this.error = Some(e.to_string().into());
                        cx.notify();
                    }
                }
            });
        })
        .detach();
    }

    fn base_label(&self) -> SharedString {
        match &self.base {
            Some(b) => b.clone().into(),
            None => match &self.branches.current {
                Some(c) => format!("Current commit ({c})").into(),
                None => "Current commit".into(),
            },
        }
    }
}

impl Focusable for CreateEnvDialog {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.input.focus_handle(cx)
    }
}

/// A random name is taken when its worktree folder exists: the new one would get a
/// `-2` folder that no longer matches it.
fn folder_taken(project: &str, name: &str) -> bool {
    workspaces_root()
        .join(project)
        .join(name.replace('/', "-"))
        .exists()
}

fn field_label(text: &str) -> impl IntoElement {
    div()
        .text_size(px(ui::TEXT_SM))
        .text_color(theme::text_muted())
        .child(text.to_string())
}

fn field_box() -> gpui::Div {
    ui::field()
}

impl Render for CreateEnvDialog {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let name = self.name(cx);
        let existing = self.branches.local.contains(&name);
        let namer = (name == self.suggested && !existing)
            .then_some(self.namer)
            .flatten();
        let target = (!name.is_empty()).then(|| worktree_path(&self.project, &name));
        let ready = !name.is_empty() && !self.busy && self.branches_loaded;

        let base_picker = div()
            .relative()
            .child(
                field_box()
                    .id("env-base")
                    .cursor_pointer()
                    .when(existing, |d| d.opacity(0.5))
                    .hover(|s| s.border_color(theme::border_strong()))
                    .child(icon("git-branch", 14.).text_color(theme::text_subtle()))
                    .child(div().flex_1().min_w_0().truncate().child(self.base_label()))
                    .child(icon("unfold-more", 14.).text_color(theme::text_subtle()))
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.base_open = !this.base_open;
                        cx.notify();
                    })),
            )
            .when(self.base_open, |d| {
                let mut options: Vec<(Option<String>, SharedString)> =
                    vec![(None, self.base_label_for_head())];
                options.extend(
                    self.branches
                        .local
                        .iter()
                        .chain(self.branches.remote.iter())
                        .map(|b| (Some(b.clone()), SharedString::from(b.clone()))),
                );
                d.child(
                    div()
                        .id("env-base-menu")
                        .absolute()
                        .top(px(40.))
                        .left_0()
                        .right_0()
                        .max_h(px(220.))
                        .overflow_y_scroll()
                        .occlude()
                        .p(px(4.))
                        .rounded(px(ui::RADIUS_XL))
                        .border_1()
                        .border_color(theme::border())
                        .bg(theme::dialog())
                        .shadow_xl()
                        .children(options.into_iter().enumerate().map(|(i, (value, label))| {
                            let chosen = value == self.base;
                            div()
                                .id(("env-base-opt", i))
                                .flex()
                                .items_center()
                                .gap(px(8.))
                                .h(px(ui::MENU_ROW_H))
                                .px(px(8.))
                                .rounded(px(ui::RADIUS))
                                .text_size(px(ui::TEXT_SM))
                                .font_family(theme::FONT_MONO)
                                .cursor_pointer()
                                .when(chosen, |d| d.bg(theme::selection()))
                                .hover(|s| s.bg(theme::selection_hover()))
                                .child(div().flex_1().min_w_0().truncate().child(label))
                                .when(chosen, |d| {
                                    d.child(icon("tick02", 12.).text_color(theme::text_muted()))
                                })
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.base = value.clone();
                                    this.base_open = false;
                                    cx.notify();
                                }))
                        })),
                )
            });

        ui::modal_panel("create-env", 420.)
            .child(ui::modal_header(
                "New environment",
                None,
                ui::close_button("create-env-close").on_click(cx.listener(|this, _, _, cx| {
                    if !this.busy {
                        cx.emit(DialogEvent::Dismissed)
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
                            .text_size(px(ui::TEXT_SM))
                            .line_height(px(18.))
                            .text_color(theme::text_subtle())
                            .child(format!(
                                "An independent worktree of {} with its own sessions, terminals and browser tabs. Uncommitted changes stay where they are.",
                                self.project
                            )),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap(px(6.))
                            .child(field_label("Branch"))
                            .child(field_box().child(self.input.clone()))
                            .when(existing, |d| {
                                d.child(
                                    div()
                                        .text_size(px(ui::TEXT_XS))
                                        .text_color(theme::text_subtle())
                                        .child("This branch already exists; it will be checked out."),
                                )
                            })
                            .when_some(namer, |d, agent| {
                                d.child(
                                    div()
                                        .text_size(px(ui::TEXT_XS))
                                        .text_color(theme::text_subtle())
                                        .child(format!(
                                            "A placeholder: after your first prompt, {} names the branch after the task.",
                                            agent.label()
                                        )),
                                )
                            }),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap(px(6.))
                            .child(field_label("Start from"))
                            .child(base_picker),
                    )
                    .when_some(target, |d, t| {
                        d.child(
                            div()
                                .text_size(px(ui::TEXT_XS))
                                .text_color(theme::text_faint())
                                .font_family(theme::FONT_MONO)
                                .child(format!("Created in {}", pretty_path(&t))),
                        )
                    })
                    .when_some(self.setup.clone(), |d, s| {
                        d.child(
                            div()
                                .flex()
                                .items_center()
                                .gap(px(6.))
                                .text_size(px(ui::TEXT_XS))
                                .text_color(theme::text_subtle())
                                .child(icon("computer-terminal01", 12.).text_color(theme::text_faint()))
                                .child(
                                    div()
                                        .flex_1()
                                        .min_w_0()
                                        .truncate()
                                        .font_family(theme::FONT_MONO)
                                        .child(format!("Setup script will run: {s}")),
                                ),
                        )
                    })
                    .when_some(self.error.clone(), |d, e| {
                        d.child(
                            div()
                                .text_size(px(ui::TEXT_SM))
                                .text_color(theme::error())
                                .child(e),
                        )
                    })
                    .child(
                        div()
                            .flex()
                            .justify_end()
                            .gap(px(8.))
                            .child(
                                ui::button("create-env-cancel", "Cancel", ButtonKind::Ghost, !self.busy)
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        if !this.busy {
                                            cx.emit(DialogEvent::Dismissed)
                                        }
                                    })),
                            )
                            .child(
                                ui::button(
                                    "create-env-submit",
                                    if self.busy { "Creating…" } else { "Create environment" },
                                    ButtonKind::Primary,
                                    ready,
                                )
                                .when(self.busy, |d| {
                                    d.child(ui::spinner("create-env-spin", 12., theme::background()))
                                })
                                .on_click(cx.listener(|this, _, _, cx| this.submit(cx))),
                            ),
                    ),
            )
    }
}

impl CreateEnvDialog {
    fn base_label_for_head(&self) -> SharedString {
        match &self.branches.current {
            Some(c) => format!("Current commit ({c})").into(),
            None => "Current commit".into(),
        }
    }
}

// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------

/// One shell command of a project, such as its setup script or its check.
pub struct CommandDialog {
    title: &'static str,
    about: &'static str,
    input: Entity<TextInput>,
    _sub: Subscription,
}

impl EventEmitter<DialogEvent> for CommandDialog {}

impl CommandDialog {
    /// The project's setup script, run in a terminal when an environment is created.
    pub fn setup(current: Option<&str>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        Self::new(
            "Setup script",
            "Runs in a terminal of every new environment of this project, right after its worktree is created. Empty disables it.",
            "npm install && cp .env.example .env",
            current,
            window,
            cx,
        )
    }

    /// The project's check, run after agent turns that edit files.
    pub fn check(current: Option<&str>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        Self::new(
            "Check command",
            "Runs in the environment after every agent turn that edits files. When it fails, Arakun sends the output back to the agent, up to 3 times in a row. Agents can run it themselves with `arakun check`. Empty disables it.",
            "cargo test",
            current,
            window,
            cx,
        )
    }

    fn new(
        title: &'static str,
        about: &'static str,
        placeholder: &'static str,
        current: Option<&str>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let input = cx.new(|cx| TextInput::new(placeholder, cx).mono());
        if let Some(current) = current
            && !current.is_empty()
        {
            input.update(cx, |i, cx| i.set_text(current, cx));
        }
        let sub = cx.subscribe_in(&input, window, |this: &mut Self, _, ev, _, cx| match ev {
            InputEvent::Submit => {
                let text = this.input.read(cx).text().trim().to_string();
                cx.emit(DialogEvent::Command((!text.is_empty()).then_some(text)));
            }
            InputEvent::Cancel => cx.emit(DialogEvent::Dismissed),
            _ => {}
        });
        window.focus(&input.focus_handle(cx), cx);
        Self {
            title,
            about,
            input,
            _sub: sub,
        }
    }
}

impl Focusable for CommandDialog {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.input.focus_handle(cx)
    }
}

impl Render for CommandDialog {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        ui::modal_panel("setup-script", 420.)
            .child(ui::modal_header(
                self.title,
                None,
                ui::close_button("setup-close")
                    .on_click(cx.listener(|_, _, _, cx| cx.emit(DialogEvent::Dismissed))),
            ))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(16.))
                    .p(px(16.))
                    .child(
                        div()
                            .text_size(px(ui::TEXT_SM))
                            .line_height(px(18.))
                            .text_color(theme::text_subtle())
                            .child(self.about),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap(px(6.))
                            .child(field_label("Command"))
                            .child(field_box().child(self.input.clone())),
                    )
                    .child(
                        div()
                            .flex()
                            .justify_end()
                            .gap(px(8.))
                            .child(
                                ui::button("setup-cancel", "Cancel", ButtonKind::Ghost, true)
                                    .on_click(
                                        cx.listener(|_, _, _, cx| cx.emit(DialogEvent::Dismissed)),
                                    ),
                            )
                            .child(
                                ui::button("setup-save", "Save", ButtonKind::Primary, true)
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        let text = this.input.read(cx).text().trim().to_string();
                                        cx.emit(DialogEvent::Command(
                                            (!text.is_empty()).then_some(text),
                                        ));
                                    })),
                            ),
                    ),
            )
    }
}

// ---------------------------------------------------------------------------

pub struct DeleteEnvDialog {
    repo: PathBuf,
    path: PathBuf,
    branch: Option<String>,
    dirty: usize,
    unpushed: Option<u32>,
    /// Commits no remote has, on a branch that was never pushed.
    local_only: Option<u32>,
    delete_branch: bool,
    busy: bool,
    error: Option<SharedString>,
    focus: FocusHandle,
}

impl EventEmitter<DialogEvent> for DeleteEnvDialog {}

impl DeleteEnvDialog {
    pub fn new(
        repo: PathBuf,
        path: PathBuf,
        branch: Option<String>,
        dirty: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let focus = cx.focus_handle();
        window.focus(&focus, cx);
        let p = path.clone();
        cx.spawn(async move |this, cx| {
            let counts = cx
                .background_spawn(async move { (git::unpushed(&p), git::local_only(&p)) })
                .await;
            let _ = this.update(cx, |this, cx| {
                this.unpushed = counts.0;
                this.local_only = counts.1;
                cx.notify();
            });
        })
        .detach();
        Self {
            repo,
            path,
            branch,
            dirty,
            unpushed: None,
            local_only: None,
            delete_branch: false,
            busy: false,
            error: None,
            focus,
        }
    }

    fn submit(&mut self, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        self.busy = true;
        self.error = None;
        cx.notify();
        let repo = self.repo.clone();
        let path = self.path.clone();
        let branch = self.delete_branch.then(|| self.branch.clone()).flatten();
        cx.spawn(async move |this, cx| {
            let p = path.clone();
            let result = cx
                .background_spawn(async move { git::remove_worktree(&repo, &p, branch.as_deref()) })
                .await;
            let _ = this.update(cx, |this, cx| {
                this.busy = false;
                match result {
                    Ok(()) => cx.emit(DialogEvent::Deleted(path)),
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

impl Focusable for DeleteEnvDialog {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

fn consequence(icon_name: &str, color: gpui::Hsla, text: impl IntoElement) -> impl IntoElement {
    div()
        .flex()
        .items_start()
        .gap(px(10.))
        .child(
            div()
                .pt(px(2.))
                .child(icon(icon_name, 14.).text_color(color)),
        )
        .child(div().flex_1().min_w_0().child(text))
}

impl Render for DeleteEnvDialog {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let muted = theme::mix(0.35);
        let warn: gpui::Hsla = theme::warn().into();
        let branch = self.branch.clone();
        let mut list = div()
            .flex()
            .flex_col()
            .gap(px(8.))
            .mt(px(10.))
            .pt(px(10.))
            .border_t_1()
            .border_color(theme::border())
            .text_size(px(ui::TEXT_SM))
            .text_color(theme::text_muted());
        if self.dirty > 0 {
            list = list.child(consequence(
                "file-diff",
                warn,
                format!(
                    "{} uncommitted change{} here {} discarded.",
                    self.dirty,
                    if self.dirty == 1 { "" } else { "s" },
                    if self.dirty == 1 { "is" } else { "are" }
                ),
            ));
        }
        list = list.child(consequence(
            "git-branch",
            if self.delete_branch {
                theme::error().into()
            } else {
                muted
            },
            match (&branch, self.delete_branch) {
                (Some(b), false) => div()
                    .flex()
                    .flex_wrap()
                    .child("The ")
                    .child(
                        div()
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(theme::content())
                            .child(b.clone()),
                    )
                    .child(" branch and its commits are kept.")
                    .into_any_element(),
                (Some(b), true) => div()
                    .flex()
                    .flex_wrap()
                    .child("The ")
                    .child(
                        div()
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(theme::content())
                            .child(b.clone()),
                    )
                    .child(" branch is deleted too.")
                    .into_any_element(),
                (None, _) => div()
                    .child("The detached HEAD is kept in the repository.")
                    .into_any_element(),
            },
        ));
        if let Some(n) = self.unpushed.filter(|n| *n > 0) {
            list = list.child(consequence(
                "cloud-upload",
                if self.delete_branch { warn } else { muted },
                format!(
                    "{n} commit{} not pushed{}.",
                    if n == 1 { " is" } else { "s are" },
                    if self.delete_branch {
                        " and will be lost"
                    } else {
                        "; they stay on the branch"
                    }
                ),
            ));
        }
        // A branch that never reached a remote has no upstream, so `unpushed`
        // says nothing; count what only exists here instead.
        if self.delete_branch
            && let Some(n) = self.local_only
        {
            list = list.child(consequence(
                "git-commit",
                warn,
                format!(
                    "{n} commit{} only on this branch and will be lost.",
                    if n == 1 { " exists" } else { "s exist" }
                ),
            ));
        }

        let switch_on = self.delete_branch;
        ui::modal_panel("delete-env", 420.)
            .track_focus(&self.focus)
            .on_key_down(cx.listener(|this, ev: &gpui::KeyDownEvent, _, cx| {
                match ev.keystroke.key.as_str() {
                    "escape" if !this.busy => cx.emit(DialogEvent::Dismissed),
                    "enter" => this.submit(cx),
                    _ => {}
                }
            }))
            .child(ui::modal_header(
                "Delete environment?",
                None,
                ui::close_button("delete-env-close").on_click(cx.listener(|this, _, _, cx| {
                    if !this.busy {
                        cx.emit(DialogEvent::Dismissed)
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
                    .child(div().text_color(theme::text_muted()).child(
                        "This permanently deletes the worktree folder and everything inside it.",
                    ))
                    .child(
                        div()
                            .p(px(12.))
                            .rounded(px(12.))
                            .border_1()
                            .border_color(theme::border())
                            .bg(theme::fill_hover())
                            .child(
                                div()
                                    .flex()
                                    .items_start()
                                    .gap(px(10.))
                                    .text_size(px(ui::TEXT_SM))
                                    .text_color(theme::text_subtle())
                                    .child(
                                        div()
                                            .pt(px(2.))
                                            .child(icon("folder01", 14.).text_color(muted)),
                                    )
                                    .child(
                                        div()
                                            .flex_1()
                                            .min_w_0()
                                            .font_family(theme::FONT_MONO)
                                            .child(pretty_path(&self.path)),
                                    ),
                            )
                            .child(list),
                    )
                    .when(branch.is_some(), |d| {
                        d.child(
                            div()
                                .id("delete-env-branch")
                                .flex()
                                .items_center()
                                .justify_between()
                                .gap(px(12.))
                                .p(px(12.))
                                .rounded(px(12.))
                                .border_1()
                                .border_color(theme::border())
                                .cursor_pointer()
                                .child(
                                    div()
                                        .text_size(px(ui::TEXT_MD))
                                        .text_color(theme::text_muted())
                                        .child("Also delete the branch"),
                                )
                                .child(
                                    div()
                                        .relative()
                                        .w(px(36.))
                                        .h(px(20.))
                                        .rounded_full()
                                        .bg(if switch_on {
                                            theme::error().into()
                                        } else {
                                            theme::mix(0.2)
                                        })
                                        .child(
                                            div()
                                                .absolute()
                                                .top(px(2.))
                                                .left(px(if switch_on { 18. } else { 2. }))
                                                .size(px(16.))
                                                .rounded_full()
                                                .bg(gpui::white()),
                                        ),
                                )
                                .on_click(cx.listener(|this, _, _, cx| {
                                    if !this.busy {
                                        this.delete_branch = !this.delete_branch;
                                        cx.notify();
                                    }
                                })),
                        )
                    })
                    .when_some(self.error.clone(), |d, e| {
                        d.child(
                            div()
                                .text_size(px(ui::TEXT_SM))
                                .text_color(theme::error())
                                .child(e),
                        )
                    })
                    .child(
                        div()
                            .flex()
                            .justify_end()
                            .gap(px(8.))
                            .child(
                                ui::button(
                                    "delete-env-cancel",
                                    "Cancel",
                                    ButtonKind::Ghost,
                                    !self.busy,
                                )
                                .on_click(cx.listener(
                                    |this, _, _, cx| {
                                        if !this.busy {
                                            cx.emit(DialogEvent::Dismissed)
                                        }
                                    },
                                )),
                            )
                            .child(
                                ui::button(
                                    "delete-env-submit",
                                    if self.busy {
                                        "Deleting…"
                                    } else {
                                        "Delete environment"
                                    },
                                    ButtonKind::Danger,
                                    !self.busy,
                                )
                                .when(self.busy, |d| {
                                    d.child(ui::spinner(
                                        "delete-env-spin",
                                        12.,
                                        theme::error().into(),
                                    ))
                                })
                                .on_click(cx.listener(|this, _, _, cx| this.submit(cx))),
                            ),
                    ),
            )
    }
}
