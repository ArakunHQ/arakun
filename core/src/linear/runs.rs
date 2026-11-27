//! The record of sessions started from Linear.
//!
//! `app_dir/linear/runs.json` holds the deliveries already handled, each run
//! with its state, and the reports Linear has not taken yet. It holds ids,
//! paths and an issue's identifier; only unsent reports carry Linear's text,
//! and they are dropped after a day.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::phone::linear::GuidanceRule;

/// Delegated sessions at work at once, unless the person chose another
/// number for the machine. A computer back after days must not start dozens
/// of agents; the rest wait their turn.
pub const DEFAULT_SLOTS: usize = 3;
/// The most a person may choose: each slot is an agent at work.
pub const MOST_SLOTS: usize = 10;
/// The most replies kept for a run that waits for a slot.
pub const KEPT_REPLIES: usize = 8;
/// Delivery ids remembered, so a delivery sent again is not run twice.
pub const HANDLED_KEPT: usize = 500;
/// How long a report Linear has not taken is tried again.
pub const REPORT_KEPT_SECS: u64 = 24 * 3600;
/// A running session silent this long tells Linear it is still working.
pub const QUIET_SECS: u64 = 25 * 60;
/// The least time between two progress reports of one run. Linear's limit
/// is shared by all of a person's runs and replies.
pub const PROGRESS_SECS: u64 = 30;
/// A progress report not sent by then says what the session did long ago:
/// it is dropped instead.
pub const PROGRESS_KEPT_SECS: u64 = 5 * 60;
/// The least time between two plans of one run while its turn goes on. The
/// plan a turn ends with always goes.
pub const PLAN_SECS: u64 = 15;

/// How long a run waits on work its session left in the background before
/// its reply goes to Linear anyway. A server started in the background never
/// ends, and the run must not hold its slot for good.
pub const HOLD_SECS: u64 = 3 * 60 * 60;

/// Reports kept for Linear at most. A server that takes none for a day must
/// not grow the file without end; the oldest go first.
pub const OUTBOX_KEPT: usize = 500;

/// Finished runs kept, so a follow-up can still find its run.
const DONE_KEPT: usize = 200;
/// The most characters of a reply sent to Linear.
const REPLY_LIMIT: usize = 7_500;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunState {
    /// All slots were taken; it starts when one frees up.
    Waiting,
    Running,
    /// The session waits on a person. It takes no slot: a reply may take
    /// days, and new work must not wait for it.
    Asked,
    Done,
    /// Arakun closed while it ran.
    Interrupted,
}

/// One Linear agent session and the Arakun session that works on it. Debug
/// leaves out the branch: Linear builds it from the issue's title.
#[derive(Clone, PartialEq, Serialize, Deserialize)]
pub struct Run {
    /// Linear's agent session id.
    pub agent_session: String,
    /// The Linear organization id.
    pub organization: String,
    /// The issue id.
    pub issue: String,
    /// `ENG-412`, to name the run without reading the issue again.
    pub identifier: String,
    /// The root of the Arakun project.
    pub project: PathBuf,
    /// The environment made for it, once there is one.
    #[serde(default)]
    pub environment: Option<PathBuf>,
    #[serde(default)]
    pub branch: String,
    /// The Arakun session id, once the session exists.
    #[serde(default)]
    pub session: Option<String>,
    pub state: RunState,
    /// When Linear last heard from this run, in unix seconds.
    #[serde(default)]
    pub reported_at: u64,
    /// A hash of the question or permission last reported, so one pending
    /// ask is told once. Never its text.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub asked: Option<String>,
    /// Linear stopped it while its session had a turn: the cancel's own
    /// events must not move it, until that turn ends.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub stopped: bool,
    /// The turn ended while work it left in the background runs on: its
    /// reply, sent once that work is done or `HOLD_SECS` passed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub held: Option<Held>,
    /// The last progress queued for Linear. Not saved: no turn survives a
    /// restart.
    #[serde(skip)]
    pub progress: Option<Progressed>,
    /// The last plan queued, and when. Not saved, like `progress`.
    #[serde(skip)]
    pub plan: Option<(u64, Vec<PlanItem>)>,
    /// The pull request link Linear was last sent for this run, so it goes
    /// once.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub linked: Option<String>,
    /// Replies that came in Linear while the run waited for a slot, as the
    /// ids of their agent activities, oldest first. Never their text: it is
    /// read from Linear when the session starts.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub kept: Vec<String>,
    /// The comment that mentioned Arakun, when a mention started the run.
    /// Its id only, so it is saved: the text is read from Linear when the
    /// session starts.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub comment: Option<String>,
    /// The guidance Linear sent with the delegation, for the first prompt.
    /// Never saved, as no text from Linear is: a run that waits across a
    /// restart starts without it.
    #[serde(skip)]
    pub guidance: Vec<GuidanceRule>,
}

/// The progress of a run that Linear last heard.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Progressed {
    /// When it was queued, in unix seconds.
    pub at: u64,
    /// The step as one line, so the same step is not sent again.
    pub step: String,
    /// The entry of the session the next digest starts at.
    pub digested: usize,
}

/// What Linear hears of a working run at one time.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Progress {
    pub agent_session: String,
    /// What the session said and did since the last digest: a lasting thought.
    pub digest: Option<String>,
    /// Whether the step it is on now goes, as an ephemeral action.
    pub step: bool,
}

/// A reply kept back while the session's background work runs. Debug leaves
/// out the body: it can hold the agent's words about private code.
#[derive(Clone, PartialEq, Serialize, Deserialize)]
pub struct Held {
    /// When the first held turn ended, in unix seconds.
    pub since: u64,
    pub body: String,
    #[serde(default)]
    pub link: Option<String>,
}

impl std::fmt::Debug for Held {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Held")
            .field("since", &self.since)
            .field("body", &format_args!("…"))
            .field("link", &self.link.is_some())
            .finish()
    }
}

impl std::fmt::Debug for Run {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Run")
            .field("agent_session", &self.agent_session)
            .field("organization", &self.organization)
            .field("issue", &self.issue)
            .field("identifier", &self.identifier)
            .field("project", &self.project)
            .field("environment", &self.environment)
            .field("branch", &format_args!("…"))
            .field("session", &self.session)
            .field("state", &self.state)
            .field("reported_at", &self.reported_at)
            .field("asked", &self.asked.is_some())
            .field("stopped", &self.stopped)
            .field("held", &self.held)
            .field("progress", &self.progress.as_ref().map(|p| p.at))
            .field("plan", &self.plan.as_ref().map(|(at, _)| at))
            .field("linked", &self.linked.is_some())
            .field("kept", &self.kept.len())
            .field("comment", &self.comment)
            .field("guidance", &self.guidance.len())
            .finish()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ReportKind {
    Thought,
    Elicitation,
    Response,
    Error,
    /// The step a working session is on: the body is what it does, the
    /// parameter on what. Linear replaces it with the next activity, and a
    /// newer report makes an unsent one moot.
    Progress,
    /// Only a link for the session, such as its pull request: no activity.
    Link,
    /// The agent's plan, as the session's checklist in Linear. A newer plan
    /// makes an unsent one moot.
    Plan,
}

/// One item of a plan as Linear shows it. `content` is the agent's words,
/// scrubbed and cut.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlanItem {
    pub content: String,
    pub status: PlanItemStatus,
}

