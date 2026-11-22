//! The design system's pieces beyond buttons and dialogs: the type scale, and
//! the chips, badges, menus, meters and stats the views share. Every color
//! comes from a `theme` role, every size from the scale in `ui`, so two views
//! that show the same kind of thing show it the same way.
//!
//! The Design system tab (`gallery_view`) shows all of them.

use std::sync::atomic::{AtomicU8, Ordering};

use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, ElementId, FontWeight, Hsla, InteractiveElement as _, IntoElement,
    ParentElement as _, Role, SharedString, StatefulInteractiveElement as _, Styled,
    StyledImage as _, div, px, relative,
};

use super::{MENU_ROW_H, RADIUS, RADIUS_LG, RADIUS_SM, icon};
use crate::theme;

// ---------------------------------------------------------------------------
// Type: each style is a size, a line height and a weight together, so text of
// one kind always sits the same way.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Type {
    /// The one big number of a page.
    Display,
    /// A page's title.
    Title,
    /// A dialog's or settings section's title.
    Subtitle,
    /// A section or card heading.
    Heading,
    /// Running text and table cells.
    Body,
    /// Controls, chips, form labels.
    Label,
    /// Hints and meta under a value.
    Caption,
    /// An eyebrow above a group, in mono; the caller upper-cases the text.
    Overline,
    /// Code, paths, ids.
    Mono,
}

impl Type {
    pub const ALL: [Type; 9] = [
        Type::Display,
        Type::Title,
        Type::Subtitle,
        Type::Heading,
        Type::Body,
        Type::Label,
        Type::Caption,
        Type::Overline,
        Type::Mono,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Type::Display => "Display",
            Type::Title => "Title",
            Type::Subtitle => "Subtitle",
            Type::Heading => "Heading",
            Type::Body => "Body",
            Type::Label => "Label",
            Type::Caption => "Caption",
            Type::Overline => "Overline",
            Type::Mono => "Mono",
        }
    }

    /// (size, line height, weight) in the current scale.
    pub fn metrics(self) -> (f32, f32, FontWeight) {
        use FontWeight as W;
        let comfy = type_scale() == TypeScale::Comfortable;
        match (self, comfy) {
            (Type::Display, false) => (32., 38., W::SEMIBOLD),
            (Type::Display, true) => (36., 42., W::SEMIBOLD),
            (Type::Title, false) => (20., 26., W::SEMIBOLD),
            (Type::Title, true) => (22., 28., W::SEMIBOLD),
            (Type::Subtitle, false) => (16., 22., W::SEMIBOLD),
            (Type::Subtitle, true) => (17., 24., W::SEMIBOLD),
            (Type::Heading, false) => (14., 20., W::SEMIBOLD),
            (Type::Heading, true) => (15., 22., W::SEMIBOLD),
            (Type::Body, false) => (13., 18., W::NORMAL),
            (Type::Body, true) => (14., 20., W::NORMAL),
            (Type::Label, false) => (12., 16., W::MEDIUM),
            (Type::Label, true) => (13., 18., W::MEDIUM),
            (Type::Caption, false) => (11., 14., W::NORMAL),
            (Type::Caption, true) => (12., 16., W::NORMAL),
            (Type::Overline, _) => (11., 14., W::MEDIUM),
            (Type::Mono, false) => (12., 18., W::NORMAL),
            (Type::Mono, true) => (13., 20., W::NORMAL),
        }
    }
}

/// How large the type runs: the app's current sizes, or one step up.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TypeScale {
    /// Body 13, labels 12, captions 11.
    Compact,
    /// Body 14, labels 13, captions 12.
    Comfortable,
}

impl TypeScale {
    pub const ALL: [TypeScale; 2] = [TypeScale::Compact, TypeScale::Comfortable];

    pub fn label(self) -> &'static str {
        match self {
            TypeScale::Compact => "Compact",
            TypeScale::Comfortable => "Comfortable",
        }
    }

    pub fn id(self) -> &'static str {
        match self {
            TypeScale::Compact => "compact",
            TypeScale::Comfortable => "comfortable",
        }
    }

    pub fn from_id(id: &str) -> Self {
        Self::ALL
            .into_iter()
            .find(|t| t.id() == id)
            .unwrap_or(TypeScale::Compact)
    }
}

static TYPE_SCALE: AtomicU8 = AtomicU8::new(0);

pub fn type_scale() -> TypeScale {
    TypeScale::ALL[TYPE_SCALE.load(Ordering::Relaxed) as usize % TypeScale::ALL.len()]
}

pub fn set_type_scale(t: TypeScale) {
    let ix = TypeScale::ALL.iter().position(|x| *x == t).unwrap_or(0);
    TYPE_SCALE.store(ix as u8, Ordering::Relaxed);
}

/// `.ty(Type::Body)` on any element: size, line height, weight and face at once.
pub trait Typography: Styled + Sized {
    fn ty(self, t: Type) -> Self {
        let (size, line, weight) = t.metrics();
        let s = self
            .text_size(px(size))
            .line_height(px(line))
            .font_weight(weight);
        match t {
            // The brand sets eyebrows in mono, like code: what a machine reads.
            Type::Mono | Type::Overline => s.font_family(theme::FONT_MONO),
            // A page's big number changes in place; tabular figures keep its
            // digits from shifting as it does.
            Type::Display => s.font_features(gpui::FontFeatures(std::sync::Arc::new(vec![(
                "tnum".into(),
                1,
            )]))),
            _ => s,
        }
    }
}

impl<E: Styled> Typography for E {}

/// A line of text in a style and role color.
pub fn text(t: Type, color: Hsla, content: impl Into<SharedString>) -> gpui::Div {
    div().ty(t).text_color(color).child(content.into())
}

/// A section's heading.
pub fn heading(content: impl Into<SharedString>) -> gpui::Div {
    text(Type::Heading, theme::text(), content)
}

/// SMALL CAPS over a group.
pub fn overline(content: &str) -> gpui::Div {
    text(Type::Overline, theme::text_subtle(), content.to_uppercase())
}

// ---------------------------------------------------------------------------
// Status colors.

/// What a badge or meter is saying.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tone {
    Neutral,
    Accent,
    Info,
    Success,
    Warning,
    Danger,
}

impl Tone {
    pub const ALL: [Tone; 6] = [
        Tone::Neutral,
        Tone::Accent,
        Tone::Info,
        Tone::Success,
        Tone::Warning,
        Tone::Danger,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Tone::Neutral => "Neutral",
            Tone::Accent => "Accent",
            Tone::Info => "Info",
            Tone::Success => "Success",
            Tone::Warning => "Warning",
            Tone::Danger => "Danger",
        }
    }

    pub fn color(self) -> Hsla {
        match self {
            Tone::Neutral => theme::text_muted(),
            Tone::Accent => theme::accent(),
            Tone::Info => theme::link().into(),
            Tone::Success => theme::success().into(),
            Tone::Warning => theme::attention().into(),
            Tone::Danger => theme::error().into(),
        }
    }

    /// The health of something that fills up: fine, getting there, full.
    pub fn of_usage(pct: f32) -> Tone {
        if pct >= 100. {
            Tone::Danger
        } else if pct >= 70. {
            Tone::Warning
        } else {
            Tone::Success
        }
    }
}

// ---------------------------------------------------------------------------
// Small labels.

/// A status word on a tinted ground: "Live", "Failed", "Team · Max 5x".
pub fn badge(content: impl Into<SharedString>, tone: Tone) -> gpui::Div {
    if tone == Tone::Neutral {
        tag(content, tone.color()).bg(theme::fill_selected())
    } else {
        tag(content, tone.color())
    }
}

/// `badge` in any color: a kind of record, a category, a PR state.
pub fn tag(content: impl Into<SharedString>, color: impl Into<Hsla>) -> gpui::Div {
    let c: Hsla = color.into();
    div()
        .flex()
        .flex_none()
        .items_center()
        .gap(crate::ui::sp(4.))
        .h(px(20.))
        .px(crate::ui::sp(6.))
        .rounded(crate::ui::rad(RADIUS_SM))
        .bg(c.opacity(0.14))
        .ty(Type::Caption)
        .font_weight(FontWeight::MEDIUM)
        .text_color(c)
        .child(content.into())
}

/// A dot and a word, for states in dense lists.
pub fn status_dot(content: impl Into<SharedString>, tone: Tone) -> gpui::Div {
    legend(content, tone.color())
}

/// A colored dot and a name: a chart's series, a category.
pub fn legend(content: impl Into<SharedString>, color: Hsla) -> gpui::Div {
    div()
        .flex()
        .items_center()
        .gap(crate::ui::sp(6.))
        .ty(Type::Label)
        .text_color(theme::text_muted())
        .child(div().size(px(7.)).rounded_full().bg(color))
        .child(content.into())
}

/// A filter or choice that toggles: outlined, filled when on.
pub fn chip(
    id: impl Into<ElementId>,
    content: impl Into<SharedString>,
    on: bool,
) -> gpui::Stateful<gpui::Div> {
    let content = content.into();
    div()
        .id(id)
        // A toggle button: screen readers say "pressed" when it is on.
        .role(Role::Button)
        .aria_label(content.clone())
        .aria_toggled(crate::ui::toggled(on))
        .flex()
        .flex_none()
        .items_center()
        .gap(crate::ui::sp(6.))
        .h(px(24.))
        .px(crate::ui::sp(9.))
        .rounded_full()
        .border_1()
        .ty(Type::Label)
        .cursor_pointer()
        .map(crate::ui::focusable)
        .when(on, |d| {
            d.bg(theme::fill_selected())
                .border_color(theme::border_strong())
                .text_color(theme::text())
        })
        .when(!on, |d| {
            d.border_color(theme::border())
                .text_color(theme::text_muted())
                .hover(|s| s.bg(theme::fill_hover()).text_color(theme::text()))
        })
        .child(content)
}

