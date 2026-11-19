//! The sidebar builder tab: pieces on the left, the sidebar in the middle where
//! they are dropped and arranged in segments, and the selected piece's or
//! segment's options on the right with the sidebar's own style below. Every
//! change applies to the real rail at once and saves (`crate::sidebar`).

use gpui::Point;

use super::rail_blocks::{avatar, git_identity, url_title, view_id};
use super::*;
use crate::input::{InputEvent, TextInput};
use crate::sidebar::{
    BadgeMode, Collapse, Density, Item, Mode, Path as ItemPath, Piece, Pin, Preset, Side, Sidebar,
    TitleAction, Variant, View,
};

/// What the right column is about.
#[derive(Clone, Copy, PartialEq)]
pub(super) enum BuilderSel {
    Item(ItemPath),
    Section(usize),
}

/// The text fields of the right column, made for the current selection.
pub(super) struct BuilderInputs {
    sel: BuilderSel,
    name: Entity<TextInput>,
    emoji: Entity<TextInput>,
    /// A pin's URL, command or path.
    target: Option<Entity<TextInput>>,
    _subs: Vec<Subscription>,
}

/// What the palette's one-line field is asking for.
#[derive(Clone, Copy, PartialEq)]
pub(super) enum PinAsk {
    Url,
    Command,
    Title,
}

pub(super) struct BuilderPrompt {
    ask: PinAsk,
    input: Entity<TextInput>,
    _sub: Subscription,
}

/// The builder's own state, kept on the shell.
#[derive(Default)]
pub(super) struct Builder {
    pub sel: Option<BuilderSel>,
    pub inputs: Option<BuilderInputs>,
    pub prompt: Option<BuilderPrompt>,
    pub undo: Vec<Sidebar>,
    pub redo: Vec<Sidebar>,
    pub flash: Option<SharedString>,
    /// The item or segment being dragged, to dim where it was.
    pub lifted: Option<BuilderSel>,
}

/// A piece lifted from the palette (`from: None`) or from its place.
#[derive(Clone)]
pub(super) struct DraggedPiece {
    from: Option<ItemPath>,
    item: Item,
    title: SharedString,
    offset: Point<Pixels>,
}

/// A segment lifted by its handle.
#[derive(Clone)]
pub(super) struct DraggedSection {
    from: usize,
    title: SharedString,
    offset: Point<Pixels>,
}

fn drag_pill(title: SharedString, offset: Point<Pixels>, icon_name: &str) -> gpui::Div {
    div()
        .relative()
        .left(-offset.x)
        .top(-offset.y)
        .w(px(190.))
        .h(px(30.))
        .flex()
        .items_center()
        .gap(crate::ui::sp(8.))
        .px(crate::ui::sp(10.))
        .rounded(crate::ui::rad(8.))
        .border_1()
        .border_color(theme::active().opacity(0.6))
        .bg(theme::dialog())
        .shadow_lg()
        .font_family(theme::FONT_SANS)
        .text_size(px(ui::TEXT_MD))
        .text_color(theme::content())
        .child(icon(icon_name, 14.).text_color(theme::text_subtle()))
        .child(div().min_w_0().truncate().child(title))
}

impl Render for DraggedPiece {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        drag_pill(self.title.clone(), self.offset, self.item.piece.icon())
    }
}

impl Render for DraggedSection {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        drag_pill(self.title.clone(), self.offset, "drag-drop-vertical")
    }
}

const UNDO_KEEP: usize = 100;

/// A titled block of the side columns.
fn panel_title(text: &str) -> gpui::Div {
    div()
        .pt(crate::ui::sp(14.))
        .pb(crate::ui::sp(6.))
        .px(crate::ui::sp(4.))
        .text_size(px(ui::TEXT_XS))
        .font_weight(FontWeight::SEMIBOLD)
        .text_color(theme::text_faint())
        .child(text.to_uppercase())
}

fn field_label(text: &str) -> gpui::Div {
    div()
        .pt(crate::ui::sp(10.))
        .pb(crate::ui::sp(4.))
        .text_size(px(ui::TEXT_SM))
        .text_color(theme::text_subtle())
        .child(text.to_string())
}

impl Shell {
    pub(super) fn open_sidebar_builder(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.menu = None;
        if !self.has_projects() {
            return;
        }
        self.show_view_tab("Sidebar", TabKind::SidebarBuilder, |k| {
            matches!(k, TabKind::SidebarBuilder)
        });
        let hidden: &[View] = if self.teams.read(cx).enabled() {
            &[]
        } else {
            &[View::Teams]
        };
        if self.sidebar.mark_known(hidden) {
            self.sidebar.save();
        }
        let _ = window;
        cx.notify();
    }

    /// Applies a change with undo, saves, and redraws the rail and the builder.
    pub(super) fn change_sidebar(&mut self, cx: &mut Context<Self>, f: impl FnOnce(&mut Sidebar)) {
        let before = self.sidebar.clone();
        f(&mut self.sidebar);
        if self.sidebar != before {
            self.builder.undo.push(before);
            if self.builder.undo.len() > UNDO_KEEP {
                self.builder.undo.remove(0);
            }
            self.builder.redo.clear();
            self.sidebar.save();
        }
        // A selection can point past what is left.
        let valid = match self.builder.sel {
            Some(BuilderSel::Item(p)) => self.sidebar.get(p).is_some(),
            Some(BuilderSel::Section(s)) => s < self.sidebar.sections.len(),
            None => true,
        };
        if !valid {
            self.builder.sel = None;
            self.builder.inputs = None;
        }
        cx.notify();
    }

    fn undo_sidebar(&mut self, redo: bool, cx: &mut Context<Self>) {
        let (from, to) = if redo {
            (&mut self.builder.redo, &mut self.builder.undo)
        } else {
            (&mut self.builder.undo, &mut self.builder.redo)
        };
        if let Some(prev) = from.pop() {
            to.push(std::mem::replace(&mut self.sidebar, prev));
            self.sidebar.save();
            self.builder.sel = None;
            self.builder.inputs = None;
            cx.notify();
        }
    }

