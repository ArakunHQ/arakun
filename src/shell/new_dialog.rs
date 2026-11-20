//! The "New" dialog behind the + on the rail's Projects heading: start a
//! session in a project the user has, or add a project.

use std::path::PathBuf;

use gpui::AppContext as _;
use gpui::prelude::FluentBuilder as _;
use gpui::{
    Context, EventEmitter, FocusHandle, Focusable, FontWeight, Hsla, InteractiveElement as _,
    IntoElement, ParentElement as _, Render, ScrollHandle, SharedString,
    StatefulInteractiveElement as _, Styled as _, Window, div, px,
};

use crate::theme;
use crate::ui::{self, Type, Typography as _, icon};

pub enum NewDialogEvent {
    /// A new session in the project at this path, in its open environment. A
    /// path, not an index: a server sync can drop a project while the dialog is
    /// open and shift the indexes after it.
    Session(PathBuf),
    /// Add a project (the folder picker).
    Project,
    Dismissed,
}

/// A project a session can start in.
pub struct ProjectChoice {
    pub index: usize,
    pub path: PathBuf,
    pub name: SharedString,
    /// The environment the session would start in.
    pub env: SharedString,
    pub icon: u64,
    pub logo: u32,
}

/// What Enter picks. The two cards first, then the other projects.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Choice {
    Session(usize),
    Project,
}

pub struct NewDialog {
    /// The open project, which the Session card starts in.
    open: Option<ProjectChoice>,
    /// The other projects, in the rail's order.
    others: Vec<ProjectChoice>,
    at: usize,
    focus: FocusHandle,
    /// The list of other projects, which can be longer than it shows.
    scroll: ScrollHandle,
}

impl EventEmitter<NewDialogEvent> for NewDialog {}

impl NewDialog {
    pub fn new(
        open: Option<ProjectChoice>,
        others: Vec<ProjectChoice>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let focus = cx.focus_handle();
        window.focus(&focus, cx);
        Self {
            open,
            others,
            at: 0,
            focus,
            scroll: ScrollHandle::new(),
        }
    }

    fn choices(&self) -> Vec<Choice> {
        let mut out = Vec::new();
        if let Some(p) = &self.open {
            out.push(Choice::Session(p.index));
        }
        out.push(Choice::Project);
        out.extend(self.others.iter().map(|p| Choice::Session(p.index)));
        out
    }

    fn pick(&self, choice: Choice, cx: &mut Context<Self>) {
        cx.emit(match choice {
            Choice::Session(i) => {
                let Some(p) = self.open.iter().chain(&self.others).find(|p| p.index == i) else {
                    return;
                };
                NewDialogEvent::Session(p.path.clone())
            }
            Choice::Project => NewDialogEvent::Project,
        });
    }

    fn step(&mut self, by: isize, cx: &mut Context<Self>) {
        let n = self.choices().len() as isize;
        self.at = (self.at as isize + by).rem_euclid(n) as usize;
        // The list holds the choices after the cards: keep the picked one in view.
        let cards = n as usize - self.others.len();
        if let Some(row) = self.at.checked_sub(cards) {
            self.scroll.scroll_to_item(row);
        }
        cx.notify();
    }

    fn highlighted(&self, choice: Choice) -> bool {
        self.choices().get(self.at) == Some(&choice)
    }

    fn hover_to(&mut self, choice: Choice, cx: &mut Context<Self>) {
        if let Some(i) = self.choices().iter().position(|c| *c == choice)
            && i != self.at
        {
            self.at = i;
            cx.notify();
        }
    }

