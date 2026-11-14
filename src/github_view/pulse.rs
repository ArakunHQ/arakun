//! The left bottom card: the week of reviews, from GitHub's own record.

use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, Context, FontWeight, IntoElement, ParentElement as _, Styled as _, div, px,
    relative,
};

use super::GitHubView;
use crate::github::{pulse, queue};
use crate::theme;
use crate::ui::{self, Type, Typography as _, icon};

const WEEK_H: f32 = 54.;
const DAYS: [&str; 7] = ["M", "T", "W", "T", "F", "S", "S"];

impl GitHubView {
    pub(super) fn render_pulse(&self, cx: &mut Context<Self>) -> AnyElement {
        let g = self.github.read(cx);
        let me = g.me().unwrap_or_default().to_string();
        let now = crate::model::unix_now();
        let wday = crate::platform::local_time(now).map_or(0, |l| (l.wday + 6) % 7);
        let days: [u64; 8] = std::array::from_fn(|i| queue::day_start(now, wday - i as i32));
        let prev = queue::day_start(now, wday + 7);
        let p = g
            .activity()
            .map(|acts| pulse::pulse(&acts, &me, &days, prev, now));

        let change = p.as_ref().and_then(pulse::Pulse::change);
        let header = div()
            .flex()
            .flex_none()
            .items_center()
            .gap(px(11.))
            .px(px(16.))
            .pt(px(14.))
            .pb(px(8.))
            .child(ui::icon_chip("activity01", 34., None))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_w_0()
                    .child(ui::text(Type::Heading, theme::text(), "Review pulse"))
                    .child(
                        div()
                            .ty(Type::Label)
                            .font_weight(FontWeight::NORMAL)
                            .text_color(theme::text_subtle())
                            .child(match g.focus() {
                                [] => "This week · all repositories".to_string(),
                                [one] => format!("This week · {one}"),
                                many => format!("This week · {} places", many.len()),
                            }),
                    ),
            )
            .when_some(change, |d, c| {
                // Faster than last week is good news.
                let color = if c <= 0 {
                    theme::success()
                } else {
                    theme::attention()
                };
                let sign = if c > 0 { "+" } else { "−" };
                d.child(
                    ui::pill(format!("{sign}{}%", c.abs()), color).font_family(theme::FONT_MONO),
                )
            });

        let Some(p) = p else {
            return ui::surface_card()
                .flex_none()
                .child(header)
                .child(ui::loading("gh-pulse-loading", "Reading this week…").py(px(20.)))
                .into_any_element();
        };

        let big = match p.avg {
            Some(avg) => div()
                .flex()
                .items_baseline()
                .gap(px(9.))
                .child(
                    div()
                        .ty(Type::Title)
                        .font_family(theme::FONT_MONO)
                        .child(pulse::span(avg)),
                )
                .child(ui::overline("avg time to review").text_color(theme::text_faint())),
            None => div()
                .flex()
                .items_baseline()
                .gap(px(9.))
                .child(
                    div()
                        .ty(Type::Title)
                        .font_family(theme::FONT_MONO)
                        .text_color(theme::text_faint())
                        .child("—"),
                )
                .child(
                    ui::text(
                        Type::Label,
                        theme::text_subtle(),
                        "Not enough reviews this week yet",
                    )
                    .font_weight(FontWeight::NORMAL),
                ),
        };

        let top = p.days.iter().map(|(a, b)| a + b).max().unwrap_or(0).max(1) as f32;
        let unit = (WEEK_H - 4.) / top;
        let blue: gpui::Hsla = theme::link().into();
        let mut week = div()
            .flex()
            .items_end()
            .gap(px(8.))
            .h(px(WEEK_H))
            .mt(px(10.));
        let mut labels = div().flex().gap(px(8.)).mt(px(5.));
        for (i, (got, gave)) in p.days.iter().enumerate() {
            let today = i as i32 == wday;
            let (soft, strong) = if today {
                (theme::accent().opacity(0.4), theme::accent())
            } else {
                (blue.opacity(0.33), blue)
            };
            let col = div()
                .flex()
                .flex_col()
                .justify_end()
                .gap(px(2.))
                .flex_1()
                .h_full();
            let col = if got + gave == 0 {
                col.child(div().h(px(2.)).rounded_full().bg(theme::border()))
            } else {
                col.when(*got > 0, |d| {
                    d.child(div().h(px(*got as f32 * unit)).rounded(px(4.)).bg(soft))
                })
                .when(*gave > 0, |d| {
                    d.child(div().h(px(*gave as f32 * unit)).rounded(px(4.)).bg(strong))
                })
            };
            week = week.child(col);
            labels = labels.child(
                div()
                    .flex_1()
                    .text_center()
                    .ty(Type::Caption)
                    .when(today, |d| {
                        d.font_weight(FontWeight::SEMIBOLD)
                            .text_color(theme::text())
                    })
                    .when(!today, |d| d.text_color(theme::text_faint()))
                    .child(DAYS[i]),
            );
        }
        let legend = div()
            .flex()
            .gap(px(12.))
            .mt(px(6.))
            .child(ui::legend("Requests you got", blue.opacity(0.33)).ty(Type::Caption))
            .child(ui::legend("Reviews you gave", blue).ty(Type::Caption));