    fn select_builder(&mut self, sel: BuilderSel, window: &mut Window, cx: &mut Context<Self>) {
        self.builder.sel = Some(sel);
        self.builder.prompt = None;
        let (name, emoji, target) = match sel {
            BuilderSel::Item(p) => {
                let Some(item) = self.sidebar.get(p) else {
                    return;
                };
                let target = match &item.piece {
                    Piece::Pin {
                        pin: Pin::Url { url, .. },
                    } => Some(url.clone()),
                    Piece::Pin {
                        pin: Pin::Command { command, .. },
                    } => Some(command.clone()),
                    Piece::Pin {
                        pin: Pin::File { path, .. },
                    } => Some(path.display().to_string()),
                    Piece::Title { text } => Some(text.clone()),
                    _ => None,
                };
                (
                    item.label.clone().unwrap_or_default(),
                    item.emoji.clone().unwrap_or_default(),
                    target,
                )
            }
            BuilderSel::Section(s) => {
                let Some(sec) = self.sidebar.sections.get(s) else {
                    return;
                };
                (sec.title.clone(), String::new(), None)
            }
        };
        let name_input = cx.new(|cx| {
            let mut t = TextInput::new("Its own name", cx);
            t.set_text(name, cx);
            t
        });
        let emoji_input = cx.new(|cx| {
            let mut t = TextInput::new("An emoji, like 📌", cx);
            t.set_text(emoji, cx);
            t
        });
        let target_input = target.map(|t| {
            cx.new(|cx| {
                let mut i = TextInput::new("", cx);
                i.set_text(t, cx);
                i
            })
        });
        let mut subs = vec![
            cx.subscribe(&name_input, move |this, input, ev: &InputEvent, cx| {
                if matches!(ev, InputEvent::Changed) {
                    let text = input.read(cx).text().to_string();
                    this.quiet_edit(cx, |s| match sel {
                        BuilderSel::Item(p) => {
                            if let Some(i) = s.get_mut(p) {
                                i.label = Some(text).filter(|t| !t.trim().is_empty());
                            }
                        }
                        BuilderSel::Section(si) => {
                            if let Some(sec) = s.sections.get_mut(si) {
                                sec.title = text;
                            }
                        }
                    });
                }
            }),
            cx.subscribe(&emoji_input, move |this, input, ev: &InputEvent, cx| {
                if let (InputEvent::Changed, BuilderSel::Item(p)) = (ev, sel) {
                    let text = input.read(cx).text().trim().to_string();
                    this.quiet_edit(cx, |s| {
                        if let Some(i) = s.get_mut(p) {
                            i.emoji = Some(text).filter(|t| !t.is_empty());
                        }
                    });
                }
            }),
        ];
        if let (Some(t), BuilderSel::Item(p)) = (&target_input, sel) {
            subs.push(cx.subscribe(t, move |this, input, ev: &InputEvent, cx| {
                if matches!(ev, InputEvent::Changed) {
                    let text = input.read(cx).text().to_string();
                    this.quiet_edit(cx, |s| {
                        if let Some(i) = s.get_mut(p) {
                            match &mut i.piece {
                                Piece::Pin {
                                    pin: Pin::Url { url, title },
                                } => {
                                    *url = text.clone();
                                    *title = url_title(&text);
                                }
                                Piece::Pin {
                                    pin: Pin::Command { command, title },
                                } => {
                                    *command = text.clone();
                                    *title = text.clone();
                                }
                                Piece::Pin {
                                    pin: Pin::File { path, .. },
                                } => *path = text.clone().into(),
                                Piece::Title { text: t } => *t = text.clone(),
                                _ => {}
                            }
                        }
                    });
                }
            }));
        }
        let _ = window;
        self.builder.inputs = Some(BuilderInputs {
            sel,
            name: name_input,
            emoji: emoji_input,
            target: target_input,
            _subs: subs,
        });
        cx.notify();
    }

    /// A change typed in a field: saved, but not one undo step per key.
    pub(super) fn quiet_edit(&mut self, cx: &mut Context<Self>, f: impl FnOnce(&mut Sidebar)) {
        f(&mut self.sidebar);
        self.sidebar.save();
        cx.notify();
    }

    fn ask_pin(&mut self, ask: PinAsk, window: &mut Window, cx: &mut Context<Self>) {
        let placeholder = match ask {
            PinAsk::Url => "https://… then Enter",
            PinAsk::Command => "npm run dev… then Enter",
            PinAsk::Title => "A heading, then Enter",
        };
        let input = cx.new(|cx| TextInput::new(placeholder, cx));
        let sub = cx.subscribe(&input, move |this, input, ev: &InputEvent, cx| match ev {
            InputEvent::Submit => {
                let text = input.read(cx).text().trim().to_string();
                this.builder.prompt = None;
                if !text.is_empty() {
                    let piece = match ask {
                        PinAsk::Url => {
                            let url = if text.contains("://") {
                                text
                            } else {
                                format!("https://{text}")
                            };
                            Piece::Pin {
                                pin: Pin::Url {
                                    title: url_title(&url),
                                    url,
                                },
                            }
                        }
                        PinAsk::Command => Piece::Pin {
                            pin: Pin::Command {
                                title: text.clone(),
                                command: text,
                            },
                        },
                        PinAsk::Title => Piece::Title { text },
                    };
                    this.add_piece(piece, cx);
                }
                cx.notify();
            }
            InputEvent::Cancel => {
                this.builder.prompt = None;
                cx.notify();
            }
            _ => {}
        });
        window.focus(&input.focus_handle(cx), cx);
        self.builder.prompt = Some(BuilderPrompt {
            ask,
            input,
            _sub: sub,
        });
        cx.notify();
    }

    /// A click on a palette piece: at the end of the selected segment, else
    /// of the first scrolling one.
    fn add_piece(&mut self, piece: Piece, cx: &mut Context<Self>) {
        let target = match self.builder.sel {
            Some(BuilderSel::Section(s)) => Some(s),
            Some(BuilderSel::Item((s, _))) => Some(s),
            None => self
                .sidebar
                .sections
                .iter()
                .position(|s| s.mode == Mode::Scroll)
                .or(Some(0)),
        };
        let Some(s) = target.filter(|s| *s < self.sidebar.sections.len()) else {
            self.change_sidebar(cx, |sb| {
                let at = sb.add_section(0);
                sb.insert(Item::new(piece), (at, 0));
            });
            return;
        };
        let end = self.sidebar.sections[s].items.len();
        self.change_sidebar(cx, |sb| {
            sb.insert(Item::new(piece), (s, end));
        });
    }

    /// The tab on screen as a pin, when it is a page or a file.
    fn tab_pin(&self) -> Option<Pin> {
        let env = self.env();
        // The builder itself is on screen; the tab before it is the one meant.
        let tab = self.strip.tabs.iter().rev().find(|t| {
            t.is_in(&env.path) && matches!(t.kind, TabKind::Browser { .. } | TabKind::File { .. })
        })?;
        match &tab.kind {
            TabKind::Browser { url } if !url.is_empty() => Some(Pin::Url {
                title: url_title(url),
                url: url.to_string(),
            }),
            TabKind::File { path } => Some(Pin::File {
                title: path.file_name().map_or_else(
                    || path.display().to_string(),
                    |n| n.to_string_lossy().into_owned(),
                ),
                path: path.clone(),
            }),
            _ => None,
        }
    }

    // ── The tab ─────────────────────────────────────────────────────────

