//! Cards that wait on the user inside the transcript: permissions, spec plans,
//! questions, secrets and rewinds.

use super::*;

impl ThreadView {
    /// Confirmation for going back to before a prompt: what happens to the files.
    pub(super) fn rewind_card(
        &self,
        plan: &RewindPlan,
        cwd: &Path,
        cx: &Context<Self>,
    ) -> AnyElement {
        const SHOWN: usize = 8;
        let rel = |p: &str| {
            Path::new(p)
                .strip_prefix(cwd)
                .map(|r| r.display().to_string())
                .unwrap_or_else(|_| p.to_string())
        };
        let mut rows: Vec<(&'static str, String)> = plan
            .restore_paths()
            .map(|p| ("Restore", rel(p)))
            .chain(plan.delete_paths().map(|p| ("Delete", rel(p))))
            .collect();
        let hidden = rows.len().saturating_sub(SHOWN);
        rows.truncate(SHOWN);
        let summary = if plan.loading {
            "Checking which files changed since…".to_string()
        } else if plan.restore.is_empty() && plan.delete.is_empty() {
            "No files changed since; only the messages go back.".to_string()
        } else {
            "Files the agent changed since go back to how they were:".to_string()
        };
        let busy = plan.loading || plan.running;
        let button = |id: &'static str, label: &'static str, primary: bool| {
            div()
                .id(id)
                .flex()
                .items_center()
                .h(px(24.))
                .px(crate::ui::sp(10.))
                .rounded(crate::ui::rad(8.))
                .text_size(px(crate::ui::TEXT_SM))
                .map(|d| {
                    if primary {
                        d.bg(theme::content()).text_color(theme::background())
                    } else {
                        d.bg(theme::fill_selected()).text_color(theme::text_muted())
                    }
                })
                .map(|d| {
                    if busy && primary {
                        d.opacity(0.5)
                    } else if primary {
                        d.cursor_pointer().hover(|s| s.bg(theme::mix(0.8)))
                    } else {
                        d.cursor_pointer().hover(|s| s.bg(theme::mix(0.18)))
                    }
                })
                .child(label)
        };
        div()
            .id("rewind")
            .mx(px(16.))
            .mb(px(12.))
            .flex()
            .flex_col()
            .gap(crate::ui::sp(8.))
            .p(crate::ui::sp(12.))
            .rounded(crate::ui::rad(crate::ui::RADIUS_LG))
            .border_1()
            .border_color(amber().opacity(0.35))
            .bg(theme::raised())
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(crate::ui::sp(8.))
                    .child(icon("undo", 14.).text_color(amber()))
                    .child(
                        div()
                            .text_size(px(crate::ui::TEXT_MD))
                            .text_color(theme::text_muted())
                            .child("Rewind to before this message?"),
                    ),
            )
            .child(
                div()
                    .text_size(px(crate::ui::TEXT_SM))
                    .text_color(theme::text_subtle())
                    .child(format!(
                        "{summary} The rewind opens as a new session with the message back in the composer; this one stays as it is."
                    )),
            )
            .when(!rows.is_empty(), |d| {
                d.child(
                    div()
                        .flex()
                        .flex_col()
                        .px(crate::ui::sp(10.))
                        .py(crate::ui::sp(6.))
                        .rounded(crate::ui::rad(12.))
                        .bg(theme::fill_hover())
                        .font_family(theme::FONT_MONO)
                        .text_size(px(crate::ui::TEXT_SM))
                        .line_height(px(20.))
                        .children(rows.into_iter().map(|(action, path)| {
                            div()
                                .flex()
                                .gap(crate::ui::sp(8.))
                                .child(
                                    div()
                                        .w(px(56.))
                                        .flex_none()
                                        .text_color(if action == "Delete" {
                                            theme::mix(0.45)
                                        } else {
                                            theme::mix(0.6)
                                        })
                                        .child(action),
                                )
                                .child(div().min_w_0().truncate().text_color(theme::text_muted()).child(path))
                        }))
                        .when(hidden > 0, |d| {
                            d.child(div().text_color(theme::text_subtle()).child(format!("…and {hidden} more")))
                        }),
                )
            })
            .when(!plan.evicted.is_empty(), |d| {
                let names: Vec<String> = plan.evicted.iter().map(|(p, _)| rel(p)).collect();
                d.child(
                    div()
                        .text_size(px(crate::ui::TEXT_SM))
                        .text_color(amber())
                        .child(format!("Can't be restored (no snapshot): {}", names.join(", "))),
                )
            })
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(crate::ui::sp(6.))
                    .child(
                        button("rewind-go", if plan.running { "Rewinding…" } else { "Rewind" }, true)
                            .when(!busy, |d| {
                                d.on_click(cx.listener(|this, _, _, cx| {
                                    this.thread.update(cx, |t, cx| t.confirm_rewind(cx))
                                }))
                            }),
                    )
                    .when(!plan.running, |d| {
                        d.child(button("rewind-cancel", "Cancel", false).on_click(cx.listener(
                            |this, _, _, cx| this.thread.update(cx, |t, cx| t.cancel_rewind(cx)),
                        )))
                    }),
            )
            .into_any_element()
    }

    pub(super) fn permission_card(
        &self,
        p: &PermissionRequest,
        tool: Option<&ToolCall>,
        cwd: &Path,
        window: &Window,
        cx: &Context<Self>,
    ) -> AnyElement {
        let focused = self.focus.is_focused(window);
        if let Some(plan) = &p.plan {
            return self.plan_permission_card(p, plan, focused, cx);
        }
        let question = match tool {
            Some(t) => {
                let l = tool_label(t, cwd, false, true);
                if l.action.is_empty() {
                    format!("Allow {}?", l.target)
                } else {
                    format!("Allow {} {}?", l.action.to_lowercase(), l.target)
                }
            }
            None => format!("Allow {}?", p.title.replace('`', "")),
        };
        let preview: Option<AnyElement> = tool
            .and_then(|t| {
                t.content.iter().find_map(|c| match c {
                    ToolContent::Diff {
                        path,
                        old_text,
                        new_text,
                    } => Some(diff_card(
                        path,
                        old_text.as_deref(),
                        new_text,
                        cwd,
                        &Self::reveal_handle(cx),
                    )),
                    _ => None,
                })
            })
            .or_else(|| {
                let t = tool?;
                let cmd = t.raw_input.as_ref()?.get("command")?.as_str()?.to_string();
                Some(
                    Self::reveal_handle(cx)
                        .text(
                            div()
                                .id("permission-command")
                                .max_h(px(128.))
                                .overflow_hidden()
                                .px(crate::ui::sp(10.))
                                .py(crate::ui::sp(8.))
                                .rounded(crate::ui::rad(12.))
                                .bg(theme::fill_hover())
                                .font_family(theme::FONT_MONO)
                                .text_size(px(crate::ui::TEXT_SM))
                                .line_height(px(20.))
                                .text_color(theme::text_muted()),
                            &cmd,
                        )
                        .into_any_element(),
                )
            });
        let first_allow = p
            .options
            .iter()
            .position(|o| o.kind == "allow_once")
            .or_else(|| p.options.iter().position(|o| o.is_allow()));

        div()
            .id("permission")
            .mt(px(6.))
            .mb(px(6.))
            .flex()
            .flex_col()
            .gap(crate::ui::sp(10.))
            .p(crate::ui::sp(12.))
            // Raised off the sheet; the amber hairline says it waits on you.
            .rounded(crate::ui::rad(crate::ui::RADIUS_LG))
            .border_1()
            .border_color(amber().opacity(if focused { 0.45 } else { 0.22 }))
            .bg(theme::raised())
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(crate::ui::sp(8.))
                    .child(icon("shield-alert", 14.).text_color(amber()))
                    .child(
                        Self::reveal_handle(cx).text(
                            div()
                                .id("permission-question")
                                .flex_1()
                                .min_w_0()
                                .text_size(px(crate::ui::TEXT_MD))
                                .text_color(theme::text_muted()),
                            &question,
                        ),
                    ),
            )
            .when_some(preview, |d, p| d.child(p))
            .when_some(p.secret.as_deref(), |d, file| {
                d.child(
                    div()
                        .flex()
                        .items_start()
                        .gap(crate::ui::sp(8.))
                        .text_size(px(crate::ui::TEXT_SM))
                        .text_color(theme::text_muted())
                        .child(icon("square-lock02", 14.).flex_none().text_color(amber()))
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .child(crate::agent::secret_reads::why(file)),
                        ),
                )
            })
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .items_center()
                    .gap(crate::ui::sp(6.))
                    .children(p.options.iter().enumerate().map(|(i, o)| {
                        let primary = Some(i) == first_allow;
                        let id = o.id.clone();
                        div()
                            .id(("perm-opt", i))
                            .flex()
                            .items_center()
                            .gap(crate::ui::sp(6.))
                            .h(px(24.))
                            .px(crate::ui::sp(10.))
                            .rounded(crate::ui::rad(8.))
                            .text_size(px(crate::ui::TEXT_SM))
                            .cursor_pointer()
                            .map(|d| {
                                if primary {
                                    d.bg(theme::content())
                                        .text_color(theme::background())
                                        .hover(|s| s.bg(theme::mix(0.8)))
                                } else {
                                    d.bg(theme::fill_selected())
                                        .text_color(theme::text_muted())
                                        .hover(|s| s.bg(theme::mix(0.18)))
                                }
                            })
                            .child(o.name.clone())
                            .when(i < 9, |d| {
                                d.child(
                                    div()
                                        .font_family(theme::FONT_MONO)
                                        .text_size(px(crate::ui::TEXT_XS))
                                        .opacity(0.5)
                                        .child(format!("{}", i + 1)),
                                )
                            })
                            .on_click(cx.listener(move |this, _, window, cx| {
                                let id = id.clone();
                                this.thread
                                    .update(cx, |t, cx| t.respond_permission(&id, cx));
                                this.after_permission(window, cx);
                            }))
                    }))
                    .child(div().flex_1())
                    .child(
                        div()
                            .text_size(px(crate::ui::TEXT_XS))
                            .text_color(theme::text_faint())
                            .child(if focused {
                                "Enter allow · Esc deny"
                            } else {
                                "Click here to use the keyboard"
                            }),
                    ),
            )
            .on_click(cx.listener(|this, _, window, cx| {
                window.focus(&this.focus, cx);
                cx.notify();
            }))
            .into_any_element()
    }

    /// Spec mode's exit request: the plan, then Approve / Keep planning.
    pub(super) fn plan_permission_card(
        &self,
        p: &PermissionRequest,
        plan: &crate::agent::thread::SpecPlan,
        focused: bool,
        cx: &Context<Self>,
    ) -> AnyElement {
        let blocks = self.plan_blocks(&plan.plan);
        let primary_id = p
            .options
            .iter()
            .find(|o| o.kind == "allow_once")
            .or_else(|| p.options.iter().find(|o| o.is_allow()))
            .map(|o| o.id.clone())
            .unwrap_or_default();
        let title = if plan.title.trim().is_empty() {
            "Ready to implement this plan?".to_string()
        } else {
            plan.title.clone()
        };
        div()
            .id("spec-plan")
            .mt(px(6.))
            .mb(px(6.))
            .flex()
            .flex_col()
            // Raised off the sheet; the amber hairline says it waits on you.
            .rounded(crate::ui::rad(crate::ui::RADIUS_LG))
            .border_1()
            .border_color(amber().opacity(if focused { 0.45 } else { 0.22 }))
            .bg(theme::raised())
            .overflow_hidden()
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(crate::ui::sp(8.))
                    .px(crate::ui::sp(12.))
                    .py(crate::ui::sp(8.))
                    .child(icon("note01", 14.).text_color(amber()))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_size(px(crate::ui::TEXT_MD))
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(theme::text_muted())
                            .child(title),
                    )
                    .child(
                        div()
                            .text_size(px(crate::ui::TEXT_XS))
                            .text_color(theme::text_subtle())
                            .child(
                                if self.thread.read(cx).kind() == crate::agent::AgentKind::Droid {
                                    "Spec"
                                } else {
                                    "Plan"
                                },
                            ),
                    ),
            )
            .child(
                div()
                    .id("spec-plan-body")
                    .max_h(px(360.))
                    .overflow_y_scroll()
                    .px(crate::ui::sp(12.))
                    .py(crate::ui::sp(4.))
                    .child(self.surface_of(PLAN_SURFACE, &blocks, MdStyle::reply(), cx)),
            )
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .items_center()
                    .gap(crate::ui::sp(6.))
                    .px(crate::ui::sp(12.))
                    .py(crate::ui::sp(10.))
                    .children(p.options.iter().enumerate().map(|(i, o)| {
                        // The one Enter picks (see `Thread::allow_permission`).
                        let primary = o.id == primary_id;
                        let id = o.id.clone();
                        div()
                            .id(("plan-opt", i))
                            .flex()
                            .items_center()
                            .gap(crate::ui::sp(6.))
                            .h(px(24.))
                            .px(crate::ui::sp(10.))
                            .rounded(crate::ui::rad(8.))
                            .text_size(px(crate::ui::TEXT_SM))
                            .cursor_pointer()
                            .map(|d| {
                                if primary {
                                    d.bg(theme::content())
                                        .text_color(theme::background())
                                        .hover(|s| s.bg(theme::mix(0.8)))
                                } else {
                                    d.bg(theme::fill_selected())
                                        .text_color(theme::text_muted())
                                        .hover(|s| s.bg(theme::mix(0.18)))
                                }
                            })
                            .child(o.name.clone())
                            .when(i < 9, |d| {
                                d.child(
                                    div()
                                        .font_family(theme::FONT_MONO)
                                        .text_size(px(crate::ui::TEXT_XS))
                                        .opacity(0.5)
                                        .child(format!("{}", i + 1)),
                                )
                            })
                            .on_click(cx.listener(move |this, _, window, cx| {
                                let id = id.clone();
                                this.thread
                                    .update(cx, |t, cx| t.respond_permission(&id, cx));
                                this.after_permission(window, cx);
                            }))
                    }))
                    .child(div().flex_1())
                    .child(
                        div()
                            .text_size(px(crate::ui::TEXT_XS))
                            .text_color(theme::text_faint())
                            .child(if focused {
                                "Enter approve · Esc keep planning"
                            } else {
                                "Click here to use the keyboard"
                            }),
                    ),
            )
            .on_click(cx.listener(|this, _, window, cx| {
                window.focus(&this.focus, cx);
                cx.notify();
            }))
            .into_any_element()
    }

    /// AskUser: each question with its options as toggles plus a free-text field.
    pub(super) fn question_card(&self, req: &AskUserRequest, cx: &Context<Self>) -> AnyElement {
        let ready = self.answers(req, cx).is_some();
        let ask = self.ask.as_ref();
        div()
            .id("ask-user")
            .mt(px(6.))
            .mb(px(6.))
            .flex()
            .flex_col()
            .gap(crate::ui::sp(12.))
            .p(crate::ui::sp(12.))
            .rounded(crate::ui::rad(crate::ui::RADIUS_LG))
            .border_1()
            .border_color(amber().opacity(0.35))
            .bg(theme::raised())
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .children(req.questions.iter().enumerate().map(|(qi, q)| {
                let picked: &[usize] = ask
                    .and_then(|a| a.picked.get(qi))
                    .map_or(&[], Vec::as_slice);
                div()
                    .flex()
                    .flex_col()
                    .gap(crate::ui::sp(8.))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(crate::ui::sp(8.))
                            .child(icon("help-circle", 14.).text_color(amber()))
                            .when(!q.topic.is_empty(), |d| {
                                d.child(
                                    div()
                                        .flex_none()
                                        .px(crate::ui::sp(6.))
                                        .rounded(crate::ui::rad(4.))
                                        .bg(theme::fill_hover())
                                        .text_size(px(crate::ui::TEXT_XS))
                                        .text_color(theme::text_subtle())
                                        .child(crate::redact::chat_text(&q.topic).0.into_owned()),
                                )
                            })
                            .child(
                                Self::reveal_handle(cx).text(
                                    div()
                                        .id(("ask-question", qi))
                                        .flex_1()
                                        .min_w_0()
                                        .text_size(px(crate::ui::TEXT_MD))
                                        .text_color(theme::text_muted()),
                                    &q.question,
                                ),
                            )
                            .when(q.multi, |d| {
                                d.child(
                                    div()
                                        .flex_none()
                                        .text_size(px(crate::ui::TEXT_XS))
                                        .text_color(theme::text_faint())
                                        .child("Pick any"),
                                )
                            }),
                    )
                    .when(!q.options.is_empty(), |d| {
                        d.child(
                            div()
                                .flex()
                                .flex_wrap()
                                .gap(crate::ui::sp(6.))
                                .pl(crate::ui::sp(22.))
                                .children(q.options.iter().enumerate().map(|(oi, label)| {
                                    let on = picked.contains(&oi);
                                    let multi = q.multi;
                                    div()
                                        .id(("ask-opt", qi * 100 + oi))
                                        .flex()
                                        .items_center()
                                        .gap(crate::ui::sp(6.))
                                        .h(px(24.))
                                        .px(crate::ui::sp(10.))
                                        .rounded(crate::ui::rad(8.))
                                        .text_size(px(crate::ui::TEXT_SM))
                                        .cursor_pointer()
                                        .map(|d| {
                                            if on {
                                                d.bg(theme::content())
                                                    .text_color(theme::background())
                                                    .hover(|s| s.bg(theme::mix(0.8)))
                                            } else {
                                                d.bg(theme::fill_selected())
                                                    .text_color(theme::text_muted())
                                                    .hover(|s| s.bg(theme::mix(0.18)))
                                            }
                                        })
                                        .when(on, |d| {
                                            d.child(
                                                icon("tick02", 12.).text_color(theme::background()),
                                            )
                                        })
                                        // A click picks the option, so a secret in
                                        // it stays hidden.
                                        .child(crate::redact::chat_text(label).0.into_owned())
                                        .on_click(cx.listener(move |this, _, _, cx| {
                                            this.toggle_pick(qi, oi, multi, cx)
                                        }))
                                })),
                        )
                    })
                    .when_some(ask.and_then(|a| a.inputs.get(qi)).cloned(), |d, input| {
                        if q.secret {
                            let shown = ask
                                .and_then(|a| a.revealed.get(qi))
                                .copied()
                                .unwrap_or(false);
                            return d.child(div().ml(px(22.)).child(masked_field(
                                ("ask-reveal", qi),
                                Some(input),
                                shown,
                                cx.listener(move |this, _, _, cx| this.toggle_ask_reveal(qi, cx)),
                            )));
                        }
                        d.child(
                            div()
                                .ml(px(22.))
                                .h(px(28.))
                                .flex()
                                .items_center()
                                .px(crate::ui::sp(8.))
                                .rounded(crate::ui::rad(8.))
                                .border_1()
                                .border_color(theme::border_strong())
                                .bg(theme::fill_raised())
                                .text_size(px(crate::ui::TEXT_MD))
                                .child(input),
                        )
                    })
            }))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(crate::ui::sp(6.))
                    .pl(crate::ui::sp(22.))
                    .child(
                        div()
                            .id("ask-submit")
                            .flex()
                            .items_center()
                            .h(px(24.))
                            .px(crate::ui::sp(10.))
                            .rounded(crate::ui::rad(8.))
                            .text_size(px(crate::ui::TEXT_SM))
                            .bg(theme::content())
                            .text_color(theme::background())
                            .when(ready, |d| {
                                d.cursor_pointer().hover(|s| s.bg(theme::mix(0.8)))
                            })
                            .when(!ready, |d| d.opacity(0.4))
                            .child("Answer")
                            .when(ready, |d| {
                                d.on_click(cx.listener(|this, _, _, cx| this.submit_answers(cx)))
                            }),
                    )
                    .child(
                        div()
                            .id("ask-skip")
                            .flex()
                            .items_center()
                            .h(px(24.))
                            .px(crate::ui::sp(10.))
                            .rounded(crate::ui::rad(8.))
                            .text_size(px(crate::ui::TEXT_SM))
                            .cursor_pointer()
                            .bg(theme::fill_selected())
                            .text_color(theme::text_muted())
                            .hover(|s| s.bg(theme::mix(0.18)))
                            .child("Skip")
                            .on_click(cx.listener(|this, _, _, cx| {
                                // A server's question keeps its answers until
                                // the server says it is gone.
                                if !this.thread.read(cx).is_remote() {
                                    this.ask = None;
                                }
                                this.thread.update(cx, |t, cx| t.skip_question(cx));
                                this.remeasure_last();
                                cx.notify();
                            })),
                    )
                    .child(div().flex_1())
                    .child(
                        div()
                            .text_size(px(crate::ui::TEXT_XS))
                            .text_color(theme::text_faint())
                            .child("Enter in a field sends the answers"),
                    ),
            )
            .into_any_element()
    }
}