/// Linear's statuses of a plan item, in its spelling.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum PlanItemStatus {
    Pending,
    InProgress,
    Completed,
    Canceled,
}

impl std::fmt::Debug for PlanItem {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PlanItem")
            .field("content", &format_args!("…"))
            .field("status", &self.status)
            .finish()
    }
}

/// A report to Linear not sent yet. Debug leaves out the body: it can hold
/// the agent's words about private code.
#[derive(Clone, PartialEq, Serialize, Deserialize)]
pub struct Report {
    pub agent_session: String,
    pub kind: ReportKind,
    pub body: String,
    #[serde(default)]
    pub link: Option<String>,
    /// A question's choices, for Linear to show as ones to pick from.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub options: Vec<String>,
    /// The thought that says the session started on this computer. The
    /// server then marks the issue as being worked on in Linear.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub started: bool,
    /// What a progress step works on: a file, or `a command`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parameter: Option<String>,
    /// The whole plan, for a plan report.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub plan: Vec<PlanItem>,
    pub made_at: u64,
}

impl std::fmt::Debug for Report {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Report")
            .field("agent_session", &self.agent_session)
            .field("kind", &self.kind)
            .field("body", &format_args!("…"))
            .field("link", &self.link.is_some())
            .field("options", &self.options.len())
            .field("started", &self.started)
            .field("parameter", &self.parameter.is_some())
            .field("plan", &self.plan.len())
            .field("made_at", &self.made_at)
            .finish()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Added {
    Start,
    /// `running` runs take the slots, and `ahead` waiting runs start before
    /// this one.
    Wait {
        running: usize,
        ahead: usize,
    },
    /// Linear sent this agent session before.
    Already,
}

/// Each field defaults, so an older file loads and a newer one's extra
/// fields are skipped. A state or kind this version does not know makes
/// the whole file unreadable: it is kept aside and the record starts empty.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Runs {
    #[serde(default)]
    handled: Vec<String>,
    #[serde(default)]
    runs: Vec<Run>,
    #[serde(default)]
    outbox: Vec<Report>,
}

fn file() -> PathBuf {
    crate::paths::shared(crate::paths::LINEAR_RUNS)
}

impl Runs {
    pub fn load() -> Runs {
        Runs::load_from(&file())
    }

    pub fn save(&self) -> Result<(), String> {
        self.save_to(&file())
    }

    /// A damaged file reads as empty and is set aside, not saved over.
    pub fn load_from(file: &Path) -> Runs {
        crate::stored::load(file).unwrap_or_default()
    }

    pub fn save_to(&self, file: &Path) -> Result<(), String> {
        serde_json::to_vec(self)
            .map_err(std::io::Error::other)
            .and_then(|bytes| crate::stored::write(file, &bytes))
            .map_err(|e| format!("Couldn't save the sessions started from Linear: {e}"))
    }

    /// Whether `delivery_id` was handled before. Records nothing: the id is
    /// recorded with the outcome it led to (`mark_seen`), in the same save.
    pub fn was_seen(&self, delivery_id: &str) -> bool {
        self.handled.iter().any(|h| h == delivery_id)
    }

    /// Records `delivery_id` as handled, forgetting the oldest beyond
    /// `HANDLED_KEPT`.
    pub fn mark_seen(&mut self, delivery_id: &str) {
        if self.was_seen(delivery_id) {
            return;
        }
        self.handled.push(delivery_id.to_string());
        let extra = self.handled.len().saturating_sub(HANDLED_KEPT);
        self.handled.drain(..extra);
    }

    /// Whether a run for this issue is still at work (waiting, running or
    /// asking), so a second delegation of it must not start another session.
    pub fn active_for_issue(&self, organization: &str, issue: &str) -> bool {
        self.runs.iter().any(|r| {
            r.organization == organization
                && r.issue == issue
                && matches!(
                    r.state,
                    RunState::Waiting | RunState::Running | RunState::Asked
                )
        })
    }

    /// Stores `run` as `Running` when one of the machine's `slots` is free
    /// and nobody waits, else as `Waiting`.
    pub fn add(&mut self, run: Run, slots: usize) -> Added {
        self.add_at(run, crate::model::unix_now(), slots)
    }

    /// [`Runs::add`] at a given time. The run counts as heard from at `now`,
    /// so "still working" cannot come before its first report.
    pub fn add_at(&mut self, mut run: Run, now: u64, slots: usize) -> Added {
        run.reported_at = now;
        if self
            .runs
            .iter()
            .any(|r| r.agent_session == run.agent_session)
        {
            return Added::Already;
        }
        let busy = self.busy();
        let waiting = self.count(RunState::Waiting);
        // Slots free up before the shell calls `next_waiting` (a run ends, or
        // a restart interrupts them all); a new run must not jump the queue.
        let added = if busy < slots && waiting == 0 {
            run.state = RunState::Running;
            Added::Start
        } else {
            run.state = RunState::Waiting;
            Added::Wait {
                running: busy,
                ahead: waiting,
            }
        };
        self.runs.push(run);
        self.forget_done();
        added
    }

    /// The oldest waiting run, now `Running`, when one of `slots` is free.
    pub fn next_waiting(&mut self, slots: usize) -> Option<&mut Run> {
        self.next_waiting_at(crate::model::unix_now(), slots)
    }

    /// [`Runs::next_waiting`] at a given time. Its wait may have been long:
    /// the quiet clock starts again, so "started" comes before "still working".
    pub fn next_waiting_at(&mut self, now: u64, slots: usize) -> Option<&mut Run> {
        if self.busy() >= slots {
            return None;
        }
        let run = self
            .runs
            .iter_mut()
            .find(|r| r.state == RunState::Waiting)?;
        run.state = RunState::Running;
        run.reported_at = now;
        Some(run)
    }

    pub fn by_agent_session(&mut self, id: &str) -> Option<&mut Run> {
        self.runs.iter_mut().find(|r| r.agent_session == id)
    }

    pub fn by_session(&mut self, arakun_session: &str) -> Option<&mut Run> {
        self.runs
            .iter_mut()
            .find(|r| r.session.as_deref() == Some(arakun_session))
    }

    /// Runs that take a slot: those at work. A run that asked waits on a
    /// person, and a held one only on work its turn left in the background,
    /// which can be a server that never ends. Neither keeps new work waiting.
    fn busy(&self) -> usize {
        self.runs
            .iter()
            .filter(|r| r.state == RunState::Running && r.held.is_none())
            .count()
    }

    fn count(&self, state: RunState) -> usize {
        self.runs.iter().filter(|r| r.state == state).count()
    }

    /// Drops the oldest finished runs beyond `DONE_KEPT`.
    fn forget_done(&mut self) {
        let mut extra = self.count(RunState::Done).saturating_sub(DONE_KEPT);
        self.runs.retain(|r| {
            let drop = extra > 0 && r.state == RunState::Done;
            extra -= usize::from(drop);
            !drop
        });
    }

