//! arakun's own "Add project" picker. Typing a name searches every git repository
//! found under the home folder (scanned once in the background, then cached);
//! typing a path (`~/`, `/`, `./`) browses folders like a shell completion;
//! pasting a clone URL (`https://github.com/o/r`, `git@host:o/r.git`,
//! `gh repo clone o/r`, or just `o/r`) offers to clone it.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::SystemTime;

use gpui::prelude::FluentBuilder as _;
use gpui::{
    App, AppContext as _, Context, Entity, EventEmitter, FocusHandle, Focusable, FontWeight, Hsla,
    InteractiveElement as _, IntoElement, ParentElement as _, Render, SharedString,
    StatefulInteractiveElement as _, Styled as _, Subscription, UniformListScrollHandle, Window,
    div, px, uniform_list,
};

use crate::clone_source::{CloneSource, parse_clone_source};
use crate::input::{InputEvent, TextInput};
use crate::model::{home, logo_for, pretty_path};
use crate::theme;
use crate::ui::{self, ButtonKind, icon};

pub enum PickerEvent {
    Picked(PathBuf),
    Dismissed,
}

#[derive(Clone)]
struct Repo {
    path: PathBuf,
    name: SharedString,
    branch: Option<SharedString>,
    touched: SystemTime,
}

#[derive(Clone)]
enum RowKind {
    Repo {
        branch: Option<SharedString>,
    },
    Folder,
    /// Clone `source` into the row's path, then add it.
    Clone(CloneSource),
}

#[derive(Clone)]
struct Row {
    path: PathBuf,
    name: SharedString,
    detail: SharedString,
    kind: RowKind,
}

/// Repositories found by the last scan, shared by every picker opened in this run.
static REPOS: Mutex<Option<Vec<Repo>>> = Mutex::new(None);

pub struct FolderPicker {
    input: Entity<TextInput>,
    existing: HashSet<PathBuf>,
    repos: Vec<Repo>,
    scanning: bool,
    rows: Vec<Row>,
    /// Folder being browsed in path mode.
    browsing: Option<PathBuf>,
    selected: usize,
    scroll: UniformListScrollHandle,
    /// A chosen folder that is not a repository yet, waiting for "Initialize".
    confirm_init: Option<PathBuf>,
    /// Repository that contains `confirm_init`, when the chosen folder sits inside one
    /// without being its root: the user picks between that repository and a new one.
    inside: Option<PathBuf>,
    error: Option<SharedString>,
    /// The repository being cloned, while `git clone` runs.
    cloning: Option<SharedString>,
    /// Shown as a card inside a page (the setup's last step), not as a dialog.
    inline: bool,
    /// Why the picker opened, in place of its usual line under the title.
    why: Option<SharedString>,
    _input_events: Subscription,
}

impl EventEmitter<PickerEvent> for FolderPicker {}

impl FolderPicker {
    pub fn new(existing: HashSet<PathBuf>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let input = cx.new(|cx| TextInput::new("Search repositories or type a path (~/…)", cx));
        let sub = cx.subscribe_in(&input, window, Self::on_input);
        window.focus(&input.focus_handle(cx), cx);
        let cached = REPOS.lock().unwrap().clone();
        let mut this = Self {
            input,
            existing,
            scanning: true,
            repos: cached.unwrap_or_default(),
            rows: Vec::new(),
            browsing: None,
            selected: 0,
            scroll: UniformListScrollHandle::new(),
            confirm_init: None,
            inside: None,
            error: None,
            cloning: None,
            inline: false,
            why: None,
            _input_events: sub,
        };
        this.refilter(cx);
        cx.spawn(async move |this, cx| {
            let repos = cx
                .background_spawn(crate::guard::task(async move { scan_repos(&home()) }))
                .await;
            *REPOS.lock().unwrap() = Some(repos.clone());
            let _ = this.update(cx, |this, cx| {
                this.repos = repos;
                this.scanning = false;
                this.refilter(cx);
            });
        })
        .detach();
        this
    }

    /// A card inside a page instead of a dialog: no title or close button,
    /// and a shorter list, since the page around it has both and its own length.
    pub fn inline(mut self) -> Self {
        self.inline = true;
        self
    }

    /// Says why the picker opened: what the user asked for needs a project.
    pub fn because(mut self, why: impl Into<SharedString>) -> Self {
        self.why = Some(why.into());
        self
    }

    /// What Enter would add, for a button outside the picker: the
    /// highlighted repository's name, or the folder being browsed.
    pub fn target_name(&self) -> Option<SharedString> {
        match (&self.browsing, self.rows.get(self.selected)) {
            (Some(dir), _) => Some(crate::model::dir_name(dir).into()),
            (None, Some(row)) => match &row.kind {
                RowKind::Clone(_) => Some(crate::model::dir_name(&row.path).into()),
                _ => Some(row.name.clone()),
            },
            (None, None) => None,
        }
    }

