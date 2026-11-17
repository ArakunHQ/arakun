//! The page of a tab with no address: what runs in this environment and how to open it,
//! with the raccoon at its bench below. Drawn natively over the (blank) Chromium page, so
//! it is up before CEF paints anything and never flashes white.

use std::rc::Rc;

use gpui::{
    AnyElement, App, Entity, FontWeight, Hsla, InteractiveElement as _, IntoElement, MouseButton,
    ObjectFit, ParentElement as _, Pixels, SharedString, Size, StatefulInteractiveElement as _,
    Styled as _, StyledImage as _, Window, div, img, linear_color_stop, linear_gradient,
    prelude::FluentBuilder as _, px, relative,
};

use super::text_field::TextField;
use crate::theme;
use crate::ui::{self, icon};

/// Something listening on localhost here, and what started it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Server {
    pub port: u16,
    /// The command `arakun run` started, or the terminal it printed its address in.
    pub by: SharedString,
}

/// What the shell knows about the environment a blank tab lives in.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct HomeInfo {
    pub branch: SharedString,
    /// Number and address of the branch's pull request.
    pub pr: Option<(u64, SharedString)>,
    pub servers: Vec<Server>,
    /// The ports were probed at least once; only then does an empty list mean
    /// "nothing running".
    pub checked: bool,
}

/// What the panel's size allows. Short panels (a laptop with the dock open) drop the
/// prose; narrow ones (a split) shrink the title and stack the rows.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Layout {
    pub compact: bool,
    pub narrow: bool,
}

impl Layout {
    pub fn of(size: Size<Pixels>) -> Self {
        let (w, h) = (f32::from(size.width), f32::from(size.height));
        Self {
            compact: h > 0. && h < 450.,
            narrow: w > 0. && w < 700.,
        }
    }
}

pub type Open = Rc<dyn Fn(&str, &mut Window, &mut App)>;
pub type Handler = Box<dyn Fn(&mut Window, &mut App)>;

pub struct Home {
    pub info: HomeInfo,
    pub size: Size<Pixels>,
    pub field: Entity<TextField>,
    pub field_focused: bool,
    /// Loads an address in this tab (the Open button, a server chip, the PR).
    pub open: Open,
    /// A click on the background: the view gives the big field the keyboard.
    pub on_background: Handler,
}

// Sizes of the hero type, outside the app's scale on purpose: this is the one
// landing-page moment in the app.
const TITLE: f32 = 46.;
const TITLE_SMALL: f32 = 30.;
const LEDE: f32 = 15.;
const LEDE_SMALL: f32 = 14.;
const FIELD: f32 = 15.;
const CHIP: f32 = 12.5;
const HINT: f32 = 11.5;

/// The scene asset's pixel size. Its alpha channel fades out over the top and both
/// sides, so it blends into the sky at any size without a visible rectangle.
const SCENE_W: f32 = 1180.;
const SCENE_H: f32 = 484.;
/// Drawn between these widths, centered: wide panels get sky around the bench instead
/// of a blown-up raccoon, narrow ones crop the faded sides rather than shrink him.
const SCENE_MIN_W: f32 = 820.;
const SCENE_MAX_W: f32 = 1040.;
/// Short panels may shrink the scene down to this width to keep the raccoon whole;
/// below it he is too small to read and the sky stands alone.
const SCENE_FLOOR_W: f32 = 560.;
/// Share of the scene's height above its highest detail, the tips of the raccoon's
/// ears; the lamp shade starts just below them (eyes ~0.35, hands ~0.66).
const EARS: f32 = 0.12;
/// Share of the scene's height that must stay on screen: down past the raccoon's
/// hands and magnifier, so a cropped bench never cuts through his face.
const SHOW: f32 = 0.67;
/// Breathing room between the text column and the ears.
const SCENE_GAP: f32 = 8.;

/// Where the text column ends, from the sizes in [`content`]; the scene stays below it.
fn content_bottom(layout: Layout, chip_rows: f32) -> f32 {
    let chips = 28. * chip_rows + 8. * (chip_rows - 1.);
    if layout.compact {
        // pill, title, field, chips
        34. + 24. + 33. + 40. + chips + 3. * 12.
    } else {
        // pill, title, lede, field, handwritten line, chips, hints
        let lede = if layout.narrow { 66. } else { 45. };
        84. + 24. + 51. + lede + 40. + 28. + chips + 16. + 6. * 18.
    }
}

