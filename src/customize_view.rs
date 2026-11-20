//! The Customize panel: how the app looks, edited at the window's right side
//! while the work stays in view. Colors holds the mode, the palettes and the
//! grays; Shape holds corners, spacing, the window gap, text size and glass.
//! Settings › Appearance only sums up what is chosen here.

use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, App, AppContext as _, Context, Entity, EventEmitter, Focusable as _, FontWeight,
    InteractiveElement as _, IntoElement, ParentElement as _, Render, SharedString,
    StatefulInteractiveElement as _, Styled as _, Subscription, Task, Window, div, px,
};

use crate::input::{InputEvent, TextInput};
use crate::sidebar::{Collapse, Density, Preset, Side, Style, Variant};
use crate::theme::{self, DiffPalette, Grays};
use crate::ui::{self, ButtonKind, TypeScale, icon};

/// How long a palette edit waits for the next before it is saved.
const PALETTE_SAVE_DELAY: std::time::Duration = std::time::Duration::from_millis(500);

/// The panel's width when it opens: three palette cards side by side.
pub const CUSTOMIZE_W: f32 = 320.;

/// The accents a palette of the user's can take.
const ACCENTS: [u32; 8] = [
    theme::AMBER,
    0xff7a59,
    0xff8fab,
    0xb394f5,
    0x8b9dff,
    0x86a6f2,
    0x5fd4c4,
    0x5fd38d,
];

/// What the panel asks the window to do. Grays, text size, diff colors and
/// glass are the window's settings, so the window applies and saves them.
pub enum CustomizeEvent {
    Close,
    OpenGallery,
    Grays(Grays),
    TypeScale(TypeScale),
    DiffPalette(DiffPalette),
    Glass(bool),
    /// The sidebar is the window's: these ask it to change and save it.
    SidebarStyle(Style),
    SidebarPreset(Preset),
    RailWidth(f32),
    ResetSidebar,
    /// The panel moved to another tab.
    Tab(CustomizeTab),
    /// Undo the last sidebar change, or redo it with `true`.
    UndoSidebar(bool),
    /// Copy `look` as a line, with the window's sidebar when `sidebar`.
    CopyLook {
        look: crate::look::Look,
        sidebar: bool,
    },
    /// Use a sidebar layout that came in a look or was pasted.
    ImportLayout(serde_json::Value),
}

/// What the Sidebar tab shows of the window's sidebar, handed over by the
/// window as it draws the panel.
#[derive(Clone, Debug, PartialEq)]
pub struct RailLook {
    pub style: Style,
    pub width: f32,
    /// The preset the pieces are laid out as, if they still are.
    pub preset: Option<Preset>,
    pub can_undo: bool,
    pub can_redo: bool,
    /// What the last import or export did.
    pub note: Option<SharedString>,
    /// "17 pieces in 6 segments, Flush, on the left".
    pub summary: SharedString,
}

/// Draws the Sidebar tab's Add and Selected parts: the sidebar and its
/// editing are the window's, so the window hands this over.
pub type ArrangeSlot = std::rc::Rc<dyn Fn(&mut Window, &mut App) -> Option<AnyElement>>;

/// The widths the Sidebar tab offers.
const RAIL_WIDTHS: [f32; 4] = [180., 200., 240., 280.];

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum CustomizeTab {
    Colors,
    Shape,
    Sidebar,
}

impl CustomizeTab {
    const ALL: [CustomizeTab; 3] = [
        CustomizeTab::Colors,
        CustomizeTab::Shape,
        CustomizeTab::Sidebar,
    ];

    fn label(self) -> &'static str {
        match self {
            CustomizeTab::Colors => "Colors",
            CustomizeTab::Shape => "Shape",
            CustomizeTab::Sidebar => "Sidebar",
        }
    }

    fn icon(self) -> &'static str {
        match self {
            CustomizeTab::Colors => "color-picker",
            CustomizeTab::Shape => "square",
            CustomizeTab::Sidebar => "sidebar-left",
        }
    }
}

/// What travels in a look line, chosen part by part.
#[derive(Clone, Copy, PartialEq)]
struct Parts {
    colors: bool,
    shape: bool,
    sidebar: bool,
}

enum Sheet {
    /// What to put in the line.
    Export(Parts),
    /// A pasted line, once read, and what of it to use.
    Import(Option<(Box<crate::look::Look>, Parts)>),
}

pub struct CustomizeView {
    tab: CustomizeTab,
    /// The palette in use, the user's own, and the shape.
    palettes: crate::palettes::Saved,
    /// Where a shared palette's line is pasted.
    share_input: Entity<TextInput>,
    /// What the last paste or copy did, and whether it failed.
    palette_note: Option<(SharedString, bool)>,
    /// Delete was clicked once on the palette in use.
    confirm_delete: bool,
    /// The name of the user's palette in use.
    name_input: Entity<TextInput>,
    /// Export or Import, open above the tab's settings.
    sheet: Option<Sheet>,
    /// The save of a palette edit that waits for the slider to stop.
    palette_save: Option<Task<()>>,
    /// A palette edit is shown but not saved yet.
    palette_dirty: bool,
    /// The save being written off the UI thread, if one is.
    palette_writing: Option<Task<()>>,
    /// The panel's scroll, so a slider that moves the layout can stay under
    /// the pointer.
    scroll: gpui::ScrollHandle,
    /// The window's sidebar, as last handed over.
    rail: Option<RailLook>,
    arrange: Option<ArrangeSlot>,
    _subs: [Subscription; 4],
}

impl EventEmitter<CustomizeEvent> for CustomizeView {}

impl CustomizeView {
    pub fn new(tab: CustomizeTab, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let share_input = cx.new(|cx| {
            TextInput::new("Paste a look, a color scheme or a sidebar layout", cx).mono()
        });
        let share_sub =
            cx.subscribe_in(
                &share_input,
                window,
                |this: &mut Self, _, ev, _, cx| match ev {
                    InputEvent::Submit => this.read_import(cx),
                    InputEvent::Cancel => {
                        this.sheet = None;
                        cx.notify();
                    }
                    _ => {}
                },
            );
        let palettes = crate::palettes::load();
        let name_input = cx.new(|cx| {
            let mut i = TextInput::new("Name this color scheme", cx);
            i.set_text(palettes.active().name, cx);
            i
        });
        let name_sub = cx.subscribe_in(&name_input, window, |this: &mut Self, input, ev, _, cx| {
            if let InputEvent::Changed = ev {
                let name = input.read(cx).text().trim().to_string();
                if !name.is_empty() && name != this.palettes.active().name {
                    this.edit_palette(|p| p.name = name, cx);
                }
            }
        });
        Self {
            tab,
            palettes,
            share_input,
            palette_note: None,
            confirm_delete: false,
            name_input,
            sheet: None,
            palette_save: None,
            palette_dirty: false,
            palette_writing: None,
            scroll: gpui::ScrollHandle::new(),
            rail: None,
            arrange: None,
            _subs: [
                share_sub,
                name_sub,
                // An edit made just before the panel closes or the app quits
                // is saved then rather than lost with its wait.
                cx.on_release(|this, _| this.flush_palette()),
                cx.on_app_quit(|this, _| {
                    this.flush_palette();
                    async {}
                }),
            ],
        }
    }

    /// The window's sidebar as it is now; redraws only when it changed.
    pub fn sync_rail(&mut self, look: RailLook, cx: &mut Context<Self>) {
        if self.rail.as_ref() != Some(&look) {
            self.rail = Some(look);
            cx.notify();
        }
    }

    pub fn set_arrange(&mut self, slot: ArrangeSlot) {
        self.arrange = Some(slot);
    }