    /// Adds what `target_name` names, as Enter (or Ctrl+Enter on a folder) does.
    pub fn add_target(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.confirm_init.is_none() && self.browsing.is_some() {
            self.pick_current_folder(cx);
        } else {
            self.activate(self.selected, window, cx);
        }
    }

    fn on_input(
        &mut self,
        _: &Entity<TextInput>,
        event: &InputEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            InputEvent::Changed => {
                self.confirm_init = None;
                self.inside = None;
                self.error = None;
                self.selected = 0;
                self.refilter(cx);
            }
            InputEvent::Up => self.move_selection(-1, cx),
            InputEvent::Down => self.move_selection(1, cx),
            InputEvent::Submit => self.activate(self.selected, window, cx),
            InputEvent::SubmitAlt => self.pick_current_folder(cx),
            InputEvent::Complete => self.complete(window, cx),
            InputEvent::Cancel => {
                if self.confirm_init.take().is_some() {
                    self.inside = None;
                    cx.notify();
                } else {
                    cx.emit(PickerEvent::Dismissed);
                }
            }
        }
    }

    fn query(&self, cx: &App) -> String {
        self.input.read(cx).text().to_string()
    }

    fn refilter(&mut self, cx: &mut Context<Self>) {
        let query = self.query(cx);
        if is_path_query(&query) {
            let (dir, filter) = split_path_query(&query);
            self.rows = list_folders(&dir, &filter);
            self.browsing = Some(dir);
        } else {
            self.browsing = None;
            let q = query.trim().to_lowercase();
            let mut scored: Vec<(i64, &Repo)> = self
                .repos
                .iter()
                .filter_map(|r| {
                    if q.is_empty() {
                        return Some((0, r));
                    }
                    fuzzy_score(
                        &q,
                        &r.name.to_lowercase(),
                        &pretty_path(&r.path).to_lowercase(),
                    )
                    .map(|s| (s, r))
                })
                .collect();
            if q.is_empty() {
                scored.sort_by_key(|s| std::cmp::Reverse(s.1.touched));
            } else {
                scored.sort_by(|a, b| b.0.cmp(&a.0).then(b.1.touched.cmp(&a.1.touched)));
            }
            self.rows = scored
                .into_iter()
                .take(500)
                .map(|(_, r)| Row {
                    path: r.path.clone(),
                    name: r.name.clone(),
                    detail: pretty_path(&r.path).into(),
                    kind: RowKind::Repo {
                        branch: r.branch.clone(),
                    },
                })
                .collect();
            if let Some(source) = parse_clone_source(&query) {
                let path = clone_parent(&self.repos).join(&source.name);
                let row = Row {
                    name: format!("Clone {}", source.label).into(),
                    detail: format!("into {}", pretty_path(&path)).into(),
                    path,
                    kind: RowKind::Clone(source.clone()),
                };
                // A pasted URL means clone; a bare `owner/repo` may be a search.
                if source.shorthand {
                    self.rows.push(row);
                } else {
                    self.rows = vec![row];
                }
            }
        }
        self.selected = self.selected.min(self.rows.len().saturating_sub(1));
        cx.notify();
    }

    fn move_selection(&mut self, delta: isize, cx: &mut Context<Self>) {
        if self.rows.is_empty() {
            return;
        }
        let n = self.rows.len() as isize;
        self.selected = (self.selected as isize + delta).rem_euclid(n) as usize;
        self.scroll
            .scroll_to_item(self.selected, gpui::ScrollStrategy::Nearest);
        cx.notify();
    }

    /// Enter: add a repository, or step into a plain folder.
    fn activate(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        if self.cloning.is_some() {
            return;
        }
        if let Some(path) = self.confirm_init.clone() {
            self.init_and_pick(path, cx);
            return;
        }
        let Some(row) = self.rows.get(index).cloned() else {
            if self.browsing.is_some() {
                self.pick_current_folder(cx);
            }
            return;
        };
        match row.kind {
            RowKind::Repo { .. } => self.pick(row.path, cx),
            RowKind::Folder => self.browse_into(&row.path, window, cx),
            RowKind::Clone(source) => self.clone_and_pick(source, row.path, cx),
        }
    }

    /// Tab: open the highlighted folder (or repository) to browse inside it.
    fn complete(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(row) = self.rows.get(self.selected).cloned() {
            self.browse_into(&row.path, window, cx);
        }
    }

    fn browse_into(&mut self, path: &Path, _: &mut Window, cx: &mut Context<Self>) {
        let mut text = pretty_path(path);
        if !text.ends_with('/') {
            text.push('/');
        }
        self.input.update(cx, |input, cx| input.set_text(text, cx));
    }

    fn pick_current_folder(&mut self, cx: &mut Context<Self>) {
        let target = match &self.browsing {
            Some(dir) => dir.clone(),
            None => match self.rows.get(self.selected) {
                Some(row) => row.path.clone(),
                None => return,
            },
        };
        self.pick(target, cx);
    }

    fn pick(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        // Ctrl+Enter or the folder buttons could otherwise add the clone's folder
        // before it is done; the clone adds it itself when it finishes.
        if self.cloning.is_some() {
            return;
        }
        if !path.is_dir() {
            self.error = Some(format!("{} is not a folder", pretty_path(&path)).into());
            cx.notify();
            return;
        }
        match classify(&path) {
            Choice::Project(root) => self.add_root(root, cx),
            Choice::Nested(root) => {
                self.confirm_init = Some(path);
                self.inside = Some(root);
                cx.notify();
            }
            Choice::NotRepo => {
                self.confirm_init = Some(path);
                self.inside = None;
                cx.notify();
            }
        }
    }

    fn add_root(&mut self, root: PathBuf, cx: &mut Context<Self>) {
        if self.existing.contains(&root) {
            self.confirm_init = None;
            self.inside = None;
            self.error = Some(format!("{} is already in Arakun", pretty_path(&root)).into());
            cx.notify();
        } else {
            cx.emit(PickerEvent::Picked(root));
        }
    }

    /// The system's own folder dialog, for those who would rather click than type a path.
    fn choose_with_dialog(&mut self, cx: &mut Context<Self>) {
        let rx = cx.prompt_for_paths(gpui::PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some("Add project".into()),
        });
        cx.spawn(async move |this, cx| {
            let picked = rx
                .await
                .ok()
                .and_then(|r| r.ok())
                .flatten()
                .unwrap_or_default();
            if let Some(path) = picked.into_iter().next() {
                let _ = this.update(cx, |this, cx| this.pick(path, cx));
            }
        })
        .detach();
    }

    /// Clones on the background executor; the picker stays open with a notice
    /// until it is done, then adds the clone like any other repository.
    fn clone_and_pick(&mut self, source: CloneSource, dest: PathBuf, cx: &mut Context<Self>) {
        self.error = None;
        self.cloning = Some(source.label.clone());
        cx.notify();
        cx.spawn(async move |this, cx| {
            let target = dest.clone();
            // Git runs here, off the UI thread: the clone, or reading the origin of
            // a folder that is already there.
            let result = cx
                .background_spawn(crate::guard::task(async move {
                    clone_or_reuse(&source, &target)
                }))
                .await;
            let _ = this.update(cx, |this, cx| {
                this.cloning = None;
                match result {
                    Ok(Cloned::New) => this.add_root(dest, cx),
                    Ok(Cloned::Before) => this.pick(dest, cx),
                    Err(e) => {
                        this.error = Some(e.to_string().into());
                        cx.notify();
                    }
                }
            });
        })
        .detach();
    }

    fn init_and_pick(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        match crate::git::init(&path) {
            Ok(()) => cx.emit(PickerEvent::Picked(path)),
            Err(e) => {
                self.confirm_init = None;
                self.inside = None;
                self.error = Some(e.to_string().into());
                cx.notify();
            }
        }
    }

    fn render_row(&self, ix: usize, row: &Row, cx: &mut Context<Self>) -> impl IntoElement {
        let selected = ix == self.selected;
        let added = self.existing.contains(&row.path);
        let leading: gpui::AnyElement = match &row.kind {
            RowKind::Repo { .. } => {
                let (color, art) = logo_for(&row.path);
                let color: Hsla = gpui::rgb(color).into();
                div()
                    .flex()
                    .flex_none()
                    .items_center()
                    .justify_center()
                    .size(px(28.))
                    .rounded(crate::ui::rad(ui::RADIUS))
                    .bg(color.opacity(0.12))
                    .child(ui::pixel_art(ui::project_art(art), 2., color))
                    .into_any_element()
            }
            RowKind::Folder | RowKind::Clone(_) => {
                let name = match &row.kind {
                    RowKind::Clone(s) if s.on_github => "github",
                    RowKind::Clone(_) => "download04",
                    _ => "folder01",
                };
                div()
                    .flex()
                    .flex_none()
                    .items_center()
                    .justify_center()
                    .size(px(28.))
                    .rounded(crate::ui::rad(ui::RADIUS))
                    .bg(theme::fill_hover())
                    .child(icon(name, 14.).text_color(theme::text_subtle()))
                    .into_any_element()
            }
        };
        let trailing: gpui::AnyElement = if added {
            div()
                .text_size(px(ui::TEXT_XS))
                .text_color(theme::text_faint())
                .child("Added")
                .into_any_element()
        } else {
            match &row.kind {
                RowKind::Repo { branch } => div()
                    .flex()
                    .flex_none()
                    .items_center()
                    .gap(crate::ui::sp(4.))
                    .max_w(px(160.))
                    .text_size(px(ui::TEXT_XS))
                    .text_color(theme::text_subtle())
                    .when_some(branch.clone(), |d, b| {
                        d.child(icon("git-branch", 12.).text_color(theme::text_faint()))
                            .child(div().min_w_0().truncate().child(b))
                    })
                    .into_any_element(),
                RowKind::Folder => div()
                    .flex()
                    .items_center()
                    .text_size(px(ui::TEXT_XS))
                    .text_color(theme::text_faint())
                    .child(icon("arrow-right01", 12.).text_color(theme::text_faint()))
                    .into_any_element(),
                RowKind::Clone(_) => div()
                    .flex_none()
                    .text_size(px(ui::TEXT_XS))
                    .text_color(theme::text_subtle())
                    .child("Clone")
                    .into_any_element(),
            }
        };
        div()
            .id(("picker-row", ix))
            .w_full()
            .flex()
            .items_center()
            .gap(crate::ui::sp(10.))
            .h(px(44.))
            .px(crate::ui::sp(8.))
            .rounded(crate::ui::rad(ui::RADIUS))
            .cursor_pointer()
            .when(selected, |d| d.bg(theme::selection()))
            .when(!selected, |d| d.hover(|s| s.bg(theme::hover())))
            .when(added, |d| d.opacity(0.55))
            .child(leading)
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_w_0()
                    .child(
                        div()
                            .text_size(px(ui::TEXT_MD))
                            .font_weight(FontWeight::MEDIUM)
                            .truncate()
                            .child(row.name.clone()),
                    )
                    .child(
                        div()
                            .text_size(px(ui::TEXT_XS))
                            .font_family(theme::FONT_MONO)
                            .text_color(theme::text_faint())
                            .truncate()
                            .child(row.detail.clone()),
                    ),
            )
            .child(trailing)
            .on_click(cx.listener(move |this, ev: &gpui::ClickEvent, window, cx| {
                this.selected = ix;
                if ev.click_count() >= 2
                    || matches!(
                        this.rows.get(ix).map(|r| &r.kind),
                        Some(RowKind::Repo { .. } | RowKind::Clone(_))
                    )
                {
                    this.activate(ix, window, cx);
                } else {
                    cx.notify();
                }
            }))
    }
}