/// A key of a shortcut: `kbd("Ctrl")`, `kbd("K")`.
pub fn kbd(key: impl Into<SharedString>) -> gpui::Div {
    div()
        .flex()
        .flex_none()
        .items_center()
        .justify_center()
        .min_w(px(20.))
        .h(px(20.))
        .px(crate::ui::sp(5.))
        .rounded(crate::ui::rad(RADIUS_SM))
        .border_1()
        .border_b_2()
        .border_color(theme::border())
        .bg(theme::fill_raised())
        .ty(Type::Caption)
        .font_family(theme::FONT_MONO)
        .text_color(theme::text_muted())
        .child(key.into())
}

/// A shortcut, keys joined: `shortcut(&["Ctrl", "K"])`, ⌘ K on macOS.
pub fn shortcut(keys: &[&str]) -> gpui::Div {
    let mut row = div().flex().items_center().gap(crate::ui::sp(3.));
    for k in crate::platform::keycaps(keys) {
        row = row.child(kbd(k));
    }
    row
}

// ---------------------------------------------------------------------------
// Controls.

/// On/off, in the accent when on. The knob springs across when it changes;
/// one shown for the first time starts where it belongs. The spring is kept
/// under the nearest element with an id, so give each switch its own row.
pub fn switch(on: bool) -> AnyElement {
    use gpui::AnimationExt as _;
    const TRAVEL: f32 = 30. - 2. * 2. - 14.;
    let (off_track, on_track) = (theme::fill_selected(), theme::active());
    let (off_knob, on_knob) = (theme::content(), theme::background());
    div()
        .flex()
        .flex_none()
        .items_center()
        .w(px(30.))
        .h(px(18.))
        .p(crate::ui::sp(2.))
        .rounded_full()
        .with_spring(
            "switch",
            gpui::SpringAnimation::new(super::motion::SNAP)
                .to(on)
                .with_epsilon(super::motion::SNAP_EPSILON),
            move |d, phase| {
                let color = phase.clamp(0.0..=1.0);
                d.bg(color.interpolate(off_track, on_track)).child(
                    div()
                        .size(px(14.))
                        .rounded_full()
                        .ml(px(TRAVEL * phase.0))
                        .bg(color.interpolate(off_knob, on_knob)),
                )
            },
        )
        .into_any_element()
}

/// A radio dot: a ring with a center when picked.
pub fn radio(on: bool) -> gpui::Div {
    div()
        .flex_none()
        .size(px(14.))
        .rounded_full()
        .border_1()
        .flex()
        .items_center()
        .justify_center()
        .when(on, |d| {
            d.border_color(theme::active())
                .child(div().size(px(6.)).rounded_full().bg(theme::active()))
        })
        .when(!on, |d| d.border_color(theme::border_strong()))
}

