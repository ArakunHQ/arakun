//! The Overview tab: 24 hours of events and visitors, the feature flags
//! and the week's error issues, each with a way to its own tab.

use super::*;

/// A titled block: its name, a note, and "All N" to the tab that lists all.
pub(super) fn section(
    title: &str,
    meta: Option<String>,
    all: Option<(usize, Tab)>,
    cx: &mut Context<PostHogView>,
) -> gpui::Div {
    div().flex().flex_col().gap(ui::sp(8.)).child(
        div()
            .flex()
            .items_baseline()
            .gap(ui::sp(8.))
            .child(ui::heading(title.to_string()))
            .when_some(meta, |d, m| {
                d.child(ui::text(Type::Caption, theme::text_faint(), m))
            })
            .when_some(all, |d, (n, tab)| {
                d.child(div().flex_1()).child(
                    div()
                        .id(SharedString::from(format!("ph-all-{title}")))
                        .cursor_pointer()
                        .ty(Type::Label)
                        .text_color(theme::text_muted())
                        .hover(|s| s.text_color(theme::text()))
                        .child(format!("All {n}"))
                        .on_click(
                            cx.listener(move |this, _, window, cx| this.show_tab(tab, window, cx)),
                        ),
                )
            }),
    )
}

impl PostHogView {
    pub(super) fn render_stats(&self, o: &Overview) -> AnyElement {
        let cell = |body: gpui::Div, last: bool| {
            div()
                .flex_1()
                .min_w(px(170.))
                .px(crate::ui::sp(16.))
                .py(crate::ui::sp(14.))
                .when(!last, |d| {
                    d.border_r_1().border_color(theme::border_subtle())
                })
                .child(body)
        };
        let live_flags = o.flags.iter().filter(|f| f.active).count();
        div()
            .flex()
            .flex_wrap()
            .border_t_1()
            .border_b_1()
            .border_color(theme::border_subtle())
            .child(cell(
                ui::stat(
                    "Events · 24 h",
                    count(o.events),
                    format!("{} pageviews", count(o.pageviews)),
                ),
                false,
            ))
            .child(cell(
                ui::stat(
                    "Unique visitors · 24 h",
                    count(o.visitors),
                    "people with an event",
                ),
                false,
            ))
            .child(cell(
                ui::stat(
                    "Error issues · 7 days",
                    o.issues.len().to_string(),
                    format!(
                        "{} exceptions",
                        count(o.issues.iter().map(|i| i.count).sum())
                    ),
                ),
                false,
            ))
            .child(cell(
                ui::stat(
                    "Feature flags",
                    o.flags.len().to_string(),
                    format!("{live_flags} on"),
                ),
                true,
            ))
            .into_any_element()
    }

    pub(super) fn render_overview(&self, o: &Overview, cx: &mut Context<Self>) -> AnyElement {
        let now = crate::model::unix_now();
        let mut notes: Vec<AnyElement> = Vec::new();
        if let Some(until) = o.limited_until.filter(|t| *t > now) {
            notes.push(notice(
                "clock01",
                Tone::Info,
                format!(
                    "PostHog's hourly query budget for this account is spent. These are the numbers from {}; the tab reads again after {}.",
                    posthog::clock(o.fetched_at),
                    posthog::clock(until)
                ),
            ));
        }
        match &self.error {
            Some(ApiError::RateLimited(until)) if *until > now => notes.push(notice(
                "clock01",
                Tone::Info,
                format!("PostHog asked to wait for its query budget. Showing the saved numbers; the tab reads again after {}.", posthog::clock(*until)),
            )),
            Some(ApiError::RateLimited(_)) | None => {}
            Some(e) => notes.push(notice("alert-circle", Tone::Warning, format!("Showing the last answer. {}", e.message()))),
        }
        for p in &o.problems {
            notes.push(notice("alert-circle", Tone::Warning, p.clone()));
        }
        let traffic = section(
            "Traffic",
            Some("last 24 hours, per hour · one scale per row".into()),
            None,
            cx,
        )
        .child(if o.hourly.len() == posthog::HOURS {
            let events: Vec<_> = o.hourly.iter().map(|h| h.0).collect();
            div()
                .flex()
                .flex_col()
                .gap(ui::sp(8.))
                .child(ui::legend("Events", Tone::Info.color()))
                .child(bars::bars(&events, 82., vec![]))
                .child(ui::legend("Visitors", gpui::rgb(theme::CATEGORY[5]).into()))
                .child(self.chart.clone())
                .into_any_element()
        } else {
            quiet("No hourly numbers yet.").into_any_element()
        });
        div()
            .flex()
            .flex_col()
            .gap(crate::ui::sp(28.))
            .children(notes)
            .child(self.render_stats(o))
            .child(traffic)
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .items_start()
                    .gap(crate::ui::sp(40.))
                    .child(self.render_flags(o, Some(6), cx))
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(300.))
                            .flex()
                            .flex_col()
                            .gap(ui::sp(20.))
                            .child(self.render_experiments(o, Some(2), cx))
                            .child(self.render_issues(o, Some(3), cx)),
                    ),
            )
            .into_any_element()
    }
}