impl Focusable for FolderPicker {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.input.focus_handle(cx)
    }
}

impl Render for FolderPicker {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let path_mode = self.browsing.is_some();
        let section: SharedString = match &self.browsing {
            Some(dir) => pretty_path(dir).into(),
            None if self.query(cx).trim().is_empty() => "Recent repositories".into(),
            None => "Repositories".into(),
        };
        let count: SharedString = if self.scanning && !path_mode {
            "Scanning…".into()
        } else if path_mode {
            plural(self.rows.len(), "folder").into()
        } else {
            plural(self.rows.len(), "repository").into()
        };

        let field = div()
            .flex()
            .items_center()
            .gap(crate::ui::sp(8.))
            .h(px(ui::INPUT_H))
            .px(crate::ui::sp(10.))
            .rounded(crate::ui::rad(ui::RADIUS))
            .border_1()
            .border_color(theme::border_strong())
            .bg(theme::background())
            .text_size(px(ui::TEXT_MD))
            .line_height(px(20.))
            .child(
                icon(if path_mode { "folder-open" } else { "search01" }, 14.)
                    .text_color(theme::text_subtle()),
            )
            .child(self.input.clone());

        let rows = self.rows.clone();
        let list: gpui::AnyElement = if rows.is_empty() {
            let msg = if self.scanning && !path_mode {
                "Looking for git repositories in your home folder…"
            } else if path_mode {
                if cfg!(target_os = "macos") {
                    "No folders here. Press ⌘↩ to add this folder."
                } else {
                    "No folders here. Press Ctrl+Enter to add this folder."
                }
            } else {
                "No repositories match. Type a path like ~/code/app to browse, or paste a URL to clone."
            };
            div()
                .flex()
                .items_center()
                .justify_center()
                .h(px(120.))
                .px(crate::ui::sp(24.))
                .text_size(px(ui::TEXT_SM))
                .text_color(theme::text_subtle())
                .child(msg)
                .into_any_element()
        } else {
            let most = if self.inline { 5. } else { 8. };
            let height = (rows.len() as f32 * 44.).min(44. * most);
            uniform_list(
                "picker-list",
                rows.len(),
                cx.processor(move |this, range: std::ops::Range<usize>, _, cx| {
                    range
                        .map(|ix| this.render_row(ix, &rows[ix], cx).into_any_element())
                        .collect::<Vec<_>>()
                }),
            )
            .track_scroll(&self.scroll)
            .h(px(height))
            .min_h_0()
            .into_any_element()
        };