/// A small segmented toggle in caps, for switching what a chart or table
/// shows (COST | TOKENS). `ui::segmented` is the larger one for page modes.
pub fn toggle_group<T: Copy + PartialEq + 'static>(
    id: &'static str,
    options: &[(T, &'static str)],
    current: T,
    mut on_pick: impl FnMut(gpui::Stateful<gpui::Div>, T) -> gpui::Stateful<gpui::Div>,
) -> gpui::Div {
    let mut track = div()
        .flex()
        .flex_none()
        .items_center()
        .p(crate::ui::sp(2.))
        .gap(crate::ui::sp(2.))
        .rounded(crate::ui::rad(RADIUS))
        .border_1()
        .border_color(theme::border())
        .bg(theme::fill_raised());
    for (i, (value, label)) in options.iter().enumerate() {
        let on = *value == current;
        let seg = div()
            .id((id, i))
            .role(Role::RadioButton)
            .aria_label(*label)
            .aria_toggled(crate::ui::toggled(on))
            .px(crate::ui::sp(8.))
            .h(px(20.))
            .flex()
            .items_center()
            .rounded(crate::ui::rad(RADIUS_SM))
            .ty(Type::Overline)
            .cursor_pointer()
            .map(crate::ui::focusable)
            .when(on, |d| {
                d.bg(theme::fill_selected()).text_color(theme::text())
            })
            .when(!on, |d| {
                d.text_color(theme::text_subtle())
                    .hover(|s| s.text_color(theme::text()))
            })
            .child(label.to_uppercase());
        track = track.child(on_pick(seg, *value));
    }
    track
}

// ---------------------------------------------------------------------------
// Menus.

pub fn menu_heading(content: &str) -> gpui::Div {
    overline(content)
        .px(crate::ui::sp(8.))
        .pt(crate::ui::sp(6.))
        .pb(crate::ui::sp(3.))
}

pub fn menu_divider() -> gpui::Div {
    div()
        .h(px(1.))
        .mx(px(4.))
        .my(px(3.))
        .bg(theme::border_subtle())
}

/// A pickable row with a radio dot for a menu; the caller adds the name and
/// any note. `name` is what screen readers call it.
pub fn choice_row(
    id: impl Into<ElementId>,
    name: impl Into<SharedString>,
    selected: bool,
) -> gpui::Stateful<gpui::Div> {
    div()
        .id(id)
        .role(Role::MenuItemRadio)
        .aria_label(name)
        .aria_toggled(crate::ui::toggled(selected))
        .flex()
        .items_center()
        .gap(crate::ui::sp(10.))
        .min_h(px(MENU_ROW_H))
        .py(crate::ui::sp(5.))
        .px(crate::ui::sp(8.))
        .rounded(crate::ui::rad(RADIUS))
        .ty(Type::Body)
        .text_color(theme::text())
        .cursor_pointer()
        .map(crate::ui::focusable)
        .hover(|d| d.bg(theme::fill_hover()))
        .when(selected, |d| d.bg(theme::fill_selected()))
        .child(radio(selected))
}

/// A row that opens more choices beside it: name, current value, chevron.
pub fn submenu_row(
    id: impl Into<ElementId>,
    name: &str,
    value: impl Into<SharedString>,
    open: bool,
) -> gpui::Stateful<gpui::Div> {
    let value = value.into();
    div()
        .id(id)
        .role(Role::MenuItem)
        .aria_label(SharedString::from(name.to_string()))
        .aria_value(value.clone())
        .aria_expanded(open)
        .flex()
        .items_center()
        .gap(crate::ui::sp(8.))
        .h(px(MENU_ROW_H))
        .px(crate::ui::sp(8.))
        .rounded(crate::ui::rad(RADIUS))
        .ty(Type::Body)
        .text_color(theme::text())
        .cursor_pointer()
        .map(crate::ui::focusable)
        .when(open, |d| d.bg(theme::fill_hover()))
        .hover(|d| d.bg(theme::fill_hover()))
        .child(div().flex_1().child(name.to_string()))
        .child(
            div()
                .min_w_0()
                .max_w(px(130.))
                .truncate()
                .ty(Type::Label)
                .text_color(theme::text_subtle())
                .child(value),
        )
        .child(icon("arrow-right01", 12.).text_color(theme::text_faint()))
}

/// A menu row that turns something on and off: name and a switch.
pub fn switch_row(id: impl Into<ElementId>, name: &str, on: bool) -> gpui::Stateful<gpui::Div> {
    div()
        .id(id)
        .role(Role::Switch)
        .aria_label(SharedString::from(name.to_string()))
        .aria_toggled(crate::ui::toggled(on))
        .flex()
        .items_center()
        .gap(crate::ui::sp(8.))
        .h(px(MENU_ROW_H))
        .px(crate::ui::sp(8.))
        .rounded(crate::ui::rad(RADIUS))
        .ty(Type::Body)
        .text_color(theme::text())
        .cursor_pointer()
        .map(crate::ui::focusable)
        .hover(|d| d.bg(theme::fill_hover()))
        .child(div().flex_1().child(name.to_string()))
        .child(switch(on))
}

// ---------------------------------------------------------------------------
// Data.

/// A thin bar filled to `pct` (0-100) in the tone's color.
pub fn meter(pct: f32, tone: Tone, height: f32) -> gpui::Div {
    let fill = (pct / 100.).clamp(0., 1.);
    div()
        .h(px(height))
        .rounded_full()
        .bg(theme::fill_selected())
        .child(
            div()
                .h_full()
                .rounded_full()
                .w(relative(fill))
                .when(fill > 0., |d| d.min_w(px(height)))
                .bg(tone.color()),
        )
}

/// One limit on one line: name, when it starts over, a short meter in the
/// health colors and the percentage.
pub fn limit_line(label: &str, pct: f32, detail: Option<String>) -> AnyElement {
    let tone = Tone::of_usage(pct);
    div()
        .flex()
        .items_center()
        .gap(crate::ui::sp(8.))
        .h(px(22.))
        .ty(Type::Label)
        .font_weight(FontWeight::NORMAL)
        .child(
            div()
                .flex_1()
                .min_w_0()
                .truncate()
                .text_color(theme::text_muted())
                .child(label.to_string()),
        )
        .child(
            div()
                .flex_none()
                .ty(Type::Caption)
                .text_color(theme::text_faint())
                .child(detail.unwrap_or_default()),
        )
        .child(meter(pct, tone, 4.).w(px(56.)).flex_none())
        .child(
            div()
                .w(px(36.))
                .flex_none()
                .flex()
                .justify_end()
                .text_color(if pct >= 70. {
                    tone.color()
                } else {
                    theme::text()
                })
                .child(format!("{:.0}%", pct.ceil())),
        )
        .into_any_element()
}

/// One metric: what it is, the value, and what it means underneath.
pub fn stat(
    label: &str,
    value: impl Into<SharedString>,
    sub: impl Into<SharedString>,
) -> gpui::Div {
    div()
        .flex()
        .flex_col()
        .gap(crate::ui::sp(4.))
        .child(
            text(Type::Label, theme::text_subtle(), label.to_string())
                .font_weight(FontWeight::NORMAL),
        )
        .child(
            div()
                .text_size(px(20.))
                .line_height(px(26.))
                .text_color(theme::text())
                .child(value.into()),
        )
        .child(text(Type::Caption, theme::text_faint(), sub))
}

/// A label and value on one ruled line.
pub fn kv(label: &str, value: impl Into<SharedString>) -> gpui::Div {
    div()
        .flex()
        .items_center()
        .h(px(36.))
        .border_b_1()
        .border_color(theme::border_subtle())
        .ty(Type::Body)
        .child(
            div()
                .flex_1()
                .text_color(theme::text_muted())
                .child(label.to_string()),
        )
        .child(div().text_color(theme::text()).child(value.into()))
}

/// What a list or page shows with nothing in it: an icon, what it is for,
/// and the one thing to do about it.
pub fn empty_state(
    icon_name: &str,
    title: &str,
    body: &str,
    action: Option<AnyElement>,
) -> gpui::Div {
    div()
        .flex()
        .flex_col()
        .items_center()
        .gap(crate::ui::sp(8.))
        .py(crate::ui::sp(32.))
        .px(crate::ui::sp(24.))
        .child(
            div()
                .size(px(40.))
                .rounded(crate::ui::rad(RADIUS))
                .bg(theme::fill_raised())
                .border_1()
                .border_color(theme::border())
                .flex()
                .items_center()
                .justify_center()
                .child(icon(icon_name, 18.).text_color(theme::text_subtle())),
        )
        .child(heading(title.to_string()).mt(px(4.)))
        .child(
            text(Type::Body, theme::text_subtle(), body.to_string())
                .max_w(px(320.))
                .text_center(),
        )
        .children(action.map(|a| div().mt(px(6.)).child(a)))
}

/// A place waiting on its data: a spinner and what it is reading.
pub fn loading(label: impl Into<SharedString>) -> gpui::Div {
    div()
        .flex()
        .flex_col()
        .items_center()
        .gap(crate::ui::sp(10.))
        .py(crate::ui::sp(32.))
        .child(super::spinner(16., theme::text_faint()))
        .child(text(Type::Body, theme::text_subtle(), label))
}

/// The refresh corner of a page header: "updated 2m ago" and a refresh button,
/// or a spinner in the button's place while it reads.
pub fn refresh_control(
    id: impl Into<ElementId>,
    busy: bool,
    updated: Option<String>,
    on_refresh: impl Fn(&gpui::ClickEvent, &mut gpui::Window, &mut gpui::App) + 'static,
) -> AnyElement {
    let id: ElementId = id.into();
    div()
        .flex()
        .flex_none()
        .items_center()
        .gap(crate::ui::sp(6.))
        .text_size(px(super::TEXT_XS))
        .text_color(theme::text_faint())
        .when_some(updated.filter(|_| !busy), |d, u| {
            d.child(format!("updated {u}"))
        })
        .child(if busy {
            div()
                .flex()
                .flex_none()
                .items_center()
                .justify_center()
                .size(px(super::ICON_BUTTON))
                .child(super::spinner(13., theme::mix(0.5)))
                .into_any_element()
        } else {
            super::icon_button(id, "refresh01", 13., "Refresh")
                .on_click(on_refresh)
                .into_any_element()
        })
        .into_any_element()
}

// ---------------------------------------------------------------------------
// Work surfaces: a card of its own inside a view, the darker panel nested in
// it, and the small pieces their rows and headers share (the GitHub tab).

/// A card inside a view: a step above the page, with a soft edge.
pub fn surface_card() -> gpui::Div {
    div()
        .flex()
        .flex_col()
        .min_h_0()
        .rounded(crate::ui::rad(super::RADIUS_XL))
        .border_1()
        .border_color(theme::border_subtle())
        .bg(theme::fill_raised())
        .overflow_hidden()
}

/// A darker panel nested in a `surface_card`, 10 px in from its edge: a
/// list or code sits in it. Its corner stays parallel to the card's.
pub fn inset_panel() -> gpui::Div {
    div()
        .flex()
        .flex_col()
        .min_h_0()
        .rounded(crate::ui::nested(super::RADIUS_XL, 10. + 1.))
        .border_1()
        .border_color(theme::border_subtle())
        .bg(theme::background())
        .overflow_hidden()
}

/// A round icon chip: neutral in a card's header (`color` None), or tinted in
/// a status color in a row.
pub fn icon_chip(name: &str, size: f32, color: Option<Hsla>) -> gpui::Div {
    let chip = div()
        .flex()
        .flex_none()
        .items_center()
        .justify_center()
        .size(px(size))
        .rounded_full();
    match color {
        Some(c) => chip
            .bg(c.opacity(0.13))
            .child(icon(name, size * 0.52).text_color(c)),
        None => chip
            .bg(theme::fill_selected())
            .border_1()
            .border_color(theme::border())
            .child(icon(name, size * 0.5).text_color(theme::text_muted())),
    }
}

/// A rounded square holding an icon in a status color: what a page is about.
pub fn icon_tile(name: &str, size: f32, color: Hsla) -> gpui::Div {
    div()
        .flex()
        .flex_none()
        .items_center()
        .justify_center()
        .size(px(size))
        .rounded(crate::ui::rad(if size > 32. { RADIUS_LG } else { RADIUS }))
        .bg(color.opacity(0.1))
        .border_1()
        .border_color(color.opacity(0.22))
        .child(icon(name, size * 0.5).text_color(color))
}

/// A count or state on a tinted pill: "4 waiting on you".
pub fn pill(content: impl Into<SharedString>, color: impl Into<Hsla>) -> gpui::Div {
    let c: Hsla = color.into();
    div()
        .flex()
        .flex_none()
        .items_center()
        .h(px(22.))
        .px(crate::ui::sp(10.))
        .rounded_full()
        .bg(c.opacity(0.12))
        .ty(Type::Label)
        .text_color(c)
        .child(content.into())
}

/// A verdict or state with its icon on a tinted pill: `✓ Approved`.
pub fn state_pill(
    icon_name: &str,
    content: impl Into<SharedString>,
    color: impl Into<Hsla>,
) -> gpui::Div {
    let c: Hsla = color.into();
    div()
        .flex()
        .flex_none()
        .items_center()
        .gap(crate::ui::sp(4.))
        .h(px(20.))
        .pl(crate::ui::sp(6.))
        .pr(crate::ui::sp(8.))
        .rounded_full()
        .bg(c.opacity(0.13))
        .ty(Type::Caption)
        .font_weight(FontWeight::MEDIUM)
        .text_color(c)
        .child(icon(icon_name, 12.).text_color(c))
        .child(content.into())
}

/// How a `key_pill` stands out.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PillKind {
    /// Outlined, on a raised fill.
    Normal,
    /// White: the one action the place is for.
    Primary,
    /// Outlined, its label quieter: Discard, Cancel.
    Quiet,
}

/// A rounded button with an optional icon before its label and the key that
/// triggers it after: `Comment [C]`, `Approve [A]`.
pub fn key_pill(
    id: impl Into<ElementId>,
    leading: Option<(&str, Option<Hsla>)>,
    label: impl Into<SharedString>,
    key: Option<&str>,
    kind: PillKind,
    enabled: bool,
) -> gpui::Stateful<gpui::Div> {
    let primary = kind == PillKind::Primary;
    let fg = match kind {
        PillKind::Primary => theme::background(),
        PillKind::Normal => theme::text(),
        PillKind::Quiet => theme::text_muted(),
    };
    let base = div()
        .id(id)
        .flex()
        .flex_none()
        .items_center()
        .gap(crate::ui::sp(7.))
        .h(px(30.))
        .px(crate::ui::sp(if primary { 14. } else { 12. }))
        .rounded_full()
        .ty(Type::Label)
        .text_color(fg)
        .when(enabled, |d| d.cursor_pointer())
        .when(!enabled, |d| d.opacity(0.45))
        .when_some(leading, |d, (name, color)| {
            d.child(icon(name, 14.).text_color(color.unwrap_or(fg)))
        })
        .child(label.into())
        .when_some(key, |d, k| {
            d.child(
                div()
                    .flex()
                    .flex_none()
                    .items_center()
                    .h(px(18.))
                    .px(crate::ui::sp(5.))
                    .rounded(crate::ui::rad(RADIUS_SM))
                    .bg(if primary {
                        theme::background().opacity(0.12)
                    } else {
                        theme::fill_selected()
                    })
                    .ty(Type::Caption)
                    .font_family(theme::FONT_MONO)
                    .text_color(if primary {
                        theme::background().opacity(0.7)
                    } else {
                        theme::text_subtle()
                    })
                    .child(k.to_string()),
            )
        });
    if primary {
        base.bg(theme::content())
            .when(enabled, |d| d.hover(|s| s.bg(theme::mix(0.8))))
    } else {
        base.bg(theme::fill_raised())
            .border_1()
            .border_color(theme::border())
            .when(enabled, |d| {
                d.hover(|s| s.bg(theme::fill_hover()).text_color(theme::text()))
            })
    }
}

