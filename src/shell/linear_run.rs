//! Running a session for an issue delegated to Arakun in Linear, as the relay
//! passes it on.
//!
//! The relay's word starts nothing by itself: the issue is read with the
//! person's own Linear key, and the guard (`target_for`) must say yes for it
//! on this computer. Text from Linear goes only into the prompt, never a log.

use std::path::{Path, PathBuf};

use gpui::{AppContext as _, Context, Window};

use super::Shell;
use super::linear_sync::{Gate, may_sync};
use super::sessions::queued_from;
use crate::cloud::{self, LinearActivity, LinearLinkOut};
use crate::linear::delegation::{Delegation, Refusal, target_for};
use crate::linear::issues::{self, Detail};
use crate::linear::runs::{Added, Report, ReportKind, Run, RunState};
use crate::linear::{ApiError, Session, prompt};
use crate::model::unix_now;
use crate::phone::linear::{Delivery, DeliveryKind};

/// The most characters of a reason shown in Linear: git and Linear can say a lot.
const REASON_LIMIT: usize = 300;

/// What a delivery's first look decides, before the issue is read.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum First {
    /// No Arakun account, flag or relay label: the relay sends it again later.
    Ignore,
    /// Handled before: accepted again so the relay stops sending it.
    AcceptOnly,
    /// No sign-in for the delivery's Linear workspace here.
    RefuseNoSignIn,
    Read,
}

pub fn first_step(seen: bool, gate: bool, signed_in_workspace: bool) -> First {
    if !gate {
        First::Ignore
    } else if seen {
        First::AcceptOnly
    } else if !signed_in_workspace {
        First::RefuseNoSignIn
    } else {
        First::Read
    }
}

pub fn refusal_line(r: &Refusal, computer: &str) -> String {
    match r {
        Refusal::Off => format!(
            "{computer} no longer takes work from Linear. Turn it on in Arakun's Linear tab, \
             then delegate again."
        ),
        Refusal::Unmapped => format!(
            "This issue's team and project are not mapped on {computer}. Map one in Arakun's \
             Linear tab, then delegate again."
        ),
        Refusal::ProjectGone => format!(
            "The project this was mapped to is no longer in Arakun on {computer}. Fix the \
             mapping in Arakun's Linear tab, then delegate again."
        ),
    }
}

pub fn started_line(project: &str, computer: &str, branch: &str) -> String {
    format!("Started in {project} on {computer}, on the branch {branch}.")
}

pub fn waiting_line(computer: &str, n: usize) -> String {
    format!(
        "Waiting on {computer}: {n} other sessions from Linear are running. This one starts \
         when one finishes."
    )
}

pub fn no_sign_in_line(computer: &str) -> String {
    format!(
        "{computer} is not signed in to this Linear workspace. Sign in in Arakun's Linear tab, \
         then delegate again."
    )
}

pub fn unreadable_line(reason: &str) -> String {
    format!(
        "Arakun could not read this issue from Linear: {}",
        short(reason)
    )
}

pub fn no_env_line(reason: &str) -> String {
    format!(
        "Arakun could not make an environment for this: {}",
        short(reason)
    )
}

fn failed_line(reason: &str) -> String {
    format!("The session failed: {}", short(reason))
}

fn short(reason: &str) -> String {
    reason.chars().take(REASON_LIMIT).collect()
}

/// The branch a delegated session works on. Never one of the project's base
/// branches: the session commits there, and a pull request needs its own branch.
pub fn work_branch(branch: &str, identifier: &str, bases: &[String]) -> String {
    if bases.iter().any(|b| b.eq_ignore_ascii_case(branch)) {
        own_branch(identifier)
    } else {
        branch.to_string()
    }
}

/// The branch for an issue when Linear's own name for it cannot be used.
pub fn own_branch(identifier: &str) -> String {
    format!("linear/{}", identifier.to_lowercase())
}

fn activity(kind: ReportKind) -> LinearActivity {
    match kind {
        ReportKind::Thought => LinearActivity::Thought,
        ReportKind::Elicitation => LinearActivity::Elicitation,
        ReportKind::Response => LinearActivity::Response,
        ReportKind::Error => LinearActivity::Error,
    }
}

/// An id the server takes in a path. Any other would be refused before it is
/// sent, every time, and hold up the reports behind it for a day.
fn sendable(agent_session: &str) -> bool {
    (1..=64).contains(&agent_session.len())
        && agent_session
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-')
}

/// What one pass over the outbox did.
#[derive(Debug, Default)]
pub struct Flushed {
    pub sent: Vec<Report>,
    /// Linear refused these for good.
    pub dropped: Vec<Report>,
    pub signed_out: bool,
}