        let notice: Option<gpui::AnyElement> = if let Some(label) = &self.cloning {
            Some(
                div()
                    .flex()
                    .items_center()
                    .gap(crate::ui::sp(10.))
                    .mx(px(16.))
                    .mb(px(10.))
                    .p(crate::ui::sp(10.))
                    .rounded(crate::ui::rad(ui::RADIUS_LG))
                    .border_1()
                    .border_color(theme::border())
                    .bg(theme::fill_hover())
                    .text_size(px(ui::TEXT_SM))
                    .text_color(theme::text_muted())
                    .child(icon("download04", 14.).text_color(theme::text_subtle()))
                    // Closing the picker lets the clone finish, but nothing adds it then.
                    .child(format!("Cloning {label}… Keep this open until it's added."))
                    .into_any_element(),
            )
        } else if let Some(path) = &self.confirm_init {
            let message = match &self.inside {
                Some(root) => format!(
                    "{} is inside the repository {}.",
                    pretty_path(path),
                    pretty_path(root)
                ),
                None => format!("{} is not a git repository yet.", pretty_path(path)),
            };
            let enclosing = self.inside.clone();
            Some(
                div()
                    .flex()
                    .items_center()
                    .gap(crate::ui::sp(10.))
                    .mx(px(16.))
                    .mb(px(10.))
                    .p(crate::ui::sp(10.))
                    .rounded(crate::ui::rad(ui::RADIUS_LG))
                    .border_1()
                    .border_color(theme::border())
                    .bg(theme::fill_hover())
                    .text_size(px(ui::TEXT_SM))
                    .child(icon("alert-circle", 14.).text_color(theme::warn()))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_color(theme::text_muted())
                            .child(message),
                    )
                    .when_some(enclosing, |d, root| {
                        let label = format!("Add {}", crate::model::dir_name(&root));
                        d.child(
                            ui::button("picker-enclosing", label, ButtonKind::Ghost, true)
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.add_root(root.clone(), cx)
                                })),
                        )
                    })
                    .child(
                        ui::button(
                            "picker-init",
                            "Initialize and add",
                            ButtonKind::Primary,
                            true,
                        )
                        .on_click(cx.listener(|this, _, _, cx| {
                            if let Some(p) = this.confirm_init.clone() {
                                this.init_and_pick(p, cx);
                            }
                        })),
                    )
                    .into_any_element(),
            )
        } else {
            self.error.clone().map(|e| {
                div()
                    .mx(px(16.))
                    .mb(px(10.))
                    .text_size(px(ui::TEXT_SM))
                    .text_color(theme::error())
                    .child(e)
                    .into_any_element()
            })
        };

        let hint = |key: &str, label: &str| {
            div()
                .flex()
                .items_center()
                .gap(crate::ui::sp(4.))
                .child(
                    div()
                        .px(crate::ui::sp(4.))
                        .h(px(16.))
                        .rounded(crate::ui::rad(ui::RADIUS_SM))
                        .bg(theme::fill_selected())
                        .text_size(px(ui::TEXT_XS))
                        .line_height(px(16.))
                        .text_color(theme::text_subtle())
                        .child(key.to_string()),
                )
                .child(label.to_string())
        };

        let panel = if self.inline {
            ui::surface_card().id("folder-picker").w_full()
        } else {
            ui::modal_panel("folder-picker", 600.).child(ui::modal_header(
                "Add project",
                Some(self.why.clone().unwrap_or_else(|| {
                    "Pick a git repository. Each branch you work on becomes its own environment."
                        .into()
                })),
                ui::close_button("picker-close")
                    .on_click(cx.listener(|_, _, _, cx| cx.emit(PickerEvent::Dismissed))),
            ))
        };
        panel
            .child(
                div()
                    .px(crate::ui::sp(16.))
                    .pt(crate::ui::sp(12.))
                    .child(field),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .px(crate::ui::sp(20.))
                    .pt(crate::ui::sp(12.))
                    .pb(crate::ui::sp(4.))
                    .text_size(px(ui::TEXT_XS))
                    .text_color(theme::text_subtle())
                    .child(div().flex_1().min_w_0().truncate().child(section))
                    .child(count),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .min_h_0()
                    .px(crate::ui::sp(8.))
                    .pb(crate::ui::sp(8.))
                    .child(list),
            )
            .children(notice)
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(crate::ui::sp(12.))
                    .px(crate::ui::sp(16.))
                    .py(crate::ui::sp(10.))
                    .border_t_1()
                    .border_color(theme::stroke())
                    .text_size(px(ui::TEXT_XS))
                    .text_color(theme::text_subtle())
                    .child(hint("↑↓", "navigate"))
                    .child(hint("Enter", if path_mode { "open / add" } else { "add" }))
                    .child(hint("Tab", "browse"))
                    .child(div().flex_1())
                    .when(cfg!(windows), |d| {
                        d.child(
                            ui::button("picker-dialog", "Choose folder…", ButtonKind::Ghost, true)
                                .on_click(
                                    cx.listener(|this, _, _, cx| this.choose_with_dialog(cx)),
                                ),
                        )
                    })
                    .child(
                        ui::button("picker-home", "Browse home", ButtonKind::Ghost, true).on_click(
                            cx.listener(|this, _, _, cx| {
                                this.input.update(cx, |i, cx| i.set_text("~/", cx));
                            }),
                        ),
                    )
                    .when(path_mode, |d| {
                        d.child(
                            ui::button(
                                "picker-add-here",
                                "Add this folder",
                                ButtonKind::Primary,
                                true,
                            )
                            .on_click(cx.listener(|this, _, _, cx| this.pick_current_folder(cx))),
                        )
                    }),
            )
    }
}