/// A shortcut hint in a bar: `Next [J]`.
pub fn key_hint(label: impl Into<SharedString>, key: &str) -> gpui::Div {
    div()
        .flex()
        .flex_none()
        .items_center()
        .gap(crate::ui::sp(5.))
        .ty(Type::Label)
        .font_weight(FontWeight::NORMAL)
        .text_color(theme::text_muted())
        .child(label.into())
        .child(
            div()
                .flex()
                .items_center()
                .h(px(18.))
                .px(crate::ui::sp(5.))
                .rounded(crate::ui::rad(RADIUS_SM))
                .bg(theme::fill_selected())
                .ty(Type::Caption)
                .font_family(theme::FONT_MONO)
                .text_color(theme::text_subtle())
                .child(key.to_string()),
        )
}

/// One tab of a row of outlined tabs, with an optional count after its name:
/// `Files 8`, `Checks 5/6`.
pub fn outline_tab(
    id: impl Into<ElementId>,
    label: impl Into<SharedString>,
    count: Option<String>,
    on: bool,
) -> gpui::Stateful<gpui::Div> {
    let label = label.into();
    div()
        .id(id)
        .role(Role::Tab)
        .aria_label(label.clone())
        .aria_selected(on)
        .flex()
        .flex_none()
        .items_center()
        .gap(crate::ui::sp(7.))
        .h(px(28.))
        .px(crate::ui::sp(11.))
        .rounded(crate::ui::rad(RADIUS))
        .border_1()
        .ty(Type::Label)
        .cursor_pointer()
        .map(crate::ui::focusable)
        .when(on, |d| {
            d.bg(theme::fill_selected())
                .border_color(theme::border_strong())
                .text_color(theme::text())
        })
        .when(!on, |d| {
            d.border_color(theme::border_subtle())
                .text_color(theme::text_muted())
                .hover(|s| s.bg(theme::fill_hover()).text_color(theme::text()))
        })
        .child(label)
        .when_some(count, |d, c| {
            d.child(
                div()
                    .ty(Type::Caption)
                    .font_family(theme::FONT_MONO)
                    .text_color(theme::text_subtle())
                    .child(c),
            )
        })
}

/// A bar split in parts as wide as their share: (amount, color).
pub fn segment_bar(parts: &[(f32, Hsla)], height: f32) -> gpui::Div {
    let total: f32 = parts.iter().map(|p| p.0).sum();
    let mut bar = div().flex().w_full().gap(crate::ui::sp(3.)).h(px(height));
    if total <= 0. {
        return bar.child(
            div()
                .flex_1()
                .h_full()
                .rounded_full()
                .bg(theme::fill_selected()),
        );
    }
    for (amount, color) in parts.iter().filter(|p| p.0 > 0.) {
        bar = bar.child(
            div()
                .h_full()
                .rounded_full()
                .bg(*color)
                .flex_grow(*amount / total)
                .flex_basis(px(0.)),
        );
    }
    bar
}

/// A row of small squares, one per item, in each item's color: checks at a glance.
pub fn mini_squares(colors: &[Hsla], size: f32) -> gpui::Div {
    div()
        .flex()
        .flex_none()
        .items_center()
        .gap(crate::ui::sp(2.))
        .children(colors.iter().map(|c| {
            div()
                .size(px(size))
                .rounded(crate::ui::rad(size * 0.28))
                .bg(*c)
        }))
}

/// A person as a colored circle with their initial; the color comes from the
/// login, so one person is always the same color.
pub fn avatar(login: &str, size: f32) -> gpui::Div {
    let hash = login
        .to_ascii_lowercase()
        .bytes()
        .fold(5381u32, |h, b| h.wrapping_mul(33) ^ u32::from(b));
    let color: Hsla = gpui::rgb(theme::CATEGORY[hash as usize % theme::CATEGORY.len()]).into();
    let initial = login
        .chars()
        .find(|c| c.is_alphanumeric())
        .map(|c| c.to_ascii_uppercase())
        .unwrap_or('?');
    div()
        .flex()
        .flex_none()
        .items_center()
        .justify_center()
        .size(px(size))
        .rounded_full()
        .bg(color)
        .ty(Type::Caption)
        .font_weight(FontWeight::SEMIBOLD)
        .text_color(theme::on_accent())
        .child(initial.to_string())
}

/// A person's GitHub picture, clipped round, when it is on disk; their
/// `avatar` initials while it downloads or when it can't be read.
pub fn avatar_image(login: &str, picture: Option<&std::path::Path>, size: f32) -> gpui::Div {
    let Some(path) = picture else {
        return avatar(login, size);
    };
    let initials = login.to_string();
    div()
        .flex_none()
        .size(px(size))
        .rounded_full()
        .overflow_hidden()
        .bg(theme::fill_selected())
        .child(
            gpui::img(path.to_path_buf())
                .size_full()
                .rounded_full()
                .object_fit(gpui::ObjectFit::Cover)
                .with_fallback(move || avatar(&initials, size).into_any_element()),
        )
}

/// An avatar with a small round badge on its lower right corner: what kind
/// of thing a row is (a pull request, an issue), in its state color. The ring
/// takes the color of what is behind it, `ground`.
pub fn avatar_badge(
    login: &str,
    picture: Option<&std::path::Path>,
    size: f32,
    badge_icon: &str,
    color: Hsla,
    ground: Hsla,
) -> gpui::Div {
    let b = (size * 0.56).round();
    div()
        .relative()
        .flex_none()
        .size(px(size))
        .child(avatar_image(login, picture, size))
        .child(
            div()
                .absolute()
                .right(px(-b * 0.22))
                .bottom(px(-b * 0.22))
                .flex()
                .items_center()
                .justify_center()
                .size(px(b))
                .rounded_full()
                .border_2()
                .border_color(ground)
                .bg(ground)
                .child(
                    div()
                        .flex()
                        .items_center()
                        .justify_center()
                        .size_full()
                        .rounded_full()
                        .bg(color.opacity(0.22))
                        .child(icon(badge_icon, b * 0.62).text_color(color)),
                ),
        )
}

/// Overlapping avatars, the first on top, each ringed in `ground` so the
/// circles read apart: who took part.
pub fn avatar_stack(
    people: &[(String, Option<std::path::PathBuf>)],
    size: f32,
    ground: Hsla,
) -> gpui::Div {
    let step = (size * 0.7).round();
    let n = people.len();
    // Placed by hand: negative margins don't give the row its width.
    div()
        .relative()
        .flex_none()
        .h(px(size))
        .w(px(size + n.saturating_sub(1) as f32 * step))
        .children(people.iter().enumerate().rev().map(|(i, (login, pic))| {
            avatar_image(login, pic.as_deref(), size)
                .absolute()
                .top_0()
                .left(px(i as f32 * step))
                .border_2()
                .border_color(ground)
        }))
}

// ---------------------------------------------------------------------------
// Command palette (Ctrl+K). One focus at a time: the selected row has a
// lighter gradient, a 3 px accent bar at the left edge and a white title; no
// borders on rows, tabs or keys; key caps only on the selected row and in the
// footer. The accent is kept for what needs the person; matches are white and
// underlined. Sizes are the palette's own (17 px query, 13.5 px rows).

pub const PALETTE_W: f32 = 1000.;
pub const PALETTE_PREVIEW_W: f32 = 360.;
pub const PALETTE_BODY_H: f32 = 458.;
pub const PALETTE_ROW_H: f32 = 38.;
/// Room the list keeps on its left, where the selection bar sits.
pub const PALETTE_LIST_PAD: f32 = 8.;
pub const PALETTE_QUERY_TEXT: f32 = 17.;
const P_ROW: f32 = 13.5;
const P_SMALL: f32 = 12.5;
const P_META: f32 = 11.5;
const P_KEY: f32 = 10.5;

/// The few colors a palette icon is tinted in, by what kind of thing it is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tint {
    Green,
    Blue,
    Amber,
    Violet,
    Teal,
    Red,
    Neutral,
}

impl Tint {
    pub fn color(self) -> Hsla {
        match self {
            Tint::Green => theme::success().into(),
            Tint::Blue => theme::link().into(),
            Tint::Amber => theme::accent(),
            Tint::Violet => gpui::rgb(theme::CATEGORY[3]).into(),
            Tint::Teal => gpui::rgb(theme::CATEGORY[5]).into(),
            Tint::Red => theme::error().into(),
            Tint::Neutral => theme::text_subtle(),
        }
    }
}

/// The palette's panel: wide, rounded 16, a step lighter than dialogs' edge.
pub fn palette_panel(id: impl Into<ElementId>) -> gpui::Stateful<gpui::Div> {
    div()
        .id(id)
        .occlude()
        .relative()
        .flex()
        .flex_col()
        .w(px(PALETTE_W))
        .max_w_full()
        .max_h_full()
        .rounded(crate::ui::rad(super::RADIUS_XL))
        .border_1()
        .border_color(theme::border_subtle())
        .bg(theme::dialog())
        .shadow_2xl()
        .overflow_hidden()
}

/// The query line: icon, what leads the text (scope, chips, crumbs), the input.
pub fn palette_query() -> gpui::Div {
    div()
        .flex()
        .flex_none()
        .items_center()
        .gap(crate::ui::sp(12.))
        .pt(crate::ui::sp(19.))
        .pb(crate::ui::sp(15.))
        .px(crate::ui::sp(22.))
        .text_size(px(PALETTE_QUERY_TEXT))
        .line_height(px(24.))
        .text_color(theme::text())
}

