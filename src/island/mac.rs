//! The island's window on macOS, which has no layer shell: a borderless panel
//! above the menu bar, at the top middle of the screen with the camera cutout
//! (else the main screen). AppKit sends a window every click on it, even on
//! its transparent parts, so the window ignores the pointer until the pointer
//! is on the island: `Island::track_pointer` asks where it is and flips that.
//!
//! Every call here is made on the main thread, from gpui's callbacks.

use std::cell::Cell;
use std::ffi::{CStr, c_char, c_void};

use super::Edge;

type Id = *mut c_void;

#[repr(C)]
#[derive(Clone, Copy, Default, PartialEq, Debug)]
struct Point {
    x: f64,
    y: f64,
}

/// An `NSRect`: its origin is the bottom left corner, and y grows upward.
#[repr(C)]
#[derive(Clone, Copy, Default, PartialEq, Debug)]
struct Rect {
    x: f64,
    y: f64,
    w: f64,
    h: f64,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct Insets {
    top: f64,
    left: f64,
    bottom: f64,
    right: f64,
}

#[link(name = "objc")]
unsafe extern "C" {
    fn objc_getClass(name: *const c_char) -> Id;
    fn sel_registerName(name: *const c_char) -> Id;
    fn objc_msgSend();
    // Intel returns a struct this big through a pointer, with a call of its own.
    #[cfg(target_arch = "x86_64")]
    fn objc_msgSend_stret();
}

/// `objc_msgSend` as a function with the signature of the method it sends.
macro_rules! send {
    (fn($($arg:ty),*) -> $ret:ty) => {
        std::mem::transmute::<*const c_void, extern "C" fn(Id, Id $(, $arg)*) -> $ret>(
            objc_msgSend as *const c_void,
        )
    };
}

/// The same for a method that returns a struct of four doubles.
macro_rules! send_big {
    ($ret:ty) => {{
        #[cfg(target_arch = "x86_64")]
        let f = objc_msgSend_stret as *const c_void;
        #[cfg(not(target_arch = "x86_64"))]
        let f = objc_msgSend as *const c_void;
        std::mem::transmute::<*const c_void, extern "C" fn(Id, Id) -> $ret>(f)
    }};
}

fn sel(name: &CStr) -> Id {
    // SAFETY: a NUL-terminated name.
    unsafe { sel_registerName(name.as_ptr()) }
}

fn class(name: &CStr) -> Id {
    // SAFETY: a NUL-terminated name.
    unsafe { objc_getClass(name.as_ptr()) }
}

/// SAFETY: `obj` is a live Objective-C object.
unsafe fn responds(obj: Id, name: &CStr) -> bool {
    unsafe { send!(fn(Id) -> bool)(obj, sel(c"respondsToSelector:"), sel(name)) }
}

/// Where the island goes: a screen's frame and what its top edge is like.
#[derive(Clone, Copy, PartialEq, Debug)]
struct Spot {
    screen: Rect,
    edge: Edge,
}

#[derive(Clone, Copy)]
struct Native {
    window: Id,
    spot: Spot,
    size: (f64, f64),
}

thread_local! {
    /// The island's window, once `adopt` took it: one per process.
    static NATIVE: Cell<Option<Native>> = const { Cell::new(None) };
}

/// The screen with a camera cutout, else the main one, and its top edge.
fn spot() -> Option<Spot> {
    // SAFETY: AppKit calls on the main thread, each cast to its method's
    // signature; the selectors newer than the system are asked for first.
    unsafe {
        let screens = send!(fn() -> Id)(class(c"NSScreen"), sel(c"screens"));
        if screens.is_null() {
            return None;
        }
        let count = send!(fn() -> usize)(screens, sel(c"count"));
        let mut found: Option<(Id, f64)> = None;
        for i in 0..count {
            let screen = send!(fn(usize) -> Id)(screens, sel(c"objectAtIndex:"), i);
            let top = if responds(screen, c"safeAreaInsets") {
                send_big!(Insets)(screen, sel(c"safeAreaInsets")).top
            } else {
                0.
            };
            if top > 0. {
                found = Some((screen, top));
                break;
            }
            if i == 0 {
                found = Some((screen, 0.));
            }
        }
        let (screen, top) = found?;
        let frame = send_big!(Rect)(screen, sel(c"frame"));
        let edge = Edge::from_env().unwrap_or_else(|| {
            if top <= 0. {
                return Edge::Flat;
            }
            let side = |name: &CStr| {
                if responds(screen, name) {
                    send_big!(Rect)(screen, sel(name)).w
                } else {
                    0.
                }
            };
            // AppKit tells where the cutout is, not whether it is a notch or
            // a Dynamic Island. The MacBook Pro with the island is the one
            // whose own screen takes touch; `from_safe_area` weighs that
            // against the cutout's width.
            let touch = responds(screen, c"touchCapabilities")
                && send!(fn() -> usize)(screen, sel(c"touchCapabilities")) != 0;
            Edge::from_safe_area(
                top as f32,
                frame.w as f32,
                side(c"auxiliaryTopLeftArea") as f32,
                side(c"auxiliaryTopRightArea") as f32,
                touch,
            )
        });
        Some(Spot {
            screen: frame,
            edge,
        })
    }
}

/// The window's frame: `size` at the top middle of the spot's screen.
fn frame(spot: &Spot, size: (f64, f64)) -> Rect {
    Rect {
        x: (spot.screen.x + (spot.screen.w - size.0) / 2.).round(),
        y: spot.screen.y + spot.screen.h - size.1,
        w: size.0,
        h: size.1,
    }
}

/// Turns the window `ns_view` is in into the island's and shows it: no
/// frame or shadow, over the menu bar, on every Space, out of the window
/// cycle, and deaf to the pointer until `click_through(false)`.
pub fn adopt(ns_view: Id, size: (f32, f32)) -> Edge {
    const BORDERLESS: usize = 0;
    const NONACTIVATING_PANEL: usize = 1 << 7;
    const ALL_SPACES: usize = 1 << 0;
    const STATIONARY: usize = 1 << 4;
    const IGNORES_CYCLE: usize = 1 << 6;
    const FULL_SCREEN_AUXILIARY: usize = 1 << 8;

    let Some(spot) = spot() else {
        return Edge::Flat;
    };
    let size = (size.0 as f64, size.1 as f64);
    // SAFETY: `ns_view` is the live view of the window gpui just made, on the
    // main thread; each objc_msgSend is cast to its method's signature.
    unsafe {
        let window = send!(fn() -> Id)(ns_view, sel(c"window"));
        if window.is_null() {
            return Edge::Flat;
        }
        // A titled window is kept under the menu bar; a borderless one isn't.
        send!(fn(usize) -> ())(
            window,
            sel(c"setStyleMask:"),
            BORDERLESS | NONACTIVATING_PANEL,
        );
        send!(fn(bool) -> ())(window, sel(c"setHasShadow:"), false);
        send!(fn(usize) -> ())(
            window,
            sel(c"setCollectionBehavior:"),
            ALL_SPACES | STATIONARY | IGNORES_CYCLE | FULL_SCREEN_AUXILIARY,
        );
        send!(fn(bool) -> ())(window, sel(c"setIgnoresMouseEvents:"), true);
        send!(fn(Rect, bool) -> ())(window, sel(c"setFrame:display:"), frame(&spot, size), true);
        // The style change drops the first responder.
        send!(fn(Id) -> bool)(window, sel(c"makeFirstResponder:"), ns_view);
        send!(fn() -> ())(window, sel(c"orderFrontRegardless"));
        NATIVE.set(Some(Native { window, spot, size }));
    }
    spot.edge
}

/// The top edge the island was opened on, before `adopt` or off its thread `Flat`.
pub fn edge() -> Edge {
    NATIVE.get().map_or(Edge::Flat, |n| n.spot.edge)
}

/// Where the pointer is in the island's window, from its top left corner.
pub fn pointer() -> Option<(f32, f32)> {
    let native = NATIVE.get()?;
    // SAFETY: as in `adopt`; the window lives as long as the process shows the island.
    unsafe {
        let at = send!(fn() -> Point)(class(c"NSEvent"), sel(c"mouseLocation"));
        let frame = send_big!(Rect)(native.window, sel(c"frame"));
        Some(((at.x - frame.x) as f32, (frame.y + frame.h - at.y) as f32))
    }
}

/// Whether clicks go through the window to what is under it.
pub fn click_through(on: bool) {
    if let Some(native) = NATIVE.get() {
        // SAFETY: as in `pointer`.
        unsafe { send!(fn(bool) -> ())(native.window, sel(c"setIgnoresMouseEvents:"), on) };
    }
}

/// Moves the island when its screen changed (a lid closed, a monitor plugged
/// in, a new resolution); returns the top edge it is on now.
pub fn follow_screen() -> Option<Edge> {
    let mut native = NATIVE.get()?;
    let spot = spot()?;
    if spot != native.spot {
        native.spot = spot;
        NATIVE.set(Some(native));
        // SAFETY: as in `pointer`.
        unsafe {
            send!(fn(Rect, bool) -> ())(
                native.window,
                sel(c"setFrame:display:"),
                frame(&spot, native.size),
                true,
            );
        }
    }
    Some(spot.edge)
}

/// Takes this process out of the Dock and the app switcher: the island's host
/// is not an app the user switches to.
pub fn hide_from_dock() {
    const ACCESSORY: isize = 1;
    // SAFETY: as in `adopt`.
    unsafe {
        let app = send!(fn() -> Id)(class(c"NSApplication"), sel(c"sharedApplication"));
        if !app.is_null() {
            send!(fn(isize) -> bool)(app, sel(c"setActivationPolicy:"), ACCESSORY);
        }
    }
}

/// Brings the app with this process id to the front.
pub fn activate(pid: u32) {
    const ALL_WINDOWS: usize = 1 << 0;
    // SAFETY: as in `adopt`; a pid with no app gives nil, which is checked.
    unsafe {
        let app = send!(fn(i32) -> Id)(
            class(c"NSRunningApplication"),
            sel(c"runningApplicationWithProcessIdentifier:"),
            pid as i32,
        );
        // macOS may decline: say so, or the window just stays behind.
        if !app.is_null()
            && !send!(fn(usize) -> bool)(app, sel(c"activateWithOptions:"), ALL_WINDOWS)
        {
            eprintln!("arakun: island: macOS did not bring app {pid} to the front");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_window_sits_at_the_top_middle_of_its_screen() {
        // A screen left of the main one, its bottom below the main one's.
        let spot = Spot {
            screen: Rect {
                x: -1512.,
                y: -98.,
                w: 1512.,
                h: 982.,
            },
            edge: Edge::Flat,
        };
        let f = frame(&spot, (640., 380.));
        assert_eq!((f.w, f.h), (640., 380.));
        assert_eq!(f.x + f.w / 2., -756.);
        // Its top edge is the screen's.
        assert_eq!(f.y + f.h, 884.);
    }
}
