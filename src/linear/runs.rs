//! The record of sessions started from Linear.
//!
//! `app_dir/linear/runs.json` holds the deliveries already handled, each run
//! with its state, and the reports Linear has not taken yet. It holds ids,
//! paths and an issue's identifier; only unsent reports carry Linear's text,
//! and they are dropped after a day.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// Delegated sessions running at once. A computer back after days must not
/// start dozens of agents; the rest wait their turn.
pub const MAX_RUNNING: usize = 3;
/// Delivery ids remembered, so a delivery sent again is not run twice.
pub const HANDLED_KEPT: usize = 500;
/// How long a report Linear has not taken is tried again.
pub const REPORT_KEPT_SECS: u64 = 24 * 3600;
/// A running session silent this long tells Linear it is still working.
#[allow(dead_code)] // Tasks 6 and 7 use it.
pub const QUIET_SECS: u64 = 25 * 60;

/// Finished runs kept, so a follow-up can still find its run.
const DONE_KEPT: usize = 200;
/// The most characters of a reply sent to Linear.
#[allow(dead_code)] // Tasks 6 and 7 use it.
const REPLY_LIMIT: usize = 7_500;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunState {
    /// All slots were taken; it starts when one frees up.
    Waiting,
    Running,
    /// The session waits on a person. It still takes a slot.
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
    pub made_at: u64,
}