/// The scope a prefix set (`>`, `/`, `:`, `@`, `#`).
pub fn palette_scope_glyph(glyph: &'static str) -> gpui::Div {
    div()
        .flex_none()
        .text_color(theme::active())
        .font_weight(FontWeight::MEDIUM)
        .child(glyph)
}

/// A word that became a chip before the query: `@Opus 5.5`, a branch.
pub fn token_chip(icon_name: &str, label: impl Into<SharedString>, tint: Tint) -> gpui::Div {
    let c = tint.color();
    div()
        .flex()
        .flex_none()
        .items_center()
        .gap(crate::ui::sp(6.))
        .h(px(26.))
        .px(crate::ui::sp(9.))
        .rounded(crate::ui::rad(7.))
        .bg(c.opacity(0.11))
        .border_1()
        .border_color(c.opacity(0.2))
        .text_size(px(P_ROW))
        .text_color(c)
        .child(icon(icon_name, 13.).text_color(c))
        .child(label.into())
}

/// A step of a drill-down path in the query line: `app / Branches`.
pub fn crumb(icon_name: Option<&str>, label: impl Into<SharedString>) -> gpui::Div {
    div()
        .flex()
        .flex_none()
        .items_center()
        .gap(crate::ui::sp(6.))
        .h(px(26.))
        .px(crate::ui::sp(9.))
        .rounded(crate::ui::rad(7.))
        .bg(theme::fill_hover())
        .text_size(px(P_ROW))
        .text_color(theme::text_muted())
        .when_some(icon_name, |d, n| {
            d.child(icon(n, 13.).text_color(theme::text_subtle()))
        })
        .child(label.into())
}

pub fn crumb_separator() -> gpui::Div {
    div().flex_none().text_color(theme::text_faint()).child("/")
}

/// The row of filter tabs under the query.
pub fn palette_tabs() -> gpui::Div {
    div()
        .flex()
        .flex_none()
        .items_center()
        .gap(crate::ui::sp(2.))
        .px(crate::ui::sp(16.))
        .pb(crate::ui::sp(12.))
        .border_b_1()
        .border_color(theme::border_subtle())
}

/// One filter tab: borderless, filled when on, with its count after it.
pub fn palette_tab(
    id: impl Into<ElementId>,
    icon_name: Option<&str>,
    label: impl Into<SharedString>,
    count: Option<usize>,
    on: bool,
) -> gpui::Stateful<gpui::Div> {
    let color = if on {
        theme::text()
    } else {
        theme::text_subtle()
    };
    let label = label.into();
    div()
        .id(id)
        .role(Role::Tab)
        .aria_label(label.clone())
        .aria_selected(on)
        .flex()
        .flex_none()
        .items_center()
        .gap(crate::ui::sp(6.))
        .h(px(28.))
        .px(crate::ui::sp(11.))
        .rounded(crate::ui::rad(RADIUS))
        .text_size(px(P_SMALL))
        .text_color(color)
        .cursor_pointer()
        .when(on, |d| d.bg(theme::fill_hover()))
        .when(!on, |d| d.hover(|s| s.text_color(theme::text_muted())))
        .when_some(icon_name, |d, n| d.child(icon(n, 14.).text_color(color)))
        .child(label)
        .when_some(count.filter(|n| *n > 0), |d, n| {
            d.child(
                div()
                    .text_size(px(P_META))
                    .text_color(theme::text_faint())
                    .child(n.to_string()),
            )
        })
}

/// A group's label over its rows, quiet, with its count at the right.
pub fn palette_section(
    label: impl Into<SharedString>,
    count: Option<String>,
    first: bool,
) -> gpui::Div {
    div()
        .flex()
        .flex_none()
        .items_center()
        .justify_between()
        .pt(crate::ui::sp(if first { 6. } else { 14. }))
        .pb(crate::ui::sp(5.))
        .px(crate::ui::sp(12.))
        .text_size(px(P_META))
        .line_height(px(16.))
        .font_weight(FontWeight::MEDIUM)
        .text_color(theme::text_faint())
        .child(label.into())
        .when_some(count, |d, n| d.child(n))
}

/// A row of results. The caller adds its lead, title, sub and end.
pub fn palette_row(id: impl Into<ElementId>, selected: bool) -> gpui::Stateful<gpui::Div> {
    div()
        .id(id)
        .relative()
        .flex()
        .flex_none()
        .items_center()
        .gap(crate::ui::sp(11.))
        .h(px(PALETTE_ROW_H))
        .px(crate::ui::sp(12.))
        .rounded(crate::ui::rad(10.))
        .text_size(px(P_ROW))
        .text_color(theme::text_muted())
        .cursor_pointer()
        .when(selected, |d| {
            d.bg(gpui::linear_gradient(
                90.,
                gpui::linear_color_stop(theme::fill_hover(), 0.),
                gpui::linear_color_stop(theme::fill_raised(), 1.),
            ))
        })
        .when(!selected, |d| d.hover(|s| s.bg(theme::fill_raised())))
}

/// The 3 px accent bar at the list's left edge beside the selected row.
pub fn selection_bar() -> gpui::Div {
    div()
        .absolute()
        .left(px(-PALETTE_LIST_PAD))
        .top(px(9.))
        .bottom(px(9.))
        .w(px(3.))
        .rounded_r(crate::ui::rad(3.))
        .bg(theme::active())
}

/// How a match shows inside a title: the text color, semibold, underlined faintly.
pub fn match_highlight() -> gpui::HighlightStyle {
    gpui::HighlightStyle {
        color: Some(theme::text()),
        font_weight: Some(FontWeight::SEMIBOLD),
        underline: Some(gpui::UnderlineStyle {
            thickness: px(1.),
            color: Some(theme::text().opacity(0.22)),
            wavy: false,
        }),
        ..Default::default()
    }
}

/// A row's title, its matches highlighted; full text color when selected.
pub fn row_title(
    text: impl Into<SharedString>,
    matches: &[std::ops::Range<usize>],
    selected: bool,
    dim: bool,
) -> gpui::Div {
    let text: SharedString = text.into();
    let hl = match_highlight();
    let runs: Vec<(std::ops::Range<usize>, gpui::HighlightStyle)> =
        matches.iter().map(|r| (r.clone(), hl)).collect();
    div()
        .flex_shrink_1()
        .min_w_0()
        .truncate()
        .text_color(if selected {
            theme::text()
        } else if dim {
            theme::text_subtle()
        } else {
            theme::text_muted()
        })
        .child(gpui::StyledText::new(text).with_highlights(runs))
}

/// What follows a title in a row, quieter: "writing tests", "app".
pub fn row_sub(text: impl Into<SharedString>, selected: bool) -> gpui::Div {
    div()
        .flex_shrink_1()
        .min_w_0()
        .truncate()
        .text_size(px(P_SMALL))
        .text_color(if selected {
            theme::text_subtle()
        } else {
            theme::text_faint()
        })
        .child(text.into())
}

/// The end of a row: time, line count, a shortcut's text.
pub fn row_end(selected: bool) -> gpui::Div {
    div()
        .flex()
        .flex_none()
        .items_center()
        .gap(crate::ui::sp(8.))
        .ml_auto()
        .pl(crate::ui::sp(10.))
        .text_color(if selected {
            theme::text_muted()
        } else {
            theme::text_faint()
        })
}

/// Small mono text at a row's end: `12m`, `L1496`, `1,204 lines`.
pub fn row_meta(text: impl Into<SharedString>) -> gpui::Div {
    div()
        .flex_none()
        .text_size(px(P_META))
        .font_family(theme::FONT_MONO)
        .child(text.into())
}

/// A key cap without a border: `↵`, `Ctrl`.
pub fn keycap(key: impl Into<SharedString>) -> gpui::Div {
    div()
        .flex()
        .flex_none()
        .items_center()
        .justify_center()
        .min_w(px(20.))
        .h(px(18.))
        .px(crate::ui::sp(6.))
        .rounded(crate::ui::rad(5.))
        .bg(theme::fill_hover())
        .shadow(vec![gpui::BoxShadow {
            color: theme::mix(0.07),
            offset: gpui::point(px(0.), px(-1.)),
            blur_radius: px(0.),
            spread_radius: px(0.),
            inset: true,
        }])
        .text_size(px(P_KEY))
        .line_height(px(14.))
        .font_family(theme::FONT_MONO)
        .text_color(theme::text_muted())
        .child(key.into())
}

/// Keys of one shortcut side by side.
pub fn keycaps(keys: &[&str]) -> gpui::Div {
    let mut row = div()
        .flex()
        .flex_none()
        .items_center()
        .gap(crate::ui::sp(4.));
    // Given the Windows and Linux way; macOS shows its symbols.
    for k in crate::platform::keycaps(keys) {
        row = row.child(keycap(k));
    }
    row
}

/// A tinted word at a row's end: `review`, `5 for you`.
pub fn palette_pill(text: impl Into<SharedString>, color: Hsla) -> gpui::Div {
    div()
        .flex()
        .flex_none()
        .items_center()
        .h(px(18.))
        .px(crate::ui::sp(8.))
        .rounded_full()
        .bg(color.opacity(0.1))
        .text_size(px(11.))
        .font_weight(FontWeight::MEDIUM)
        .text_color(color)
        .child(text.into())
}