impl ThreadView {
    /// `request_secret`: masked inputs the agent never sees, with Save and
    /// Decline. What is typed goes to `secret::keep`, and the agent gets a
    /// handle, or to `secret::file` after the card said so, and it gets a path.
    pub(super) fn secret_card(&self, req: &SecretRequest, cx: &Context<Self>) -> AnyElement {
        // A server reached over plain http: only Decline, nothing to type.
        let private = self.thread.read(cx).secret_private();
        let ready = private && self.secret_ready(cx);
        let state = self.secret.as_ref().filter(|s| s.id == req.id);
        let error = state.and_then(|s| s.error.clone());
        let button = |id: &'static str, label: &'static str, primary: bool, on: bool| {
            crate::ui::labelled(div().id(id), gpui::Role::Button, label)
                .flex()
                .items_center()
                .h(px(24.))
                .px(crate::ui::sp(10.))
                .rounded(crate::ui::rad(8.))
                .text_size(px(crate::ui::TEXT_SM))
                .map(|d| {
                    if primary {
                        d.bg(theme::content()).text_color(theme::background())
                    } else {
                        d.bg(theme::fill_selected()).text_color(theme::text_muted())
                    }
                })
                .when(on, |d| {
                    d.cursor_pointer().hover(move |s| {
                        s.bg(if primary {
                            theme::mix(0.8)
                        } else {
                            theme::mix(0.18)
                        })
                    })
                })
                .when(!on, |d| d.opacity(0.4))
                .child(label)
        };
        div()
            .id("secret-card")
            .mt(px(6.))
            .mb(px(6.))
            .flex()
            .flex_col()
            .gap(crate::ui::sp(10.))
            .p(crate::ui::sp(12.))
            .rounded(crate::ui::rad(crate::ui::RADIUS_LG))
            .border_1()
            .border_color(amber().opacity(0.35))
            .bg(theme::raised())
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(crate::ui::sp(8.))
                    .child(icon("square-lock02", 14.).text_color(amber()))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_size(px(crate::ui::TEXT_MD))
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(theme::text_muted())
                            .child(req.label.clone()),
                    )
                    .child(
                        div()
                            .flex_none()
                            .text_size(px(crate::ui::TEXT_XS))
                            .text_color(theme::text_faint())
                            .child("Stays out of the transcript"),
                    ),
            )
            .child(
                div()
                    .pl(crate::ui::sp(22.))
                    .text_size(px(crate::ui::TEXT_SM))
                    .text_color(theme::text_subtle())
                    .child(req.reason.clone()),
            )
            .when_some(req.url.clone(), |d, url| {
                d.child(
                    crate::ui::labelled(div().id("secret-url"), gpui::Role::Link, url.clone())
                        .ml(crate::ui::sp(22.))
                        .flex()
                        .items_center()
                        .gap(crate::ui::sp(4.))
                        .text_size(px(crate::ui::TEXT_SM))
                        .text_color(theme::link())
                        .cursor_pointer()
                        .hover(|s| s.underline())
                        .child(icon("link-square02", 12.).text_color(theme::link()))
                        .child(url.clone())
                        .on_click(move |_, _, cx| cx.open_url(&url)),
                )
            })
            .when(req.deliver == crate::agent::secret::Deliver::File, |d| {
                d.child(
                    div()
                        .pl(crate::ui::sp(22.))
                        .text_size(px(crate::ui::TEXT_SM))
                        .text_color(amber())
                        .child(crate::agent::secret::FILE_NOTICE),
                )
            })
            .when(!private, |d| {
                d.child(
                    div()
                        .pl(crate::ui::sp(22.))
                        .text_size(px(crate::ui::TEXT_SM))
                        .text_color(theme::danger())
                        .child(crate::agent::secret::NOT_PRIVATE),
                )
            })
            .children(
                req.fields
                    .iter()
                    .enumerate()
                    .filter(|_| private)
                    .map(|(i, field)| {
                        let input = state.and_then(|s| s.inputs.get(i)).cloned();
                        let shown = state
                            .and_then(|s| s.revealed.get(i))
                            .copied()
                            .unwrap_or(false);
                        div()
                            .pl(crate::ui::sp(22.))
                            .flex()
                            .flex_col()
                            .gap(crate::ui::sp(4.))
                            .child(
                                div()
                                    .text_size(px(crate::ui::TEXT_XS))
                                    .text_color(theme::text_subtle())
                                    .child(field.label.clone()),
                            )
                            .child(masked_field(
                                ("secret-reveal", i),
                                input,
                                shown,
                                cx.listener(move |this, _, _, cx| this.toggle_reveal(i, cx)),
                            ))
                    }),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(crate::ui::sp(6.))
                    .pl(crate::ui::sp(22.))
                    .child(button("secret-save", "Save", true, ready).when(ready, |d| {
                        d.on_click(cx.listener(|this, _, _, cx| this.save_secret(cx)))
                    }))
                    .child(
                        button("secret-decline", "Decline", false, true)
                            .on_click(cx.listener(|this, _, _, cx| this.decline_secret(cx))),
                    )
                    .child(div().flex_1())
                    .child(div().text_size(px(crate::ui::TEXT_XS)).map(|d| {
                        match &error {
                            Some(e) => d.text_color(theme::danger()).child(e.clone()),
                            None if !private => d,
                            None => d
                                .text_color(theme::text_faint())
                                .child("The agent gets a handle, never the value"),
                        }
                    })),
            )
            .into_any_element()
    }
}

