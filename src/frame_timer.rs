//! The frame timer of an animated drawing: the mascot and the ASCII face.

use std::cell::Cell;
use std::rc::Rc;
use std::time::Duration;

use futures::StreamExt;
use futures::channel::mpsc;
use gpui::{Context, Task};

/// About 30 frames a second while something moves.
const FRAME: Duration = Duration::from_millis(33);
/// How long a drawing with nothing to do sleeps before it checks again. A
/// change it isn't told about (a dance started elsewhere) waits at most this.
const IDLE: Duration = Duration::from_millis(500);

/// Ends a frame timer's long sleep. Its view calls `wake` when it renders,
/// so a drawing shown again moves at once.
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

/// Calls `tick` about 30 times a second while it returns true. When it returns
/// false (nothing moves, the window is in the background or the view is not on
/// screen) the timer sleeps until `Waker::wake` or for `IDLE`, so an idle
/// drawing doesn't wake the app 30 times a second.
pub fn start<T: 'static>(
    cx: &mut Context<T>,
    mut tick: impl FnMut(&mut T, &mut Context<T>) -> bool + 'static,
) -> (Task<()>, Waker) {
    let sleeping = Rc::new(Cell::new(false));
    let (tx, mut rx) = mpsc::unbounded();
    let asleep = sleeping.clone();
    let task = cx.spawn(async move |this, cx| {
        let mut busy = true;
        loop {
            if busy {
                cx.background_executor().timer(FRAME).await;
            } else {
                asleep.set(true);
                let timer = std::pin::pin!(cx.background_executor().timer(IDLE));
                futures::future::select(timer, rx.next()).await;
                asleep.set(false);
            }
            match this.update(cx, &mut tick) {
                Ok(b) => busy = b,
                Err(_) => break,
            }
        }
    });
    (task, Waker { sleeping, tx })
}
