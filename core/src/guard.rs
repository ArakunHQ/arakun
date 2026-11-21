//! Keeps a panic in background work from taking the app down or leaving a
//! feature stuck.
//!
//! GPUI builds its tasks without `propagate_panic`: when a task run with
//! `cx.background_spawn(...)` panics, whoever awaits it on the main thread
//! panics too ("Task polled after completion"), so one bad parse on a worker
//! thread ends the whole app. A plain thread that panics dies without a word,
//! and what waited for it waits forever.
//!
//! `task` wraps the body of a background task and `run` the body of a thread.
//! Both stop the panic there and hand the caller something it already knows
//! how to handle. The panic hook (`crash`) has by then written the panic to
//! the crash log, and reports it as an error instead of a crash (`active`).

use std::any::Any;
use std::cell::Cell;
use std::fmt;
use std::future::Future;
use std::panic::{AssertUnwindSafe, Location, catch_unwind};
use std::pin::Pin;
use std::task::{Context, Poll};

/// A panic that was stopped: what it said.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Panicked {
    pub message: String,
}

impl Panicked {
    fn from_payload(payload: &(dyn Any + Send)) -> Self {
        let message = payload
            .downcast_ref::<&str>()
            .map(|s| s.to_string())
            .or_else(|| payload.downcast_ref::<String>().cloned())
            .unwrap_or_else(|| "panic occurred".into());
        Panicked { message }
    }
}

/// Shown to the user where the work's own errors are shown.
impl fmt::Display for Panicked {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Arakun hit an internal error ({})", self.message)
    }
}

impl std::error::Error for Panicked {}

impl From<Panicked> for String {
    fn from(p: Panicked) -> String {
        p.to_string()
    }
}

impl From<Panicked> for std::io::Error {
    fn from(p: Panicked) -> std::io::Error {
        std::io::Error::other(p)
    }
}

thread_local! {
    /// How many guards this thread is inside of.
    static DEPTH: Cell<u32> = const { Cell::new(0) };
}

/// Whether a panic on this thread right now will be stopped by a guard, so
/// the process lives on. The panic hook asks, to tell an error from a crash.
pub fn active() -> bool {
    DEPTH.with(Cell::get) > 0
}

fn catch<T>(f: impl FnOnce() -> T) -> Result<T, Panicked> {
    DEPTH.with(|d| d.set(d.get() + 1));
    // The callers hand back an error and never look at what `f` left half done.
    let result = catch_unwind(AssertUnwindSafe(f));
    DEPTH.with(|d| d.set(d.get() - 1));
    result.map_err(|payload| {
        let panicked = Panicked::from_payload(&*payload);
        // A payload's own destructor can panic too.
        let _ = catch_unwind(AssertUnwindSafe(move || drop(payload)));
        panicked
    })
}

/// Runs `f`, the body of a thread or a part of one, and turns a panic into an
/// error, so the thread can still answer whoever waits for it. `what` names
/// the work in the note on stderr.
pub fn run<T>(what: &str, f: impl FnOnce() -> T) -> Result<T, Panicked> {
    catch(f).inspect_err(|p| eprintln!("arakun: {what} stopped: {p}"))
}

/// What a guarded task hands back when it panicked: the value its caller
/// already takes as "this did not work" or "nothing found".
pub trait Fallback {
    fn fallback(panicked: Panicked) -> Self;
}

impl<T, E: From<Panicked>> Fallback for Result<T, E> {
    fn fallback(panicked: Panicked) -> Self {
        Err(panicked.into())
    }
}

impl<T> Fallback for Option<T> {
    fn fallback(_: Panicked) -> Self {
        None
    }
}

impl<T> Fallback for Vec<T> {
    fn fallback(_: Panicked) -> Self {
        Vec::new()
    }
}

impl Fallback for () {
    fn fallback(_: Panicked) -> Self {}
}

impl Fallback for bool {
    fn fallback(_: Panicked) -> Self {
        false
    }
}

impl Fallback for String {
    fn fallback(_: Panicked) -> Self {
        String::new()
    }
}

impl<A: Fallback, B: Fallback> Fallback for (A, B) {
    fn fallback(panicked: Panicked) -> Self {
        (A::fallback(panicked.clone()), B::fallback(panicked))
    }
}

impl<A: Fallback, B: Fallback, C: Fallback> Fallback for (A, B, C) {
    fn fallback(panicked: Panicked) -> Self {
        (
            A::fallback(panicked.clone()),
            B::fallback(panicked.clone()),
            C::fallback(panicked),
        )
    }
}