/// The agent's plan as a checklist, with how many entries are done.
pub(super) fn plan_card(entries: &[PlanEntry]) -> AnyElement {
    let done = entries
        .iter()
        .filter(|e| e.status == PlanStatus::Completed)
        .count();
    div()
        .flex()
        .flex_col()
        .rounded(crate::ui::rad(crate::ui::RADIUS_LG))
        .bg(theme::raised())
        .overflow_hidden()
        .child(
            div()
                .flex()
                .items_center()
                .gap(crate::ui::sp(8.))
                .px(crate::ui::sp(10.))
                .py(crate::ui::sp(8.))
                .child(icon("list-end", 16.).text_color(theme::text_subtle()))
                .child(
                    div()
                        .flex_1()
                        .font_family(theme::FONT_MONO)
                        .text_size(px(crate::ui::TEXT_SM))
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(theme::text_muted())
                        .child("Tasks"),
                )
                .child(
                    div()
                        .px(crate::ui::sp(8.))
                        .py(crate::ui::sp(1.))
                        .rounded_full()
                        .bg(theme::fill_hover())
                        .font_family(theme::FONT_MONO)
                        .text_size(px(crate::ui::TEXT_XS))
                        .text_color(theme::text_subtle())
                        .child(format!("{done}/{}", entries.len())),
                ),
        )
        .child(
            div()
                .flex()
                .flex_col()
                .py(crate::ui::sp(4.))
                .children(entries.iter().map(|e| {
                    let (color, strike) = match e.status {
                        PlanStatus::Completed => (theme::mix(0.4), true),
                        PlanStatus::InProgress => (theme::mix(0.85), false),
                        PlanStatus::Pending | PlanStatus::Unknown => (theme::mix(0.6), false),
                    };
                    let state: AnyElement = match e.status {
                        PlanStatus::Completed => div()
                            .size(px(16.))
                            .flex()
                            .items_center()
                            .justify_center()
                            .rounded_full()
                            .bg(Hsla::from(theme::success()).opacity(0.2))
                            .child(icon("tick02", 10.).text_color(theme::success()))
                            .into_any_element(),
                        PlanStatus::InProgress => icon("loading03", 16.)
                            .text_color(theme::link())
                            .into_any_element(),
                        PlanStatus::Pending | PlanStatus::Unknown => div()
                            .size(px(16.))
                            .rounded_full()
                            .border_1()
                            .border_color(theme::mix(0.25))
                            .into_any_element(),
                    };
                    div()
                        .flex()
                        .items_start()
                        .gap(crate::ui::sp(10.))
                        .px(crate::ui::sp(10.))
                        .py(crate::ui::sp(6.))
                        .child(div().flex_none().pt(crate::ui::sp(1.)).child(state))
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .text_size(px(crate::ui::TEXT_MD))
                                .line_height(px(18.))
                                .text_color(color)
                                .when(strike, |d| d.line_through())
                                .child(e.content.clone()),
                        )
                })),
        )
        .into_any_element()
}

/// A masked input with a Show/Hide toggle, for a secret the user types: the
/// secret card's fields and a question's password field.
fn masked_field(
    id: impl Into<gpui::ElementId>,
    input: Option<Entity<TextInput>>,
    shown: bool,
    on_toggle: impl Fn(&gpui::ClickEvent, &mut Window, &mut App) + 'static,
) -> Div {
    div()
        .h(px(28.))
        .flex()
        .items_center()
        .gap(crate::ui::sp(6.))
        .px(crate::ui::sp(8.))
        .rounded(crate::ui::rad(8.))
        .border_1()
        .border_color(theme::border_strong())
        .bg(theme::fill_raised())
        .text_size(px(crate::ui::TEXT_MD))
        .child(div().flex_1().min_w_0().children(input))
        .child(
            crate::ui::labelled(
                div().id(id),
                gpui::Role::Button,
                if shown { "Hide" } else { "Show" },
            )
            .group("secret-reveal")
            .flex_none()
            .cursor_pointer()
            .child(hover_icon(
                if shown { "view-off" } else { "view" },
                14.,
                theme::text_faint(),
                theme::text_muted(),
                "secret-reveal",
            ))
            .on_click(on_toggle),
        )
}
