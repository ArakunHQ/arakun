//! How the interface moves: the durations and easings every surface shares,
//! so dialogs, menus and tooltips arrive the same way.
//!
//! gpui draws a oneshot animation at its end when the app reduces motion, and
//! `apply` sets that from the user's choice and the system's own setting, so
//! nothing here needs its own check. Drawings that run their own timers check
//! `cx.reduce_motion()`.

use std::cell::{Cell, RefCell};
use std::future::{Future, poll_fn};
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::task::{Poll, Waker};
use std::time::{Duration, Instant};

use gpui::{
    Animation, AnimationElement, AnimationExt as _, AnyElement, App, Bounds, Element, ElementId,
    EntityId, GlobalElementId, InspectorElementId, IntoElement, LayoutId, Pixels, Styled, Window,
    px,
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

/// Slow at both ends. For something that crosses and leaves again, like a
/// shimmer's band.
pub fn ease_in_out(t: f32) -> f32 {
    if t < 0.5 {
        4. * t * t * t
    } else {
        1. - (-2. * t + 2.).powi(3) / 2.
    }
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

/// Fades `el` in. For backdrops, which dim the app rather than travel.
pub fn fade_in<E: IntoElement + Styled + 'static>(
    el: E,
    id: impl Into<ElementId>,
    duration: Duration,
) -> AnimationElement<E> {
    el.with_animation(id, Animation::new(duration), |el, t| {
        el.opacity(ease_out(t))
    })
}

/// Fades `el` in while it slides `distance` px in from the right: sheets and
/// drawers held to the right edge. Moves it with its right margin, which
/// shifts an element placed by its right inset.
pub fn enter_from_right<E: IntoElement + Styled + 'static>(
    el: E,
    id: impl Into<ElementId>,
    distance: f32,
    duration: Duration,
) -> AnimationElement<E> {
    el.with_animation(id, Animation::new(duration), move |el, t| {
        let (opacity, offset) = enter_at(t, From::Below, distance);
        el.opacity(opacity).mr(px(-offset))
    })
}

/// Whether Arakun reduces motion: as the system says, or always, or never.
/// Settings › Appearance, `reduce_motion` in the state file.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Reduce {
    #[default]
    System,
    On,
    Off,
}

impl Reduce {
    pub const ALL: [Reduce; 3] = [Reduce::System, Reduce::On, Reduce::Off];

    /// How the state file names it; the default is empty, so files from
    /// before this setting read as System.
    pub fn id(self) -> &'static str {
        match self {
            Reduce::System => "",
            Reduce::On => "on",
            Reduce::Off => "off",
        }
    }

    pub fn from_id(id: &str) -> Self {
        match id {
            "on" => Reduce::On,
            "off" => Reduce::Off,
            _ => Reduce::System,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Reduce::System => "System",
            Reduce::On => "On",
            Reduce::Off => "Off",
        }
    }

    fn reduces(self, system: bool) -> bool {
        match self {
            Reduce::System => system,
            Reduce::On => true,
            Reduce::Off => false,
        }
    }
}

static CHOICE: AtomicU8 = AtomicU8::new(0);
static SYSTEM: AtomicBool = AtomicBool::new(false);

pub fn choice() -> Reduce {
    match CHOICE.load(Ordering::Relaxed) {
        1 => Reduce::On,
        2 => Reduce::Off,
        _ => Reduce::System,
    }
}

/// Sets the choice without telling gpui; `apply` does that once there is an
/// app. For the state file, read before the first window.
pub fn restore(r: Reduce) {
    CHOICE.store(r as u8, Ordering::Relaxed);
}

/// Sets the choice and moves the whole app to it.
pub fn set_choice(r: Reduce, cx: &mut App) {
    restore(r);
    apply(cx);
}

/// Whether motion is reduced now: the choice, and the system's setting when
/// the choice is System.
fn reduced() -> bool {
    choice().reduces(SYSTEM.load(Ordering::Relaxed))
}

/// Hands `reduced` to gpui, which stills its own animations and redraws
/// every window, so the drawings with timers of their own (the mascot, the
/// ASCII face) see `cx.reduce_motion()` change too. Starts the loop clock the
/// first time.
pub fn apply(cx: &mut App) {
    cx.set_reduce_motion(reduced());
    start_loop_clock(cx);
}

/// The motion clock's rate: how often a drawing with a timer of its own (the
/// mascot, the ASCII face) moves on. Everything timed shares this one clock,
/// so a loop and a drawing that move together cost one frame, not two.
pub const CLOCK_FPS: f32 = 30.;

