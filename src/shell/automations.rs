//! Running automations: by hand from the rail or the Automations dialog, and on
//! their schedules while the window is open.

use std::path::{Path, PathBuf};
use std::time::Duration;

use gpui::{AppContext as _, Context, SharedString, Window};

use super::{Modal, Shell};
use crate::automations_view::{AutomationsDialog, AutomationsEvent};
use crate::model::{self, Automation, GRACE_SECS, unix_now};

/// How often the schedules are checked; runs start at most this late.
const TICK: Duration = Duration::from_secs(20);

impl Shell {
    pub(super) fn start_scheduler(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self._scheduler = Some(cx.spawn_in(window, async move |this, cx| {
            loop {
                cx.background_executor().timer(TICK).await;
                // Also repaints the rail, so its reset countdowns keep counting.
                if this
                    .update_in(cx, |this, window, cx| {
                        this.run_due_automations(window, cx);
                        cx.notify();
                    })
                    .is_err()
                {
                    break;
                }
            }
        }));
    }

    fn run_due_automations(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let now = unix_now();
        let mut due = Vec::new();
        let mut changed = false;
        for (pi, p) in self.projects.iter_mut().enumerate() {
            for (ai, a) in p.automations.iter_mut().enumerate() {
                let Some(at) = a
                    .enabled
                    .then(|| a.schedule.next_after(a.checked))
                    .flatten()
                else {
                    continue;
                };
                if at > now {
                    continue;
                }
                a.checked = now;
                changed = true;
                // Older misses (arakun closed for a day) are skipped, not replayed.
                if now - at <= GRACE_SECS {
                    due.push((pi, ai));
                }
            }
        }
        for (pi, ai) in due {
            self.run_automation(pi, ai, false, window, cx);
        }
        if changed {
            self.save(cx);
            self.refresh_automations(cx);
        }
    }

    /// Runs automation `ai` of project `pi`. `show` (a run by hand) opens the
    /// agent's session in a tab and focuses a command's terminal; scheduled runs
    /// stay in the background and show up in Sessions, the Inbox and the dock.
    pub(super) fn run_automation(
        &mut self,
        pi: usize,
        ai: usize,
        show: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(project) = self.projects.get(pi) else {
            return;
        };
        let Some(a) = project.automations.get(ai).cloned() else {
            return;
        };
        let Some(env_path) = automation_env(project, &a) else {
            return;
        };
        // A run by hand is watched: bring its environment on screen first.
        if show
            && self.env().path != env_path
            && let Some(ei) = project.env_index(&env_path)
        {
            self.select_env(pi, ei, window, cx);
        }
        let session = match a.agent() {
            Some(agent) => {
                let title: SharedString = a.name.clone().into();
                self.start_session_in(&env_path, agent, title, a.body.clone(), show, window, cx)
            }
            None => {
                self.run_in_terminal(&env_path, &a.body, show, window, cx);
                None
            }
        };
        if let Some(a) = self
            .projects
            .get_mut(pi)
            .and_then(|p| p.automations.get_mut(ai))
        {
            a.last_run = Some(unix_now());
            if let Some(id) = session {
                a.last_session = Some(id.to_string());
            }
        }
        self.save(cx);
        self.refresh_automations(cx);
        cx.notify();
    }

    /// The Automations dialog of the active project; `create` opens the editor.
    pub(super) fn open_automations(
        &mut self,
        create: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.menu = None;
        if !self.has_projects() {
            return;
        }
        let project = self.project();
        let project_path = project.path.clone();
        let mut envs: Vec<(PathBuf, SharedString)> = Vec::new();
        if let Some(main) = main_env(project) {
            envs.push((main.clone(), "Main checkout".into()));
        }
        envs.extend(
            project
                .environments
                .iter()
                .filter(|e| Some(&e.path) != main_env(project).as_ref())
                .map(|e| (e.path.clone(), e.name.clone())),
        );
        let (name, items) = (project.name.clone(), project.automations.clone());
        let dialog = cx.new(|cx| AutomationsDialog::new(name, items, envs, create, window, cx));
        let rules = self.rules.items.clone();
        dialog.update(cx, |d, cx| d.set_rules(rules, cx));
        let sub = cx.subscribe_in(
            &dialog,
            window,
            move |this, _, ev: &AutomationsEvent, window, cx| {
                let Some(pi) = this.projects.iter().position(|p| p.path == project_path) else {
                    return this.close_modal(window, cx);
                };
                let list = &mut this.projects[pi].automations;
                match ev {
                    AutomationsEvent::Save { index, automation } => {
                        match index.filter(|i| *i < list.len()) {
                            Some(i) => list[i] = automation.clone(),
                            None => list.push(automation.clone()),
                        }
                    }
                    AutomationsEvent::Delete(i) => {
                        if *i < list.len() {
                            list.remove(*i);
                        }
                    }
                    AutomationsEvent::SetEnabled(i, on) => {
                        if let Some(a) = list.get_mut(*i) {
                            a.enabled = *on;
                            // Turning it back on never catches up on the paused time.
                            a.checked = unix_now();
                        }
                    }
                    AutomationsEvent::Run(i) => {
                        let i = *i;
                        this.close_modal(window, cx);
                        this.run_automation(pi, i, true, window, cx);
                        return;
                    }
                    AutomationsEvent::OpenSession(i) => {
                        let id = list.get(*i).and_then(|a| a.last_session.clone());
                        let target = id.and_then(|id| {
                            let env = this.projects[pi]
                                .environments
                                .iter()
                                .position(|e| e.session_index(&id).is_some())?;
                            Some((env, id))
                        });
                        this.close_modal(window, cx);
                        if let Some((ei, id)) = target {
                            this.select_env(pi, ei, window, cx);
                            this.open_session(id.into(), window, cx);
                        }
                        return;
                    }
                    AutomationsEvent::CancelRule(id) => {
                        if this.rules.cancel(*id) {
                            this.rules.save();
                        }
                        this.refresh_automations(cx);
                        cx.notify();
                        return;
                    }
                    AutomationsEvent::Dismissed => return this.close_modal(window, cx),
                }
                this.save(cx);
                this.refresh_automations(cx);
                cx.notify();
            },
        );
        self.modal = Some((Modal::Automations(dialog), sub));
        cx.notify();
    }

    /// Hands the open Automations dialog the current list (last runs, edits).
    pub(super) fn refresh_automations(&mut self, cx: &mut Context<Self>) {
        let Some((Modal::Automations(dialog), _)) = &self.modal else {
            return;
        };
        let dialog = dialog.clone();
        let Some(items) = self
            .projects
            .get(self.active_project)
            .map(|p| p.automations.clone())
        else {
            return;
        };
        let rules = self.rules.items.clone();
        dialog.update(cx, |d, cx| {
            d.set_items(items, cx);
            d.set_rules(rules, cx);
        });
    }
}

pub(super) fn main_env(project: &model::Project) -> Option<PathBuf> {
    project
        .environments
        .iter()
        .find(|e| e.is_checkout)
        .or(project.environments.first())
        .map(|e| e.path.clone())
}

/// Where an automation runs: its environment while that still exists, else the main checkout.
fn automation_env(project: &model::Project, a: &Automation) -> Option<PathBuf> {
    a.env
        .as_deref()
        .filter(|p: &&Path| project.env_index(p).is_some())
        .map(Path::to_path_buf)
        .or_else(|| main_env(project))
}