/// Wraps the future given to `cx.background_spawn(...)`: when it panics, the
/// task ends with its output's `Fallback` instead of panicking in whoever
/// awaits it.
#[track_caller]
pub fn task<F>(future: F) -> Task<F, fn(Panicked) -> F::Output>
where
    F: Future,
    F::Output: Fallback,
{
    task_or(future, F::Output::fallback)
}

/// `task` for an output whose empty value already means "nothing found".
#[track_caller]
pub fn task_or_default<F>(future: F) -> Task<F, fn(Panicked) -> F::Output>
where
    F: Future,
    F::Output: Default,
{
    task_or(future, |_| F::Output::default())
}

/// `task` where the caller says what a panic turns into.
#[track_caller]
pub fn task_or<F, R>(future: F, recover: R) -> Task<F, R>
where
    F: Future,
    R: FnOnce(Panicked) -> F::Output + Unpin,
{
    Task {
        running: Some((Box::pin(future), recover)),
        at: Location::caller(),
    }
}

/// The future `task` makes.
pub struct Task<F, R> {
    /// Gone once it finished or panicked.
    running: Option<(Pin<Box<F>>, R)>,
    at: &'static Location<'static>,
}

impl<F, R> Future for Task<F, R>
where
    F: Future,
    R: FnOnce(Panicked) -> F::Output + Unpin,
{
    type Output = F::Output;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let (future, _) = self
            .running
            .as_mut()
            .expect("a guarded task polled after it finished");
        match catch(|| future.as_mut().poll(cx)) {
            Ok(Poll::Pending) => Poll::Pending,
            Ok(Poll::Ready(output)) => {
                self.running = None;
                Poll::Ready(output)
            }
            Err(panicked) => {
                let (future, recover) = self.running.take().expect("checked above");
                // What it held may be half changed, and dropping that can panic again.
                let _ = catch(move || drop(future));
                eprintln!(
                    "arakun: the background task at {}:{} stopped: {panicked}",
                    self.at.file(),
                    self.at.line()
                );
                Poll::Ready(recover(panicked))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::task::Waker;

    fn poll_once<F: Future + Unpin>(mut future: F) -> Poll<F::Output> {
        Pin::new(&mut future).poll(&mut Context::from_waker(Waker::noop()))
    }

    #[test]
    fn run_hands_back_the_value_or_the_panic() {
        assert_eq!(run("adding", || 1 + 1), Ok(2));
        let stopped = run("parsing", || -> u32 { panic!("bad byte {}", 7) });
        assert_eq!(stopped.unwrap_err().message, "bad byte 7");
        assert!(!active());
    }

    #[test]
    fn a_guard_is_active_only_inside() {
        assert!(!active());
        assert_eq!(run("looking", active), Ok(true));
        assert!(!active());
    }

    #[test]
    fn a_task_that_panics_ends_with_its_fallback() {
        let failed: Poll<Result<u32, String>> = poll_once(task(async { panic!("no rows") }));
        assert_eq!(
            failed,
            Poll::Ready(Err("Arakun hit an internal error (no rows)".into()))
        );
        let none: Poll<Option<u32>> = poll_once(task(async { panic!("gone") }));
        assert_eq!(none, Poll::Ready(None));
        let error: Poll<anyhow::Result<()>> = poll_once(task(async { panic!("gone") }));
        assert!(matches!(error, Poll::Ready(Err(_))));
    }

    #[test]
    fn a_task_can_say_what_a_panic_turns_into() {
        let empty: Poll<std::collections::HashMap<u8, u8>> =
            poll_once(task_or_default(async { panic!("gone") }));
        assert!(matches!(empty, Poll::Ready(map) if map.is_empty()));
        let all: Poll<usize> = poll_once(task_or(async { panic!("gone") }, |_| 4));
        assert_eq!(all, Poll::Ready(4));
    }

    #[test]
    fn a_task_that_works_is_left_alone() {
        let done: Poll<Result<u32, String>> = poll_once(task(async { Ok(3) }));
        assert_eq!(done, Poll::Ready(Ok(3)));
    }

    #[test]
    fn a_task_can_panic_after_waiting() {
        let mut waited = false;
        let mut guarded = task(std::future::poll_fn(move |_| {
            if !waited {
                waited = true;
                return Poll::<Option<u32>>::Pending;
            }
            panic!("second poll")
        }));
        let mut cx = Context::from_waker(Waker::noop());
        assert_eq!(Pin::new(&mut guarded).poll(&mut cx), Poll::Pending);
        assert_eq!(Pin::new(&mut guarded).poll(&mut cx), Poll::Ready(None));
    }
}
