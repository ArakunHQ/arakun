//! Tells each followed server how long the person has been idle here
//! (`idle`). A server has no display of its own: it holds a phone's notice
//! until the person is away, and only an app knows when that is.

use std::time::{Duration, Instant};

use super::*;

/// How often idle time is read while a server is connected.
const CHECK_EVERY: Duration = Duration::from_secs(5);
/// A server trusts what it heard for 60 s, so it hears again well before.
const TELL_EVERY: Duration = Duration::from_secs(30);

/// Whether a server that was last told `told` (idle seconds, and when) needs
/// to hear `secs` at `now`: never told, idle time on another whole minute (the
/// away times are whole minutes), or told 30 s ago.
fn due(told: Option<(u64, Instant)>, secs: u64, now: Instant) -> bool {
    told.is_none_or(|(was, at)| {
        was / 60 != secs / 60 || now.saturating_duration_since(at) >= TELL_EVERY
    })
}

impl Shell {
    /// Reads idle time off the main thread every few seconds and tells the
    /// connected servers that need it, for as long as the servers are linked.
    pub(super) fn servers_time_idle(window: &mut Window, cx: &mut Context<Self>) -> Task<()> {
        cx.spawn_in(window, async move |this, cx| {
            loop {
                cx.background_executor().timer(CHECK_EVERY).await;
                let Ok(any) = this.update(cx, |this, _| this.servers_want_idle()) else {
                    break;
                };
                if !any {
                    continue;
                }
                // A Linux desktop is asked over D-Bus, which may be slow.
                let idle = cx.background_spawn(async { crate::idle::idle_for() }).await;
                // Unknown here: the servers fall back to their own wait.
                let Some(idle) = idle else { continue };
                if this
                    .update(cx, |this, _| {
                        this.servers_tell_idle(idle.as_secs(), Instant::now())
                    })
                    .is_err()
                {
                    break;
                }
            }
        })
    }

    /// Whether any connected server would take an idle time.
    fn servers_want_idle(&self) -> bool {
        self.servers.as_ref().is_some_and(|l| {
            l.conns
                .values()
                .any(|c| c.reach == Reach::Connected && !c.no_idle)
        })
    }

    fn servers_tell_idle(&mut self, secs: u64, now: Instant) {
        let Some(link) = &mut self.servers else {
            return;
        };
        for conn in link.conns.values_mut() {
            if conn.reach != Reach::Connected || conn.no_idle || !due(conn.idle_told, secs, now) {
                continue;
            }
            conn.client.send(&json!({ "type": "idle", "secs": secs }));
            conn.idle_told = Some((secs, now));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_server_hears_at_once_then_on_each_new_minute() {
        let start = Instant::now();
        assert!(due(None, 0, start));
        let told = Some((10, start));
        let soon = start + Duration::from_secs(5);
        assert!(!due(told, 15, soon), "same minute");
        assert!(due(told, 60, soon), "crossed a minute");
        // Back at the desk after a while away.
        assert!(due(Some((300, start)), 0, soon));
    }

    #[test]
    fn a_server_hears_again_every_30_s() {
        let start = Instant::now();
        let told = Some((10, start));
        assert!(!due(told, 10, start + Duration::from_secs(29)));
        assert!(due(told, 10, start + TELL_EVERY));
    }
}