fn plural(n: usize, word: &str) -> String {
    match (n, word) {
        (1, _) => format!("1 {word}"),
        (_, "repository") => format!("{n} repositories"),
        _ => format!("{n} {word}s"),
    }
}

enum Choice {
    /// A repository's root or one of its linked worktrees: added as its main checkout.
    Project(PathBuf),
    /// A folder further down inside this repository.
    Nested(PathBuf),
    NotRepo,
}

/// A folder below a repository's root is not quietly swapped for that repository:
/// an empty folder meant for a new project would vanish into it.
fn classify(path: &Path) -> Choice {
    let Ok(root) = crate::git::main_root(path) else {
        return Choice::NotRepo;
    };
    match crate::git::top_level(path) {
        Ok(top) if !same_dir(&top, path) => Choice::Nested(root),
        _ => Choice::Project(root),
    }
}

/// What `clone_or_reuse` found at the destination.
enum Cloned {
    /// Cloned just now.
    New,
    /// Already there, a clone of the same repository.
    Before,
}

/// Clones `source` into `dest`, or reuses `dest` when it already holds that
/// repository: `~/code/app` may hold another `app`, so its origin must match.
fn clone_or_reuse(source: &CloneSource, dest: &Path) -> anyhow::Result<Cloned> {
    if !dest.exists() {
        crate::git::clone(&source.url, source.on_github, dest)?;
        return Ok(Cloned::New);
    }
    let origin = dest
        .join(".git")
        .exists()
        .then(|| crate::git::origin_url(dest))
        .flatten();
    match origin.as_deref().and_then(parse_clone_source) {
        Some(found) if found.same_repo(source) => Ok(Cloned::Before),
        Some(found) => anyhow::bail!("{} already holds {}", pretty_path(dest), found.label),
        None => anyhow::bail!("{} already exists", pretty_path(dest)),
    }
}

