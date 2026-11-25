//! Headless sessions (`arakun agent run`, see `crate::headless`) in the
//! sidebar: under the project holding their folder, else under a Headless
//! project, followed once a second. One opens read-only; a message to one
//! goes to its host (started again if it finished), so it stays sandboxed.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use gpui::{AppContext as _, Context, SharedString, Window};

use super::Shell;
use crate::agent::{self, Thread};
use crate::headless::{self, Meta, Status};
use crate::model::{self, Session};
use crate::state;

thread_local! {
    /// What each open viewer last showed: the transcript's time and the status.
    static SEEN: RefCell<HashMap<String, (Option<SystemTime>, Status)>> = RefCell::new(HashMap::new());
}

fn app_status(s: Status) -> model::Status {
    match s {
        Status::Working => model::Status::Working,
        Status::NeedsInput => model::Status::NeedsInput,
        Status::Idle => model::Status::Idle,
        Status::Error => model::Status::Error,
    }
}

fn session_meta(m: &Meta) -> agent::SessionMeta {
    agent::SessionMeta {
        agent: m.agent,
        session_id: m.session_id.clone(),
        title: m.title.clone(),
        cwd: m.cwd.clone(),
        created: m.created,
        updated: m.updated,
        pr: None,
        seat: None,
        seat_key: None,
    }
}

fn modified(id: &str) -> Option<SystemTime> {
    std::fs::metadata(headless::transcript_path(id))
        .and_then(|m| m.modified())
        .ok()
}

impl Shell {
    pub(super) fn watch_headless(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        cx.spawn_in(window, async move |this, cx| {
            // Every meta file is read and parsed on each scan. In the
            // background one in five seconds keeps the sidebar and the
            // phone's statuses close; coming to the front scans within one.
            let mut skipped = 0;
            let mut has_active = false;
            loop {
                let Ok((active, open)) = this.update_in(cx, |this, window, _| {
                    (window.is_window_active(), this.open_headless())
                }) else {
                    break;
                };
                if !has_active || active || skipped >= 4 {
                    skipped = 0;
                    let found = cx
                        .background_spawn(crate::guard::task(async move {
                            headless::list()
                                .into_iter()
                                .map(|m| {
                                    let time = modified(&m.id);
                                    // Parsed here, off the UI thread: a long
                                    // session's transcript is large.
                                    let t = open
                                        .get(&m.id)
                                        .is_some_and(|&seen| {
                                            seen != Some((time, headless::status(&m)))
                                        })
                                        .then(|| headless::read_transcript(&m.id))
                                        .flatten();
                                    (time, m, t)
                                })
                                .collect::<Vec<_>>()
                        }))
                        .await;
                    has_active = found.iter().any(|(_, m, _)| {
                        matches!(
                            headless::status(m),
                            headless::Status::Working | headless::Status::NeedsInput
                        )
                    });
                    if this
                        .update_in(cx, |this, window, cx| this.sync_headless(found, window, cx))
                        .is_err()
                    {
                        break;
                    }
                } else {
                    skipped += 1;
                }
                let wait = if has_active {
                    Duration::from_secs(1)
                } else {
                    Duration::from_secs(10)
                };
                cx.background_executor().timer(wait).await;
            }
        })
        .detach();
    }

    /// The headless sessions open in a viewer, with what each last showed.
    fn open_headless(&self) -> HashMap<String, Option<(Option<SystemTime>, Status)>> {
        self.projects
            .iter()
            .flat_map(|p| &p.environments)
            .flat_map(|e| &e.sessions)
            .filter(|s| s.live.is_some() && headless::shown(&s.id))
            .map(|s| {
                let seen = SEEN.with(|seen| seen.borrow().get(s.id.as_ref()).copied());
                (s.id.to_string(), seen)
            })
            .collect()
    }

