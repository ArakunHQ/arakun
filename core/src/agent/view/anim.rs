//! How the transcript moves: the live beacon, spinners and shimmer that loop
//! while the agent works, and the way entries and folds arrive.
//!
//! Loops run on the app's motion clock (`motion::looping`), which redraws at
//! `motion::LOOP_FPS`, or at its full rate on ticks that draw a frame anyway,
//! rather than at the display's rate. It keeps loops side by side in step,
//! and holds them still when the system reduces motion.
//! Arrivals are keyed by entry, not by element: the list drops an element's
//! state when it scrolls away, and an old reply must not replay its entrance.

use super::*;
use crate::ui::motion;
use gpui::{AnimationExt as _, Svg};

/// One turn of the spinner's arc.
const SPIN: Duration = Duration::from_millis(1000);
/// One sweep of the shimmer's bright band, pause included.
const SHIMMER: Duration = Duration::from_millis(2000);
/// One breath of the pulse on a streaming thought.
const PULSE: Duration = Duration::from_millis(1800);
/// How far an arriving entry rises into place.
const RISE: f32 = 6.;
/// How far an opened fold's contents drop into place.
const DROP: f32 = 4.;
/// More entries than this in one change is a load, not the agent at work.
const MAX_ARRIVALS: usize = 6;

/// What the live beacon says.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum Beacon {
    /// The agent works: a ring swells out of the dot and fades, steadily.
    Working,
    /// The agent waits on the user: the same ring, slower.
    Waiting,
    /// Nothing to watch (stopping, or the window is in the background).
    Still,
}

impl Beacon {
    fn period(self) -> Option<Duration> {
        match self {
            Beacon::Working => Some(Duration::from_millis(1600)),
            Beacon::Waiting => Some(Duration::from_millis(2400)),
            Beacon::Still => None,
        }
    }
}

const BEACON_BOX: f32 = 14.;
const BEACON_DOT: f32 = 6.;

/// The ring `t` (0 to 1) into a beat: its diameter and opacity. It swells
/// over the first three quarters and rests for the last, so beats read apart.
pub(super) fn ring_at(t: f32) -> (f32, f32) {
    let g = (t / 0.75).min(1.);
    let size = BEACON_DOT + (BEACON_BOX - BEACON_DOT) * motion::ease_out(g);
    let opacity = 0.6 * (1. - g) * (1. - g);
    (size, opacity)
}

/// A dot with a ring that swells out of it: the agent is working, or waits.
pub(super) fn beacon(state: Beacon, color: Hsla) -> AnyElement {
    let offset = |size: f32| px((BEACON_BOX - size) / 2.);
    let dot = div()
        .absolute()
        .top(offset(BEACON_DOT))
        .left(offset(BEACON_DOT))
        .size(px(BEACON_DOT))
        .rounded_full()
        .bg(color);
    let mut b = div().relative().flex_none().size(px(BEACON_BOX));
    if let Some(period) = state.period() {
        b = b.child(motion::looping(period, |t| {
            let (size, opacity) = ring_at(t);
            div()
                .absolute()
                .rounded_full()
                .border_1()
                .top(offset(size))
                .left(offset(size))
                .size(px(size))
                .border_color(color.opacity(opacity))
        }));
    }
    b.child(dot).into_any_element()
}

/// A faint ring with an arc going round it. Still while `moving` is off.
pub(super) fn spinner(moving: bool, size: f32, color: Hsla) -> AnyElement {
    let arc = icon("spinner-arc", size)
        .absolute()
        .top_0()
        .left_0()
        .text_color(color);
    let arc = if moving {
        motion::looping(SPIN, |t| {
            arc.with_transformation(Transformation::rotate(radians(t * std::f32::consts::TAU)))
        })
    } else {
        arc.into_any_element()
    };
    div()
        .relative()
        .flex_none()
        .size(px(size))
        .child(
            icon("circle", size)
                .absolute()
                .top_0()
                .left_0()
                .text_color(color)
                .opacity(0.25),
        )
        .child(arc)
        .into_any_element()
}

/// Text with a bright band sweeping across it, then a pause: `base` lit
/// up to `peak`, two text roles so the Grays setting holds. Plain, at
/// `base`, while `moving` is off.
pub(super) fn shimmer(moving: bool, text: &str, base: Hsla, peak: Hsla) -> AnyElement {
    let draw = |t| {
        StyledText::new(SharedString::from(text.to_string()))
            .with_runs(shimmer_runs(text, t, base, peak))
    };
    if moving {
        motion::looping(SHIMMER, draw)
    } else {
        draw(0.).into_any_element()
    }
}

