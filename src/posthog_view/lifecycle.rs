//! Ending a running experiment, or shipping its winning variant, after the
//! flag key is typed to confirm it.

use super::confirm::Confirm;
use super::*;
use crate::posthog::lifecycle::{self, Action, ReviewedExperiment};
use gpui::Focusable as _;

pub(super) struct Pending {
    project: Project,
    experiment: ReviewedExperiment,
    action: Action,
    busy: bool,
    /// The number of the send in flight, when `busy`.
    seq: u64,
    error: Option<String>,
}

impl PostHogView {
    /// End, and Ship for each variant the results call a winner.
    pub(super) fn experiment_actions(
        &self,
        experiment: &ReviewedExperiment,
        winners: &[String],
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let mut row = div().flex().flex_wrap().gap(ui::sp(8.));
        let actions = std::iter::once(Action::End).chain(
            winners
                .iter()
                .filter(|k| experiment.variants.contains(k))
                .cloned()
                .map(Action::Ship),
        );
        for (i, action) in actions.enumerate() {
            let e = experiment.clone();
            row = row.child(
                ui::button(
                    ("ph-exp-action", i),
                    format!("{}…", action.label()),
                    ButtonKind::Outline,
                    true,
                )
                .on_click(cx.listener(move |this, _, window, cx| {
                    let Some(project) = this.current.clone() else {
                        return;
                    };
                    this.confirm.update(cx, |i, cx| i.clear(cx));
                    this.write_note = None;
                    this.exp_change = Some(Pending {
                        project,
                        experiment: e.clone(),
                        action: action.clone(),
                        busy: false,
                        seq: 0,
                        error: None,
                    });
                    window.focus(&this.confirm.focus_handle(cx), cx);
                    cx.notify();
                })),
            );
        }
        row.into_any_element()
    }

    pub(super) fn experiment_confirmation(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let e = self.exp_change.as_ref()?;
        if self.current.as_ref() != Some(&e.project)
            || self.detail != Some(Detail::Experiment(e.experiment.id))
            || self.page != Page::Main
        {
            return None;
        }
        let ask = Confirm {
            id: "ph-exp-confirm",
            title: format!("{}?", e.action.label()),
            lines: vec![
                format!("{} · {}", e.project.name, e.project.host),
                e.action.effect(),
                "It needs a key with experiment:write.".into(),
            ],
            key: e.experiment.flag.clone(),
            error: e.error.clone(),
            busy: e.busy,
            ready: e
                .experiment
                .accepts(&e.action, self.confirm.read(cx).text()),
            submit: e.action.label(),
        };
        Some(self.confirm_dialog(ask, Self::cancel_experiment, Self::apply_experiment, cx))
    }

    pub(super) fn cancel_experiment(&mut self, cx: &mut Context<Self>) {
        if self.exp_change.as_ref().is_some_and(|e| !e.busy) {
            self.exp_change = None;
            self.confirm.update(cx, |i, cx| i.clear(cx));
            cx.notify();
        }
    }

    pub(super) fn apply_experiment(&mut self, cx: &mut Context<Self>) {
        let typed = self.confirm.read(cx).text().to_owned();
        let Some(e) = &mut self.exp_change else {
            return;
        };
        if e.busy
            || self.current.as_ref() != Some(&e.project)
            || self.detail != Some(Detail::Experiment(e.experiment.id))
            || !e.experiment.accepts(&e.action, &typed)
        {
            return;
        }
        let Some(s) = self
            .sessions
            .iter()
            .find(|s| s.id == e.project.session)
            .cloned()
        else {
            return;
        };
        let (p, review, action) = (e.project.clone(), e.experiment.clone(), e.action.clone());
        let Some(seq) = self.start_sending() else {
            return;
        };
        let Some(e) = self.exp_change.as_mut() else {
            return;
        };
        e.busy = true;
        e.seq = seq;
        e.error = None;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let target = p.clone();
            let id = review.id;
            let done = action.done();
            let result = cx
                .background_spawn(crate::guard::task(async move {
                    lifecycle::apply(&s, &target, &review, &action, &typed)
                }))
                .await;
            this.update(cx, |this, cx| {
                this.done_sending(seq, &p);
                if this.current.as_ref() != Some(&p) {
                    return;
                }
                this._load = None;
                this.loading = false;
                this.refresh(false, cx);
                let open = this.exp_change.as_mut().filter(|e| e.busy && e.seq == seq);
                match (open, result) {
                    // Read the experiment again, which also closes the dialog.
                    (Some(_), Ok(())) => {
                        this.inspect(Detail::Experiment(id), cx);
                        this.write_note = Some(done);
                    }
                    // Stays open with PostHog's reason. Sending again checks
                    // the experiment first, in case this one went through.
                    (Some(e), Err(err)) => {
                        e.busy = false;
                        e.error = Some(err.message());
                        this.confirm.update(cx, |i, cx| i.clear(cx));
                    }
                    (None, _) => {}
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }
}