/// Where new clones go, by the repositories the picker knows.
fn clone_parent(repos: &[Repo]) -> PathBuf {
    crate::clone_source::clone_parent(repos.iter().map(|r| r.path.as_path()), &home())
}

/// Git prints `C:/x` where Windows reads `C:\x`; compare the folders themselves.
fn same_dir(a: &Path, b: &Path) -> bool {
    match (std::fs::canonicalize(a), std::fs::canonicalize(b)) {
        (Ok(a), Ok(b)) => a == b,
        _ => a == b,
    }
}

/// Separators a typed path may use. Windows also takes `\`; elsewhere a backslash
/// is an ordinary character in a folder name.
const SEPARATORS: &[char] = if cfg!(windows) { &['/', '\\'] } else { &['/'] };

pub(crate) fn is_path_query(q: &str) -> bool {
    if q.starts_with('/') || q.starts_with('~') || q.starts_with("./") || q.starts_with("../") {
        return true;
    }
    cfg!(windows)
        && (q.starts_with('\\')
            || q.starts_with(".\\")
            || q.starts_with("..\\")
            || is_drive_root(q))
}

/// `C:\` or `C:/` (`C:` alone means that drive's current folder, not its root).
fn is_drive_root(q: &str) -> bool {
    let b = q.as_bytes();
    b.len() >= 3
        && b[0].is_ascii_alphabetic()
        && b[1] == b':'
        && SEPARATORS.contains(&(b[2] as char))
}

