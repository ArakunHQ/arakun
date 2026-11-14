//! Small building blocks shared by the shell: icons, icon buttons, pixel art.

use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnimationExt as _, AnyView, App, AppContext as _, Bounds, Context, ElementId, Hsla,
    InteractiveElement as _, IntoElement, ParentElement as _, Pixels, Render, SharedString, Styled,
    Svg, Window, canvas, div, fill, point, px, size, svg,
};

use crate::theme;

/// The color of tree guides: content at 14% mixed into the background, opaque
/// so the spine and its curve do not darken where they overlap.
pub fn tree_color() -> Hsla {
    let (bg, fg) = (theme::background().l, theme::content().l);
    gpui::hsla(0., 0., bg + (fg - bg) * 0.14, 1.)
}

/// An item under a parent: a spine down the left that curves into the row,
/// meeting it `mid` px from its top. `gap` is the space to the next item, which
/// the spine crosses; the last item's spine stops at its curve.
pub fn tree_item(last: bool, mid: f32, gap: f32, row: gpui::AnyElement) -> gpui::AnyElement {
    let c = tree_color();
    let top = mid - 8.;
    div()
        .relative()
        .min_w_0()
        .pl(px(20.))
        .child(
            div()
                .absolute()
                .left(px(6.))
                .top_0()
                .w(px(1.))
                .bg(c)
                .map(|d| {
                    if last {
                        d.h(px(top + 1.))
                    } else {
                        d.bottom(px(-gap))
                    }
                }),
        )
        .child(
            div()
                .absolute()
                .left(px(6.))
                .top(px(top))
                .size(px(8.))
                .border_l_1()
                .border_b_1()
                .border_color(c)
                .rounded_bl(px(8.)),
        )
        .child(row)
        .into_any_element()
}

// Explicit paths: the examples include this file by `#[path]` from elsewhere.
#[path = "ui/chart.rs"]
mod chart;
#[path = "ui/kit.rs"]
mod kit;

#[allow(unused_imports)]
pub use chart::{AreaChart, Format, Series, sparkline};
#[allow(unused_imports)]
pub use kit::*;

pub fn icon(name: &str, size: f32) -> Svg {
    svg()
        .path(SharedString::from(format!("icons/{name}.svg")))
        .size(px(size))
        .flex_none()
}

/// Factory's mark for Droid, tinted like the text around it.
pub fn factory_logo(size: f32) -> Svg {
    svg()
        .path("providers/factory.svg")
        .size(px(size))
        .flex_none()
        .text_color(theme::content())
}

/// OpenCode's mark (from the ACP registry), tinted like the text around it.
pub fn opencode_logo(size: f32) -> Svg {
    svg()
        .path("providers/opencode.svg")
        .size(px(size))
        .flex_none()
        .text_color(theme::content())
}

/// What a hovered icon-only button shows: one line in a small dark card.
struct Tip(SharedString);

impl Render for Tip {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
            .px(px(8.))
            .py(px(4.))
            .rounded(px(RADIUS_SM))
            .bg(theme::dialog())
            .border_1()
            .border_color(theme::border())
            .shadow_lg()
            .text_size(px(TEXT_XS))
            .text_color(theme::content())
            .child(self.0.clone())
    }
}

/// Builds the tooltip closure for `.tooltip()`: tip("New session").
/// Shortcuts in it ("New session (Ctrl+T)") show as macOS writes them there.
pub fn tip(label: &'static str) -> impl Fn(&mut Window, &mut App) -> AnyView {
    let label = SharedString::from(crate::platform::shortcut_label(label));
    move |_, cx| {
        let label = label.clone();
        cx.new(|_| Tip(label)).into()
    }
}

/// `tip` for a label made at run time (a project's name).
pub fn tip_text(label: impl Into<SharedString>) -> impl Fn(&mut Window, &mut App) -> AnyView {
    let label = label.into();
    move |_, cx| {
        let label = label.clone();
        cx.new(|_| Tip(label)).into()
    }
}