/// A row's icon, tinted by kind.
pub fn lead_icon(name: &str, tint: Tint) -> gpui::Div {
    div()
        .flex()
        .flex_none()
        .items_center()
        .justify_center()
        .size(px(16.))
        .child(icon(name, 16.).text_color(tint.color()))
}

/// A working session: a ring with a turning arc in the working blue.
pub fn lead_spinner() -> AnyElement {
    let t = super::motion::loop_t(std::time::Duration::from_millis(1100));
    div()
        .flex()
        .flex_none()
        .items_center()
        .justify_center()
        .size(px(16.))
        .child(
            icon("loading03", 15.)
                .text_color(theme::working())
                .with_transformation(gpui::Transformation::rotate(gpui::percentage(t))),
        )
        .into_any_element()
}

/// An idle session: an empty ring.
pub fn lead_ring() -> gpui::Div {
    div()
        .flex()
        .flex_none()
        .items_center()
        .justify_center()
        .size(px(16.))
        .child(
            div()
                .size(px(14.))
                .rounded_full()
                .border(px(1.5))
                .border_color(theme::border_strong()),
        )
}

/// A dot in a state color, with a soft halo when it asks for attention.
pub fn lead_dot(color: Hsla, halo: bool) -> gpui::Div {
    div()
        .flex()
        .flex_none()
        .items_center()
        .justify_center()
        .size(px(16.))
        .child(div().size(px(8.)).rounded_full().bg(color).when(halo, |d| {
            d.shadow(vec![gpui::BoxShadow {
                color: color.opacity(0.14),
                offset: gpui::point(px(0.), px(0.)),
                blur_radius: px(0.),
                spread_radius: px(3.),
                inset: false,
            }])
        }))
}

/// A person's picture in a white ring, set off from the row by a gap.
pub fn ringed_avatar(login: &str, picture: Option<&std::path::Path>, size: f32) -> gpui::Div {
    div()
        .flex()
        .flex_none()
        .items_center()
        .justify_center()
        .size(px(size + 3.))
        .rounded_full()
        .border(px(1.5))
        .border_color(theme::mix(0.8))
        .child(avatar_image(login, picture, size - 2.))
}

/// A square check for multi-select.
pub fn check_box(on: bool) -> gpui::Div {
    div()
        .flex()
        .flex_none()
        .items_center()
        .justify_center()
        .size(px(16.))
        .rounded(crate::ui::rad(5.))
        .border(px(1.5))
        .when(on, |d| {
            d.bg(theme::active())
                .border_color(theme::active())
                .child(icon("tick02", 11.).text_color(theme::background()))
        })
        .when(!on, |d| d.border_color(theme::border_strong()))
}

/// A card at the top of the list (clipboard, ask an agent, an answer).
pub fn palette_card(
    id: impl Into<ElementId>,
    tint: Tint,
    selected: bool,
) -> gpui::Stateful<gpui::Div> {
    let c = tint.color();
    let strong = matches!(tint, Tint::Amber | Tint::Violet);
    div()
        .id(id)
        .relative()
        .flex()
        .flex_none()
        .items_center()
        .gap(crate::ui::sp(12.))
        .my(px(4.))
        .py(crate::ui::sp(12.))
        .px(crate::ui::sp(14.))
        .rounded(crate::ui::rad(RADIUS_LG))
        .cursor_pointer()
        .when(strong, |d| {
            d.bg(gpui::linear_gradient(
                100.,
                gpui::linear_color_stop(c.opacity(0.11), 0.),
                gpui::linear_color_stop(c.opacity(0.024), 0.6),
            ))
            .border_1()
            .border_color(c.opacity(if selected { 0.32 } else { 0.19 }))
        })
        .when(!strong && tint != Tint::Neutral, |d| {
            d.bg(c.opacity(if selected { 0.09 } else { 0.05 }))
        })
        .when(tint == Tint::Neutral, |d| {
            d.bg(if selected {
                theme::fill_hover()
            } else {
                theme::fill_raised()
            })
            .border_1()
            .border_color(theme::border_subtle())
        })
}

/// The tinted square holding a card's icon.
pub fn glow_tile(icon_name: &str, tint: Tint) -> gpui::Div {
    let c = tint.color();
    div()
        .flex()
        .flex_none()
        .items_center()
        .justify_center()
        .size(px(30.))
        .rounded(crate::ui::rad(9.))
        .bg(c.opacity(0.13))
        .child(icon(icon_name, 16.).text_color(c))
}

/// A card's two lines: what it does and what it knows.
pub fn card_text(title: impl IntoElement, sub: Option<AnyElement>) -> gpui::Div {
    div()
        .flex()
        .flex_col()
        .flex_1()
        .min_w_0()
        .child(
            div()
                .truncate()
                .text_size(px(P_ROW))
                .line_height(px(19.))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(theme::text())
                .child(title),
        )
        .when_some(sub, |d, s| {
            d.child(
                div()
                    .mt(px(1.))
                    .truncate()
                    .text_size(px(P_SMALL))
                    .line_height(px(18.))
                    .text_color(theme::text_subtle())
                    .child(s),
            )
        })
}

/// The preview pane at the right of the list. It scrolls: a pull request
/// with its people, checks and diff lines is taller than the palette. Give
/// each item its own `id`, so a new one starts at the top.
pub fn preview_pane(id: impl Into<ElementId>) -> gpui::Stateful<gpui::Div> {
    div()
        .id(id)
        .flex()
        .flex_col()
        .flex_none()
        .w(px(PALETTE_PREVIEW_W))
        .h_full()
        .min_h_0()
        .gap(crate::ui::sp(16.))
        .p(crate::ui::sp(22.))
        .border_l_1()
        .border_color(theme::border_subtle())
        .bg(gpui::linear_gradient(
            180.,
            gpui::linear_color_stop(theme::mix(0.016), 0.),
            gpui::linear_color_stop(theme::mix(0.), 1.),
        ))
        .overflow_y_scroll()
}

/// The small line over a preview's title: `ArakunHQ/arakun · #61`.
pub fn preview_eyebrow(icon_name: &str, text: impl Into<SharedString>) -> gpui::Div {
    div()
        .flex()
        .items_center()
        .gap(crate::ui::sp(6.))
        .text_size(px(P_META))
        .line_height(px(16.))
        .text_color(theme::text_faint())
        .child(icon(icon_name, 12.).text_color(theme::text_faint()))
        .child(div().truncate().child(text.into()))
}

pub fn preview_title(text: impl Into<SharedString>) -> gpui::Div {
    div()
        .text_size(px(PALETTE_QUERY_TEXT))
        .line_height(px(22.))
        .font_weight(FontWeight::SEMIBOLD)
        .text_color(theme::text())
        .child(text.into())
}

/// Running text in a preview.
pub fn preview_text(text: impl Into<SharedString>) -> gpui::Div {
    div()
        .text_size(px(P_ROW))
        .line_height(px(21.))
        .text_color(theme::text_muted())
        .child(text.into())
}

/// Label and value rows in a preview.
pub fn props() -> gpui::Div {
    div()
        .flex()
        .flex_col()
        .gap(crate::ui::sp(9.))
        .text_size(px(P_SMALL))
        .line_height(px(18.))
}

pub fn prop(label: impl Into<SharedString>, value: impl IntoElement) -> gpui::Div {
    div()
        .flex()
        .items_center()
        .justify_between()
        .gap(crate::ui::sp(10.))
        .child(
            div()
                .flex_none()
                .text_color(theme::text_faint())
                .child(label.into()),
        )
        .child(
            div()
                .flex()
                .items_center()
                .gap(crate::ui::sp(6.))
                .min_w_0()
                .truncate()
                .text_color(theme::text())
                .child(value),
        )
}

/// Mono text in props: `+140 −20 · 9 files`.
pub fn mono(text: impl Into<SharedString>, color: Hsla) -> gpui::Div {
    div()
        .flex_none()
        .text_size(px(P_META))
        .font_family(theme::FONT_MONO)
        .text_color(color)
        .child(text.into())
}

/// A darker well for code, logs and diffs.
pub fn code_well() -> gpui::Div {
    div()
        .flex()
        .flex_col()
        .flex_none()
        .py(crate::ui::sp(12.))
        .px(crate::ui::sp(14.))
        .rounded(crate::ui::rad(10.))
        .bg(theme::surface(0.045))
        .text_size(px(P_META))
        .line_height(px(19.5))
        .font_family(theme::FONT_MONO)
        .text_color(theme::text_subtle())
        .overflow_hidden()
}

/// One line of a `code_well`, numbered or not, in a color.
pub fn code_line(number: Option<u32>, text: impl Into<SharedString>, color: Hsla) -> gpui::Div {
    div()
        .flex()
        .gap(crate::ui::sp(10.))
        .min_w_0()
        .when_some(number, |d, n| {
            d.child(
                div()
                    .flex_none()
                    .w(px(30.))
                    .text_color(theme::text_faint())
                    .child(n.to_string()),
            )
        })
        .child(div().truncate().text_color(color).child(text.into()))
}

/// A quote of what an agent last said.
pub fn quote(text: impl Into<SharedString>) -> gpui::Div {
    div()
        .pl(crate::ui::sp(12.))
        .border_l_2()
        .border_color(theme::border())
        .text_size(px(13.))
        .line_height(px(20.))
        .text_color(theme::text_muted())
        .child(text.into())
}