/// Sends `reports` in order. It blocks, so it runs off the UI thread. A
/// limit, no network or a server error stops it and keeps the rest for later.
pub fn flush(base: &str, session: &str, reports: Vec<Report>) -> Flushed {
    let mut out = Flushed::default();
    for r in reports {
        if !sendable(&r.agent_session) {
            out.dropped.push(r);
            continue;
        }
        let links: Vec<LinearLinkOut> = r
            .link
            .iter()
            .map(|url| LinearLinkOut {
                label: "Pull request".into(),
                url: url.clone(),
            })
            .collect();
        let kind = activity(r.kind);
        match cloud::linear_activity(base, session, &r.agent_session, kind, &r.body, &links) {
            Ok(()) => out.sent.push(r),
            Err(cloud::Error::Status { status, .. }) if status != 429 => out.dropped.push(r),
            Err(cloud::Error::SignedOut) => {
                out.signed_out = true;
                break;
            }
            Err(_) => break,
        }
    }
    out
}

/// This computer's name, as the relay knows it.
fn computer() -> String {
    crate::phone::host_name()
}

/// The outbox's sender: one pass at a time.
#[derive(Default)]
pub(super) struct Outbox {
    flushing: bool,
    /// A report came in during a pass: one more pass after it.
    owed: bool,
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
        // Without the gate it is not recorded: the relay sends it again later.
        let seen = gate && self.linear_runs.seen(&d.id);
        let sessions = if gate && !seen {
            crate::linear::sessions()
        } else {
            Vec::new()
        };
        let sign_in = crate::linear::session_for(&sessions, &d.organization).cloned();
        match (first_step(seen, gate, sign_in.is_some()), sign_in) {
            (First::Ignore, _) => {}
            (First::AcceptOnly, _) => self.linear_accept(&d.id, cx),
            (First::Read, Some(sign_in)) => {
                // Recorded before anything goes out, so a crash never runs it twice.
                self.linear_save();
                match d.kind {
                    DeliveryKind::Created => {
                        let start = Start {
                            agent_session: d.agent_session,
                            organization: d.organization,
                            issue: d.issue,
                            delivery: Some(d.id),
                        };
                        self.linear_read(start, sign_in, window, cx);
                    }
                    DeliveryKind::Prompt | DeliveryKind::Stop => {
                        self.linear_follow_up(d, window, cx)
                    }
                }
            }
            (First::RefuseNoSignIn | First::Read, _) => {
                self.linear_save();
                let line = no_sign_in_line(&computer());
                self.linear_report(&d.agent_session, ReportKind::Error, line, None, cx);
                self.linear_accept(&d.id, cx);
            }
        }
    }

    /// A prompt or a stop for a run. Taking them is a later change; until
    /// then they are accepted, so the relay stops sending them.
    fn linear_follow_up(&mut self, d: Delivery, _window: &mut Window, cx: &mut Context<Self>) {
        self.linear_accept(&d.id, cx);
    }

    /// Starts the runs that wait, while there are free slots. Each one's
    /// issue is read again and the guard asked again: either may have changed.
    pub(super) fn linear_start_waiting(&mut self, window: &mut Window, cx: &mut Context<Self>) {
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
                None => self.linear_fail(&start, no_sign_in_line(&computer()), window, cx),
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
                .background_spawn(crate::guard::task(async move {
                    issues::detail(&sign_in, &issue)
                }))
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
        let d = Delegation::load();
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
                Err(e) => return this.linear_fail(&start, no_env_line(&e), window, cx),
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
        if let Err(r) = self.linear_guard(&start.organization, &detail) {
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
        if let Some(run) = self.linear_runs.by_agent_session(&start.agent_session) {
            run.session = Some(session.to_string());
            run.environment = Some(env_path);
            run.branch = branch.clone();
            run.state = RunState::Running;
        }
        self.linear_save();
        let line = started_line(&name, &computer(), &branch);
        self.linear_report(&start.agent_session, ReportKind::Thought, line, None, cx);
        if let Some(id) = &start.delivery {
            self.linear_accept(id, cx);
        }
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
    fn linear_gate(&self, cx: &Context<Self>) -> Option<Gate> {
        let teams = self.teams.read(cx);
        may_sync(
            teams.account(),
            teams.linear_enabled(),
            crate::phone::Settings::load().relay_label(),
        )
    }

    fn linear_save(&self) {
        if let Err(e) = self.linear_runs.save() {
            eprintln!("arakun: {e}");
        }
    }

    /// Tells the relay this delivery is handled, off the UI thread. A failure
    /// is let go: the relay sends it again, and it is accepted again then.
    fn linear_accept(&self, delivery: &str, cx: &mut Context<Self>) {
        let Some(Gate { base, session, .. }) = self.linear_gate(cx) else {
            return;
        };
        let id = delivery.to_string();
        cx.background_spawn(crate::guard::task(async move {
            let _ = cloud::linear_accept(&base, &session, &id);
        }))
        .detach();
    }

    /// Queues a report to Linear and sends the outbox. A report never waits
    /// on the network, and one not sent stays for the next pass.
    pub(super) fn linear_report(
        &mut self,
        agent_session: &str,
        kind: ReportKind,
        body: String,
        link: Option<String>,
        cx: &mut Context<Self>,
    ) {
        let now = unix_now();
        self.linear_runs.queue(Report {
            agent_session: agent_session.to_string(),
            kind,
            body,
            link,
            made_at: now,
        });
        if let Some(run) = self.linear_runs.by_agent_session(agent_session) {
            run.reported_at = now;
        }
        self.linear_save();
        self.linear_flush(cx);
    }

    /// Sends what the outbox holds, in order, off the UI thread.
    pub(super) fn linear_flush(&mut self, cx: &mut Context<Self>) {
        if self.linear_outbox.flushing {
            self.linear_outbox.owed = true;
            return;
        }
        let Some(Gate { base, session, .. }) = self.linear_gate(cx) else {
            return;
        };
        let due = self.linear_runs.due(unix_now());
        if due.is_empty() {
            return;
        }
        self.linear_outbox.flushing = true;
        self.linear_outbox.owed = false;
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
        self.linear_outbox.flushing = false;
        for r in &out.sent {
            self.linear_runs.sent(r);
        }
        for r in &out.dropped {
            self.linear_runs.dropped(r);
        }
        if !out.sent.is_empty() || !out.dropped.is_empty() {
            self.linear_save();
        }
        if out.signed_out {
            self.account_call_failed(cloud::Error::SignedOut, cx);
            return;
        }
        if std::mem::take(&mut self.linear_outbox.owed) {
            self.linear_flush(cx);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shell::linear_sync::tests::serve;

    #[test]
    fn each_refusal_has_its_line() {
        assert_eq!(
            refusal_line(&Refusal::Off, "rik-mac"),
            "rik-mac no longer takes work from Linear. Turn it on in Arakun's Linear tab, \
             then delegate again."
        );
        assert_eq!(
            refusal_line(&Refusal::Unmapped, "rik-mac"),
            "This issue's team and project are not mapped on rik-mac. Map one in Arakun's \
             Linear tab, then delegate again."
        );
        assert_eq!(
            refusal_line(&Refusal::ProjectGone, "rik-mac"),
            "The project this was mapped to is no longer in Arakun on rik-mac. Fix the \
             mapping in Arakun's Linear tab, then delegate again."
        );
    }

    #[test]
    fn the_started_line_names_the_project_computer_and_branch() {
        assert_eq!(
            started_line("arakun", "rik-mac", "rik/eng-412-fix"),
            "Started in arakun on rik-mac, on the branch rik/eng-412-fix."
        );
    }

    #[test]
    fn the_other_lines_read_as_in_the_plan() {
        assert_eq!(
            waiting_line("rik-mac", 3),
            "Waiting on rik-mac: 3 other sessions from Linear are running. This one starts \
             when one finishes."
        );
        assert_eq!(
            no_sign_in_line("rik-mac"),
            "rik-mac is not signed in to this Linear workspace. Sign in in Arakun's Linear \
             tab, then delegate again."
        );
        assert_eq!(
            unreadable_line("Linear refused the sign-in. Sign in again."),
            "Arakun could not read this issue from Linear: Linear refused the sign-in. \
             Sign in again."
        );
        assert_eq!(
            no_env_line("git said \"fatal: bad\""),
            "Arakun could not make an environment for this: git said \"fatal: bad\""
        );
    }

    #[test]
    fn a_long_reason_is_cut_on_a_character() {
        let line = no_env_line(&"é".repeat(REASON_LIMIT + 50));
        assert_eq!(line.matches('é').count(), REASON_LIMIT);
        let line = unreadable_line(&"x".repeat(REASON_LIMIT + 1));
        assert!(line.ends_with(&format!(": {}", "x".repeat(REASON_LIMIT))));
    }

    #[test]
    fn the_first_look_goes_in_order() {
        // No account, flag or label: the relay sends it again later.
        for (seen, signed_in) in [(false, false), (false, true), (true, false), (true, true)] {
            assert_eq!(first_step(seen, false, signed_in), First::Ignore);
        }
        // Seen before: accepted again, whatever else holds.
        assert_eq!(first_step(true, true, false), First::AcceptOnly);
        assert_eq!(first_step(true, true, true), First::AcceptOnly);
        assert_eq!(first_step(false, true, false), First::RefuseNoSignIn);
        assert_eq!(first_step(false, true, true), First::Read);
    }

    #[test]
    fn a_session_never_works_on_a_base_branch() {
        let bases = ["main".to_string(), "develop".to_string()];
        assert_eq!(
            work_branch("rik/eng-412-fix", "ENG-412", &bases),
            "rik/eng-412-fix"
        );
        assert_eq!(work_branch("develop", "ENG-412", &bases), "linear/eng-412");
        assert_eq!(work_branch("Main", "ENG-412", &bases), "linear/eng-412");
        assert_eq!(work_branch("develop", "ENG-412", &[]), "develop");
    }

    fn report(body: &str) -> Report {
        Report {
            agent_session: "as-1".into(),
            kind: ReportKind::Thought,
            body: body.into(),
            link: None,
            made_at: 1,
        }
    }

    fn bodies(rs: &[Report]) -> Vec<&str> {
        rs.iter().map(|r| r.body.as_str()).collect()
    }

    #[test]
    fn the_outbox_goes_in_order() {
        let (base, rx) = serve(vec![(204, ""), (204, ""), (204, "")]);
        let mut last = report("three");
        last.kind = ReportKind::Response;
        last.link = Some("https://github.com/a/b/pull/1".into());
        let out = flush(&base, "s", vec![report("one"), report("two"), last]);
        assert_eq!(bodies(&out.sent), ["one", "two", "three"]);
        assert!(out.dropped.is_empty() && !out.signed_out);
        let got: Vec<String> = rx.try_iter().collect();
        assert_eq!(got.len(), 3);
        assert!(got[0].starts_with("POST /linear/sessions/as-1/activity "));
        assert!(got[0].contains(r#""body":"one""#) && got[1].contains(r#""body":"two""#));
        assert!(got[2].contains(r#""type":"response""#), "{}", got[2]);
        assert!(
            got[2].contains(r#"{"label":"Pull request","url":"https://github.com/a/b/pull/1"}"#),
            "{}",
            got[2]
        );
    }

    #[test]
    fn a_limit_stops_the_outbox_and_keeps_the_rest() {
        let (base, rx) = serve(vec![(204, ""), (429, r#"{"error": "slow down"}"#)]);
        let out = flush(
            &base,
            "s",
            vec![report("one"), report("two"), report("three")],
        );
        assert_eq!(bodies(&out.sent), ["one"]);
        assert!(out.dropped.is_empty() && !out.signed_out);
        assert_eq!(rx.try_iter().count(), 2);
    }

    #[test]
    fn a_refused_report_is_dropped_and_the_next_goes() {
        let (base, rx) = serve(vec![
            (204, ""),
            (404, r#"{"error": "no such session"}"#),
            (204, ""),
        ]);
        let out = flush(
            &base,
            "s",
            vec![report("one"), report("two"), report("three")],
        );
        assert_eq!(bodies(&out.sent), ["one", "three"]);
        assert_eq!(bodies(&out.dropped), ["two"]);
        assert_eq!(rx.try_iter().count(), 3);
    }

    #[test]
    fn signed_out_nothing_more_is_sent() {
        let (base, rx) = serve(vec![(401, r#"{"error": "not signed in"}"#), (204, "")]);
        let out = flush(&base, "s", vec![report("one"), report("two")]);
        assert!(out.signed_out && out.sent.is_empty() && out.dropped.is_empty());
        assert_eq!(rx.try_iter().count(), 1);
    }

    #[test]
    fn offline_the_outbox_keeps_everything() {
        // A port nobody listens on.
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        drop(listener);
        let out = flush(&base, "s", vec![report("one"), report("two")]);
        assert!(out.sent.is_empty() && out.dropped.is_empty() && !out.signed_out);
    }

    #[test]
    fn a_report_for_an_id_the_server_cannot_take_is_dropped_unsent() {
        let (base, rx) = serve(vec![(204, "")]);
        let mut bad = report("bad");
        bad.agent_session = "as/../x".into();
        let out = flush(&base, "s", vec![bad, report("good")]);
        assert_eq!(bodies(&out.dropped), ["bad"]);
        assert_eq!(bodies(&out.sent), ["good"]);
        assert_eq!(rx.try_iter().count(), 1);
    }
}