/// Icon button: 24×24, rounded-md, content/50 → hover content on content/10.
pub fn icon_button(
    id: impl Into<ElementId>,
    name: &str,
    icon_size: f32,
) -> gpui::Stateful<gpui::Div> {
    div()
        .id(id)
        .flex()
        .flex_none()
        .items_center()
        .justify_center()
        .size(px(ICON_BUTTON))
        .rounded(px(RADIUS))
        .cursor_pointer()
        .group("icon-button")
        .hover(|s| s.bg(theme::fill_selected()))
        .child(hover_icon(
            name,
            icon_size,
            theme::text_subtle(),
            theme::content(),
            "icon-button",
        ))
}

/// SVGs don't inherit text color in GPUI, so hover tinting goes through a group.
pub fn hover_icon(name: &str, size: f32, color: Hsla, hover: Hsla, group: &'static str) -> Svg {
    icon(name, size)
        .text_color(color)
        .group_hover(group, move |s| s.text_color(hover))
}

/// Pixel art from `#`/`.` rows, one `unit` square per `#`.
pub fn pixel_art(rows: &'static [&'static str], unit: f32, color: Hsla) -> impl IntoElement {
    let w = rows.first().map_or(0, |r| r.len()) as f32 * unit;
    let h = rows.len() as f32 * unit;
    canvas(
        |_, _, _| {},
        move |bounds: Bounds<Pixels>, _, window: &mut Window, _: &mut App| {
            paint_pixels(rows, bounds.origin, unit, color, window);
        },
    )
    .w(px(w))
    .h(px(h))
    .flex_none()
}

pub fn paint_pixels(
    rows: &[&str],
    origin: gpui::Point<Pixels>,
    unit: f32,
    color: Hsla,
    window: &mut Window,
) {
    for (y, row) in rows.iter().enumerate() {
        let bytes = row.as_bytes();
        let mut x = 0;
        while x < bytes.len() {
            if bytes[x] != b'#' {
                x += 1;
                continue;
            }
            let mut run = 1;
            while x + run < bytes.len() && bytes[x + run] == b'#' {
                run += 1;
            }
            window.paint_quad(fill(
                Bounds::new(
                    point(
                        origin.x + px(x as f32 * unit),
                        origin.y + px(y as f32 * unit),
                    ),
                    size(px(run as f32 * unit), px(unit)),
                ),
                color,
            ));
            x += run;
        }
    }
}

pub const MOON: &[&str] = &[
    "..####..", ".###....", "###.....", "###.....", "###.....", "###.....", ".###....", "..####..",
];
pub const PLANET: &[&str] = &[
    "......##", "..####.#", ".#####.#", ".####.#.", ".#.###..", "#.####..", "..##....", "........",
];
pub const COMET: &[&str] = &[
    "#.......", ".#..#...", "..#..#..", "...#.##.", "....####", "...#####", "....####", ".....##.",
];
pub const STAR: &[&str] = &[
    "...##...", "...##...", "..####..", "########", ".######.", "..####..", ".##..##.", ".#....#.",
];
pub const CONSTELLATION: &[&str] = &[
    "........", "##.....#", "##....#.", "..#..#..", "...##...", "...##...", "........", "........",
];

/// Logos a project can get; `Project::art` indexes this.
pub const PROJECT_ART: [&[&str]; 5] = [MOON, PLANET, COMET, STAR, CONSTELLATION];

pub fn project_art(art: u8) -> &'static [&'static str] {
    PROJECT_ART[art as usize % PROJECT_ART.len()]
}

// ---------------------------------------------------------------------------
// The layout scale every view draws from, so controls of one kind match.
//
// Spacing steps: 2, 4, 6, 8, 12, 16, 24 (and 32+ for page-level room).
// Radii follow the rule of 4: 4 for chips and badges, 8 for controls,
// 12 for cards and menus, 16 for the window's floating cards.
// Text: 11 meta, 12 controls, 13 body, 14 reading text, 16/20 titles.

pub const RADIUS_SM: f32 = 4.;
pub const RADIUS: f32 = 8.;
pub const RADIUS_LG: f32 = 12.;
pub const RADIUS_XL: f32 = 16.;

pub const TEXT_XS: f32 = 11.;
pub const TEXT_SM: f32 = 12.;
pub const TEXT_MD: f32 = 13.;
pub const TEXT_LG: f32 = 14.;
pub const TEXT_TITLE: f32 = 20.;

