//! How the interface moves: the durations and easings every surface shares,
//! so dialogs, menus and tooltips arrive the same way.
//!
//! gpui draws a oneshot animation at its end when the app reduces motion, and
//! `follow_system` sets that from the system's own setting, so nothing here
//! needs its own check.

use std::time::Duration;

use gpui::{
    Animation, AnimationElement, AnimationExt as _, App, ElementId, IntoElement, Styled, px,
};

/// Tooltips and small menus: there before the hand has stopped.
pub const FAST: Duration = Duration::from_millis(140);
/// Dialogs and the panels they open.
pub const BASE: Duration = Duration::from_millis(220);
/// Things that travel: a row sliding to its new place.
pub const SLOW: Duration = Duration::from_millis(280);

/// Starts fast and lands softly. For anything arriving or moving.
pub fn ease_out(t: f32) -> f32 {
    1. - (1. - t).powi(4)
}

/// Like `ease_out`, but goes a hair past the end and settles back, so a
/// surface lands with a little give instead of stopping dead.
pub fn ease_out_back(t: f32) -> f32 {
    const C1: f32 = 1.1;
    const C3: f32 = C1 + 1.;
    let u = t - 1.;
    1. + C3 * u * u * u + C1 * u * u
}

/// A quick spring that overshoots a touch: a switch's knob snapping over.
pub const SNAP: gpui::SpringConfig = gpui::SpringConfig::new(700., 36., 1.);
/// Close enough to stop `SNAP` on a 0 to 1 phase, and stop drawing frames.
pub const SNAP_EPSILON: f32 = 0.008;

/// Where a surface comes from as it enters.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum From {
    /// Drops in from above: menus that open below their anchor.
    Above,
    /// Rises from below: dialogs, and menus that open above their anchor.
    Below,
}

/// Where `enter` has a surface `t` (0 to 1) into its way in: its opacity, and
/// how far it still is from its place, in pixels (negative is above).
pub fn enter_at(t: f32, from: From, distance: f32) -> (f32, f32) {
    // The fade finishes early: a surface that is still see-through while
    // already in place reads as slow.
    let opacity = ease_out((t * 1.6).min(1.));
    let rest = distance * (1. - ease_out_back(t));
    let offset = match from {
        From::Above => -rest,
        From::Below => rest,
    };
    (opacity, offset)
}

/// Fades `el` in while it slides `distance` px into place. Moves it with its
/// top margin, so it works on absolutely placed menus too.
pub fn enter<E: IntoElement + Styled + 'static>(
    el: E,
    id: impl Into<ElementId>,
    from: From,
    distance: f32,
    duration: Duration,
) -> AnimationElement<E> {
    el.with_animation(id, Animation::new(duration), move |el, t| {
        let (opacity, offset) = enter_at(t, from, distance);
        el.opacity(opacity).mt(px(offset))
    })
}

/// Reduces motion when the system asks for it (macOS's Reduce motion,
/// GNOME's Animations switch). Read once, at start.
pub fn follow_system(cx: &mut App) {
    cx.spawn(async move |cx| {
        let reduce = cx
            .background_executor()
            .spawn(async { system_reduces_motion() })
            .await;
        if reduce {
            cx.update(|cx| cx.set_reduce_motion(true));
        }
    })
    .detach();
}

fn system_reduces_motion() -> bool {
    #[cfg(target_os = "macos")]
    {
        read(
            "defaults",
            &["read", "com.apple.universalaccess", "reduceMotion"],
        )
        .is_some_and(|out| out == "1")
    }
    #[cfg(target_os = "linux")]
    {
        read(
            "gsettings",
            &["get", "org.gnome.desktop.interface", "enable-animations"],
        )
        .is_some_and(|out| out == "false")
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        false
    }
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn read(program: &str, args: &[&str]) -> Option<String> {
    let out = std::process::Command::new(program)
        .args(args)
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output()
        .ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_snap_spring_gives_a_little_but_settles_fast() {
        let (_, damping) = SNAP.canonical();
        assert!(damping > 0.55 && damping < 0.85, "damping ratio {damping}");
        // Within a tenth of a pixel of a switch's 12 px of travel.
        let settle = SNAP.settle_time(gpui::SpringState::default(), 1., SNAP_EPSILON);
        assert!(settle < Duration::from_millis(320), "{settle:?}");
    }

    #[test]
    fn easings_start_at_zero_and_end_at_one() {
        for f in [ease_out, ease_out_back] {
            assert!(f(0.).abs() < 1e-5);
            assert!((f(1.) - 1.).abs() < 1e-5);
        }
    }

    #[test]
    fn the_back_easing_overshoots_only_a_little() {
        let peak = (0..=100)
            .map(|i| ease_out_back(i as f32 / 100.))
            .fold(0., f32::max);
        assert!(peak > 1. && peak < 1.05, "peak {peak}");
    }

    #[test]
    fn a_surface_enters_from_its_side_and_lands_in_place() {
        let (o, y) = enter_at(0., From::Below, 8.);
        assert_eq!(o, 0.);
        assert!((y - 8.).abs() < 1e-4);
        let (_, y) = enter_at(0., From::Above, 8.);
        assert!((y + 8.).abs() < 1e-4);
        let (o, y) = enter_at(1., From::Below, 8.);
        assert_eq!(o, 1.);
        assert!(y.abs() < 1e-4);
        // Fully opaque well before it stops moving.
        assert_eq!(enter_at(0.7, From::Below, 8.).0, 1.);
    }
}