impl std::fmt::Debug for Report {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Report")
            .field("agent_session", &self.agent_session)
            .field("kind", &self.kind)
            .field("body", &format_args!("…"))
            .field("link", &self.link.is_some())
            .field("made_at", &self.made_at)
            .finish()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Added {
    Start,
    /// `ahead` runs start before this one.
    Wait {
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
    crate::platform::app_dir().join("linear").join("runs.json")
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

    /// Whether `delivery_id` was handled before. Records it if not.
    pub fn seen(&mut self, delivery_id: &str) -> bool {
        if self.handled.iter().any(|h| h == delivery_id) {
            return true;
        }
        self.handled.push(delivery_id.to_string());
        let extra = self.handled.len().saturating_sub(HANDLED_KEPT);
        self.handled.drain(..extra);
        false
    }

    /// Stores `run` as `Running` when a slot is free and nobody waits, else
    /// as `Waiting`.
    pub fn add(&mut self, mut run: Run) -> Added {
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
        let added = if busy < MAX_RUNNING && waiting == 0 {
            run.state = RunState::Running;
            Added::Start
        } else {
            run.state = RunState::Waiting;
            Added::Wait {
                ahead: busy + waiting,
            }
        };
        self.runs.push(run);
        self.forget_done();
        added
    }

    pub fn next_waiting(&mut self) -> Option<&mut Run> {
        if self.busy() >= MAX_RUNNING {
            return None;
        }
        let run = self
            .runs
            .iter_mut()
            .find(|r| r.state == RunState::Waiting)?;
        run.state = RunState::Running;
        Some(run)
    }

    pub fn by_agent_session(&mut self, id: &str) -> Option<&mut Run> {
        self.runs.iter_mut().find(|r| r.agent_session == id)
    }

    #[allow(dead_code)] // Tasks 6 and 7 use it.
    pub fn by_session(&mut self, arakun_session: &str) -> Option<&mut Run> {
        self.runs
            .iter_mut()
            .find(|r| r.session.as_deref() == Some(arakun_session))
    }

    /// Runs that take a slot.
    fn busy(&self) -> usize {
        self.count(RunState::Running) + self.count(RunState::Asked)
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
    #[allow(dead_code)] // Tasks 6 and 7 use it.
    pub fn interrupted(&mut self, live: &dyn Fn(&str) -> bool) -> Vec<String> {
        let mut lost = Vec::new();
        for r in &mut self.runs {
            let ran = matches!(r.state, RunState::Running | RunState::Asked);
            // No session yet means Arakun closed while making it.
            if ran && !r.session.as_deref().is_some_and(live) {
                r.state = RunState::Interrupted;
                lost.push(r.agent_session.clone());
            }
        }
        lost
    }

    /// The agent sessions of running runs Linear has not heard from in a while.
    /// Only `Running`: an `Asked` run is waiting on a person, not working.
    #[allow(dead_code)] // Tasks 6 and 7 use it.
    pub fn quiet(&self, now: u64) -> Vec<String> {
        self.runs
            .iter()
            .filter(|r| {
                r.state == RunState::Running && now.saturating_sub(r.reported_at) >= QUIET_SECS
            })
            .map(|r| r.agent_session.clone())
            .collect()
    }

    pub fn queue(&mut self, r: Report) {
        self.outbox.push(r);
    }

    /// The reports to send, oldest first. They stay until `sent` or `dropped`.
    pub fn due(&mut self, now: u64) -> Vec<Report> {
        self.outbox
            .retain(|r| now.saturating_sub(r.made_at) <= REPORT_KEPT_SECS);
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

/// A reply short enough for Linear; the whole of it stays in Arakun.
#[allow(dead_code)] // Tasks 6 and 7 use it.
pub fn cut(reply: &str) -> String {
    match reply.char_indices().nth(REPLY_LIMIT) {
        Some((end, _)) => format!("{}…\n\nThe rest is in Arakun.", &reply[..end]),
        None => reply.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
        }
    }

    fn report(agent_session: &str, body: &str, made_at: u64) -> Report {
        Report {
            agent_session: agent_session.into(),
            kind: ReportKind::Thought,
            body: body.into(),
            link: None,
            made_at,
        }
    }

    fn state(r: &mut Runs, id: &str) -> RunState {
        r.by_agent_session(id).unwrap().state
    }

    #[test]
    fn a_delivery_is_seen_once_and_old_ones_are_forgotten() {
        let mut r = Runs::default();
        assert!(!r.seen("d0"));
        assert!(r.seen("d0"));
        for i in 1..=HANDLED_KEPT {
            assert!(!r.seen(&format!("d{i}")));
        }
        assert_eq!(r.handled.len(), HANDLED_KEPT);
        assert!(!r.seen("d0"), "the oldest is forgotten");
        assert!(r.seen(&format!("d{HANDLED_KEPT}")));
    }

    #[test]
    fn three_run_at_once_and_the_rest_wait() {
        let mut r = Runs::default();
        for id in ["a", "b", "c"] {
            assert_eq!(r.add(run(id)), Added::Start);
            assert_eq!(state(&mut r, id), RunState::Running);
        }
        assert_eq!(r.add(run("d")), Added::Wait { ahead: 3 });
        assert_eq!(r.add(run("e")), Added::Wait { ahead: 4 });
        assert_eq!(state(&mut r, "d"), RunState::Waiting);
        assert_eq!(r.add(run("a")), Added::Already);
        assert_eq!(r.add(run("d")), Added::Already);
    }

    #[test]
    fn a_session_waiting_on_a_person_keeps_its_slot() {
        let mut r = Runs::default();
        for id in ["a", "b", "c"] {
            r.add(run(id));
        }
        r.by_agent_session("a").unwrap().state = RunState::Asked;
        assert_eq!(r.add(run("d")), Added::Wait { ahead: 3 });
        assert!(r.next_waiting().is_none());
    }

    #[test]
    fn a_free_slot_starts_the_oldest_waiting_run() {
        let mut r = Runs::default();
        for id in ["a", "b", "c", "d", "e"] {
            r.add(run(id));
        }
        assert!(r.next_waiting().is_none());
        r.by_agent_session("b").unwrap().state = RunState::Done;
        let next = r.next_waiting().unwrap();
        assert_eq!(
            (next.agent_session.as_str(), next.state),
            ("d", RunState::Running)
        );
        assert!(r.next_waiting().is_none(), "the slot is taken again");
        assert_eq!(state(&mut r, "e"), RunState::Waiting);
    }

    #[test]
    fn a_new_run_waits_behind_one_already_waiting() {
        let mut r = Runs::default();
        for id in ["a", "b", "c", "d"] {
            r.add(run(id));
        }
        for id in ["a", "b", "c"] {
            r.by_agent_session(id).unwrap().state = RunState::Done;
        }
        assert_eq!(r.add(run("e")), Added::Wait { ahead: 1 });
        assert_eq!(r.next_waiting().unwrap().agent_session, "d");
    }

    #[test]
    fn after_a_restart_the_queue_keeps_its_order() {
        let mut r = Runs::default();
        for id in ["a", "b", "c", "d", "e"] {
            r.add(run(id));
        }
        assert_eq!(r.interrupted(&|_| false), ["a", "b", "c"]);
        assert_eq!(r.add(run("f")), Added::Wait { ahead: 2 });
        assert_eq!(r.next_waiting().unwrap().agent_session, "d");
    }

    #[test]
    fn runs_are_found_by_either_session() {
        let mut r = Runs::default();
        r.add(run("a"));
        r.by_agent_session("a").unwrap().session = Some("s1".into());
        assert_eq!(r.by_session("s1").unwrap().agent_session, "a");
        assert!(r.by_session("s2").is_none() && r.by_agent_session("b").is_none());
    }

    #[test]
    fn only_runs_whose_session_is_gone_are_interrupted() {
        let mut r = Runs::default();
        for id in ["a", "b", "c", "d"] {
            r.add(run(id));
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
    fn a_run_still_being_made_counts_as_lost() {
        let mut r = Runs::default();
        r.add(run("a"));
        assert_eq!(r.interrupted(&|_| true), ["a"]);
    }

    #[test]
    fn a_run_is_quiet_after_25_minutes() {
        let mut r = Runs::default();
        r.add(run("a"));
        r.add(run("b"));
        r.by_agent_session("a").unwrap().reported_at = 1_000;
        r.by_agent_session("b").unwrap().reported_at = 1_000;
        r.by_agent_session("b").unwrap().state = RunState::Asked;
        assert!(r.quiet(1_000 + 24 * 60).is_empty());
        assert_eq!(r.quiet(1_000 + QUIET_SECS), ["a"]);
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
            r.add(run(&format!("r{i}")));
            r.by_agent_session(&format!("r{i}")).unwrap().state = RunState::Done;
        }
        r.add(run("last"));
        assert_eq!(r.runs.len(), DONE_KEPT + 1);
        assert!(r.by_agent_session("r4").is_none() && r.by_agent_session("r5").is_some());
    }

    #[test]
    fn it_survives_the_disk_and_a_damaged_file() {
        let dir = crate::stored::tests::scratch("runs");
        let file = dir.join("linear").join("runs.json");
        assert_eq!(Runs::load_from(&file), Runs::default());

        let mut r = Runs::default();
        r.seen("d1");
        r.add(run("a"));
        r.by_agent_session("a").unwrap().environment = Some(PathBuf::from("/e/a"));
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
