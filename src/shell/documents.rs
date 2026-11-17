//! File and diff tabs and the Explorer: opening, reloading and drawing documents.

use super::*;

impl Shell {
    pub(super) fn ensure_explorer(&mut self, cx: &mut Context<Self>) {
        if !self.has_projects() {
            return;
        }
        let env = self.env();
        let root = env.explorer.root.clone();
        if !env.explorer.children.contains_key(&root) && !env.explorer.loading.contains(&root) {
            self.env_mut().explorer.loading.insert(root.clone());
            let env_path = self.env().path.clone();
            self.reload_dirs(env_path, vec![root], cx);
        }
    }

    pub(super) fn reload_dirs(
        &mut self,
        env_path: std::path::PathBuf,
        dirs: Vec<std::path::PathBuf>,
        cx: &mut Context<Self>,
    ) {
        cx.spawn(async move |this, cx| {
            let listed = cx
                .background_spawn(async move {
                    dirs.into_iter()
                        .map(|d| {
                            let entries = explorer::list_dir(&d);
                            (d, entries)
                        })
                        .collect::<Vec<_>>()
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                if let Some(env) = this.find_env_mut(&env_path) {
                    for (dir, entries) in listed {
                        env.explorer.loading.remove(&dir);
                        env.explorer.children.insert(dir, entries);
                    }
                    cx.notify();
                }
            });
        })
        .detach();
    }

    pub(super) fn toggle_dir(&mut self, dir: std::path::PathBuf, cx: &mut Context<Self>) {
        let env = self.env_mut();
        env.explorer.selected = Some(dir.clone());
        if !env.explorer.expanded.remove(&dir) {
            env.explorer.expanded.insert(dir.clone());
            if !env.explorer.children.contains_key(&dir) {
                let env_path = env.path.clone();
                self.reload_dirs(env_path, vec![dir], cx);
            }
        }
        cx.notify();
    }

    /// Opens (or focuses) a read-only tab for a file or a diff and loads it.
    pub(super) fn open_doc_tab(
        &mut self,
        kind: TabKind,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.has_projects() {
            return;
        }
        let env_path = self.env().path.clone();
        self.open_doc_in(&env_path, kind, true, cx);
    }

    /// Opens a file or diff tab in any environment, focusing it only when
    /// `activate` is set; returns the tab's id.
    pub(super) fn open_doc_in(
        &mut self,
        env_path: &std::path::Path,
        kind: TabKind,
        activate: bool,
        cx: &mut Context<Self>,
    ) -> Option<u64> {
        let visible = self.has_projects() && self.env().path == env_path;
        let (title, key): (SharedString, String) = match &kind {
            TabKind::File { path } => (
                model::dir_name(path).into(),
                format!("f:{}", path.display()),
            ),
            TabKind::Diff { path, .. } => {
                let name = path.rsplit('/').next().unwrap_or(path);
                (format!("{name} (diff)").into(), format!("d:{path}"))
            }
            _ => return None,
        };
        let env = self.find_env_mut(env_path)?;
        let existing = env.tabs.iter().position(|t| match &t.kind {
            TabKind::File { path } => key == format!("f:{}", path.display()),
            TabKind::Diff { path, .. } => key == format!("d:{path}"),
            _ => false,
        });
        let index = match existing {
            Some(i) => i,
            None => {
                let mut tab = Tab::new(title, kind.clone());
                tab.doc = Some(Document::Loading);
                env.tabs.push(tab);
                env.tabs.len() - 1
            }
        };
        let id = env.tabs[index].id;
        if activate {
            env.active_tab = index;
            if visible {
                self.note_active_tab(cx);
            }
        }
        self.reload_doc(env_path.to_path_buf(), kind, cx);
        cx.notify();
        Some(id)
    }

    /// Scrolls the file tab `id` to `line` (1-based) and marks it, also
    /// while the file is still loading.
    pub(super) fn reveal_line(
        &mut self,
        env_path: &std::path::Path,
        id: u64,
        line: usize,
        cx: &mut Context<Self>,
    ) {
        let Some(tab) = self
            .find_env_mut(env_path)
            .and_then(|e| e.tabs.iter_mut().find(|t| t.id == id))
        else {
            return;
        };
        // A sheet's first item is its row of column letters, so Excel's row N is item N.
        let sheet = matches!(&tab.kind, TabKind::File { path } if explorer::file_kind(path) == explorer::FileKind::Sheet);
        let ix = if sheet { line } else { line.saturating_sub(1) };
        tab.target_line = Some(ix);
        tab.scroll.scroll_to_item(ix, gpui::ScrollStrategy::Center);
        cx.notify();
    }

