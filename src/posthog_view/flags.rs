//! The Flags tab: every feature flag of the project, live first, read-only.
//! Changing a flag comes in a later version.

use super::overview::section;
use super::*;

impl PostHogView {
    pub(super) fn render_flags(
        &self,
        o: &Overview,
        limit: Option<usize>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let shown = limit.unwrap_or(o.flags.len());
        let mut list = section(
            "Feature flags",
            Some(format!("{} · live first", o.flags.len())),
            limit.map(|_| (o.flags.len(), Tab::Flags)),
            cx,
        );
        if o.flags.is_empty() {
            list = list.child(quiet("No feature flags in this project."));
        }
        for (i, f) in o.flags.iter().take(shown).enumerate() {
            let path = format!("/feature_flags/{}", f.id);
            list = list.child(
                div()
                    .id(("ph-flag", i))
                    .flex()
                    .items_center()
                    .gap(crate::ui::sp(10.))
                    .px(crate::ui::sp(8.))
                    .py(crate::ui::sp(6.))
                    .rounded(crate::ui::rad(ui::RADIUS))
                    .cursor_pointer()
                    .hover(|d| d.bg(theme::fill_hover()))
                    .when(!f.active, |d| d.opacity(0.6))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .child(
                                div()
                                    .truncate()
                                    .ty(Type::Mono)
                                    .text_color(theme::text())
                                    .child(f.key.clone()),
                            )
                            .when(!f.name.is_empty() && f.name != f.key, |d| {
                                d.child(
                                    div()
                                        .truncate()
                                        .ty(Type::Caption)
                                        .text_color(theme::text_faint())
                                        .child(f.name.clone()),
                                )
                            }),
                    )
                    .child(
                        ui::text(Type::Caption, theme::text_subtle(), f.rollout.clone())
                            .w(px(120.)),
                    )
                    .child(ui::switch(f.active))
                    .on_click(cx.listener(move |this, _, _, cx| this.open(&path, cx))),
            );
        }
        list.flex_1().min_w(px(360.)).into_any_element()
    }
}
