//! The dialog every change to PostHog goes through: it names the account,
//! project and change, and only sends once the flag key is typed.

use super::*;
use gpui::AnimationExt as _;

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
    /// Numbers a change about to be sent, or None while another is on its way.
    pub(super) fn start_sending(&mut self) -> Option<u64> {
        if self.sending.is_some() {
            return None;
        }
        self.sent += 1;
        self.sending = Some(self.sent);
        Some(self.sent)
    }

    /// Change `seq` got its answer. When its project is no longer the one on
    /// screen, its saved overview is dropped so the next visit reads it again.
    pub(super) fn done_sending(&mut self, seq: u64, project: &Project, cx: &mut Context<Self>) {
        if self.sending == Some(seq) {
            self.sending = None;
            // A dialog open elsewhere waits on this one.
            cx.notify();
        }
        if self.current.as_ref() != Some(project) {
            posthog::forget_cached(project);
        }
    }

    /// "Project · host · account", so two accounts that see a project of the
    /// same name can't be mistaken for each other.
    pub(super) fn destination(&self, p: &Project) -> String {
        let account = self
            .session_of(p)
            .map(|s| s.email)
            .filter(|e| !e.is_empty())
            .unwrap_or_else(|| p.session.clone());
        let account = crate::redact::value(&account);
        format!("{} · {} · {account}", p.name, p.host)
    }

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
            mut lines,
            key,
            error,
            busy,
            ready,
            submit: label,
        } = ask;
        let other = self.sending.is_some() && !busy;
        if other {
            lines.push("Another change is still on its way to PostHog. Wait for it first.".into());
        }
        let ready = ready && !other;
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
                // While the change is on its way there is nothing to close:
                // the dialog stays until PostHog answers.
                if busy {
                    div().into_any_element()
                } else {
                    ui::close_button((id, 3usize))
                        .on_click(cx.listener(move |this, _, _, cx| cancel(this, cx)))
                        .into_any_element()
                },
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
            .child(ui::motion::enter(
                panel,
                (id, 3usize),
                ui::motion::From::Below,
                10.,
                ui::motion::BASE,
            ))
            .with_animation(
                (id, 4usize),
                gpui::Animation::new(ui::motion::BASE),
                |d, t| d.bg(gpui::black().opacity(0.4 * ui::motion::ease_out(t))),
            )
            .into_any_element()
    }
}