/// Square icon-only buttons in toolbars, headers and rows.
pub const ICON_BUTTON: f32 = 24.;
/// Pills, chips and segmented controls.
pub const CONTROL_H: f32 = 26.;
/// Text buttons.
pub const BUTTON_H: f32 = 28.;
/// Rows of menus and pickers.
pub const MENU_ROW_H: f32 = 30.;
/// Text fields in pages and dialogs.
pub const INPUT_H: f32 = 32.;
/// The window's top bars: title bar, rail and panel headers.
pub const BAR_H: f32 = 40.;
/// Bars inside a view: browser toolbar, dock header, find bar.
pub const TOOLBAR_H: f32 = 36.;
/// Room around a full-page view (Tasks, Runs, Memory…).
pub const PAGE_PAD: f32 = 24.;

/// A window-level floating card: the rail, the main area and the workspace
/// panel. Rounds and clips its contents; menus and popovers render deferred
/// at the root, so the clip never cuts them. The caller adds size and `bg`.
pub fn card() -> gpui::Div {
    div()
        .rounded(px(RADIUS_XL))
        .border_1()
        .border_color(theme::stroke())
        .overflow_hidden()
}

/// Keeps a press inside a card from reaching the window root, whose handler
/// moves the window. Not on Windows: a press on a drag area there arrives as a
/// non-client click on HTCAPTION, and gpui cancels the native move whenever a
/// handler stops it, so the title bar could never move the window.
pub fn stop_window_move<E: gpui::InteractiveElement>(e: E) -> E {
    if cfg!(windows) {
        e
    } else {
        e.on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
    }
}

/// What a press on a drag area does: moves the window, and on macOS a double
/// click zooms or minimizes it, whichever the user picked in System Settings.
pub fn drag_window(ev: &gpui::MouseDownEvent, window: &mut Window) {
    if cfg!(target_os = "macos") && ev.click_count == 2 {
        window.titlebar_double_click();
    } else {
        window.start_window_move();
    }
}

/// Where the macOS traffic lights end, from the window's left edge, plus a gap:
/// what the top bar keeps clear. Zero elsewhere and in full screen, where macOS
/// hides them.
pub fn traffic_lights_inset(window: &Window) -> Pixels {
    if cfg!(target_os = "macos") && !window.is_fullscreen() {
        px(TRAFFIC_LIGHTS_W)
    } else {
        px(0.)
    }
}

/// The lights start at `TRAFFIC_LIGHTS` and take 52pt.
const TRAFFIC_LIGHTS_W: f32 = 80.;

/// The close button's top-left corner, centred on the 40pt top bar that sits
/// inside the window's 8pt margin.
pub const TRAFFIC_LIGHTS: (f32, f32) = (16., 22.);

/// A bordered box for a text input; the caller adds a leading icon and the input.
pub fn field() -> gpui::Div {
    div()
        .flex()
        .items_center()
        .gap(px(8.))
        .h(px(INPUT_H))
        .px(px(10.))
        .rounded(px(RADIUS))
        .border_1()
        .border_color(theme::border_strong())
        .bg(theme::background())
        .text_size(px(TEXT_MD))
        .line_height(px(20.))
        .overflow_hidden()
}

/// A page's heading: title and the line under it, as Tasks, Runs and Memory show.
pub fn page_title(title: impl Into<SharedString>, subtitle: Option<gpui::AnyElement>) -> gpui::Div {
    div()
        .flex()
        .flex_col()
        .min_w_0()
        .gap(px(4.))
        .child(
            div()
                .text_size(px(TEXT_TITLE))
                .line_height(px(26.))
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .text_color(theme::content())
                .truncate()
                .child(title.into()),
        )
        .when_some(subtitle, |d, s| {
            d.child(
                div()
                    .text_size(px(TEXT_MD))
                    .line_height(px(18.))
                    .text_color(theme::text_subtle())
                    .child(s),
            )
        })
}