/// How often looping effects (spinners, the shimmer, the beacon, the pulse)
/// redraw when nothing else on the clock moves: every other tick. A gpui
/// animation that repeats asks for every display frame, and a frame
/// re-renders the window: at 165 Hz that held the UI thread at about half a
/// core for as long as an agent worked. On a tick that draws a frame for a
/// drawing anyway, loops redraw too and turn at the full [`CLOCK_FPS`].
pub const LOOP_FPS: f32 = CLOCK_FPS / 2.;

thread_local! {
    /// The views that drew a loop since the clock last ticked.
    static LOOP_VIEWS: RefCell<Vec<EntityId>> = const { RefCell::new(Vec::new()) };
    /// The views to redraw on the next tick (see [`redraw_on_tick`]).
    static TICK_VIEWS: RefCell<Vec<EntityId>> = const { RefCell::new(Vec::new()) };
    /// The drawings waiting for the next tick (see [`next_tick`]).
    static TICK_WAITERS: RefCell<Vec<Waker>> = const { RefCell::new(Vec::new()) };
    /// How many times the clock has ticked.
    static TICKS: Cell<u64> = const { Cell::new(0) };
    static CLOCK_STARTED: Cell<bool> = const { Cell::new(false) };
    /// The sleeping clock's waker.
    static PARKED: RefCell<Option<Waker>> = const { RefCell::new(None) };
}

/// Whether anything waits on the clock: a loop drawn, or a drawing's tick.
fn clock_wanted() -> bool {
    LOOP_VIEWS.with(|v| !v.borrow().is_empty())
        || TICK_VIEWS.with(|v| !v.borrow().is_empty())
        || TICK_WAITERS.with(|w| !w.borrow().is_empty())
}

/// Ready once something waits on the clock. Until then the clock parks here
/// with no timer running, and `mark_looping` or `next_tick` wakes it.
fn loop_drawn() -> impl Future<Output = ()> {
    poll_fn(|task| {
        if clock_wanted() {
            Poll::Ready(())
        } else {
            PARKED.with(|p| *p.borrow_mut() = Some(task.waker().clone()));
            Poll::Pending
        }
    })
}

fn wake_clock() {
    let waker = PARKED.with(|p| p.borrow_mut().take());
    if let Some(waker) = waker {
        waker.wake();
    }
}

fn add_view(list: &'static std::thread::LocalKey<RefCell<Vec<EntityId>>>, view: EntityId) {
    list.with(|v| {
        let mut v = v.borrow_mut();
        if !v.contains(&view) {
            v.push(view);
        }
    });
    wake_clock();
}

/// Records that `view` drew a loop, and wakes the clock if it is parked.
fn mark_looping(view: EntityId) {
    add_view(&LOOP_VIEWS, view);
}

/// Redraws the view being drawn on the motion clock's next tick, for a fade
/// in progress: a gpui frame request redraws at the display's rate (165 Hz
/// on some screens), and a fade looks as smooth at [`CLOCK_FPS`]. Keep
/// `request_animation_frame` for things that travel under the hand, like
/// the caret's glide. Before the clock starts (a test app), it asks gpui.
pub fn redraw_on_tick(window: &Window) {
    if !CLOCK_STARTED.with(Cell::get) {
        window.request_animation_frame();
        return;
    }
    add_view(&TICK_VIEWS, window.current_view());
}

/// Ready at the motion clock's next tick: the frame timer of a drawing
/// (`frame_timer`) waits on this instead of a timer of its own, so its frames
/// land on the same ticks as the loops' and the window draws them as one.
pub fn next_tick() -> impl Future<Output = ()> {
    let start = TICKS.with(Cell::get);
    poll_fn(move |task| {
        if TICKS.with(Cell::get) != start {
            return Poll::Ready(());
        }
        TICK_WAITERS.with(|w| w.borrow_mut().push(task.waker().clone()));
        wake_clock();
        Poll::Pending
    })
}

/// What the clock does on tick `n`: wake the drawings waiting on it, and say
/// whether the loops redraw. Loops redraw on every other tick, and on any
/// tick a drawing or a fade (`redraw_on_tick`) draws a frame for anyway.
fn tick(n: u64) -> bool {
    TICKS.with(|t| t.set(n));
    let waiters = TICK_WAITERS.with(|w| std::mem::take(&mut *w.borrow_mut()));
    let drawing = !waiters.is_empty() || TICK_VIEWS.with(|v| !v.borrow().is_empty());
    for w in waiters {
        w.wake();
    }
    drawing || n.is_multiple_of(2)
}

