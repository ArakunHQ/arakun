//! Running an issue delegated in Linear on this server, as the relay passes
//! it on: the same steps and words as the desktop app (`src/shell/linear_*`),
//! from the rules in `crate::linear`.
//!
//! The relay's word starts nothing by itself: the issue is read with the
//! workspace's key the app gave this server, and the guard must say yes for
//! one of this server's places. Text from Linear goes only into the prompt or
//! to the session as a message, never into a log.
//!
//! `linear-runs.json` in the server's folder holds the deliveries handled,
//! each run and the reports Linear has not taken yet.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use gpui::{AppContext as _, Context, Entity};

use super::{Daemon, PlaceMeta};
use crate::agent::{Thread, ThreadEvent, ThreadStatus};
use crate::cloud;
use crate::linear::delegation::{Refusal, target_for};
use crate::linear::events::{
    Event, Now, STILL_WORKING, Seen, Step, advance, ask_of, closed_line, pr_link, removed_line,
    to_report, turn_reply,
};
use crate::linear::follow::{
    FollowUp, SKIPS, STOPPED, Source, Stop, after_reply, follow_up, hold_stop, interrupted_line,
    message, no_directions_line, no_session_line, not_started_line, passed_line, pending_in,
    release_stops, reply_no_sign_in_line, reply_off_line, reply_source, run_state, skips_question,
    stop, stopped_after_reply, unread_line,
};
use crate::linear::issues::{self, Detail};
use crate::linear::outbox::{Flushed, flush};
use crate::linear::runs::{Added, Held, Report, ReportKind, Run, RunState, Runs};
use crate::linear::start::{
    HIT_AN_ERROR, busy_line, guard_again, keep, may_read, next_branch, no_env_line,
    no_sign_in_line, own_branch, refusal_line, started_line, unreadable_line, waiting_line,
    work_branch,
};
use crate::linear::{ApiError, env, prompt};
use crate::model::unix_now;
use crate::phone::linear::{Delivery, DeliveryKind};

/// The file in the server's folder.
pub const FILE: &str = "linear-runs.json";

/// How often a quiet session is looked at and the outbox tried again.
const TICK: Duration = Duration::from_secs(5 * 60);

/// What the server keeps for Linear's runs while it runs.
#[derive(Default)]
pub(super) struct Runner {
    pub runs: Runs,
    /// Deliveries being read now, by delivery id, with their agent session.
    in_flight: HashMap<String, String>,
    /// New issues being read.
    reads: usize,
    /// Deliveries that came while [`may_read`] said no.
    kept: Vec<Delivery>,
    /// Stops that wait for other work on their agent session.
    held_stops: Vec<Delivery>,
    flushing: bool,
    flush_owed: bool,
    watching: bool,
}

impl Runner {
    pub fn load(dir: &Path) -> Runner {
        Runner {
            runs: Runs::load_from(&path(dir)),
            ..Runner::default()
        }
    }
}

fn path(dir: &Path) -> PathBuf {
    dir.join(FILE)
}