fn expand(path: &str) -> PathBuf {
    if path == "~" {
        return home();
    }
    if let Some(rest) = path
        .strip_prefix('~')
        .and_then(|r| r.strip_prefix(SEPARATORS))
    {
        return home().join(rest);
    }
    let relative = path.starts_with("./")
        || path.starts_with("../")
        || (cfg!(windows) && (path.starts_with(".\\") || path.starts_with("..\\")));
    if relative {
        return home().join(path);
    }
    PathBuf::from(path)
}

/// `~/code/ap` → (`~/code`, `ap`).
pub(crate) fn split_path_query(q: &str) -> (PathBuf, String) {
    match q.rfind(SEPARATORS) {
        Some(i) => {
            // A root keeps its separator: `/`, and `C:\` on Windows.
            let dir = &q[..i];
            let dir = if dir.is_empty() || dir.ends_with(':') {
                &q[..=i]
            } else {
                dir
            };
            (expand(dir), q[i + 1..].to_string())
        }
        None => (expand(q), String::new()),
    }
}

fn list_folders(dir: &Path, filter: &str) -> Vec<Row> {
    let Ok(read) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let f = filter.to_lowercase();
    let show_hidden = filter.starts_with('.');
    let mut rows: Vec<(bool, String, Row)> = read
        .filter_map(Result::ok)
        .filter(|e| e.path().is_dir())
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            if name.starts_with('.') && !show_hidden {
                return None;
            }
            let lower = name.to_lowercase();
            if !f.is_empty() && !lower.contains(&f) {
                return None;
            }
            let path = e.path();
            let kind = if path.join(".git").exists() {
                RowKind::Repo {
                    branch: crate::git::head_branch_fast(&path).map(Into::into),
                }
            } else {
                RowKind::Folder
            };
            let row = Row {
                detail: pretty_path(&path).into(),
                name: name.into(),
                path,
                kind,
            };
            Some((!lower.starts_with(&f), lower, row))
        })
        .collect();
    // Prefix matches first, then alphabetical.
    rows.sort_by(|a, b| (a.0, &a.1).cmp(&(b.0, &b.1)));
    rows.into_iter().take(1000).map(|(_, _, row)| row).collect()
}

/// Subsequence match, rewarding hits at the start of the name and of words.
pub(crate) fn fuzzy_score(q: &str, name: &str, path: &str) -> Option<i64> {
    if name.starts_with(q) {
        return Some(10_000 - name.len() as i64);
    }
    if let Some(i) = name.find(q) {
        return Some(8_000 - i as i64 * 10 - name.len() as i64);
    }
    if path.contains(q) {
        return Some(5_000 - path.len() as i64);
    }
    let mut score = 0i64;
    let mut last = None;
    let hay: Vec<char> = path.chars().collect();
    let mut pos = 0;
    for qc in q.chars() {
        let found = hay[pos..].iter().position(|&c| c == qc)?;
        let at = pos + found;
        score += match last {
            Some(l) if at == l + 1 => 15,
            _ => 1,
        };
        if at == 0 || matches!(hay[at - 1], '/' | '-' | '_' | '.' | ' ') {
            score += 10;
        }
        last = Some(at);
        pos = at + 1;
    }
    Some(score)
}

const SKIP: &[&str] = &[
    "node_modules",
    "target",
    "dist",
    "build",
    "out",
    "vendor",
    "venv",
    "__pycache__",
    "site-packages",
    "Library",
    "snap",
    "go",
    "arakun-workspaces",
    "Steam",
    "Games",
];

