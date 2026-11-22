//! Experiments reported by the selected project, independent of its flags scope.

use super::overview::section;
use super::*;

impl PostHogView {
    pub(super) fn render_experiments(
        &self,
        o: &Overview,
        limit: Option<usize>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let mut list = section(
            "Experiments",
            None,
            limit
                .filter(|_| o.experiment_error.is_none())
                .map(|_| (o.experiments.len(), Tab::Experiments)),
            cx,
        );
        if let Some(e) = &o.experiment_error {
            return list.child(quiet(e.clone())).into_any_element();
        }
        if o.experiments.is_empty() {
            return list
                .child(quiet("No experiments in this project."))
                .into_any_element();
        }
        for (i, e) in o
            .experiments
            .iter()
            .take(limit.unwrap_or(usize::MAX))
            .enumerate()
        {
            let id = e.id;
            list = list.child(
                ui::labelled(
                    div().id(("ph-experiment", i)),
                    gpui::Role::Button,
                    format!("Open experiment {}", e.name),
                )
                .flex()
                .items_center()
                .gap(ui::sp(12.))
                .p(ui::sp(8.))
                .rounded(ui::rad(ui::RADIUS))
                .cursor_pointer()
                .hover(|d| d.bg(theme::fill_hover()))
                .child(ui::text(Type::Caption, theme::text_subtle(), e.status()))
                .child(
                    ui::text(Type::Body, theme::text(), e.name.clone())
                        .flex_1()
                        .truncate(),
                )
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.tab = Tab::Experiments;
                    this.inspect(Detail::Experiment(id), cx);
                    cx.notify();
                })),
            );
        }
        if o.experiments_more && limit.is_none() {
            list = list.child(
                ui::button(
                    "ph-more-experiments",
                    "More in PostHog",
                    ButtonKind::Outline,
                    true,
                )
                .on_click(cx.listener(|this, _, _, cx| this.open("/experiments", cx))),
            );
        }
        list.into_any_element()
    }
}