    pub fn set_tab(&mut self, tab: CustomizeTab, cx: &mut Context<Self>) {
        if self.tab != tab {
            self.tab = tab;
            cx.emit(CustomizeEvent::Tab(tab));
        }
        cx.notify();
    }

    /// The palettes and shape as shown now, saved or not.
    pub fn saved(&self) -> &crate::palettes::Saved {
        &self.palettes
    }

    fn use_palette(&mut self, id: &str, cx: &mut Context<Self>) {
        self.palettes.current = id.to_string();
        self.confirm_delete = false;
        self.apply_palette(cx);
    }

    fn apply_palette(&mut self, cx: &mut Context<Self>) {
        self.palette_save = None;
        self.palette_dirty = true;
        self.write_palette(cx);
        self.show_palette(cx);
    }

    /// Shows the palette in use across the app without saving it.
    fn show_palette(&mut self, cx: &mut Context<Self>) {
        crate::palettes::apply(&self.palettes);
        // The name field follows the palette in use, unless it is what changed.
        let name = self.palettes.active().name;
        if self.name_input.read(cx).text().trim() != name {
            self.name_input.update(cx, |i, cx| i.set_text(name, cx));
        }
        cx.refresh_windows();
        cx.notify();
    }

    /// Changes the palette in use, which is one of the user's (the editor
    /// shows only for those).
    fn edit_palette(&mut self, change: impl FnOnce(&mut theme::Palette), cx: &mut Context<Self>) {
        let mut p = self.palettes.active();
        if !self.palettes.is_custom(&p.id) {
            return;
        }
        change(&mut p);
        self.palettes.update(p);
        self.save_palette_later(cx);
    }

