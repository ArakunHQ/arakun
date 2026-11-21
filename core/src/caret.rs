//! The typing caret of the composer and the text fields: it glides to where it
//! goes instead of jumping, and fades out and back in while you don't type.
//!
//! The caret is driven from paint. Each frame it is drawn, it asks for the next
//! frame only while it moves or fades, and sets a timer for the still parts in
//! between, so an idle caret costs nothing.

use std::time::{Duration, Instant};

use gpui::{App, Hsla, Pixels, Point, Window, point, px};

/// The caret stays whole this long after input before it starts to blink.
const SOLID: Duration = Duration::from_millis(500);
/// One blink: the caret fades out and back in.
const PERIOD: Duration = Duration::from_millis(1000);
/// Blinks before the caret stays still, as GTK's stops after ~10 s idle.
const BLINKS: u32 = 10;
/// How long the caret takes to glide to a new place. Short enough that it
/// never trails your typing by more than a letter.
const GLIDE: Duration = Duration::from_millis(90);

/// How much of the caret shows after `idle` without input: whole while held,
/// then each half of a blink holds its end for a fifth and eases across the
/// rest, so it rests a moment both lit and dark.
fn alpha(idle: Duration) -> f32 {
    let Some(t) = idle.checked_sub(SOLID) else {
        return 1.;
    };
    if t >= PERIOD * BLINKS {
        return 1.;
    }
    let p = (t.as_secs_f32() / PERIOD.as_secs_f32()).fract();
    let half = if p < 0.5 { p * 2. } else { 2. - p * 2. };
    let k = ((half - 0.2) / 0.6).clamp(0., 1.);
    1. - k * k * (3. - 2. * k)
}

/// How long until `alpha` changes again: zero while it fades, the rest of the
/// hold while it rests, `None` once the caret stays still.
fn next_change(idle: Duration) -> Option<Duration> {
    let Some(t) = idle.checked_sub(SOLID) else {
        return Some(SOLID - idle);
    };
    if t >= PERIOD * BLINKS {
        return None;
    }
    let p = (t.as_secs_f32() / PERIOD.as_secs_f32()).fract();
    // The fades run over 0.1..0.4 and 0.6..0.9 of a blink.
    let wait = match p {
        p if p < 0.1 => 0.1 - p,
        p if p < 0.4 => 0.,
        p if p < 0.6 => 0.6 - p,
        p if p < 0.9 => 0.,
        p => 1.1 - p,
    };
    Some(PERIOD.mul_f32(wait))
}

/// Ease-out cubic: fast off the mark, settling into place.
fn ease(t: f32) -> f32 {
    1. - (1. - t.clamp(0., 1.)).powi(3)
}

pub struct Caret {
    /// The last input; the caret stays whole for `SOLID` after it.
    held: Instant,
    /// The glide: from where, to where, and when it set off. Points are
    /// relative to the text's origin, so scrolling moves the caret at once.
    from: Point<Pixels>,
    to: Point<Pixels>,
    moved: Instant,
    /// Whether the caret was drawn in the last frame; one that reappears
    /// (focus, the end of a selection) starts in place rather than gliding.
    shown: bool,
    /// Whether the window was active in the last frame.
    active: bool,
    /// When a pending timer will repaint, so repaints don't pile up timers.
    wake: Option<Instant>,
}

impl Default for Caret {
    fn default() -> Self {
        let now = Instant::now();
        Self {
            held: now,
            from: Point::default(),
            to: Point::default(),
            moved: now,
            shown: false,
            active: false,
            wake: None,
        }
    }
}

impl Caret {
    /// Input shows the caret whole until the next blink.
    pub fn hold(&mut self) {
        self.held = Instant::now();
    }

    /// The caret isn't drawn this frame: unfocused, or text is selected.
    pub fn hide(&mut self) {
        self.shown = false;
    }