    /// Loads (or reloads) the document of an open file or diff tab.
    pub(super) fn reload_doc(
        &mut self,
        env_path: std::path::PathBuf,
        kind: TabKind,
        cx: &mut Context<Self>,
    ) {
        if let TabKind::File { path } = &kind
            && explorer::file_kind(path) == explorer::FileKind::Image
        {
            if let Some(env) = self.find_env_mut(&env_path)
                && let Some(tab) = env.tabs.iter_mut().find(|t| same_doc(&t.kind, &kind))
            {
                tab.doc = Some(Document::Image(path.clone()));
                cx.notify();
            }
            return;
        }
        if let TabKind::File { path } = &kind
            && explorer::file_kind(path) == explorer::FileKind::Sheet
        {
            let file = path.clone();
            cx.spawn(async move |this, cx| {
                let doc = cx
                    .background_spawn(async move {
                        match crate::sheet::read(&file) {
                            Ok(book) => Document::Sheet(Arc::new(book)),
                            Err(e) => Document::Failed(e.into()),
                        }
                    })
                    .await;
                let _ = this.update(cx, |this, cx| {
                    if let Some(env) = this.find_env_mut(&env_path)
                        && let Some(tab) = env.tabs.iter_mut().find(|t| same_doc(&t.kind, &kind))
                    {
                        if let Document::Sheet(book) = &doc {
                            tab.sheet = tab.sheet.min(book.sheets.len() - 1);
                        }
                        tab.doc = Some(doc);
                        cx.notify();
                    }
                });
            })
            .detach();
            return;
        }
        cx.spawn(async move |this, cx| {
            let k = kind.clone();
            let ep = env_path.clone();
            let doc = cx
                .background_spawn(async move {
                    let result: Result<(String, Vec<SharedString>), String> = match &k {
                        TabKind::File { path } => explorer::read_lines(path)
                            .map(|lines| (path.display().to_string(), lines)),
                        TabKind::Diff { path, untracked } => git::diff(&ep, path, *untracked)
                            .map(|d| {
                                (
                                    path.clone(),
                                    d.lines()
                                        .map(|l| SharedString::from(l.replace('\t', "    ")))
                                        .collect(),
                                )
                            })
                            .map_err(|e| e.to_string()),
                        _ => Ok((String::new(), Vec::new())),
                    };
                    match result {
                        Ok((path, lines)) => {
                            let is_diff = matches!(&k, TabKind::Diff { .. });
                            // Diff markers are not code; headers feed nothing.
                            let code: Vec<String> = if is_diff {
                                lines
                                    .iter()
                                    .map(|l| highlight::diff_code(l).to_string())
                                    .collect()
                            } else {
                                lines.iter().map(|l| l.to_string()).collect()
                            };
                            let mut doc = model::Doc::plain(lines);
                            if let Some(spans) = highlight::highlight(&path, &code) {
                                doc.spans = spans
                                    .into_iter()
                                    .enumerate()
                                    .map(|(i, spans)| {
                                        if is_diff && highlight::diff_shifted(&doc.lines[i]) {
                                            spans
                                                .into_iter()
                                                .map(|(r, c)| (r.start + 1..r.end + 1, c))
                                                .collect()
                                        } else {
                                            spans
                                        }
                                    })
                                    .collect();
                            }
                            Document::Lines(Arc::new(doc))
                        }
                        Err(e) => Document::Failed(e.into()),
                    }
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                if let Some(env) = this.find_env_mut(&env_path)
                    && let Some(tab) = env.tabs.iter_mut().find(|t| same_doc(&t.kind, &kind))
                {
                    tab.doc = Some(doc);
                    if tab.markdown.is_some() {
                        tab.markdown = parse_markdown(tab);
                    }
                    cx.notify();
                }
            });
        })
        .detach();
    }

    /// Closes the diff tab of a file that no longer has changes, in whichever
    /// environment holds it. Runs from `render`, where a window is at hand.
    pub(super) fn close_diff_tab(
        &mut self,
        env_path: &std::path::Path,
        file: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let is_diff = |t: &Tab| matches!(&t.kind, TabKind::Diff { path, .. } if path == file);
        if self.has_projects() && self.env().path == env_path {
            if self.env().changes.iter().any(|c| c.path == file) {
                return;
            }
            if let Some(i) = self.env().tabs.iter().position(&is_diff) {
                self.discard_tab(i, window, cx);
            }
            return;
        }
        let Some(env) = self.find_env_mut(env_path) else {
            return;
        };
        if env.changes.iter().any(|c| c.path == file) {
            return;
        }
        if let Some(i) = env.tabs.iter().position(is_diff) {
            env.tabs.remove(i);
            if env.tabs.is_empty() {
                env.tabs.push(Tab::new("New session", TabKind::NewSession));
            }
            if env.active_tab >= env.tabs.len() || env.active_tab > i {
                env.active_tab = env.active_tab.saturating_sub(1);
            }
            self.unfloat_github(cx);
            self.save(cx);
            cx.notify();
        }
    }
}