    fn sync_headless(
        &mut self,
        found: Vec<(Option<SystemTime>, Meta, Option<agent::Transcript>)>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // A scan that started before a delete can still carry the deleted session.
        let found: Vec<_> = found
            .into_iter()
            .filter(|(_, m, _)| !headless::discarding(&m.id))
            .collect();
        let ids: HashSet<&str> = found.iter().map(|(_, m, _)| m.id.as_str()).collect();
        let gone: Vec<(PathBuf, String)> = self
            .projects
            .iter()
            .flat_map(|p| &p.environments)
            .flat_map(|e| {
                e.sessions
                    .iter()
                    .map(move |s| (e.path.clone(), s.id.to_string()))
            })
            .filter(|(_, id)| headless::shown(id) && !ids.contains(id.as_str()))
            .collect();
        let mut changed = !gone.is_empty();
        for (env_path, id) in gone {
            self.drop_headless(&env_path, &id, window, cx);
        }
        for (time, m, transcript) in found {
            let status = headless::status(&m);
            let place = self
                .projects
                .iter()
                .flat_map(|p| &p.environments)
                .find(|e| e.session_index(&m.id).is_some())
                .map(|e| e.path.clone());
            let env_path = match place {
                Some(p) if headless::shown(&m.id) => p,
                // Adopted by the app meanwhile: its own now.
                Some(_) => continue,
                None => {
                    let env_path = self.env_for(&m.cwd).unwrap_or_else(|| self.headless_env());
                    let Some(env) = self.find_env_mut(&env_path) else {
                        continue;
                    };
                    let mut s = Session::new(m.id.clone().into(), m.title.clone().into(), m.agent);
                    s.status = app_status(status);
                    s.updated = m.updated;
                    s.cli_model = Some(m.model.clone());
                    s.meta = Some(session_meta(&m));
                    let at = env
                        .sessions
                        .iter()
                        .position(|o| o.updated < m.updated)
                        .unwrap_or(env.sessions.len());
                    env.sessions.insert(at, s);
                    headless::set_shown(&m.id, true);
                    changed = true;
                    env_path
                }
            };
            let Some(s) = self.session_mut(&env_path, &m.id) else {
                continue;
            };
            let title: SharedString = m.title.clone().into();
            if s.status != app_status(status) || s.updated != m.updated || s.title != title {
                s.status = app_status(status);
                s.updated = m.updated;
                s.title = title;
                changed = true;
            }
            let Some(thread) = s.live.as_ref().map(|l| l.thread.clone()) else {
                continue;
            };
            let seen = SEEN.with(|seen| seen.borrow().get(&m.id).copied());
            if seen == Some((time, status)) {
                continue;
            }
            // Not read: the viewer opened after the scan began, or reading
            // failed. The next scan tries again.
            let Some(t) = transcript else {
                continue;
            };
            SEEN.with(|seen| seen.borrow_mut().insert(m.id.clone(), (time, status)));
            let shown = headless::thread_status(&m);
            thread.update(cx, |th, cx| th.show_transcript(t, shown, cx));
        }
        if changed {
            cx.notify();
        }
    }

    /// The Headless project, for sessions in no project's folder.
    fn headless_env(&mut self) -> PathBuf {
        let root = headless::root();
        if !self.projects.iter().any(|p| p.path == root) {
            let mut p = state::new_project(&root);
            p.name = "Headless".into();
            p.environments[0].set_branch(Some("agents".into()));
            self.projects.push(p);
            // The first project: the row gets the tab it must never be without.
            if self.strip.tabs.is_empty() {
                self.show_env_tab(true);
                self.strip.seed_tab_history();
            }
        }
        root
    }

    /// A read-only view of a headless session's transcript.
    pub(super) fn view_headless(
        &mut self,
        env_path: &Path,
        id: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(m) = headless::read_meta(id) else {
            return;
        };
        let t = headless::read_transcript(id).unwrap_or_else(|| agent::Transcript {
            version: 1,
            meta: session_meta(&m),
            entries: Vec::new(),
            usage: agent::Usage::default(),
            fork_point: None,
            interrupted: false,
            resume: false,
            queue: Vec::new(),
            handoff: None,
            subagents: Vec::new(),
        });
        SEEN.with(|seen| {
            seen.borrow_mut()
                .insert(id.to_string(), (modified(id), headless::status(&m)))
        });
        let thread = Thread::viewer(t, headless::thread_status(&m), cx);
        self.attach(env_path, id, thread, window, cx);
    }

    /// What the user writes to a headless session goes to it, and it goes on
    /// headless: in its sandbox, with its policy and caps and `request_action`,
    /// never as the app's own unsandboxed session. Always `false`: the app
    /// doesn't take it over.
    pub(super) fn adopt_headless(
        &mut self,
        env_path: &Path,
        id: &str,
        text: &str,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let notice = match headless::read_meta(id) {
            Some(m) => match headless::continue_with(&m, text) {
                Ok(()) => (
                    format!(
                        "Sent to the headless agent, which goes on in its sandbox ({}). Its replies show here; approve its requests with `arakun agent pending`.",
                        m.confinement()
                    ),
                    false,
                ),
                Err(e) => (
                    format!("Could not send it to the headless agent: {e}"),
                    true,
                ),
            },
            None => ("This headless session is gone.".to_string(), true),
        };
        if let Some(t) = self
            .session_mut(env_path, id)
            .and_then(|s| s.live.as_ref().map(|l| l.thread.clone()))
        {
            t.update(cx, |t, cx| t.post_notice(notice.0, notice.1, cx));
        }
        false
    }

    /// A headless session whose folder is gone leaves the list.
    fn drop_headless(
        &mut self,
        env_path: &Path,
        id: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        while let Some(i) = self.strip.session_tab(id) {
            self.discard_tab(i, window, cx);
        }
        let Some(env) = self.find_env_mut(env_path) else {
            return;
        };
        headless::set_shown(id, false);
        SEEN.with(|seen| seen.borrow_mut().remove(id));
        if let Some(i) = env.session_index(id) {
            let s = env.sessions.remove(i);
            if let Some(live) = s.live {
                super::sessions::report_failure(&live.thread, true, cx);
                live.thread.update(cx, |t, _| t.shutdown());
            }
        }
    }
}
