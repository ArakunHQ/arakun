//! The Doop tab's accounts: the saved sign-ins side by side, adding one (in
//! the browser, or with an agent key) on Doop or a self-hosted instance, and
//! connecting the shown instance's MCP server to the agents.

use std::sync::atomic::Ordering;

use super::*;

impl DoopView {
    /// The instance typed in the form, or why it can't be one.
    fn typed_host(&mut self, cx: &mut Context<Self>) -> Option<String> {
        match doop::normalize_host(self.host_input.read(cx).text()) {
            Ok(host) => Some(host),
            Err(e) => {
                self.connect_note = Some((e, true));
                cx.notify();
                None
            }
        }
    }

    /// Runs a sign-in off the UI thread, saves it and shows it.
    fn connect(
        &mut self,
        cancel: Arc<AtomicBool>,
        sign_in: impl FnOnce() -> Result<Session, Error> + Send + 'static,
        cx: &mut Context<Self>,
    ) {
        self.connecting = Some(cancel);
        self.connect_note = None;
        cx.notify();
        self._connect = Some(cx.spawn(async move |this, cx| {
            let got = cx
                .background_spawn(crate::guard::task(async move {
                    let s = sign_in().map_err(|e| e.message())?;
                    doop::save_session(s.clone())?;
                    Ok::<Session, String>(s)
                }))
                .await;
            this.update(cx, |this, cx| {
                this.connecting = None;
                match got {
                    Ok(s) => this.signed_in(&s, cx),
                    Err(e) => this.connect_note = Some((e, true)),
                }
                cx.notify();
            })
            .ok();
        }));
    }

    /// Signs in to the typed instance in the browser.
    pub(super) fn connect_browser(&mut self, cx: &mut Context<Self>) {
        if self.connecting.is_some() {
            return;
        }
        let Some(host) = self.typed_host(cx) else {
            return;
        };
        let cancel = Arc::new(AtomicBool::new(false));
        let flag = cancel.clone();
        self.connect(
            cancel,
            // The system's browser, where the user is already signed in to Doop.
            move || doop::oauth::sign_in(&host, &flag, |url| crate::platform::open(url)),
            cx,
        );
    }

    /// Checks the pasted agent key against the typed instance.
    pub(super) fn connect_key(&mut self, cx: &mut Context<Self>) {
        let key = self.key_input.read(cx).text().trim().to_string();
        if key.is_empty() || self.connecting.is_some() {
            return;
        }
        let Some(host) = self.typed_host(cx) else {
            return;
        };
        self.connect(
            Arc::new(AtomicBool::new(false)),
            move || doop::sign_in_with_key(&host, &key),
            cx,
        );
    }

    /// Stops a sign-in that waits on the browser.
    fn cancel_connect(&mut self, cx: &mut Context<Self>) {
        if let Some(flag) = &self.connecting {
            flag.store(true, Ordering::Relaxed);
        }
        cx.notify();
    }

    /// After a sign-in was saved: show its instance.
    fn signed_in(&mut self, s: &Session, cx: &mut Context<Self>) {
        self.sessions = doop::sessions();
        self.connect_note = Some((
            format!("Signed in to {}.", doop::host_label(&s.host)),
            false,
        ));
        self.key_input.update(cx, |i, cx| i.set_text("", cx));
        self.host_input.update(cx, |i, cx| i.set_text("", cx));
        self.page = Page::Main;
        self.switch(Some(s.id.clone()), cx);
    }

    fn remove(&mut self, id: String, cx: &mut Context<Self>) {
        if let Err(e) = doop::remove_session(&id) {
            self.connect_note = Some((e, true));
        }
        self.sessions = doop::sessions();
        if self.current.as_ref() == Some(&id) {
            let next = self.sessions.first().map(|s| s.id.clone());
            self.switch(next, cx);
        }
        cx.notify();
    }