    /// Marks the runs Arakun lost while they ran, and gives their agent sessions.
    pub fn interrupted(&mut self, live: &dyn Fn(&str) -> bool) -> Vec<String> {
        let mut lost = Vec::new();
        for r in &mut self.runs {
            let ran = matches!(r.state, RunState::Running | RunState::Asked);
            let Some(session) = r.session.as_deref() else {
                // Arakun closed while making it: nothing ran yet, so it
                // waits again and starts from the top.
                if ran {
                    r.state = RunState::Waiting;
                }
                continue;
            };
            if ran && !live(session) {
                r.state = RunState::Interrupted;
                lost.push(r.agent_session.clone());
            }
        }
        lost
    }

    /// No turn survives a restart, so no stop is winding one down. A flag
    /// saved mid-cancel would silence the run's next turn. True when one was set.
    pub fn clear_stopped(&mut self) -> bool {
        let mut any = false;
        for r in &mut self.runs {
            any |= std::mem::take(&mut r.stopped);
        }
        any
    }

    /// The session `old` was handed to another agent and is now `new`: its
    /// run follows it. True when a run pointed at `old`.
    pub fn repoint(&mut self, old: &str, new: &str) -> bool {
        let Some(run) = self.by_session(old) else {
            return false;
        };
        run.session = Some(new.to_string());
        true
    }

    /// The agent sessions of running runs Linear has not heard from in a while.
    /// Only `Running`: an `Asked` run is waiting on a person, not working.
    pub fn quiet(&self, now: u64) -> Vec<String> {
        self.runs
            .iter()
            .filter(|r| {
                r.state == RunState::Running && now.saturating_sub(r.reported_at) >= QUIET_SECS
            })
            .map(|r| r.agent_session.clone())
            .collect()
    }

    /// Runs whose held reply waited `HOLD_SECS` for background work that
    /// never ended: they are done, and give their reply to send.
    pub fn overdue(&mut self, now: u64) -> Vec<(String, Held)> {
        self.runs
            .iter_mut()
            .filter(|r| r.state == RunState::Running)
            .filter(|r| {
                r.held
                    .as_ref()
                    .is_some_and(|h| now.saturating_sub(h.since) >= HOLD_SECS)
            })
            .filter_map(|r| {
                r.state = RunState::Done;
                Some((r.agent_session.clone(), r.held.take()?))
            })
            .collect()
    }

    /// The Arakun sessions of the runs that have one.
    pub fn session_ids(&self) -> Vec<String> {
        self.runs.iter().filter_map(|r| r.session.clone()).collect()
    }

    /// The sessions in `gone` were removed from Arakun: their runs end. Gives
    /// the agent sessions whose issue must hear it; a run already over says
    /// nothing more.
    pub fn forget_sessions(&mut self, gone: &[String]) -> Vec<String> {
        let mut told = Vec::new();
        for r in &mut self.runs {
            let Some(s) = &r.session else { continue };
            if !gone.contains(s) {
                continue;
            }
            if matches!(
                r.state,
                RunState::Running | RunState::Asked | RunState::Interrupted
            ) {
                told.push(r.agent_session.clone());
            }
            r.state = RunState::Done;
            r.stopped = false;
        }
        told
    }

    /// Whether a run is at work, so Linear may need telling it still is.
    pub fn any_running(&self) -> bool {
        self.count(RunState::Running) > 0
    }

    /// Whether reports wait to be sent.
    pub fn unsent(&self) -> bool {
        !self.outbox.is_empty()
    }

    /// Keeps a reply for a run of `organization` that waits for a slot, by
    /// the id of its agent activity. False when there is no such run or it
    /// holds `KEPT_REPLIES` already. The same reply twice is kept once.
    pub fn keep_reply(&mut self, agent_session: &str, organization: &str, activity: &str) -> bool {
        let Some(run) = self.by_agent_session(agent_session) else {
            return false;
        };
        if run.organization != organization || run.state != RunState::Waiting {
            return false;
        }
        if run.kept.iter().any(|a| a == activity) {
            return true;
        }
        if run.kept.len() >= KEPT_REPLIES {
            return false;
        }
        run.kept.push(activity.to_string());
        true
    }

    /// The replies kept for a run, taken out: its session starts now.
    pub fn take_kept(&mut self, agent_session: &str) -> Vec<String> {
        self.by_agent_session(agent_session)
            .map(|r| std::mem::take(&mut r.kept))
            .unwrap_or_default()
    }

    /// The agent session to send `url` to as the pull request of the run of
    /// `arakun_session`, when Linear has not been sent it yet. Only a web
    /// address, and only for a run that has started.
    pub fn link(&mut self, arakun_session: &str, url: &str) -> Option<String> {
        let run = self.by_session(arakun_session)?;
        if !url.starts_with("https://") || run.linked.as_deref() == Some(url) {
            return None;
        }
        run.linked = Some(url.to_string());
        Some(run.agent_session.clone())
    }

    /// The started runs Linear has no pull request link for yet: their
    /// Arakun session, project and branch.
    pub fn unlinked(&self) -> Vec<(String, PathBuf, String)> {
        self.runs
            .iter()
            .filter(|r| r.linked.is_none() && !r.branch.is_empty())
            .filter_map(|r| Some((r.session.clone()?, r.project.clone(), r.branch.clone())))
            .collect()
    }

    /// What Linear should hear of the run of `arakun_session` now, at most
    /// every `PROGRESS_SECS` and only while it works. `digest` gets the entry
    /// the last digest ended at and gives the new digest, if any, and where it
    /// ended. `step` is the current step as one line; the same step is not
    /// sent twice.
    pub fn progress(
        &mut self,
        arakun_session: &str,
        now: u64,
        digest: impl FnOnce(usize) -> (Option<String>, usize),
        step: Option<&str>,
    ) -> Option<Progress> {
        let run = self.by_session(arakun_session)?;
        if run.state != RunState::Running || run.stopped {
            return None;
        }
        let last = run.progress.clone().unwrap_or_default();
        if run.progress.is_some() && now.saturating_sub(last.at) < PROGRESS_SECS {
            return None;
        }
        let (digest, digested) = digest(last.digested);
        let step = step.filter(|s| *s != last.step);
        if digest.is_none() && step.is_none() {
            return None;
        }
        run.progress = Some(Progressed {
            at: now,
            step: step.map_or(last.step, str::to_string),
            digested,
        });
        Some(Progress {
            agent_session: run.agent_session.clone(),
            digest,
            step: step.is_some(),
        })
    }

    /// The agent session to send `plan` to as the plan of the run of
    /// `arakun_session`, when Linear should hear it: the run works, the plan
    /// changed, and the last one is `PLAN_SECS` old. `ends` is set when the
    /// turn ends or waits on a person: then the plan goes at once, so Linear
    /// is left with the last one.
    pub fn plan(
        &mut self,
        arakun_session: &str,
        plan: &[PlanItem],
        now: u64,
        ends: bool,
    ) -> Option<String> {
        let run = self.by_session(arakun_session)?;
        if !matches!(run.state, RunState::Running | RunState::Asked) || run.stopped {
            return None;
        }
        if let Some((at, last)) = &run.plan
            && (last == plan || (!ends && now.saturating_sub(*at) < PLAN_SECS))
        {
            return None;
        }
        run.plan = Some((now, plan.to_vec()));
        Some(run.agent_session.clone())
    }

