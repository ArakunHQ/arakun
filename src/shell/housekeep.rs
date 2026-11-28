//! The app's cleanup of what it keeps on disk: once when the window opens and
//! then once a day, the way `arakun server` does (`Daemon::housekeep_tick`).
//! People leave the window open for weeks, so startup alone is not enough.
//! Each pass that removes files Arakun no longer needs belongs here.

use std::collections::HashSet;
use std::time::{Duration, Instant};

use gpui::{AppContext as _, Context, Window};

use super::{Shell, servers, sessions};
use crate::state;

/// How often the cleanup runs while the window stays open.
const EVERY: Duration = crate::housekeep::DAY;

/// Whether a run is due at `now`, the last one having run at `last`.
fn due(last: Option<Instant>, now: Instant) -> bool {
    last.is_none_or(|at| now.saturating_duration_since(at) >= EVERY)
}

impl Shell {
    /// Called when the window opens and on the scheduler's tick; runs the
    /// cleanup at most every [`EVERY`], off this thread.
    pub(super) fn housekeep(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let now = Instant::now();
        if !due(self.housekept_at, now) {
            return;
        }
        self.housekept_at = Some(now);
        // Before the sessions are listed: their files go with them.
        self.clean_up_sessions(window, cx);
        // From the state in memory, not from the state file: a refresh can
        // have saved one without a worktree removed outside Arakun, and a
        // save may still be queued. A session dropped that way is only
        // marked gone here; its files stay a week.
        let listed = self.listed_session_ids();
        let images = servers::images_dir();
        cx.background_spawn(crate::guard::task(async move {
            state::sweep_orphans(listed);
            crate::cleanup::sweep_leftovers(&images);
        }))
        .detach();
    }

    /// The ids whose files the sessions in the state own.
    fn listed_session_ids(&self) -> HashSet<String> {
        self.projects
            .iter()
            .flat_map(|p| &p.environments)
            .flat_map(|e| &e.sessions)
            .flat_map(sessions::session_file_ids)
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn runs_at_start_then_once_a_day() {
        let start = Instant::now();
        assert!(due(None, start));
        assert!(!due(Some(start), start));
        assert!(!due(Some(start), start + Duration::from_secs(3600)));
        assert!(!due(Some(start), start + EVERY - Duration::from_secs(1)));
        assert!(due(Some(start), start + EVERY));
        assert!(due(Some(start), start + 3 * EVERY));
    }

    #[test]
    fn a_clock_before_the_last_run_waits() {
        let start = Instant::now();
        let later = start + Duration::from_secs(60);
        assert!(!due(Some(later), start));
    }
}
