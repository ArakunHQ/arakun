//! The frame timer of an animated drawing: the mascot and the ASCII face.

use std::cell::Cell;
use std::rc::Rc;
use std::time::Duration;

use futures::StreamExt;
use futures::channel::mpsc;
use gpui::{Context, Task};

/// When a drawing wants its next tick.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Next {
    /// Call again on the next motion-clock tick.
    Tick,
    /// Sleep until `Waker::wake` or `Duration` elapses, whichever comes first.
    After(Duration),
    /// Sleep until `Waker::wake` without arming a periodic timer.
    Idle,
}

/// Ends a frame timer's sleep. Its view calls `wake` when it renders or
/// receives input, so a drawing shown again moves at once.
pub struct Waker {
    sleeping: Rc<Cell<bool>>,
    tx: mpsc::UnboundedSender<()>,
}

impl Waker {
    pub fn wake(&self) {
        if self.sleeping.replace(false) {
            let _ = self.tx.unbounded_send(());
        }
    }
}

/// Calls `tick` on every tick of the motion clock (`motion::CLOCK_FPS`) while
/// it returns [`Next::Tick`]. The clock is shared with the looping effects, so a
/// drawing that moves while a spinner turns costs one frame a tick, not two.
/// When it returns [`Next::After`] or [`Next::Idle`], the timer sleeps until
/// `Waker::wake` (or the requested deadline), so an idle drawing costs zero
/// periodic wakeups.
pub fn start<T: 'static>(
    cx: &mut Context<T>,
    mut tick: impl FnMut(&mut T, &mut Context<T>) -> Next + 'static,
) -> (Task<()>, Waker) {
    let sleeping = Rc::new(Cell::new(false));
    let (tx, mut rx) = mpsc::unbounded();
    let asleep = sleeping.clone();
    crate::ui::motion::start_loop_clock(cx);
    let task = cx.spawn(async move |this, cx| {
        let mut next = Next::Tick;
        loop {
            match next {
                Next::Tick => crate::ui::motion::next_tick().await,
                Next::After(wait) => {
                    asleep.set(true);
                    let timer = std::pin::pin!(cx.background_executor().timer(wait));
                    futures::future::select(timer, rx.next()).await;
                    asleep.set(false);
                }
                Next::Idle => {
                    asleep.set(true);
                    let _ = rx.next().await;
                    asleep.set(false);
                }
            }
            match this.update(cx, &mut tick) {
                Ok(n) => next = n,
                Err(_) => break,
            }
        }
    });
    (task, Waker { sleeping, tx })
}