    /// One of the two big choices: an icon, a name, a line about it and its shortcut.
    #[allow(clippy::too_many_arguments)]
    fn card(
        &self,
        id: &'static str,
        choice: Option<Choice>,
        glyph: &'static str,
        title: &'static str,
        detail: SharedString,
        keys: &'static [&'static str],
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let lit = choice.is_some_and(|c| self.highlighted(c));
        div()
            .id(id)
            .flex()
            .flex_col()
            .flex_1()
            .min_w_0()
            .gap(ui::sp(10.))
            .p(ui::sp(14.))
            .rounded(ui::rad(12.))
            .border_1()
            .border_color(if lit {
                theme::border_strong()
            } else {
                theme::border()
            })
            .when(lit, |d| d.bg(theme::fill_hover()))
            .when(choice.is_none(), |d| d.opacity(0.5))
            .when_some(choice, |d, c| {
                d.cursor_pointer()
                    .on_hover(cx.listener(move |this, hovered: &bool, _, cx| {
                        if *hovered {
                            this.hover_to(c, cx)
                        }
                    }))
                    .on_click(cx.listener(move |this, _, _, cx| this.pick(c, cx)))
            })
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .justify_center()
                            .size(px(32.))
                            .rounded(ui::rad(8.))
                            .bg(theme::fill_raised())
                            .border_1()
                            .border_color(theme::border_subtle())
                            .child(icon(glyph, 16.).text_color(if lit {
                                theme::content()
                            } else {
                                theme::text_muted()
                            })),
                    )
                    .when(choice.is_some(), |d| d.child(ui::shortcut(keys))),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(ui::sp(2.))
                    .child(
                        div()
                            .ty(Type::Label)
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(theme::text())
                            .child(title),
                    )
                    .child(
                        div()
                            .ty(Type::Caption)
                            .text_color(theme::text_subtle())
                            .truncate()
                            .child(detail),
                    ),
            )
    }

    fn project_row(&self, p: &ProjectChoice, cx: &mut Context<Self>) -> impl IntoElement {
        let choice = Choice::Session(p.index);
        let lit = self.highlighted(choice);
        let logo: Hsla = gpui::rgb(p.logo).into();
        div()
            .id(("new-session-in", p.index))
            .flex()
            .items_center()
            .gap(ui::sp(10.))
            .h(px(32.))
            .px(ui::sp(10.))
            .rounded(ui::rad(8.))
            .cursor_pointer()
            .when(lit, |d| d.bg(theme::fill_hover()))
            .on_hover(cx.listener(move |this, hovered: &bool, _, cx| {
                if *hovered {
                    this.hover_to(choice, cx)
                }
            }))
            .on_click(cx.listener(move |this, _, _, cx| this.pick(choice, cx)))
            .child(
                div()
                    .flex()
                    .flex_none()
                    .items_center()
                    .justify_center()
                    .size(px(16.))
                    .child(ui::project_icon(p.icon, 1.5, logo)),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .ty(Type::Body)
                    .text_color(theme::text())
                    .child(p.name.clone()),
            )
            .child(
                div()
                    .flex_none()
                    .max_w(px(160.))
                    .truncate()
                    .ty(Type::Caption)
                    .text_color(theme::text_faint())
                    .child(p.env.clone()),
            )
            .child(icon("arrow-right01", 12.).text_color(if lit {
                theme::text_muted()
            } else {
                theme::text_faint()
            }))
    }
}