    /// Shows a change now and saves it once changes settle: a slider drag or
    /// typing a name changes the palette many times a second, and each save
    /// is a synced write.
    fn save_palette_later(&mut self, cx: &mut Context<Self>) {
        self.show_palette(cx);
        self.palette_dirty = true;
        self.palette_save = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(PALETTE_SAVE_DELAY).await;
            let _ = this.update(cx, |this, cx| this.write_palette(cx));
        }));
    }

    /// Saves the palettes off the UI thread: each save is a synced write,
    /// which can take long enough to stall typing and drawing. One save at a
    /// time; an edit made while it writes is saved when it is done. A failed
    /// save stays dirty, so closing the panel or quitting tries again.
    fn write_palette(&mut self, cx: &mut Context<Self>) {
        if !self.palette_dirty || self.palette_writing.is_some() {
            return;
        }
        self.palette_dirty = false;
        let (saved, version) = (self.palettes.clone(), crate::palettes::next_version());
        self.palette_writing = Some(cx.spawn(async move |this, cx| {
            let ok = cx
                .background_executor()
                .spawn(async move { crate::palettes::store_version(&saved, version) })
                .await;
            let _ = this.update(cx, |this, cx| {
                this.palette_writing = None;
                if !ok {
                    this.palette_dirty = true;
                } else if this.palette_dirty {
                    this.write_palette(cx);
                }
            });
        }));
    }

    /// Saves a palette edit that is still waiting, if there is one, right
    /// away: the panel is closing or the app is quitting, and a save left to
    /// a task would be dropped with it.
    fn flush_palette(&mut self) {
        // A save still writing may not finish once its task is dropped.
        if self.palette_dirty || self.palette_writing.take().is_some() {
            let version = crate::palettes::next_version();
            self.palette_dirty = !crate::palettes::store_version(&self.palettes, version);
        }
    }

    /// A copy of the palette in use that the user can change.
    fn customize_palette(&mut self, cx: &mut Context<Self>) {
        let from = self.palettes.active();
        self.palettes.add(&from);
        self.palette_note = None;
        self.confirm_delete = false;
        self.apply_palette(cx);
    }

    /// Reads what was pasted: a look shows what it holds to choose from, a
    /// palette is added and used, a sidebar layout waits to be applied.
    fn read_import(&mut self, cx: &mut Context<Self>) {
        let text = self.share_input.read(cx).text().to_string();
        if let Some(look) = crate::look::Look::decode(&text) {
            // The sidebar replaces the whole layout, so it is opt-in.
            let parts = Parts {
                colors: look.colors.is_some(),
                shape: look.shape.is_some(),
                sidebar: false,
            };
            self.sheet = Some(Sheet::Import(Some((Box::new(look), parts))));
            self.palette_note = None;
        } else if let Some(p) = theme::Palette::from_share_code(&text) {
            crate::look::adopt_palette(&mut self.palettes, &p);
            self.share_input.update(cx, |i, cx| i.set_text("", cx));
            self.sheet = None;
            // A Delete armed for the old palette must not delete the new one.
            self.confirm_delete = false;
            self.palette_note = Some(("Color scheme added and in use.".into(), false));
            self.apply_palette(cx);
        } else if let Ok(layout) = serde_json::from_str::<serde_json::Value>(text.trim())
            && layout.get("sections").is_some()
        {
            let look = crate::look::Look {
                sidebar: Some(layout),
                ..Default::default()
            };
            let parts = Parts {
                colors: false,
                shape: false,
                sidebar: true,
            };
            self.sheet = Some(Sheet::Import(Some((Box::new(look), parts))));
            self.palette_note = None;
        } else {
            self.palette_note = Some((
                "That isn't a look (arakun:look/v1:…), a color scheme (arakun:palette/v1:…) or a sidebar layout."
                    .into(),
                true,
            ));
        }
        cx.notify();
    }

    /// Uses the parts of a pasted look that were chosen.
    fn apply_import(&mut self, cx: &mut Context<Self>) {
        let Some(Sheet::Import(Some((look, parts)))) = self.sheet.take() else {
            return;
        };
        let mut used = Vec::new();
        if parts.colors
            && let Some(c) = &look.colors
        {
            crate::look::adopt_palette(&mut self.palettes, &c.palette);
            self.palettes.light = c.light;
            self.confirm_delete = false;
            cx.emit(CustomizeEvent::Grays(c.grays()));
            cx.emit(CustomizeEvent::DiffPalette(c.diff()));
            used.push("colors");
        }
        if parts.shape
            && let Some(s) = &look.shape
        {
            self.palettes.radius = s.radius;
            self.palettes.space = s.space;
            self.palettes.window_gap = s.window_gap;
            cx.emit(CustomizeEvent::TypeScale(s.text()));
            cx.emit(CustomizeEvent::Glass(s.glass));
            used.push("shape");
        }
        if parts.colors || parts.shape {
            self.apply_palette(cx);
        }
        if parts.sidebar
            && let Some(layout) = look.sidebar
        {
            cx.emit(CustomizeEvent::ImportLayout(layout));
            used.push("sidebar");
        }
        self.share_input.update(cx, |i, cx| i.set_text("", cx));
        self.palette_note = Some((
            if used.is_empty() {
                "Nothing was chosen, so nothing changed.".into()
            } else {
                format!("Applied: {}.", used.join(", ")).into()
            },
            false,
        ));
        cx.notify();
    }

    /// Puts the chosen parts of how the app looks now in a line to share.
    fn copy_look(&mut self, parts: Parts, cx: &mut Context<Self>) {
        let colors = parts.colors.then(|| crate::look::Colors {
            palette: self.palettes.active(),
            light: self.palettes.light,
            grays: theme::grays().id().into(),
            diff: theme::diff_palette().id().into(),
        });
        let shape = parts.shape.then(|| crate::look::Shape {
            radius: self.palettes.radius,
            space: self.palettes.space,
            window_gap: self.palettes.window_gap,
            text: ui::type_scale().id().into(),
            glass: theme::glass(),
        });
        cx.emit(CustomizeEvent::CopyLook {
            look: crate::look::Look {
                colors,
                shape,
                sidebar: None,
            },
            sidebar: parts.sidebar,
        });
        self.sheet = None;
        self.palette_note = Some((
            "Look copied. Anyone can paste it with Import and choose what to use.".into(),
            false,
        ));
        cx.notify();
    }

    /// The Export or Import card, above the tab's settings.
    fn render_sheet(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let sheet = self.sheet.as_ref()?;
        let rail = self.rail.as_ref();
        let colors_note = format!(
            "{}, {} grays, {} diff colors{}",
            self.palettes.active().name,
            theme::grays().label().to_lowercase(),
            theme::diff_palette().label().to_lowercase(),
            if self.palettes.light { ", light" } else { "" }
        );
        let shape_note = Summary::shape_of(&self.palettes);
        let sidebar_note: SharedString = rail
            .map(|r| r.summary.clone())
            .unwrap_or_else(|| "The layout and its style".into());
        let row = |id: &'static str,
                   on: bool,
                   enabled: bool,
                   title: &'static str,
                   note: String,
                   warn: Option<&'static str>,
                   flip: fn(&mut Parts),
                   cx: &mut Context<Self>| {
            div()
                .id(id)
                .flex()
                .items_start()
                .gap(crate::ui::sp(10.))
                .py(crate::ui::sp(6.))
                .when(enabled, |d| d.cursor_pointer())
                .when(!enabled, |d| d.opacity(0.45))
                .child(
                    div()
                        .pt(crate::ui::sp(1.))
                        .child(ui::check_box(on && enabled)),
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
                                .flex()
                                .items_center()
                                .justify_between()
                                .gap(crate::ui::sp(8.))
                                .child(name_label(title))
                                .when_some(warn.filter(|_| on && enabled), |d, w| {
                                    d.child(ui::tag(w, theme::attention()))
                                }),
                        )
                        .child(hint_label(note)),
                )
                .when(enabled, |d| {
                    d.on_click(cx.listener(move |this, _, _, cx| {
                        match &mut this.sheet {
                            Some(Sheet::Export(p)) => flip(p),
                            Some(Sheet::Import(Some((_, p)))) => flip(p),
                            _ => {}
                        }
                        cx.notify();
                    }))
                })
                .into_any_element()
        };
        let card = |title: &'static str, hint: &'static str| {
            div()
                .flex()
                .flex_col()
                .gap(crate::ui::sp(8.))
                .p(crate::ui::sp(12.))
                .rounded(crate::ui::rad(ui::RADIUS_LG))
                .border_1()
                .border_color(theme::border())
                .bg(theme::fill_raised())
                .child(heading(title, None))
                .child(hint_label(hint))
        };
        let cancel = ui::button("look-cancel", "Cancel", ButtonKind::Ghost, true).on_click(
            cx.listener(|this, _, _, cx| {
                this.sheet = None;
                cx.notify();
            }),
        );
        let el = match sheet {
            Sheet::Export(p) => {
                let p = *p;
                card(
                    "Export look",
                    "Choose what travels. Whoever imports it chooses again.",
                )
                .child(row(
                    "look-colors",
                    p.colors,
                    true,
                    "Colors",
                    colors_note,
                    None,
                    |p| p.colors = !p.colors,
                    cx,
                ))
                .child(row(
                    "look-shape",
                    p.shape,
                    true,
                    "Shape",
                    shape_note,
                    None,
                    |p| p.shape = !p.shape,
                    cx,
                ))
                .child(row(
                    "look-sidebar",
                    p.sidebar,
                    true,
                    "Sidebar",
                    sidebar_note.to_string(),
                    Some("Replaces theirs"),
                    |p| p.sidebar = !p.sidebar,
                    cx,
                ))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(crate::ui::sp(6.))
                        .pt(crate::ui::sp(4.))
                        .child(value_label("arakun:look/v1:…".into()))
                        .child(div().flex_1())
                        .child(cancel)
                        .child(
                            ui::button_with(
                                "look-copy",
                                Some("copy01"),
                                "Copy",
                                ButtonKind::Primary,
                                p.colors || p.shape || p.sidebar,
                            )
                            .on_click(cx.listener(
                                move |this, _, _, cx| {
                                    if p.colors || p.shape || p.sidebar {
                                        this.copy_look(p, cx)
                                    }
                                },
                            )),
                        ),
                )
                .into_any_element()
            }
            Sheet::Import(None) => card(
                "Import",
                "Paste a look, a color scheme line or a sidebar layout, then Read.",
            )
            .child(field(self.share_input.clone()))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(crate::ui::sp(6.))
                    .child(div().flex_1())
                    .child(cancel)
                    .child(
                        ui::button("look-read", "Read", ButtonKind::Primary, true)
                            .on_click(cx.listener(|this, _, _, cx| this.read_import(cx))),
                    ),
            )
            .into_any_element(),
            Sheet::Import(Some((look, p))) => {
                let p = *p;
                let has = (
                    look.colors.is_some(),
                    look.shape.is_some(),
                    look.sidebar.is_some(),
                );
                let from_look_colors = look.colors.as_ref().map(|c| {
                    format!(
                        "{}, {} grays",
                        c.palette.name,
                        c.grays().label().to_lowercase()
                    )
                });
                card(
                    "Import look",
                    "Choose what to use. The rest of yours stays.",
                )
                .child(row(
                    "look-in-colors",
                    p.colors,
                    has.0,
                    "Colors",
                    from_look_colors.unwrap_or_else(|| "Not in this line".into()),
                    None,
                    |p| p.colors = !p.colors,
                    cx,
                ))
                .child(row(
                    "look-in-shape",
                    p.shape,
                    has.1,
                    "Shape",
                    if has.1 {
                        "Corners, spacing, window gap, text size and glass".into()
                    } else {
                        "Not in this line".into()
                    },
                    None,
                    |p| p.shape = !p.shape,
                    cx,
                ))
                .child(row(
                    "look-in-sidebar",
                    p.sidebar,
                    has.2,
                    "Sidebar",
                    if has.2 {
                        "Its layout, without commands, which run on a click".into()
                    } else {
                        "Not in this line".into()
                    },
                    Some("Replaces yours"),
                    |p| p.sidebar = !p.sidebar,
                    cx,
                ))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(crate::ui::sp(6.))
                        .pt(crate::ui::sp(4.))
                        .child(div().flex_1())
                        .child(cancel)
                        .child(
                            ui::button("look-apply", "Apply", ButtonKind::Primary, true)
                                .on_click(cx.listener(|this, _, _, cx| this.apply_import(cx))),
                        ),
                )
                .into_any_element()
            }
        };
        Some(el)
    }

    /// Changes how round, roomy or spread out the app is, or its mode,
    /// everywhere at once.
    fn set_shape(
        &mut self,
        change: impl FnOnce(&mut crate::palettes::Saved),
        cx: &mut Context<Self>,
    ) {
        change(&mut self.palettes);
        self.save_palette_later(cx);
    }

    /// The default palette, dark mode, Crisp grays and the usual diff colors.
    /// The user's own palettes stay.
    fn reset_colors(&mut self, cx: &mut Context<Self>) {
        let fresh = crate::palettes::Saved::default();
        self.palettes.current = fresh.current;
        self.palettes.light = fresh.light;
        self.confirm_delete = false;
        self.palette_note = None;
        self.apply_palette(cx);
        cx.emit(CustomizeEvent::Grays(Grays::Crisp));
        cx.emit(CustomizeEvent::DiffPalette(DiffPalette::Default));
    }

    /// The usual corners, spacing, gap and text size, without glass.
    fn reset_shape(&mut self, cx: &mut Context<Self>) {
        let fresh = crate::palettes::Saved::default();
        self.palettes.radius = fresh.radius;
        self.palettes.space = fresh.space;
        self.palettes.window_gap = fresh.window_gap;
        self.apply_palette(cx);
        cx.emit(CustomizeEvent::TypeScale(TypeScale::Compact));
        cx.emit(CustomizeEvent::Glass(false));
    }

    fn render_header(&self, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .flex()
            .flex_none()
            .items_center()
            .h(px(ui::BAR_H))
            .pl(crate::ui::sp(12.))
            .pr(ui::icon_inset())
            .gap(crate::ui::sp(8.))
            .child(icon("paint-board", 14.).text_color(theme::text_muted()))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_size(px(ui::TEXT_MD))
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(theme::text())
                    .child("Customize"),
            )
            .when_some(
                self.rail
                    .as_ref()
                    .filter(|_| self.tab == CustomizeTab::Sidebar)
                    .map(|l| (l.can_undo, l.can_redo)),
                |d, (undo, redo)| {
                    d.child(
                        ui::icon_button("customize-undo", "undo", 14., "Undo")
                            .when(!undo, |b| b.opacity(0.4))
                            .on_click(cx.listener(|_, _, _, cx| {
                                cx.emit(CustomizeEvent::UndoSidebar(false))
                            })),
                    )
                    .child(
                        ui::icon_button("customize-redo", "arrow-turn-forward", 14., "Redo")
                            .when(!redo, |b| b.opacity(0.4))
                            .on_click(cx.listener(|_, _, _, cx| {
                                cx.emit(CustomizeEvent::UndoSidebar(true))
                            })),
                    )
                },
            )
            .child(
                ui::icon_button("customize-close", "cancel01", 14., "Close")
                    .on_click(cx.listener(|_, _, _, cx| cx.emit(CustomizeEvent::Close))),
            )
    }

    /// Colors and Shape as one track across the panel, like the workspace
    /// panel's tabs.
    fn render_tabs(&self, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .flex()
            .flex_none()
            .items_center()
            .h(px(40.))
            .px(crate::ui::sp(8.))
            .child(
                div()
                    .flex()
                    .flex_1()
                    .min_w_0()
                    .items_center()
                    .h(px(28.))
                    .p(crate::ui::sp(2.))
                    .gap(crate::ui::sp(2.))
                    .rounded(crate::ui::rad(8.))
                    .bg(theme::fill_hover())
                    .children(CustomizeTab::ALL.into_iter().map(|tab| {
                        let active = tab == self.tab;
                        let color = if active {
                            theme::content()
                        } else {
                            theme::text_subtle()
                        };
                        div()
                            .id(("customize-tab", tab as usize))
                            .flex()
                            .flex_1()
                            .min_w_0()
                            .items_center()
                            .justify_center()
                            .gap(crate::ui::sp(6.))
                            .h_full()
                            .px(crate::ui::sp(8.))
                            // 2 px inside an 8 px track: parallel to it.
                            .rounded(ui::nested(ui::RADIUS, 2.))
                            .text_size(px(ui::TEXT_SM))
                            .line_height(px(12.))
                            .cursor_pointer()
                            .text_color(color)
                            .when(active, |d| d.bg(theme::selection()))
                            .when(!active, |d| {
                                d.hover(|s| s.bg(theme::hover()).text_color(theme::content()))
                            })
                            .child(icon(tab.icon(), 12.).text_color(color))
                            .child(div().min_w_0().truncate().child(tab.label()))
                            .on_click(cx.listener(move |this, _, _, cx| this.set_tab(tab, cx)))
                    })),
            )
    }

    fn render_palettes(&self, cx: &mut Context<Self>) -> AnyElement {
        let active = self.palettes.active();
        let all = self.palettes.all();
        let card = |i: usize, p: &theme::Palette, cx: &mut Context<Self>| {
            let on = p.id == active.id;
            let id = p.id.clone();
            let bar =
                |w: f32, c: gpui::Hsla| div().h(px(3.)).w(gpui::relative(w)).rounded_full().bg(c);
            // 5 px (padding and border) inside the card's 12 px corner.
            let preview = div()
                .flex()
                .h(px(36.))
                .rounded(ui::nested(ui::RADIUS_LG, 5.))
                .overflow_hidden()
                .bg(p.background())
                .child(
                    div()
                        .w(gpui::relative(0.34))
                        .h_full()
                        .flex()
                        .flex_col()
                        .gap(crate::ui::sp(4.))
                        .p(crate::ui::sp(5.))
                        .rounded_l(ui::nested(ui::RADIUS_LG, 5.))
                        .bg(p.surface(0.07))
                        .child(bar(0.7, p.accent()))
                        .child(bar(0.9, p.content().opacity(0.22))),
                )
                .child(
                    div()
                        .flex_1()
                        .flex()
                        .flex_col()
                        .gap(crate::ui::sp(4.))
                        .p(crate::ui::sp(6.))
                        .child(bar(0.8, p.content().opacity(0.8)))
                        .child(bar(0.55, p.content().opacity(0.25)))
                        .child(div().size(px(6.)).rounded_full().bg(p.accent())),
                );
            div()
                .id(("palette", i))
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .gap(crate::ui::sp(5.))
                .p(crate::ui::sp(4.))
                .rounded(crate::ui::rad(ui::RADIUS_LG))
                .border_1()
                .border_color(if on {
                    theme::active().opacity(0.55)
                } else {
                    theme::stroke()
                })
                .cursor_pointer()
                .hover(|s| s.bg(theme::fill_hover()))
                .child(preview)
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(crate::ui::sp(4.))
                        .px(crate::ui::sp(2.))
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .truncate()
                                .text_size(px(ui::TEXT_XS))
                                .font_weight(FontWeight::MEDIUM)
                                .text_color(if on {
                                    theme::text()
                                } else {
                                    theme::text_muted()
                                })
                                .child(p.name.clone()),
                        )
                        .when(on, |d| {
                            d.child(icon("tick02", 12.).text_color(theme::active()))
                        }),
                )
                .on_click(cx.listener(move |this, _, _, cx| this.use_palette(&id, cx)))
                .into_any_element()
        };
        // The last place makes a copy of the palette in use, to edit.
        let duplicate = div()
            .id("palette-duplicate")
            .flex_1()
            .min_w_0()
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap(crate::ui::sp(4.))
            .min_h(px(62.))
            .rounded(crate::ui::rad(ui::RADIUS_LG))
            .border_1()
            .border_dashed()
            .border_color(theme::border_strong())
            .cursor_pointer()
            .text_size(px(ui::TEXT_XS))
            .text_color(theme::text_subtle())
            .hover(|s| s.bg(theme::fill_hover()).text_color(theme::content()))
            .child(icon("copy01", 13.).text_color(theme::text_subtle()))
            .child("Duplicate")
            .on_click(cx.listener(|this, _, _, cx| this.customize_palette(cx)))
            .into_any_element();
        let mut cards: Vec<AnyElement> = all
            .iter()
            .enumerate()
            .map(|(i, p)| card(i, p, cx))
            .collect();
        cards.push(duplicate);
        let mut grid = div().flex().flex_col().gap(crate::ui::sp(8.));
        let mut cards = cards.into_iter();
        loop {
            let row: Vec<_> = cards.by_ref().take(3).collect();
            if row.is_empty() {
                break;
            }
            let pad = 3 - row.len();
            grid = grid.child(
                div()
                    .flex()
                    .gap(crate::ui::sp(8.))
                    .children(row)
                    .children((0..pad).map(|_| div().flex_1())),
            );
        }
        let custom = self.palettes.is_custom(&active.id);
        div()
            .flex()
            .flex_col()
            .gap(crate::ui::sp(10.))
            .child(heading(
                "Color scheme",
                Some(if custom {
                    "Yours: edit it below"
                } else {
                    "Duplicate one to edit it"
                }),
            ))
            .child(grid)
            .when(custom, |d| d.child(self.render_palette_editor(&active, cx)))
            .into_any_element()
    }

    fn render_palette_editor(&self, p: &theme::Palette, cx: &mut Context<Self>) -> AnyElement {
        let this = cx.entity().downgrade();
        let setter = move |f: fn(&mut theme::Palette, f32)| {
            let this = this.clone();
            move |t: f32, _: &mut Window, cx: &mut App| {
                let _ = this.update(cx, |this, cx| this.edit_palette(|p| f(p, t), cx));
            }
        };
        let line = |title: &'static str, value: String, control: AnyElement| {
            div()
                .flex()
                .flex_col()
                .gap(crate::ui::sp(6.))
                .child(
                    div()
                        .flex()
                        .justify_between()
                        .text_size(px(ui::TEXT_SM))
                        .text_color(theme::text_muted())
                        .child(title)
                        .child(value_label(value)),
                )
                .child(control)
        };
        let dark = theme::Palette {
            bg: 0.03,
            ..p.clone()
        };
        let light = theme::Palette {
            bg: 0.16,
            ..p.clone()
        };
        let gray = theme::Palette {
            tint: 0.,
            ..p.clone()
        };
        let full = theme::Palette {
            tint: 0.6,
            ..p.clone()
        };
        // Colors that wouldn't read are adjusted when drawn; say so, since
        // the slider or swatch then shows something a little different.
        let text_ok = p.readable_bg() >= p.bg;
        let accent_ok = p.readable_accent() == p.accent;
        let readable: (&str, gpui::Hsla) = match (text_ok, accent_ok) {
            (true, true) => ("Text and accent read well", theme::success().into()),
            (false, true) => (
                "The background is drawn darker so text stays readable",
                theme::attention().into(),
            ),
            (true, false) => (
                "The accent is drawn lighter so it stays readable",
                theme::attention().into(),
            ),
            (false, false) => (
                "Drawn with a darker background and a lighter accent so text stays readable",
                theme::attention().into(),
            ),
        };
        let id = p.id.clone();
        let armed = self.confirm_delete;
        div()
            .flex()
            .flex_col()
            .gap(crate::ui::sp(14.))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(crate::ui::sp(8.))
                    .child(field(self.name_input.clone()).flex_1())
                    .child(
                        ui::button(
                            "pal-delete",
                            if armed { "Delete it?" } else { "Delete" },
                            ButtonKind::Danger,
                            true,
                        )
                        .on_click(cx.listener(move |this, _, _, cx| {
                            if this.confirm_delete {
                                this.palettes.remove(&id);
                                this.confirm_delete = false;
                                this.apply_palette(cx);
                            } else {
                                this.confirm_delete = true;
                                cx.notify();
                            }
                        })),
                    ),
            )
            .child(line(
                "Tint",
                format!("{}°", p.hue.round()),
                ui::slider(
                    "pal-hue",
                    p.hue / 360.,
                    ui::hue_track(0.6, 0.55),
                    gpui::hsla(p.hue / 360., 0.6, 0.55, 1.),
                    setter(|p, t| p.hue = hue_at(t)),
                )
                .into_any_element(),
            ))
            .child(line(
                "Background",
                format!("{:.1}%", p.bg * 100.),
                ui::slider(
                    "pal-bg",
                    (p.bg - 0.03) / 0.13,
                    ui::blend_track(dark.background(), light.background()),
                    p.background(),
                    setter(|p, t| p.bg = 0.03 + t * 0.13),
                )
                .into_any_element(),
            ))
            .child(line(
                "Tint strength",
                format!("{}%", (p.tint / 0.6 * 100.).round()),
                ui::slider(
                    "pal-tint",
                    p.tint / 0.6,
                    ui::blend_track(gray.surface(0.2), full.surface(0.2)),
                    p.surface(0.2),
                    setter(|p, t| p.tint = t * 0.6),
                )
                .into_any_element(),
            ))
            .child(line(
                "Accent",
                format!("#{:06x}", p.accent),
                div()
                    .flex()
                    .flex_wrap()
                    .gap(crate::ui::sp(8.))
                    .children(ACCENTS.into_iter().enumerate().map(|(i, c)| {
                        ui::swatch(("pal-accent", i), gpui::rgb(c), p.accent == c).on_click(
                            cx.listener(move |this, _, _, cx| {
                                this.edit_palette(|p| p.accent = c, cx)
                            }),
                        )
                    }))
                    .into_any_element(),
            ))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(crate::ui::sp(6.))
                    .text_size(px(ui::TEXT_XS))
                    .text_color(readable.1)
                    .child(
                        icon(
                            if text_ok && accent_ok {
                                "checkmark-circle01"
                            } else {
                                "alert-circle"
                            },
                            13.,
                        )
                        .flex_none()
                        .text_color(readable.1),
                    )
                    .child(readable.0),
            )
            .into_any_element()
    }

    fn render_colors(&self, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let mode = {
            let mut seg = ui::segmented();
            for (i, m) in theme::Mode::ALL.into_iter().enumerate() {
                seg = seg.child(
                    ui::segment(("cz-mode", i), None, m.label(), theme::mode() == m).on_click(
                        cx.listener(move |this, _, _, cx| {
                            this.set_shape(|s| s.light = m == theme::Mode::Light, cx);
                        }),
                    ),
                );
            }
            inline("Mode", seg.into_any_element())
        };
        let grays = {
            let mut seg = ui::segmented();
            for (i, g) in Grays::ALL.into_iter().enumerate() {
                seg = seg.child(
                    ui::segment(("cz-grays", i), None, g.label(), theme::grays() == g).on_click(
                        cx.listener(move |_, _, _, cx| {
                            cx.emit(CustomizeEvent::Grays(g));
                            cx.notify();
                        }),
                    ),
                );
            }
            inline("Grays", seg.into_any_element())
        };
        let diffs = {
            let now = theme::diff_palette();
            let mut seg = ui::segmented().w_full();
            for p in DiffPalette::ALL {
                seg = seg.child(
                    wide(ui::segment(p.id(), None, p.label(), p == now)).on_click(cx.listener(
                        move |_, _, _, cx| {
                            cx.emit(CustomizeEvent::DiffPalette(p));
                            cx.notify();
                        },
                    )),
                );
            }
            div()
                .flex()
                .flex_col()
                .gap(crate::ui::sp(8.))
                .child(stacked("Diff colors", None, seg.into_any_element()))
                // The session in the middle seldom shows a diff while
                // this is picked, so a few lines show the choice here.
                .child(diff_sample())
                .child(
                    div()
                        .text_size(px(ui::TEXT_XS))
                        .text_color(theme::text_faint())
                        .child(now.note()),
                )
                .into_any_element()
        };
        vec![mode, self.render_palettes(cx), grays, diffs]
    }

    fn render_shape(&self, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let s = &self.palettes;
        let this = cx.entity().downgrade();
        let setter = move |f: fn(&mut crate::palettes::Saved, f32)| {
            let this = this.clone();
            move |t: f32, _: &mut Window, cx: &mut App| {
                let _ = this.update(cx, |this, cx| this.set_shape(|s| f(s, t), cx));
            }
        };
        let presets = |id: &'static str,
                       items: [(&'static str, f32); 3],
                       now: f32,
                       set: fn(&mut crate::palettes::Saved, f32),
                       cx: &mut Context<Self>| {
            let mut seg = ui::segmented().w_full();
            for (i, (label, v)) in items.into_iter().enumerate() {
                seg = seg.child(
                    wide(ui::segment((id, i), None, label, (now - v).abs() < 0.01)).on_click(
                        cx.listener(move |this, _, _, cx| this.set_shape(|s| set(s, v), cx)),
                    ),
                );
            }
            seg
        };
        let line = |title: &'static str,
                    value: String,
                    seg: gpui::Div,
                    slider: gpui::Stateful<gpui::Div>| {
            div()
                .flex()
                .flex_col()
                .gap(crate::ui::sp(8.))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .justify_between()
                        .child(name_label(title))
                        .child(value_label(value)),
                )
                .child(seg)
                .child(slider)
                .into_any_element()
        };
        let track = || ui::blend_track(theme::fill_raised(), theme::fill_selected());
        let text_size = {
            let mut seg = ui::segmented();
            for (i, t) in TypeScale::ALL.into_iter().enumerate() {
                seg = seg.child(
                    ui::segment(("cz-type", i), None, t.label(), ui::type_scale() == t).on_click(
                        cx.listener(move |_, _, _, cx| {
                            cx.emit(CustomizeEvent::TypeScale(t));
                            cx.notify();
                        }),
                    ),
                );
            }
            inline("Text size", seg.into_any_element())
        };
        let glass = theme::glass();
        let glass_row = div()
            .id("cz-glass")
            .flex()
            .items_center()
            .gap(crate::ui::sp(12.))
            .cursor_pointer()
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_w_0()
                    .gap(crate::ui::sp(2.))
                    .child(name_label("Glass"))
                    .child(hint_label(
                        "See-through window and sidebar; the compositor blurs what is behind.",
                    )),
            )
            .child(ui::switch(glass))
            .on_click(cx.listener(move |_, _, _, cx| {
                cx.emit(CustomizeEvent::Glass(!glass));
                cx.notify();
            }))
            .into_any_element();
        vec![
            line(
                "Corners",
                format!("{:.0}%", s.radius * 100.),
                presets(
                    "shape-radius",
                    [("Sharp", 0.5), ("Default", 1.), ("Round", 1.5)],
                    s.radius,
                    |s, v| s.radius = v,
                    cx,
                ),
                ui::pinned_slider(
                    "shape-radius-slider",
                    s.radius / 2.,
                    track(),
                    theme::active(),
                    &self.scroll,
                    setter(|s, t| s.radius = (t * 2. * 20.).round() / 20.),
                ),
            ),
            line(
                "Spacing",
                format!("{:.0}%", s.space * 100.),
                presets(
                    "shape-space",
                    [("Compact", 0.8), ("Default", 1.), ("Roomy", 1.25)],
                    s.space,
                    |s, v| s.space = v,
                    cx,
                ),
                ui::pinned_slider(
                    "shape-space-slider",
                    s.space - 0.6,
                    track(),
                    theme::active(),
                    &self.scroll,
                    setter(|s, t| s.space = ((0.6 + t) * 20.).round() / 20.),
                ),
            ),
            line(
                "Window gap",
                format!("{:.0} px", s.window_gap),
                presets(
                    "shape-gap",
                    [("None", 0.), ("Default", 8.), ("Wide", 14.)],
                    s.window_gap,
                    |s, v| s.window_gap = v,
                    cx,
                ),
                ui::pinned_slider(
                    "shape-gap-slider",
                    s.window_gap / 20.,
                    track(),
                    theme::active(),
                    &self.scroll,
                    setter(|s, t| s.window_gap = (t * 20.).round()),
                ),
            ),
            text_size,
            glass_row,
        ]
    }

    fn render_sidebar(&self, window: &mut Window, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let Some(look) = self.rail.clone() else {
            return Vec::new();
        };
        let arranged = self.arrange.clone().and_then(|slot| slot(window, cx));
        let style = look.style;
        fn choices<T: Copy + PartialEq + 'static>(
            id: &'static str,
            current: T,
            opts: &[(T, &'static str)],
            set: fn(&mut Style, T),
            style: Style,
            cx: &mut Context<CustomizeView>,
        ) -> AnyElement {
            let mut seg = ui::segmented().w_full();
            for (i, (v, label)) in opts.iter().copied().enumerate() {
                seg = seg.child(
                    wide(ui::segment((id, i), None, label, current == v)).on_click(cx.listener(
                        move |_, _, _, cx| {
                            let mut s = style;
                            set(&mut s, v);
                            cx.emit(CustomizeEvent::SidebarStyle(s));
                        },
                    )),
                );
            }
            seg.into_any_element()
        }
        let presets = div()
            .flex()
            .flex_wrap()
            .gap(crate::ui::sp(4.))
            .children(Preset::ALL.into_iter().enumerate().map(|(i, p)| {
                ui::segment(("cz-preset", i), None, p.label(), look.preset == Some(p))
                    .border_1()
                    .border_color(theme::border_subtle())
                    .tooltip(ui::tip_text(p.blurb()))
                    .on_click(
                        cx.listener(move |_, _, _, cx| cx.emit(CustomizeEvent::SidebarPreset(p))),
                    )
            }))
            .into_any_element();
        let width = {
            let mut seg = ui::segmented().w_full();
            for (i, w) in RAIL_WIDTHS.into_iter().enumerate() {
                seg = seg.child(
                    wide(ui::segment(
                        ("cz-rail-width", i),
                        None,
                        format!("{w:.0}"),
                        (look.width - w).abs() < 1.,
                    ))
                    .on_click(
                        cx.listener(move |_, _, _, cx| cx.emit(CustomizeEvent::RailWidth(w))),
                    ),
                );
            }
            seg.into_any_element()
        };
        let switch = |id: &'static str, title: &'static str, on: bool, flip: fn(&mut Style)| {
            div()
                .id(id)
                .flex()
                .items_center()
                .justify_between()
                .gap(crate::ui::sp(12.))
                .cursor_pointer()
                .child(name_label(title))
                .child(ui::switch(on))
                .on_click(cx.listener(move |_, _, _, cx| {
                    let mut s = style;
                    flip(&mut s);
                    cx.emit(CustomizeEvent::SidebarStyle(s));
                }))
                .into_any_element()
        };

        let switches = div()
            .flex()
            .flex_col()
            .gap(crate::ui::sp(12.))
            .child(switch("cz-badges", "Counters", style.badges, |s| {
                s.badges = !s.badges
            }))
            .child(switch("cz-shortcuts", "Shortcuts", style.shortcuts, |s| {
                s.shortcuts = !s.shortcuts
            }))
            .into_any_element();
        let mut parts: Vec<AnyElement> = look
            .note
            .clone()
            .map(|n| hint_label(n).into_any_element())
            .into_iter()
            .chain(arranged)
            .collect();
        parts.extend([
            stacked(
                "Start from",
                look.preset.is_none().then_some("Arranged by you"),
                presets,
            ),
            stacked(
                "Style",
                None,
                choices(
                    "cz-variant",
                    style.variant,
                    &[
                        (Variant::Card, "Card"),
                        (Variant::Flush, "Flush"),
                        (Variant::Floating, "Floating"),
                    ],
                    |s, v| s.variant = v,
                    style,
                    cx,
                ),
            ),
            stacked(
                "Rows",
                None,
                choices(
                    "cz-density",
                    style.density,
                    &[
                        (Density::Compact, "Compact"),
                        (Density::Normal, "Normal"),
                        (Density::Roomy, "Roomy"),
                    ],
                    |s, v| s.density = v,
                    style,
                    cx,
                ),
            ),
            stacked("Width", None, width),
            stacked(
                "Side",
                None,
                choices(
                    "cz-side",
                    style.side,
                    &[(Side::Left, "Left"), (Side::Right, "Right")],
                    |s, v| s.side = v,
                    style,
                    cx,
                ),
            ),
            stacked(
                "When hidden",
                Some("Ctrl+B"),
                choices(
                    "cz-collapse",
                    style.collapse,
                    &[
                        (Collapse::Hide, "Hides"),
                        (Collapse::Icons, "Icons"),
                        (Collapse::Never, "Never"),
                    ],
                    |s, v| s.collapse = v,
                    style,
                    cx,
                ),
            ),
            switches,
        ]);
        parts
    }

    /// The card at the foot of Colors and Shape: the colors in use, live, and
    /// the way into the Design system tab, which shows every component.
    fn render_preview(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let strip = [
            theme::text(),
            theme::text_subtle(),
            theme::text_faint(),
            theme::border_strong(),
            theme::active(),
            theme::success().into(),
            theme::error().into(),
            theme::attention().into(),
            theme::link().into(),
        ];
        div()
            .flex()
            .flex_none()
            .flex_col()
            .gap(crate::ui::sp(10.))
            .mx(crate::ui::sp(12.))
            .p(crate::ui::sp(10.))
            .rounded(crate::ui::rad(ui::RADIUS_LG))
            .border_1()
            .border_color(theme::stroke())
            .bg(theme::fill_raised())
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .child(name_label("See it on every component"))
                    .child(ui::overline("Live")),
            )
            .child(div().flex().gap(crate::ui::sp(4.)).children(strip.map(|c| {
                div()
                    .flex_1()
                    .h(px(14.))
                    .rounded(crate::ui::rad(ui::RADIUS_SM))
                    .border_1()
                    .border_color(theme::border_subtle())
                    .bg(c)
            })))
            .child(
                ui::button_with(
                    "customize-gallery",
                    Some("dashboard-square01"),
                    "Open Design system",
                    ButtonKind::Outline,
                    true,
                )
                .w_full()
                .justify_center()
                .on_click(cx.listener(|_, _, _, cx| cx.emit(CustomizeEvent::OpenGallery))),
            )
    }

    fn render_footer(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let tab = self.tab;
        div()
            .flex()
            .flex_none()
            .items_center()
            .gap(crate::ui::sp(4.))
            .px(crate::ui::sp(8.))
            .py(crate::ui::sp(8.))
            .border_t_1()
            .border_color(theme::border_subtle())
            .child(
                ui::button_with(
                    "customize-reset",
                    Some("refresh01"),
                    match tab {
                        CustomizeTab::Colors => "Reset colors",
                        CustomizeTab::Shape => "Reset shape",
                        CustomizeTab::Sidebar => "Reset sidebar",
                    },
                    ButtonKind::Ghost,
                    true,
                )
                .on_click(cx.listener(move |this, _, _, cx| match tab {
                    CustomizeTab::Colors => this.reset_colors(cx),
                    CustomizeTab::Shape => this.reset_shape(cx),
                    CustomizeTab::Sidebar => cx.emit(CustomizeEvent::ResetSidebar),
                })),
            )
            .child(div().flex_1())
            .child(
                ui::button("customize-import", "Import", ButtonKind::Ghost, true).on_click(
                    cx.listener(|this, _, window, cx| {
                        this.sheet = Some(Sheet::Import(None));
                        this.palette_note = None;
                        window.focus(&this.share_input.focus_handle(cx), cx);
                        cx.notify();
                    }),
                ),
            )
            .child(
                ui::button("customize-export", "Export…", ButtonKind::Outline, true)
                    .tooltip(ui::tip(
                        "Copy the colors, shape or sidebar as one line to share",
                    ))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.sheet = Some(Sheet::Export(Parts {
                            colors: true,
                            shape: true,
                            sidebar: tab == CustomizeTab::Sidebar,
                        }));
                        this.palette_note = None;
                        cx.notify();
                    })),
            )
    }
}

