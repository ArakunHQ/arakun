//! How the transcript moves: the live beacon, spinners and shimmer that loop
//! while the agent works, and the way entries and folds arrive.
//!
//! Loops run on the app's motion clock (`motion::looping`), which redraws at
//! `motion::LOOP_FPS`, or at its full rate on ticks that draw a frame anyway,
//! rather than at the display's rate. It keeps loops side by side in step,
//! and holds them still when the system reduces motion.
//! Arrivals are keyed by entry, not by element: the list drops an element's
//! state when it scrolls away, and an old reply must not replay its entrance.
//! Every arrival lasts `motion::FAST` and only the newest entries make one,
//! so a busy turn costs a few frames per block, not a frame per display tick.

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

/// A fold on its way open or shut: when it started, and which way.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Toggle {
    start: Instant,
    open: bool,
}

/// When entries arrived and folds opened, for the motion they get.
#[derive(Default)]
pub(super) struct Arrivals {
    /// Entries the view has seen; ones past this are new.
    seen: usize,
    entries: HashMap<usize, Instant>,
    folds: HashMap<(Fold, usize), Toggle>,
    /// The full height of each moving fold's contents, as last laid out,
    /// for the height it grows to or shrinks from.
    heights: Rc<RefCell<HashMap<(Fold, usize), Pixels>>>,
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
    /// of history just shows up. True when one does.
    pub(super) fn entries_now(&mut self, len: usize, at_work: bool, now: Instant) -> bool {
        let mut arrived = false;
        if len < self.seen {
            self.entries.retain(|&ix, _| ix < len);
        } else if at_work && len > self.seen && len - self.seen <= MAX_ARRIVALS {
            for ix in self.seen..len {
                self.entries.insert(ix, now);
            }
            arrived = true;
        }
        self.seen = len;
        arrived
    }

    pub(super) fn reset(&mut self, len: usize) {
        *self = Arrivals::new(len);
    }

    /// Fold `key` starts to open, or to shut. Turned back midway, it starts
    /// from the height it shows, so it doesn't jump.
    pub(super) fn toggled(&mut self, fold: Fold, key: usize, open: bool, now: Instant) {
        let start = match self.folds.get(&(fold, key)) {
            Some(t) if t.open != open && since(t.start, now) < 1. => {
                let shown = fold_share(since(t.start, now), t.open);
                now - motion::FAST.mul_f32(fold_t_at(shown, open))
            }
            _ => now,
        };
        self.folds.insert((fold, key), Toggle { start, open });
    }

    /// Whether fold `key` is still shutting, so its contents stay drawn.
    pub(super) fn shutting(&self, fold: Fold, key: usize, now: Instant) -> bool {
        self.folds
            .get(&(fold, key))
            .is_some_and(|t| !t.open && now.saturating_duration_since(t.start) < motion::FAST)
    }

    /// How far entry `ix` is into its entrance, if it is still making one.
    fn entry_t(&mut self, ix: usize, now: Instant) -> Option<f32> {
        let start = *self.entries.get(&ix)?;
        let t = since(start, now);
        if t >= 1. {
            self.entries.remove(&ix);
            return None;
        }
        Some(t)
    }

    /// How far fold `key` is into opening or shutting, and which.
    fn fold_t(&mut self, fold: Fold, key: usize, now: Instant) -> Option<(f32, bool)> {
        let toggle = *self.folds.get(&(fold, key))?;
        let t = since(toggle.start, now);
        if t >= 1. {
            self.folds.remove(&(fold, key));
            self.heights.borrow_mut().remove(&(fold, key));
            return None;
        }
        Some((t, toggle.open))
    }
}

/// How far `FAST` has run since `start`, from 0.
fn since(start: Instant, now: Instant) -> f32 {
    now.saturating_duration_since(start).as_secs_f32() / motion::FAST.as_secs_f32()
}

/// How much of a fold's full height shows `t` into its toggle, 0 to 1.
pub(super) fn fold_share(t: f32, open: bool) -> f32 {
    let e = motion::ease_out(t.clamp(0., 1.));
    if open { e } else { 1. - e }
}