/// A file tab whose file is markdown, so it can be shown rendered.
pub(super) fn is_markdown(tab: &Tab) -> bool {
    matches!(&tab.kind, TabKind::File { path } if path
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| ["md", "mdx", "markdown"].contains(&e.to_ascii_lowercase().as_str())))
}

/// The loaded text of a markdown tab, parsed for the MD Viewer.
fn parse_markdown(tab: &Tab) -> Option<Rc<Vec<crate::agent::markdown::Block>>> {
    let Some(Document::Lines(doc)) = &tab.doc else {
        return None;
    };
    let text = doc
        .lines
        .iter()
        .map(|l| l.as_ref())
        .collect::<Vec<&str>>()
        .join("\n");
    Some(Rc::new(crate::agent::markdown::parse(&text)))
}

pub(super) fn open_external(path: &std::path::Path) {
    crate::platform::open(path);
}

pub(super) fn same_doc(a: &TabKind, b: &TabKind) -> bool {
    match (a, b) {
        (TabKind::File { path: x }, TabKind::File { path: y }) => x == y,
        (TabKind::Diff { path: x, .. }, TabKind::Diff { path: y, .. }) => x == y,
        _ => false,
    }
}

/// Icon tint per file type, enough to scan a tree at a glance.
pub(super) fn file_icon(name: &str) -> (&'static str, Hsla) {
    let ext = name.rsplit_once('.').map_or("", |(_, e)| e);
    let [blue, _, green, violet, coral, cyan, rose, sand] = theme::CATEGORY;
    let color: u32 = match ext {
        "rs" => coral,
        "ts" | "tsx" | "py" => blue,
        "js" | "jsx" | "mjs" | "cjs" => sand,
        "md" | "mdx" => return ("file01", theme::mix(0.6)),
        "json" | "toml" | "yaml" | "yml" | "lock" => violet,
        "css" | "scss" | "html" => rose,
        "sh" | "zsh" | "bash" | "fish" => green,
        "png" | "jpg" | "jpeg" | "gif" | "svg" | "webp" | "ico" => cyan,
        "xlsx" | "xlsm" | "xls" | "ods" | "csv" | "tsv" => green,
        _ => return ("file01", theme::mix(0.45)),
    };
    let icon = match ext {
        "sh" | "zsh" | "bash" | "fish" => "file-script",
        "png" | "jpg" | "jpeg" | "gif" | "svg" | "webp" | "ico" => "image-add01",
        _ => "file01",
    };
    (icon, gpui::rgb(color).into())
}