/// Three numbers side by side: (value, label, color).
pub fn stat_grid(cells: &[(String, String, Option<Hsla>)]) -> gpui::Div {
    div()
        .flex()
        .w_full()
        .gap(crate::ui::sp(1.))
        .rounded(crate::ui::rad(11.))
        .bg(theme::border_subtle())
        .overflow_hidden()
        .children(cells.iter().map(|(value, label, color)| {
            div()
                .flex()
                .flex_col()
                .flex_1()
                .min_w_0()
                .py(crate::ui::sp(10.))
                .px(crate::ui::sp(12.))
                .bg(theme::surface(0.135))
                .child(
                    div()
                        .text_size(px(18.))
                        .line_height(px(22.))
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(color.unwrap_or(theme::text()))
                        .truncate()
                        .child(value.clone()),
                )
                .child(
                    div()
                        .text_size(px(P_META))
                        .line_height(px(16.))
                        .text_color(theme::text_faint())
                        .truncate()
                        .child(label.clone()),
                )
        }))
}

/// Check results as small green, red or amber bars.
pub fn check_bars(colors: &[Hsla]) -> gpui::Div {
    div()
        .flex()
        .flex_none()
        .items_center()
        .gap(crate::ui::sp(2.))
        .children(colors.iter().take(8).map(|c| {
            div()
                .w(px(8.))
                .h(px(12.))
                .rounded(crate::ui::rad(2.))
                .bg(*c)
        }))
}

/// The bar of hints along the palette's bottom.
pub fn palette_footer() -> gpui::Div {
    div()
        .flex()
        .flex_none()
        .items_center()
        .gap(crate::ui::sp(16.))
        .py(crate::ui::sp(11.))
        .px(crate::ui::sp(20.))
        .border_t_1()
        .border_color(theme::border_subtle())
        .text_size(px(12.))
        .line_height(px(18.))
        .text_color(theme::text_faint())
}

/// Keys and what they do: `↑ ↓ move`.
pub fn footer_hint(keys: &[&str], label: impl Into<SharedString>) -> gpui::Div {
    div()
        .flex()
        .flex_none()
        .items_center()
        .gap(crate::ui::sp(6.))
        .child(keycaps(keys))
        .child(label.into())
}

/// The floating bar for what is marked with Space.
pub fn bulk_bar() -> gpui::Div {
    div()
        .flex()
        .items_center()
        .gap(crate::ui::sp(6.))
        .py(crate::ui::sp(6.))
        .pr(crate::ui::sp(6.))
        .pl(crate::ui::sp(14.))
        .rounded(crate::ui::rad(RADIUS_LG))
        .bg(theme::surface(0.16))
        .border_1()
        .border_color(theme::border())
        .shadow_xl()
        .text_size(px(P_SMALL))
}

pub fn bulk_button(
    id: impl Into<ElementId>,
    icon_name: Option<&str>,
    label: impl Into<SharedString>,
    primary: bool,
) -> gpui::Stateful<gpui::Div> {
    let label = label.into();
    div()
        .id(id)
        .role(Role::Button)
        .aria_label(label.clone())
        .flex()
        .flex_none()
        .items_center()
        .gap(crate::ui::sp(6.))
        .h(px(30.))
        .px(crate::ui::sp(11.))
        .rounded(crate::ui::rad(RADIUS))
        .cursor_pointer()
        .when(primary, |d| {
            d.bg(theme::content())
                .text_color(theme::background())
                .font_weight(FontWeight::SEMIBOLD)
                .hover(|s| s.bg(theme::mix(0.8)))
        })
        .when(!primary, |d| {
            d.text_color(theme::text_muted())
                .hover(|s| s.bg(theme::fill_hover()).text_color(theme::text()))
        })
        .when_some(icon_name, |d, n| {
            d.child(icon(n, 13.).text_color(if primary {
                theme::background()
            } else {
                theme::text_subtle()
            }))
        })
        .child(label)
}

/// A pinned thing as a chip in the palette's Pinned row, with its number.
pub fn pin_chip(
    id: impl Into<ElementId>,
    icon_name: &str,
    tint: Tint,
    label: impl Into<SharedString>,
    number: usize,
    selected: bool,
) -> gpui::Stateful<gpui::Div> {
    let label = label.into();
    div()
        .id(id)
        .role(Role::Button)
        .aria_label(label.clone())
        // Only the highlighted one says so; the rest would all be "not
        // selected", which is noise.
        .when(selected, |d| d.aria_selected(true))
        .flex()
        .flex_none()
        .items_center()
        .gap(crate::ui::sp(7.))
        .h(px(32.))
        .px(crate::ui::sp(11.))
        .rounded(crate::ui::rad(9.))
        .bg(if selected {
            theme::fill_selected()
        } else {
            theme::fill_raised()
        })
        .text_size(px(P_SMALL))
        .text_color(if selected {
            theme::text()
        } else {
            theme::text_muted()
        })
        .cursor_pointer()
        .hover(|s| s.bg(theme::fill_hover()))
        .max_w(px(260.))
        .child(icon(icon_name, 14.).text_color(tint.color()))
        .child(div().min_w_0().truncate().child(label))
        .child(
            div()
                .flex_none()
                .ml(px(4.))
                .text_size(px(P_KEY))
                .font_family(theme::FONT_MONO)
                .text_color(theme::text_faint())
                .child(number.to_string()),
        )
}

/// The WHEN / DO / THEN box of an automation draft.
pub fn rule_well() -> gpui::Div {
    div()
        .flex()
        .flex_col()
        .gap(crate::ui::sp(8.))
        .mt(px(12.))
        .p(crate::ui::sp(12.))
        .rounded(crate::ui::rad(10.))
        .bg(theme::surface(0.045))
        .text_size(px(P_SMALL))
}

pub fn rule_line(word: &'static str, icon_name: &str, text: impl Into<SharedString>) -> gpui::Div {
    div()
        .flex()
        .items_center()
        .gap(crate::ui::sp(10.))
        .child(
            div()
                .flex_none()
                .w(px(44.))
                .text_size(px(11.))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(Tint::Violet.color())
                .child(word),
        )
        .child(
            div()
                .flex()
                .items_center()
                .gap(crate::ui::sp(5.))
                .h(px(24.))
                .px(crate::ui::sp(9.))
                .rounded(crate::ui::rad(7.))
                .bg(theme::fill_hover())
                .text_color(theme::text())
                .child(icon(icon_name, 13.).text_color(theme::text_muted()))
                .child(text.into()),
        )
}

/// A source an answer read: `CI #4812`, `memory #325`.
pub fn source_chip(icon_name: &str, label: impl Into<SharedString>) -> gpui::Div {
    div()
        .flex()
        .flex_none()
        .items_center()
        .gap(crate::ui::sp(5.))
        .h(px(22.))
        .px(crate::ui::sp(8.))
        .rounded(crate::ui::rad(6.))
        .bg(theme::fill_raised())
        .text_size(px(P_META))
        .text_color(theme::text_subtle())
        .child(icon(icon_name, 12.).text_color(theme::text_faint()))
        .child(label.into())
}

/// Text of a quick answer or memory card: 13.5 px, loose lines.
pub fn answer_text() -> gpui::Div {
    div()
        .text_size(px(P_ROW))
        .line_height(px(21.5))
        .text_color(theme::text_muted())
}

/// The small header line of an answer card: `Quick answer · Haiku 5.5`.
pub fn answer_header() -> gpui::Div {
    div()
        .flex()
        .items_center()
        .gap(crate::ui::sp(8.))
        .mb(px(10.))
        .text_size(px(12.))
        .text_color(theme::text_faint())
}

/// A quiet line in the palette: no key yet, nothing matches.
pub fn palette_note(text: impl Into<SharedString>) -> gpui::Div {
    div()
        .py(crate::ui::sp(10.))
        .px(crate::ui::sp(12.))
        .text_size(px(P_SMALL))
        .line_height(px(18.))
        .text_color(theme::text_faint())
        .child(text.into())
}

// ---------------------------------------------------------------------------
// Slider

/// A drag on a slider: which slider, and where its track was when the button
/// went down.
type SliderDrag = (ElementId, gpui::Bounds<gpui::Pixels>);

thread_local! {
    /// The slider the left button went down on. gpui sends a move with the
    /// button held to whatever slider is under the pointer, so without this a
    /// drag that starts on one slider would change the next one it crosses.
    static SLIDER_DRAG: std::cell::RefCell<Option<SliderDrag>> = const { std::cell::RefCell::new(None) };
}

/// The track a move with the left button `held` drags on, if slider `id` owns
/// the drag. A move without the button means the release happened where no
/// slider saw it, so the drag is over.
fn slider_drags(id: &ElementId, held: bool) -> Option<gpui::Bounds<gpui::Pixels>> {
    SLIDER_DRAG.with_borrow_mut(|d| {
        if !held {
            *d = None;
        }
        d.as_ref().filter(|(at, _)| at == id).map(|(_, b)| *b)
    })
}

/// The track slider `id` is being dragged on, without ending the drag.
fn slider_drag_start(id: &ElementId) -> Option<gpui::Bounds<gpui::Pixels>> {
    SLIDER_DRAG.with_borrow(|d| d.as_ref().filter(|(at, _)| at == id).map(|(_, b)| *b))
}

/// The scroll offset that brings a slider drawn at `now` back to `start`, the
/// place its drag began, as far as `max` (the furthest the pane scrolls) allows.
/// `None` when it is already there or the pane can't move.
fn pin_offset(
    start: gpui::Pixels,
    now: gpui::Pixels,
    offset: gpui::Pixels,
    max: gpui::Pixels,
) -> Option<gpui::Pixels> {
    let y = (offset - (now - start)).clamp(-max.max(px(0.)), px(0.));
    ((y - offset).abs() >= px(0.5)).then_some(y)
}

