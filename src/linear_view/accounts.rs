//! The Linear tab's accounts: the saved sign-ins, and adding one with a
//! personal API key.

use super::*;

impl LinearView {
    pub(super) fn sign_in(&mut self, cx: &mut Context<Self>) {
        let key = self.key_input.read(cx).text().trim().to_string();
        if key.is_empty() || self.signing_in.loading() {
            return;
        }
        self.signing_in.error = None;
        self.signing_in.start(
            cx,
            |this| &mut this.signing_in,
            async move {
                let s = linear::sign_in_with_key(&key)?;
                linear::save_session(s.clone()).map_err(ApiError::Failed)?;
                Ok::<Session, ApiError>(s)
            },
            |this, got, cx| {
                if let Ok(s) = &got {
                    // The key is saved; it must not stay in the field.
                    this.key_input.update(cx, |i, cx| i.set_text("", cx));
                    this.sessions = linear::sessions();
                    if this.current_id().as_deref() != Some(s.id.as_str()) {
                        this.current = Some(s.id.clone());
                        this.clear();
                    }
                    // A new key for the same workspace reads its list again.
                    this.read_at = None;
                    // A read still running with the old key would end as
                    // "sign-in no longer works" right after this good one,
                    // and would keep the read below from starting.
                    this.assigned.cancel();
                    this.assigned.error = None;
                    this.page = Page::Main;
                }
                let signed_in = got.is_ok();
                this.signing_in.settle(got);
                if signed_in {
                    this.read(false, cx);
                }
            },
        );
    }

    fn sign_out(&mut self, id: String, cx: &mut Context<Self>) {
        if let Err(e) = linear::remove_session(&id) {
            self.signing_in.error = Some(ApiError::Failed(e));
            cx.notify();
            return;
        }
        self.reload(cx);
        self.read(false, cx);
    }

    pub(super) fn render_accounts(&self, cx: &mut Context<Self>) -> AnyElement {
        let busy = self.signing_in.loading();
        let field = ui::field()
            .flex_1()
            .min_w_0()
            .child(icon("square-lock02", 14.).text_color(theme::text_subtle()))
            .child(self.key_input.clone());
        let first = self.sessions.is_empty();
        let mut card = div()
            .flex()
            .flex_col()
            .gap(crate::ui::sp(14.))
            .w_full()
            .max_w(px(560.))
            .p(crate::ui::sp(20.))
            .rounded(crate::ui::rad(ui::RADIUS_LG))
            .border_1()
            .border_color(theme::border())
            .bg(theme::fill_raised())
            .child(ui::heading(if first {
                "Sign in to Linear"
            } else {
                "Add a workspace"
            }));
        if first {
            card = card.child(ui::text(
                Type::Body,
                theme::text_muted(),
                "Sign in to every workspace you use; they stay signed in side by side.",
            ));
        }
        let card = card
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(crate::ui::sp(6.))
                    .child(ui::text(
                        Type::Label,
                        theme::text_subtle(),
                        "Personal API key",
                    ))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(crate::ui::sp(8.))
                            .child(field)
                            .child(
                                ui::button(
                                    "lin-sign-in",
                                    if busy { "Checking…" } else { "Sign in" },
                                    ButtonKind::Primary,
                                    !busy,
                                )
                                .when(!busy, |b| {
                                    b.on_click(cx.listener(|this, _, _, cx| this.sign_in(cx)))
                                }),
                            ),
                    )
                    .child(ui::text(
                        Type::Caption,
                        theme::text_faint(),
                        "Linear › Settings › Security & access › Personal API keys. Read access is enough.",
                    )),
            )
            .when_some(self.signing_in.error.as_ref(), |d, e| {
                let note = crate::redact::text(&e.message()).into_owned();
                d.child(ui::text(Type::Body, theme::error().into(), note))
            });

        let mut list = div()
            .flex()
            .flex_col()
            .gap(crate::ui::sp(8.))
            .max_w(px(720.))
            .child(ui::heading("Accounts"))
            .child(ui::text(
                Type::Body,
                theme::text_muted(),
                "Every sign-in stays saved on this computer. The key goes to Linear. If you let Linear start sessions on this computer, it also goes to Arakun, to prove the Linear user is you.",
            ));
        for (i, s) in self.sessions.iter().enumerate() {
            let id = s.id.clone();
            list = list.child(
                div()
                    .flex()
                    .items_center()
                    .gap(crate::ui::sp(12.))
                    .px(crate::ui::sp(12.))
                    .py(crate::ui::sp(10.))
                    .rounded(crate::ui::rad(ui::RADIUS_LG))
                    .border_1()
                    .border_color(theme::border_subtle())
                    .child(icon("user", 16.).text_color(theme::text_subtle()))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .gap(crate::ui::sp(2.))
                            .child(ui::text(
                                Type::Body,
                                theme::text(),
                                crate::redact::value(&s.workspace),
                            ))
                            .child(
                                ui::text(
                                    Type::Caption,
                                    theme::text_faint(),
                                    crate::redact::value(&s.email),
                                )
                                .truncate(),
                            ),
                    )
                    .child(
                        ui::button(("lin-sign-out", i), "Sign out", ButtonKind::Ghost, true)
                            .on_click(
                                cx.listener(move |this, _, _, cx| this.sign_out(id.clone(), cx)),
                            ),
                    ),
            );
        }
        if first {
            return div()
                .flex()
                .justify_center()
                .pt(crate::ui::sp(24.))
                .child(card)
                .into_any_element();
        }
        div()
            .flex()
            .flex_col()
            .gap(crate::ui::sp(28.))
            .child(list)
            .child(card)
            .into_any_element()
    }
}