impl Shell {
    pub(super) fn render_explorer(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let env = self.env();
        let ex = &env.explorer;
        if !ex.children.contains_key(&ex.root) {
            return panel_empty("Loading files…").into_any_element();
        }
        let rows = Rc::new(ex.rows());
        if rows.is_empty() {
            return panel_empty("This worktree is empty.").into_any_element();
        }
        let selected = ex.selected.clone();
        let changed: Rc<std::collections::HashSet<std::path::PathBuf>> =
            Rc::new(env.changes.iter().map(|c| env.path.join(&c.path)).collect());
        let count = rows.len();
        uniform_list(
            "explorer",
            count,
            cx.processor(move |_this, range: Range<usize>, _, cx| {
                range
                    .map(|ix| {
                        let row = &rows[ix];
                        let entry = row.entry.clone();
                        let is_selected = selected.as_ref() == Some(&entry.path);
                        let modified = !entry.is_dir && changed.contains(&entry.path);
                        let (file_icon_name, file_color) = file_icon(&entry.name);
                        let name_color = if entry.ignored {
                            theme::mix(0.35)
                        } else if modified {
                            theme::warn().into()
                        } else if entry.is_dir {
                            theme::mix(0.85)
                        } else {
                            theme::mix(0.7)
                        };
                        div()
                            .id(("file", ix))
                            .w_full()
                            .flex()
                            .items_center()
                            .gap(crate::ui::sp(6.))
                            .h(px(26.))
                            .pl(crate::ui::sp(8. + row.depth as f32 * 12.))
                            .pr(crate::ui::sp(8.))
                            .rounded(crate::ui::rad(4.))
                            .text_size(px(ui::TEXT_MD))
                            .text_color(name_color)
                            .cursor_pointer()
                            .when(is_selected, |d| d.bg(theme::selection()))
                            .when(!is_selected, |d| d.hover(|s| s.bg(theme::hover())))
                            .child(div().flex().flex_none().w(px(12.)).when(entry.is_dir, |d| {
                                d.child(
                                    icon(
                                        if row.expanded {
                                            "arrow-down01"
                                        } else {
                                            "arrow-right01"
                                        },
                                        12.,
                                    )
                                    .text_color(theme::text_faint()),
                                )
                            }))
                            .child(if entry.is_dir {
                                icon(
                                    if row.expanded {
                                        "folder-open"
                                    } else {
                                        "folder01"
                                    },
                                    14.,
                                )
                                .text_color(if entry.ignored {
                                    theme::mix(0.3)
                                } else {
                                    theme::mix(0.55)
                                })
                            } else {
                                icon(file_icon_name, 14.).text_color(if entry.ignored {
                                    file_color.opacity(0.4)
                                } else {
                                    file_color
                                })
                            })
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .truncate()
                                    .child(entry.name.clone()),
                            )
                            .when(modified, |d| {
                                d.child(div().size(px(6.)).rounded_full().bg(theme::warn()))
                            })
                            .on_click(cx.listener(move |this, _, window, cx| {
                                if entry.is_dir {
                                    this.toggle_dir(entry.path.clone(), cx);
                                } else {
                                    this.env_mut().explorer.selected = Some(entry.path.clone());
                                    this.open_doc_tab(
                                        TabKind::File {
                                            path: entry.path.clone(),
                                        },
                                        window,
                                        cx,
                                    );
                                }
                            }))
                            .into_any_element()
                    })
                    .collect::<Vec<_>>()
            }),
        )
        .track_scroll(&self.explorer_scroll)
        .flex_1()
        .min_h_0()
        .py(crate::ui::sp(6.))
        .px(crate::ui::sp(6.))
        .into_any_element()
    }

    /// File and diff tabs: monospace lines with a gutter, virtualized.
    /// The working file behind a file or diff tab, and the line to open it at
    /// (a diff's first hunk).
    pub(super) fn doc_edit_target(&self, tab: &Tab) -> Option<(std::path::PathBuf, Option<usize>)> {
        match &tab.kind {
            TabKind::File { path } => Some((path.clone(), None)),
            TabKind::Diff { path, .. } => {
                let line = match &tab.doc {
                    Some(Document::Lines(doc)) => doc.lines.iter().find_map(|l| hunk_start(l)),
                    _ => None,
                };
                Some((self.env().path.join(path), line))
            }
            _ => None,
        }
    }

    /// Opens `path` in the user's editor in a dock terminal; file tabs reload
    /// when it exits.
    pub(super) fn edit_in_terminal(
        &mut self,
        path: &std::path::Path,
        line: Option<usize>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match editor_command(path, line) {
            Some(cmd) => self.spawn_terminal_command(Some(cmd), true, window, cx),
            None => eprintln!("arakun: set $EDITOR to edit files"),
        }
    }

    /// Switches a markdown file tab of the current environment between its raw
    /// lines and the rendered MD Viewer.
    pub(super) fn toggle_md_viewer(&mut self, id: u64, cx: &mut Context<Self>) {
        let Some(tab) = self.env_mut().tabs.iter_mut().find(|t| t.id == id) else {
            return;
        };
        tab.markdown = match tab.markdown {
            Some(_) => None,
            None => parse_markdown(tab),
        };
        cx.notify();
    }

    /// Shows sheet `index` of the spreadsheet tab `id` in the current environment.
    pub(super) fn select_sheet(&mut self, id: u64, index: usize, cx: &mut Context<Self>) {
        if let Some(tab) = self.env_mut().tabs.iter_mut().find(|t| t.id == id) {
            tab.sheet = index;
            tab.scroll = UniformListScrollHandle::new();
            cx.notify();
        }
    }

    /// Re-reads the open file tabs of an environment (after an editor exits).
    pub(super) fn reload_file_tabs(&mut self, env_path: &std::path::Path, cx: &mut Context<Self>) {
        let Some(env) = self.find_env_mut(env_path) else {
            return;
        };
        let kinds: Vec<TabKind> = env
            .tabs
            .iter()
            .filter(|t| matches!(t.kind, TabKind::File { .. }))
            .map(|t| t.kind.clone())
            .collect();
        for kind in kinds {
            self.reload_doc(env_path.to_path_buf(), kind, cx);
        }
    }

    pub(super) fn render_document(
        &self,
        tab: &Tab,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let (subtitle, is_diff): (SharedString, bool) = match &tab.kind {
            TabKind::File { path } => (
                path.strip_prefix(&self.env().path)
                    .map(|p| p.display().to_string())
                    .unwrap_or_else(|_| pretty_path(path))
                    .into(),
                false,
            ),
            TabKind::Diff { path, .. } => (path.clone().into(), true),
            _ => (SharedString::default(), false),
        };
        let header = div()
            .flex()
            .flex_none()
            .items_center()
            .gap(crate::ui::sp(8.))
            .h(px(36.))
            .px(crate::ui::sp(14.))
            .border_b_1()
            .border_color(theme::stroke())
            .text_size(px(ui::TEXT_SM))
            .text_color(theme::text_subtle())
            .child(
                icon(if is_diff { "file-diff" } else { "file01" }, 14.)
                    .text_color(theme::text_subtle()),
            )
            .child(
                div()
                    .font_family(theme::FONT_MONO)
                    .truncate()
                    .child(subtitle.clone()),
            )
            .child(div().flex_1())
            .when(
                is_markdown(tab) && matches!(tab.doc, Some(Document::Lines(_))),
                |d| {
                    let id = tab.id;
                    let on = tab.markdown.is_some();
                    d.child(
                        div()
                            .id("doc-md-viewer")
                            .flex()
                            .flex_none()
                            .items_center()
                            .justify_center()
                            .size(px(ui::ICON_BUTTON))
                            .rounded(crate::ui::rad(ui::RADIUS))
                            .cursor_pointer()
                            .group("doc-md-viewer")
                            .when(on, |b| b.bg(theme::fill_selected()))
                            .hover(|s| s.bg(theme::fill_selected()))
                            .child(ui::hover_icon(
                                "view",
                                15.,
                                if on {
                                    theme::content()
                                } else {
                                    theme::text_subtle()
                                },
                                theme::content(),
                                "doc-md-viewer",
                            ))
                            .tooltip(ui::tip(if on {
                                "MD Viewer: show the raw file"
                            } else {
                                "MD Viewer: show it rendered"
                            }))
                            .on_click(
                                cx.listener(move |this, _, _, cx| this.toggle_md_viewer(id, cx)),
                            ),
                    )
                },
            )
            .when_some(self.doc_edit_target(tab), |d, (path, line)| {
                let open = path.clone();
                // A picture or a workbook has no text to edit, except an SVG or a CSV.
                let editable = !matches!(tab.doc, Some(Document::Image(_) | Document::Sheet(_)))
                    || path.extension().is_some_and(|e| {
                        ["svg", "csv", "tsv"]
                            .iter()
                            .any(|t| e.eq_ignore_ascii_case(t))
                    });
                d.when(editable, |d| {
                    d.child(
                        ui::button("doc-edit", "Edit", ui::ButtonKind::Ghost, true)
                            .h(px(24.))
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.edit_in_terminal(&path, line, window, cx)
                            })),
                    )
                })
                .child(
                    ui::button("doc-open", "Open", ui::ButtonKind::Ghost, true)
                        .h(px(24.))
                        .on_click(move |_, _, cx| cx.open_with_system(&open)),
                )
            });
        let body: AnyElement = match &tab.doc {
            None | Some(Document::Loading) => panel_empty("Loading…").into_any_element(),
            Some(Document::Failed(e)) => {
                let file = match &tab.kind {
                    TabKind::File { path } if path.is_file() => Some(path.clone()),
                    _ => None,
                };
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .gap(crate::ui::sp(12.))
                    .items_center()
                    .justify_center()
                    .text_size(px(ui::TEXT_MD))
                    .text_color(theme::text_subtle())
                    .child(e.clone())
                    .when_some(file, |d, file| {
                        d.child(
                            ui::button(
                                "doc-open-system",
                                "Open with default app",
                                ui::ButtonKind::Ghost,
                                true,
                            )
                            .h(px(28.))
                            .on_click(move |_, _, cx| cx.open_with_system(&file)),
                        )
                    })
                    .into_any_element()
            }
            Some(Document::Image(path)) => div()
                .flex()
                .flex_1()
                .min_h_0()
                .p(crate::ui::sp(16.))
                .child(crate::zoom::zoom_image(
                    SharedString::from(format!("zoom-{}", path.display())),
                    path.clone(),
                    Some(path.clone()),
                    None,
                    window,
                    cx,
                ))
                .into_any_element(),
            Some(Document::Lines(_)) if tab.markdown.is_some() => {
                let blocks = tab.markdown.clone().unwrap_or_default();
                // Relative image paths start from the file's folder.
                let base = match &tab.kind {
                    TabKind::File { path } => path.parent().map(std::path::Path::to_path_buf),
                    _ => None,
                };
                div()
                    .id(("md-viewer", tab.id as usize))
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .child(
                        div()
                            .mx_auto()
                            .w_full()
                            .max_w(px(820.))
                            .px(crate::ui::sp(32.))
                            .py(crate::ui::sp(24.))
                            .child(crate::agent::markdown::render(
                                ("md-doc", tab.id as usize),
                                &blocks,
                                crate::agent::markdown::MdStyle::reply(),
                                &crate::agent::TextMap::default(),
                                None,
                                &[],
                                base.as_deref(),
                                None,
                                None,
                            )),
                    )
                    .into_any_element()
            }
            Some(Document::Sheet(book)) => render_sheet(tab, book.clone(), cx),
            Some(Document::Lines(doc)) => {
                let doc = doc.clone();
                // The widest line sets the scrollable width, so long lines can be scrolled to.
                let longest = doc
                    .lines
                    .iter()
                    .enumerate()
                    .max_by_key(|(_, l)| {
                        l.chars()
                            .map(|c| if c == '\t' { 4 } else { 1 })
                            .sum::<usize>()
                    })
                    .map(|(i, _)| i);
                let gutter = (doc.lines.len().max(1) as f32).log10().floor() as usize + 1;
                let gutter_w = gutter as f32 * 7.8 + 24.;
                let numbers: Rc<Vec<Option<usize>>> = Rc::new(if is_diff {
                    notes::diff_line_numbers(&doc.lines)
                } else {
                    (1..=doc.lines.len()).map(Some).collect()
                });
                let review_path = subtitle.to_string();
                let target = tab.target_line.filter(|_| !is_diff);
                let commented = self.commented_lines(&review_path);
                let drafting = self
                    .review_draft
                    .as_ref()
                    .filter(|d| d.path == review_path)
                    .map(|d| d.rows.0..=d.rows.1);
                uniform_list(
                    "document",
                    doc.lines.len(),
                    cx.processor(move |_, range: Range<usize>, _, cx| {
                        range
                            .map(|ix| {
                                let number = numbers[ix];
                                let has_comment = number
                                    .is_some_and(|n| commented.iter().any(|&(a, b)| a <= n && n <= b));
                                let in_draft = drafting.as_ref().is_some_and(|r| r.contains(&ix));
                                let line = doc.lines[ix].clone();
                                let spans = doc.spans.get(ix).cloned().unwrap_or_default();
                                let (bg, fg): (Option<Hsla>, Hsla) = if is_diff {
                                    if line.starts_with("+++") || line.starts_with("---") {
                                        (None, theme::mix(0.45))
                                    } else if line.starts_with('+') {
                                        (Some(theme::diff_add_bg()), theme::diff_add().into())
                                    } else if line.starts_with('-') {
                                        (Some(theme::diff_del_bg()), theme::diff_del().into())
                                    } else if line.starts_with("@@") {
                                        (Some(theme::mix(0.04)), theme::accent())
                                    } else if line.starts_with("diff ") || line.starts_with("index ") || line.starts_with("new file") {
                                        (None, theme::mix(0.35))
                                    } else {
                                        (None, theme::mix(0.75))
                                    }
                                } else {
                                    (None, theme::mix(0.85))
                                };
                                let content: AnyElement = if spans.is_empty() {
                                    div().text_color(fg).whitespace_nowrap().child(line).into_any_element()
                                } else {
                                    // Syntax colors over the line's own color (the
                                    // diff marker at byte 0 keeps the +/- tint).
                                    div()
                                        .text_color(fg)
                                        .whitespace_nowrap()
                                        .child(StyledText::new(line).with_highlights(
                                            spans
                                                .into_iter()
                                                .map(|(r, color)| {
                                                    (
                                                        r,
                                                        HighlightStyle {
                                                            color: Some(color),
                                                            ..Default::default()
                                                        },
                                                    )
                                                })
                                                .collect::<Vec<_>>(),
                                        ))
                                        .into_any_element()
                                };
                                let mark = crate::agent::mark_color();
                                let group: SharedString = format!("doc-line-{ix}").into();
                                div()
                                    .id(("doc-line", ix))
                                    .group(group.clone())
                                    .relative()
                                    .flex()
                                    .h(px(19.))
                                    .when_some(bg, |d, bg| d.bg(bg))
                                    .when(target == Some(ix), |d| d.bg(theme::accent().opacity(0.14)))
                                    .when(has_comment || in_draft, |d| {
                                        d.bg(mark.opacity(if in_draft { 0.16 } else { 0.08 }))
                                    })
                                    .child(
                                        div()
                                            .id(("doc-gutter", ix))
                                            .flex_none()
                                            .relative()
                                            .w(px(gutter_w))
                                            .pr(crate::ui::sp(12.))
                                            .text_right()
                                            .text_color(theme::text_faint())
                                            .when(number.is_some(), |d| d.cursor_pointer())
                                            .when(!is_diff, |d| d.child(format!("{}", ix + 1)))
                                            .when(number.is_some(), |d| {
                                                d.child(
                                                    div()
                                                        .absolute()
                                                        .left(px(4.))
                                                        .top(px(2.))
                                                        .size(px(15.))
                                                        .flex()
                                                        .items_center()
                                                        .justify_center()
                                                        .rounded(crate::ui::rad(4.))
                                                        .when(!has_comment, |d| {
                                                            d.opacity(0.).group_hover(group.clone(), |s| s.opacity(1.))
                                                        })
                                                        .bg(if has_comment { mark.opacity(0.25) } else { theme::accent() })
                                                        .child(
                                                            icon(if has_comment { "comment01" } else { "add01" }, 10.)
                                                                .text_color(if has_comment { mark } else { theme::content() }),
                                                        ),
                                                )
                                            })
                                            .on_click({
                                                let (doc, numbers, path) = (doc.clone(), numbers.clone(), review_path.clone());
                                                cx.listener(move |this, ev: &gpui::ClickEvent, window, cx| {
                                                    if numbers[ix].is_none() {
                                                        return;
                                                    }
                                                    let shift = ev.modifiers().shift;
                                                    this.start_review(path.clone(), &doc.lines, &numbers, ix, shift, window, cx);
                                                })
                                            }),
                                    )
                                    .child(content)
                                    .into_any_element()
                            })
                            .collect::<Vec<_>>()
                    }),
                )
                .with_horizontal_sizing_behavior(gpui::ListHorizontalSizingBehavior::Unconstrained)
                .with_width_from_item(longest)
                .track_scroll(&tab.scroll)
                .flex_1()
                .min_h_0()
                .py(crate::ui::sp(8.))
                .font_family(theme::FONT_MONO)
                .text_size(px(ui::TEXT_MD))
                .line_height(px(19.))
                .into_any_element()
            }
        };
        let review_box = self.render_review_box(&subtitle, cx);
        div()
            .relative()
            .flex()
            .flex_col()
            .size_full()
            .child(header)
            .child(body)
            .when_some(review_box, |d, b| d.child(b))
            .into_any_element()
    }
}

