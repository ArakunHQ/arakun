//! A running widget's life, and the state its row in Installed shows.
//!
//! [`Runner`] calls `refresh` on time, keeps the last good state when it
//! fails and waits longer after each failure, and stops the widget when it
//! asks for more than it was given or keeps hitting the limits.
//! [`cell`] turns that and the install record into one of the six states
//! every Marketplace item uses: Ready, Broken (the word says why), Waiting,
//! Update, Not installed and Paused (the off state).

use std::time::{Duration, Instant, SystemTime};

use rhai::{Dynamic, Map};

use super::blocks::Block;
use super::sandbox::{Failure, Stop, Widget};
use super::store::Record;

/// The longest wait after failures.
const MAX_BACKOFF: Duration = Duration::from_secs(15 * 60);
/// Limit hits in a row that stop a widget.
const LIMIT_HITS: u32 = 3;

/// What the Agents cell says for every widget.
pub const AGENTS: &str = "Runs in Arakun, for any agent";
/// What the Cloud column says: a widget runs where the app runs.
pub const NOT_IN_CLOUD: &str = "Does not run in Cloud";

#[derive(Clone, Debug, PartialEq)]
pub enum Status {
    /// Before the first refresh ends.
    Starting,
    Ready,
    /// The last refresh failed. The widget shows its last state, when that
    /// was and the error.
    Failing {
        error: String,
        last_ok: Option<SystemTime>,
    },
    Stopped(Stop),
}

pub struct Runner {
    pub widget: Widget,
    state: Dynamic,
    pub status: Status,
    every: Duration,
    next: Instant,
    failures: u32,
    limit_hits: u32,
    last_ok: Option<SystemTime>,
}

impl Runner {
    pub fn new(widget: Widget, now: Instant) -> Runner {
        let every = Duration::from_secs(widget.manifest.every());
        Runner {
            widget,
            state: Map::new().into(),
            status: Status::Starting,
            every,
            next: now,
            failures: 0,
            limit_hits: 0,
            last_ok: None,
        }
    }

    /// When `refresh` runs next; None once stopped.
    pub fn next(&self) -> Option<Instant> {
        (!matches!(self.status, Status::Stopped(_))).then_some(self.next)
    }

    /// Runs `refresh` if it is due. True when the status or state changed.
    pub fn tick(&mut self, now: Instant) -> bool {
        if self.next().is_none_or(|n| now < n) {
            return false;
        }
        match self.widget.refresh(&self.state) {
            Ok(state) => {
                self.state = state;
                self.status = Status::Ready;
                self.failures = 0;
                self.limit_hits = 0;
                self.last_ok = Some(SystemTime::now());
                self.next = now + self.every;
            }
            Err(Failure::Stop(stop)) => self.status = Status::Stopped(stop),
            Err(Failure::Limit(why)) => {
                self.limit_hits += 1;
                if self.limit_hits >= LIMIT_HITS {
                    self.status = Status::Stopped(Stop::Limit(format!("Stopped: {why}")));
                } else {
                    self.fail(why, now);
                }
            }
            Err(Failure::Error(why)) => self.fail(why, now),
        }
        true
    }

    fn fail(&mut self, error: String, now: Instant) {
        self.failures += 1;
        self.status = Status::Failing {
            error,
            last_ok: self.last_ok,
        };
        self.next = now + backoff(self.every, self.failures);
    }

    /// What to draw at `size`. None while starting or stopped; the place
    /// shows the status instead.
    pub fn view(&mut self, size: &str) -> Option<Block> {
        if matches!(self.status, Status::Starting | Status::Stopped(_)) {
            return None;
        }
        match self.widget.view(&self.state, size) {
            Ok(b) => Some(b),
            Err(Failure::Stop(stop)) => {
                self.status = Status::Stopped(stop);
                None
            }
            Err(Failure::Error(e) | Failure::Limit(e)) => Some(Block::Empty(e)),
        }
    }
}

/// The wait before the next try: `every` doubled for each failure in a row,
/// up to 15 minutes, never less than `every`.
pub fn backoff(every: Duration, failures: u32) -> Duration {
    let doubled = every.saturating_mul(1 << failures.min(16));
    doubled.min(MAX_BACKOFF).max(every)
}

/// The columns of Installed: the places agents run.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Column {
    ThisComputer,
    /// A paired computer. Widgets don't go there yet.
    Paired,
    Cloud,
}

/// One cell of a widget's row.
#[derive(Clone, Debug, PartialEq)]
pub enum Cell {
    Ready,
    /// Stopped, with why.
    Broken(String),
    /// Waiting for the user, with what for.
    Waiting(String),
    /// A newer version asks for more.
    Update(String),
    NotInstalled,
    Paused,
    /// Not a state: the place has no widgets at all.
    DoesNotRun,
}

impl Cell {
    pub fn label(&self) -> String {
        match self {
            Cell::Ready => "Ready".into(),
            Cell::Broken(why) | Cell::Waiting(why) => why.clone(),
            Cell::Update(v) => format!("Update {v}"),
            Cell::NotInstalled => "Not installed".into(),
            Cell::Paused => "Paused".into(),
            Cell::DoesNotRun => NOT_IN_CLOUD.into(),
        }
    }
}

