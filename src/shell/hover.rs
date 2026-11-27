//! Which row of a list is under the pointer, for what a row shows only
//! there: the archive button and the age in the session tree and the active
//! list.
//!
//! Not gpui's `on_hover`. On macOS it stopped firing on these rows while
//! their hover background still came and went (ARAKUN-290). The two listen
//! differently: the background reads the hit test in the capture phase of a
//! mouse move, while `on_hover` waits for the bubble phase and stays quiet
//! while the row holds a mouse-down or a drag is on. [`probe`] does what the
//! background does, so the row's controls show exactly when it is lit.

use std::cell::Cell;
use std::rc::Rc;

use gpui::{
    App, DispatchPhase, HitboxBehavior, IntoElement, MouseExitEvent, MouseMoveEvent, Styled as _,
    Window,
};

/// Fills its row (which must be `relative`) and calls `on_hover` with true
/// when the pointer comes onto the row, false when it leaves. `shown` is
/// whether the view treats the row as under the pointer now.
pub(super) fn probe(
    shown: bool,
    on_hover: impl Fn(&bool, &mut Window, &mut App) + 'static,
) -> impl IntoElement {
    let on_hover = Rc::new(on_hover);
    gpui::canvas(
        // Normal, so it takes nothing from the row or the controls on it.
        |bounds, window, _| window.insert_hitbox(bounds, HitboxBehavior::Normal),
        move |_, hitbox, window, cx| {
            let over = hitbox.is_hovered(window);
            // A row can slide under a still pointer. Only this way: a row
            // drawn twice (the tree in a menu and in the sidebar) would
            // otherwise take it back and forth each frame.
            if arrived(shown, over) {
                let on_hover = on_hover.clone();
                window.defer(cx, move |window, cx| on_hover(&true, window, cx));
            }
            let was = Rc::new(Cell::new(over));
            window.on_mouse_event({
                let (was, on_hover, hitbox) = (was.clone(), on_hover.clone(), hitbox.clone());
                move |_: &MouseMoveEvent, phase, window, cx| {
                    if phase != DispatchPhase::Capture {
                        return;
                    }
                    if let Some(now) = moved(&was, hitbox.is_hovered(window)) {
                        on_hover(&now, window, cx);
                    }
                }
            });
            // The pointer can leave the window with no last move.
            window.on_mouse_event(move |_: &MouseExitEvent, phase, window, cx| {
                if phase == DispatchPhase::Capture
                    && let Some(now) = moved(&was, false)
                {
                    on_hover(&now, window, cx);
                }
            });
        },
    )
    .absolute()
    .size_full()
}

/// The row is under the pointer as it is drawn, and the view doesn't know.
fn arrived(shown: bool, over: bool) -> bool {
    over && !shown
}

/// The new state when the pointer came onto the row or left it since `was`.
fn moved(was: &Cell<bool>, over: bool) -> Option<bool> {
    (was.replace(over) != over).then_some(over)
}

/// Keeps `slot`, the row under the pointer, as `row` reports the pointer
/// coming (`over`) or going. A row that leaves clears only itself: the next
/// one may have come first. `same` tells rows apart. True when `slot` changed
/// and the view needs drawing again.
pub(super) fn track<T>(
    slot: &mut Option<T>,
    row: T,
    over: bool,
    same: impl Fn(&T, &T) -> bool,
) -> bool {
    let here = slot.as_ref().is_some_and(|s| same(s, &row));
    if over {
        *slot = Some(row);
        !here
    } else if here {
        *slot = None;
        true
    } else {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn eq(a: &&str, b: &&str) -> bool {
        a == b
    }

    #[test]
    fn a_row_reports_the_pointer_coming_and_going_once() {
        let was = Cell::new(false);
        assert_eq!(moved(&was, true), Some(true));
        // More moves over the same row say nothing new.
        assert_eq!(moved(&was, true), None);
        assert_eq!(moved(&was, false), Some(false));
        assert_eq!(moved(&was, false), None);
    }

    #[test]
    fn a_row_drawn_under_a_still_pointer_reports_it() {
        assert!(arrived(false, true));
        assert!(!arrived(true, true));
        // Leaving is left to the next move, so two copies of a row can't
        // take the pointer from each other each frame.
        assert!(!arrived(true, false));
        assert!(!arrived(false, false));
    }

    #[test]
    fn the_hovered_row_follows_the_pointer_from_row_to_row() {
        let mut slot = None;
        assert!(track(&mut slot, "a", true, eq));
        assert_eq!(slot, Some("a"));
        // Already there: nothing to draw again.
        assert!(!track(&mut slot, "a", true, eq));
        // "b" hears the move before "a" does.
        assert!(track(&mut slot, "b", true, eq));
        assert!(!track(&mut slot, "a", false, eq));
        assert_eq!(slot, Some("b"));
        assert!(track(&mut slot, "b", false, eq));
        assert_eq!(slot, None);
    }

    #[test]
    fn rows_are_told_apart_by_what_same_compares() {
        // The tree keys rows by session id; the project and environment
        // indexes can move under it.
        let same = |a: &(usize, &str), b: &(usize, &str)| a.1 == b.1;
        let mut slot = Some((0, "a"));
        assert!(!track(&mut slot, (1, "a"), true, same));
        assert_eq!(slot, Some((1, "a")));
        assert!(track(&mut slot, (2, "a"), false, same));
        assert_eq!(slot, None);
    }
}