/// A spreadsheet tab: the grid of the shown sheet with Excel's row numbers and
/// column letters, virtualized by row, and a bar of sheets below when there are several.
fn render_sheet(
    tab: &Tab,
    book: Arc<crate::sheet::Workbook>,
    cx: &mut Context<Shell>,
) -> AnyElement {
    use crate::sheet::CellKind;
    const ROW_H: f32 = 26.;
    let index = tab.sheet.min(book.sheets.len().saturating_sub(1));
    let Some(sheet) = book.sheets.get(index) else {
        return panel_empty("This workbook is empty.").into_any_element();
    };
    let digits = (sheet.rows.len().max(1) as f32).log10().floor() as usize + 1;
    let gutter_w = digits as f32 * 7.8 + 20.;
    // `sheet::column_width` sizes a column for its text plus the default
    // 8 px padding on each side; the padding follows Spacing, so widen by
    // what it grew.
    let cell = |w: f32| {
        div()
            .flex_none()
            .w(px(w - 16.) + crate::ui::sp(8.) * 2.)
            .h_full()
            .flex()
            .items_center()
            .px(crate::ui::sp(8.))
            .border_r_1()
            .border_b_1()
            .border_color(theme::stroke())
            .overflow_hidden()
            .whitespace_nowrap()
    };
    let gutter = move || {
        div()
            .flex_none()
            .w(px(gutter_w))
            .h_full()
            .flex()
            .items_center()
            .justify_end()
            .pr(crate::ui::sp(8.))
            .border_r_1()
            .border_b_1()
            .border_color(theme::stroke())
            .bg(theme::fill_raised())
            .text_color(theme::text_faint())
    };
    let shown = book.clone();
    let target = tab.target_line;
    let grid = uniform_list(
        ("sheet", tab.id as usize),
        sheet.rows.len() + 1,
        cx.processor(move |_, range: Range<usize>, _, _| {
            let sheet = &shown.sheets[index];
            range
                .map(|ix| {
                    let row = div().flex().h(px(ROW_H));
                    // Row 0 is the header of column letters.
                    if ix == 0 {
                        return row
                            .bg(theme::fill_raised())
                            .text_color(theme::text_faint())
                            .child(gutter())
                            .children(sheet.widths.iter().enumerate().map(|(c, &w)| {
                                cell(w).justify_center().child(crate::sheet::column_name(c))
                            }))
                            .into_any_element();
                    }
                    let cells = &sheet.rows[ix - 1];
                    row.when(target == Some(ix), |d| d.bg(theme::accent().opacity(0.14)))
                        .child(gutter().child(ix.to_string()))
                        .children(sheet.widths.iter().enumerate().map(|(c, &w)| {
                            let d = cell(w);
                            match cells.get(c) {
                                Some(v) if !v.text.is_empty() => {
                                    let d = match v.kind {
                                        CellKind::Number => {
                                            d.justify_end().text_color(theme::text())
                                        }
                                        CellKind::Formula => d
                                            .text_color(theme::text_faint())
                                            .font_family(theme::FONT_MONO),
                                        CellKind::Error => d.text_color(theme::diff_del()),
                                        CellKind::Text => d.text_color(theme::text()),
                                    };
                                    d.child(v.text.clone())
                                }
                                _ => d,
                            }
                        }))
                        .into_any_element()
                })
                .collect::<Vec<_>>()
        }),
    )
    .with_horizontal_sizing_behavior(gpui::ListHorizontalSizingBehavior::Unconstrained)
    .with_width_from_item(Some(0))
    .track_scroll(&tab.scroll)
    .flex_1()
    .min_h_0()
    .text_size(px(ui::TEXT_SM));
    let id = tab.id;
    let bar = (book.sheets.len() > 1 || sheet.truncated).then(|| {
        div()
            .id(("sheet-bar", id as usize))
            .flex()
            .flex_none()
            .items_center()
            .gap(crate::ui::sp(6.))
            .h(px(40.))
            .px(crate::ui::sp(12.))
            .border_t_1()
            .border_color(theme::stroke())
            .overflow_x_scroll()
            .children(
                book.sheets
                    .iter()
                    .enumerate()
                    .filter(|_| book.sheets.len() > 1)
                    .map(|(i, s)| {
                        ui::chip(("sheet-tab", i), s.name.clone(), i == index).on_click(
                            cx.listener(move |this, _, _, cx| this.select_sheet(id, i, cx)),
                        )
                    }),
            )
            .when(sheet.truncated, |d| {
                d.child(div().flex_1()).child(
                    div()
                        .flex_none()
                        .text_size(px(ui::TEXT_SM))
                        .text_color(theme::text_faint())
                        .child("Large sheet: only the first rows and columns are shown"),
                )
            })
    });
    div()
        .flex()
        .flex_col()
        .flex_1()
        .min_h_0()
        .child(grid)
        .when_some(bar, |d, bar| d.child(bar))
        .into_any_element()
}