    /// Where to draw the caret that belongs at `to` (relative to the text's
    /// origin) and how opaque, and schedules the frame it next changes in.
    pub fn frame(
        &mut self,
        to: Point<Pixels>,
        window: &mut Window,
        cx: &App,
    ) -> (Point<Pixels>, f32) {
        let now = Instant::now();
        let active = window.is_window_active();
        if !self.shown || (active && !self.active) {
            self.hold();
        }
        if !self.shown {
            (self.from, self.to) = (to, to);
        } else if to != self.to {
            // Set off from where it is drawn now, so a glide that is cut short
            // by the next key bends towards the new place instead of jumping.
            self.from = self.at(now);
            self.to = to;
            self.moved = now;
            self.hold();
        }
        self.shown = true;
        self.active = active;

        let at = self.at(now);
        if at != self.to {
            window.request_animation_frame();
            return (at, 1.);
        }
        // A window in the background keeps a still caret.
        if !active {
            return (at, 1.);
        }
        let idle = now - self.held;
        match next_change(idle) {
            Some(Duration::ZERO) => window.request_animation_frame(),
            Some(wait) => {
                let wake = now + wait;
                if self.wake.is_none_or(|w| w <= now || w > wake) {
                    self.wake = Some(wake);
                    let view = window.current_view();
                    window
                        .spawn(cx, async move |cx| {
                            cx.background_executor().timer(wait).await;
                            let _ = cx.update(|_, cx| cx.notify(view));
                        })
                        .detach();
                }
            }
            None => {}
        }
        (at, alpha(idle))
    }

    fn at(&self, now: Instant) -> Point<Pixels> {
        let t = ease((now - self.moved).as_secs_f32() / GLIDE.as_secs_f32());
        if t >= 1. {
            return self.to;
        }
        let lerp = |a: Pixels, b: Pixels| a + (b - a) * t;
        point(lerp(self.from.x, self.to.x), lerp(self.from.y, self.to.y))
    }
}

/// `color` faded to the caret's `alpha`.
pub fn faded(color: Hsla, alpha: f32) -> Hsla {
    Hsla {
        a: color.a * alpha,
        ..color
    }
}

/// The caret's corners: round enough to soften a thin bar, never a pill.
pub fn radius(width: f32) -> Pixels {
    px(width / 2.)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn caret_fades_after_a_pause() {
        let at = |ms: u64| alpha(Duration::from_millis(ms));
        assert_eq!(at(0), 1.);
        assert_eq!(at(499), 1.);
        // Each half holds its end for a fifth: lit, then dark at the middle.
        assert_eq!(at(550), 1.);
        assert!(at(700) < 1. && at(700) > 0.);
        assert_eq!(at(1000), 0.);
        assert_eq!(at(1050), 0.);
        assert!((at(750) - at(1250)).abs() < 1e-4, "comes back as it went");
        assert_eq!(at(1500), 1.);
        // Still after the last blink.
        assert_eq!(at(10_500), 1.);
        assert_eq!(at(60_000), 1.);
    }

    #[test]
    fn caret_wakes_only_when_it_changes() {
        let wait = |ms: u64| next_change(Duration::from_millis(ms)).map(|d| d.as_millis());
        assert_eq!(wait(0), Some(500));
        assert_eq!(wait(500), Some(100));
        assert_eq!(wait(700), Some(0));
        assert_eq!(wait(1000), Some(100));
        assert_eq!(wait(1300), Some(0));
        assert_eq!(wait(1450), Some(150));
        // The last blink ends lit; nothing more to draw.
        assert_eq!(wait(10_500), None);
        // Every wait lands where the caret starts to change.
        for ms in (0..10_400).step_by(37) {
            let w = next_change(Duration::from_millis(ms)).unwrap();
            if w > Duration::ZERO {
                let before = alpha(Duration::from_millis(ms));
                let end = Duration::from_millis(ms) + w - Duration::from_millis(1);
                assert_eq!(alpha(end), before, "still until {ms} + {w:?}");
            }
        }
    }

    #[test]
    fn glide_eases_into_place() {
        assert_eq!(ease(0.), 0.);
        assert_eq!(ease(1.), 1.);
        assert!(ease(0.5) > 0.5, "most of the way by halfway");
    }
}