impl Render for CustomizeView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let parts = match self.tab {
            CustomizeTab::Colors => self.render_colors(cx),
            CustomizeTab::Shape => self.render_shape(cx),
            CustomizeTab::Sidebar => self.render_sidebar(window, cx),
        };
        let note = self.palette_note.clone().map(|(note, bad)| {
            div()
                .text_size(px(ui::TEXT_SM))
                .text_color(if bad {
                    theme::error().into()
                } else {
                    theme::text_subtle()
                })
                .child(note)
                .into_any_element()
        });
        let parts: Vec<AnyElement> = note
            .into_iter()
            .chain(self.render_sheet(cx))
            .chain(parts)
            .collect();
        // The sidebar is beside the panel already; the card shows colors.
        // Export and Import need the room more than it does.
        let preview = self.tab != CustomizeTab::Sidebar && self.sheet.is_none();
        div()
            .flex()
            .flex_col()
            .size_full()
            .min_h_0()
            .child(self.render_header(cx))
            .child(self.render_tabs(cx))
            .child(
                div()
                    .id("customize-scroll")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .track_scroll(&self.scroll)
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .px(crate::ui::sp(12.))
                            .pt(crate::ui::sp(4.))
                            .pb(crate::ui::sp(16.))
                            .children(parts.into_iter().enumerate().map(|(i, part)| {
                                div()
                                    .py(crate::ui::sp(12.))
                                    .when(i > 0, |d| {
                                        d.border_t_1().border_color(theme::border_subtle())
                                    })
                                    .child(part)
                            })),
                    ),
            )
            .when(preview, |d| {
                d.child(self.render_preview(cx))
                    .child(div().h(px(8.)).flex_none())
            })
            .child(self.render_footer(cx))
    }
}

