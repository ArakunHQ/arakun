//! What a view reads in the background: the last answer, the last error,
//! whether a read is under way, and the task doing it. One shape for every
//! view, so no path forgets to clear `loading`.

use std::convert::Infallible;
use std::future::Future;

use futures::StreamExt as _;
use futures::channel::mpsc::UnboundedSender;
use gpui::{AppContext as _, Context, Task};

use crate::guard::Fallback;

pub struct Loadable<T, E = String> {
    /// The last answer. Kept while a new read runs and when it fails, so the
    /// view doesn't go blank.
    pub data: Option<T>,
    pub error: Option<E>,
    loading: bool,
    task: Option<Task<()>>,
}

impl<T, E> Default for Loadable<T, E> {
    fn default() -> Self {
        Self::new(None)
    }
}

impl<T, E> Loadable<T, E> {
    /// Starts with `data`, such as an answer cached on disk.
    pub fn new(data: Option<T>) -> Self {
        Self {
            data,
            error: None,
            loading: false,
            task: None,
        }
    }

    pub fn loading(&self) -> bool {
        self.loading
    }

    /// Drops the read under way. Its answer never arrives: dropping a gpui
    /// task stops it, and its update runs on this thread, so it can't be
    /// halfway through either.
    pub fn cancel(&mut self) {
        self.task = None;
        self.loading = false;
    }

    /// Forgets the answer too, for another project or account.
    pub fn reset(&mut self) {
        self.cancel();
        self.data = None;
        self.error = None;
    }

    /// Keeps an answer: data clears the error, an error keeps the old data.
    pub fn settle(&mut self, result: Result<T, E>) {
        match result {
            Ok(data) => {
                self.data = Some(data);
                self.error = None;
            }
            Err(e) => self.error = Some(e),
        }
    }

    /// Runs `work` off the UI thread, then hands its output to `apply` on the
    /// view, which usually keeps it with [`Self::settle`]. `slot` finds this
    /// field in the view again. `loading` is clear before `apply` runs, and
    /// the view is notified after it. A panic in `work` ends the read with
    /// its output's fallback. Does nothing while a read is under way.
    pub fn start<V, W>(
        &mut self,
        cx: &mut Context<V>,
        slot: fn(&mut V) -> &mut Self,
        work: W,
        apply: impl FnOnce(&mut V, W::Output, &mut Context<V>) + 'static,
    ) where
        V: 'static,
        T: 'static,
        E: 'static,
        W: Future + Send + 'static,
        W::Output: Fallback + Send + 'static,
    {
        self.start_streaming(
            cx,
            slot,
            |_: UnboundedSender<Infallible>| work,
            |_, never, _| match never {},
            apply,
        );
    }

    /// [`Self::start`] for a read that has something to show before it
    /// ends: `work` gets a sender, and each value it sends reaches `progress`
    /// on the view, which is notified after each. `loading` stays set through
    /// them; `apply` runs after the last one, as in `start`.
    pub fn start_streaming<V, P, W>(
        &mut self,
        cx: &mut Context<V>,
        slot: fn(&mut V) -> &mut Self,
        work: impl FnOnce(UnboundedSender<P>) -> W,
        progress: impl Fn(&mut V, P, &mut Context<V>) + 'static,
        apply: impl FnOnce(&mut V, W::Output, &mut Context<V>) + 'static,
    ) where
        V: 'static,
        T: 'static,
        E: 'static,
        P: Send + 'static,
        W: Future + Send + 'static,
        W::Output: Fallback + Send + 'static,
    {
        if self.loading {
            return;
        }
        self.loading = true;
        cx.notify();
        let (tx, mut rx) = futures::channel::mpsc::unbounded();
        let work = work(tx);
        self.task = Some(cx.spawn(async move |this, cx| {
            let work = cx.background_spawn(crate::guard::task(work));
            // Ends when `work` drops its sender, by finishing or panicking.
            while let Some(p) = rx.next().await {
                let alive = this.update(cx, |view, cx| {
                    progress(view, p, cx);
                    cx.notify();
                });
                if alive.is_err() {
                    return;
                }
            }
            let out = work.await;
            this.update(cx, |view, cx| {
                slot(view).loading = false;
                apply(view, out, cx);
                cx.notify();
            })
            .ok();
        }));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn data_clears_the_error() {
        let mut l = Loadable::<u32>::default();
        l.settle(Err("down".into()));
        assert_eq!(l.error.as_deref(), Some("down"));
        l.settle(Ok(1));
        assert_eq!((l.data, l.error), (Some(1), None));
    }

    #[test]
    fn an_error_keeps_the_last_data() {
        let mut l = Loadable::<u32>::new(Some(1));
        l.settle(Err("down".into()));
        assert_eq!(l.data, Some(1));
        assert_eq!(l.error.as_deref(), Some("down"));
    }

    #[test]
    fn reset_forgets_everything() {
        let mut l = Loadable::<u32>::new(Some(1));
        l.loading = true;
        l.error = Some("down".into());
        l.reset();
        assert!(!l.loading());
        assert_eq!((l.data, l.error), (None, None));
    }

    #[test]
    fn cancel_keeps_the_data() {
        let mut l = Loadable::<u32>::new(Some(1));
        l.loading = true;
        l.cancel();
        assert!(!l.loading());
        assert_eq!(l.data, Some(1));
    }
}
