//! Running a session for an issue delegated to Arakun in Linear, as the relay
//! passes it on.
//!
//! The relay's word starts nothing by itself: the issue is read with the
//! person's own Linear key, and the guard (`target_for`) must say yes for it
//! on this computer. Text from Linear goes only into the prompt, never a log.
//!
//! A delivery that comes before the Arakun account is loaded is kept in
//! memory and handled once it is, without waiting for the relay to send it
//! again.

use std::path::{Path, PathBuf};

use gpui::{AppContext as _, Context, Window};

use super::Shell;
use super::linear_follow::needs_sign_in;
use super::linear_sync::{Gate, may_sync};
use super::sessions::queued_from;
use crate::cloud;
use crate::linear::delegation::{Delegation, Refusal, target_for};
use crate::linear::issues::{self, Detail};
use crate::linear::runs::{Added, ReportKind, Run, RunState};
use crate::linear::{ApiError, Session, prompt};
use crate::phone::linear::{Delivery, DeliveryKind};

// The rules are in core, which the server shares; the shell's other Linear
// files reach them through here.
pub(super) use crate::linear::start::*;

/// This computer's name, as the relay knows it.
pub(super) fn computer() -> String {
    crate::phone::host_name()
}

/// A run on its way to a session, from a new delivery or from the queue.
#[derive(Clone)]
struct Start {
    agent_session: String,
    organization: String,
    issue: String,
    /// The delivery to accept once handled. None for a run from the queue:
    /// its delivery was accepted when it began to wait.
    delivery: Option<String>,
}

impl Shell {
    /// A delivery from the relay: the order of steps is in the plan, "Order
    /// for a `Created` delivery".
    pub(super) fn linear_delivery(
        &mut self,
        d: Delivery,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let gate = self.linear_gate(cx).is_some();
        if !gate {
            return self.linear_keep(d, cx);
        }
        let in_flight = self.linear_in_flight.contains_key(&d.id);
        let handled = self.linear_runs.was_seen(&d.id);
        let sessions = if gate && !in_flight && !handled {
            crate::linear::sessions()
        } else {
            Vec::new()
        };
        let sign_in = crate::linear::session_for(&sessions, &d.organization).cloned();
        // A stop needs no Linear key; a reply's checks are its own.
        let may_go = sign_in.is_some() || !needs_sign_in(d.kind);
        match (first_step(in_flight, handled, gate, may_go), sign_in) {
            (First::Ignore, _) => {}
            (First::AcceptOnly, _) => self.linear_accept(&d.id, cx),
            (First::Read, Some(sign_in)) if d.kind == DeliveryKind::Created => {
                // Kept for when a read ends: the relay only sends an
                // unaccepted delivery again after the next connect.
                if !may_read(self.linear_reads) {
                    return keep(&mut self.linear_kept, d);
                }
                self.linear_reads += 1;
                // Recorded as handled only with its outcome, so a crash
                // while the issue is read leaves it for the relay to send again.
                let flight = d.agent_session.clone();
                self.linear_in_flight.insert(d.id.clone(), flight);
                let start = Start {
                    agent_session: d.agent_session,
                    organization: d.organization,
                    issue: d.issue,
                    delivery: Some(d.id),
                };
                self.linear_read(start, sign_in, window, cx);
            }
            (First::Read, _) if d.kind == DeliveryKind::Stop => self.linear_stop(&d, window, cx),
            (First::Read, sign_in) if d.kind == DeliveryKind::Prompt => {
                self.linear_prompt(d, sign_in, window, cx)
            }
            (First::RefuseNoSignIn | First::Read, _) => {
                self.linear_runs.mark_seen(&d.id);
                let line = no_sign_in_line(&computer());
                self.linear_report(&d.agent_session, ReportKind::Error, line, None, cx);
                self.linear_accept(&d.id, cx);
                // The server must stop sending this workspace's issues here.
                self.sync_linear(window, cx);
            }
        }
    }

    /// Keeps a delivery that met a closed gate, when the reason is that the
    /// Arakun account is not loaded yet. Otherwise it is let go: an account
    /// without the flag never handles it.
    pub(super) fn linear_keep(&mut self, d: Delivery, cx: &Context<Self>) {
        if self.teams.read(cx).me().is_none() {
            keep(&mut self.linear_kept, d);
        }
    }