/// A segment that shares a full-width track evenly with the others.
fn wide(segment: gpui::Stateful<gpui::Div>) -> gpui::Stateful<gpui::Div> {
    segment.flex_1().min_w_0().justify_center()
}

/// A text field as tall as a button.
fn field(input: Entity<TextInput>) -> gpui::Div {
    div()
        .flex()
        .min_w_0()
        .items_center()
        .h(px(ui::BUTTON_H))
        .px(crate::ui::sp(8.))
        .rounded(crate::ui::rad(ui::RADIUS))
        .border_1()
        .border_color(theme::border())
        .bg(theme::background())
        .text_size(px(ui::TEXT_SM))
        .overflow_hidden()
        .child(input)
}

fn name_label(title: impl Into<SharedString>) -> gpui::Div {
    div()
        .text_size(px(ui::TEXT_MD))
        .font_weight(FontWeight::MEDIUM)
        .text_color(theme::text())
        .child(title.into())
}

fn hint_label(hint: impl Into<SharedString>) -> gpui::Div {
    div()
        .text_size(px(ui::TEXT_SM))
        .text_color(theme::text_subtle())
        .child(hint.into())
}

fn value_label(value: String) -> gpui::Div {
    div()
        .font_family(theme::FONT_MONO)
        .text_size(px(ui::TEXT_XS))
        .text_color(theme::text_subtle())
        .child(value)
}