/// The new-file start line of a `@@ -a,b +c,d @@` hunk header.
pub(super) fn hunk_start(line: &str) -> Option<usize> {
    let rest = line.strip_prefix("@@ ")?;
    let plus = rest.split_whitespace().find(|p| p.starts_with('+'))?;
    plus[1..].split(',').next()?.parse().ok()
}

/// `$VISUAL` / `$EDITOR` (which may carry arguments), else the first terminal
/// editor on PATH, opening `path` at `line` when the editor takes `+N`.
pub(super) fn editor_command(path: &std::path::Path, line: Option<usize>) -> Option<Vec<String>> {
    const KNOWN: &[&str] = &[
        "nvim", "vim", "vi", "nano", "micro", "emacs", "kak", "helix", "hx",
    ];
    let on_path = |name: &str| {
        std::env::var_os("PATH")
            .into_iter()
            .flat_map(|p| std::env::split_paths(&p).collect::<Vec<_>>())
            .any(|d| d.join(name).is_file())
    };
    let configured = ["VISUAL", "EDITOR"]
        .iter()
        .filter_map(|v| std::env::var(v).ok())
        .find(|v| !v.trim().is_empty());
    let mut cmd: Vec<String> = match configured {
        Some(v) => v.split_whitespace().map(str::to_string).collect(),
        None => vec![KNOWN.iter().find(|e| on_path(e))?.to_string()],
    };
    let program = cmd
        .first()?
        .rsplit('/')
        .next()
        .unwrap_or_default()
        .to_string();
    let file = path.display().to_string();
    match line {
        Some(n) if matches!(program.as_str(), "helix" | "hx") => cmd.push(format!("{file}:{n}")),
        Some(n) if KNOWN.contains(&program.as_str()) => {
            cmd.push(format!("+{n}"));
            cmd.push(file);
        }
        _ => cmd.push(file),
    }
    Some(cmd)
}