    /// The account changed: the kept deliveries are handled if the gate is
    /// open now, and dropped if this account may not use Linear.
    pub(super) fn linear_replay_kept(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.linear_kept.is_empty() {
            return;
        }
        let teams = self.teams.read(cx);
        let (loaded, flagged) = (teams.me().is_some(), teams.linear_enabled());
        let label = crate::phone::Settings::load().relay_label().is_some();
        match kept_after(loaded, flagged, label) {
            Kept::Hold => {}
            Kept::Drop => self.linear_kept.clear(),
            Kept::Replay => {
                for d in std::mem::take(&mut self.linear_kept) {
                    self.linear_delivery(d, window, cx);
                }
            }
        }
    }

    /// Starts the runs that wait, while there are free slots. Each one's
    /// issue is read again and the guard asked again: either may have changed.
    pub(super) fn linear_start_waiting(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        // Without an account nothing could be reported or refused: they
        // wait until it is back (`linear_account_changed`).
        if self.linear_gate(cx).is_none() {
            return;
        }
        while let Some(run) = self.linear_runs.next_waiting() {
            let start = Start {
                agent_session: run.agent_session.clone(),
                organization: run.organization.clone(),
                issue: run.issue.clone(),
                delivery: None,
            };
            self.linear_save();
            let sessions = crate::linear::sessions();
            match crate::linear::session_for(&sessions, &start.organization) {
                Some(s) => self.linear_read(start, s.clone(), window, cx),
                None => {
                    self.linear_fail(&start, no_sign_in_line(&computer()), window, cx);
                    self.sync_linear(window, cx);
                }
            }
        }
    }