/// A part's name, and on its right a short note.
fn heading(title: &'static str, note: Option<&'static str>) -> gpui::Div {
    div()
        .flex()
        .items_center()
        .justify_between()
        .gap(crate::ui::sp(8.))
        .child(name_label(title))
        .when_some(note, |d, note| {
            d.child(
                div()
                    .text_size(px(ui::TEXT_XS))
                    .text_color(theme::text_faint())
                    .child(note),
            )
        })
}

/// A setting with its control on the same line, for a narrow control.
fn inline(title: &'static str, control: AnyElement) -> AnyElement {
    div()
        .flex()
        .items_center()
        .justify_between()
        .gap(crate::ui::sp(12.))
        .child(name_label(title))
        .child(control)
        .into_any_element()
}

/// A setting with its control under it, for one too wide for the line.
fn stacked(title: &'static str, note: Option<&'static str>, control: AnyElement) -> AnyElement {
    div()
        .flex()
        .flex_col()
        .gap(crate::ui::sp(8.))
        .child(heading(title, note))
        .child(control)
        .into_any_element()
}

/// The lines of the diff sample: added (`Some(true)`), removed
/// (`Some(false)`) or around them (`None`).
const DIFF_SAMPLE: [(Option<bool>, &str); 5] = [
    (None, "fn wait(n: u32) -> u64 {"),
    (Some(false), "    let ms = 100;"),
    (Some(true), "    let ms = 100 * 2u64.pow(n);"),
    (Some(true), "    ms.min(30_000)"),
    (None, "}"),
];