/// A value from 0 to 1 picked on a track: click or drag. `track` fills the
/// track (a gradient shows what the value means, as a hue rainbow does); the
/// knob is filled with `knob`. `on_change` gets the new
/// value while the pointer moves.
///
/// A drag keeps going wherever the pointer goes until the button is up, and
/// reads the pointer against the track as it was when the drag began. A
/// slider such as Spacing moves its own track as it changes: with the live
/// track the drag stopped when the slider slid out from under the pointer,
/// and the value chased the moving track.
pub fn slider(
    id: impl Into<ElementId>,
    value: f32,
    track: impl IntoElement,
    knob: Hsla,
    on_change: impl Fn(f32, &mut gpui::Window, &mut gpui::App) + 'static,
) -> gpui::Stateful<gpui::Div> {
    slider_with(id, value, track, knob, None, on_change)
}

/// A [`slider`] in the pane `scroll` scrolls, for a value that changes the
/// layout above it. While it is dragged the pane scrolls by as much as the
/// slider moved, so it stays under the pointer.
pub fn pinned_slider(
    id: impl Into<ElementId>,
    value: f32,
    track: impl IntoElement,
    knob: Hsla,
    scroll: &gpui::ScrollHandle,
    on_change: impl Fn(f32, &mut gpui::Window, &mut gpui::App) + 'static,
) -> gpui::Stateful<gpui::Div> {
    slider_with(id, value, track, knob, Some(scroll.clone()), on_change)
}

fn slider_with(
    id: impl Into<ElementId>,
    value: f32,
    track: impl IntoElement,
    knob: Hsla,
    pin: Option<gpui::ScrollHandle>,
    on_change: impl Fn(f32, &mut gpui::Window, &mut gpui::App) + 'static,
) -> gpui::Stateful<gpui::Div> {
    const H: f32 = 20.;
    const KNOB: f32 = 18.;
    let bounds = std::rc::Rc::new(std::cell::Cell::new(gpui::Bounds::<gpui::Pixels>::default()));
    let on_change = std::rc::Rc::new(on_change);
    let at = |b: gpui::Bounds<gpui::Pixels>, x: gpui::Pixels| -> f32 {
        let w = (b.size.width - px(KNOB)).as_f32().max(1.);
        ((x - b.origin.x - px(KNOB / 2.)).as_f32() / w).clamp(0., 1.)
    };
    let seen = bounds.clone();
    let down_b = bounds;
    let (down_f, move_f) = (on_change.clone(), on_change);
    let id = id.into();
    let (down_id, move_id, pin_id) = (id.clone(), id.clone(), id.clone());
    div()
        .id(id)
        .relative()
        .h(px(H))
        .w_full()
        .cursor_pointer()
        .child(
            gpui::canvas(
                move |b, window, _| {
                    seen.set(b);
                    if let Some(scroll) = &pin
                        && let Some(start) = slider_drag_start(&pin_id)
                    {
                        let (offset, max) = (scroll.offset(), scroll.max_offset());
                        if let Some(y) = pin_offset(start.origin.y, b.origin.y, offset.y, max.y) {
                            scroll.set_offset(gpui::point(offset.x, y));
                            // The pane already took its offset for this frame, and
                            // `refresh` does nothing while drawing: draw once more
                            // after it, or the slider stays off when the pointer stops.
                            window.on_next_frame(|window, _| window.refresh());
                        }
                    }
                },
                // A window-wide listener, not `on_mouse_move`: that one only
                // hears moves while the pointer is over the slider.
                move |_, _, window, _| {
                    window.on_mouse_event(move |ev: &gpui::MouseMoveEvent, phase, window, cx| {
                        if phase != gpui::DispatchPhase::Capture {
                            return;
                        }
                        let held = ev.pressed_button == Some(gpui::MouseButton::Left);
                        if let Some(b) = slider_drags(&move_id, held) {
                            move_f(at(b, ev.position.x), window, cx);
                        }
                    });
                },
            )
            .absolute()
            .size_full(),
        )
        .child(
            div()
                .absolute()
                .left(px(KNOB / 2. - 4.))
                .right(px(KNOB / 2. - 4.))
                .top(px((H - 8.) / 2.))
                .h(px(8.))
                .rounded_full()
                .overflow_hidden()
                .flex()
                // A dark gradient on a dark panel needs an edge to read as a track.
                .border_1()
                .border_color(theme::border())
                .child(track),
        )
        .child(
            div()
                .absolute()
                .top(px((H - KNOB) / 2.))
                // At 0 the knob's left edge meets the track's start, at 1 its
                // right edge meets the end.
                .left(relative(value.clamp(0., 1.)))
                .ml(px(-KNOB * value.clamp(0., 1.)))
                .size(px(KNOB))
                .rounded_full()
                .border_2()
                .border_color(theme::text())
                .bg(knob)
                .shadow_sm(),
        )
        .on_mouse_down(gpui::MouseButton::Left, move |ev, window, cx| {
            cx.stop_propagation();
            let b = down_b.get();
            SLIDER_DRAG.set(Some((down_id.clone(), b)));
            down_f(at(b, ev.position.x), window, cx);
        })
        .on_mouse_up(gpui::MouseButton::Left, |_, _, _| SLIDER_DRAG.set(None))
        .on_mouse_up_out(gpui::MouseButton::Left, |_, _, _| SLIDER_DRAG.set(None))
}

/// A slider track that runs through every hue, for picking a tint.
pub fn hue_track(saturation: f32, lightness: f32) -> gpui::Div {
    div().size_full().flex().children((0..12).map(move |i| {
        let at = |k: f32| gpui::hsla(k / 12., saturation, lightness, 1.);
        div().flex_1().h_full().bg(gpui::linear_gradient(
            90.,
            gpui::linear_color_stop(at(i as f32), 0.),
            gpui::linear_color_stop(at(i as f32 + 1.), 1.),
        ))
    }))
}

/// A slider track that blends from one color to another.
pub fn blend_track(from: Hsla, to: Hsla) -> gpui::Div {
    div().size_full().bg(gpui::linear_gradient(
        90.,
        gpui::linear_color_stop(from, 0.),
        gpui::linear_color_stop(to, 1.),
    ))
}

/// A round color to pick, ringed when it is the one chosen.
pub fn swatch(
    id: impl Into<ElementId>,
    color: impl Into<gpui::Fill>,
    chosen: bool,
) -> gpui::Stateful<gpui::Div> {
    div()
        .id(id)
        .flex_none()
        .size(px(26.))
        .rounded_full()
        .cursor_pointer()
        .bg(color)
        .border_2()
        .border_color(if chosen {
            theme::content()
        } else {
            theme::border()
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_drag_changes_only_the_slider_it_started_on() {
        let (tint, bg): (ElementId, ElementId) = ("tint".into(), "bg".into());
        let track = gpui::Bounds::new(gpui::point(px(10.), px(40.)), gpui::size(px(200.), px(20.)));
        SLIDER_DRAG.set(Some((tint.clone(), track)));
        // The track it began on, even if the slider has moved since.
        assert_eq!(slider_drags(&tint, true), Some(track));
        // Crossing another slider with the button still held.
        assert_eq!(slider_drags(&bg, true), None);
        // Released where no slider saw it: the next move ends the drag.
        assert_eq!(slider_drags(&bg, false), None);
        assert_eq!(slider_drags(&tint, true), None);
        // Pressing the button off any slider starts no drag.
        assert_eq!(slider_drags(&bg, true), None);
    }

    #[test]
    fn a_pinned_slider_scrolls_back_under_the_pointer() {
        // Spacing grew the rows above: the slider is 40 px lower, so the pane
        // scrolls 40 px further down.
        assert_eq!(
            pin_offset(px(500.), px(540.), px(-100.), px(300.)),
            Some(px(-140.))
        );
        // It moved up: scroll back up.
        assert_eq!(
            pin_offset(px(500.), px(480.), px(-100.), px(300.)),
            Some(px(-80.))
        );
        // Only as far as the pane goes.
        assert_eq!(
            pin_offset(px(500.), px(540.), px(-290.), px(300.)),
            Some(px(-300.))
        );
        assert_eq!(pin_offset(px(500.), px(480.), px(0.), px(300.)), None);
        // Already in place.
        assert_eq!(pin_offset(px(500.), px(500.), px(-100.), px(300.)), None);
    }

    #[test]
    fn type_steps_up_and_back() {
        let sizes = |s| {
            set_type_scale(s);
            Type::ALL.map(|t| t.metrics().0)
        };
        let compact = sizes(TypeScale::Compact);
        let comfy = sizes(TypeScale::Comfortable);
        set_type_scale(TypeScale::Compact);
        assert_eq!(compact[4], 13.);
        assert_eq!(comfy[4], 14.);
        // Every size is a whole pixel, and lines never squeeze their text.
        for s in [TypeScale::Compact, TypeScale::Comfortable] {
            set_type_scale(s);
            for t in Type::ALL {
                let (size, line, _) = t.metrics();
                assert_eq!(size.fract(), 0., "{t:?}");
                assert!(line >= size * 1.15, "{t:?} {size}/{line}");
                assert_eq!(line % 2., 0., "{t:?} line {line} is odd");
            }
        }
        set_type_scale(TypeScale::Compact);
    }

    #[test]
    fn usage_tones() {
        assert_eq!(Tone::of_usage(10.), Tone::Success);
        assert_eq!(Tone::of_usage(70.), Tone::Warning);
        assert_eq!(Tone::of_usage(100.), Tone::Danger);
    }
}