/// One option of a segmented control (Due date | Status | Project, Graph | List).
pub fn segment(
    id: impl Into<ElementId>,
    leading: Option<&str>,
    label: impl Into<SharedString>,
    on: bool,
) -> gpui::Stateful<gpui::Div> {
    div()
        .id(id)
        .flex()
        .flex_none()
        .items_center()
        .gap(px(6.))
        .h(px(BUTTON_H - 2. * (SEGMENT_PAD + 1.)))
        .px(px(8.))
        .rounded(px(RADIUS_SM))
        .text_size(px(TEXT_SM))
        .font_weight(gpui::FontWeight::MEDIUM)
        .cursor_pointer()
        .when(on, |d| {
            d.bg(theme::fill_selected()).text_color(theme::content())
        })
        .when(!on, |d| {
            d.text_color(theme::text_subtle())
                .hover(|s| s.text_color(theme::content()))
        })
        .when_some(leading, |d, name| {
            d.child(icon(name, 12.).text_color(if on {
                theme::content()
            } else {
                theme::text_subtle()
            }))
        })
        .child(label.into())
}

/// How far a `segment` sits inside its track's border: with the 1 px border
/// that is 4, so a RADIUS_SM segment nests concentric in a RADIUS track.
const SEGMENT_PAD: f32 = 3.;

/// The track around `segment`s: as tall as a `button`, so they line up in a toolbar.
pub fn segmented() -> gpui::Div {
    div()
        .flex()
        .flex_none()
        .items_center()
        .gap(px(2.))
        .h(px(BUTTON_H))
        .p(px(SEGMENT_PAD))
        .rounded(px(RADIUS))
        .bg(theme::fill_raised())
        .border_1()
        .border_color(theme::border())
}
// ---------------------------------------------------------------------------
// Dialogs and menus.

/// Panel of a centered dialog: `rounded-2xl border-content/7 shadow-2xl`.
///
/// It never outgrows the window: the part meant to give way on short windows
/// (a list or a scrolling body) needs `min_h_0()` so flexbox can shrink it.
pub fn modal_panel(id: impl Into<ElementId>, width: f32) -> gpui::Stateful<gpui::Div> {
    div()
        .id(id)
        .occlude()
        .flex()
        .flex_col()
        .w(px(width))
        .max_w_full()
        .max_h_full()
        .rounded(px(16.))
        .border_1()
        .border_color(theme::border_subtle())
        .bg(theme::dialog())
        .shadow_2xl()
        .overflow_hidden()
}

/// Header row: title, optional description, and the slot for a close button.
pub fn modal_header(
    title: impl Into<SharedString>,
    description: Option<SharedString>,
    close: impl IntoElement,
) -> gpui::Div {
    div()
        .flex()
        .flex_none()
        .items_start()
        .gap(px(8.))
        .px(px(16.))
        .pt(px(12.))
        .child(
            div()
                .flex()
                .flex_col()
                .flex_1()
                .min_w_0()
                .pt(px(2.))
                .child(div().ty(Type::Subtitle).child(title.into()))
                .when_some(description, |d, desc| {
                    d.child(
                        div()
                            .mt(px(2.))
                            .text_size(px(TEXT_SM))
                            .line_height(px(17.))
                            .text_color(theme::text_subtle())
                            .child(desc),
                    )
                }),
        )
        .child(close)
}