        let longest = p.waiting.first().map_or(1, |w| w.1).max(1) as f32;
        let mut waiting = div().flex().flex_col();
        waiting = waiting.child(
            div()
                .flex()
                .justify_between()
                .mt(px(14.))
                .mb(px(7.))
                .child(ui::overline("Waiting on you").text_color(theme::text_faint()))
                .child(ui::overline("longest first").text_color(theme::text_faint())),
        );
        if p.waiting.is_empty() {
            waiting = waiting.child(
                ui::text(
                    Type::Label,
                    theme::text_subtle(),
                    "Nobody waits on your review.",
                )
                .font_weight(FontWeight::NORMAL),
            );
        }
        for (i, (who, secs)) in p.waiting.iter().take(4).enumerate() {
            let late = i == 0 && *secs > 3600;
            let color = if late { theme::accent() } else { blue };
            waiting =
                waiting.child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(10.))
                        .py(px(3.5))
                        .child(
                            div()
                                .w(px(104.))
                                .flex_none()
                                .truncate()
                                .ty(Type::Label)
                                .font_weight(FontWeight::NORMAL)
                                .text_color(theme::text_muted())
                                .child(who.clone()),
                        )
                        .child(
                            div()
                                .flex_1()
                                .h(px(6.))
                                .rounded_full()
                                .bg(theme::fill_selected())
                                .child(
                                    div()
                                        .h_full()
                                        .rounded_full()
                                        .min_w(px(6.))
                                        .w(relative(*secs as f32 / longest))
                                        .bg(color),
                                ),
                        )
                        .child(div().w(px(52.)).flex_none().flex().justify_end().child(
                            super::mono(
                                pulse::span(*secs),
                                if late { theme::accent() } else { theme::text() },
                            ),
                        )),
                );
        }

        // The last seven days, oldest first; today last.
        let recent: Vec<String> = (0..7)
            .map(|i| queue::day_key(queue::day_start(now, i) + 3600))
            .collect();
        let zero = g.zero_days();
        let streak = queue::streak(zero, &recent);
        let dots = div()
            .flex()
            .gap(px(3.))
            .ml_auto()
            .children((0..7).rev().map(|i| {
                let hit = zero.contains(&recent[i]);
                let fade = 1. - i as f32 * 0.12;
                div().size(px(8.)).rounded(px(2.5)).bg(if hit {
                    theme::accent().opacity(fade)
                } else {
                    theme::fill_selected()
                })
            }));
        let days_text = match streak {
            0 => "not yet".to_string(),
            1 => "1 day".into(),
            n => format!("{n} days"),
        };
        let streak_pill = div()
            .flex()
            .items_center()
            .gap(px(9.))
            .mt(px(12.))
            .px(px(14.))
            .py(px(8.))
            .rounded_full()
            .border_1()
            .border_color(theme::border_subtle())
            .bg(theme::background())
            .child(icon("flash", 14.).text_color(theme::accent()))
            .child(
                ui::text(Type::Label, theme::text(), "Inbox zero").font_weight(FontWeight::NORMAL),
            )
            .child(super::mono(
                days_text,
                if streak > 0 {
                    theme::accent()
                } else {
                    theme::text_subtle()
                },
            ))
            .child(dots);

        ui::surface_card()
            .flex_none()
            .child(header)
            .child(
                div()
                    .flex()
                    .flex_col()
                    .px(px(16.))
                    .pb(px(14.))
                    .child(big)
                    .child(week)
                    .child(labels)
                    .child(legend)
                    .child(waiting)
                    .child(streak_pill),
            )
            .into_any_element()
    }
}