/// The clock every loop reads, so loops side by side move together.
static EPOCH: OnceLock<Instant> = OnceLock::new();

/// Draws a loop of `period`: `draw` gets how far (0 to 1) the loop is now,
/// and the view it is drawn in redraws on the next tick of the loop clock.
/// Only that view redraws, so the rest of the window keeps its cached
/// drawing. 0 and no redraw while motion is reduced.
pub fn looping<E: IntoElement>(period: Duration, draw: impl FnOnce(f32) -> E) -> AnyElement {
    if reduced() {
        return draw(0.).into_any_element();
    }
    let secs = EPOCH.get_or_init(Instant::now).elapsed().as_secs_f64();
    Looping(draw(phase(secs, period)).into_any_element()).into_any_element()
}

/// `secs` into a loop of `period`, from 0 up to (not including) 1.
fn phase(secs: f64, period: Duration) -> f32 {
    let p = period.as_secs_f64();
    if p <= 0. {
        return 0.;
    }
    (secs / p).fract() as f32
}

/// An element drawn as it is, that tells the loop clock which view it is in.
/// Laid out with its view, so only a loop actually on screen keeps the clock
/// going.
struct Looping(AnyElement);

impl IntoElement for Looping {
    type Element = Self;

    fn into_element(self) -> Self {
        self
    }
}

impl Element for Looping {
    type RequestLayoutState = ();
    type PrepaintState = ();

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static std::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, ()) {
        mark_looping(window.current_view());
        (self.0.request_layout(window, cx), ())
    }

    fn prepaint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        _: Bounds<Pixels>,
        _: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) {
        self.0.prepaint(window, cx);
    }

    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        _: Bounds<Pixels>,
        _: &mut (),
        _: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) {
        self.0.paint(window, cx);
    }
}

/// Ticks [`CLOCK_FPS`] times a second while anything waits on it, so every
/// timed motion on screen shares one timer and one frame per tick. Each tick
/// wakes the drawings waiting on [`next_tick`], redraws the views that asked
/// with [`redraw_on_tick`] and, per [`tick`], the views that drew a loop. It
/// notifies those views instead of refreshing the windows: a refresh turns off
/// every view's cache, and redrawing whole windows made every other animation
/// stutter. With nothing waiting, the clock sleeps without a timer until
/// something asks for a tick again.
pub(crate) fn start_loop_clock(cx: &mut App) {
    if CLOCK_STARTED.with(|s| s.replace(true)) {
        return;
    }
    let period = Duration::from_secs_f32(1. / CLOCK_FPS);
    cx.spawn(async move |cx| {
        let mut n = TICKS.with(Cell::get);
        loop {
            loop_drawn().await;
            loop {
                cx.background_executor().timer(period).await;
                if !clock_wanted() {
                    break;
                }
                n += 1;
                let loops = tick(n);
                let mut views = TICK_VIEWS.with(|v| std::mem::take(&mut *v.borrow_mut()));
                if loops {
                    views.extend(LOOP_VIEWS.with(|v| std::mem::take(&mut *v.borrow_mut())));
                }
                if !views.is_empty() {
                    cx.update(|cx| {
                        for view in views {
                            cx.notify(view);
                        }
                    });
                }
            }
        }
    })
    .detach();
}

