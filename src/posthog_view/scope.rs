//! What a tab shows when the key lacks a scope it needs: the scope to add,
//! where to add it, and a way to read again. Only that tab stops.

use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, App, ClickEvent, IntoElement, ParentElement as _, StatefulInteractiveElement as _,
    Styled as _, Window, div, px,
};

use crate::posthog::scope;
use crate::theme;
use crate::ui::{self, ButtonKind, Type, Typography as _, icon};

fn scope_chip(name: &str) -> gpui::Div {
    div()
        .flex()
        .flex_none()
        .items_center()
        .h(px(ui::CONTROL_H))
        .px(ui::sp(8.))
        .rounded(ui::rad(ui::RADIUS_SM))
        .border_1()
        .border_color(theme::border())
        .bg(theme::fill_raised())
        .ty(Type::Mono)
        .text_color(theme::text())
        .child(name.to_string())
}

fn row(label: &str, chips: Vec<gpui::Div>) -> gpui::Div {
    div()
        .flex()
        .items_start()
        .gap(ui::sp(12.))
        .child(
            ui::text(Type::Body, theme::text_muted(), label.to_string())
                .w(px(140.))
                .flex_none()
                .pt(ui::sp(4.)),
        )
        .child(div().flex().flex_wrap().gap(ui::sp(8.)).children(chips))
}

/// The card for a `tab` ("People") that needs `missing`. `works` names the
/// scopes calls already succeeded with, and `other_tabs` the tabs that keep
/// working.
#[allow(clippy::too_many_arguments)]
pub fn missing_scope(
    tab: &str,
    missing: &str,
    email: &str,
    host: &str,
    works: &[&str],
    other_tabs: &str,
    on_open: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    on_retry: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> AnyElement {
    let url = scope::keys_url(host);
    let what = tab.to_lowercase();
    div()
        .flex()
        .flex_col()
        .gap(ui::sp(24.))
        .w_full()
        .max_w(px(560.))
        .mx_auto()
        .pt(ui::sp(48.))
        .child(
            div()
                .size(px(40.))
                .flex()
                .items_center()
                .justify_center()
                .rounded(ui::rad(ui::RADIUS))
                .border_1()
                .border_color(theme::border())
                .bg(theme::fill_raised())
                .child(icon("square-lock02", 16.).text_color(theme::text_muted())),
        )
        .child(
            div()
                .flex()
                .flex_col()
                .gap(ui::sp(16.))
                .p(ui::sp(24.))
                .rounded(ui::rad(ui::RADIUS_XL))
                .border_1()
                .border_color(theme::border_subtle())
                .bg(theme::fill_raised())
                .child(ui::heading(format!("{tab} needs one more scope")))
                .child(ui::text(
                    Type::Body,
                    theme::text_muted(),
                    format!(
                        "The key for {} can't read {what}. Add this scope to the key, then read again. Nothing else changes.",
                        crate::redact::value(email)
                    ),
                ))
                .child(row("Add", vec![scope_chip(missing)]))
                .when(!works.is_empty(), |d| {
                    d.child(row(
                        "Already works with",
                        works.iter().map(|s| scope_chip(s)).collect(),
                    ))
                })
                .child(
                    div()
                        .flex()
                        .gap(ui::sp(8.))
                        .child(
                            ui::button_with(
                                "ph-scope-open",
                                Some("link-square02"),
                                "Open personal API keys",
                                ButtonKind::Primary,
                                true,
                            )
                            .on_click(on_open),
                        )
                        .child(
                            ui::button_with(
                                "ph-scope-retry",
                                Some("refresh01"),
                                "Read again",
                                ButtonKind::Outline,
                                true,
                            )
                            .on_click(on_retry),
                        ),
                )
                .child(ui::text(
                    Type::Mono,
                    theme::text_faint(),
                    url.trim_start_matches("https://").to_string(),
                )),
        )
        .child(ui::text(
            Type::Caption,
            theme::text_faint(),
            format!("Arakun only asks PostHog for what this tab reads. {other_tabs} keep working."),
        ))
        .into_any_element()
}