    /// Adds a report, dropping the oldest beyond `OUTBOX_KEPT`. Any report
    /// makes the unsent progress of its agent session moot, and a plan the
    /// unsent plan.
    pub fn queue(&mut self, r: Report) {
        self.outbox.retain(|o| {
            let moot = o.kind == ReportKind::Progress
                || (o.kind == ReportKind::Plan && r.kind == ReportKind::Plan);
            !moot || o.agent_session != r.agent_session
        });
        self.outbox.push(r);
        let extra = self.outbox.len().saturating_sub(OUTBOX_KEPT);
        self.outbox.drain(..extra);
    }

    /// The reports to send, oldest first. They stay until `sent` or `dropped`.
    pub fn due(&mut self, now: u64) -> Vec<Report> {
        self.outbox.retain(|r| {
            let kept = match r.kind {
                ReportKind::Progress => PROGRESS_KEPT_SECS,
                _ => REPORT_KEPT_SECS,
            };
            now.saturating_sub(r.made_at) <= kept
        });
        self.outbox.clone()
    }

    pub fn sent(&mut self, r: &Report) {
        self.remove(r);
    }

    /// Linear refused it for good.
    pub fn dropped(&mut self, r: &Report) {
        self.remove(r);
    }

    /// Only the first: the same words can be reported twice on purpose.
    fn remove(&mut self, r: &Report) {
        if let Some(i) = self.outbox.iter().position(|x| x == r) {
            self.outbox.remove(i);
        }
    }
}

/// The slots of a machine: the number the person chose, within reason.
pub fn slots(chosen: Option<usize>) -> usize {
    chosen.unwrap_or(DEFAULT_SLOTS).clamp(1, MOST_SLOTS)
}

