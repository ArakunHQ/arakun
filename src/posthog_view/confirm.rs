//! The dialog every change to PostHog goes through: it names the account,
//! project and change, and only sends once the flag key is typed.

use super::*;

/// What one confirmation dialog shows.
pub(super) struct Confirm {
    pub id: &'static str,
    pub title: String,
    /// Where the change lands and what it does, one line each.
    pub lines: Vec<String>,
    pub key: String,
    pub error: Option<String>,
    pub busy: bool,
    /// The typed key matches and nothing else stands in the way.
    pub ready: bool,
    pub submit: String,
}

impl PostHogView {
    /// `cancel` runs on Cancel, the close button and Escape; `submit` on the
    /// Danger button and Enter, and must check the typed key itself.
    pub(super) fn confirm_dialog(
        &self,
        ask: Confirm,
        cancel: fn(&mut Self, &mut Context<Self>),
        submit: fn(&mut Self, &mut Context<Self>),
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let Confirm {
            id,
            title,
            lines,
            key,
            error,
            busy,
            ready,
            submit: label,
        } = ask;
        let send = ui::button(
            (id, 1usize),
            if busy {
                "Sending…".to_string()
            } else {
                label
            },
            ButtonKind::Danger,
            ready && !busy,
        )
        .when(ready && !busy, |b| {
            b.on_click(cx.listener(move |this, _, _, cx| submit(this, cx)))
        });
        let close = ui::button((id, 2usize), "Cancel", ButtonKind::Ghost, !busy).when(!busy, |b| {
            b.on_click(cx.listener(move |this, _, _, cx| cancel(this, cx)))
        });
        let panel = ui::modal_panel(id, 480., title.clone())
            .on_key_down(cx.listener(move |this, ev: &gpui::KeyDownEvent, _, cx| {
                if ev.keystroke.key == "escape" && !busy {
                    cancel(this, cx);
                }
            }))
            .child(ui::modal_header(
                title,
                None,
                ui::close_button((id, 3usize)).on_click(cx.listener(move |this, _, _, cx| {
                    if !busy {
                        cancel(this, cx);
                    }
                })),
            ))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(ui::sp(12.))
                    .p(ui::sp(16.))
                    .children(
                        lines
                            .into_iter()
                            .map(|l| ui::text(Type::Body, theme::text_muted(), l)),
                    )
                    .child(ui::text(
                        Type::Body,
                        theme::text(),
                        format!("Type {key} to confirm."),
                    ))
                    .child(ui::field().child(div().flex_1().child(self.confirm.clone())))
                    .when_some(error, |d, e| {
                        d.child(notice("alert-circle", Tone::Warning, e))
                    })
                    .child(
                        div()
                            .flex()
                            .justify_end()
                            .gap(ui::sp(8.))
                            .child(close)
                            .child(send),
                    ),
            );
        div()
            .id((id, 0usize))
            .absolute()
            .inset_0()
            .occlude()
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .p(ui::sp(12.))
            .bg(gpui::black().opacity(0.4))
            .child(panel)
            .into_any_element()
    }
}
