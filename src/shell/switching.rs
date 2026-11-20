//! Going from one thread to another without a blank beat: the transcript a
//! click would show is read while the pointer rests on its row, the new
//! thread fades in from the sheet, and "Connecting…" only shows when the
//! wait is long enough to need saying.

use std::collections::HashMap;
use std::path::Path;
use std::time::{Duration, Instant, SystemTime};

use gpui::{
    Animation, AnimationExt as _, AnyElement, AppContext as _, Context, IntoElement, SharedString,
    Styled, div,
};

use super::Shell;
use super::sessions::read_restored;
use crate::agent::Restored;
use crate::model::TabKind;
use crate::state;
use crate::theme;
use crate::ui::motion;

/// How many transcripts read ahead wait for their click at most.
const KEEP: usize = 4;
/// A read nobody clicked on is dropped after this.
const STALE: Duration = Duration::from_secs(60);
/// How long a thread takes to fade in.
const FADE: Duration = motion::BASE;
/// "Connecting…" waits this long: most transcripts are there before it, and
/// a word that flashes for a moment reads as a glitch.
const QUIET: Duration = Duration::from_millis(400);

/// The transcript file as it was read: a write since makes the read stale.
type Stamp = Option<(SystemTime, u64)>;

fn stamp(path: &Path) -> Stamp {
    let m = std::fs::metadata(path).ok()?;
    Some((m.modified().ok()?, m.len()))
}

/// A transcript read before its click.
pub(super) struct Ahead {
    at: Instant,
    stamp: Stamp,
    /// `None` while it is still being read.
    read: Option<anyhow::Result<Option<Restored>>>,
}

/// The pane's thread as last drawn, and the fade it started.
#[derive(Default)]
pub(super) struct Shown {
    /// The session on screen, and whether its transcript was there.
    session: Option<(SharedString, bool)>,
    /// Counts fades, so each one starts its animation over.
    fades: usize,
    fade_at: Option<Instant>,
}

/// Which entries of `ahead` to drop to make room for one more: the stale
/// ones, then the oldest until there are fewer than [`KEEP`].
fn to_drop(ahead: &HashMap<SharedString, Ahead>, now: Instant) -> Vec<SharedString> {
    let mut by_age: Vec<(&SharedString, Instant)> =
        ahead.iter().map(|(id, a)| (id, a.at)).collect();
    by_age.sort_by_key(|(_, at)| *at);
    let stale = by_age
        .iter()
        .filter(|(_, at)| now.duration_since(*at) >= STALE)
        .count();
    let over = by_age.len().saturating_sub(KEEP - 1);
    by_age
        .into_iter()
        .take(stale.max(over))
        .map(|(id, _)| id.clone())
        .collect()
}

impl Shell {
    /// Starts reading the transcript a click on environment `ei` of project
    /// `pi` would show (`opens`, or the tab it showed last), so the click can
    /// show it at once.
    pub(super) fn read_ahead(
        &mut self,
        pi: usize,
        ei: usize,
        opens: Option<SharedString>,
        cx: &mut Context<Self>,
    ) {
        let Some(env) = self.projects.get(pi).and_then(|p| p.environments.get(ei)) else {
            return;
        };
        // A server's sessions are shown from there.
        if env.remote.is_some() {
            return;
        }
        let id = opens.or_else(|| {
            let tab = &self.strip.tabs[self.strip.front_of(&env.path)?];
            match &tab.kind {
                TabKind::Session { id, .. } => Some(id.clone()),
                _ => None,
            }
        });
        let Some(id) = id else {
            return;
        };
        let cold = env
            .session_index(&id)
            .is_some_and(|i| env.sessions[i].live.is_none());
        if !cold
            || self.ahead.contains_key(&id)
            || self.restoring.contains(&id)
            || crate::headless::shown(&id)
        {
            return;
        }
        for gone in to_drop(&self.ahead, Instant::now()) {
            self.ahead.remove(&gone);
        }
        self.ahead.insert(
            id.clone(),
            Ahead {
                at: Instant::now(),
                stamp: None,
                read: None,
            },
        );
        let path = state::transcript_path(&id);
        cx.spawn(async move |this, cx| {
            let (stamp, read) = cx
                .background_spawn(crate::guard::task(async move {
                    // Before the read: a write during it makes the stamp old.
                    (stamp(&path), read_restored(&path))
                }))
                .await;
            let _ = this.update(cx, |this, _| {
                if let Some(a) = this.ahead.get_mut(&id) {
                    a.stamp = stamp;
                    a.read = Some(read);
                }
            });
        })
        .detach();
    }

