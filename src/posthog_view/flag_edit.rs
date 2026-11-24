//! Changing a flag: stage one change, review it with its account and
//! project, then type the flag key to send it.

use super::confirm::Confirm;
use super::*;
use crate::posthog::{
    Flag,
    flag_edit::{self, Change},
};
use gpui::Focusable as _;

pub(super) struct Edit {
    target: Project,
    flag: Flag,
    change: Change,
    /// The confirmation dialog is open.
    review: bool,
    busy: bool,
    /// The number of the send in flight, when `busy`.
    seq: u64,
    error: Option<String>,
}

impl Edit {
    pub(super) fn reviewing(&self) -> bool {
        self.review
    }

    fn summary(&self) -> String {
        let state = |on| if on { "Enabled" } else { "Disabled" };
        match self.change {
            Change::Active(on) => format!("{} → {}", state(self.flag.active), state(on)),
            Change::Rollout(i, pct) => format!(
                "Condition {}: {}% → {pct}%",
                i + 1,
                flag_edit::rollout(&self.flag, i).unwrap_or(100.)
            ),
        }
    }
}

impl PostHogView {
    /// The dialog for the staged change, while it is under review.
    pub(super) fn confirmation(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let e = self.edit.as_ref()?;
        if !e.review
            || self.page != Page::Main
            || self.current.as_ref() != Some(&e.target)
            || self.detail != Some(Detail::Flag(e.flag.id))
        {
            return None;
        }
        let current = self.flag_unchanged(&e.flag);
        let mut lines = vec![
            self.destination(&e.target),
            e.summary(),
            "This changes the flag for everyone who evaluates it. It needs a key with \
             feature_flag:write."
                .into(),
        ];
        if !current {
            lines.push(
                "The flag changed since you staged this. Cancel and review the latest version."
                    .into(),
            );
        }
        let ask = Confirm {
            id: "ph-flag-confirm",
            title: format!("Change {}?", e.flag.key),
            lines,
            key: e.flag.key.clone(),
            error: e.error.clone(),
            busy: e.busy,
            ready: current && self.confirm.read(cx).text() == e.flag.key,
            submit: "Change flag".into(),
        };
        Some(self.confirm_dialog(ask, Self::cancel_flag_review, Self::apply_flag, cx))
    }

    /// Closes the dialog; the change stays staged under the flag.
    pub(super) fn cancel_flag_review(&mut self, cx: &mut Context<Self>) {
        if let Some(e) = self.edit.as_mut().filter(|e| !e.busy) {
            e.review = false;
            e.error = None;
            self.confirm.update(cx, |i, cx| i.clear(cx));
            cx.notify();
        }
    }

    /// `flag` is still what the last read of the project says.
    fn flag_unchanged(&self, flag: &Flag) -> bool {
        self.overview
            .data
            .as_ref()
            .is_some_and(|o| o.flags.contains(flag))
    }

    fn stage_flag(&mut self, id: u64, change: Change, cx: &mut Context<Self>) {
        let Some(target) = self.current.clone() else {
            return;
        };
        let Some(f) = self
            .overview
            .data
            .as_ref()
            .and_then(|o| o.flags.iter().find(|f| f.id == id))
        else {
            return;
        };
        if self.edit.as_ref().is_some_and(|e| e.busy)
            || flag_edit::patch(f, change, &f.key).is_none()
        {
            return;
        }
        self.confirm.update(cx, |i, cx| i.clear(cx));
        self.write_note = None;
        self.edit = Some(Edit {
            target,
            flag: f.clone(),
            change,
            review: false,
            busy: false,
            seq: 0,
            error: None,
        });
        cx.notify();
    }