/// Breadth-first walk of `root` for folders with a `.git` directory (linked
/// worktrees have a `.git` file and are skipped; they belong to their project).
fn scan_repos(root: &Path) -> Vec<Repo> {
    let mut out = Vec::new();
    let mut queue = std::collections::VecDeque::from([(root.to_path_buf(), 0usize)]);
    let mut visited = 0usize;
    while let Some((dir, depth)) = queue.pop_front() {
        visited += 1;
        if visited > 40_000 {
            break;
        }
        let git = dir.join(".git");
        if depth > 0 && git.is_dir() {
            let touched = ["index", "HEAD", "logs/HEAD"]
                .iter()
                .filter_map(|f| {
                    std::fs::metadata(git.join(f))
                        .and_then(|m| m.modified())
                        .ok()
                })
                .max()
                .unwrap_or(SystemTime::UNIX_EPOCH);
            out.push(Repo {
                name: crate::model::dir_name(&dir).into(),
                branch: crate::git::head_branch_fast(&dir).map(Into::into),
                path: dir,
                touched,
            });
            continue;
        }
        if depth >= 5 {
            continue;
        }
        let Ok(read) = std::fs::read_dir(&dir) else {
            continue;
        };
        for e in read.filter_map(Result::ok) {
            let Ok(t) = e.file_type() else { continue };
            if !t.is_dir() {
                continue;
            }
            let name = e.file_name();
            let name = name.to_string_lossy();
            if name.starts_with('.') || SKIP.contains(&name.as_ref()) {
                continue;
            }
            queue.push_back((e.path(), depth + 1));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn git(dir: &Path, args: &[&str]) {
        let ok = crate::git::unpinned(&mut crate::git::git(dir))
            .args([
                "-c",
                "user.name=t",
                "-c",
                "user.email=t@t",
                "-c",
                "commit.gpgsign=false",
            ])
            .args(args)
            .status()
            .unwrap()
            .success();
        assert!(ok, "git {args:?}");
    }

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("arakun-picker-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn a_folder_inside_a_repository_is_not_swapped_for_it() {
        let base = scratch("classify");
        let repo = base.join("repo");
        let empty = repo.join("new-project");
        let outside = base.join("outside");
        for d in [&empty, &outside] {
            std::fs::create_dir_all(d).unwrap();
        }
        git(&repo, &["init", "-q"]);
        git(&repo, &["commit", "-q", "--allow-empty", "-m", "init"]);
        let linked = base.join("linked");
        git(
            &repo,
            &[
                "worktree",
                "add",
                "-q",
                "-b",
                "side",
                linked.to_str().unwrap(),
            ],
        );

        // The repository's root and a linked worktree still add the main checkout.
        assert!(matches!(classify(&repo), Choice::Project(r) if same_dir(&r, &repo)));
        assert!(matches!(classify(&linked), Choice::Project(r) if same_dir(&r, &repo)));
        // A folder inside it asks, naming the repository it is in.
        assert!(matches!(classify(&empty), Choice::Nested(r) if same_dir(&r, &repo)));
        assert!(matches!(classify(&outside), Choice::NotRepo));

        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn typed_paths_split_into_folder_and_filter() {
        assert!(is_path_query("~/code"));
        assert!(is_path_query("/usr"));
        assert!(!is_path_query("arakun"));
        assert_eq!(
            split_path_query("~/code/ap"),
            (home().join("code"), "ap".into())
        );
        assert_eq!(split_path_query("/"), (PathBuf::from("/"), String::new()));
    }

    #[test]
    fn an_existing_folder_is_reused_only_when_its_origin_matches() {
        let dest = std::env::temp_dir().join(format!("arakun-reuse-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dest);
        std::fs::create_dir_all(&dest).unwrap();
        // `git` drops GIT_DIR and the like, so a hook running the tests can't
        // point these at its own checkout.
        git(&dest, &["init", "-q"]);
        git(
            &dest,
            &["remote", "add", "origin", "git@github.com:owner-a/app.git"],
        );
        let source = |q: &str| parse_clone_source(q).unwrap();
        assert!(matches!(
            clone_or_reuse(&source("https://github.com/owner-a/app"), &dest),
            Ok(Cloned::Before)
        ));
        let other = clone_or_reuse(&source("https://github.com/owner-b/app"), &dest);
        assert!(other.is_err_and(|e| e.to_string().ends_with("already holds owner-a/app")));
        let _ = std::fs::remove_dir_all(&dest);
    }

    #[test]
    fn clones_go_where_most_repositories_are() {
        let repo = |p: PathBuf| Repo {
            name: "x".into(),
            branch: None,
            path: p,
            touched: SystemTime::UNIX_EPOCH,
        };
        assert_eq!(clone_parent(&[]), home().join("code"));
        let dev = home().join("Developer");
        let repos = [
            repo(dev.join("a")),
            repo(dev.join("b")),
            repo(home().join("other/c")),
            repo(home().join("d")),
        ];
        assert_eq!(clone_parent(&repos), dev);
    }

    #[cfg(windows)]
    #[test]
    fn windows_paths_take_drives_and_backslashes() {
        assert!(is_path_query(r"C:\Users"));
        assert!(is_path_query("c:/Users"));
        assert!(is_path_query(r"\server\share"));
        assert!(!is_path_query("C:"));
        assert_eq!(
            split_path_query(r"C:\"),
            (PathBuf::from(r"C:\"), String::new())
        );
        assert_eq!(
            split_path_query(r"C:\Users\Da"),
            (PathBuf::from(r"C:\Users"), "Da".into())
        );
        assert_eq!(
            split_path_query(r"~\Desktop\pro"),
            (home().join("Desktop"), "pro".into())
        );
        // What browsing into a folder writes back into the field.
        assert_eq!(
            split_path_query(r"~/Desktop\proyectos/"),
            (home().join(r"Desktop\proyectos"), String::new())
        );
    }

    #[cfg(not(windows))]
    #[test]
    fn a_backslash_is_part_of_a_name_outside_windows() {
        assert!(!is_path_query(r"C:\Users"));
        assert_eq!(split_path_query(r"~/a\b"), (home(), r"a\b".into()));
    }
}