/// The dialog's `X`: 28×28, content/45 → content on hover.
pub fn close_button(id: impl Into<ElementId>) -> gpui::Stateful<gpui::Div> {
    div()
        .id(id)
        .group("dialog-close")
        .flex()
        .flex_none()
        .items_center()
        .justify_center()
        .size(px(28.))
        .rounded(px(8.))
        .cursor_pointer()
        .hover(|s| s.bg(theme::fill_hover()))
        .child(hover_icon(
            "cancel01",
            14.,
            theme::text_subtle(),
            theme::content(),
            "dialog-close",
        ))
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ButtonKind {
    Primary,
    Ghost,
    /// Ghost with a border, for secondary actions in toolbars and pages.
    Outline,
    Danger,
}

/// Dialog buttons: `px-3 py-1.5 text-[12px] rounded-md`.
pub fn button(
    id: impl Into<ElementId>,
    label: impl Into<SharedString>,
    kind: ButtonKind,
    enabled: bool,
) -> gpui::Stateful<gpui::Div> {
    button_with(id, None, label, kind, enabled)
}

/// `button` with an icon before its label.
pub fn button_with(
    id: impl Into<ElementId>,
    leading: Option<&str>,
    label: impl Into<SharedString>,
    kind: ButtonKind,
    enabled: bool,
) -> gpui::Stateful<gpui::Div> {
    let lead = leading.map(|name| {
        let color = match kind {
            ButtonKind::Primary => theme::background(),
            ButtonKind::Danger => theme::error().into(),
            _ => theme::text_muted(),
        };
        icon(name, 14.).text_color(color).into_any_element()
    });
    button_lead(id, lead, label, kind, enabled)
}

/// `button` with any element before its label, such as a color dot.
pub fn button_lead(
    id: impl Into<ElementId>,
    leading: Option<gpui::AnyElement>,
    label: impl Into<SharedString>,
    kind: ButtonKind,
    enabled: bool,
) -> gpui::Stateful<gpui::Div> {
    let base = div()
        .id(id)
        .flex()
        .flex_none()
        .items_center()
        .justify_center()
        .gap(px(6.))
        .h(px(BUTTON_H))
        .px(px(if leading.is_some() { 10. } else { 12. }))
        .rounded(px(RADIUS))
        .text_size(px(TEXT_SM))
        .font_weight(gpui::FontWeight::MEDIUM)
        .when(enabled, |d| d.cursor_pointer())
        .when(!enabled, |d| d.opacity(0.4))
        .when_some(leading, |d, lead| d.child(lead))
        .child(div().min_w_0().truncate().child(label.into()));
    match kind {
        ButtonKind::Primary => base
            .bg(theme::content())
            .text_color(theme::background())
            .when(enabled, |d| d.hover(|s| s.bg(theme::mix(0.8)))),
        ButtonKind::Ghost => base
            .text_color(theme::content())
            .when(enabled, |d| d.hover(|s| s.bg(theme::fill_hover()))),
        ButtonKind::Outline => base
            .border_1()
            .border_color(theme::border_strong())
            .text_color(theme::text_muted())
            .when(enabled, |d| {
                d.hover(|s| s.bg(theme::fill_hover()).text_color(theme::content()))
            }),
        ButtonKind::Danger => base
            .bg(Hsla::from(theme::error()).opacity(0.2))
            .text_color(theme::error())
            .when(enabled, |d| {
                d.hover(|s| s.bg(Hsla::from(theme::error()).opacity(0.3)))
            }),
    }
}

/// Floating menu surface, like the environment menu.
pub fn menu_panel(id: impl Into<ElementId>, width: f32) -> gpui::Stateful<gpui::Div> {
    div()
        .id(id)
        .occlude()
        .w(px(width))
        .p(px(4.))
        .flex()
        .flex_col()
        .gap(px(1.))
        .bg(theme::dialog())
        .border_1()
        .border_color(theme::border())
        .rounded(px(RADIUS_XL))
        .shadow_xl()
}

pub fn menu_item(
    id: impl Into<ElementId>,
    icon_name: &str,
    label: impl Into<SharedString>,
    danger: bool,
    enabled: bool,
) -> gpui::Stateful<gpui::Div> {
    let color: Hsla = if danger {
        theme::error().into()
    } else {
        theme::text_muted()
    };
    div()
        .id(id)
        .flex()
        .items_center()
        .gap(px(8.))
        .h(px(MENU_ROW_H))
        .px(px(8.))
        .rounded(px(RADIUS))
        .text_size(px(TEXT_MD))
        .text_color(color)
        .when(enabled, |d| {
            d.cursor_pointer().hover(|s| s.bg(theme::selection_hover()))
        })
        .when(!enabled, |d| d.opacity(0.4))
        .child(icon(icon_name, 14.).text_color(color))
        .child(div().flex_1().min_w_0().truncate().child(label.into()))
}

/// Small spinning loader for busy buttons and rows.
pub fn spinner(id: impl Into<ElementId>, size: f32, color: Hsla) -> impl IntoElement {
    icon("loading03", size).text_color(color).with_animation(
        id,
        gpui::Animation::new(std::time::Duration::from_millis(900)).repeat(),
        |svg, t| svg.with_transformation(gpui::Transformation::rotate(gpui::percentage(t))),
    )
}