    /// The transcript of session `id` read ahead, if it is done and the
    /// file has not changed since.
    pub(super) fn take_ahead(&mut self, id: &str) -> Option<anyhow::Result<Option<Restored>>> {
        let a = self.ahead.remove(id)?;
        let read = a.read?;
        (a.stamp == stamp(&state::transcript_path(id))).then_some(read)
    }

    /// Notes which session the pane draws, and whether its transcript is
    /// there; when that changes, the pane fades in.
    pub(super) fn note_shown(&self, id: &SharedString, ready: bool) {
        let mut shown = self.shown.borrow_mut();
        let now = Some((id.clone(), ready));
        if shown.session != now {
            shown.session = now;
            shown.fades += 1;
            shown.fade_at = Some(Instant::now());
        }
    }

    /// A sheet over the thread that fades out, so the thread fades in.
    pub(super) fn render_fade(&self) -> Option<AnyElement> {
        let shown = self.shown.borrow();
        let at = shown.fade_at?;
        if at.elapsed() >= FADE {
            return None;
        }
        Some(
            div()
                .absolute()
                .inset_0()
                .bg(theme::sheet())
                .with_animation(
                    ("thread-fade", shown.fades),
                    Animation::new(FADE),
                    |d, t| d.opacity(1. - motion::ease_out(t)),
                )
                .into_any_element(),
        )
    }
}

/// What the pane says while a thread's transcript is read: nothing at first,
/// then "Connecting…" when it takes a while.
pub(super) fn render_waiting(id: &SharedString) -> AnyElement {
    let wait = QUIET.as_secs_f32();
    let total = wait + motion::FAST.as_secs_f32();
    super::panel_empty("Connecting…")
        .with_animation(
            SharedString::from(format!("waiting-{id}")),
            Animation::new(Duration::from_secs_f32(total)),
            move |d, t| {
                let shown = ((t * total - wait) / (total - wait)).clamp(0., 1.);
                d.opacity(shown)
            },
        )
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ahead_at(at: Instant) -> Ahead {
        Ahead {
            at,
            stamp: None,
            read: None,
        }
    }

    #[test]
    fn reads_ahead_keep_room_for_one_more() {
        let now = Instant::now();
        let mut ahead = HashMap::new();
        for (i, age) in [3u64, 1, 2].into_iter().enumerate() {
            let at = now - Duration::from_secs(age);
            ahead.insert(SharedString::from(format!("s{i}")), ahead_at(at));
        }
        // Three of four: room enough.
        assert!(to_drop(&ahead, now).is_empty());
        ahead.insert("s3".into(), ahead_at(now));
        // Four: the oldest goes.
        assert_eq!(to_drop(&ahead, now), vec![SharedString::from("s0")]);
    }

    #[test]
    fn stale_reads_go() {
        let now = Instant::now();
        let mut ahead = HashMap::new();
        ahead.insert("old".into(), ahead_at(now - STALE));
        ahead.insert("new".into(), ahead_at(now));
        assert_eq!(to_drop(&ahead, now), vec![SharedString::from("old")]);
    }

    #[test]
    fn a_changed_file_makes_its_read_stale() {
        let dir = std::env::temp_dir().join(format!("arakun-ahead-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("t.json");
        assert_eq!(stamp(&file), None);
        std::fs::write(&file, "a").unwrap();
        let first = stamp(&file);
        assert!(first.is_some());
        std::fs::write(&file, "ab").unwrap();
        assert_ne!(stamp(&file), first);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