    /// The form that adds a sign-in. `first`: nothing is saved yet.
    pub(super) fn render_connect(&self, first: bool, cx: &mut Context<Self>) -> AnyElement {
        let busy = self.connecting.is_some();
        let label = |text: &'static str| ui::text(Type::Label, theme::text_subtle(), text);
        let host = ui::field()
            .flex_1()
            .min_w_0()
            .child(icon("internet", 14.).text_color(theme::text_subtle()))
            .child(self.host_input.clone());
        let key = ui::field()
            .flex_1()
            .min_w_0()
            .child(icon("square-lock02", 14.).text_color(theme::text_subtle()))
            .child(self.key_input.clone());
        let at_hand = (!busy && self.key_input.read(cx).text().is_empty())
            .then(doop::key_at_hand)
            .flatten();
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
            .bg(theme::fill_raised());
        if first {
            card = card.child(ui::heading("Sign in to Doop")).child(ui::text(
                Type::Body,
                theme::text_muted(),
                "See your canvases and their frames here, and let your agents read and draw on them. Works with Doop and with an instance you host yourself.",
            ));
        } else {
            card = card.child(ui::heading("Add an instance or an account"));
        }
        card.child(
            div()
                .flex()
                .flex_col()
                .gap(crate::ui::sp(6.))
                .child(label("Instance"))
                .child(host)
                .child(ui::text(
                    Type::Caption,
                    theme::text_faint(),
                    "Leave it empty for doop.arakun.dev. For a self-hosted instance, type its address.",
                )),
        )
        .child(
            div()
                .flex()
                .items_center()
                .gap(crate::ui::sp(8.))
                .child(
                    ui::button(
                        "doop-browser",
                        if busy { "Waiting…" } else { "Sign in with the browser" },
                        ButtonKind::Primary,
                        !busy,
                    )
                    .when(!busy, |b| {
                        b.on_click(cx.listener(|this, _, _, cx| this.connect_browser(cx)))
                    }),
                )
                .when(busy, |d| {
                    d.child(
                        ui::button("doop-cancel", "Cancel", ButtonKind::Ghost, true)
                            .on_click(cx.listener(|this, _, _, cx| this.cancel_connect(cx))),
                    )
                })
                .child(ui::text(
                    Type::Caption,
                    theme::text_faint(),
                    if busy {
                        "Approve Arakun in the browser page that opened."
                    } else {
                        "Opens the instance's sign-in page."
                    },
                )),
        )
        .child(
            div()
                .flex()
                .flex_col()
                .gap(crate::ui::sp(6.))
                .child(label("Or an agent key"))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(crate::ui::sp(8.))
                        .child(key)
                        .child(
                            ui::button("doop-key-save", "Add", ButtonKind::Outline, !busy).when(
                                !busy,
                                |b| b.on_click(cx.listener(|this, _, _, cx| this.connect_key(cx))),
                            ),
                        ),
                )
                .child(ui::text(
                    Type::Caption,
                    theme::text_faint(),
                    "For an instance without a sign-in page. Saved on this computer only.",
                ))
                .children(at_hand.map(|k| {
                    ui::button(
                        "doop-key-found",
                        "Use the key in DOOP_KEY or ~/.config/arakun/doop_key",
                        ButtonKind::Ghost,
                        true,
                    )
                    .on_click(cx.listener(move |this, _, _, cx| {
                        let k = k.clone();
                        this.key_input.update(cx, |i, cx| i.set_text(k, cx));
                        cx.notify();
                    }))
                })),
        )
        .when_some(self.connect_note.clone(), |d, (note, error)| {
            let note = crate::redact::text(&note).into_owned();
            d.child(ui::text(
                Type::Body,
                if error {
                    theme::error().into()
                } else {
                    theme::text_muted()
                },
                note,
            ))
        })
        .into_any_element()
    }

    /// The shown instance's MCP server in each agent: there, or a button that adds it.
    fn render_agents(&self, s: &Session, cx: &mut Context<Self>) -> AnyElement {
        let mut card = div()
            .flex()
            .flex_col()
            .gap(crate::ui::sp(12.))
            .max_w(px(720.))
            .child(ui::heading(format!(
                "Agents on {}",
                doop::host_label(&s.host)
            )))
            .child(ui::text(
                Type::Body,
                theme::text_muted(),
                format!(
                    "Connect this instance's MCP server ({}) so sessions can read your canvases, tokens and style guides, and draw frames. Each agent signs in by itself in the browser.",
                    doop::mcp_url(&s.host)
                ),
            ));
        for (i, (agent, server)) in self.agents.iter().enumerate() {
            let name = agent.label();
            let Some(command) = doop::connect_command(*agent, &s.host, server.is_some()) else {
                continue;
            };
            let state: AnyElement = match server {
                Some(server) => div()
                    .flex()
                    .items_center()
                    .gap(crate::ui::sp(6.))
                    .child(icon("checkmark-circle02", 14.).text_color(theme::success()))
                    .child(ui::text(
                        Type::Caption,
                        theme::text_faint(),
                        format!("added as {server}"),
                    ))
                    .into_any_element(),
                None => {
                    ui::text(Type::Caption, theme::text_faint(), "not connected").into_any_element()
                }
            };
            card =
                card.child(
                    div()
                        .flex()
                        .items_center()
                        .gap(crate::ui::sp(12.))
                        .px(crate::ui::sp(12.))
                        .py(crate::ui::sp(10.))
                        .rounded(crate::ui::rad(ui::RADIUS_LG))
                        .border_1()
                        .border_color(theme::border_subtle())
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .flex()
                                .flex_col()
                                .gap(crate::ui::sp(2.))
                                .child(ui::text(Type::Body, theme::text(), name))
                                .child(state),
                        )
                        .child(
                            ui::button(
                                ("doop-agent", i),
                                if server.is_some() {
                                    "Sign in"
                                } else {
                                    "Connect"
                                },
                                ButtonKind::Outline,
                                true,
                            )
                            .on_click(cx.listener(
                                move |_, _, _, cx| cx.emit(DoopEvent::Run(command.clone())),
                            )),
                        ),
                );
        }
        card.into_any_element()
    }

    pub(super) fn render_accounts(&self, cx: &mut Context<Self>) -> AnyElement {
        let mut list = div()
            .flex()
            .flex_col()
            .gap(crate::ui::sp(8.))
            .max_w(px(720.))
            .child(ui::heading("Accounts"))
            .child(ui::text(
                Type::Body,
                theme::text_muted(),
                "Every sign-in stays saved, one for each instance and account. The tab shows the one picked in its header.",
            ));
        for (i, s) in self.sessions.iter().enumerate() {
            let id = s.id.clone();
            let shown = self.current.as_ref() == Some(&s.id);
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
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap(crate::ui::sp(8.))
                                    .child(ui::text(
                                        Type::Body,
                                        theme::text(),
                                        crate::redact::value(&s.who()),
                                    ))
                                    .when(s.self_hosted(), |d| {
                                        d.child(ui::badge("self-hosted", Tone::Neutral))
                                    })
                                    .when(shown, |d| d.child(ui::badge("shown", Tone::Accent))),
                            )
                            .child(
                                ui::text(
                                    Type::Caption,
                                    theme::text_faint(),
                                    format!(
                                        "{} · {}",
                                        doop::host_label(&s.host),
                                        s.credential.label()
                                    ),
                                )
                                .truncate(),
                            ),
                    )
                    .child(
                        ui::button(("doop-remove", i), "Remove", ButtonKind::Ghost, true).on_click(
                            cx.listener(move |this, _, _, cx| this.remove(id.clone(), cx)),
                        ),
                    ),
            );
        }
        if let Some(Error::SignedOut(why)) = &self.error {
            list = list.child(notice("alert-circle", Tone::Warning, why.clone()));
        }
        div()
            .flex()
            .flex_col()
            .gap(crate::ui::sp(28.))
            .child(list)
            .children(self.session().map(|s| self.render_agents(s, cx)))
            .child(self.render_connect(false, cx))
            .into_any_element()
    }
}
