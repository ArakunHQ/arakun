//! A flag, error or experiment opened inside the tab. Only Open in PostHog
//! (or a replay) leaves it for the browser.

use super::*;

#[derive(Clone, Debug, PartialEq)]
pub(super) enum Detail {
    Flag(u64),
    Issue(String),
    Experiment(u64),
}

/// What the Set up with agent button puts in the composer.
const SETUP_DRAFT: &str = "Inspect this project for existing PostHog instrumentation. \
    Propose a minimal setup using the project's conventions and environment variables. \
    Do not add credentials or start collecting events without my approval.";

impl PostHogView {
    /// Back to the list, dropping whatever the details had open.
    pub(super) fn close_detail(&mut self) {
        self.detail = None;
        self.inspection = None;
        self._inspect = None;
        self.exp_change = None;
        self.edit = None;
        self.write_note = None;
    }

    /// Opens `detail`; an error or experiment reads its details first.
    pub(super) fn inspect(&mut self, detail: Detail, cx: &mut Context<Self>) {
        self.close_detail();
        self.detail = Some(detail.clone());
        let Some(p) = self.current.clone() else {
            return;
        };
        let Some(s) = self.session_of(&p) else { return };
        let id = p.id;
        let read: Box<dyn FnOnce() -> _ + Send> = match detail.clone() {
            Detail::Flag(_) => return,
            Detail::Issue(k) => Box::new(move || posthog::inspection::issue(&s, id, &k)),
            Detail::Experiment(k) => Box::new(move || posthog::inspection::experiment(&s, id, k)),
        };
        self._inspect = Some(cx.spawn(async move |this, cx| {
            let got = cx
                .background_spawn(crate::guard::task(async move { read() }))
                .await;
            this.update(cx, |this, cx| {
                if this.current.as_ref() == Some(&p) && this.detail.as_ref() == Some(&detail) {
                    this.inspection = Some(got);
                    cx.notify();
                }
            })
            .ok();
        }));
    }

    /// Puts a prompt to set up PostHog in the composer; nothing is sent.
    pub(super) fn setup_draft(&self, cx: &mut Context<Self>) -> impl IntoElement {
        ui::button(
            "ph-setup-draft",
            "Set up with agent",
            ButtonKind::Outline,
            true,
        )
        .on_click(cx.listener(|_, _, _, cx| cx.emit(PostHogEvent::Draft(SETUP_DRAFT.into()))))
    }

    pub(super) fn render_detail(
        &self,
        detail: &Detail,
        overview: &Overview,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let mut body = div()
            .flex()
            .flex_col()
            .items_start()
            .gap(ui::sp(14.))
            .child(
                ui::button_with(
                    "ph-back",
                    Some("arrow-left01"),
                    self.tab.label(),
                    ButtonKind::Ghost,
                    true,
                )
                .on_click(cx.listener(|this, _, _, cx| {
                    this.close_detail();
                    cx.notify();
                })),
            );
        if let Some(note) = &self.write_note {
            body = body.child(notice("checkmark-circle02", Tone::Success, note.clone()));
        }
        let path = match detail {
            Detail::Flag(id) => {
                let Some(flag) = overview.flags.iter().find(|f| f.id == *id) else {
                    return body
                        .child(quiet("This flag is no longer in the list."))
                        .into_any_element();
                };
                body = body
                    .child(ui::heading(flag.key.clone()))
                    .when(!flag.name.is_empty() && flag.name != flag.key, |d| {
                        d.child(quiet(flag.name.clone()))
                    })
                    .child(ui::stat(
                        "Status",
                        if flag.active { "Enabled" } else { "Disabled" },
                        "Current flag state",
                    ))
                    .child(ui::stat(
                        "Rollout",
                        flag.rollout.clone(),
                        "Current release conditions",
                    ))
                    .child(self.flag_controls(flag, cx));
                format!("/feature_flags/{id}")
            }
            Detail::Issue(id) => {
                let Some(issue) = overview.issues.iter().find(|i| i.id == *id) else {
                    return body
                        .child(quiet("This issue is no longer in the list."))
                        .into_any_element();
                };
                let path = format!("/error_tracking/{}", posthog::encode(id));
                // Exception messages and event properties can hold customer
                // data, so the draft links the issue instead of copying it.
                let url = self
                    .current
                    .as_ref()
                    .map(|p| p.url(&path))
                    .unwrap_or_default();
                let prompt = format!(
                    "Investigate PostHog issue {url} in this project. Identify the cause and \
                     suggest a tested fix. Do not change the issue in PostHog."
                );
                body = body
                    .child(ui::heading(if issue.kind.is_empty() {
                        "Exception".to_string()
                    } else {
                        issue.kind.clone()
                    }))
                    .child(quiet(crate::redact::text(&issue.message).into_owned()))
                    .child(ui::stat("Occurrences", count(issue.count), "Last 7 days"))
                    .child(ui::stat(
                        "People",
                        count(issue.users),
                        "Affected in the last 7 days",
                    ))
                    .child(
                        ui::button("ph-fix-draft", "Fix with agent", ButtonKind::Primary, true)
                            .on_click(cx.listener(move |_, _, _, cx| {
                                cx.emit(PostHogEvent::Draft(prompt.clone()))
                            })),
                    );
                path
            }
            Detail::Experiment(id) => {
                let Some(e) = overview.experiments.iter().find(|e| e.id == *id) else {
                    return body
                        .child(quiet("This experiment is no longer in the list."))
                        .into_any_element();
                };
                body = body
                    .child(ui::heading(e.name.clone()))
                    .child(quiet(e.status()))
                    .when_some(
                        e.description.clone().filter(|d| !d.is_empty()),
                        |d, text| d.child(quiet(text)),
                    )
                    .when_some(e.conclusion.clone(), |d, conclusion| {
                        d.child(ui::stat(
                            "Recorded conclusion",
                            conclusion,
                            "Reported by PostHog",
                        ))
                    });
                format!("/experiments/{id}")
            }
        };
        if !matches!(detail, Detail::Flag(_)) {
            match &self.inspection {
                None => body = body.child(quiet("Reading details…")),
                Some(Err(e)) => {
                    body = body.child(notice("alert-circle", Tone::Warning, e.message()))
                }
                Some(Ok(v)) => {
                    if let Some(e) = &v.experiment {
                        body = body.child(self.experiment_actions(e, &v.winners, cx));
                    }
                    for (heading, lines) in &v.sections {
                        body = body.child(ui::heading(heading.clone()));
                        for line in lines {
                            body = body.child(quiet(crate::redact::text(line).into_owned()));
                        }
                    }
                    if let Some(path) = v.replay.clone() {
                        body = body.child(
                            ui::button(
                                "ph-replay",
                                "Open session replay",
                                ButtonKind::Outline,
                                true,
                            )
                            .on_click(cx.listener(move |this, _, _, cx| this.open(&path, cx))),
                        );
                    }
                }
            }
        }
        body.child(
            ui::button(
                "ph-detail-open",
                "Open in PostHog",
                ButtonKind::Outline,
                true,
            )
            .on_click(cx.listener(move |this, _, _, cx| this.open(&path, cx))),
        )
        .into_any_element()
    }
}