/// Reads the system's setting (macOS's Reduce motion, GNOME's Animations
/// switch, Windows' "Animation effects") off the UI thread and applies it.
/// Called at start and whenever the main window comes to the front, so a
/// change made in the system's settings shows on return.
pub fn follow_system(cx: &mut App) {
    cx.spawn(async move |cx| {
        let reduce = cx
            .background_executor()
            .spawn(async { system_reduces_motion() })
            .await;
        SYSTEM.store(reduce, Ordering::Relaxed);
        cx.update(apply);
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
    #[cfg(windows)]
    {
        use windows_sys::Win32::UI::WindowsAndMessaging::{
            SPI_GETCLIENTAREAANIMATION, SystemParametersInfoW,
        };
        let mut on: i32 = 1;
        // SAFETY: SPI_GETCLIENTAREAANIMATION writes one BOOL to the pointer.
        let ok = unsafe {
            SystemParametersInfoW(
                SPI_GETCLIENTAREAANIMATION,
                0,
                (&mut on as *mut i32).cast(),
                0,
            )
        };
        ok != 0 && on == 0
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux", windows)))]
    {
        false
    }
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
pub(super) fn read(program: &str, args: &[&str]) -> Option<String> {
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
    use std::sync::Arc;
    use std::sync::atomic::AtomicUsize;
    use std::task::{Context, Wake};

    #[test]
    fn the_loop_clock_sleeps_until_a_loop_is_drawn() {
        struct WakeCount(AtomicUsize);

        impl Wake for WakeCount {
            fn wake(self: Arc<Self>) {
                self.0.fetch_add(1, Ordering::Relaxed);
            }
        }

        LOOP_VIEWS.with(|v| v.borrow_mut().clear());
        PARKED.with(|p| *p.borrow_mut() = None);
        let view = EntityId::from(1u64);
        let count = Arc::new(WakeCount(AtomicUsize::new(0)));
        let waker = Waker::from(count.clone());
        let mut cx = Context::from_waker(&waker);
        let mut drawn = std::pin::pin!(loop_drawn());

        assert_eq!(drawn.as_mut().poll(&mut cx), Poll::Pending);
        assert_eq!(count.0.load(Ordering::Relaxed), 0);
        mark_looping(view);
        assert_eq!(count.0.load(Ordering::Relaxed), 1);
        assert_eq!(drawn.as_mut().poll(&mut cx), Poll::Ready(()));
        mark_looping(view);
        assert_eq!(count.0.load(Ordering::Relaxed), 1);
        // A view with two loops redraws once a tick.
        assert_eq!(LOOP_VIEWS.with(|v| v.borrow().clone()), vec![view]);

        LOOP_VIEWS.with(|v| v.borrow_mut().clear());
        let mut drawn = std::pin::pin!(loop_drawn());
        assert_eq!(drawn.as_mut().poll(&mut cx), Poll::Pending);
        PARKED.with(|p| *p.borrow_mut() = None);
    }

    #[test]
    fn a_drawing_shares_the_loops_ticks() {
        struct WakeCount(AtomicUsize);

        impl Wake for WakeCount {
            fn wake(self: Arc<Self>) {
                self.0.fetch_add(1, Ordering::Relaxed);
            }
        }

        TICK_WAITERS.with(|w| w.borrow_mut().clear());
        PARKED.with(|p| *p.borrow_mut() = None);
        let count = Arc::new(WakeCount(AtomicUsize::new(0)));
        let waker = Waker::from(count.clone());
        let mut cx = Context::from_waker(&waker);

        // Alone, loops redraw on every other tick.
        assert!(!tick(1));
        assert!(tick(2));
        // A drawing waiting on the clock wakes it, and its tick redraws the
        // loops too, since that tick draws a frame anyway.
        let mut next = std::pin::pin!(next_tick());
        assert_eq!(next.as_mut().poll(&mut cx), Poll::Pending);
        assert!(clock_wanted());
        assert!(tick(3));
        assert_eq!(count.0.load(Ordering::Relaxed), 1);
        assert_eq!(next.as_mut().poll(&mut cx), Poll::Ready(()));
        assert!(!clock_wanted());
        assert!(!tick(5));
        // So does a view with a fade in progress.
        add_view(&TICK_VIEWS, EntityId::from(2u64));
        assert!(clock_wanted());
        assert!(tick(7));
        TICK_VIEWS.with(|v| v.borrow_mut().clear());
    }

    #[test]
    fn the_choice_overrides_the_system_and_round_trips() {
        for r in Reduce::ALL {
            assert_eq!(Reduce::from_id(r.id()), r);
        }
        assert_eq!(Reduce::from_id("something newer"), Reduce::System);
        assert!(Reduce::System.reduces(true));
        assert!(!Reduce::System.reduces(false));
        assert!(Reduce::On.reduces(false));
        assert!(!Reduce::Off.reduces(true));
    }

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
        for f in [ease_out, ease_in_out, ease_out_back] {
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

    #[test]
    fn a_loop_wraps_at_its_period() {
        let p = Duration::from_millis(1000);
        assert_eq!(phase(0., p), 0.);
        assert!((phase(0.25, p) - 0.25).abs() < 1e-6);
        assert!((phase(2.5, p) - 0.5).abs() < 1e-6);
        // A whole number of periods is the start again, never 1.
        assert!(phase(3., p) < 1e-6);
        assert_eq!(phase(1., Duration::ZERO), 0.);
    }
}