/// This server's name, as the relay knows it.
fn computer() -> String {
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

/// The branches a delegated session must not commit on: the branch the
/// project's own folder is on, and origin's default branch.
fn bases(project: &Path) -> Vec<String> {
    let checkout = crate::git::current_branch(project);
    let base = crate::git::new_env_base(project, None)
        .map(|b| crate::git::base_branch_name(&b).to_string());
    checkout
        .into_iter()
        .chain(base)
        .filter(|b| !b.is_empty())
        .collect()
}

/// Whether `a` and `b` are the same folder.
fn same_folder(a: &Path, b: &Path) -> bool {
    a == b
        || matches!(
            (std::fs::canonicalize(a), std::fs::canonicalize(b)),
            (Ok(x), Ok(y)) if x == y
        )
}

/// What making the worktree gave: its folder, how git describes it, and the
/// branch it is on; or why there is none.
type Made = Result<(PathBuf, PlaceMeta, String), String>;

impl Daemon {
    fn linear_gate(&self) -> Option<(String, String)> {
        let s = self.signed_in.as_ref().filter(|_| !self.session_dead)?;
        Some((s.base.clone(), s.session.expose().to_string()))
    }

    fn linear_key(&self, organization: &str) -> Option<String> {
        self.linear
            .kept
            .settings
            .key_for(organization)
            .map(str::to_string)
    }

    pub(super) fn linear_save_runs(&self) {
        if let Err(e) = self.linear_run.runs.save_to(&path(&super::super::dir())) {
            eprintln!("arakun server: {e}");
        }
    }

    /// A delivery from the relay. Without a sign-in nothing can be accepted
    /// or reported: the relay sends it again after the next connect.
    pub(super) fn linear_delivery(&mut self, d: Delivery, cx: &mut Context<Self>) {
        if self.linear_gate().is_none() || self.linear_run.in_flight.contains_key(&d.id) {
            return;
        }
        if self.linear_run.runs.was_seen(&d.id) {
            return self.linear_accept(&d.id, cx);
        }
        match d.kind {
            DeliveryKind::Stop => self.linear_stop(&d, cx),
            DeliveryKind::Prompt => self.linear_prompt(d, cx),
            DeliveryKind::Created => {
                let Some(key) = self.linear_key(&d.organization) else {
                    self.linear_run.runs.mark_seen(&d.id);
                    let line = no_sign_in_line(&computer());
                    self.linear_report(&d.agent_session, ReportKind::Error, line, None, cx);
                    self.linear_accept(&d.id, cx);
                    // Arakun's server must stop sending this workspace here.
                    return self.linear_sync(cx);
                };
                if !may_read(self.linear_run.reads) {
                    return keep(&mut self.linear_run.kept, d);
                }
                self.linear_run.reads += 1;
                // Recorded as handled only with its outcome, so a crash while
                // the issue is read leaves it for the relay to send again.
                let flight = d.agent_session.clone();
                self.linear_run.in_flight.insert(d.id.clone(), flight);
                let start = Start {
                    agent_session: d.agent_session,
                    organization: d.organization,
                    issue: d.issue,
                    delivery: Some(d.id),
                };
                self.linear_read(start, key, cx);
            }
        }
    }

    /// Reads the issue with the workspace's key, off the UI thread.
    fn linear_read(&mut self, start: Start, key: String, cx: &mut Context<Self>) {
        let issue = start.issue.clone();
        cx.spawn(async move |this, cx| {
            let read = cx
                // A panic's text could hold anything; Linear gets a fixed reason.
                .background_spawn(crate::guard::task_or(
                    async move { issues::detail(|q, v| crate::linear::call(&key, q, v), &issue) },
                    |_| Err(ApiError::Failed(HIT_AN_ERROR.into())),
                ))
                .await;
            let _ = this.update(cx, |this, cx| this.linear_issue_read(start, read, cx));
        })
        .detach();
    }

    fn linear_issue_read(
        &mut self,
        start: Start,
        read: Result<Detail, ApiError>,
        cx: &mut Context<Self>,
    ) {
        let held = start
            .delivery
            .is_some()
            .then(|| start.agent_session.clone());
        self.linear_issue_outcome(start, read, cx);
        // A stop that came during the read finds the run, if any, in place.
        if let Some(agent_session) = held {
            self.linear_release_stops(&agent_session, cx);
        }
    }

    fn linear_issue_outcome(
        &mut self,
        start: Start,
        read: Result<Detail, ApiError>,
        cx: &mut Context<Self>,
    ) {
        if let Some(id) = &start.delivery {
            self.linear_run.in_flight.remove(id);
            self.linear_run.reads = self.linear_run.reads.saturating_sub(1);
            // Room for those that waited for a read to end.
            for d in std::mem::take(&mut self.linear_run.kept) {
                self.linear_delivery(d, cx);
            }
            if self.linear_gate().is_none() {
                // Signed out meanwhile: the relay sends it again.
                return;
            }
        }
        let detail = match read {
            Ok(detail) => detail,
            Err(e) => return self.linear_refuse(&start, unreadable_line(&e.message()), cx),
        };
        let project = match self.linear_guard(&start.organization, &detail) {
            Ok(project) => project,
            Err(r) => {
                self.linear_refuse(&start, refusal_line(&r, &computer()), cx);
                // Arakun's server sent what this server no longer takes.
                self.linear_sync(cx);
                return;
            }
        };
        let Some(id) = start.delivery.clone() else {
            // From the queue: stopped while its issue was read.
            if !self.linear_still_running(&start.agent_session) {
                return;
            }
            if let Some(run) = self.linear_run.runs.by_agent_session(&start.agent_session) {
                run.project = project.clone();
            }
            self.linear_save_runs();
            return self.linear_make_env(start, detail, project, cx);
        };
        // From here every outcome is saved together with the handled id.
        self.linear_run.runs.mark_seen(&id);
        let known = self
            .linear_run
            .runs
            .by_agent_session(&start.agent_session)
            .is_some();
        if !known
            && self
                .linear_run
                .runs
                .active_for_issue(&start.organization, &start.issue)
        {
            // Two sessions on one issue would share its worktree.
            let line = busy_line(&computer());
            self.linear_report(&start.agent_session, ReportKind::Thought, line, None, cx);
            return self.linear_accept(&id, cx);
        }
        let added = self.linear_run.runs.add(Run {
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
        });
        self.linear_save_runs();
        match added {
            Added::Already => self.linear_accept(&id, cx),
            Added::Wait { ahead } => {
                let line = waiting_line(&computer(), ahead);
                self.linear_report(&start.agent_session, ReportKind::Thought, line, None, cx);
                self.linear_accept(&id, cx);
            }
            Added::Start => self.linear_make_env(start, detail, project, cx),
        }
    }

    /// The guard, on what this server has now: the switch and mappings the
    /// app sent, and its places.
    fn linear_guard(&self, organization: &str, detail: &Detail) -> Result<PathBuf, Refusal> {
        let d = self.linear.kept.settings.delegation();
        let issue = &detail.issue;
        target_for(
            &d,
            organization,
            &issue.team_id,
            issue.project_id.as_deref(),
            &self.linear_places(),
        )
        .map(|m| m.project.clone())
    }

    fn linear_make_env(
        &mut self,
        start: Start,
        detail: Detail,
        project: PathBuf,
        cx: &mut Context<Self>,
    ) {
        let identifier = detail.issue.identifier.clone();
        let wanted = prompt::branch(&detail);
        let root = project.clone();
        cx.spawn(async move |this, cx| {
            let made = cx
                .background_spawn(crate::guard::task_or(
                    async move { make_worktree(&root, &wanted, &identifier) },
                    |_| Err(HIT_AN_ERROR.to_string()),
                ))
                .await;
            let _ = this.update(cx, |this, cx| {
                this.linear_env_made(start, detail, project, made, cx)
            });
        })
        .detach();
    }

    fn linear_env_made(
        &mut self,
        start: Start,
        detail: Detail,
        project: PathBuf,
        made: Made,
        cx: &mut Context<Self>,
    ) {
        let (path, meta, branch) = match made {
            Ok(made) => made,
            Err(e) => return self.linear_fail(&start, no_env_line(&e), cx),
        };
        if !self.linear_still_running(&start.agent_session) {
            if let Some(id) = &start.delivery {
                self.linear_accept(id, cx);
            }
            return;
        }
        // Making the worktree takes a while; the switch or the mapping may
        // have changed since the guard said yes.
        let again = self.linear_guard(&start.organization, &detail);
        if let Err(r) = guard_again(again, &project) {
            self.linear_fail(&start, refusal_line(&r, &computer()), cx);
            return self.linear_sync(cx);
        }
        if self.being_deleted(&path) {
            let line = no_env_line("its folder is being deleted on this server");
            return self.linear_fail(&start, line, cx);
        }
        let Some(home) = self.store.place(&project).cloned() else {
            let line = refusal_line(&Refusal::ProjectGone, &computer());
            return self.linear_fail(&start, line, cx);
        };
        // The worktree is a place of its own, with the project's agent and
        // settings, as a device's new environment is.
        if self.store.place(&path).is_none() {
            if let Err(e) = self.offer_place(&path, home.agent) {
                return self.linear_fail(&start, no_env_line(&e), cx);
            }
            if let Some(p) = self.store.places.iter_mut().find(|p| p.path == path)
                && p.picks.is_empty()
            {
                p.picks = home.picks.clone();
            }
        }
        self.meta.insert(path.clone(), meta);
        let agent = self.store.place(&path).map_or(home.agent, |p| p.agent);
        let title = prompt::title(&detail);
        let text = prompt::delegated(&detail);
        let session = self.start_session(&path, agent, Some(title), text, cx);
        if let Some(run) = self.linear_run.runs.by_agent_session(&start.agent_session) {
            run.session = Some(session);
            run.environment = Some(path);
            run.branch = branch.clone();
            run.state = RunState::Running;
        }
        self.linear_save_runs();
        self.linear_watch(cx);
        let name = crate::server::store::repo_name(&project)
            .unwrap_or_else(|| project.display().to_string());
        let line = started_line(&name, &computer(), &branch);
        self.linear_report(&start.agent_session, ReportKind::Thought, line, None, cx);
        if let Some(id) = &start.delivery {
            self.linear_accept(id, cx);
        }
    }

    fn linear_still_running(&mut self, agent_session: &str) -> bool {
        self.linear_run
            .runs
            .by_agent_session(agent_session)
            .is_some_and(|r| r.state == RunState::Running)
    }

    /// Refused before a run was added for it: Linear hears why. A run from
    /// the queue has one, so it ends.
    fn linear_refuse(&mut self, start: &Start, line: String, cx: &mut Context<Self>) {
        let Some(id) = &start.delivery else {
            return self.linear_fail(start, line, cx);
        };
        self.linear_run.runs.mark_seen(id);
        self.linear_report(&start.agent_session, ReportKind::Error, line, None, cx);
        self.linear_accept(id, cx);
    }

    /// The run could not start: Linear hears why, the run ends and its slot
    /// goes to the next one waiting.
    fn linear_fail(&mut self, start: &Start, line: String, cx: &mut Context<Self>) {
        if let Some(run) = self.linear_run.runs.by_agent_session(&start.agent_session) {
            run.state = RunState::Done;
        }
        self.linear_save_runs();
        self.linear_report(&start.agent_session, ReportKind::Error, line, None, cx);
        if let Some(id) = &start.delivery {
            self.linear_accept(id, cx);
        }
        self.linear_start_waiting(cx);
    }

    /// Starts the runs that wait, while there are free slots. Each one's
    /// issue is read again and the guard asked again: either may have changed.
    pub(super) fn linear_start_waiting(&mut self, cx: &mut Context<Self>) {
        if self.linear_gate().is_none() {
            return;
        }
        while let Some(run) = self.linear_run.runs.next_waiting() {
            let start = Start {
                agent_session: run.agent_session.clone(),
                organization: run.organization.clone(),
                issue: run.issue.clone(),
                delivery: None,
            };
            self.linear_save_runs();
            match self.linear_key(&start.organization) {
                Some(key) => self.linear_read(start, key, cx),
                None => {
                    self.linear_fail(&start, no_sign_in_line(&computer()), cx);
                    self.linear_sync(cx);
                }
            }
        }
    }

    // ---- replies and stops ---------------------------------------------------

    /// A reply from Linear: passed to the run's session as a new message,
    /// never as the answer to a permission. Its text is read from Linear.
    fn linear_prompt(&mut self, d: Delivery, cx: &mut Context<Self>) {
        let on = self.linear.kept.settings.enabled;
        let key = self.linear_key(&d.organization);
        let activity = match reply_source(&d, on, key.is_some()) {
            Source::Off => return self.linear_turn_away(&d, reply_off_line(&computer()), cx),
            Source::NoSignIn => {
                self.linear_turn_away(&d, reply_no_sign_in_line(&computer()), cx);
                return self.linear_sync(cx);
            }
            Source::Unreadable => None,
            Source::Read(activity) => Some(activity.to_string()),
        };
        // Without its text yet: is there a session to read it for?
        let decided = self.linear_follow_up(&d, None);
        let (Some(activity), Some(key), FollowUp::Unreadable) = (activity, key, decided) else {
            return self.linear_answer(&d.id, &d.agent_session, decided, None, cx);
        };
        let flight = d.agent_session.clone();
        self.linear_run.in_flight.insert(d.id.clone(), flight);
        let agent_session = d.agent_session.clone();
        cx.spawn(async move |this, cx| {
            // Any error, a panic too, means the text could not be read.
            let text = cx
                .background_spawn(crate::guard::task_or(
                    async move {
                        issues::prompt_text(
                            |q, v| crate::linear::call(&key, q, v),
                            &activity,
                            &agent_session,
                        )
                        .ok()
                    },
                    |_| None,
                ))
                .await;
            let _ = this.update(cx, |this, cx| this.linear_prompt_read(d, text, cx));
        })
        .detach();
    }

    /// A reply refused whatever its run: the issue hears why.
    fn linear_turn_away(&mut self, d: &Delivery, line: String, cx: &mut Context<Self>) {
        self.linear_run.runs.mark_seen(&d.id);
        self.linear_report(&d.agent_session, ReportKind::Error, line, None, cx);
        self.linear_accept(&d.id, cx);
    }

    fn linear_prompt_read(&mut self, d: Delivery, text: Option<String>, cx: &mut Context<Self>) {
        self.linear_run.in_flight.remove(&d.id);
        if self.linear_gate().is_none() {
            // Signed out meanwhile: the relay sends it again.
        } else if !self.linear.kept.settings.enabled {
            self.linear_turn_away(&d, reply_off_line(&computer()), cx);
        } else {
            // The run may have changed during the read.
            let decided = self.linear_follow_up(&d, text.as_deref());
            self.linear_answer(&d.id, &d.agent_session, decided, text, cx);
        }
        // A stop sent after this reply goes after it too.
        self.linear_release_stops(&d.agent_session, cx);
    }

    fn linear_follow_up(&mut self, d: &Delivery, text: Option<&str>) -> FollowUp {
        let run = self.linear_run.runs.by_agent_session(&d.agent_session);
        let state = run_state(run.as_deref(), &d.organization);
        // The run's own issue, not the one the relay names.
        let issue = run.as_ref().map(|r| r.issue.clone());
        let session = run.and_then(|r| r.session.clone());
        let exists = session.is_some_and(|s| self.store.session(&s).is_some());
        let busy =
            issue.is_some_and(|i| self.linear_run.runs.active_for_issue(&d.organization, &i));
        follow_up(state, exists, busy, text)
    }

    fn linear_answer(
        &mut self,
        id: &str,
        agent_session: &str,
        decided: FollowUp,
        text: Option<String>,
        cx: &mut Context<Self>,
    ) {
        self.linear_run.runs.mark_seen(id);
        let (kind, line) = match decided {
            FollowUp::NoRun => (ReportKind::Error, no_session_line(&computer())),
            FollowUp::NotStarted => (ReportKind::Thought, not_started_line(&computer())),
            FollowUp::Busy => (ReportKind::Thought, busy_line(&computer())),
            FollowUp::Unreadable => (ReportKind::Thought, unread_line(&computer())),
            FollowUp::NoDirections => (ReportKind::Thought, no_directions_line(&computer())),
            FollowUp::Send | FollowUp::Interrupt => {
                let interrupt = decided == FollowUp::Interrupt;
                let text = message(text.as_deref().unwrap_or_default()).to_string();
                match self.linear_pass_on(agent_session, text, interrupt, cx) {
                    Some((_, true)) if interrupt => {
                        (ReportKind::Thought, interrupted_line(&computer()))
                    }
                    Some((pending, busy)) => {
                        (ReportKind::Thought, passed_line(&computer(), pending, busy))
                    }
                    None => (ReportKind::Error, no_session_line(&computer())),
                }
            }
        };
        self.linear_report(agent_session, kind, line, None, cx);
        self.linear_accept(id, cx);
    }

    /// Sends `text` to the run's session as a device's message would: one that
    /// is busy queues it. Gives what it waits behind, or None when no session
    /// took it.
    fn linear_pass_on(
        &mut self,
        agent_session: &str,
        text: String,
        interrupt: bool,
        cx: &mut Context<Self>,
    ) -> Option<(crate::linear::follow::Pending, bool)> {
        let sid = self
            .linear_run
            .runs
            .by_agent_session(agent_session)?
            .session
            .clone()?;
        let place = self.store.session(&sid)?.place.clone();
        self.admit(&place).ok()?;
        let thread = self.ensure_live(&sid, cx).ok()?;
        let waits = thread.update(cx, |t, cx| {
            // A question that waits is answered by this reply: it is skipped,
            // and the reply goes to the agent as the next message.
            for _ in 0..SKIPS {
                if !skips_question(pending_in(t)) {
                    break;
                }
                t.skip_question(cx);
            }
            let waits = (pending_in(t), t.is_busy());
            // Held follow-ups from before a restart would keep this one back.
            if t.queue_paused() {
                t.resume_queue(cx);
            }
            if interrupt {
                t.interrupt_with(text, cx);
            } else {
                t.send(text, cx);
            }
            // The cancelled turn still winds down: its end is not the run's.
            (
                waits,
                interrupt && waits.1 && t.is_busy() && t.is_cancelling(),
            )
        });
        let (waits, winding_down) = waits;
        if let Some(s) = self.store.session_mut(&sid) {
            s.updated = unix_now();
        }
        self.inbox_changed(cx);
        let mut revived = false;
        if let Some(run) = self.linear_run.runs.by_agent_session(agent_session) {
            run.stopped = stopped_after_reply(run.stopped, run.state, winding_down);
            if let Some(to) = after_reply(run.state) {
                run.state = to;
                run.asked = None;
                revived = true;
            }
        }
        if revived {
            self.linear_watch(cx);
        }
        Some(waits)
    }

    /// Runs the stops held for `agent_session`, once nothing else for it is
    /// in flight.
    fn linear_release_stops(&mut self, agent_session: &str, cx: &mut Context<Self>) {
        let stops = release_stops(
            &mut self.linear_run.held_stops,
            &mut self.linear_run.in_flight,
            agent_session,
        );
        for d in stops {
            if self.linear_gate().is_some() {
                self.linear_stop(&d, cx);
            }
        }
    }

    fn linear_stop(&mut self, d: &Delivery, cx: &mut Context<Self>) {
        if hold_stop(&self.linear_run.in_flight, &d.agent_session) {
            // In flight too, so a frame sent again is ignored, not accepted.
            let flight = d.agent_session.clone();
            self.linear_run.in_flight.insert(d.id.clone(), flight);
            self.linear_run.held_stops.push(d.clone());
            return;
        }
        let run = self.linear_run.runs.by_agent_session(&d.agent_session);
        let decided = stop(run_state(run.as_deref(), &d.organization));
        let session = run.and_then(|r| r.session.clone());
        self.linear_run.runs.mark_seen(&d.id);
        if matches!(decided, Stop::Dequeue | Stop::Cancel)
            && let Some(run) = self.linear_run.runs.by_agent_session(&d.agent_session)
        {
            // Done before the cancel, so the turn's end is not taken for a
            // finish. A run still being made sees it and does not start.
            run.state = RunState::Done;
        }
        if decided == Stop::Cancel
            && let Some(thread) = session.as_deref().and_then(|s| self.thread(s))
        {
            // While the cancel's turn winds down, its events must not move
            // the run again.
            let busy = thread.read(cx).is_busy();
            self.linear_mark_stopped(&d.agent_session, busy);
            thread.update(cx, |t, cx| t.cancel(cx));
            if busy && !thread.read(cx).is_cancelling() {
                self.linear_mark_stopped(&d.agent_session, false);
            }
        }
        match decided {
            Stop::Nothing => self.linear_save_runs(),
            Stop::Report | Stop::Dequeue | Stop::Cancel => {
                let line = STOPPED.to_string();
                self.linear_report(&d.agent_session, ReportKind::Response, line, None, cx);
            }
        }
        self.linear_accept(&d.id, cx);
        if decided == Stop::Cancel {
            self.linear_start_waiting(cx);
        }
    }

    fn linear_mark_stopped(&mut self, agent_session: &str, stopped: bool) {
        if let Some(run) = self.linear_run.runs.by_agent_session(agent_session) {
            run.stopped = stopped;
        }
    }

    // ---- what the session does -------------------------------------------

    /// A session event, for a session that works on a Linear issue. Cheap
    /// for any other session: one lookup, then nothing.
    pub(super) fn linear_thread_event(
        &mut self,
        id: &str,
        thread: &Entity<Thread>,
        ev: &ThreadEvent,
        cx: &mut Context<Self>,
    ) {
        let event = match ev {
            ThreadEvent::StatusChanged | ThreadEvent::NeedsPermission => Event::Status,
            ThreadEvent::Finished { stop_reason } => Event::Finished(stop_reason),
            _ => return,
        };
        if self.linear_run.runs.by_session(id).is_none() {
            return;
        }
        let t = thread.read(cx);
        let ask = ask_of(t, &computer());
        let status = t.status();
        let now = Now {
            working: status == ThreadStatus::Working,
            failed: matches!(status, ThreadStatus::Error(_)),
            asking: ask.is_some(),
            continues: !t.queued().is_empty() && !t.queue_paused(),
            background: t.left_work_running(),
        };
        let reply = matches!(event, Event::Finished(_)).then(|| turn_reply(t.entries()));
        let session_pr = t.meta().pr.clone();
        let Some(run) = self.linear_run.runs.by_session(id) else {
            return;
        };
        let key = ask.as_ref().map(|(k, _)| k.as_str());
        let Some(effect) = advance(run, event, now, key) else {
            return;
        };
        let to = run.state;
        if effect.step == Step::Background {
            // Sent when the agent's own turn after the background work ends,
            // or by the timer if that never comes.
            let body = to_report(Seen::Finished {
                reply: reply.clone().unwrap_or_default(),
            })
            .map(|(_, body)| body)
            .unwrap_or_default();
            let since = run.held.as_ref().map_or_else(unix_now, |h| h.since);
            run.held = Some(Held {
                since,
                body,
                link: None,
            });
        }
        let agent_session = run.agent_session.clone();
        let seen = match effect.step {
            Step::Ask => ask.map(|(_, seen)| seen),
            Step::Finished => Some(Seen::Finished {
                reply: reply.unwrap_or_default(),
            }),
            Step::Failed => Some(Seen::Failed(match status {
                ThreadStatus::Error(msg) => msg,
                _ => String::new(),
            })),
            Step::Cancelled => Some(Seen::Cancelled),
            Step::Working | Step::Background | Step::Nothing => None,
        };
        let report = seen.filter(|_| effect.report).and_then(to_report);
        let link = matches!(effect.step, Step::Finished | Step::Background)
            .then(|| pr_link(session_pr.as_deref(), None))
            .flatten();
        let link = self.linear_hold_link(id, effect.step, link);
        match report {
            Some((kind, body)) => self.linear_report(&agent_session, kind, body, link, cx),
            None => self.linear_save_runs(),
        }
        if to == RunState::Running {
            self.linear_watch(cx);
        }
        if effect.frees {
            self.linear_start_waiting(cx);
        }
    }

    /// At start: no turn survives a restart, so a run that was at work was
    /// cut off, and a run whose session was removed meanwhile is over. Their
    /// issues hear it, and the runs that wait start in the freed slots.
    pub(super) fn linear_recover(&mut self, cx: &mut Context<Self>) {
        let gone: Vec<String> = self
            .linear_run
            .runs
            .session_ids()
            .into_iter()
            .filter(|s| self.store.session(s).is_none())
            .collect();
        let removed = self.linear_run.runs.forget_sessions(&gone);
        let before = self.linear_run.runs.clone();
        let lost = self.linear_run.runs.interrupted(&|_| false);
        self.linear_run.runs.clear_stopped();
        if !removed.is_empty() || self.linear_run.runs != before {
            self.linear_save_runs();
        }
        let line = removed_line(&computer());
        for agent_session in removed {
            self.linear_report(&agent_session, ReportKind::Error, line.clone(), None, cx);
        }
        let line = closed_line(&computer());
        for agent_session in lost {
            self.linear_report(&agent_session, ReportKind::Error, line.clone(), None, cx);
        }
        self.linear_start_waiting(cx);
        self.linear_watch(cx);
    }

    /// Sessions removed from the server: their runs end, their issues hear
    /// it, and the freed slots go to the runs that wait.
    pub(super) fn linear_sessions_removed(&mut self, ids: &[String], cx: &mut Context<Self>) {
        if ids.is_empty() {
            return;
        }
        let before = self.linear_run.runs.clone();
        let told = self.linear_run.runs.forget_sessions(ids);
        if self.linear_run.runs == before {
            return;
        }
        self.linear_save_runs();
        let line = removed_line(&computer());
        for agent_session in told {
            self.linear_report(&agent_session, ReportKind::Error, line.clone(), None, cx);
        }
        self.linear_start_waiting(cx);
    }

    // ---- reports ------------------------------------------------------------

    /// Queues a report to Linear and sends the outbox. A report never waits
    /// on the network, and one not sent stays for the next pass.
    fn linear_report(
        &mut self,
        agent_session: &str,
        kind: ReportKind,
        body: String,
        link: Option<String>,
        cx: &mut Context<Self>,
    ) {
        let now = unix_now();
        self.linear_run.runs.queue(Report {
            agent_session: agent_session.to_string(),
            kind,
            body,
            link,
            made_at: now,
        });
        if let Some(run) = self.linear_run.runs.by_agent_session(agent_session) {
            run.reported_at = now;
        }
        self.linear_save_runs();
        self.linear_flush(cx);
    }

    /// Sends what the outbox holds, in order, off the UI thread.
    pub(super) fn linear_flush(&mut self, cx: &mut Context<Self>) {
        if self.linear_run.flushing {
            self.linear_run.flush_owed = true;
            return;
        }
        let Some((base, session)) = self.linear_gate() else {
            return;
        };
        let due = self.linear_run.runs.due(unix_now());
        if due.is_empty() {
            return;
        }
        self.linear_run.flushing = true;
        self.linear_run.flush_owed = false;
        cx.spawn(async move |this, cx| {
            let out = cx
                .background_spawn(crate::guard::task_or(
                    async move { flush(&base, &session, due) },
                    |_| Flushed::default(),
                ))
                .await;
            let _ = this.update(cx, |this, cx| this.linear_flushed(out, cx));
        })
        .detach();
    }

    fn linear_flushed(&mut self, out: Flushed, cx: &mut Context<Self>) {
        self.linear_run.flushing = false;
        for r in &out.sent {
            self.linear_run.runs.sent(r);
        }
        for r in &out.dropped {
            self.linear_run.runs.dropped(r);
        }
        if !out.sent.is_empty() || !out.dropped.is_empty() {
            self.linear_save_runs();
        }
        if out.signed_out {
            // `login` again brings the reports back; they are kept a day.
            eprintln!("arakun server: Linear: the account refused the sign-in");
            return;
        }
        if std::mem::take(&mut self.linear_run.flush_owed) {
            self.linear_flush(cx);
        }
        self.linear_watch(cx);
    }

    /// Tells the relay this delivery is handled, off the UI thread. A failure
    /// is let go: the relay sends it again, and it is accepted again then.
    fn linear_accept(&self, delivery: &str, cx: &mut Context<Self>) {
        let Some((base, session)) = self.linear_gate() else {
            return;
        };
        let id = delivery.to_string();
        cx.background_spawn(crate::guard::task(async move {
            let _ = cloud::linear_accept(&base, &session, &id);
        }))
        .detach();
    }

    /// Starts the one timer, while a run works or reports wait. It tells
    /// Linear a quiet session still works and sends the outbox again.
    fn linear_watch(&mut self, cx: &mut Context<Self>) {
        if self.linear_run.watching || !self.linear_needs_watch() {
            return;
        }
        self.linear_run.watching = true;
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(TICK).await;
                let more = this.update(cx, |this, cx| this.linear_tick(cx));
                if !more.unwrap_or(false) {
                    break;
                }
            }
        })
        .detach();
    }

    /// A held turn keeps its pull request with its reply; any other step
    /// sends the link now.
    fn linear_hold_link(&mut self, id: &str, step: Step, link: Option<String>) -> Option<String> {
        if step != Step::Background {
            return link;
        }
        if let Some(held) = self
            .linear_run
            .runs
            .by_session(id)
            .and_then(|r| r.held.as_mut())
        {
            held.link = link;
        }
        None
    }

    fn linear_needs_watch(&self) -> bool {
        self.linear_run.runs.any_running() || self.linear_run.runs.unsent()
    }

    fn linear_tick(&mut self, cx: &mut Context<Self>) -> bool {
        let now = unix_now();
        let quiet = if self.linear_gate().is_some() {
            self.linear_run.runs.quiet(now)
        } else {
            Vec::new()
        };
        for agent_session in quiet {
            let line = STILL_WORKING.to_string();
            self.linear_report(&agent_session, ReportKind::Thought, line, None, cx);
        }
        let overdue = self.linear_run.runs.overdue(now);
        let freed = !overdue.is_empty();
        for (agent_session, held) in overdue {
            self.linear_report(
                &agent_session,
                ReportKind::Response,
                held.body,
                held.link,
                cx,
            );
        }
        if freed {
            self.linear_start_waiting(cx);
        }
        // Reports older than a day go, so a signed-out outbox ends the timer.
        self.linear_run.runs.due(now);
        self.linear_flush(cx);
        let more = self.linear_needs_watch();
        self.linear_run.watching = more;
        more
    }
}

