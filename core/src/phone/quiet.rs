//! How long a peer has been quiet, counting the time the computer slept.
//!
//! `Instant` stops while a laptop sleeps (on macOS and Linux), so a check made
//! with it alone takes the full silence again after waking before it gives up
//! on a socket that died in the meantime. The wall clock keeps going, but it
//! can jump back; the longer of the two is right in both cases.

use std::time::{Duration, Instant, SystemTime};

#[derive(Clone, Copy, Debug)]
pub(crate) struct Quiet {
    mono: Instant,
    wall: SystemTime,
}

impl Quiet {
    pub(crate) fn now() -> Self {
        Quiet {
            mono: Instant::now(),
            wall: SystemTime::now(),
        }
    }

    /// The time since `now()`, sleep included.
    pub(crate) fn elapsed(&self) -> Duration {
        let wall = SystemTime::now()
            .duration_since(self.wall)
            .unwrap_or_default();
        self.mono.elapsed().max(wall)
    }

    #[cfg(test)]
    fn at(mono: Instant, wall: SystemTime) -> Self {
        Quiet { mono, wall }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counts_time_the_monotonic_clock_missed() {
        // A wall clock that moved a minute while the monotonic one did not: asleep.
        let q = Quiet::at(Instant::now(), SystemTime::now() - Duration::from_secs(60));
        assert!(q.elapsed() >= Duration::from_secs(60));
    }

    #[test]
    fn a_wall_clock_set_back_does_not_hide_the_silence() {
        let q = Quiet::at(
            Instant::now() - Duration::from_secs(60),
            SystemTime::now() + Duration::from_secs(3600),
        );
        assert!(q.elapsed() >= Duration::from_secs(60));
    }
}