impl Focusable for NewDialog {
    fn focus_handle(&self, _: &gpui::App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for NewDialog {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let session_detail: SharedString = match &self.open {
            Some(p) => format!("In {} · {}", p.name, p.env).into(),
            None => "Add a project first".into(),
        };
        let session = self.open.as_ref().map(|p| Choice::Session(p.index));
        let others: Vec<_> = self
            .others
            .iter()
            .map(|p| self.project_row(p, cx).into_any_element())
            .collect();
        ui::modal_panel("new-dialog", 480.)
            .track_focus(&self.focus)
            .on_key_down(cx.listener(move |this, ev: &gpui::KeyDownEvent, _, cx| {
                match ev.keystroke.key.as_str() {
                    "escape" => cx.emit(NewDialogEvent::Dismissed),
                    "enter" => {
                        if let Some(c) = this.choices().get(this.at).copied() {
                            this.pick(c, cx)
                        }
                    }
                    "down" | "right" => this.step(1, cx),
                    "up" | "left" => this.step(-1, cx),
                    "tab" if ev.keystroke.modifiers.shift => this.step(-1, cx),
                    "tab" => this.step(1, cx),
                    _ => return,
                }
                cx.stop_propagation();
            }))
            // The cards answer to the same keys as everywhere else. Those keys
            // are bound to the shell's actions, which run before any key-down,
            // so the dialog takes the actions and closes itself first.
            .on_action(cx.listener(move |this, _: &super::NewSession, _, cx| {
                if let Some(c) = session {
                    this.pick(c, cx)
                }
            }))
            .on_action(
                cx.listener(|this, _: &super::AddProject, _, cx| this.pick(Choice::Project, cx)),
            )
            .child(ui::modal_header(
                "New",
                Some("Start a session, or add a project to work in.".into()),
                ui::close_button("new-dialog-close")
                    .on_click(cx.listener(|_, _, _, cx| cx.emit(NewDialogEvent::Dismissed))),
            ))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .min_h_0()
                    .gap(ui::sp(14.))
                    .p(ui::sp(16.))
                    .child(
                        div()
                            .flex()
                            .gap(ui::sp(10.))
                            .child(self.card(
                                "new-dialog-session",
                                session,
                                "message01",
                                "Session",
                                session_detail,
                                &["Ctrl", "T"],
                                cx,
                            ))
                            .child(self.card(
                                "new-dialog-project",
                                Some(Choice::Project),
                                "folder-add",
                                "Project",
                                "Open a folder or clone a repository".into(),
                                &["Ctrl", "O"],
                                cx,
                            )),
                    )
                    .when(!others.is_empty(), |d| {
                        d.child(
                            div()
                                .flex()
                                .flex_col()
                                .min_h_0()
                                .gap(ui::sp(4.))
                                .child(
                                    div()
                                        .px(ui::sp(10.))
                                        .ty(Type::Overline)
                                        .text_color(theme::text_faint())
                                        .child("SESSION IN ANOTHER PROJECT"),
                                )
                                .child(
                                    div()
                                        .id("new-dialog-projects")
                                        .flex()
                                        .flex_col()
                                        .gap(ui::sp(1.))
                                        .max_h(px(240.))
                                        .overflow_y_scroll()
                                        .track_scroll(&self.scroll)
                                        .children(others),
                                ),
                        )
                    }),
            )
    }
}

impl super::Shell {
    pub(super) fn open_new_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.menu = None;
        self.env_menu_open = false;
        let choice = |(index, p): (usize, &crate::model::Project)| ProjectChoice {
            index,
            path: p.path.clone(),
            name: p.name.clone(),
            env: p
                .environments
                .get(p.active_env)
                .map(|e| e.name.clone())
                .unwrap_or_default(),
            icon: p.icon,
            logo: p.logo,
        };
        let open = self
            .has_projects()
            .then(|| {
                self.projects
                    .get(self.active_project)
                    .map(|p| choice((self.active_project, p)))
            })
            .flatten();
        let others = self
            .projects
            .iter()
            .enumerate()
            .filter(|(i, _)| *i != self.active_project)
            .map(choice)
            .collect();
        let dialog = cx.new(|cx| NewDialog::new(open, others, window, cx));
        let sub = cx.subscribe_in(
            &dialog,
            window,
            |this, _, ev: &NewDialogEvent, window, cx| match ev {
                NewDialogEvent::Session(path) => {
                    let found = this.projects.iter().position(|p| &p.path == path);
                    this.close_modal(window, cx);
                    // The project went away while the dialog was open.
                    let Some(i) = found else { return };
                    if i != this.active_project {
                        this.select_project(i, window, cx);
                    }
                    this.new_session(window, cx);
                }
                NewDialogEvent::Project => {
                    this.close_modal(window, cx);
                    this.open_picker(window, cx);
                }
                NewDialogEvent::Dismissed => this.close_modal(window, cx),
            },
        );
        self.modal = Some((super::Modal::New(dialog), sub));
        cx.notify();
    }
}