/// The band's brightness at char `i` of `n`, `t` (0 to 1) into a sweep. The
/// band crosses in the first 70% of the sweep and is gone for the rest.
pub(super) fn shimmer_at(i: usize, n: usize, t: f32) -> f32 {
    const WIDTH: f32 = 4.;
    let travel = (t / 0.7).min(1.);
    let center = -WIDTH + motion::ease_in_out(travel) * (n as f32 + WIDTH * 2.);
    let k = (1. - (i as f32 - center).abs() / WIDTH).max(0.);
    k * k * (3. - 2. * k)
}

fn shimmer_runs(text: &str, t: f32, base: Hsla, peak: Hsla) -> Vec<TextRun> {
    let n = text.chars().count();
    let f = font(theme::FONT_SANS);
    text.chars()
        .enumerate()
        .map(|(i, c)| TextRun {
            len: c.len_utf8(),
            font: f.clone(),
            // Both roles are the content color at an alpha; the band lifts the alpha.
            color: Hsla {
                a: base.a + (peak.a - base.a) * shimmer_at(i, n, t),
                ..base
            },
            background_color: None,
            underline: None,
            strikethrough: None,
        })
        .collect()
}

/// Breathes `el`'s opacity between 0.35 and 0.9: a thought still streaming.
/// Held at the top of the breath while `moving` is off.
pub(super) fn pulse(moving: bool, el: Div) -> AnyElement {
    if !moving {
        return el.opacity(0.9).into_any_element();
    }
    motion::looping(PULSE, |t| {
        el.opacity(0.625 + 0.275 * (t * std::f32::consts::TAU).cos())
    })
}

/// A chevron that springs between pointing right (closed) and down (open).
/// A new one starts where it points, so only a toggle moves it.
pub(super) fn chevron(id: impl Into<ElementId>, open: bool, svg: Svg) -> impl IntoElement {
    svg.with_spring(
        id,
        gpui::SpringAnimation::new(motion::SNAP)
            .to(open)
            .with_epsilon(motion::SNAP_EPSILON),
        |s, phase| {
            s.with_transformation(Transformation::rotate(radians(
                phase.0 * std::f32::consts::FRAC_PI_2,
            )))
        },
    )
}

/// What a fold belongs to, so two kinds keyed by the same entry don't mix.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub(super) enum Fold {
    /// A settled turn's work, under its "Worked for" line.
    Work,
    /// A settled turn's changed files.
    Changes,
    /// A phase of the work trail.
    Phase,
    /// A row's details, or an Agent call's subagent steps.
    Row,
}

/// When entries arrived and folds opened, for the motion they get.
#[derive(Default)]
pub(super) struct Arrivals {
    /// Entries the view has seen; ones past this are new.
    seen: usize,
    entries: HashMap<usize, Instant>,
    folds: HashMap<(Fold, usize), Instant>,
}

impl Arrivals {
    pub(super) fn new(seen: usize) -> Self {
        Arrivals {
            seen,
            ..Default::default()
        }
    }

    /// The thread now has `len` entries. New ones get an entrance when the
    /// agent is at work and they came a few at a time; a load or a replay
    /// of history just shows up.
    pub(super) fn entries_now(&mut self, len: usize, at_work: bool, now: Instant) {
        if len < self.seen {
            self.entries.retain(|&ix, _| ix < len);
        } else if at_work && len - self.seen <= MAX_ARRIVALS {
            for ix in self.seen..len {
                self.entries.insert(ix, now);
            }
        }
        self.seen = len;
    }

    pub(super) fn reset(&mut self, len: usize) {
        *self = Arrivals::new(len);
    }

    pub(super) fn opened(&mut self, fold: Fold, key: usize, now: Instant) {
        self.folds.insert((fold, key), now);
    }

    pub(super) fn closed(&mut self, fold: Fold, key: usize) {
        self.folds.remove(&(fold, key));
    }

    /// How far entry `ix` is into its entrance, if it is still making one.
    fn entry_t(&mut self, ix: usize, now: Instant) -> Option<f32> {
        progress(&mut self.entries, ix, motion::BASE, now)
    }

    fn fold_t(&mut self, fold: Fold, key: usize, now: Instant) -> Option<f32> {
        progress(&mut self.folds, (fold, key), motion::BASE, now)
    }
}

/// `start`'s progress through `duration`, forgetting it once done.
fn progress<K: std::hash::Hash + Eq>(
    map: &mut HashMap<K, Instant>,
    key: K,
    duration: Duration,
    now: Instant,
) -> Option<f32> {
    let start = *map.get(&key)?;
    let t = now.saturating_duration_since(start).as_secs_f32() / duration.as_secs_f32();
    if t >= 1. {
        map.remove(&key);
        return None;
    }
    Some(t)
}

