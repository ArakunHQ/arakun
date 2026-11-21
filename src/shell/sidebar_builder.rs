//! Editing the sidebar: the selection, undo, the pieces that can be added,
//! dragging, and the options of a piece or a segment. Customize's Sidebar
//! tab and the rail use these while the sidebar is being arranged
//! (`shell::arrange`). Every change applies to the rail at once and saves
//! (`crate::sidebar`).

use gpui::Point;

use super::rail_blocks::url_title;
use super::*;
use crate::input::{InputEvent, TextInput};
use crate::sidebar::{
    BadgeMode, Item, Mode, Path as ItemPath, Piece, Pin, Preset, Sidebar, TitleAction,
};

/// What the right column is about.
#[derive(Clone, Copy, PartialEq)]
pub(super) enum BuilderSel {
    Item(ItemPath),
    Section(usize),
}

/// The text fields of the right column, made for the current selection.
pub(super) struct BuilderInputs {
    pub(super) sel: BuilderSel,
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
    pub(super) input: Entity<TextInput>,
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
    pub(super) from: Option<ItemPath>,
    pub(super) item: Item,
    pub(super) title: SharedString,
    pub(super) offset: Point<Pixels>,
}

/// A segment lifted by its handle.
#[derive(Clone)]
pub(super) struct DraggedSection {
    pub(super) from: usize,
    pub(super) title: SharedString,
    pub(super) offset: Point<Pixels>,
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

    /// A preset lays out the pieces; the look (card, side) stays as chosen.
    pub(super) fn apply_sidebar_preset(&mut self, preset: Preset, cx: &mut Context<Self>) {
        self.change_sidebar(cx, |s| {
            let style = s.style;
            *s = preset.layout();
            s.style = style;
        });
        self.clear_builder_selection();
    }

    /// The preset the sidebar is laid out as, if it is one.
    pub(super) fn sidebar_preset(&self) -> Option<Preset> {
        Preset::ALL
            .into_iter()
            .find(|p| p.layout().sections == self.sidebar.sections)
    }

    pub(super) fn undo_sidebar(&mut self, redo: bool, cx: &mut Context<Self>) {
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

    pub(super) fn select_builder(
        &mut self,
        sel: BuilderSel,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
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

    pub(super) fn ask_pin(&mut self, ask: PinAsk, window: &mut Window, cx: &mut Context<Self>) {
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
        self.builder.prompt = Some(BuilderPrompt { input, _sub: sub });
        cx.notify();
    }

    /// A click on a palette piece: at the end of the selected segment, else
    /// of the first scrolling one.
    pub(super) fn add_piece(&mut self, piece: Piece, cx: &mut Context<Self>) {
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
    pub(super) fn tab_pin(&self) -> Option<Pin> {
        let env = self.env();
        let tab = self.strip.active().filter(|t| t.is_in(&env.path))?;
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

    /// Forgets the selection and its fields. They point at a place, not at
    /// a piece: after a move or an import, another piece sits there.
    pub(super) fn clear_builder_selection(&mut self) {
        self.builder.sel = None;
        self.builder.inputs = None;
        self.builder.prompt = None;
    }

    pub(super) fn drop_piece(&mut self, d: &DraggedPiece, to: ItemPath, cx: &mut Context<Self>) {
        self.builder.lifted = None;
        let (from, item) = (d.from, d.item.clone());
        self.change_sidebar(cx, |s| {
            match from {
                Some(from) => s.move_item(from, to),
                None => s.insert(item, to),
            };
        });
        self.clear_builder_selection();
    }

    pub(super) fn render_item_options(
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

    pub(super) fn render_section_options(
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
                Some(TitleAction::New) => "New",
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
}