/// Off the UI thread: the worktree the issue's session works in, as the app
/// makes it. Never the project's own folder or a base branch: the session
/// commits there. A branch name git refuses falls back once to the issue's
/// own (`linear/eng-412`).
fn make_worktree(project: &Path, wanted: &str, identifier: &str) -> Made {
    let name = crate::server::store::repo(project)
        .and_then(|(root, _)| crate::server::store::repo_name(&root))
        .ok_or("that folder is not in a git repository")?;
    let branch = work_branch(wanted, identifier, &bases(project));
    let own = own_branch(identifier);
    let mut tries = vec![branch];
    if tries[0] != own {
        tries.push(own);
    }
    let mut last = String::new();
    for (i, branch) in tries.iter().enumerate() {
        let may_retry = i + 1 < tries.len();
        match env::worktree_on(project, &name, branch, None) {
            Ok((path, _)) if same_folder(&path, project) => {
                last = "its branch is the one the project's main folder is on".into();
                if !may_retry {
                    break;
                }
            }
            Ok((path, _)) => {
                let path = path.canonicalize().unwrap_or(path);
                return Ok((path.clone(), PlaceMeta::read(&path), branch.clone()));
            }
            Err(e) => {
                let retry = next_branch(&e, branch, &tries[tries.len() - 1], may_retry);
                last = e;
                if retry.is_none() {
                    break;
                }
            }
        }
    }
    Err(last)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn git(dir: &Path, args: &[&str]) {
        let ok = std::process::Command::new("git")
            .args(args)
            .current_dir(dir)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_AUTHOR_NAME", "t")
            .env("GIT_AUTHOR_EMAIL", "t@t")
            .env("GIT_COMMITTER_NAME", "t")
            .env("GIT_COMMITTER_EMAIL", "t@t")
            .status()
            .unwrap()
            .success();
        assert!(ok, "git {args:?}");
    }

    #[test]
    fn a_delegated_session_never_works_in_the_projects_own_folder() {
        let dir = std::env::temp_dir().join(format!("arakun-linear-run-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        git(&dir, &["init", "-q", "-b", "main"]);
        git(&dir, &["commit", "-q", "--allow-empty", "-m", "first"]);
        // The issue's branch is the one checked out: the issue's own is used.
        git(&dir, &["switch", "-q", "-c", "eng-1-fix"]);
        let bases = bases(&dir);
        assert!(bases.contains(&"eng-1-fix".to_string()), "{bases:?}");
        assert_eq!(work_branch("eng-1-fix", "ENG-1", &bases), "linear/eng-1");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_same_folder_is_told_through_a_link() {
        let dir = std::env::temp_dir();
        assert!(same_folder(&dir, &dir.join(".")));
        assert!(!same_folder(&dir, &dir.join("no-such-arakun-folder")));
    }
}