/// Tiny stars over the top of the sky: x and y as shares of the panel, size, alpha.
const STARS: &[(f32, f32, f32, f32)] = &[
    (0.06, 0.08, 2., 0.30),
    (0.13, 0.22, 1., 0.22),
    (0.19, 0.05, 1., 0.26),
    (0.27, 0.16, 2., 0.18),
    (0.33, 0.31, 1., 0.20),
    (0.41, 0.07, 1., 0.30),
    (0.48, 0.26, 2., 0.16),
    (0.55, 0.04, 1., 0.24),
    (0.62, 0.19, 1., 0.28),
    (0.69, 0.33, 2., 0.18),
    (0.74, 0.09, 1., 0.22),
    (0.81, 0.24, 1., 0.26),
    (0.88, 0.06, 2., 0.30),
    (0.93, 0.29, 1., 0.20),
    (0.97, 0.14, 1., 0.24),
    (0.09, 0.40, 1., 0.16),
    (0.58, 0.44, 1., 0.14),
    (0.84, 0.42, 1., 0.16),
];

fn sky_top() -> Hsla {
    gpui::rgb(0x070912).into()
}
fn sky_mid() -> Hsla {
    gpui::rgb(0x0d1020).into()
}
fn field_bg() -> Hsla {
    gpui::rgb(0x161a27).into()
}
fn pill_bg() -> Hsla {
    theme::background().opacity(0.6)
}
fn chip_bg() -> Hsla {
    theme::background().opacity(0.8)
}

pub fn render(h: Home) -> AnyElement {
    let layout = Layout::of(h.size);
    let empty = h.info.checked && h.info.servers.is_empty();
    let on_background = h.on_background;
    div()
        .id("browser-home")
        .absolute()
        .inset_0()
        .overflow_hidden()
        .bg(theme::background())
        .on_mouse_down(MouseButton::Left, move |_, window, cx| {
            on_background(window, cx)
        })
        .child(sky())
        .children(STARS.iter().map(|&(x, y, size, alpha)| {
            div()
                .absolute()
                .left(relative(x))
                .top(relative(y))
                .size(px(size))
                .rounded_full()
                .bg(gpui::white().opacity(alpha))
        }))
        .children(scene(h.size, layout, &h.info))
        .child(content(
            &h.info,
            layout,
            empty,
            h.field,
            h.field_focused,
            h.open,
        ))
        .into_any_element()
}

/// Darker at the top, the app's background where the scene starts.
fn sky() -> gpui::Div {
    div()
        .absolute()
        .inset_0()
        .child(
            div()
                .absolute()
                .left_0()
                .right_0()
                .top_0()
                .h(relative(0.55))
                .bg(linear_gradient(
                    180.,
                    linear_color_stop(sky_top(), 0.),
                    linear_color_stop(sky_mid(), 1.),
                )),
        )
        .child(
            div()
                .absolute()
                .left_0()
                .right_0()
                .bottom_0()
                .h(relative(0.45))
                .bg(linear_gradient(
                    180.,
                    linear_color_stop(sky_mid(), 0.),
                    linear_color_stop(theme::background(), 1.),
                )),
        )
}

/// The bench, anchored to the bottom edge. Nothing until the panel's size is known.
fn scene(size: Size<Pixels>, layout: Layout, info: &HomeInfo) -> Option<gpui::Div> {
    let (w, h) = (f32::from(size.width), f32::from(size.height));
    if w <= 0. || h <= 0. {
        return None;
    }
    // Narrow panels wrap the server chips onto a second row above the branch chip.
    let chip_rows = if layout.narrow && !info.servers.is_empty() {
        2.
    } else {
        1.
    };
    let room = h - content_bottom(layout, chip_rows) - SCENE_GAP;
    // Only the ears may rise above the text; the bottom edge crops the bench. When
    // that would leave less than SHOW of the picture, shrink it rather than cut it.
    let fit_h = room / (SHOW - EARS);
    let scene_w = w
        .clamp(SCENE_MIN_W, SCENE_MAX_W)
        .min(fit_h * SCENE_W / SCENE_H);
    if scene_w < SCENE_FLOOR_W {
        return None;
    }
    let scene_h = scene_w * SCENE_H / SCENE_W;
    let top = (h - scene_h).max(h - room - scene_h * EARS);
    Some(
        div()
            .absolute()
            .left(px((w - scene_w) / 2.))
            .top(px(top))
            .w(px(scene_w))
            .h(px(scene_h))
            .child(
                img("brand/browser-home.webp")
                    .size_full()
                    .object_fit(ObjectFit::Cover),
            ),
    )
}

