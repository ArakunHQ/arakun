//! What sits on the composer: the session's footer, the queue and the banners above it.

use super::*;

impl ThreadView {
    pub(super) fn render_footer(&self, cx: &Context<Self>) -> AnyElement {
        let t = self.thread.read(cx);
        match t.status() {
            ThreadStatus::Connecting => div()
                .flex()
                .items_center()
                .gap(crate::ui::sp(8.))
                .px(crate::ui::sp(16.))
                .py(crate::ui::sp(10.))
                .text_size(px(crate::ui::TEXT_MD))
                .text_color(theme::text_subtle())
                .child(spinner(self.phase, 14., theme::mix(0.45)))
                .child(
                    t.status_detail()
                        .unwrap_or_else(|| format!("Starting {}…", t.kind().label()).into()),
                )
                .into_any_element(),
            ThreadStatus::Error(_) if t.needs_install().is_some() => self.render_install(cx),
            ThreadStatus::Error(message) => {
                let mut lines = message.lines();
                let headline = lines.next().unwrap_or("").to_string();
                let rest: Vec<&str> = lines.collect();
                let log = launch::log_path(t.kind());
                let needs_login = t.needs_login();
                div()
                    .px(crate::ui::sp(16.))
                    .pt(crate::ui::sp(8.))
                    .pb(crate::ui::sp(40.))
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap(crate::ui::sp(8.))
                            .p(crate::ui::sp(12.))
                            .rounded(crate::ui::rad(crate::ui::RADIUS_LG))
                            .border_1()
                            .border_color(red().opacity(0.3))
                            .bg(red().opacity(0.06))
                            .child(
                                div()
                                    .flex()
                                    .items_start()
                                    .gap(crate::ui::sp(8.))
                                    .child(
                                        div()
                                            .pt(crate::ui::sp(3.))
                                            .child(icon("alert-circle", 14.).text_color(red())),
                                    )
                                    .child(
                                        div()
                                            .flex_1()
                                            .min_w_0()
                                            .text_size(px(crate::ui::TEXT_MD))
                                            .line_height(px(20.))
                                            .text_color(theme::text_muted())
                                            .child(headline),
                                    ),
                            )
                            .when(!rest.is_empty(), |d| {
                                d.child(
                                    div()
                                        .pl(crate::ui::sp(22.))
                                        .font_family(theme::FONT_MONO)
                                        .text_size(px(crate::ui::TEXT_XS))
                                        .line_height(px(17.))
                                        .text_color(theme::text_subtle())
                                        .child(rest.join("\n")),
                                )
                            })
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap(crate::ui::sp(8.))
                                    .pl(crate::ui::sp(22.))
                                    .child(
                                        div()
                                            .id("retry")
                                            .flex()
                                            .items_center()
                                            .gap(crate::ui::sp(6.))
                                            .h(px(24.))
                                            .px(crate::ui::sp(10.))
                                            .rounded(crate::ui::rad(8.))
                                            .bg(theme::content())
                                            .text_color(theme::background())
                                            .text_size(px(crate::ui::TEXT_SM))
                                            .cursor_pointer()
                                            .hover(|s| s.bg(theme::mix(0.8)))
                                            .child(
                                                icon("refresh01", 12.)
                                                    .text_color(theme::background()),
                                            )
                                            .child("Retry")
                                            .on_click(cx.listener(|this, _, _, cx| {
                                                this.thread.update(cx, |t, cx| t.retry(cx))
                                            })),
                                    )
                                    .when(needs_login, |d| {
                                        d.child(
                                            div()
                                                .id("sign-in")
                                                .flex()
                                                .items_center()
                                                .gap(crate::ui::sp(6.))
                                                .h(px(24.))
                                                .px(crate::ui::sp(10.))
                                                .rounded(crate::ui::rad(8.))
                                                .bg(theme::fill_selected())
                                                .text_color(theme::text_muted())
                                                .text_size(px(crate::ui::TEXT_SM))
                                                .cursor_pointer()
                                                .hover(|s| s.bg(theme::mix(0.18)))
                                                .child(
                                                    icon("square-lock02", 12.)
                                                        .text_color(theme::text_muted()),
                                                )
                                                .child("Sign in in a terminal")
                                                .on_click(cx.listener(|this, _, _, cx| {
                                                    this.thread
                                                        .update(cx, |t, cx| t.request_login(cx))
                                                })),
                                        )
                                    })
                                    .child(
                                        div()
                                            .min_w_0()
                                            .truncate()
                                            .text_size(px(crate::ui::TEXT_XS))
                                            .text_color(theme::text_faint())
                                            .child(format!("Log: {}", log.display())),
                                    ),
                            ),
                    )
                    .into_any_element()
            }
            // Room for the raccoon perched on the composer.
            _ => div().h(px(40.)).into_any_element(),
        }
    }

    /// Asks before the first npm install of the agent's adapter: what, where and
    /// the command, which the user can also copy and run themselves.
    fn render_install(&self, cx: &Context<Self>) -> AnyElement {
        let t = self.thread.read(cx);
        let Some(plan) = t.needs_install() else {
            return div().into_any_element();
        };
        let command = plan.command();
        let copied = markdown::was_copied(&command);
        let rows = [
            ("Package", plan.package.to_string()),
            ("Folder", plan.dir.display().to_string()),
            ("Command", command.clone()),
        ];
        let button = |id: &'static str, glyph: &'static str, label: &'static str, primary: bool| {
            let (bg, fg, hover) = if primary {
                (theme::content(), theme::background(), theme::mix(0.8))
            } else {
                (
                    theme::fill_selected(),
                    theme::text_muted(),
                    theme::mix(0.18),
                )
            };
            div()
                .id(id)
                .flex()
                .items_center()
                .gap(crate::ui::sp(6.))
                .h(px(24.))
                .px(crate::ui::sp(10.))
                .rounded(crate::ui::rad(8.))
                .bg(bg)
                .text_color(fg)
                .text_size(px(crate::ui::TEXT_SM))
                .cursor_pointer()
                .hover(move |s| s.bg(hover))
                .child(icon(glyph, 12.).text_color(fg))
                .child(label)
        };
        div()
            .px(crate::ui::sp(16.))
            .pt(crate::ui::sp(8.))
            .pb(crate::ui::sp(40.))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(crate::ui::sp(8.))
                    .p(crate::ui::sp(12.))
                    .rounded(crate::ui::rad(crate::ui::RADIUS_LG))
                    .border_1()
                    .border_color(theme::stroke())
                    .bg(theme::raised())
                    .child(
                        div()
                            .flex()
                            .items_start()
                            .gap(crate::ui::sp(8.))
                            .child(
                                div()
                                    .pt(crate::ui::sp(3.))
                                    .child(icon("download04", 14.).text_color(theme::text_subtle())),
                            )
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .text_size(px(crate::ui::TEXT_MD))
                                    .line_height(px(20.))
                                    .text_color(theme::text_muted())
                                    .child(format!(
                                        "{} needs its ACP adapter. Arakun can install it with npm:",
                                        t.kind().label()
                                    )),
                            ),
                    )
                    .child(
                        div()
                            .id("install-plan")
                            .flex()
                            .flex_col()
                            .gap(crate::ui::sp(2.))
                            .pl(crate::ui::sp(22.))
                            .text_size(px(crate::ui::TEXT_XS))
                            .line_height(px(17.))
                            .children(rows.into_iter().map(|(label, value)| {
                                div()
                                    .flex()
                                    .gap(crate::ui::sp(8.))
                                    .child(
                                        div()
                                            .w(px(60.))
                                            .flex_none()
                                            .text_color(theme::text_faint())
                                            .child(label),
                                    )
                                    .child(
                                        div()
                                            .flex_1()
                                            .min_w_0()
                                            .font_family(theme::FONT_MONO)
                                            .text_color(theme::text_subtle())
                                            .child(value),
                                    )
                            })),
                    )
                    .child(
                        div()
                            .pl(crate::ui::sp(22.))
                            .text_size(px(crate::ui::TEXT_XS))
                            .line_height(px(17.))
                            .text_color(theme::text_faint())
                            .child(format!(
                                "To install it yourself, run the command, then Retry. {} starts another adapter instead.",
                                plan.override_var()
                            )),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(crate::ui::sp(8.))
                            .pl(crate::ui::sp(22.))
                            .child(button("install-adapter", "download04", "Install", true).on_click(
                                cx.listener(|this, _, _, cx| {
                                    this.thread.update(cx, |t, cx| t.install_adapter(cx))
                                }),
                            ))
                            .child(
                                button(
                                    "install-copy",
                                    if copied { "tick02" } else { "copy01" },
                                    if copied { "Copied" } else { "Copy command" },
                                    false,
                                )
                                .on_click(move |_, window, cx| {
                                    markdown::copy_with_feedback(&command, window, cx)
                                }),
                            )
                            .child(button("install-retry", "refresh01", "Retry", false).on_click(
                                cx.listener(|this, _, _, cx| {
                                    this.thread.update(cx, |t, cx| t.retry(cx))
                                }),
                            )),
                    ),
            )
            .into_any_element()
    }

    /// How tall `render_queue`'s card is, 0 without one: the raccoon sits on
    /// top of it. Its rows are 28 px unless their text wraps.
    pub fn queue_height(&self, cx: &App) -> f32 {
        let t = self.thread.read(cx);
        let background = !t.is_subagent_view() && !t.is_busy() && t.running_subagents() > 0;
        let rows = t.queued().len()
            + usize::from(t.stuck().is_some())
            + usize::from(t.check_offer().is_some())
            + usize::from(background);
        if rows == 0 {
            return 0.;
        }
        // Padding top and bottom (it follows the Spacing setting, as in
        // `render_queue`), and the top border.
        (rows + usize::from(t.queue_paused())) as f32 * 28. + 2. * f32::from(crate::ui::sp(4.)) + 1.
    }

    pub(super) fn render_queue(&self, cx: &Context<Self>) -> Option<AnyElement> {
        let t = self.thread.read(cx);
        let queue = t.queued();
        let stuck = t.stuck().map(|s| s.message());
        let offer = t.check_offer().map(str::to_string);
        // Subagents left running once the turn was handed back.
        let background = (!t.is_subagent_view() && !t.is_busy())
            .then(|| t.running_subagents())
            .filter(|&n| n > 0);
        if queue.is_empty() && stuck.is_none() && offer.is_none() && background.is_none() {
            return None;
        }
        let paused = t.queue_paused();
        let button = |id: &'static str, glyph: Option<&'static str>, label: &'static str| {
            div()
                .id(id)
                .flex()
                .flex_none()
                .items_center()
                .gap(crate::ui::sp(6.))
                .h(px(24.))
                .px(crate::ui::sp(6.))
                .rounded(crate::ui::rad(8.))
                .cursor_pointer()
                .hover(|s| s.bg(theme::fill_selected()).text_color(theme::content()))
                .when_some(glyph, |d, g| {
                    d.child(icon(g, 14.).text_color(theme::text_subtle()))
                })
                .child(label)
        };
        Some(
            div()
                .w_full()
                .flex()
                .justify_center()
                .child(
                    div()
                        .w_full()
                        .max_w(px(COLUMN_W))
                        .px(crate::ui::sp(14.))
                        .child(
                            div()
                                .flex()
                                .flex_col()
                                .px(crate::ui::sp(8.))
                                .py(crate::ui::sp(4.))
                                .rounded_t(crate::ui::rad(10.))
                                .bg(theme::raised())
                                .text_size(px(crate::ui::TEXT_SM))
                                .text_color(theme::text_subtle())
                                .when_some(background, |d, n| {
                                    d.child(
                                        div()
                                            .flex()
                                            .items_center()
                                            .gap(crate::ui::sp(8.))
                                            .min_h(px(28.))
                                            .when(stuck.is_some() || offer.is_some() || paused || !queue.is_empty(), |d| {
                                                d.border_b_1().border_color(theme::stroke())
                                            })
                                            .child(spinner(self.phase, 14., theme::text_subtle()))
                                            .child(
                                                div()
                                                    .id("background-subagents")
                                                    .min_w_0()
                                                    .truncate()
                                                    .text_color(theme::text_muted())
                                                    .child(if n == 1 {
                                                        "1 subagent running in the background".to_string()
                                                    } else {
                                                        format!("{n} subagents running in the background")
                                                    }),
                                            )
                                            .child(
                                                button("background-view", Some("sidebar-right01"), "View").on_click(cx.listener(
                                                    |_, _, _, cx| cx.emit(ThreadViewEvent::OpenSubagent(None)),
                                                )),
                                            )
                                            .child(button("background-stop", Some("square"), "Stop").on_click(cx.listener(
                                                |this, _, _, cx| this.thread.update(cx, |t, cx| t.cancel(cx)),
                                            )))
                                            // The buttons stay by the words, clear of the raccoon on the right.
                                            .child(div().flex_1()),
                                    )
                                })
                                .when_some(stuck, |d, message| {
                                    d.child(
                                        div()
                                            .flex()
                                            .items_center()
                                            .gap(crate::ui::sp(8.))
                                            .min_h(px(28.))
                                            .when(!queue.is_empty(), |d| {
                                                d.border_b_1().border_color(theme::stroke())
                                            })
                                            .child(icon("alert-circle", 14.).text_color(theme::warn()))
                                            .child(
                                                div()
                                                    .id("stuck-message")
                                                    .flex_1()
                                                    .min_w_0()
                                                    .truncate()
                                                    .text_color(theme::text_muted())
                                                    .child(message),
                                            )
                                            .child(
                                                button("stuck-rethink", Some("rotate-ccw"), "Stop and rethink")
                                                    .on_click(cx.listener(|this, _, _, cx| {
                                                        this.thread.update(cx, |t, cx| t.rethink(cx))
                                                    })),
                                            )
                                            .child(button("stuck-keep", None, "Keep going").on_click(
                                                cx.listener(|this, _, _, cx| {
                                                    this.thread.update(cx, |t, cx| t.keep_going(cx))
                                                }),
                                            )),
                                    )
                                })
                                .when_some(offer, |d, command| {
                                    let answer = |answer: CheckAnswer| {
                                        cx.listener(move |this, _, _, cx| {
                                            this.thread.update(cx, |t, cx| t.answer_check_offer(answer, cx))
                                        })
                                    };
                                    d.child(
                                        div()
                                            .flex()
                                            .items_center()
                                            .gap(crate::ui::sp(8.))
                                            .min_h(px(28.))
                                            .when(!queue.is_empty() || paused, |d| {
                                                d.border_b_1().border_color(theme::stroke())
                                            })
                                            .child(icon("checkmark-circle02", 14.).text_color(theme::text_subtle()))
                                            .child(
                                                div()
                                                    .id("check-offer-message")
                                                    .flex_1()
                                                    .min_w_0()
                                                    .truncate()
                                                    .text_color(theme::text_muted())
                                                    .child(format!("No check here yet. Run “{command}” after turns that edit files?")),
                                            )
                                            .child(button("check-offer-use", None, "Use it").on_click(answer(CheckAnswer::Use)))
                                            .child(button("check-offer-edit", None, "Edit…").on_click(answer(CheckAnswer::Edit)))
                                            .child(button("check-offer-never", None, "Don't ask").on_click(answer(CheckAnswer::Never))),
                                    )
                                })
                                .when(paused, |d| {
                                    d.child(
                                        div()
                                            .flex()
                                            .items_center()
                                            .gap(crate::ui::sp(8.))
                                            .h(px(28.))
                                            .border_b_1()
                                            .border_color(theme::stroke())
                                            .child(icon("pause", 14.).text_color(theme::text_subtle()))
                                            .child(
                                                div()
                                                    .flex_1()
                                                    .min_w_0()
                                                    .truncate()
                                                    .child("Queue paused because you interrupted"),
                                            )
                                            .child(
                                                div()
                                                    .id("queue-resume")
                                                    .flex()
                                                    .items_center()
                                                    .gap(crate::ui::sp(6.))
                                                    .h(px(24.))
                                                    .px(crate::ui::sp(6.))
                                                    .rounded(crate::ui::rad(8.))
                                                    .cursor_pointer()
                                                    .hover(|s| s.bg(theme::fill_selected()).text_color(theme::content()))
                                                    .child(icon("play", 14.).text_color(theme::text_subtle()))
                                                    .child("Resume")
                                                    .on_click(cx.listener(|this, _, _, cx| {
                                                        this.thread.update(cx, |t, cx| t.resume_queue(cx))
                                                    })),
                                            ),
                                    )
                                })
                                .children(queue.iter().enumerate().map(|(i, text)| {
                                    div()
                                        .flex()
                                        .items_center()
                                        .gap(crate::ui::sp(8.))
                                        .min_h(px(28.))
                                        .when(i > 0, |d| d.border_t_1().border_color(theme::stroke()))
                                        .child(icon("list-end", 14.).text_color(theme::text_subtle()))
                                        .child(
                                            div()
                                                .flex_1()
                                                .min_w_0()
                                                .truncate()
                                                .text_color(theme::text_muted())
                                                .child(text.replace('\n', " ")),
                                        )
                                        .child(
                                            div()
                                                .id(("queue-del", i))
                                                .group("queue-del")
                                                .size(px(24.))
                                                .flex()
                                                .items_center()
                                                .justify_center()
                                                .rounded(crate::ui::rad(8.))
                                                .cursor_pointer()
                                                .hover(|s| s.bg(theme::fill_selected()))
                                                .child(hover_icon(
                                                    "delete02",
                                                    14.,
                                                    theme::text_subtle(),
                                                    theme::content(),
                                                    "queue-del",
                                                ))
                                                .on_click(cx.listener(move |this, _, _, cx| {
                                                    this.thread.update(cx, |t, cx| t.remove_queued(i, cx))
                                                })),
                                        )
                                })),
                        ),
                )
                .into_any_element(),
        )
    }
}
