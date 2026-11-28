//! How the tab bar moves: a new tab grows in, a closed one shrinks away while
//! its neighbours slide into the gap, and the active tab's fill slides from
//! the tab it left.
//!
//! The strip changes at once, so shortcuts never wait on an animation. The
//! bar compares each frame with the one before: a tab id it has not seen
//! grows in, and a tab gone from the strip is drawn for [`FAST`] more as an
//! inert ghost that shrinks to nothing. Several tabs closed at once all leave
//! in the same frame, so they shrink together.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::time::Instant;

use gpui::{Bounds, Pixels};

use crate::ui::motion::FAST;

/// Where a ghost is drawn: in front of the tab that followed it, or after
/// the tab before it when it was the last one shown.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum Anchor {
    Before(u64),
    After(u64),
}

/// A closed tab on its way out.
pub(super) struct Ghost<G> {
    pub id: u64,
    pub look: G,
    pub active: bool,
    /// Its width when it closed, which it shrinks from.
    pub width: f32,
    pub anchor: Anchor,
    since: Instant,
}

/// The active fill sliding onto tab `to`, `to_w` px wide: it starts `dx` px
/// from it (negative is to the left) and `from_w` px wide.
#[derive(Clone, Copy, PartialEq, Debug)]
pub(super) struct Marker {
    pub generation: u64,
    pub to: u64,
    pub dx: f32,
    pub from_w: f32,
    pub to_w: f32,
    since: Instant,
}

/// A tab shown in the bar this frame, with what its ghost needs.
pub(super) struct Shown<S> {
    pub id: u64,
    pub look: S,
}

pub(super) struct TabMotion<S, G> {
    started: bool,
    /// Every tab in the strip last frame, shown or not.
    known: HashSet<u64>,
    /// The tabs shown last frame, in order, and which was active.
    shown: Vec<Shown<S>>,
    active: Option<u64>,
    born: HashMap<u64, Instant>,
    pub ghosts: Vec<Ghost<G>>,
    pub marker: Option<Marker>,
    /// Where each shown tab was drawn, filled while the bar paints. Read
    /// before the next frame paints, so it holds the last frame's places.
    pub bounds: Rc<RefCell<HashMap<u64, Bounds<Pixels>>>>,
}

impl<S, G> Default for TabMotion<S, G> {
    fn default() -> Self {
        Self {
            started: false,
            known: HashSet::new(),
            shown: Vec::new(),
            active: None,
            born: HashMap::new(),
            ghosts: Vec::new(),
            marker: None,
            bounds: Rc::default(),
        }
    }
}