    /// Reads the issue with the person's own key, off the UI thread.
    fn linear_read(
        &mut self,
        start: Start,
        sign_in: Session,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let issue = start.issue.clone();
        cx.spawn_in(window, async move |this, cx| {
            let read = cx
                // A panic's text could hold anything; Linear gets a fixed reason.
                .background_spawn(crate::guard::task_or(
                    async move { issues::detail(&sign_in, &issue) },
                    |_| Err(ApiError::Failed(HIT_AN_ERROR.into())),
                ))
                .await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.linear_issue_read(start, read, window, cx)
            });
        })
        .detach();
    }

    fn linear_issue_read(
        &mut self,
        start: Start,
        read: Result<Detail, ApiError>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let held = start
            .delivery
            .is_some()
            .then(|| start.agent_session.clone());
        self.linear_issue_outcome(start, read, window, cx);
        // A stop that came during the read finds the run, if any, in place.
        if let Some(agent_session) = held {
            self.linear_release_stops(&agent_session, window, cx);
        }
    }

    fn linear_issue_outcome(
        &mut self,
        start: Start,
        read: Result<Detail, ApiError>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(id) = &start.delivery {
            self.linear_in_flight.remove(id);
            self.linear_reads = self.linear_reads.saturating_sub(1);
            // Room for one that waited for a read to end.
            self.linear_replay_kept(window, cx);
            // Signed out of Arakun meanwhile: not handled. It is kept, or
            // the relay sends it again, for when there is an account to
            // accept it with.
            if self.linear_gate(cx).is_none() {
                let again = Delivery {
                    id: id.clone(),
                    kind: DeliveryKind::Created,
                    agent_session: start.agent_session,
                    issue: start.issue,
                    organization: start.organization,
                    activity: None,
                    body: None,
                };
                return self.linear_keep(again, cx);
            }
        }
        let detail = match read {
            Ok(detail) => detail,
            Err(e) => {
                let line = unreadable_line(&e.message());
                return self.linear_refuse(&start, line, window, cx);
            }
        };
        let project = match self.linear_guard(&start.organization, &detail) {
            Ok(project) => project,
            Err(r) => {
                self.linear_refuse(&start, refusal_line(&r, &computer()), window, cx);
                // The server sent what this computer no longer takes.
                self.sync_linear(window, cx);
                return;
            }
        };
        let Some(id) = start.delivery.clone() else {
            // From the queue: stopped while its issue was read.
            if !self.linear_still_running(&start.agent_session) {
                return;
            }
            if let Some(run) = self.linear_runs.by_agent_session(&start.agent_session) {
                run.project = project.clone();
            }
            self.linear_save();
            return self.linear_make_env(start, detail, project, window, cx);
        };
        // From here every outcome is saved together with the handled id.
        self.linear_runs.mark_seen(&id);
        let known = self
            .linear_runs
            .by_agent_session(&start.agent_session)
            .is_some();
        if !known
            && self
                .linear_runs
                .active_for_issue(&start.organization, &start.issue)
        {
            // Two sessions on one issue would share its worktree.
            let line = busy_line(&computer());
            self.linear_report(&start.agent_session, ReportKind::Thought, line, None, cx);
            self.linear_accept(&id, cx);
            return;
        }
        let added = self.linear_runs.add(Run {
            agent_session: start.agent_session.clone(),
            organization: start.organization.clone(),
            issue: start.issue.clone(),
            identifier: detail.issue.identifier.clone(),
            project: project.clone(),
            environment: None,
            branch: String::new(),
            session: None,
            state: RunState::Waiting,
            reported_at: 0,
            asked: None,
            stopped: false,
            held: None,
            progress: None,
            linked: None,
            kept: Vec::new(),
        });
        self.linear_save();
        match added {
            Added::Already => self.linear_accept(&id, cx),
            Added::Wait { ahead } => {
                let line = waiting_line(&computer(), ahead);
                self.linear_report(&start.agent_session, ReportKind::Thought, line, None, cx);
                self.linear_accept(&id, cx);
            }
            Added::Start => self.linear_make_env(start, detail, project, window, cx),
        }
    }

    /// The guard, on what this window has now: the switch, the mappings and
    /// the projects open here.
    fn linear_guard(&self, organization: &str, detail: &Detail) -> Result<PathBuf, Refusal> {
        // Only this computer's mappings: a server takes its own.
        let d = Delegation::load().on(None);
        let local: Vec<PathBuf> = self
            .projects
            .iter()
            .filter(|p| !p.is_remote())
            .map(|p| p.path.clone())
            .collect();
        let issue = &detail.issue;
        target_for(
            &d,
            organization,
            &issue.team_id,
            issue.project_id.as_deref(),
            &local,
        )
        .map(|m| m.project.clone())
    }

    fn linear_make_env(
        &mut self,
        start: Start,
        detail: Detail,
        project: PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let bases = self.linear_bases(&project);
        let branch = work_branch(&prompt::branch(&detail), &detail.issue.identifier, &bases);
        self.linear_env(start, detail, project, branch, true, window, cx);
    }

    /// The branches a delegated session must not commit on: the project's
    /// base, and the branch its main checkout is on.
    fn linear_bases(&self, project: &Path) -> Vec<String> {
        let Some(p) = self.projects.iter().find(|p| p.path == project) else {
            return Vec::new();
        };
        let base = p
            .base
            .as_deref()
            .map(|b| crate::git::base_branch_name(b).to_string());
        let checkout = p
            .environments
            .iter()
            .find(|e| e.is_checkout)
            .map(|e| e.branch.to_string());
        base.into_iter()
            .chain(checkout)
            .filter(|b| !b.is_empty())
            .collect()
    }

    #[allow(clippy::too_many_arguments)]
    fn linear_env(
        &mut self,
        start: Start,
        detail: Detail,
        project: PathBuf,
        branch: String,
        may_retry: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let (root, name) = (project.clone(), branch.clone());
        self.env_on_branch(&root, &name, window, cx, move |this, made, window, cx| {
            let path = match made {
                Ok(path) => path,
                Err(e) => {
                    let own = own_branch(&detail.issue.identifier);
                    return match next_branch(&e, &branch, &own, may_retry) {
                        Some(own) => {
                            this.linear_env(start, detail, project, own, false, window, cx)
                        }
                        None => this.linear_fail(&start, no_env_line(&e), window, cx),
                    };
                }
            };
            if !this.linear_is_checkout(&project, &path) {
                return this.linear_begin(start, detail, project, path, branch, window, cx);
            }
            // The branch is the one the main checkout is on: a session there
            // would commit under the person's feet.
            let own = own_branch(&detail.issue.identifier);
            if may_retry && own != branch {
                return this.linear_env(start, detail, project, own, false, window, cx);
            }
            let line = no_env_line("its branch is the one the project's main folder is on");
            this.linear_fail(&start, line, window, cx);
        });
    }

    fn linear_is_checkout(&self, project: &Path, path: &Path) -> bool {
        let same = |a: &Path, b: &Path| {
            a == b
                || matches!(
                    (std::fs::canonicalize(a), std::fs::canonicalize(b)),
                    (Ok(x), Ok(y)) if x == y
                )
        };
        same(project, path) || self.find_env(path).is_some_and(|e| e.is_checkout)
    }

    #[allow(clippy::too_many_arguments)]
    fn linear_begin(
        &mut self,
        start: Start,
        detail: Detail,
        project: PathBuf,
        env_path: PathBuf,
        branch: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.linear_still_running(&start.agent_session) {
            if let Some(id) = &start.delivery {
                self.linear_accept(id, cx);
            }
            return;
        }
        // Making the environment takes a while; the switch or the mapping
        // may have changed since the guard said yes.
        let again = self.linear_guard(&start.organization, &detail);
        if let Err(r) = guard_again(again, &project) {
            self.linear_fail(&start, refusal_line(&r, &computer()), window, cx);
            self.sync_linear(window, cx);
            return;
        }
        let Some(p) = self.projects.iter().find(|p| p.path == project) else {
            let line = refusal_line(&Refusal::ProjectGone, &computer());
            return self.linear_fail(&start, line, window, cx);
        };
        let name = p.name.to_string();
        // The project's usual agent and settings: those of its environment in use.
        let usual = p.environments.get(p.active_env);
        let agent = usual.map_or_else(crate::model::default_agent, |e| e.agent);
        let settings = usual
            .filter(|e| e.path != env_path)
            .map(|e| queued_from(e, String::new()));
        // An environment already in use keeps what it was set to.
        if let Some(settings) = settings
            && let Some(env) = self.find_env_mut(&env_path)
            && env.sessions.is_empty()
        {
            settings.settle_in(env);
        }
        let title = prompt::title(&detail).into();
        let text = prompt::delegated(&detail);
        let started =
            self.start_session_with(&env_path, agent, title, text, false, Vec::new(), window, cx);
        let Some(session) = started else {
            let line = failed_line("Arakun could not start it.");
            return self.linear_fail(&start, line, window, cx);
        };
        self.mark_session_linear(&env_path, &session, &detail.issue.identifier, cx);
        if let Some(run) = self.linear_runs.by_agent_session(&start.agent_session) {
            run.session = Some(session.to_string());
            run.environment = Some(env_path);
            run.branch = branch.clone();
            run.state = RunState::Running;
        }
        self.linear_save();
        self.linear_watch(cx);
        let line = started_line(&name, &computer(), &branch);
        self.linear_report(&start.agent_session, ReportKind::Thought, line, None, cx);
        if let Some(id) = &start.delivery {
            self.linear_accept(id, cx);
        }
        self.linear_send_kept(&start.agent_session, &start.organization, window, cx);
    }

    fn linear_still_running(&mut self, agent_session: &str) -> bool {
        self.linear_runs
            .by_agent_session(agent_session)
            .is_some_and(|r| r.state == RunState::Running)
    }

    /// Refused before a run was added for it: Linear hears why. A run from
    /// the queue has one, so it ends.
    fn linear_refuse(
        &mut self,
        start: &Start,
        line: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(id) = &start.delivery else {
            return self.linear_fail(start, line, window, cx);
        };
        // Saved with the report.
        self.linear_runs.mark_seen(id);
        self.linear_report(&start.agent_session, ReportKind::Error, line, None, cx);
        self.linear_accept(id, cx);
    }

    /// The run could not start: Linear hears why, the run ends and its slot
    /// goes to the next one waiting.
    fn linear_fail(
        &mut self,
        start: &Start,
        line: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(run) = self.linear_runs.by_agent_session(&start.agent_session) {
            run.state = RunState::Done;
        }
        self.linear_save();
        self.linear_report(&start.agent_session, ReportKind::Error, line, None, cx);
        if let Some(id) = &start.delivery {
            self.linear_accept(id, cx);
        }
        self.linear_start_waiting(window, cx);
    }

    /// The Arakun account's server and session, when Linear may be used.
    pub(super) fn linear_gate(&self, cx: &Context<Self>) -> Option<Gate> {
        let teams = self.teams.read(cx);
        may_sync(
            teams.account(),
            teams.linear_enabled(),
            crate::phone::Settings::load().relay_label(),
        )
    }

    pub(super) fn linear_save(&self) {
        if let Err(e) = self.linear_runs.save() {
            eprintln!("arakun: {e}");
        }
    }

    /// Tells the relay this delivery is handled, off the UI thread. A failure
    /// is let go: the relay sends it again, and it is accepted again then.
    pub(super) fn linear_accept(&self, delivery: &str, cx: &mut Context<Self>) {
        let Some(Gate { base, session, .. }) = self.linear_gate(cx) else {
            return;
        };
        let id = delivery.to_string();
        cx.background_spawn(crate::guard::task(async move {
            let _ = cloud::linear_accept(&base, &session, &id);
        }))
        .detach();
    }
}