/// A reply short enough for Linear; the whole of it stays in Arakun.
pub fn cut(reply: &str) -> String {
    match reply.char_indices().nth(REPLY_LIMIT) {
        Some((end, _)) => format!("{}…\n\nThe rest is in Arakun.", &reply[..end]),
        None => reply.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SLOTS: usize = DEFAULT_SLOTS;

    fn run(agent_session: &str) -> Run {
        Run {
            agent_session: agent_session.into(),
            organization: "w1".into(),
            issue: "i1".into(),
            identifier: "ENG-412".into(),
            project: PathBuf::from("/p/a"),
            environment: None,
            branch: "rik/eng-412".into(),
            session: None,
            state: RunState::Waiting,
            reported_at: 0,
            asked: None,
            stopped: false,
            held: None,
            progress: None,
            plan: None,
            linked: None,
            kept: Vec::new(),
            comment: None,
            guidance: Vec::new(),
        }
    }

    fn report(agent_session: &str, body: &str, made_at: u64) -> Report {
        Report {
            agent_session: agent_session.into(),
            kind: ReportKind::Thought,
            body: body.into(),
            link: None,
            options: Vec::new(),
            started: false,
            parameter: None,
            plan: Vec::new(),
            made_at,
        }
    }

    fn state(r: &mut Runs, id: &str) -> RunState {
        r.by_agent_session(id).unwrap().state
    }

    #[test]
    fn a_pull_request_link_goes_once_and_only_as_a_web_address() {
        let mut r = Runs::default();
        r.add(
            Run {
                session: Some("s1".into()),
                branch: "rik/eng-412".into(),
                state: RunState::Running,
                ..run("a")
            },
            SLOTS,
        );
        r.add(
            Run {
                agent_session: "b".into(),
                issue: "i2".into(),
                ..run("b")
            },
            SLOTS,
        );
        // Only a run with a session and a branch can have a pull request.
        let unlinked: Vec<String> = r.unlinked().into_iter().map(|(s, _, _)| s).collect();
        assert_eq!(unlinked, ["s1"]);
        let pr = "https://github.com/a/b/pull/1";
        assert_eq!(r.link("s1", "file:///x"), None);
        assert_eq!(r.link("other", pr), None);
        assert_eq!(r.link("s1", pr).as_deref(), Some("a"));
        assert_eq!(r.link("s1", pr), None, "told once");
        assert!(r.unlinked().is_empty());
        // It still goes after the run ended, and a new one replaces it.
        r.by_session("s1").unwrap().state = RunState::Done;
        let next = "https://github.com/a/b/pull/2";
        assert_eq!(r.link("s1", next).as_deref(), Some("a"));
        // The link survives a save, so a restart does not send it again.
        let saved = serde_json::to_string(&r).unwrap();
        let mut back: Runs = serde_json::from_str(&saved).unwrap();
        assert_eq!(back.link("s1", next), None);
    }

    #[test]
    fn replies_are_kept_only_for_a_waiting_run_of_the_same_workspace() {
        let mut r = Runs::default();
        r.add(run("a"), SLOTS);
        // With a free slot it would start at once; here it waits for one.
        r.by_agent_session("a").unwrap().state = RunState::Waiting;
        assert!(!r.keep_reply("other", "w1", "act-1"));
        assert!(!r.keep_reply("a", "w2", "act-1"), "another workspace");
        assert!(r.keep_reply("a", "w1", "act-1"));
        assert!(r.keep_reply("a", "w1", "act-1"), "the relay sent it again");
        for i in 2..=KEPT_REPLIES {
            assert!(r.keep_reply("a", "w1", &format!("act-{i}")));
        }
        assert!(!r.keep_reply("a", "w1", "one-too-many"));
        // Only ids are saved, and they survive a restart.
        let saved = serde_json::to_string(&r).unwrap();
        let mut back: Runs = serde_json::from_str(&saved).unwrap();
        let kept = back.take_kept("a");
        assert_eq!(kept.len(), KEPT_REPLIES);
        assert_eq!((kept[0].as_str(), kept[1].as_str()), ("act-1", "act-2"));
        assert!(back.take_kept("a").is_empty(), "taken once");
        // A run at work takes a reply at once: nothing to keep.
        back.by_agent_session("a").unwrap().state = RunState::Running;
        assert!(!back.keep_reply("a", "w1", "act-9"));
    }

    #[test]
    fn progress_goes_only_while_running_and_not_too_often() {
        let mut r = Runs::default();
        r.add(
            Run {
                session: Some("s1".into()),
                state: RunState::Running,
                ..run("a")
            },
            SLOTS,
        );
        let none = |from: usize| (None, from);
        let step = |r: &mut Runs, line: &str, now: u64| {
            r.progress("s1", now, none, Some(line))
                .map(|p| p.agent_session)
        };
        assert_eq!(step(&mut r, "Editing", 100).as_deref(), Some("a"));
        assert_eq!(step(&mut r, "Reading", 100 + PROGRESS_SECS - 1), None);
        assert_eq!(
            step(&mut r, "Editing", 100 + PROGRESS_SECS),
            None,
            "the same line"
        );
        assert_eq!(
            step(&mut r, "Reading", 100 + PROGRESS_SECS).as_deref(),
            Some("a")
        );
        assert_eq!(r.progress("other", 1_000, none, Some("x")), None);
        r.by_session("s1").unwrap().state = RunState::Asked;
        assert_eq!(step(&mut r, "Running", 1_000), None);
        let run = r.by_session("s1").unwrap();
        run.state = RunState::Running;
        run.stopped = true;
        assert_eq!(step(&mut r, "Running", 1_000), None);
    }

    #[test]
    fn a_digest_goes_with_the_step_and_moves_on_from_where_it_ended() {
        let mut r = Runs::default();
        r.add(
            Run {
                session: Some("s1".into()),
                state: RunState::Running,
                ..run("a")
            },
            SLOTS,
        );
        let p = r
            .progress("s1", 100, |from| (Some(format!("from {from}")), 4), None)
            .unwrap();
        assert_eq!(p.digest.as_deref(), Some("from 0"));
        assert!(!p.step);
        let p = r
            .progress(
                "s1",
                100 + PROGRESS_SECS,
                |from| (Some(format!("from {from}")), 9),
                Some("Editing"),
            )
            .unwrap();
        assert_eq!(p.digest.as_deref(), Some("from 4"));
        assert!(p.step);
        // Nothing new: nothing goes, and the step is not sent again.
        let at = 100 + 2 * PROGRESS_SECS;
        let same = r.progress("s1", at, |from| (None, from), Some("Editing"));
        assert_eq!(same, None);
        let p = r
            .progress(
                "s1",
                at,
                |from| (Some(format!("from {from}")), from),
                Some("Editing"),
            )
            .unwrap();
        assert_eq!(p.digest.as_deref(), Some("from 9"));
        assert!(!p.step);
    }

    fn items(content: &str) -> Vec<PlanItem> {
        vec![PlanItem {
            content: content.into(),
            status: PlanItemStatus::Pending,
        }]
    }

    #[test]
    fn a_plan_goes_when_it_changes_and_not_too_often_unless_the_turn_ends() {
        let mut r = Runs::default();
        r.add(
            Run {
                session: Some("s1".into()),
                state: RunState::Running,
                ..run("a")
            },
            SLOTS,
        );
        let (one, two, three) = (items("one"), items("two"), items("three"));
        assert_eq!(r.plan("s1", &one, 100, false).as_deref(), Some("a"));
        assert_eq!(r.plan("s1", &one, 200, false), None, "the same plan");
        assert_eq!(r.plan("s1", &two, 100 + PLAN_SECS - 1, false), None);
        assert_eq!(
            r.plan("s1", &two, 100 + PLAN_SECS - 1, true).as_deref(),
            Some("a"),
            "the turn ends: it goes at once"
        );
        assert_eq!(r.plan("s1", &two, 1_000, true), None, "told already");
        assert_eq!(r.plan("other", &three, 1_000, false), None);
        r.by_session("s1").unwrap().state = RunState::Asked;
        assert_eq!(r.plan("s1", &three, 1_000, true).as_deref(), Some("a"));
        let run = r.by_session("s1").unwrap();
        run.state = RunState::Running;
        run.stopped = true;
        assert_eq!(r.plan("s1", &one, 2_000, true), None, "Linear stopped it");
        r.by_session("s1").unwrap().state = RunState::Done;
        r.by_session("s1").unwrap().stopped = false;
        assert_eq!(r.plan("s1", &one, 2_000, true), None);
    }

    #[test]
    fn a_newer_plan_replaces_the_unsent_one_of_its_session_only() {
        let mut r = Runs::default();
        let plan = |a: &str, body: &str, at| Report {
            kind: ReportKind::Plan,
            plan: items(body),
            ..report(a, "", at)
        };
        r.queue(plan("a", "one", 1));
        r.queue(report("a", "reply", 2));
        r.queue(plan("b", "other", 3));
        r.queue(plan("a", "two", 4));
        let got: Vec<(String, ReportKind)> = r
            .due(5)
            .into_iter()
            .map(|x| (x.agent_session, x.kind))
            .collect();
        assert_eq!(
            got,
            [
                ("a".to_string(), ReportKind::Thought),
                ("b".to_string(), ReportKind::Plan),
                ("a".to_string(), ReportKind::Plan),
            ]
        );
        assert_eq!(r.due(5)[2].plan, items("two"));
        // A plan is no step: another report leaves it be.
        r.queue(report("b", "reply", 6));
        assert_eq!(r.due(6).len(), 4);
    }

    #[test]
    fn a_plan_never_prints_its_items() {
        let r = Report {
            kind: ReportKind::Plan,
            plan: items("rotate the secret key"),
            ..report("a", "", 1)
        };
        assert!(!format!("{r:?}").contains("secret"));
    }

    #[test]
    fn a_newer_report_replaces_unsent_progress_of_its_session_only() {
        let mut r = Runs::default();
        let progress = |a: &str, body: &str, at| Report {
            kind: ReportKind::Progress,
            ..report(a, body, at)
        };
        r.queue(progress("a", "Editing", 1));
        r.queue(progress("b", "Reading", 1));
        r.queue(progress("a", "Running", 2));
        r.queue(report("b", "Done.", 3));
        let due: Vec<String> = r.due(4).into_iter().map(|x| x.body).collect();
        assert_eq!(due, ["Running", "Done."]);
        // Stale progress goes; an older report of another kind stays.
        r.queue(progress("c", "Searching", 4));
        let left: Vec<String> = r
            .due(4 + PROGRESS_KEPT_SECS + 1)
            .into_iter()
            .map(|x| x.body)
            .collect();
        assert_eq!(left, ["Done."]);
    }

    #[test]
    fn a_delivery_is_seen_once_and_old_ones_are_forgotten() {
        let mut r = Runs::default();
        assert!(!r.was_seen("d0"));
        assert!(!r.was_seen("d0"), "asking records nothing");
        r.mark_seen("d0");
        r.mark_seen("d0");
        assert!(r.was_seen("d0"));
        assert_eq!(r.handled.len(), 1);
        for i in 1..=HANDLED_KEPT {
            r.mark_seen(&format!("d{i}"));
        }
        assert_eq!(r.handled.len(), HANDLED_KEPT);
        assert!(!r.was_seen("d0"), "the oldest is forgotten");
        assert!(r.was_seen(&format!("d{HANDLED_KEPT}")));
    }

    #[test]
    fn a_handled_delivery_and_its_run_are_saved_together() {
        let dir = crate::stored::tests::scratch("runs-handled");
        let file = dir.join("runs.json");
        let mut r = Runs::default();
        // Nothing is saved while the issue is read: a crash then leaves the
        // delivery unhandled, and the relay's next frame runs it.
        assert!(!r.was_seen("d1"));
        r.save_to(&file).unwrap();
        assert!(!Runs::load_from(&file).was_seen("d1"));
        // The outcome and the id go in one save.
        r.mark_seen("d1");
        assert_eq!(r.add(run("a"), SLOTS), Added::Start);
        r.save_to(&file).unwrap();
        let mut back = Runs::load_from(&file);
        assert!(back.was_seen("d1"));
        assert_eq!(state(&mut back, "a"), RunState::Running);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn guidance_stays_in_memory_and_is_never_saved() {
        let dir = crate::stored::tests::scratch("runs-guidance");
        let file = dir.join("runs.json");
        let mut r = Runs::default();
        r.add(
            Run {
                guidance: vec![GuidanceRule {
                    team: Some("Secret team".into()),
                    body: "Secret rule".into(),
                }],
                ..run("a")
            },
            SLOTS,
        );
        assert_eq!(r.by_agent_session("a").unwrap().guidance.len(), 1);
        r.save_to(&file).unwrap();
        let saved = std::fs::read_to_string(&file).unwrap();
        assert!(
            !saved.contains("Secret") && !saved.contains("guidance"),
            "{saved}"
        );
        let mut back = Runs::load_from(&file);
        assert!(back.by_agent_session("a").unwrap().guidance.is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_issue_in_work_blocks_a_second_run_until_it_is_over() {
        let mut r = Runs::default();
        assert!(!r.active_for_issue("w1", "i1"));
        r.add(run("a"), SLOTS);
        assert!(r.active_for_issue("w1", "i1"));
        assert!(!r.active_for_issue("w1", "i2") && !r.active_for_issue("w2", "i1"));
        for (state, active) in [
            (RunState::Waiting, true),
            (RunState::Asked, true),
            (RunState::Done, false),
            (RunState::Interrupted, false),
        ] {
            r.by_agent_session("a").unwrap().state = state;
            assert_eq!(r.active_for_issue("w1", "i1"), active, "{state:?}");
        }
    }

    #[test]
    fn three_run_at_once_and_the_rest_wait() {
        let mut r = Runs::default();
        for id in ["a", "b", "c"] {
            assert_eq!(r.add(run(id), SLOTS), Added::Start);
            assert_eq!(state(&mut r, id), RunState::Running);
        }
        assert_eq!(
            r.add(run("d"), SLOTS),
            Added::Wait {
                running: 3,
                ahead: 0
            }
        );
        // Three run and one waits before it, not "four running".
        assert_eq!(
            r.add(run("e"), SLOTS),
            Added::Wait {
                running: 3,
                ahead: 1
            }
        );
        assert_eq!(state(&mut r, "d"), RunState::Waiting);
        assert_eq!(r.add(run("a"), SLOTS), Added::Already);
        assert_eq!(r.add(run("d"), SLOTS), Added::Already);
    }

    #[test]
    fn a_session_waiting_on_a_person_frees_its_slot() {
        let mut r = Runs::default();
        for id in ["a", "b", "c", "d"] {
            r.add(run(id), SLOTS);
        }
        r.by_agent_session("a").unwrap().state = RunState::Asked;
        assert_eq!(r.next_waiting(SLOTS).unwrap().agent_session, "d");
        // The reply brings it back to work even with every slot taken: the
        // person must not wait.
        r.by_agent_session("a").unwrap().state = RunState::Running;
        assert_eq!(
            r.add(run("e"), SLOTS),
            Added::Wait {
                running: 4,
                ahead: 0
            }
        );
        assert!(r.next_waiting(SLOTS).is_none());
    }

    #[test]
    fn a_held_run_frees_its_slot() {
        // As on trashpanda: two of three turns ended, their background work
        // went on, and new work waited behind them.
        let mut r = Runs::default();
        for id in ["a", "b", "c", "d", "e"] {
            r.add(run(id), SLOTS);
        }
        for id in ["a", "b"] {
            r.by_agent_session(id).unwrap().held = Some(Held {
                since: 0,
                body: "Done.".into(),
                link: None,
            });
        }
        assert_eq!(r.next_waiting(SLOTS).unwrap().agent_session, "d");
        assert_eq!(r.next_waiting(SLOTS).unwrap().agent_session, "e");
        assert!(r.next_waiting(SLOTS).is_none());
        assert_eq!(state(&mut r, "a"), RunState::Running, "still its issue's");
    }

    #[test]
    fn each_machine_has_its_own_number_of_slots() {
        let mut r = Runs::default();
        assert_eq!(r.add(run("a"), 1), Added::Start);
        assert_eq!(
            r.add(run("b"), 1),
            Added::Wait {
                running: 1,
                ahead: 0
            }
        );
        // More slots chosen later: the next waiting run starts at once.
        assert_eq!(r.next_waiting(2).unwrap().agent_session, "b");
        assert!(r.next_waiting(2).is_none());
    }

    #[test]
    fn the_chosen_slots_stay_within_reason() {
        assert_eq!(slots(None), DEFAULT_SLOTS);
        assert_eq!(slots(Some(5)), 5);
        assert_eq!(slots(Some(0)), 1, "nothing would ever start");
        assert_eq!(slots(Some(1000)), MOST_SLOTS);
    }

    #[test]
    fn a_free_slot_starts_the_oldest_waiting_run() {
        let mut r = Runs::default();
        for id in ["a", "b", "c", "d", "e"] {
            r.add(run(id), SLOTS);
        }
        assert!(r.next_waiting(SLOTS).is_none());
        r.by_agent_session("b").unwrap().state = RunState::Done;
        let next = r.next_waiting(SLOTS).unwrap();
        assert_eq!(
            (next.agent_session.as_str(), next.state),
            ("d", RunState::Running)
        );
        assert!(r.next_waiting(SLOTS).is_none(), "the slot is taken again");
        assert_eq!(state(&mut r, "e"), RunState::Waiting);
    }

    #[test]
    fn a_new_run_waits_behind_one_already_waiting() {
        let mut r = Runs::default();
        for id in ["a", "b", "c", "d"] {
            r.add(run(id), SLOTS);
        }
        for id in ["a", "b", "c"] {
            r.by_agent_session(id).unwrap().state = RunState::Done;
        }
        assert_eq!(
            r.add(run("e"), SLOTS),
            Added::Wait {
                running: 0,
                ahead: 1
            }
        );
        assert_eq!(r.next_waiting(SLOTS).unwrap().agent_session, "d");
    }

    #[test]
    fn a_stopped_waiting_run_leaves_the_queue_in_order() {
        let mut r = Runs::default();
        for id in ["a", "b", "c", "d", "e", "f"] {
            r.add(run(id), SLOTS);
        }
        // A stop from Linear ends a run that never started.
        r.by_agent_session("e").unwrap().state = RunState::Done;
        assert!(r.next_waiting(SLOTS).is_none(), "it freed no slot");
        assert_eq!(
            r.add(run("g"), SLOTS),
            Added::Wait {
                running: 3,
                ahead: 2
            }
        );
        r.by_agent_session("a").unwrap().state = RunState::Done;
        assert_eq!(r.next_waiting(SLOTS).unwrap().agent_session, "d");
        r.by_agent_session("b").unwrap().state = RunState::Done;
        assert_eq!(r.next_waiting(SLOTS).unwrap().agent_session, "f");
        r.by_agent_session("c").unwrap().state = RunState::Done;
        assert_eq!(r.next_waiting(SLOTS).unwrap().agent_session, "g");
        assert_eq!(state(&mut r, "e"), RunState::Done);
    }

    #[test]
    fn after_a_restart_the_queue_keeps_its_order() {
        let mut r = Runs::default();
        for id in ["a", "b", "c", "d", "e"] {
            r.add(run(id), SLOTS);
        }
        for id in ["a", "b", "c"] {
            r.by_agent_session(id).unwrap().session = Some(format!("s-{id}"));
        }
        assert_eq!(r.interrupted(&|_| false), ["a", "b", "c"]);
        assert_eq!(
            r.add(run("f"), SLOTS),
            Added::Wait {
                running: 0,
                ahead: 2
            }
        );
        assert_eq!(r.next_waiting(SLOTS).unwrap().agent_session, "d");
    }

    #[test]
    fn runs_are_found_by_either_session() {
        let mut r = Runs::default();
        r.add(run("a"), SLOTS);
        r.by_agent_session("a").unwrap().session = Some("s1".into());
        assert_eq!(r.by_session("s1").unwrap().agent_session, "a");
        assert!(r.by_session("s2").is_none() && r.by_agent_session("b").is_none());
    }

    #[test]
    fn only_runs_whose_session_is_gone_are_interrupted() {
        let mut r = Runs::default();
        for id in ["a", "b", "c", "d"] {
            r.add(run(id), SLOTS);
        }
        r.by_agent_session("a").unwrap().session = Some("live".into());
        r.by_agent_session("b").unwrap().session = Some("gone".into());
        r.by_agent_session("c").unwrap().session = Some("asked".into());
        r.by_agent_session("c").unwrap().state = RunState::Asked;
        // "d" waits and has no session: it was never started, so not lost.
        let live = |s: &str| s == "live";
        assert_eq!(r.interrupted(&live), ["b", "c"]);
        assert_eq!(state(&mut r, "a"), RunState::Running);
        assert_eq!(state(&mut r, "b"), RunState::Interrupted);
        assert_eq!(state(&mut r, "c"), RunState::Interrupted);
        assert_eq!(state(&mut r, "d"), RunState::Waiting);
    }

    #[test]
    fn a_run_still_being_made_at_a_restart_waits_again() {
        let mut r = Runs::default();
        for id in ["a", "b", "c", "d"] {
            r.add(run(id), SLOTS);
        }
        r.by_agent_session("b").unwrap().session = Some("s-b".into());
        // "a" and "c" had no session yet: nothing to continue, and nothing
        // for the issue to hear. They start again, ahead of "d".
        assert_eq!(r.interrupted(&|_| false), ["b"]);
        assert_eq!(state(&mut r, "a"), RunState::Waiting);
        assert_eq!(state(&mut r, "b"), RunState::Interrupted);
        assert_eq!(state(&mut r, "c"), RunState::Waiting);
        let order: Vec<String> =
            std::iter::from_fn(|| r.next_waiting(SLOTS).map(|run| run.agent_session.clone()))
                .collect();
        assert_eq!(order, ["a", "c", "d"]);
    }

    #[test]
    fn a_restart_clears_every_stop_in_progress() {
        let mut r = Runs::default();
        for id in ["a", "b"] {
            r.add(run(id), SLOTS);
        }
        assert!(!r.clear_stopped(), "nothing to clear");
        r.by_agent_session("a").unwrap().stopped = true;
        r.by_agent_session("a").unwrap().state = RunState::Done;
        assert!(r.clear_stopped());
        assert!(!r.by_agent_session("a").unwrap().stopped);
        assert_eq!(state(&mut r, "a"), RunState::Done, "only the flag goes");
        assert!(!r.clear_stopped());
    }

    #[test]
    fn a_run_follows_its_session_to_another_agent() {
        let mut r = Runs::default();
        r.add(run("a"), SLOTS);
        r.add(run("b"), SLOTS);
        r.by_agent_session("a").unwrap().session = Some("s1".into());
        assert!(r.repoint("s1", "s2"));
        assert!(r.by_session("s1").is_none());
        let moved = r.by_session("s2").unwrap();
        assert_eq!(
            (moved.agent_session.as_str(), moved.state),
            ("a", RunState::Running),
            "the same run, in the same state"
        );
        assert!(!r.repoint("s1", "s3"), "no run points at it any more");
        assert!(!r.repoint("other", "s4"));
        assert_eq!(r.by_agent_session("b").unwrap().session, None);
    }

    #[test]
    fn a_new_run_is_not_quiet_before_its_first_report() {
        let mut r = Runs::default();
        let now = 50_000;
        assert_eq!(r.add_at(run("a"), now, SLOTS), Added::Start);
        assert_eq!(r.by_agent_session("a").unwrap().reported_at, now);
        assert!(r.quiet(now).is_empty());
        assert!(r.quiet(now + QUIET_SECS - 1).is_empty());
        assert_eq!(r.quiet(now + QUIET_SECS), ["a"]);
        // One that waited a long time starts its clock again when it starts.
        for id in ["b", "c", "d"] {
            r.add_at(run(id), now, SLOTS);
        }
        r.by_agent_session("a").unwrap().state = RunState::Done;
        let later = now + 10 * QUIET_SECS;
        assert_eq!(r.next_waiting_at(later, SLOTS).unwrap().agent_session, "d");
        assert!(!r.quiet(later).contains(&"d".to_string()));
    }

    #[test]
    fn a_held_reply_goes_once_the_background_work_took_too_long() {
        let mut r = Runs::default();
        for id in ["a", "b"] {
            r.add_at(run(id), 0, SLOTS);
            let run = r.by_agent_session(id).unwrap();
            run.state = RunState::Running;
        }
        r.by_agent_session("a").unwrap().held = Some(Held {
            since: 100,
            body: "Done.".into(),
            link: Some("https://github.com/o/r/pull/1".into()),
        });
        // Still waiting: it keeps telling Linear it works.
        assert!(r.overdue(100 + HOLD_SECS - 1).is_empty());
        assert!(r.quiet(100 + QUIET_SECS).contains(&"a".to_string()));
        let gone = r.overdue(100 + HOLD_SECS);
        assert_eq!(gone.len(), 1);
        assert_eq!(
            (gone[0].0.as_str(), gone[0].1.body.as_str()),
            ("a", "Done.")
        );
        let a = r.by_agent_session("a").unwrap();
        assert_eq!((a.state, a.held.is_none()), (RunState::Done, true));
        assert_eq!(r.by_agent_session("b").unwrap().state, RunState::Running);
    }

    #[test]
    fn the_outbox_keeps_the_newest_reports_up_to_a_limit() {
        let mut r = Runs::default();
        for i in 0..OUTBOX_KEPT + 3 {
            r.queue(report("a", &format!("r{i}"), 5));
        }
        let due = r.due(5);
        assert_eq!(due.len(), OUTBOX_KEPT);
        assert_eq!(due[0].body, "r3", "the oldest went");
        assert_eq!(due[OUTBOX_KEPT - 1].body, format!("r{}", OUTBOX_KEPT + 2));
    }

    #[test]
    fn a_run_is_quiet_after_25_minutes() {
        let mut r = Runs::default();
        r.add(run("a"), SLOTS);
        r.add(run("b"), SLOTS);
        r.by_agent_session("a").unwrap().reported_at = 1_000;
        r.by_agent_session("b").unwrap().reported_at = 1_000;
        r.by_agent_session("b").unwrap().state = RunState::Asked;
        assert!(r.quiet(1_000 + 24 * 60).is_empty());
        assert_eq!(r.quiet(1_000 + QUIET_SECS), ["a"]);
    }

    #[test]
    fn a_removed_session_ends_its_run() {
        let mut r = Runs::default();
        for (id, state) in [
            ("a", RunState::Running),
            ("b", RunState::Asked),
            ("c", RunState::Interrupted),
            ("d", RunState::Done),
            ("e", RunState::Running),
        ] {
            r.add(run(id), SLOTS);
            let x = r.by_agent_session(id).unwrap();
            x.session = Some(format!("s-{id}"));
            x.state = state;
        }
        r.add(run("f"), SLOTS);
        let f = state(&mut r, "f");
        let gone: Vec<String> = ["s-a", "s-b", "s-c", "s-d", "s-x"].map(String::from).into();
        assert_eq!(r.forget_sessions(&gone), ["a", "b", "c"]);
        for id in ["a", "b", "c", "d"] {
            assert_eq!(state(&mut r, id), RunState::Done, "{id}");
        }
        assert_eq!(state(&mut r, "e"), RunState::Running);
        assert_eq!(state(&mut r, "f"), f, "not its session");
        assert!(r.forget_sessions(&gone).is_empty(), "told once");
        assert_eq!(r.session_ids(), ["s-a", "s-b", "s-c", "s-d", "s-e"]);
    }

    #[test]
    fn the_timer_is_needed_while_a_run_works_or_reports_wait() {
        let mut r = Runs::default();
        assert!(!r.any_running() && !r.unsent());
        r.add(run("a"), SLOTS);
        assert!(r.any_running());
        r.by_agent_session("a").unwrap().state = RunState::Asked;
        assert!(!r.any_running(), "an asked run waits on a person");
        r.queue(report("a", "one", 5));
        assert!(r.unsent());
        r.due(5 + REPORT_KEPT_SECS + 1);
        assert!(!r.unsent(), "a day-old report is gone");
    }

    #[test]
    fn reports_go_in_order_and_expire_after_a_day() {
        let now = 100_000;
        let mut r = Runs::default();
        r.queue(report("a", "old", now - REPORT_KEPT_SECS - 1));
        r.queue(report("a", "one", now - REPORT_KEPT_SECS));
        r.queue(report("a", "two", now - 5));
        r.queue(report("a", "two", now - 5));
        let due = r.due(now);
        let bodies: Vec<&str> = due.iter().map(|x| x.body.as_str()).collect();
        assert_eq!(bodies, ["one", "two", "two"]);
        assert_eq!(r.due(now).len(), 3, "due does not remove");
        r.sent(&due[1]);
        assert_eq!(r.due(now).len(), 2, "sent removes exactly one");
        r.dropped(&due[0]);
        assert_eq!(r.due(now), [due[2].clone()]);
    }

    #[test]
    fn a_report_never_prints_its_body() {
        let r = report("a", "secret plan", 1);
        assert!(!format!("{r:?}").contains("secret"));
        assert!(
            !format!(
                "{:?}",
                Runs {
                    outbox: vec![r],
                    ..Runs::default()
                }
            )
            .contains("secret")
        );
    }

    #[test]
    fn a_run_never_prints_its_branch() {
        let mut r = run("a");
        r.branch = "rik/eng-412-secret-launch-plan".into();
        let text = format!("{r:?}");
        assert!(!text.contains("secret") && text.contains("ENG-412"));
    }

    #[test]
    fn finished_runs_are_kept_up_to_a_limit() {
        let mut r = Runs::default();
        for i in 0..DONE_KEPT + 5 {
            r.add(run(&format!("r{i}")), SLOTS);
            r.by_agent_session(&format!("r{i}")).unwrap().state = RunState::Done;
        }
        r.add(run("last"), SLOTS);
        assert_eq!(r.runs.len(), DONE_KEPT + 1);
        assert!(r.by_agent_session("r4").is_none() && r.by_agent_session("r5").is_some());
    }

    #[test]
    fn it_survives_the_disk_and_a_damaged_file() {
        let dir = crate::stored::tests::scratch("runs");
        let file = dir.join("linear").join("runs.json");
        assert_eq!(Runs::load_from(&file), Runs::default());

        let mut r = Runs::default();
        r.mark_seen("d1");
        r.add(run("a"), SLOTS);
        r.by_agent_session("a").unwrap().environment = Some(PathBuf::from("/e/a"));
        r.by_agent_session("a").unwrap().asked = Some("00ff".into());
        r.by_agent_session("a").unwrap().stopped = true;
        r.queue(Report {
            kind: ReportKind::Response,
            link: Some("https://x".into()),
            ..report("a", "done", 5)
        });
        r.save_to(&file).unwrap();
        assert_eq!(Runs::load_from(&file), r);
        let text = std::fs::read_to_string(&file).unwrap();
        assert!(text.contains(r#""kind":"response""#) && text.contains(r#""state":"running""#));

        std::fs::write(&file, b"not json {").unwrap();
        assert_eq!(Runs::load_from(&file), Runs::default());
        assert_eq!(
            crate::stored::tests::kept(&file).as_deref(),
            Some("not json {")
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_older_file_loads_what_it_can() {
        let dir = crate::stored::tests::scratch("runs-versions");
        let file = dir.join("runs.json");
        std::fs::write(
            &file,
            br#"{"runs": [{"agent_session": "a", "organization": "w1", "issue": "i1",
                "identifier": "ENG-1", "project": "/p/a", "state": "waiting"}]}"#,
        )
        .unwrap();
        let mut r = Runs::load_from(&file);
        assert_eq!(state(&mut r, "a"), RunState::Waiting);
        assert!(r.handled.is_empty() && r.outbox.is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_short_reply_is_unchanged() {
        assert_eq!(cut("Done."), "Done.");
        let edge = "x".repeat(REPLY_LIMIT);
        assert_eq!(cut(&edge), edge);
    }

    #[test]
    fn a_long_reply_is_cut_with_a_note() {
        let c = cut(&"x".repeat(REPLY_LIMIT + 1));
        assert_eq!(
            c,
            format!("{}…\n\nThe rest is in Arakun.", "x".repeat(REPLY_LIMIT))
        );
    }

    #[test]
    fn a_reply_is_cut_on_a_character() {
        let c = cut(&"é".repeat(REPLY_LIMIT + 3));
        assert_eq!(c.matches('é').count(), REPLY_LIMIT);
        assert!(c.ends_with("…\n\nThe rest is in Arakun."));
    }
}