/// The point in a toggle where `shown` of the fold's height shows: the
/// inverse of `fold_share`.
fn fold_t_at(shown: f32, open: bool) -> f32 {
    let shown = shown.clamp(0., 1.);
    let eased = if open { shown } else { 1. - shown };
    // `ease_out` is 1 - (1 - t)^4.
    1. - (1. - eased).powf(0.25)
}

/// Where the transcript is `t` into a glide from `from` to `to`, in pixels
/// scrolled. It eases out, so it moves off quickly and settles at the end.
pub(super) fn glide_at(from: f32, to: f32, t: f32) -> f32 {
    from + (to - from) * motion::ease_out(t.clamp(0., 1.))
}

/// The transcript easing down to its new end after an entry arrived.
#[derive(Clone, Copy, Debug)]
pub(super) struct Glide {
    start: Instant,
    /// Pixels scrolled when it began.
    from: f32,
}

impl ThreadView {
    /// `el` making its entrance if entry `ix` just arrived: it fades in and
    /// rises into place. It moves by offset, so the list's measure holds.
    pub(super) fn arrive(&mut self, ix: usize, el: AnyElement, cx: &App) -> AnyElement {
        let t = self.entrances.entry_t(ix, Instant::now());
        self.moving(t, motion::From::Below, RISE, el, cx)
    }

    /// `el`, the contents of a fold. While it opens it grows from nothing
    /// to its full height and drops into place; while it shuts it shrinks
    /// away and fades. The rows under it follow the height, not jump.
    pub(super) fn unfold(
        &mut self,
        fold: Fold,
        key: usize,
        el: AnyElement,
        cx: &App,
    ) -> AnyElement {
        let Some((t, open)) = self
            .entrances
            .fold_t(fold, key, Instant::now())
            .filter(|_| !cx.reduce_motion())
        else {
            return el;
        };
        self.fading = true;
        let el = if open {
            self.moving(Some(t), motion::From::Above, DROP, el, cx)
        } else {
            div()
                .min_w_0()
                .opacity(1. - motion::ease_out(t))
                .child(el)
                .into_any_element()
        };
        let heights = self.entrances.heights.clone();
        let full = heights.borrow().get(&(fold, key)).copied();
        // The contents keep their own height inside a clip; the first frame
        // only measures it. Opening shows none of it yet, shutting all of it.
        let shown = match full {
            Some(full) => Some(full * fold_share(t, open)),
            None if open => Some(px(0.)),
            None => None,
        };
        div()
            .flex()
            .flex_col()
            .min_w_0()
            .overflow_hidden()
            .when_some(shown, |d, h| d.h(h))
            .on_children_prepainted(move |bounds, _, _| {
                if let Some(b) = bounds.first() {
                    heights.borrow_mut().insert((fold, key), b.size.height);
                }
            })
            .child(div().flex_none().min_w_0().child(el))
            .into_any_element()
    }

    pub(super) fn opened(&mut self, fold: Fold, key: usize, open: bool) {
        // A turn's work shuts by rebuilding the list's rows around it, so it
        // can't draw on its way out: it snaps shut, with nothing to turn back from.
        if fold == Fold::Work && !open {
            self.entrances.folds.remove(&(fold, key));
            return;
        }
        self.entrances.toggled(fold, key, open, Instant::now());
    }

    /// Whether a fold just closed and still draws its contents on the way.
    pub(super) fn shutting(&self, fold: Fold, key: usize, cx: &App) -> bool {
        !cx.reduce_motion() && self.entrances.shutting(fold, key, Instant::now())
    }

    /// Starts easing the transcript down to its new end, if it follows the
    /// end and motion is on. Streamed text still follows at once; this is
    /// for a new block.
    pub(super) fn start_glide(&mut self, cx: &App) {
        if cx.reduce_motion() || !(self.following && self.list.is_following_tail()) {
            return;
        }
        let from = f32::from(-self.list.scroll_px_offset_for_scrollbar().y);
        // Hold the list where it is; `step_glide` moves it from here.
        self.list.pause_following_tail();
        self.glide = Some(Glide {
            start: Instant::now(),
            from,
        });
    }