/// The state `rec` shows in `column`, with `live` the runner's status on
/// this computer, if it runs.
pub fn cell(rec: &Record, column: Column, live: Option<&Status>) -> Cell {
    match column {
        Column::Cloud => return Cell::DoesNotRun,
        Column::Paired => return Cell::NotInstalled,
        Column::ThisComputer => {}
    }
    if let Some(why) = &rec.pulled {
        return Cell::Broken(format!("Pulled: {why}"));
    }
    if rec.paused {
        return Cell::Paused;
    }
    if let Some(w) = &rec.waiting {
        return Cell::Update(w.version.clone());
    }
    match live {
        Some(Status::Stopped(Stop::NeedsToken(why))) => Cell::Waiting(why.clone()),
        Some(Status::Stopped(stop)) => Cell::Broken(stop.reason().to_string()),
        Some(Status::Failing {
            error,
            last_ok: None,
        }) => Cell::Broken(error.clone()),
        _ => Cell::Ready,
    }
}

/// The second line under its name: "Widget · by mrvn · Not verified".
pub fn kind_line(rec: &Record) -> String {
    let verified = if rec.source.verified {
        "Verified"
    } else {
        "Not verified"
    };
    format!("Widget · by {} · {verified}", rec.author())
}

/// The Set for cell: who it is for and the place.
pub fn set_for(rec: &Record) -> (String, &'static str) {
    let project = rec
        .project
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    (format!("Just you, in {project}"), rec.place.label())
}

#[cfg(test)]
mod tests {
    use super::super::manifest::{DEPLOY_BOARD, Grant, Manifest};
    use super::super::sandbox::tests::Fake;
    use super::super::store::{Place, Source, Waiting};
    use super::*;

    fn runner(src: &str) -> Runner {
        let m = Manifest::parse(DEPLOY_BOARD).unwrap();
        let g = Grant::of(&m);
        let w = Widget::new(m, src, g, Box::new(Fake::default())).unwrap();
        Runner::new(w, Instant::now())
    }

    fn record() -> Record {
        let m = Manifest::parse(DEPLOY_BOARD).unwrap();
        Record {
            id: m.widget.id.clone(),
            name: m.widget.name.clone(),
            version: m.widget.version.clone(),
            source: Source::default(),
            place: Place::Workspace,
            size: "wide".into(),
            project: "/code/arakun".into(),
            paused: false,
            grant: Grant::of(&m),
            waiting: None,
            pulled: None,
        }
    }

    #[test]
    fn a_failing_refresh_keeps_the_state_and_backs_off() {
        let mut r = runner(
            r#"fn refresh(s) { if s.n == () { #{ n: 1 } } else { throw "down" } }
               fn view(s, z) { text(`${s.n}`) }"#,
        );
        let t0 = Instant::now();
        assert!(r.tick(t0));
        assert_eq!(r.status, Status::Ready);
        assert!(!r.tick(t0 + Duration::from_secs(5)), "not due yet");
        let t1 = t0 + Duration::from_secs(30);
        assert!(r.tick(t1));
        assert!(matches!(
            r.status,
            Status::Failing {
                last_ok: Some(_),
                ..
            }
        ));
        assert_eq!(r.next(), Some(t1 + Duration::from_secs(60)));
        assert!(matches!(r.view("wide"), Some(Block::Text { text, .. }) if text == "1"));
    }

    #[test]
    fn hitting_the_limits_again_and_again_stops_it() {
        let mut r = runner(r#"fn refresh(s) { loop {} } fn view(s, z) { empty("") }"#);
        let mut t = Instant::now();
        for _ in 0..LIMIT_HITS {
            r.tick(t);
            t += MAX_BACKOFF;
        }
        assert!(matches!(r.status, Status::Stopped(Stop::Limit(_))));
        assert_eq!(r.next(), None);
        assert_eq!(r.view("wide"), None);
    }

    #[test]
    fn asking_for_more_stops_it_with_why() {
        let mut r = runner(r#"fn refresh(s) { sessions(); #{} } fn view(s, z) { empty("") }"#);
        r.tick(Instant::now());
        let c = cell(&record(), Column::ThisComputer, Some(&r.status));
        assert_eq!(
            c.label(),
            "It asked for your sessions, which you did not allow"
        );
    }

    #[test]
    fn backoff_doubles_up_to_fifteen_minutes() {
        let every = Duration::from_secs(30);
        assert_eq!(backoff(every, 1), Duration::from_secs(60));
        assert_eq!(backoff(every, 3), Duration::from_secs(240));
        assert_eq!(backoff(every, 30), MAX_BACKOFF);
        assert_eq!(
            backoff(Duration::from_secs(3600), 1),
            Duration::from_secs(3600)
        );
    }

    #[test]
    fn the_six_states() {
        let mut rec = record();
        let ready = Some(&Status::Ready);
        assert_eq!(cell(&rec, Column::ThisComputer, ready), Cell::Ready);
        assert_eq!(cell(&rec, Column::Paired, ready), Cell::NotInstalled);
        assert_eq!(cell(&rec, Column::Cloud, ready).label(), NOT_IN_CLOUD);
        let token = Status::Stopped(Stop::NeedsToken("Needs your Vercel token".into()));
        assert_eq!(
            cell(&rec, Column::ThisComputer, Some(&token)),
            Cell::Waiting("Needs your Vercel token".into())
        );
        rec.waiting = Some(Waiting {
            version: "0.5.0".into(),
            source: Source::default(),
            grant: Grant::default(),
            asks: vec![],
        });
        assert_eq!(
            cell(&rec, Column::ThisComputer, ready).label(),
            "Update 0.5.0"
        );
        rec.paused = true;
        assert_eq!(cell(&rec, Column::ThisComputer, ready), Cell::Paused);
        rec.pulled = Some("Malware".into());
        assert_eq!(
            cell(&rec, Column::ThisComputer, None).label(),
            "Pulled: Malware"
        );
        assert_eq!(kind_line(&rec), "Widget · by mrvn · Not verified");
        assert_eq!(
            set_for(&rec),
            ("Just you, in arakun".into(), "Workspace panel")
        );
    }
}
