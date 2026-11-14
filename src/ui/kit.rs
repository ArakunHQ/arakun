//! The design system's pieces beyond buttons and dialogs: the type scale, and
//! the chips, badges, menus, meters and stats the views share. Every color
//! comes from a `theme` role, every size from the scale in `ui`, so two views
//! that show the same kind of thing show it the same way.
//!
//! The Design system tab (`gallery_view`) shows all of them.

use std::sync::atomic::{AtomicU8, Ordering};

use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, ElementId, FontWeight, Hsla, InteractiveElement as _, IntoElement, ParentElement as _, StatefulInteractiveElement as _,
    SharedString, Styled, div, px, relative,
};

use super::{MENU_ROW_H, RADIUS, RADIUS_SM, icon};
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
    /// SMALL CAPS above a group; the caller upper-cases the text.
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
        Self::ALL.into_iter().find(|t| t.id() == id).unwrap_or(TypeScale::Compact)
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

/// `.ty(Type::Body)` on any element: size, line height and weight at once.
pub trait Typography: Styled + Sized {
    fn ty(self, t: Type) -> Self {
        let (size, line, weight) = t.metrics();
        let s = self.text_size(px(size)).line_height(px(line)).font_weight(weight);
        if t == Type::Mono { s.font_family(theme::FONT_MONO) } else { s }
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
    pub const ALL: [Tone; 6] = [Tone::Neutral, Tone::Accent, Tone::Info, Tone::Success, Tone::Warning, Tone::Danger];

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
        .gap(px(4.))
        .h(px(20.))
        .px(px(6.))
        .rounded(px(RADIUS_SM))
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
        .gap(px(6.))
        .ty(Type::Label)
        .text_color(theme::text_muted())
        .child(div().size(px(7.)).rounded_full().bg(color))
        .child(content.into())
}

/// A filter or choice that toggles: outlined, filled when on.
pub fn chip(id: impl Into<ElementId>, content: impl Into<SharedString>, on: bool) -> gpui::Stateful<gpui::Div> {
    div()
        .id(id)
        .flex()
        .flex_none()
        .items_center()
        .gap(px(6.))
        .h(px(24.))
        .px(px(9.))
        .rounded_full()
        .border_1()
        .ty(Type::Label)
        .cursor_pointer()
        .when(on, |d| d.bg(theme::fill_selected()).border_color(theme::border_strong()).text_color(theme::text()))
        .when(!on, |d| {
            d.border_color(theme::border())
                .text_color(theme::text_muted())
                .hover(|s| s.bg(theme::fill_hover()).text_color(theme::text()))
        })
        .child(content.into())
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
        .px(px(5.))
        .rounded(px(RADIUS_SM))
        .border_1()
        .border_b_2()
        .border_color(theme::border())
        .bg(theme::fill_raised())
        .ty(Type::Caption)
        .font_family(theme::FONT_MONO)
        .text_color(theme::text_muted())
        .child(key.into())
}

/// A shortcut, keys joined: `shortcut(&["Ctrl", "K"])`.
pub fn shortcut(keys: &[&str]) -> gpui::Div {
    let mut row = div().flex().items_center().gap(px(3.));
    for k in keys {
        row = row.child(kbd(k.to_string()));
    }
    row
}

// ---------------------------------------------------------------------------
// Controls.

/// On/off, in the accent when on.
pub fn switch(on: bool) -> gpui::Div {
    div()
        .flex()
        .flex_none()
        .items_center()
        .w(px(30.))
        .h(px(18.))
        .p(px(2.))
        .rounded_full()
        .bg(if on { theme::accent() } else { theme::fill_selected() })
        .when(on, |d| d.justify_end())
        .child(div().size(px(14.)).rounded_full().bg(theme::content()))
}

/// A radio dot: an accent ring with a center when picked.
pub fn radio(on: bool) -> gpui::Div {
    div()
        .flex_none()
        .size(px(14.))
        .rounded_full()
        .border_1()
        .flex()
        .items_center()
        .justify_center()
        .when(on, |d| d.border_color(theme::accent()).child(div().size(px(6.)).rounded_full().bg(theme::accent())))
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
        .p(px(2.))
        .gap(px(2.))
        .rounded(px(RADIUS))
        .border_1()
        .border_color(theme::border())
        .bg(theme::fill_raised());
    for (i, (value, label)) in options.iter().enumerate() {
        let on = *value == current;
        let seg = div()
            .id((id, i))
            .px(px(8.))
            .h(px(20.))
            .flex()
            .items_center()
            .rounded(px(RADIUS_SM))
            .ty(Type::Overline)
            .cursor_pointer()
            .when(on, |d| d.bg(theme::fill_selected()).text_color(theme::text()))
            .when(!on, |d| d.text_color(theme::text_subtle()).hover(|s| s.text_color(theme::text())))
            .child(label.to_uppercase());
        track = track.child(on_pick(seg, *value));
    }
    track
}

// ---------------------------------------------------------------------------
// Menus.

pub fn menu_heading(content: &str) -> gpui::Div {
    overline(content).px(px(8.)).pt(px(6.)).pb(px(3.))
}

pub fn menu_divider() -> gpui::Div {
    div().h(px(1.)).mx(px(4.)).my(px(3.)).bg(theme::border_subtle())
}

/// A pickable row with a radio dot; the caller adds the name and any note.
pub fn choice_row(id: impl Into<ElementId>, selected: bool) -> gpui::Stateful<gpui::Div> {
    div()
        .id(id)
        .flex()
        .items_center()
        .gap(px(10.))
        .min_h(px(MENU_ROW_H))
        .py(px(5.))
        .px(px(8.))
        .rounded(px(RADIUS))
        .ty(Type::Body)
        .text_color(theme::text())
        .cursor_pointer()
        .hover(|d| d.bg(theme::fill_hover()))
        .when(selected, |d| d.bg(theme::fill_selected()))
        .child(radio(selected))
}

/// A row that opens more choices beside it: name, current value, chevron.
pub fn submenu_row(id: impl Into<ElementId>, name: &str, value: impl Into<SharedString>, open: bool) -> gpui::Stateful<gpui::Div> {
    div()
        .id(id)
        .flex()
        .items_center()
        .gap(px(8.))
        .h(px(MENU_ROW_H))
        .px(px(8.))
        .rounded(px(RADIUS))
        .ty(Type::Body)
        .text_color(theme::text())
        .cursor_pointer()
        .when(open, |d| d.bg(theme::fill_hover()))
        .hover(|d| d.bg(theme::fill_hover()))
        .child(div().flex_1().child(name.to_string()))
        .child(div().min_w_0().max_w(px(130.)).truncate().ty(Type::Label).text_color(theme::text_subtle()).child(value.into()))
        .child(icon("arrow-right01", 12.).text_color(theme::text_faint()))
}

/// A menu row that turns something on and off: name and a switch.
pub fn switch_row(id: impl Into<ElementId>, name: &str, on: bool) -> gpui::Stateful<gpui::Div> {
    div()
        .id(id)
        .flex()
        .items_center()
        .gap(px(8.))
        .h(px(MENU_ROW_H))
        .px(px(8.))
        .rounded(px(RADIUS))
        .ty(Type::Body)
        .text_color(theme::text())
        .cursor_pointer()
        .hover(|d| d.bg(theme::fill_hover()))
        .child(div().flex_1().child(name.to_string()))
        .child(switch(on))
}

// ---------------------------------------------------------------------------
// Data.

/// A thin bar filled to `pct` (0-100) in the tone's color.
pub fn meter(pct: f32, tone: Tone, height: f32) -> gpui::Div {
    let fill = (pct / 100.).clamp(0., 1.);
    div().h(px(height)).rounded_full().bg(theme::fill_selected()).child(
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
        .gap(px(8.))
        .h(px(22.))
        .ty(Type::Label)
        .font_weight(FontWeight::NORMAL)
        .child(div().flex_1().min_w_0().truncate().text_color(theme::text_muted()).child(label.to_string()))
        .child(div().flex_none().ty(Type::Caption).text_color(theme::text_faint()).child(detail.unwrap_or_default()))
        .child(meter(pct, tone, 4.).w(px(56.)).flex_none())
        .child(
            div()
                .w(px(36.))
                .flex_none()
                .flex()
                .justify_end()
                .text_color(if pct >= 70. { tone.color() } else { theme::text() })
                .child(format!("{:.0}%", pct.ceil())),
        )
        .into_any_element()
}

/// One metric: what it is, the value, and what it means underneath.
pub fn stat(label: &str, value: impl Into<SharedString>, sub: impl Into<SharedString>) -> gpui::Div {
    div()
        .flex()
        .flex_col()
        .gap(px(4.))
        .child(text(Type::Label, theme::text_subtle(), label.to_string()).font_weight(FontWeight::NORMAL))
        .child(div().text_size(px(20.)).line_height(px(26.)).text_color(theme::text()).child(value.into()))
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
        .child(div().flex_1().text_color(theme::text_muted()).child(label.to_string()))
        .child(div().text_color(theme::text()).child(value.into()))
}

/// What a list or page shows with nothing in it: an icon, what it is for,
/// and the one thing to do about it.
pub fn empty_state(icon_name: &str, title: &str, body: &str, action: Option<AnyElement>) -> gpui::Div {
    div()
        .flex()
        .flex_col()
        .items_center()
        .gap(px(8.))
        .py(px(32.))
        .px(px(24.))
        .child(
            div()
                .size(px(40.))
                .rounded(px(RADIUS))
                .bg(theme::fill_raised())
                .border_1()
                .border_color(theme::border())
                .flex()
                .items_center()
                .justify_center()
                .child(icon(icon_name, 18.).text_color(theme::text_subtle())),
        )
        .child(heading(title.to_string()).mt(px(4.)))
        .child(text(Type::Body, theme::text_subtle(), body.to_string()).max_w(px(320.)).text_center())
        .children(action.map(|a| div().mt(px(6.)).child(a)))
}

/// A place waiting on its data: a spinner and what it is reading.
pub fn loading(id: impl Into<ElementId>, label: impl Into<SharedString>) -> gpui::Div {
    div()
        .flex()
        .flex_col()
        .items_center()
        .gap(px(10.))
        .py(px(32.))
        .child(super::spinner(id, 16., theme::text_faint()))
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
        .gap(px(6.))
        .text_size(px(super::TEXT_XS))
        .text_color(theme::text_faint())
        .when_some(updated.filter(|_| !busy), |d, u| d.child(format!("updated {u}")))
        .child(if busy {
            div()
                .flex()
                .flex_none()
                .items_center()
                .justify_center()
                .size(px(super::ICON_BUTTON))
                .child(super::spinner(id, 13., theme::mix(0.5)))
                .into_any_element()
        } else {
            super::icon_button(id, "refresh01", 13.).tooltip(super::tip("Refresh")).on_click(on_refresh).into_any_element()
        })
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;

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