impl ThreadView {
    /// `el` making its entrance if entry `ix` just arrived: it fades in and
    /// rises into place. It moves by offset, so the list's measure holds.
    pub(super) fn arrive(&mut self, ix: usize, el: AnyElement, cx: &App) -> AnyElement {
        let t = self.entrances.entry_t(ix, Instant::now());
        self.moving(t, motion::From::Below, RISE, el, cx)
    }

    /// `el`, the contents of a fold, dropping into place if it just opened.
    pub(super) fn unfold(
        &mut self,
        fold: Fold,
        key: usize,
        el: AnyElement,
        cx: &App,
    ) -> AnyElement {
        let t = self.entrances.fold_t(fold, key, Instant::now());
        self.moving(t, motion::From::Above, DROP, el, cx)
    }

    pub(super) fn opened(&mut self, fold: Fold, key: usize, open: bool) {
        if open {
            self.entrances.opened(fold, key, Instant::now());
        } else {
            self.entrances.closed(fold, key);
        }
    }

    fn moving(
        &mut self,
        t: Option<f32>,
        from: motion::From,
        distance: f32,
        el: AnyElement,
        cx: &App,
    ) -> AnyElement {
        let Some(t) = t.filter(|_| !cx.reduce_motion()) else {
            return el;
        };
        self.fading = true;
        let (opacity, offset) = motion::enter_at(t, from, distance);
        div()
            .relative()
            .top(px(offset))
            .opacity(opacity)
            .min_w_0()
            .child(el)
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_ring_swells_and_fades_then_rests() {
        assert_eq!(ring_at(0.), (BEACON_DOT, 0.6));
        let (size, opacity) = ring_at(0.75);
        assert!((size - BEACON_BOX).abs() < 1e-4);
        assert!(opacity.abs() < 1e-6);
        assert_eq!(ring_at(0.75), ring_at(1.));
        // It only grows, and only fades.
        let beats: Vec<_> = (0..=20).map(|i| ring_at(i as f32 / 20.)).collect();
        assert!(
            beats
                .windows(2)
                .all(|w| w[1].0 >= w[0].0 && w[1].1 <= w[0].1)
        );
    }

    #[test]
    fn the_shimmer_band_crosses_then_pauses() {
        let n = 10;
        // Off the text at both ends of its travel, and through the pause.
        for t in [0., 0.7, 0.85, 1.] {
            assert!((0..n).all(|i| shimmer_at(i, n, t) == 0.), "t {t}");
        }
        // Mid-sweep, the middle of the text is lit.
        assert!(shimmer_at(5, n, 0.35) > 0.9);
    }

    #[test]
    fn entries_arriving_while_at_work_make_an_entrance() {
        let t0 = Instant::now();
        let mut a = Arrivals::new(3);
        a.entries_now(5, true, t0);
        assert_eq!(a.entry_t(2, t0), None);
        assert_eq!(a.entry_t(3, t0), Some(0.));
        let half = t0 + motion::BASE / 2;
        assert!((a.entry_t(4, half).unwrap() - 0.5).abs() < 1e-3);
        // Done is forgotten.
        assert_eq!(a.entry_t(4, t0 + motion::BASE), None);
        assert!(!a.entries.contains_key(&4));
    }

    #[test]
    fn a_load_or_a_replay_just_shows_up() {
        let t0 = Instant::now();
        let mut a = Arrivals::new(0);
        a.entries_now(40, true, t0);
        assert!(a.entries.is_empty());
        a.entries_now(41, false, t0);
        assert!(a.entries.is_empty());
        // A rewind drops what it cut; what comes after it arrives again.
        a.entries_now(42, true, t0);
        a.entries_now(30, true, t0);
        assert!(a.entries.is_empty());
        a.entries_now(31, true, t0);
        assert_eq!(a.entry_t(30, t0), Some(0.));
    }

    #[test]
    fn folds_of_different_kinds_keep_apart() {
        let t0 = Instant::now();
        let mut a = Arrivals::default();
        a.opened(Fold::Phase, 7, t0);
        assert_eq!(a.fold_t(Fold::Row, 7, t0), None);
        assert_eq!(a.fold_t(Fold::Phase, 7, t0), Some(0.));
        a.closed(Fold::Phase, 7);
        assert_eq!(a.fold_t(Fold::Phase, 7, t0), None);
    }
}