/// A few lines of a diff drawn the way the chat draws one, in the diff
/// colors now chosen.
fn diff_sample() -> AnyElement {
    let count = |kind: bool| DIFF_SAMPLE.iter().filter(|(k, _)| *k == Some(kind)).count();
    let line = |(kind, text): (Option<bool>, &'static str)| {
        let (mark, color, bar, bg) = match kind {
            Some(true) => (
                "+",
                gpui::Hsla::from(theme::diff_add()),
                Some(theme::diff_add_bar()),
                Some(theme::diff_add_bg()),
            ),
            Some(false) => (
                "−",
                gpui::Hsla::from(theme::diff_del()),
                Some(theme::diff_del_bar()),
                Some(theme::diff_del_bg()),
            ),
            None => (" ", gpui::transparent_black(), None, None),
        };
        div()
            .relative()
            .flex()
            .items_center()
            .h(px(18.))
            .when_some(bg, |d, bg| d.bg(bg))
            .when_some(bar, |d, bar| {
                d.child(
                    div()
                        .absolute()
                        .left_0()
                        .top_0()
                        .bottom_0()
                        .w(px(2.))
                        .bg(bar),
                )
            })
            .child(
                div()
                    .flex_none()
                    .w(px(20.))
                    .flex()
                    .justify_center()
                    .text_color(color)
                    .child(mark),
            )
            .child(
                div()
                    .text_color(if kind.is_some() {
                        theme::text_muted()
                    } else {
                        theme::text_subtle()
                    })
                    .child(text),
            )
    };
    div()
        .flex()
        .flex_col()
        .overflow_hidden()
        .rounded(crate::ui::rad(8.))
        .border_1()
        .border_color(theme::border())
        .bg(theme::sunk())
        .font_family(theme::FONT_MONO)
        .text_size(px(ui::TEXT_XS))
        .child(
            div()
                .flex()
                .items_center()
                .gap(crate::ui::sp(8.))
                .h(px(22.))
                .px(crate::ui::sp(10.))
                .border_b_1()
                .border_color(theme::border_subtle())
                .child(
                    div()
                        .flex_1()
                        .text_color(theme::text_subtle())
                        .child("retry.rs"),
                )
                .child(
                    div()
                        .text_color(theme::diff_add())
                        .child(format!("+{}", count(true))),
                )
                .child(
                    div()
                        .text_color(theme::diff_del())
                        .child(format!("−{}", count(false))),
                ),
        )
        .child(
            div()
                .py(crate::ui::sp(2.))
                .children(DIFF_SAMPLE.into_iter().map(line)),
        )
        .into_any_element()
}

