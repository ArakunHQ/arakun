//! Work a view or model repeats on a timer for as long as it lives.

use std::time::Duration;

use gpui::{Context, Task};

/// Calls `f` on the entity after `first`, then every `every`, until the
/// entity is dropped. Dropping the task stops it sooner. A `first` of zero
/// calls it on the next turn of the main thread.
pub fn poll_every<T: 'static>(
    cx: &mut Context<T>,
    first: Duration,
    every: Duration,
    mut f: impl FnMut(&mut T, &mut Context<T>) + 'static,
) -> Task<()> {
    cx.spawn(async move |this, cx| {
        let mut wait = first;
        loop {
            if !wait.is_zero() {
                cx.background_executor().timer(wait).await;
            }
            if this.update(cx, |this, cx| f(this, cx)).is_err() {
                break;
            }
            wait = every;
        }
    })
}
