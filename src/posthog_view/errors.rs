//! The Errors tab: the week's error tracking issues, read-only. Each opens
//! in PostHog.

use super::overview::section;
use super::*;

impl PostHogView {
    pub(super) fn render_issues(
        &self,
        o: &Overview,
        limit: Option<usize>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let now = crate::model::unix_now();
        let mut list = section(
            "Errors",
            Some("last 7 days · most people first".into()),
            limit.map(|_| (o.issues.len(), Tab::Errors)),
            cx,
        );
        if o.issues.is_empty() {
            list = list.child(quiet("No exceptions this week."));
        }
        for (i, issue) in o
            .issues
            .iter()
            .take(limit.unwrap_or(usize::MAX))
            .enumerate()
        {
            let detail = Detail::Issue(issue.id.clone());
            let title = if issue.kind.is_empty() {
                "Exception".to_string()
            } else {
                issue.kind.clone()
            };
            list = list.child(
                ui::labelled(
                    div().id(("ph-issue", i)),
                    gpui::Role::Button,
                    format!("Open {title}"),
                )
                .flex()
                .items_center()
                .gap(crate::ui::sp(12.))
                .px(crate::ui::sp(8.))
                .py(crate::ui::sp(8.))
                .rounded(crate::ui::rad(ui::RADIUS))
                .cursor_pointer()
                .hover(|d| d.bg(theme::fill_hover()))
                .child(icon("alert-circle", 14.).text_color(theme::error()))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .flex()
                        .flex_col()
                        .gap(crate::ui::sp(2.))
                        .child(
                            div()
                                .truncate()
                                .ty(Type::Body)
                                .text_color(theme::text())
                                .child(title),
                        )
                        .child(
                            div()
                                .truncate()
                                .ty(Type::Mono)
                                .text_color(theme::text_subtle())
                                .child(issue.message.clone()),
                        ),
                )
                .child(ui::text(Type::Label, theme::text(), count(issue.count)))
                .child(
                    ui::text(
                        Type::Caption,
                        theme::text_faint(),
                        format!("{} people", count(issue.users)),
                    )
                    .w(px(70.)),
                )
                .child(
                    ui::text(
                        Type::Caption,
                        theme::text_subtle(),
                        if issue.last_seen > 0 {
                            crate::memory::ago(issue.last_seen, now)
                        } else {
                            String::new()
                        },
                    )
                    .w(px(80.)),
                )
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.tab = Tab::Errors;
                    this.inspect(detail.clone(), cx);
                    cx.notify();
                })),
            );
        }
        list.flex_1().min_w(px(360.)).into_any_element()
    }
}