impl<S, G> TabMotion<S, G> {
    /// Notes this frame's bar: `strip` is every tab id in the strip, `shown`
    /// the tabs the bar draws, in order, and `active` the active tab. `ghost`
    /// turns the look of a closed tab into what its ghost draws. With
    /// `reduced` set nothing moves, and what was moving stops.
    pub fn step(
        &mut self,
        strip: &[u64],
        shown: Vec<Shown<S>>,
        active: Option<u64>,
        now: Instant,
        reduced: bool,
        ghost: impl Fn(S) -> G,
    ) {
        let places: HashMap<u64, (f32, f32)> = self
            .bounds
            .borrow()
            .iter()
            .map(|(id, b)| (*id, (f32::from(b.origin.x), f32::from(b.size.width))))
            .collect();
        let live = |since: Instant| now.saturating_duration_since(since) < FAST;
        self.born.retain(|_, since| live(*since));
        self.ghosts.retain(|g| live(g.since));
        self.marker = self.marker.filter(|m| live(m.since));
        if reduced {
            self.born.clear();
            self.ghosts.clear();
            self.marker = None;
        }
        let strip_now: HashSet<u64> = strip.iter().copied().collect();
        let shown_now: HashSet<u64> = shown.iter().map(|s| s.id).collect();
        if self.started && !reduced {
            for id in strip {
                if !self.known.contains(id) {
                    self.born.insert(*id, now);
                }
            }
            let before = std::mem::take(&mut self.shown);
            let anchors: Vec<Option<Anchor>> = (0..before.len())
                .map(|pos| {
                    let after = before[pos + 1..]
                        .iter()
                        .find(|s| shown_now.contains(&s.id))
                        .map(|s| Anchor::Before(s.id));
                    after.or_else(|| {
                        before[..pos]
                            .iter()
                            .rev()
                            .find(|s| shown_now.contains(&s.id))
                            .map(|s| Anchor::After(s.id))
                    })
                })
                .collect();
            // A ghost still leaving in front of a tab that closes now moves
            // to that tab's place.
            let moved: HashMap<u64, Option<Anchor>> = before
                .iter()
                .zip(&anchors)
                .filter(|(s, _)| !shown_now.contains(&s.id))
                .map(|(s, a)| (s.id, *a))
                .collect();
            self.ghosts.retain_mut(|g| {
                let (Anchor::Before(id) | Anchor::After(id)) = g.anchor;
                match moved.get(&id) {
                    None => shown_now.contains(&id),
                    Some(Some(a)) => {
                        g.anchor = *a;
                        true
                    }
                    Some(None) => false,
                }
            });
            for (s, anchor) in before.into_iter().zip(anchors) {
                if strip_now.contains(&s.id) {
                    continue;
                }
                let (Some(anchor), Some(&(_, width))) = (anchor, places.get(&s.id)) else {
                    continue;
                };
                self.ghosts.push(Ghost {
                    id: s.id,
                    active: self.active == Some(s.id),
                    look: ghost(s.look),
                    width,
                    anchor,
                    since: now,
                });
            }
            if active != self.active {
                // Only between two tabs that were both on the bar: a tab
                // that just opened grows in with its fill.
                let from = self.active.filter(|id| shown_now.contains(id));
                self.marker = match (from.and_then(|id| places.get(&id)), active) {
                    (Some(&(x, w)), Some(to))
                        if shown_now.contains(&to) && !self.born.contains_key(&to) =>
                    {
                        places.get(&to).map(|&(to_x, to_w)| Marker {
                            generation: self.marker.map_or(0, |m| m.generation + 1),
                            to,
                            dx: x - to_x,
                            from_w: w,
                            to_w,
                            since: now,
                        })
                    }
                    _ => None,
                };
            }
        }
        self.started = true;
        self.known = strip_now;
        self.shown = shown;
        self.active = active;
        // Tabs not drawn this frame must not leave old places behind.
        self.bounds
            .borrow_mut()
            .retain(|id, _| shown_now.contains(id));
    }

    /// Whether tab `id` opened in the last [`FAST`] and grows in.
    pub fn growing(&self, id: u64) -> bool {
        self.born.contains_key(&id)
    }

    /// Whether anything in the bar is still moving.
    #[cfg(test)]
    fn moving(&self) -> bool {
        !self.born.is_empty() || !self.ghosts.is_empty() || self.marker.is_some()
    }

    /// The ghosts drawn at `anchor`, oldest place first.
    pub fn ghosts_at(&self, anchor: Anchor) -> impl Iterator<Item = &Ghost<G>> {
        self.ghosts.iter().filter(move |g| g.anchor == anchor)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{point, px, size};

    type Motion = TabMotion<&'static str, &'static str>;

    fn shown(ids: &[u64]) -> Vec<Shown<&'static str>> {
        ids.iter().map(|&id| Shown { id, look: "tab" }).collect()
    }

    /// Records where the bar drew each tab: 100 px wide, side by side.
    fn paint(m: &Motion, ids: &[u64]) {
        let mut b = m.bounds.borrow_mut();
        for (i, id) in ids.iter().enumerate() {
            b.insert(
                *id,
                Bounds::new(point(px(i as f32 * 100.), px(0.)), size(px(100.), px(30.))),
            );
        }
    }