/// The tint at slider position `t`. 360° is 0° again, which would send the
/// thumb back to the left end, so the right end stops just short of it.
fn hue_at(t: f32) -> f32 {
    (t * 360.).clamp(0., 359.)
}

/// What Settings › Appearance says is chosen, one line per part.
pub struct Summary {
    pub colors: String,
    pub shape: String,
    pub sidebar: String,
}

impl Summary {
    /// "Round corners · Default spacing · 8 px gap".
    pub fn shape_of(saved: &crate::palettes::Saved) -> String {
        let corners = preset_name(
            saved.radius,
            &[("Sharp", 0.5), ("Default", 1.), ("Round", 1.5)],
        )
        .map(|n| format!("{n} corners"))
        .unwrap_or_else(|| format!("{:.0}% corners", saved.radius * 100.));
        let spacing = preset_name(
            saved.space,
            &[("Compact", 0.8), ("Default", 1.), ("Roomy", 1.25)],
        )
        .map(|n| format!("{n} spacing"))
        .unwrap_or_else(|| format!("{:.0}% spacing", saved.space * 100.));
        format!("{corners} · {spacing} · {:.0} px gap", saved.window_gap)
    }

    pub fn of(saved: &crate::palettes::Saved, sidebar: &crate::sidebar::Sidebar) -> Summary {
        let mode = if saved.light { "Light" } else { "Dark" };
        let colors = format!(
            "{mode} · {} · {} grays",
            saved.active().name,
            theme::grays().label()
        );
        let shape = Summary::shape_of(saved);
        let style = match sidebar.style.variant {
            Variant::Card => "Card",
            Variant::Flush => "Flush",
            Variant::Floating => "Floating",
        };
        let side = match sidebar.style.side {
            Side::Left => "left",
            Side::Right => "right",
        };
        let pieces = sidebar
            .sections
            .iter()
            .flat_map(|s| &s.items)
            .filter(|i| !i.piece.is_decoration())
            .count();
        let sidebar = format!(
            "{style}, {side} · {pieces} {}",
            if pieces == 1 { "piece" } else { "pieces" }
        );
        Summary {
            colors,
            shape,
            sidebar,
        }
    }
}

/// The preset a value sits on, if it sits on one.
fn preset_name(value: f32, presets: &[(&'static str, f32)]) -> Option<&'static str> {
    presets
        .iter()
        .find(|(_, v)| (value - v).abs() < 0.01)
        .map(|(n, _)| *n)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_tint_slider_stops_short_of_360() {
        assert_eq!(hue_at(0.), 0.);
        assert_eq!(hue_at(0.5), 180.);
        assert_eq!(hue_at(1.), 359.);
    }

    #[test]
    fn the_summary_names_presets_and_falls_back_to_numbers() {
        let mut saved = crate::palettes::Saved::default();
        let mut sidebar = crate::sidebar::Preset::Focused.layout();
        let s = Summary::of(&saved, &sidebar);
        assert!(s.colors.starts_with("Dark · "), "{}", s.colors);
        assert_eq!(s.shape, "Default corners · Default spacing · 8 px gap");

        saved.light = true;
        saved.radius = 1.5;
        saved.space = 0.9;
        saved.window_gap = 0.;
        sidebar.style.variant = Variant::Card;
        sidebar.style.side = Side::Right;
        sidebar.sections = vec![crate::sidebar::Section::new(
            "A",
            crate::sidebar::Mode::Fixed,
            vec![
                crate::sidebar::Piece::Search,
                crate::sidebar::Piece::Separator,
            ],
        )];
        let s = Summary::of(&saved, &sidebar);
        assert_eq!(s.sidebar, "Card, right · 1 piece");
        assert!(s.colors.starts_with("Light · "), "{}", s.colors);
        assert_eq!(s.shape, "Round corners · 90% spacing · 0 px gap");
    }
}