    pub(super) fn render_sidebar_builder(
        &self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let _ = window;
        div()
            .id("sidebar-builder")
            .flex()
            .flex_col()
            .size_full()
            .bg(theme::background())
            .on_key_down(cx.listener(|this, ev: &gpui::KeyDownEvent, _, cx| {
                let m = &ev.keystroke.modifiers;
                if crate::platform::command_key(m) && ev.keystroke.key == "z" {
                    this.undo_sidebar(m.shift, cx);
                    cx.stop_propagation();
                }
            }))
            .child(self.render_builder_bar(cx))
            .child(
                div()
                    .flex()
                    .flex_1()
                    .min_h_0()
                    .child(self.render_builder_palette(cx))
                    .child(self.render_builder_canvas(cx))
                    .child(self.render_builder_inspector(cx)),
            )
            .into_any_element()
    }

    fn render_builder_bar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let can_undo = !self.builder.undo.is_empty();
        let can_redo = !self.builder.redo.is_empty();
        div()
            .flex()
            .flex_none()
            .items_center()
            .gap(crate::ui::sp(8.))
            .h(px(48.))
            .px(crate::ui::sp(16.))
            .border_b_1()
            .border_color(theme::stroke())
            .child(
                div()
                    .text_size(px(ui::TEXT_SM))
                    .text_color(theme::text_faint())
                    .child("Design system ›"),
            )
            .child(
                div()
                    .text_size(px(ui::TEXT_LG))
                    .font_weight(FontWeight::SEMIBOLD)
                    .child("Sidebar"),
            )
            .child(div().flex_1())
            .when_some(self.builder.flash.clone(), |d, f| {
                d.child(
                    div()
                        .text_size(px(ui::TEXT_SM))
                        .text_color(theme::text_subtle())
                        .child(f),
                )
            })
            .child(
                icon_button("sb-undo", "arrow-left01", 14.)
                    .tooltip(ui::tip("Undo (Ctrl+Z)"))
                    .when(!can_undo, |d| d.opacity(0.4))
                    .on_click(cx.listener(|this, _, _, cx| this.undo_sidebar(false, cx))),
            )
            .child(
                icon_button("sb-redo", "arrow-right01", 14.)
                    .tooltip(ui::tip("Redo (Ctrl+Shift+Z)"))
                    .when(!can_redo, |d| d.opacity(0.4))
                    .on_click(cx.listener(|this, _, _, cx| this.undo_sidebar(true, cx))),
            )
            .child(
                ui::button("sb-copy", "Export", ui::ButtonKind::Ghost, true)
                    .tooltip(ui::tip("Copies the layout as JSON, to share or keep"))
                    .on_click(cx.listener(|this, _, _, cx| {
                        cx.write_to_clipboard(gpui::ClipboardItem::new_string(
                            this.sidebar.to_json(),
                        ));
                        this.builder.flash = Some("Layout copied to the clipboard".into());
                        cx.notify();
                    })),
            )
            .child(
                ui::button("sb-paste", "Import", ui::ButtonKind::Ghost, true)
                    .tooltip(ui::tip("Uses a layout copied from Arakun"))
                    .on_click(cx.listener(|this, _, _, cx| {
                        let text = cx
                            .read_from_clipboard()
                            .and_then(|c| c.text())
                            .unwrap_or_default();
                        match Sidebar::parse(text.as_bytes()) {
                            Some(s) => {
                                this.change_sidebar(cx, |cur| *cur = s);
                                this.builder.flash = Some("Layout imported".into());
                            }
                            None => this.builder.flash = Some("The clipboard has no layout".into()),
                        }
                        cx.notify();
                    })),
            )
            .child(
                ui::button("sb-reset", "Reset", ui::ButtonKind::Outline, true)
                    .tooltip(ui::tip("Back to the sidebar Arakun ships with"))
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.change_sidebar(cx, |s| *s = Sidebar::default())
                    })),
            )
    }

    // ── Left: the pieces ────────────────────────────────────────────────

    fn render_builder_palette(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let piece_row = |id: SharedString,
                         piece: Option<Piece>,
                         icon_name: &'static str,
                         label: String,
                         note: Option<&'static str>| {
            let placed = piece
                .as_ref()
                .is_some_and(|p| p.unique() && self.sidebar.has(p));
            let el = div()
                .id(id.clone())
                .flex()
                .items_center()
                .gap(crate::ui::sp(8.))
                .h(px(32.))
                .px(crate::ui::sp(10.))
                .mb(px(4.))
                .rounded(crate::ui::rad(8.))
                .border_1()
                .border_color(theme::stroke())
                .bg(theme::fill_hover())
                .cursor_pointer()
                .text_size(px(ui::TEXT_MD))
                .hover(|s| s.border_color(theme::border()).bg(theme::fill_selected()))
                .when(placed, |d| d.opacity(0.5))
                .child(icon(icon_name, 14.).text_color(theme::text_subtle()))
                .child(div().flex_1().min_w_0().truncate().child(label.clone()))
                .when(placed, |d| {
                    d.child(
                        div()
                            .text_size(px(ui::TEXT_XS))
                            .text_color(theme::text_faint())
                            .child("in use"),
                    )
                })
                .when_some(note, |d, n| {
                    d.child(
                        div()
                            .px(crate::ui::sp(5.))
                            .rounded(crate::ui::rad(4.))
                            .bg(theme::fill_hover())
                            .text_size(px(ui::TEXT_XS))
                            .text_color(theme::text_muted())
                            .child(n),
                    )
                });
            match piece {
                Some(piece) => {
                    let dragged = DraggedPiece {
                        from: None,
                        item: Item::new(piece.clone()),
                        title: label.into(),
                        offset: Point::default(),
                    };
                    el.on_click(cx.listener(move |this, _, window, cx| {
                        // A piece already in the sidebar is selected instead of moved.
                        match this.sidebar.find(&piece).filter(|_| piece.unique()) {
                            Some(p) => this.select_builder(BuilderSel::Item(p), window, cx),
                            None => this.add_piece(piece.clone(), cx),
                        }
                    }))
                    .on_drag(dragged, |d, offset, _, cx| {
                        cx.new(|_| {
                            let mut d = d.clone();
                            d.offset = offset;
                            d
                        })
                    })
                }
                None => el,
            }
        };
        let mut col = div()
            .id("sb-palette")
            // Block, not a flex column: flex children shrink to fit and leave
            // nothing to scroll.
            .flex_none()
            .w(px(220.))
            .h_full()
            .overflow_y_scroll()
            .px(crate::ui::sp(12.))
            .pb(crate::ui::sp(16.))
            .border_r_1()
            .border_color(theme::stroke());
        col =
            col.child(panel_title("Templates")).child(
                div().flex().flex_wrap().gap(crate::ui::sp(4.)).children(
                    Preset::ALL.into_iter().map(|p| {
                        ui::chip(
                            SharedString::from(format!("sb-preset-{}", p.label())),
                            p.label(),
                            self.sidebar.sections == p.layout().sections,
                        )
                        .tooltip(ui::tip(p.blurb()))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.change_sidebar(cx, |s| *s = p.layout())
                        }))
                    }),
                ),
            );
        col = col
            .child(panel_title("Navigation"))
            .child(piece_row(
                "sb-p-search".into(),
                Some(Piece::Search),
                "search01",
                "Search".into(),
                None,
            ))
            .child(piece_row(
                "sb-p-projects".into(),
                Some(Piece::projects()),
                "folder01",
                "Projects".into(),
                None,
            ))
            .child(piece_row(
                "sb-p-active".into(),
                Some(Piece::active()),
                "clock01",
                "Active".into(),
                None,
            ));
        // Extensions that are off are added in the Marketplace, not here.
        for &v in View::ALL {
            if !self.rail_shows(&Piece::view(v), cx) {
                continue;
            }
            let note = self.sidebar.is_new(v).then_some("New");
            col = col.child(piece_row(
                format!("sb-p-{}", view_id(v)).into(),
                Some(Piece::view(v)),
                v.icon(),
                v.label().into(),
                note,
            ));
        }
        col = col.child(
            piece_row(
                "sb-p-more".into(),
                None,
                "store01",
                "More in Marketplace…".into(),
                None,
            )
            .on_click(cx.listener(|this, _, window, cx| {
                this.open_extensions_tab(window, cx);
                this.extensions_view
                    .update(cx, |v, cx| v.show_extensions(cx));
            })),
        );
        col = col
            .child(panel_title("Your own"))
            .child(
                piece_row("sb-p-link".into(), None, "link01", "Link…".into(), None).on_click(
                    cx.listener(|this, _, window, cx| this.ask_pin(PinAsk::Url, window, cx)),
                ),
            )
            .child(
                piece_row(
                    "sb-p-cmd".into(),
                    None,
                    "computer-terminal01",
                    "Command…".into(),
                    None,
                )
                .on_click(
                    cx.listener(|this, _, window, cx| this.ask_pin(PinAsk::Command, window, cx)),
                ),
            );
        if let Some(pin) = self.tab_pin() {
            let label = format!("Pin {}", pin.title());
            col = col.child(piece_row(
                "sb-p-tab".into(),
                Some(Piece::Pin { pin }),
                "pin",
                label,
                None,
            ));
        }
        if let Some(prompt) = &self.builder.prompt
            && prompt.ask != PinAsk::Title
        {
            col = col.child(
                div()
                    .mb(px(4.))
                    .child(ui::field().child(prompt.input.clone())),
            );
        }
        col = col
            .child(panel_title("Account"))
            .child(piece_row(
                "sb-p-user".into(),
                Some(Piece::User),
                "user",
                "User".into(),
                None,
            ))
            .child(piece_row(
                "sb-p-usage".into(),
                Some(Piece::Usage),
                "dashboard-square01",
                "Usage card".into(),
                None,
            ))
            .child(piece_row(
                "sb-p-settings".into(),
                Some(Piece::Settings),
                "settings01",
                "Settings".into(),
                None,
            ))
            .child(piece_row(
                "sb-p-mascot".into(),
                Some(Piece::Mascot),
                "smile-plus",
                "Mascot".into(),
                None,
            ))
            .child(panel_title("Structure"))
            .child(
                piece_row(
                    "sb-p-title".into(),
                    None,
                    "pencil-edit01",
                    "Title…".into(),
                    None,
                )
                .on_click(
                    cx.listener(|this, _, window, cx| this.ask_pin(PinAsk::Title, window, cx)),
                ),
            );
        if let Some(prompt) = &self.builder.prompt
            && prompt.ask == PinAsk::Title
        {
            col = col.child(
                div()
                    .mb(px(4.))
                    .child(ui::field().child(prompt.input.clone())),
            );
        }
        col.child(piece_row(
            "sb-p-sep".into(),
            Some(Piece::Separator),
            "minus-sign",
            "Separator".into(),
            None,
        ))
        .child(piece_row(
            "sb-p-spacer".into(),
            Some(Piece::Spacer),
            "unfold-more",
            "Space".into(),
            None,
        ))
    }

    // ── Middle: the sidebar ─────────────────────────────────────────────

    fn render_builder_canvas(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let style = self.sidebar.style;
        let mut rail = div()
            .flex()
            .flex_col()
            // Its own height, or the canvas has nothing to scroll.
            .flex_none()
            .gap(crate::ui::sp(8.))
            .w(px(self.rail_w.max(220.) + 24.))
            .min_h(px(560.))
            .p(crate::ui::sp(10.))
            .when(style.variant != Variant::Flush, |d| {
                d.rounded(crate::ui::rad(14.))
                    .border_1()
                    .border_color(theme::stroke())
                    .bg(theme::sidebar())
            })
            .when(style.variant == Variant::Floating, |d| {
                d.shadow_lg().rounded(crate::ui::rad(18.))
            });
        for (si, section) in self.sidebar.sections.iter().enumerate() {
            rail = rail.child(self.render_builder_section(si, section, cx));
        }
        rail = rail.child(
            div()
                .id("sb-add-section")
                .flex()
                .items_center()
                .justify_center()
                .gap(crate::ui::sp(6.))
                .h(px(34.))
                .rounded(crate::ui::rad(10.))
                .border_1()
                .border_dashed()
                .border_color(theme::border())
                .cursor_pointer()
                .text_size(px(ui::TEXT_SM))
                .text_color(theme::text_subtle())
                .hover(|s| s.border_color(theme::active()).text_color(theme::content()))
                .child(icon("add01", 14.).text_color(theme::text_subtle()))
                .child("Add segment")
                .drag_over::<DraggedSection>(|s, _, _, _| s.border_color(theme::active()))
                .on_drop(cx.listener(|this, d: &DraggedSection, _, cx| {
                    let n = this.sidebar.sections.len();
                    this.change_sidebar(cx, |s| {
                        s.move_section(d.from, n);
                    })
                }))
                .on_click(cx.listener(|this, _, window, cx| {
                    let n = this.sidebar.sections.len();
                    let mut at = n;
                    this.change_sidebar(cx, |s| at = s.add_section(n));
                    this.select_builder(BuilderSel::Section(at), window, cx);
                })),
        );
        div()
            .id("sb-canvas")
            .flex()
            .flex_col()
            .flex_1()
            .min_w_0()
            .h_full()
            .overflow_y_scroll()
            .items_center()
            .py(crate::ui::sp(24.))
            .px(crate::ui::sp(16.))
            .bg(theme::window_bg())
            .on_click(cx.listener(|this, _, _, cx| {
                this.builder.sel = None;
                this.builder.inputs = None;
                cx.notify();
            }))
            .child(
                div()
                    .flex_none()
                    .pb(crate::ui::sp(10.))
                    .text_size(px(ui::TEXT_SM))
                    .text_color(theme::text_faint())
                    .child("Drag pieces in · drag ⋮⋮ to move segments · click anything to edit it"),
            )
            .child(rail)
    }

    fn render_builder_section(
        &self,
        si: usize,
        section: &crate::sidebar::Section,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let selected = self.builder.sel == Some(BuilderSel::Section(si));
        let lifted = cx.has_active_drag() && self.builder.lifted == Some(BuilderSel::Section(si));
        let scroll = section.mode == Mode::Scroll;
        let dragged = DraggedSection {
            from: si,
            title: section.title.clone().into(),
            offset: Point::default(),
        };
        let header = div()
            .id(("sb-sec-head", si))
            .flex()
            .items_center()
            .gap(crate::ui::sp(6.))
            .h(px(24.))
            .text_size(px(ui::TEXT_XS))
            .text_color(theme::text_faint())
            .child(
                div()
                    .id(("sb-sec-grip", si))
                    .cursor_grab()
                    .child(icon("drag-drop-vertical", 12.).text_color(theme::text_faint()))
                    .on_drag(dragged, {
                        let shell = cx.entity().downgrade();
                        move |d, offset, _, cx| {
                            let _ = shell.update(cx, |this, _| {
                                this.builder.lifted = Some(BuilderSel::Section(d.from))
                            });
                            cx.new(|_| {
                                let mut d = d.clone();
                                d.offset = offset;
                                d
                            })
                        }
                    }),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(if section.title.is_empty() {
                        "Segment".to_string()
                    } else {
                        section.title.to_uppercase()
                    }),
            )
            .when(!section.show_title, |d| {
                d.child(icon("view-off", 11.).text_color(theme::text_faint()))
            })
            .child(
                div()
                    .id(("sb-sec-mode", si))
                    .px(crate::ui::sp(6.))
                    .py(crate::ui::sp(1.))
                    .rounded(crate::ui::rad(6.))
                    .border_1()
                    .border_color(if scroll {
                        theme::active().opacity(0.5)
                    } else {
                        theme::border()
                    })
                    .text_color(if scroll {
                        theme::active()
                    } else {
                        theme::text_subtle()
                    })
                    .cursor_pointer()
                    .tooltip(ui::tip(
                        "Fixed stays on screen; Scroll scrolls with its neighbours",
                    ))
                    .child(if scroll { "Scroll" } else { "Fixed" })
                    .on_click(cx.listener(move |this, _, _, cx| {
                        cx.stop_propagation();
                        this.change_sidebar(cx, |s| {
                            if let Some(sec) = s.sections.get_mut(si) {
                                sec.mode = if sec.mode == Mode::Scroll {
                                    Mode::Fixed
                                } else {
                                    Mode::Scroll
                                };
                            }
                        })
                    })),
            )
            .child(
                div()
                    .id(("sb-sec-del", si))
                    .flex()
                    .items_center()
                    .justify_center()
                    .size(px(18.))
                    .rounded(crate::ui::rad(5.))
                    .cursor_pointer()
                    .hover(|s| s.bg(theme::fill_selected()))
                    .tooltip(ui::tip("Remove segment"))
                    .child(icon("cancel01", 11.).text_color(theme::text_subtle()))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        cx.stop_propagation();
                        this.change_sidebar(cx, |s| {
                            s.remove_section(si);
                        })
                    })),
            );
        let mut body = div().flex().flex_col().gap(crate::ui::sp(1.));
        for (ii, item) in section.items.iter().enumerate() {
            // An extension that is off keeps its place for when it is back on.
            if self.rail_shows(&item.piece, cx) {
                body = body.child(self.render_builder_item((si, ii), item, cx));
            }
        }
        let end = section.items.len();
        body = body.child(
            div()
                .id(("sb-sec-end", si))
                .h(px(if section.items.is_empty() { 40. } else { 14. }))
                .flex()
                .items_center()
                .justify_center()
                .rounded(crate::ui::rad(8.))
                .when(section.items.is_empty(), |d| {
                    d.border_1()
                        .border_dashed()
                        .border_color(theme::stroke())
                        .text_size(px(ui::TEXT_XS))
                        .text_color(theme::text_faint())
                        .child("Drop pieces here")
                })
                .drag_over::<DraggedPiece>(|s, _, _, _| s.bg(theme::active().opacity(0.12)))
                .on_drop(cx.listener(move |this, d: &DraggedPiece, _, cx| {
                    this.drop_piece(d, (si, end), cx)
                })),
        );
        div()
            .id(("sb-sec", si))
            .flex()
            .flex_col()
            .gap(crate::ui::sp(4.))
            .p(crate::ui::sp(8.))
            .rounded(crate::ui::rad(10.))
            .border_1()
            .when(selected, |d| d.border_color(theme::active()))
            .when(!selected, |d| {
                d.border_dashed().border_color(theme::border())
            })
            .when(lifted, |d| d.opacity(0.35))
            .when(section.line_above, |d| d.border_t_2())
            .cursor_pointer()
            .on_click(cx.listener(move |this, _, window, cx| {
                cx.stop_propagation();
                this.select_builder(BuilderSel::Section(si), window, cx)
            }))
            // A segment dropped on another lands above it.
            .drag_over::<DraggedSection>(|s, _, _, _| s.border_t_2().border_color(theme::active()))
            .on_drop(cx.listener(move |this, d: &DraggedSection, _, cx| {
                this.builder.lifted = None;
                this.change_sidebar(cx, |s| {
                    s.move_section(d.from, si);
                })
            }))
            .child(header)
            .when(section.collapsible, |d| {
                d.child(
                    div()
                        .text_size(px(ui::TEXT_XS))
                        .text_color(theme::text_faint())
                        .child(if section.collapsed {
                            "Collapsible · starts folded"
                        } else {
                            "Collapsible"
                        }),
                )
            })
            .child(body)
    }

    fn drop_piece(&mut self, d: &DraggedPiece, to: ItemPath, cx: &mut Context<Self>) {
        self.builder.lifted = None;
        let (from, item) = (d.from, d.item.clone());
        self.change_sidebar(cx, |s| {
            match from {
                Some(from) => s.move_item(from, to),
                None => s.insert(item, to),
            };
        });
    }

    /// A piece in the canvas, drawn as the rail draws it.
    fn render_builder_item(
        &self,
        path: ItemPath,
        item: &Item,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let style = self.sidebar.style;
        let selected = self.builder.sel == Some(BuilderSel::Item(path));
        let lifted = cx.has_active_drag() && self.builder.lifted == Some(BuilderSel::Item(path));
        let lead: AnyElement = match &item.emoji {
            Some(e) => div()
                .w(px(16.))
                .text_size(px(ui::TEXT_MD))
                .child(e.clone())
                .into_any_element(),
            None => icon(item.piece.icon(), 16.)
                .text_color(theme::text_subtle())
                .into_any_element(),
        };
        let badge = match &item.piece {
            Piece::View { view } if style.badges && item.badge != BadgeMode::Off => {
                self.view_badge(*view, cx).map(|b| {
                    if item.badge == BadgeMode::Dot {
                        "•".to_string()
                    } else {
                        format!("{}", badge_count(b))
                    }
                })
            }
            _ => None,
        };
        let content: AnyElement = match &item.piece {
            Piece::Separator => div()
                .w_full()
                .my(px(6.))
                .h(px(1.))
                .bg(theme::stroke())
                .into_any_element(),
            Piece::Spacer => div()
                .w_full()
                .h(px(16.))
                .border_1()
                .border_dashed()
                .border_color(theme::stroke())
                .rounded(crate::ui::rad(6.))
                .into_any_element(),
            Piece::Title { text } => div()
                .px(crate::ui::sp(8.))
                .pt(crate::ui::sp(4.))
                .text_size(px(ui::TEXT_XS))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(theme::text_faint())
                .child(text.to_uppercase())
                .into_any_element(),
            Piece::User => {
                let (name, email) = git_identity();
                let shown = item
                    .label
                    .clone()
                    .filter(|l| !l.is_empty())
                    .unwrap_or_else(|| {
                        if name.is_empty() {
                            "You".into()
                        } else {
                            name.clone()
                        }
                    });
                div()
                    .flex()
                    .items_center()
                    .gap(crate::ui::sp(8.))
                    .py(crate::ui::sp(4.))
                    .px(crate::ui::sp(6.))
                    .child(avatar(&shown, 26.))
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .min_w_0()
                            .child(div().truncate().text_size(px(ui::TEXT_MD)).child(shown))
                            .child(
                                div()
                                    .truncate()
                                    .text_size(px(ui::TEXT_XS))
                                    .text_color(theme::text_faint())
                                    .child(email.clone()),
                            ),
                    )
                    .into_any_element()
            }
            Piece::Usage => div()
                .flex()
                .flex_col()
                .gap(crate::ui::sp(6.))
                .p(crate::ui::sp(8.))
                .rounded(crate::ui::rad(8.))
                .border_1()
                .border_color(theme::stroke())
                .bg(theme::fill_hover())
                .child(div().text_size(px(ui::TEXT_SM)).child("Usage"))
                .child(
                    div()
                        .h(px(5.))
                        .rounded(crate::ui::rad(3.))
                        .bg(theme::fill_selected())
                        .child(
                            div()
                                .w(px(90.))
                                .h_full()
                                .rounded(crate::ui::rad(3.))
                                .bg(theme::active()),
                        ),
                )
                .into_any_element(),
            Piece::Active => {
                let row = |text: &'static str, color: Hsla| {
                    div()
                        .flex()
                        .items_center()
                        .gap(crate::ui::sp(8.))
                        .h(px(24.))
                        .px(crate::ui::sp(8.))
                        .text_size(px(ui::TEXT_SM))
                        .child(div().size(px(6.)).rounded_full().bg(color))
                        .child(text)
                };
                let head = |text: &'static str| {
                    div()
                        .px(crate::ui::sp(8.))
                        .pt(crate::ui::sp(4.))
                        .text_size(px(ui::TEXT_XS))
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(theme::text_faint())
                        .child(text)
                };
                div()
                    .flex()
                    .flex_col()
                    .child(head("NEEDS YOU"))
                    .child(row("Waiting on you", theme::needs_you()))
                    .child(head("RUNNING"))
                    .child(row("Working", theme::working()))
                    .into_any_element()
            }
            Piece::Projects {
                envs, only_open, ..
            } => {
                let mut list = div().flex().flex_col().gap(crate::ui::sp(1.));
                for (pi, p) in self
                    .projects
                    .iter()
                    .enumerate()
                    .filter(|(pi, _)| !only_open || *pi == self.active_project)
                    .take(6)
                {
                    list = list.child(
                        div()
                            .flex()
                            .items_center()
                            .gap(crate::ui::sp(8.))
                            .h(px(28.))
                            .px(crate::ui::sp(8.))
                            .rounded(crate::ui::rad(8.))
                            .when(pi == self.active_project, |d| d.bg(theme::selection()))
                            .child(ui::pixel_art(
                                ui::project_art(p.art),
                                1.5,
                                gpui::rgb(p.logo).into(),
                            ))
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .truncate()
                                    .text_size(px(ui::TEXT_MD))
                                    .child(p.name.clone()),
                            )
                            .when(*only_open, |d| {
                                d.child(icon("unfold-more", 14.).text_color(theme::text_subtle()))
                            }),
                    );
                    if pi == self.active_project && *envs {
                        for e in p.environments.iter().take(3) {
                            list = list.child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap(crate::ui::sp(6.))
                                    .h(px(24.))
                                    .pl(crate::ui::sp(28.))
                                    .text_size(px(ui::TEXT_SM))
                                    .text_color(theme::text_subtle())
                                    .child(icon("git-branch", 12.).text_color(theme::text_faint()))
                                    .child(div().truncate().child(e.name.clone())),
                            );
                        }
                    }
                }
                list.into_any_element()
            }
            _ => div()
                .flex()
                .items_center()
                .gap(crate::ui::sp(8.))
                .h(px(style.density.row_h()))
                .px(crate::ui::sp(8.))
                .text_size(px(ui::TEXT_LG))
                .font_weight(FontWeight::MEDIUM)
                .text_color(theme::text_subtle())
                .when(matches!(item.piece, Piece::Search), |d| {
                    d.border_1()
                        .border_color(theme::border())
                        .rounded(crate::ui::rad(8.))
                })
                .child(lead)
                .child(div().flex_1().min_w_0().truncate().child(item.name()))
                .when_some(badge, |d, b| {
                    d.child(
                        div()
                            .px(crate::ui::sp(6.))
                            .rounded_full()
                            .bg(theme::fill_selected())
                            .text_size(px(ui::TEXT_XS))
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(b),
                    )
                })
                .into_any_element(),
        };
        let dragged = DraggedPiece {
            from: Some(path),
            item: item.clone(),
            title: item.name().into(),
            offset: Point::default(),
        };
        div()
            .id(SharedString::from(format!("sb-item-{}-{}", path.0, path.1)))
            .group("sb-item")
            .relative()
            .rounded(crate::ui::rad(8.))
            .cursor_grab()
            .border_1()
            .border_color(if selected {
                theme::active()
            } else {
                gpui::transparent_black()
            })
            .hover(|s| s.bg(theme::hover()))
            .when(lifted, |d| d.opacity(0.35))
            .child(content)
            .child(
                div()
                    .id(SharedString::from(format!(
                        "sb-item-x-{}-{}",
                        path.0, path.1
                    )))
                    .absolute()
                    .top(px(4.))
                    .right(px(4.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .size(px(20.))
                    .rounded(crate::ui::rad(6.))
                    .invisible()
                    .group_hover("sb-item", |s| s.visible())
                    .bg(theme::dialog())
                    .hover(|s| s.bg(theme::fill_selected()))
                    .tooltip(ui::tip("Remove from the sidebar"))
                    .child(icon("cancel01", 11.).text_color(theme::text_subtle()))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        cx.stop_propagation();
                        this.change_sidebar(cx, |s| {
                            s.remove(path);
                        })
                    })),
            )
            .on_click(cx.listener(move |this, _, window, cx| {
                cx.stop_propagation();
                this.select_builder(BuilderSel::Item(path), window, cx)
            }))
            .on_drag(dragged, {
                let shell = cx.entity().downgrade();
                move |d, offset, _, cx| {
                    let _ = shell.update(cx, |this, _| {
                        this.builder.lifted = d.from.map(BuilderSel::Item)
                    });
                    cx.new(|_| {
                        let mut d = d.clone();
                        d.offset = offset;
                        d
                    })
                }
            })
            // The line above the row says where a dropped piece lands.
            .drag_over::<DraggedPiece>(|s, _, _, _| s.border_t_2().border_color(theme::active()))
            .on_drop(cx.listener(move |this, d: &DraggedPiece, _, cx| this.drop_piece(d, path, cx)))
    }

    // ── Right: options ──────────────────────────────────────────────────

    fn render_builder_inspector(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let mut col = div()
            .id("sb-inspector")
            // Block, not a flex column: flex children shrink to fit and leave
            // nothing to scroll.
            .flex_none()
            .w(px(280.))
            .h_full()
            .overflow_y_scroll()
            .px(crate::ui::sp(14.))
            .pb(crate::ui::sp(20.))
            .border_l_1()
            .border_color(theme::stroke());
        col = match (self.builder.sel, &self.builder.inputs) {
            (Some(BuilderSel::Item(p)), Some(inputs)) if inputs.sel == BuilderSel::Item(p) => {
                col.child(self.render_item_options(p, inputs, cx))
            }
            (Some(BuilderSel::Section(s)), Some(inputs))
                if inputs.sel == BuilderSel::Section(s) =>
            {
                col.child(self.render_section_options(s, inputs, cx))
            }
            _ => col.child(panel_title("Selected")).child(
                div()
                    .text_size(px(ui::TEXT_SM))
                    .text_color(theme::text_faint())
                    .child("Click a piece or a segment to see its options."),
            ),
        };
        col.child(self.render_style_options(cx))
    }

    fn render_item_options(
        &self,
        p: ItemPath,
        inputs: &BuilderInputs,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let Some(item) = self.sidebar.get(p).cloned() else {
            return div();
        };
        let mut out = div()
            .flex()
            .flex_col()
            .child(panel_title(&item.piece.label()))
            .child(field_label("Name"))
            .child(ui::field().child(inputs.name.clone()))
            .child(field_label("Emoji instead of the icon"))
            .child(ui::field().child(inputs.emoji.clone()));
        if let Some(target) = &inputs.target {
            let label = match item.piece {
                Piece::Pin {
                    pin: Pin::Url { .. },
                } => "Opens",
                Piece::Pin {
                    pin: Pin::Command { .. },
                } => "Runs",
                Piece::Pin {
                    pin: Pin::File { .. },
                } => "File",
                _ => "Text",
            };
            out = out
                .child(field_label(label))
                .child(ui::field().child(target.clone()));
        }
        if let Piece::View { .. } = item.piece {
            let mut seg = ui::segmented();
            for (i, (mode, label)) in [
                (BadgeMode::Count, "Count"),
                (BadgeMode::Dot, "Dot"),
                (BadgeMode::Urgent, "Urgent"),
                (BadgeMode::Off, "Off"),
            ]
            .into_iter()
            .enumerate()
            {
                seg = seg.child(
                    ui::segment(("sb-badge", i), None, label, item.badge == mode).on_click(
                        cx.listener(move |this, _, _, cx| {
                            this.change_sidebar(cx, |s| {
                                if let Some(i) = s.get_mut(p) {
                                    i.badge = mode;
                                }
                            })
                        }),
                    ),
                );
            }
            out = out.child(field_label("Counter")).child(seg).child(
                ui::switch_row("sb-busy", "Only when it has something", item.only_when_busy)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.change_sidebar(cx, |s| {
                            if let Some(i) = s.get_mut(p) {
                                i.only_when_busy = !i.only_when_busy;
                            }
                        })
                    })),
            );
        }
        if let Piece::Projects {
            envs,
            changes,
            only_open,
        } = item.piece
        {
            out = out
                .child(
                    ui::switch_row("sb-envs", "Show environments", envs).on_click(cx.listener(
                        move |this, _, _, cx| {
                            this.change_sidebar(cx, |s| {
                                if let Some(Item {
                                    piece: Piece::Projects { envs, .. },
                                    ..
                                }) = s.get_mut(p)
                                {
                                    *envs = !*envs;
                                }
                            })
                        },
                    )),
                )
                .child(
                    ui::switch_row("sb-changes", "Show changes (+/−)", changes).on_click(
                        cx.listener(move |this, _, _, cx| {
                            this.change_sidebar(cx, |s| {
                                if let Some(Item {
                                    piece: Piece::Projects { changes, .. },
                                    ..
                                }) = s.get_mut(p)
                                {
                                    *changes = !*changes;
                                }
                            })
                        }),
                    ),
                )
                .child(
                    ui::switch_row(
                        "sb-only-open",
                        "Only the open project (others in a dropdown)",
                        only_open,
                    )
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.change_sidebar(cx, |s| {
                            if let Some(Item {
                                piece: Piece::Projects { only_open, .. },
                                ..
                            }) = s.get_mut(p)
                            {
                                *only_open = !*only_open;
                            }
                        })
                    })),
                );
        }
        out.child(
            div().pt(crate::ui::sp(12.)).child(
                ui::button_with(
                    "sb-item-remove",
                    Some("view-off"),
                    "Remove from the sidebar",
                    ui::ButtonKind::Danger,
                    true,
                )
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.change_sidebar(cx, |s| {
                        s.remove(p);
                    })
                })),
            ),
        )
    }

    fn render_section_options(
        &self,
        si: usize,
        inputs: &BuilderInputs,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let Some(sec) = self.sidebar.sections.get(si).cloned() else {
            return div();
        };
        let toggle = |id: &'static str,
                      label: &'static str,
                      on: bool,
                      f: fn(&mut crate::sidebar::Section)| {
            ui::switch_row(id, label, on).on_click(cx.listener(move |this, _, _, cx| {
                this.change_sidebar(cx, |s| {
                    if let Some(sec) = s.sections.get_mut(si) {
                        f(sec)
                    }
                })
            }))
        };
        let mut mode = ui::segmented();
        for (i, (m, label)) in [(Mode::Fixed, "Fixed"), (Mode::Scroll, "Scroll")]
            .into_iter()
            .enumerate()
        {
            mode = mode.child(
                ui::segment(("sb-sec-mode-opt", i), None, label, sec.mode == m).on_click(
                    cx.listener(move |this, _, _, cx| {
                        this.change_sidebar(cx, |s| {
                            if let Some(sec) = s.sections.get_mut(si) {
                                sec.mode = m;
                            }
                        })
                    }),
                ),
            );
        }
        let mut action = ui::segmented();
        let actions: Vec<Option<TitleAction>> = std::iter::once(None)
            .chain(TitleAction::ALL.map(Some))
            .collect();
        for (i, a) in actions.into_iter().enumerate() {
            let label = match a {
                None => "None",
                Some(TitleAction::AddProject) => "Project",
                Some(TitleAction::NewSession) => "Session",
                Some(TitleAction::NewTerminal) => "Terminal",
            };
            action = action.child(
                ui::segment(("sb-sec-action", i), None, label, sec.action == a).on_click(
                    cx.listener(move |this, _, _, cx| {
                        this.change_sidebar(cx, |s| {
                            if let Some(sec) = s.sections.get_mut(si) {
                                sec.action = a;
                            }
                        })
                    }),
                ),
            );
        }
        div()
            .flex()
            .flex_col()
            .child(panel_title("Segment"))
            .child(field_label("Name"))
            .child(ui::field().child(inputs.name.clone()))
            .child(field_label("Behaviour"))
            .child(mode)
            .child(toggle(
                "sb-sec-title",
                "Show its title",
                sec.show_title,
                |s| s.show_title = !s.show_title,
            ))
            .child(toggle("sb-sec-fold", "Collapsible", sec.collapsible, |s| {
                s.collapsible = !s.collapsible;
                s.show_title |= s.collapsible;
            }))
            .when(sec.collapsible, |d| {
                d.child(toggle(
                    "sb-sec-folded",
                    "Starts folded",
                    sec.collapsed,
                    |s| s.collapsed = !s.collapsed,
                ))
                .child(toggle(
                    "sb-sec-peek",
                    "Show what is inside when folded",
                    sec.peek,
                    |s| s.peek = !s.peek,
                ))
            })
            .child(toggle("sb-sec-row", "Icons in a row", sec.row, |s| {
                s.row = !s.row
            }))
            .child(toggle("sb-sec-line", "Line above", sec.line_above, |s| {
                s.line_above = !s.line_above
            }))
            .child(toggle(
                "sb-sec-icons",
                "Hide when narrowed to icons",
                sec.hide_in_icons,
                |s| s.hide_in_icons = !s.hide_in_icons,
            ))
            .child(field_label("Button on its title"))
            .child(action)
            .when(self.sidebar.sections.len() > 1, |d| {
                d.child(
                    div().pt(crate::ui::sp(12.)).child(
                        ui::button_with(
                            "sb-sec-ungroup",
                            Some("ungroup-items"),
                            "Ungroup",
                            ui::ButtonKind::Ghost,
                            true,
                        )
                        .tooltip(ui::tip("Remove the segment and keep its pieces"))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.change_sidebar(cx, |s| {
                                s.ungroup_section(si);
                            })
                        })),
                    ),
                )
            })
            .child(
                div().pt(crate::ui::sp(12.)).child(
                    ui::button_with(
                        "sb-sec-remove",
                        Some("cancel01"),
                        "Remove segment",
                        ui::ButtonKind::Danger,
                        true,
                    )
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.change_sidebar(cx, |s| {
                            s.remove_section(si);
                        })
                    })),
                ),
            )
    }

    fn render_style_options(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let style = self.sidebar.style;
        fn choices<T: Copy + PartialEq + 'static>(
            this: &Shell,
            id: &'static str,
            current: T,
            opts: &[(T, &'static str)],
            set: fn(&mut crate::sidebar::Style, T),
            cx: &mut Context<Shell>,
        ) -> gpui::Div {
            let _ = this;
            let mut seg = ui::segmented();
            for (i, (v, label)) in opts.iter().copied().enumerate() {
                seg = seg.child(ui::segment((id, i), None, label, current == v).on_click(
                    cx.listener(move |this, _, _, cx| {
                        this.change_sidebar(cx, |s| set(&mut s.style, v))
                    }),
                ));
            }
            seg
        }
        div()
            .flex()
            .flex_col()
            .mt(px(10.))
            .pt(crate::ui::sp(4.))
            .border_t_1()
            .border_color(theme::stroke())
            .child(panel_title("Whole sidebar"))
            .child(field_label("Style"))
            .child(choices(
                self,
                "sb-variant",
                style.variant,
                &[
                    (Variant::Card, "Card"),
                    (Variant::Flush, "Flush"),
                    (Variant::Floating, "Floating"),
                ],
                |s, v| s.variant = v,
                cx,
            ))
            .child(field_label(&crate::platform::shortcut_label(
                "When hidden (Ctrl+B)",
            )))
            .child(choices(
                self,
                "sb-collapse",
                style.collapse,
                &[
                    (Collapse::Hide, "Hides"),
                    (Collapse::Icons, "Icons"),
                    (Collapse::Never, "Never"),
                ],
                |s, v| s.collapse = v,
                cx,
            ))
            .child(field_label("Side"))
            .child(choices(
                self,
                "sb-side",
                style.side,
                &[(Side::Left, "Left"), (Side::Right, "Right")],
                |s, v| s.side = v,
                cx,
            ))
            .child(field_label("Rows"))
            .child(choices(
                self,
                "sb-density",
                style.density,
                &[
                    (Density::Compact, "Compact"),
                    (Density::Normal, "Normal"),
                    (Density::Roomy, "Roomy"),
                ],
                |s, v| s.density = v,
                cx,
            ))
            .child(field_label("Width"))
            .child({
                let mut seg = ui::segmented();
                for (i, w) in [180., 200., 240., 280.].into_iter().enumerate() {
                    seg = seg.child(
                        ui::segment(
                            ("sb-width", i),
                            None,
                            format!("{w}"),
                            (self.rail_w - w).abs() < 1.,
                        )
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.rail_w = w;
                            this.save(cx);
                            cx.notify();
                        })),
                    );
                }
                seg
            })
            .child(
                ui::switch_row("sb-badges", "Counters", style.badges).on_click(cx.listener(
                    |this, _, _, cx| this.change_sidebar(cx, |s| s.style.badges = !s.style.badges),
                )),
            )
            .child(
                ui::switch_row("sb-shortcuts", "Shortcuts", style.shortcuts).on_click(cx.listener(
                    |this, _, _, cx| {
                        this.change_sidebar(cx, |s| s.style.shortcuts = !s.style.shortcuts)
                    },
                )),
            )
            .child(
                div()
                    .pt(crate::ui::sp(8.))
                    .text_size(px(ui::TEXT_XS))
                    .text_color(theme::text_faint())
                    .child("Changes apply to your sidebar as you make them."),
            )
    }
}

fn badge_count(b: super::rail_blocks::Badge) -> usize {
    b.count()
}