fn content(
    info: &HomeInfo,
    layout: Layout,
    empty: bool,
    field: Entity<TextField>,
    field_focused: bool,
    open: Open,
) -> gpui::Div {
    let small_title = layout.compact || layout.narrow;
    let first_port = info.servers.first().map_or(5173, |s| s.port);
    div()
        .absolute()
        .inset_0()
        .flex()
        .flex_col()
        .items_center()
        .pt(crate::ui::sp(if layout.compact { 34. } else { 84. }))
        .px(crate::ui::sp(24.))
        .gap(crate::ui::sp(if layout.compact { 12. } else { 18. }))
        .child(pill(info, empty))
        .child(title(small_title))
        .when(!layout.compact, |d| d.child(lede(layout, empty)))
        .child(address(field, field_focused, layout, open.clone()))
        .when(!layout.compact, |d| {
            d.child(if empty {
                div()
                    .font_family(theme::FONT_MONO)
                    .text_size(px(ui::TEXT_MD))
                    .text_color(theme::text_muted())
                    .child("$ arakun run --port 5173 npm run dev")
            } else {
                div()
                    .text_size(px(LEDE))
                    .text_color(theme::accent().opacity(0.8))
                    .child("Works on localhost. Works on staging. Works at 3am.")
            })
        })
        .child(chips(info, layout, open))
        .when(!layout.compact, |d| d.child(hints(first_port, layout)))
}

fn pill(info: &HomeInfo, empty: bool) -> gpui::Div {
    let mut label = String::from("New tab");
    if !info.branch.is_empty() {
        label.push_str(&format!(" · {}", info.branch));
    }
    if empty {
        label.push_str(" · nothing running");
    }
    div()
        .flex()
        .flex_none()
        .items_center()
        .gap(crate::ui::sp(8.))
        .h(px(24.))
        .px(crate::ui::sp(10.))
        .rounded_full()
        .border_1()
        .border_color(theme::border())
        .bg(pill_bg())
        .text_size(px(ui::TEXT_XS))
        .text_color(theme::text_subtle())
        .child(div().size(px(6.)).rounded_full().bg(theme::success()))
        .child(label)
}

fn title(small: bool) -> gpui::Div {
    div()
        .flex()
        .flex_none()
        .items_baseline()
        .gap(crate::ui::sp(if small { 7. } else { 11. }))
        .text_size(px(if small { TITLE_SMALL } else { TITLE }))
        .line_height(relative(1.1))
        .font_weight(FontWeight::SEMIBOLD)
        .text_color(theme::text())
        .child("Nothing to look at")
        .child(
            div()
                .flex()
                .child(div().text_color(theme::accent()).child("yet"))
                .child("."),
        )
}

fn lede(layout: Layout, empty: bool) -> gpui::Div {
    let text = if empty {
        "No servers running in this environment. The raccoon refuses to inspect a blank page, so start something it can look at:"
    } else {
        "Type an address, open a server running in this environment, or let an agent bring the page it just built. The raccoon checks it either way."
    };
    div()
        .flex_none()
        .max_w(px(if layout.narrow { 440. } else { 540. }))
        .text_center()
        .text_size(px(if layout.narrow { LEDE_SMALL } else { LEDE }))
        .line_height(relative(1.5))
        .text_color(theme::text_subtle())
        .child(text)
}