    pub(super) fn apply_flag(&mut self, cx: &mut Context<Self>) {
        let typed = self.confirm.read(cx).text().to_string();
        let unchanged = self
            .edit
            .as_ref()
            .is_some_and(|e| self.flag_unchanged(&e.flag));
        let Some(e) = self.edit.as_mut() else { return };
        if !e.review
            || e.busy
            || !unchanged
            || self.current.as_ref() != Some(&e.target)
            || typed != e.flag.key
            || self.detail != Some(Detail::Flag(e.flag.id))
        {
            return;
        }
        let Some(session) = self
            .sessions
            .iter()
            .find(|s| s.id == e.target.session)
            .cloned()
        else {
            return;
        };
        let (project, flag, change) = (e.target.clone(), e.flag.clone(), e.change);
        let Some(seq) = self.start_sending() else {
            return;
        };
        let Some(e) = self.edit.as_mut() else { return };
        e.busy = true;
        e.seq = seq;
        e.error = None;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let target = project.clone();
            let key = flag.key.clone();
            let result = cx
                .background_spawn(crate::guard::task(async move {
                    flag_edit::apply(&session, &target, &flag, change, &typed)
                }))
                .await;
            this.update(cx, |this, cx| {
                this.done_sending(seq, &project, cx);
                if this.current.as_ref() != Some(&project) {
                    return;
                }
                // Read the flag again either way: a refused or timed-out
                // change may still have changed it.
                this.overview.cancel();
                this.refresh(false, cx);
                if let Some(e) = this.edit.as_mut().filter(|e| e.busy && e.seq == seq) {
                    match result {
                        Ok(()) => {
                            this.edit = None;
                            this.write_note = Some(format!("Changed {key}."));
                        }
                        // Stays open with PostHog's reason; Cancel goes back to the flag.
                        Err(err) => {
                            e.error = Some(err.message());
                            e.busy = false;
                        }
                    }
                    this.confirm.update(cx, |i, cx| i.clear(cx));
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Under a flag's details: the changes it can stage, or the staged one.
    pub(super) fn flag_controls(&self, f: &Flag, cx: &mut Context<Self>) -> AnyElement {
        let mut body = div().flex().flex_col().items_start().gap(ui::sp(10.));
        if f.version.is_none() {
            return body
                .child(quiet(
                    "PostHog did not return a version for this flag, so it can only be \
                     changed in PostHog.",
                ))
                .into_any_element();
        }
        let id = f.id;
        if let Some(e) = &self.edit
            && e.flag.id == id
            && self.current.as_ref() == Some(&e.target)
        {
            body = body
                .child(ui::heading("1 change not sent"))
                .child(quiet(format!(
                    "{} · {} · {}",
                    e.target.name, e.target.host, e.flag.key
                )))
                .child(quiet(e.summary()));
            if !e.busy {
                let fresh = &e.flag == f;
                let mut row = div().flex().gap(ui::sp(8.));
                if fresh {
                    row = row.child(
                        ui::button("ph-review", "Review change", ButtonKind::Primary, true)
                            .on_click(cx.listener(|this, _, window, cx| {
                                if let Some(e) = &mut this.edit {
                                    e.review = true;
                                    e.error = None;
                                }
                                window.focus(&this.confirm.focus_handle(cx), cx);
                                cx.notify();
                            })),
                    );
                } else {
                    body = body.child(quiet(
                        "The flag changed since you staged this. Discard it and stage again.",
                    ));
                }
                body = body.child(row.child(
                    ui::button("ph-discard", "Discard", ButtonKind::Ghost, true).on_click(
                        cx.listener(|this, _, _, cx| {
                            this.edit = None;
                            cx.notify();
                        }),
                    ),
                ));
            }
            return body.into_any_element();
        }
        let active = f.active;
        body = body.child(
            ui::button(
                "ph-stage-active",
                if active { "Disable…" } else { "Enable…" },
                ButtonKind::Outline,
                true,
            )
            .on_click(
                cx.listener(move |this, _, _, cx| this.stage_flag(id, Change::Active(!active), cx)),
            ),
        );
        for i in 0..flag_edit::conditions(f) {
            let Some(pct) = flag_edit::rollout(f, i) else {
                continue;
            };
            let mut row = div()
                .flex()
                .items_center()
                .gap(ui::sp(10.))
                .child(quiet(format!("Condition {} · {pct}%", i + 1)));
            for (n, (step, label)) in [(-10., "−10%"), (10., "+10%")].into_iter().enumerate() {
                let to = (pct + step).clamp(0., 100.).round() as u64;
                if to as f64 != pct {
                    row = row.child(
                        ui::button(("ph-rollout", i * 2 + n), label, ButtonKind::Outline, true)
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.stage_flag(id, Change::Rollout(i, to), cx)
                            })),
                    );
                }
            }
            body = body.child(row);
        }
        body.into_any_element()
    }
}
