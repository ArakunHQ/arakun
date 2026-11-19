//! Watches the environments' branches for a merged pull request and settles it
//! (`crate::settle`): its review threads resolved, its notifications read, and
//! the environment's sessions no longer asking for attention.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use gpui::{AppContext as _, Context};

use super::Shell;
use crate::github::{pr, queue};
use crate::model::{Status, unix_now};
use crate::settle::{self, Book, Merged};

/// Each check asks GitHub once per environment: not more often than this.
const EVERY: Duration = Duration::from_secs(180);
/// Checks that try again to resolve a merge's threads before giving up on them.
const TRIES: u32 = 5;

#[derive(Default)]
pub(super) struct Watch {
    last: Option<Instant>,
    /// A check is asking GitHub or resolving threads.
    busy: bool,
    /// Read from disk on the first check. A merge goes in once its threads are
    /// resolved, or once [`TRIES`] checks failed to.
    book: Option<Book>,
    /// Merges whose sessions and notifications were settled in this run, while
    /// their threads may still be waiting for another try.
    local: HashSet<String>,
    /// Failed tries to resolve each merge's threads.
    tries: HashMap<String, u32>,
}

impl Watch {
    /// After a try to resolve `url`'s threads: whether the merge is done with.
    fn tried(&mut self, url: &str, ok: bool) -> bool {
        if ok {
            self.tries.remove(url);
            return true;
        }
        let n = self.tries.entry(url.to_string()).or_default();
        *n += 1;
        *n >= TRIES
    }
}

impl Shell {
    /// Called on the scheduler's tick; asks GitHub at most every [`EVERY`].
    pub(super) fn watch_merges(&mut self, cx: &mut Context<Self>) {
        if !settle::on()
            || self.settle.busy
            || self.settle.last.is_some_and(|t| t.elapsed() < EVERY)
        {
            return;
        }
        let envs: Vec<(PathBuf, String)> = self
            .projects
            .iter()
            .flat_map(|p| &p.environments)
            // The main checkout sits on the default branch: `--head main` would
            // match any merged pull request opened from a fork's `main`.
            .filter(|e| !e.is_checkout && e.sync.github && e.sync.gh)
            .filter_map(|e| Some((e.path.clone(), e.sync.branch.clone()?)))
            .collect();
        self.settle.last = Some(Instant::now());
        if envs.is_empty() {
            return;
        }
        self.settle.busy = true;
        cx.spawn(async move |this, cx| {
            let found: Vec<(PathBuf, Merged)> = cx
                .background_spawn(crate::guard::task(async move {
                    envs.into_iter()
                        .flat_map(|(path, branch)| {
                            settle::merged_prs(&path, &branch)
                                .into_iter()
                                .map(move |m| (path.clone(), m))
                        })
                        .collect()
                }))
                .await;
            let Ok(threads) = this.update(cx, |this, cx| this.settle_found(found, cx)) else {
                return;
            };
            let results: Vec<(String, bool)> = cx
                .background_spawn(crate::guard::task(async move {
                    threads
                        .into_iter()
                        .map(|(url, host, item)| {
                            let done = pr::resolve_open_threads(&host, &item);
                            if let Err(e) = &done {
                                eprintln!(
                                    "arakun: could not resolve the threads of {}#{}: {}",
                                    item.repo,
                                    item.number,
                                    e.text()
                                );
                            }
                            (url, done.is_ok())
                        })
                        .collect()
                }))
                .await;
            let _ = this.update(cx, |this, _| {
                let w = &mut this.settle;
                w.busy = false;
                let done: Vec<String> = results
                    .into_iter()
                    .filter_map(|(url, ok)| w.tried(&url, ok).then_some(url))
                    .collect();
                if let Some(book) = w.book.as_mut().filter(|_| !done.is_empty()) {
                    for url in &done {
                        book.add(url);
                    }
                    book.store();
                }
            });
        })
        .detach();
    }

    /// Settles here what GitHub found merged and not settled yet, and returns
    /// the threads still to resolve: (pull request URL, host, item).
    fn settle_found(
        &mut self,
        found: Vec<(PathBuf, Merged)>,
        cx: &mut Context<Self>,
    ) -> Vec<(String, String, pr::ItemRef)> {
        // Turned off while GitHub was asked.
        if !settle::on() {
            self.settle.busy = false;
            return Vec::new();
        }
        let now = unix_now();
        let book = self.settle.book.get_or_insert_with(Book::load);
        let fresh: Vec<(PathBuf, Merged)> = found
            .into_iter()
            .filter(|(_, m)| m.recent(now) && !book.has(&m.url))
            .collect();
        let mut threads = Vec::new();
        for (path, m) in fresh {
            let Some((host, item)) = pr::ItemRef::host_and_item(&m.url) else {
                continue;
            };
            if self.settle.local.insert(m.url.clone()) {
                self.settle_sessions(&path, m.merged_secs(), cx);
                let key = queue::key_of(&item);
                self.github.update(cx, |g, cx| g.settle_merged(&key, cx));
                cx.notify();
            }
            if !threads.iter().any(|(u, _, _)| *u == m.url) {
                threads.push((m.url, host, item));
            }
        }
        threads
    }

    /// The environment's sessions from before the merge stop asking for
    /// attention, and the agents nobody is looking at stop. A session still
    /// working, or used after the merge, is left alone.
    fn settle_sessions(&mut self, path: &Path, merged_at: u64, cx: &mut Context<Self>) {
        let strip = &self.strip;
        let Some(env) = self
            .projects
            .iter_mut()
            .flat_map(|p| p.environments.iter_mut())
            .find(|e| e.path == path)
        else {
            return;
        };
        let mut ids = Vec::new();
        for s in &mut env.sessions {
            let open = strip.session_tab(&s.id).is_some();
            if let Some(status) = settled_status(s.status, s.updated, merged_at, open) {
                s.status = status;
                ids.push(s.id.clone());
            }
        }
        if ids.is_empty() {
            return;
        }
        for id in ids {
            self.retire_if_hidden(path, &id, cx);
        }
        self.save(cx);
    }
}

/// What a session's status becomes once its pull request merged at
/// `merged_at`; None for one to leave alone.
fn settled_status(status: Status, updated: u64, merged_at: u64, open: bool) -> Option<Status> {
    if updated > merged_at {
        return None;
    }
    match status {
        Status::Working => None,
        // A question in a tab the user has open is theirs to answer.
        Status::NeedsInput if open => None,
        _ => Some(Status::Idle),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_merge_is_done_once_resolved_or_after_its_last_try() {
        let mut w = Watch::default();
        for _ in 1..TRIES {
            assert!(!w.tried("u", false));
        }
        assert!(w.tried("u", false));
        assert!(w.tried("v", true));
        assert!(!w.tries.contains_key("v"));
    }

    #[test]
    fn only_sessions_from_before_the_merge_settle() {
        let s = |status, updated, open| settled_status(status, updated, 100, open);
        assert_eq!(s(Status::Error, 90, false), Some(Status::Idle));
        assert_eq!(s(Status::NeedsInput, 90, false), Some(Status::Idle));
        assert_eq!(s(Status::Idle, 100, true), Some(Status::Idle));
        // Still running, waiting in an open tab, or used since: left alone.
        assert_eq!(s(Status::Working, 90, false), None);
        assert_eq!(s(Status::NeedsInput, 90, true), None);
        assert_eq!(s(Status::Error, 101, false), None);
    }
}
