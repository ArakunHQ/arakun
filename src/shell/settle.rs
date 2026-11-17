//! Watches the environments' branches for a merged pull request and settles it
//! (`crate::settle`): its review threads resolved, its notifications read, and
//! the environment's sessions no longer asking for attention.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use gpui::{AppContext as _, Context};

use super::Shell;
use crate::github::{pr, queue};
use crate::model::{Status, TabKind, unix_now};
use crate::settle::{self, Book, Merged};

/// Each check asks GitHub once per environment: not more often than this.
const EVERY: Duration = Duration::from_secs(180);

#[derive(Default)]
pub(super) struct Watch {
    last: Option<Instant>,
    busy: bool,
    /// Read from disk on the first check.
    book: Option<Book>,
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
                .background_spawn(async move {
                    envs.into_iter()
                        .filter_map(|(path, branch)| {
                            settle::merged_pr(&path, &branch).map(|m| (path, m))
                        })
                        .collect()
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                this.settle.busy = false;
                // Turned off while GitHub was asked.
                if !settle::on() {
                    return;
                }
                let now = unix_now();
                let book = this.settle.book.get_or_insert_with(Book::load);
                let fresh: Vec<(PathBuf, Merged)> = found
                    .into_iter()
                    .filter(|(_, m)| m.recent(now) && !book.has(&m.url))
                    .collect();
                if fresh.is_empty() {
                    return;
                }
                for (_, m) in &fresh {
                    book.add(&m.url);
                }
                book.store();
                for (path, m) in fresh {
                    this.settle_merge(&path, &m, cx);
                }
            });
        })
        .detach();
    }

    fn settle_merge(&mut self, path: &Path, m: &Merged, cx: &mut Context<Self>) {
        self.settle_sessions(path, m.merged_secs(), cx);
        let Some(item) = pr::ItemRef::from_url(&m.url) else {
            return;
        };
        let key = queue::key_of(&item);
        self.github.update(cx, |g, cx| g.settle_merged(&key, cx));
        cx.background_spawn(async move {
            if let Err(e) = pr::resolve_open_threads(&item) {
                eprintln!(
                    "arakun: could not resolve the threads of {}#{}: {}",
                    item.repo,
                    item.number,
                    e.text()
                );
            }
        })
        .detach();
        cx.notify();
    }

    /// The environment's sessions from before the merge stop asking for
    /// attention, and the agents nobody is looking at stop. A session still
    /// working, or used after the merge, is left alone.
    fn settle_sessions(&mut self, path: &Path, merged_at: u64, cx: &mut Context<Self>) {
        let Some(env) = self.find_env_mut(path) else {
            return;
        };
        let mut ids = Vec::new();
        for s in &mut env.sessions {
            let open = env
                .tabs
                .iter()
                .any(|t| matches!(&t.kind, TabKind::Session { id, .. } if *id == s.id));
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