fn address(field: Entity<TextField>, focused: bool, layout: Layout, open: Open) -> gpui::Div {
    let submit = field.clone();
    div()
        .flex()
        .flex_none()
        .items_center()
        .gap(crate::ui::sp(10.))
        .child(
            div()
                .flex()
                .items_center()
                .gap(crate::ui::sp(8.))
                .w(px(if layout.narrow { 300. } else { 380. }))
                .h(px(40.))
                .px(crate::ui::sp(12.))
                .rounded(crate::ui::rad(ui::RADIUS))
                .border_1()
                .border_color(if focused {
                    theme::accent().opacity(0.7)
                } else {
                    theme::border_strong()
                })
                .bg(field_bg())
                .text_size(px(FIELD))
                .line_height(px(20.))
                .child(icon("globe", 14.).text_color(theme::text_subtle()))
                .child(field),
        )
        .child(
            div()
                .id("browser-home-open")
                .flex()
                .flex_none()
                .items_center()
                .h(px(40.))
                .px(crate::ui::sp(16.))
                .rounded(crate::ui::rad(ui::RADIUS))
                .bg(theme::accent())
                .text_size(px(ui::TEXT_LG))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(theme::on_accent())
                .cursor_pointer()
                .hover(|s| s.opacity(0.9))
                .on_click(move |_, window, cx| {
                    let text = submit.read(cx).text().to_string();
                    open(&text, window, cx);
                })
                .child("Open"),
        )
}

fn chips(info: &HomeInfo, layout: Layout, open: Open) -> gpui::Div {
    let mut row = div()
        .flex()
        .flex_none()
        .flex_wrap()
        .justify_center()
        .items_center()
        .gap(crate::ui::sp(8.))
        .max_w(px(if layout.narrow { 460. } else { 760. }));
    for server in info.servers.iter().take(4) {
        let url = format!("http://localhost:{}", server.port);
        let open = open.clone();
        row = row.child(
            chip(("browser-home-server", server.port as usize))
                .cursor_pointer()
                .hover(|s| s.border_color(theme::border_strong()))
                .on_click(move |_, window, cx| open(&url, window, cx))
                .child(div().size(px(6.)).rounded_full().bg(theme::success()))
                .child(
                    div()
                        .font_family(theme::FONT_MONO)
                        .text_color(theme::text())
                        .child(format!("localhost:{}", server.port)),
                )
                .child(
                    div()
                        .text_color(theme::text_subtle())
                        .child(server.by.clone()),
                ),
        );
    }
    if !info.branch.is_empty() {
        let mut branch = chip("browser-home-branch")
            .child(icon("git-branch", 12.).text_color(theme::text_subtle()))
            .child(
                div()
                    .font_family(theme::FONT_MONO)
                    .text_color(theme::text_muted())
                    .child(info.branch.clone()),
            );
        if let Some((number, url)) = &info.pr {
            let url = url.to_string();
            let open = open.clone();
            branch = branch
                .child(div().text_color(theme::text_faint()).child("·"))
                .child(
                    div()
                        .id("browser-home-pr")
                        .cursor_pointer()
                        .text_color(theme::link())
                        .hover(|s| s.text_color(theme::text()))
                        .on_click(move |_, window, cx| open(&url, window, cx))
                        .child(format!("PR #{number}")),
                );
        }
        row = row.child(branch);
    }
    row
}

fn chip(id: impl Into<gpui::ElementId>) -> gpui::Stateful<gpui::Div> {
    div()
        .id(id)
        .flex()
        .flex_none()
        .items_center()
        .gap(crate::ui::sp(8.))
        .h(px(28.))
        .px(crate::ui::sp(12.))
        .rounded_full()
        .border_1()
        .border_color(theme::border())
        .bg(chip_bg())
        .text_size(px(CHIP))
}

/// What an agent (or F12) can do with this tab, in one quiet line.
fn hints(port: u16, layout: Layout) -> gpui::Div {
    let items: [(&str, String); 3] = [
        ("bot", format!("arakun page open :{port}")),
        ("image01", "arakun page screenshot".into()),
        ("keyboard", "F12 · real DevTools".into()),
    ];
    let mut row = div()
        .flex()
        .flex_none()
        .items_center()
        .gap(crate::ui::sp(if layout.narrow { 6. } else { 10. }))
        .font_family(theme::FONT_MONO)
        .text_size(px(HINT))
        .text_color(theme::text_faint())
        .when(layout.narrow, |d| d.flex_col().items_start());
    for (i, (name, text)) in items.into_iter().enumerate() {
        if i > 0 && !layout.narrow {
            row = row.child(div().text_color(theme::border_strong()).child("·"));
        }
        row = row.child(
            div()
                .flex()
                .items_center()
                .gap(crate::ui::sp(6.))
                .child(icon(name, 12.).text_color(theme::text_faint()))
                .child(text),
        );
    }
    row
}