    /// One frame of the glide: scroll toward the end as it stands now, which
    /// moves while text streams, then follow the end again.
    pub(super) fn step_glide(&mut self, window: &Window) {
        let Some(g) = self.glide else {
            return;
        };
        let t = since(g.start, Instant::now());
        let to = f32::from(self.list.max_offset_for_scrollbar().y);
        let at = f32::from(-self.list.scroll_px_offset_for_scrollbar().y);
        if t >= 1. || !self.following || to <= g.from {
            self.end_glide();
            return;
        }
        let want = glide_at(g.from, to, t);
        if want > at {
            self.list.scroll_by(px(want - at));
        }
        motion::redraw_on_tick(window);
    }

    /// Stops the glide and follows the end again.
    pub(super) fn end_glide(&mut self) {
        if self.glide.take().is_some() && self.following {
            self.list.set_follow_mode(FollowMode::Tail);
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
        assert!(a.entries_now(5, true, t0));
        assert_eq!(a.entry_t(2, t0), None);
        assert_eq!(a.entry_t(3, t0), Some(0.));
        let half = t0 + motion::FAST / 2;
        assert!((a.entry_t(4, half).unwrap() - 0.5).abs() < 1e-3);
        // Done is forgotten.
        assert_eq!(a.entry_t(4, t0 + motion::FAST), None);
        assert!(!a.entries.contains_key(&4));
    }

    #[test]
    fn a_load_or_a_replay_just_shows_up() {
        let t0 = Instant::now();
        let mut a = Arrivals::new(0);
        assert!(!a.entries_now(40, true, t0));
        assert!(a.entries.is_empty());
        assert!(!a.entries_now(41, false, t0));
        assert!(a.entries.is_empty());
        // Nothing new, nothing to glide to.
        assert!(!a.entries_now(41, true, t0));
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
        a.toggled(Fold::Phase, 7, true, t0);
        assert_eq!(a.fold_t(Fold::Row, 7, t0), None);
        assert_eq!(a.fold_t(Fold::Phase, 7, t0), Some((0., true)));
        assert!(!a.shutting(Fold::Phase, 7, t0));
    }

    #[test]
    fn a_shutting_fold_draws_until_it_is_shut() {
        let t0 = Instant::now();
        let mut a = Arrivals::default();
        a.toggled(Fold::Row, 3, false, t0);
        assert!(a.shutting(Fold::Row, 3, t0));
        assert_eq!(a.fold_t(Fold::Row, 3, t0), Some((0., false)));
        let end = t0 + motion::FAST;
        assert!(!a.shutting(Fold::Row, 3, end));
        assert_eq!(a.fold_t(Fold::Row, 3, end), None);
        assert!(a.folds.is_empty());
    }

    #[test]
    fn a_fold_turned_back_midway_starts_from_where_it_shows() {
        let t0 = Instant::now();
        let mut a = Arrivals::default();
        a.toggled(Fold::Row, 3, true, t0);
        let quarter = t0 + motion::FAST / 4;
        a.toggled(Fold::Row, 3, false, quarter);
        let (t, open) = a.fold_t(Fold::Row, 3, quarter).unwrap();
        assert!(!open);
        let before = fold_share(0.25, true);
        assert!((fold_share(t, false) - before).abs() < 1e-3, "{t}");
    }

    #[test]
    fn a_fold_grows_to_its_height_and_shrinks_away() {
        assert_eq!(fold_share(0., true), 0.);
        assert_eq!(fold_share(1., true), 1.);
        assert_eq!(fold_share(0., false), 1.);
        assert_eq!(fold_share(1., false), 0.);
        let opening: Vec<_> = (0..=10).map(|i| fold_share(i as f32 / 10., true)).collect();
        assert!(opening.windows(2).all(|w| w[1] >= w[0]));
    }

    #[test]
    fn the_glide_eases_to_the_end_and_stops_there() {
        assert_eq!(glide_at(100., 300., 0.), 100.);
        assert_eq!(glide_at(100., 300., 1.), 300.);
        assert_eq!(glide_at(100., 300., 2.), 300.);
        // Eased out: past halfway by half time.
        assert!(glide_at(100., 300., 0.5) > 200.);
    }
}
