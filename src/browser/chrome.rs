//! The browser toolbar and the small overlays drawn over the page, shared by the real view
//! and the placeholder used when arakun is built without the `browser` feature.

use std::sync::Arc;

use gpui::{
    AnyElement, App, ClickEvent, Div, ElementId, Entity, Hsla, InteractiveElement as _,
    IntoElement, ParentElement as _, RenderImage, SharedString, Stateful,
    StatefulInteractiveElement as _, Styled as _, Window, div, img, prelude::FluentBuilder as _,
    px, relative,
};

use super::text_field::TextField;
use crate::theme;
use crate::ui::{hover_icon, icon};

pub type Handler = Box<dyn Fn(&ClickEvent, &mut Window, &mut App) + 'static>;

pub struct Toolbar {
    pub can_back: bool,
    pub can_forward: bool,
    pub can_reload: bool,
    pub loading: bool,
    pub progress: f64,
    pub address_focused: bool,
    pub favicon: Option<Arc<RenderImage>>,
    pub download: Option<AnyElement>,
    pub on_back: Handler,
    pub on_forward: Handler,
    pub on_reload: Handler,
}

pub fn toolbar(t: Toolbar, address: Entity<TextField>) -> Div {
    let site_icon: AnyElement = match t.favicon {
        Some(image) => img(image).size(px(14.)).flex_none().into_any_element(),
        None => icon("internet", 12.).text_color(theme::text_subtle()).into_any_element(),
    };
    div()
        .relative()
        .flex()
        .flex_none()
        .items_center()
        .gap(px(4.))
        .h(px(36.))
        .px(px(8.))
        .border_b_1()
        .border_color(theme::stroke())
        .child(nav_button("browser-back", "arrow-left01", t.can_back, t.on_back))
        .child(nav_button("browser-forward", "arrow-right01", t.can_forward, t.on_forward))
        .child(nav_button(
            "browser-reload",
            if t.loading { "cancel01" } else { "refresh01" },
            t.can_reload,
            t.on_reload,
        ))
        .child(
            div()
                .flex()
                .flex_1()
                .min_w_0()
                .items_center()
                .gap(px(8.))
                .h(px(26.))
                .px(px(10.))
                .rounded(px(8.))
                .border_1()
                .border_color(if t.address_focused { theme::mix(0.16) } else { gpui::transparent_black() })
                .bg(theme::mix(if t.address_focused { 0.08 } else { 0.05 }))
                .text_size(px(crate::ui::TEXT_SM))
                .child(site_icon)
                .child(address)
                .children(t.download),
        )
        .when(t.loading, |d| {
            d.child(
                div()
                    .absolute()
                    .left_0()
                    .bottom(px(-1.))
                    .h(px(2.))
                    .w(relative(t.progress.clamp(0.08, 1.0) as f32))
                    .bg(theme::accent()),
            )
        })
}

fn nav_button(id: &'static str, name: &str, enabled: bool, on_click: Handler) -> Stateful<Div> {
    let button = div()
        .id(id)
        .flex()
        .flex_none()
        .items_center()
        .justify_center()
        .size(px(24.))
        .rounded(px(8.));
    if enabled {
        button
            .cursor_pointer()
            .group("browser-nav")
            .hover(|s| s.bg(theme::fill_selected()))
            .child(hover_icon(name, 14., theme::text_subtle(), theme::content(), "browser-nav"))
            .on_click(on_click)
    } else {
        button.child(icon(name, 14.).text_color(theme::mix(0.18)))
    }
}

pub fn panel_bg() -> Hsla {
    theme::surface(0.13)
}

/// The shared popover surface.
pub fn popover(id: impl Into<ElementId>) -> Stateful<Div> {
    div()
        .id(id)
        .occlude()
        .flex()
        .flex_col()
        .p(px(4.))
        .bg(panel_bg())
        .border_1()
        .border_color(theme::border())
        .rounded(px(12.))
        .shadow_lg()
        .text_size(px(crate::ui::TEXT_MD))
        .text_color(theme::content())
}

pub fn menu_item(id: impl Into<ElementId>, label: &str, shortcut: Option<&str>, enabled: bool) -> Stateful<Div> {
    let item = div()
        .id(id)
        .flex()
        .items_center()
        .justify_between()
        .gap(px(16.))
        .h(px(30.))
        .px(px(8.))
        .rounded(px(8.))
        .child(label.to_string())
        .children(shortcut.map(|s| div().text_size(px(crate::ui::TEXT_XS)).text_color(theme::text_faint()).child(crate::platform::shortcut_label(s))));
    if enabled {
        item.cursor_pointer().hover(|s| s.bg(theme::fill_selected()))
    } else {
        item.text_color(theme::text_faint())
    }
}

pub fn separator() -> Div {
    div().h(px(1.)).my(px(4.)).mx(px(4.)).bg(theme::fill_selected())
}

pub fn button(id: impl Into<ElementId>, label: impl Into<SharedString>, primary: bool) -> Stateful<Div> {
    div()
        .id(id)
        .flex()
        .items_center()
        .flex_none()
        .justify_center()
        .h(px(28.))
        .px(px(12.))
        .rounded(px(8.))
        .cursor_pointer()
        .text_size(px(crate::ui::TEXT_SM))
        .font_weight(gpui::FontWeight::MEDIUM)
        .when(primary, |d| d.bg(theme::accent()).text_color(theme::background()).hover(|s| s.opacity(0.9)))
        .when(!primary, |d| d.bg(theme::fill_selected()).text_color(theme::content()).hover(|s| s.bg(theme::fill_selected())))
        .child(label.into())
}

/// Full-page message (runtime missing, load error, crash).
pub fn notice(icon_name: &str, title: &str, detail: Option<SharedString>, action: Option<AnyElement>) -> Div {
    div()
        .absolute()
        .inset_0()
        .flex()
        .flex_col()
        .items_center()
        .justify_center()
        .gap(px(10.))
        .px(px(32.))
        .bg(theme::background())
        .text_size(px(crate::ui::TEXT_MD))
        .text_color(theme::text_subtle())
        .child(icon(icon_name, 28.).text_color(theme::text_faint()))
        .child(div().text_color(theme::content()).child(title.to_string()))
        .children(detail.map(|d| div().max_w(px(560.)).text_size(px(crate::ui::TEXT_SM)).text_color(theme::text_subtle()).child(d)))
        .children(action)
}