    fn frame(m: &mut Motion, ids: &[u64], active: u64, now: Instant) {
        m.step(ids, shown(ids), Some(active), now, false, |s| s);
        paint(m, ids);
    }

    #[test]
    fn the_first_frame_moves_nothing() {
        let mut m = Motion::default();
        frame(&mut m, &[1, 2, 3], 1, Instant::now());
        assert!(!m.moving());
    }

    #[test]
    fn a_new_tab_grows_in_for_fast() {
        let mut m = Motion::default();
        let t = Instant::now();
        frame(&mut m, &[1, 2], 1, t);
        frame(&mut m, &[1, 2, 3], 1, t);
        assert!(m.growing(3));
        assert!(!m.growing(1));
        frame(&mut m, &[1, 2, 3], 1, t + FAST);
        assert!(!m.growing(3));
    }

    #[test]
    fn closed_tabs_leave_together_where_they_were() {
        let mut m = Motion::default();
        let t = Instant::now();
        frame(&mut m, &[1, 2, 3, 4], 1, t);
        // Close 2 and 3 at once: both shrink in front of 4, in this frame.
        frame(&mut m, &[1, 4], 1, t);
        let at: Vec<u64> = m.ghosts_at(Anchor::Before(4)).map(|g| g.id).collect();
        assert_eq!(at, vec![2, 3]);
        assert!(m.ghosts.iter().all(|g| g.width == 100.));
        // The last tab leaves after the one before it, and takes the
        // ghosts still leaving in front of it along.
        frame(&mut m, &[1], 1, t);
        let at: Vec<u64> = m.ghosts_at(Anchor::After(1)).map(|g| g.id).collect();
        assert_eq!(at, vec![2, 3, 4]);
        frame(&mut m, &[1], 1, t + FAST);
        assert!(m.ghosts.is_empty());
    }

    #[test]
    fn a_ghost_follows_when_its_neighbour_closes_too() {
        let mut m = Motion::default();
        let t = Instant::now();
        frame(&mut m, &[1, 2, 3, 4], 1, t);
        frame(&mut m, &[1, 3, 4], 1, t);
        // 3, which 2 was leaving in front of, closes before 2 is gone.
        frame(&mut m, &[1, 4], 1, t + FAST / 2);
        let at: Vec<u64> = m.ghosts_at(Anchor::Before(4)).map(|g| g.id).collect();
        assert_eq!(at, vec![2, 3]);
    }

    #[test]
    fn a_hidden_tab_is_not_closed() {
        let mut m = Motion::default();
        let t = Instant::now();
        frame(&mut m, &[1, 2, 3], 1, t);
        // 3 is still in the strip, just not on the bar (another project).
        m.step(&[1, 2, 3], shown(&[1, 2]), Some(1), t, false, |s| s);
        assert!(m.ghosts.is_empty());
    }

    #[test]
    fn the_fill_slides_from_the_tab_it_left() {
        let mut m = Motion::default();
        let t = Instant::now();
        frame(&mut m, &[1, 2, 3], 1, t);
        frame(&mut m, &[1, 2, 3], 3, t);
        let mk = m.marker.expect("a slide");
        assert_eq!((mk.to, mk.dx, mk.from_w), (3, -200., 100.));
        frame(&mut m, &[1, 2, 3], 2, t);
        assert_eq!(m.marker.map(|m| m.generation), Some(mk.generation + 1));
        // Onto a tab that just opened: it grows in, no slide.
        frame(&mut m, &[1, 2, 3, 4], 4, t);
        assert!(m.marker.is_none());
    }

    #[test]
    fn reduced_motion_snaps() {
        let mut m = Motion::default();
        let t = Instant::now();
        frame(&mut m, &[1, 2, 3], 1, t);
        m.step(&[1, 3, 4], shown(&[1, 3, 4]), Some(3), t, true, |s| s);
        assert!(!m.moving());
    }
}
