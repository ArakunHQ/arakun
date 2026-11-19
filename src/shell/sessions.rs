//! Agent sessions: starting, reviving and retiring the ACP threads behind the
//! session tabs. An agent process only runs while its session is open in a tab
//! or still busy; everything else lives in its saved transcript.

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};

use gpui::{App, AppContext as _, Context, Entity, SharedString, Window};

use super::Shell;
use crate::agent::catalog;
use crate::agent::{
    AgentKind, FailedAt, NativeSettings, Setting, SettingOption, Thread, ThreadEvent, ThreadStatus,
    ThreadView, ThreadViewEvent,
};
use crate::model::{self, LiveSession, Session, Status, Tab, TabKind};
use crate::state;

/// Sends an agent's error to crash reports with where it failed. The folder
/// check tells a deleted project apart from a broken agent.
fn report_agent_error(t: &Thread, at: FailedAt, msg: &str) {
    use serde_json::Value;
    let mut props = vec![
        ("agent", Value::from(t.kind().id())),
        ("cwd_exists", t.cwd().is_dir().into()),
    ];
    if let Some(version) = t.agent_title().and_then(agent_version) {
        props.push(("agent_version", version.into()));
    }
    props.push(("stage", at.stage().into()));
    if let Some(code) = at.code() {
        props.push(("code", code.into()));
    }
    crate::telemetry::error("AgentError", msg, &props);
}

/// Sends `thread`'s failure if crash reports have not had it yet. `closing`
/// sends a disconnect that still waits for its exit, which a thread about to
/// shut down will never hear.
pub(super) fn report_failure(thread: &Entity<Thread>, closing: bool, cx: &mut App) {
    let Some(at) = thread.update(cx, |t, _| t.take_failure_report(closing)) else {
        return;
    };
    if let ThreadStatus::Error(msg) = thread.read(cx).status() {
        report_agent_error(thread.read(cx), at, &msg);
    }
}

/// The version at the end of an agent's title ("Factory Droid 0.240.1"), and
/// nothing else of it: the title is the agent's own text.
fn agent_version(title: &str) -> Option<&str> {
    let v = title.rsplit(' ').next()?.trim_start_matches('v');
    (v.starts_with(|c: char| c.is_ascii_digit())
        && v.len() <= 32
        && v.chars()
            .all(|c| c.is_ascii_alphanumeric() || ".-+".contains(c)))
    .then_some(v)
}

pub fn status_of(t: &Thread) -> Status {
    match t.status() {
        ThreadStatus::NeedsInput => Status::NeedsInput,
        // Waits for the user to agree to install its adapter, not broken.
        ThreadStatus::Error(_) if t.needs_install().is_some() => Status::NeedsInput,
        ThreadStatus::Working | ThreadStatus::Connecting if t.is_busy() => Status::Working,
        ThreadStatus::Working => Status::Working,
        ThreadStatus::Error(_) => Status::Error,
        ThreadStatus::Connecting | ThreadStatus::Idle => Status::Idle,
    }
}

/// How much of the conversation an agent taking it over gets inline; the rest
/// it reads from the linked transcript.
const HANDOFF_BUDGET: usize = 16 * 1024;

fn new_session_id() -> SharedString {
    static COUNTER: AtomicU32 = AtomicU32::new(0);
    let millis = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis());
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    format!("{millis:x}-{n}").into()
}

pub(super) fn summary(text: &str) -> SharedString {
    let text = crate::agent::attach::title_text(text);
    let line = text
        .lines()
        .find(|l| !l.trim().is_empty())
        .unwrap_or("")
        .trim();
    match line.char_indices().nth(80) {
        Some((i, _)) => format!("{}…", &line[..i]).into(),
        None => line.to_string().into(),
    }
}

impl Shell {
    pub(super) fn session_mut(&mut self, env_path: &Path, id: &str) -> Option<&mut Session> {
        let env = self.find_env_mut(env_path)?;
        let i = env.session_index(id)?;
        Some(&mut env.sessions[i])
    }

    pub(super) fn active_session_id(&self) -> Option<SharedString> {
        if !self.has_projects() {
            return None;
        }
        match self.strip.active().map(|t| &t.kind) {
            Some(TabKind::Session { id, .. }) => Some(id.clone()),
            _ => None,
        }
    }

    pub(super) fn active_thread(&self, _cx: &gpui::App) -> Option<Entity<Thread>> {
        let id = self.active_session_id()?;
        let env = self.env();
        env.sessions[env.session_index(&id)?]
            .live
            .as_ref()
            .map(|l| l.thread.clone())
    }

    pub(super) fn active_thread_view(&self) -> Option<Entity<crate::agent::ThreadView>> {
        let id = self.active_session_id()?;
        let env = self.env();
        env.sessions[env.session_index(&id)?]
            .live
            .as_ref()
            .map(|l| l.view.clone())
    }

    /// Makes sure the sessions on screen (active tab and other pane) have their agent running.
    pub(super) fn ensure_active_live(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.has_projects() {
            return;
        }
        let ids: Vec<(std::path::PathBuf, SharedString)> = self
            .visible_tabs()
            .into_iter()
            .filter_map(|i| {
                let tab = self.strip.tabs.get(i)?;
                match &tab.kind {
                    TabKind::Session { id, .. } => Some((tab.env.clone()?, id.clone())),
                    _ => None,
                }
            })
            .collect();
        for (path, id) in &ids {
            self.ensure_live(path, id, window, cx);
        }
        // A server sends every change of one session: the one in front.
        if self.env().remote.is_some()
            && let Some(id) = self.active_session_id()
        {
            let path = self.env().path.clone();
            self.servers_follow(&path, &id, super::servers::Watcher::Desktop, cx);
        }
    }

    pub(super) fn ensure_live(
        &mut self,
        env_path: &Path,
        id: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let remote = self.find_env(env_path).is_some_and(|e| e.remote.is_some());
        let Some(session) = self.session_mut(env_path, id) else {
            return;
        };
        if session.live.is_some() {
            return;
        }
        if remote {
            return self.view_remote(env_path, id, window, cx);
        }
        if crate::headless::shown(id) {
            return self.view_headless(env_path, id, window, cx);
        }
        let (kind, meta, title) = (
            session.agent.kind(),
            session.meta.clone(),
            session.title.clone(),
        );
        let transcript = state::transcript_path(id);
        let thread = transcript
            .exists()
            .then(|| Thread::restore(&transcript, cx))
            .and_then(|r| {
                r.map_err(|e| eprintln!("arakun: transcript {id}: {e:#}"))
                    .ok()
            })
            .unwrap_or_else(|| Thread::start(kind, env_path.to_path_buf(), meta, cx));
        // A rename made while the agent was not running reaches it now.
        if !title.is_empty() && thread.read(cx).title() != title {
            thread.update(cx, |t, cx| t.rename(&title, cx));
        }
        // Counts saved by older versions may be off; the transcript has the truth.
        let t = thread.read(cx);
        if !t.entries().is_empty() {
            let (added, removed) = t.diff_stats();
            if let Some(s) = self.session_mut(env_path, id) {
                (s.added, s.removed) = (added, removed);
            }
        }
        self.attach(env_path, id, thread, window, cx);
    }

    pub(super) fn attach(
        &mut self,
        env_path: &Path,
        id: &str,
        thread: Entity<Thread>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let view = cx.new(|cx| ThreadView::new(thread.clone(), window, cx));
        let (path, sid): (PathBuf, SharedString) = (env_path.to_path_buf(), id.to_string().into());
        let events = cx.subscribe_in(&thread, window, move |this, thread, ev, window, cx| {
            this.thread_event(&path, &sid, thread, ev, window, cx)
        });
        let env = env_path.to_path_buf();
        let sid_for_notes: SharedString = id.to_string().into();
        let view_events = cx.subscribe_in(
            &view,
            window,
            move |this, _, ev: &ThreadViewEvent, window, cx| match ev {
                ThreadViewEvent::OpenFile(file, line) => {
                    this.open_file_beside(&env, file, *line, window, cx)
                }
                ThreadViewEvent::OpenSubagent(subagent) => {
                    this.open_subagent(sid_for_notes.clone(), subagent.clone(), window, cx)
                }
                ThreadViewEvent::AddNote {
                    surface,
                    range,
                    quote,
                    note,
                } => {
                    let n = crate::agent::notes::ChatNote {
                        kind: crate::agent::notes::NoteKind::Annotation,
                        quote: quote.clone(),
                        note: note.clone(),
                        path: None,
                        lines: None,
                        mark: Some((sid_for_notes.to_string(), *surface, range.clone())),
                    };
                    this.add_note(&env, n, window, cx);
                }
            },
        );
        if let Some(session) = self.session_mut(env_path, id) {
            // Each new agent process of the session answers its permissions the same way.
            if session.auto_allow {
                thread.update(cx, |t, _| t.set_auto_allow(true));
            }
            session.status = status_of(thread.read(cx));
            session.live = Some(LiveSession {
                thread,
                view,
                _events: events,
                _view_events: view_events,
            });
        }
        cx.notify();
    }

    /// Shows a file beside the chat in `env_path`, at `line`; files arakun
    /// cannot show open in their default app.
    pub(super) fn open_file_beside(
        &mut self,
        env_path: &Path,
        file: &Path,
        line: Option<usize>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if crate::explorer::file_kind(file) == crate::explorer::FileKind::External {
            cx.open_with_system(file);
            return;
        }
        let kind = TabKind::File {
            path: file.to_path_buf(),
        };
        if let Some(id) = self.open_doc_in(env_path, kind, false, cx) {
            if let Some(line) = line {
                self.reveal_line(env_path, id, line, cx);
            }
            self.show_beside(env_path, id, window, cx);
        }
    }

    fn thread_event(
        &mut self,
        env_path: &Path,
        id: &SharedString,
        thread: &Entity<Thread>,
        ev: &ThreadEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // A server's session: what the user did goes there; the rest (checks,
        // transcripts, the phone) is the server's to do.
        let pid = self.phone_id(env_path, id);
        self.phone_thread_event(&pid, ev, cx);
        if self.find_env(env_path).is_some_and(|e| e.remote.is_some()) {
            if let ThreadEvent::Remote(action) = ev {
                self.servers_remote_action(env_path, id, action, cx);
            }
            return;
        }
        // A turn whose check is about to run is announced once the check is done.
        self.speak_question(env_path, id, thread, cx);
        let checking = matches!(ev, ThreadEvent::Finished { stop_reason }
            if self.check_for_turn(env_path, thread, stop_reason, cx).is_some());
        if !checking {
            self.notify_desktop(env_path, id, thread, ev, window, cx);
        }
        match ev {
            ThreadEvent::Forked(meta) => {
                let meta = meta.clone();
                self.add_forked_session(env_path, meta, window, cx);
            }
            ThreadEvent::ShellChanges => {
                let stats = thread.read(cx).diff_stats();
                if let Some(s) = self.session_mut(env_path, id) {
                    (s.added, s.removed) = stats;
                }
                self.save_transcript(env_path, id, cx);
                self.save(cx);
                cx.notify();
            }
            ThreadEvent::CheckOffer { command, answer } => {
                self.check_offer_answered(
                    env_path,
                    id,
                    thread,
                    command.clone(),
                    *answer,
                    window,
                    cx,
                );
            }
            ThreadEvent::SubagentsChanged { spawned } => {
                if spawned.is_some() {
                    self.subagent_spawned(id, cx);
                }
                // The background work is done: keep what it did, and let the agent go if hidden.
                if !thread.read(cx).in_background() {
                    self.save_transcript(env_path, id, cx);
                    self.retire_if_hidden(env_path, id, cx);
                }
                cx.notify();
            }
            ThreadEvent::LoginRequested => {
                let kind = thread.read(cx).kind();
                self.sign_in(kind, window, cx);
            }
            ThreadEvent::Rewound { meta, prompt } => {
                let meta = meta.clone();
                self.add_forked_session(env_path, meta, window, cx);
                if let Some(env) = self.find_env_mut(env_path) {
                    env.draft.set(prompt.clone());
                }
                window.focus(&self.composer_focus, cx);
                self.save(cx);
                cx.notify();
            }
            ThreadEvent::TitleChanged => {
                let title = thread.read(cx).title();
                if title.is_empty() {
                    return;
                }
                if let Some(env) = self.find_env_mut(env_path)
                    && let Some(i) = env.session_index(id)
                {
                    env.sessions[i].title = title.clone();
                }
                if let Some(i) = self.strip.session_tab(id) {
                    self.strip.tabs[i].title = title.clone();
                }
                self.save(cx);
                cx.notify();
            }
            ThreadEvent::StatusChanged | ThreadEvent::NeedsPermission => {
                let status = status_of(thread.read(cx));
                if status == Status::Error {
                    report_failure(thread, false, cx);
                }
                if let Some(s) = self.session_mut(env_path, id)
                    && s.status != status
                {
                    s.status = status;
                    s.updated = model::unix_now();
                    cx.notify();
                }
            }
            ThreadEvent::Finished { stop_reason } => {
                let t = thread.read(cx);
                let seconds = t.last_turn_ms().map(|ms| ("seconds", ms / 1000));
                crate::telemetry::capture_with(
                    crate::telemetry::events::TURN_FINISHED,
                    &[("agent", t.kind().id()), ("stop_reason", stop_reason)],
                    seconds.as_slice(),
                );
                let (status, (added, removed), meta) = (status_of(t), t.diff_stats(), t.meta());
                let transcript = t.transcript(cx);
                let mut turns: Vec<crate::memory::Turn> = Vec::new();
                let mut since = None;
                for e in t.entries() {
                    match e {
                        crate::agent::Entry::User { text, at, .. } => {
                            since.get_or_insert(*at);
                            turns.push(crate::memory::Turn {
                                asked: text.clone(),
                                reply: String::new(),
                            });
                        }
                        crate::agent::Entry::Agent { text } if !text.trim().is_empty() => {
                            if let Some(turn) = turns.last_mut() {
                                turn.reply = text.clone();
                            }
                        }
                        _ => {}
                    }
                }
                let since = since.unwrap_or_else(model::unix_now);
                let record = (
                    t.cwd().to_path_buf(),
                    t.title().to_string(),
                    turns,
                    t.edited_files(),
                    since,
                );
                if let Some(s) = self.session_mut(env_path, id) {
                    s.status = status;
                    s.added = added;
                    s.removed = removed;
                    s.meta = Some(meta);
                    s.updated = model::unix_now();
                }
                let path = state::transcript_path(id);
                let key = id.to_string();
                cx.background_spawn(crate::guard::task(async move {
                    if let Err(e) = write_transcript(&path, &transcript) {
                        eprintln!("arakun: could not save {}: {e}", path.display());
                    }
                    let (cwd, title, turns, files, since) = record;
                    if let Err(e) =
                        crate::memory::record_session(&cwd, &key, &title, &turns, &files, since)
                    {
                        eprintln!("arakun: could not remember the session: {e}");
                    }
                }))
                .detach();
                self.save(cx);
                self.check_after_turn(env_path, id, thread, stop_reason, window, cx);
                // The island keeps how the turn ended, for after the thread is retired.
                self.feed_island(window, cx);
                self.retire_if_hidden(env_path, id, cx);
                // Edits show up in Changes without waiting for the next poll.
                self.refresh_status(env_path, cx);
                if thread.read(cx).kind() == AgentKind::Droid {
                    self.account.update(cx, |a, cx| {
                        a.refresh_if_stale(std::time::Duration::from_secs(30), cx)
                    });
                }
                cx.notify();
            }
            _ => {}
        }
    }

    /// Stops the agent of a session nobody is looking at once it has nothing to do.
    pub(super) fn retire_if_hidden(&mut self, env_path: &Path, id: &str, cx: &mut Context<Self>) {
        let open = self.strip.session_tab(id).is_some();
        let Some(env) = self.find_env_mut(env_path) else {
            return;
        };
        let Some(i) = env.session_index(id) else {
            return;
        };
        let busy = matches!(env.sessions[i].status, Status::Working | Status::NeedsInput)
            || env.sessions[i]
                .live
                .as_ref()
                .is_some_and(|l| l.thread.read(cx).in_background());
        if open || busy || self.check_running(id) || self.phone_watching(env_path, id) {
            return;
        }
        let Some(env) = self.find_env_mut(env_path) else {
            return;
        };
        let remote = env.remote.as_ref().map(|r| r.server.clone());
        let session = &mut env.sessions[i];
        if let Some(live) = session.live.take() {
            // A server's transcript stays on the server.
            if remote.is_none() {
                let t = live.thread.read(cx);
                let transcript = t.transcript(cx);
                session.meta = Some(t.meta());
                let path = state::transcript_path(id);
                cx.background_spawn(crate::guard::task(async move {
                    let _ = write_transcript(&path, &transcript);
                }))
                .detach();
            }
            report_failure(&live.thread, true, cx);
            live.thread.update(cx, |t, _| t.shutdown());
            if let Some(server) = remote {
                self.servers_unfollow(&server, id);
            }
        }
    }

    /// Sends the composer text: to the open session, or as the first prompt of a new one.
    pub(super) fn send_prompt(
        &mut self,
        text: String,
        now: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // On a server: the session runs there, and nothing here is renamed,
        // staged or checked for it.
        if self.env().remote.is_some() {
            let path = self.env().path.clone();
            let agent = self.env().agent;
            // `/stage` runs git here, on files that are on the server.
            if let Some(id) = self.active_session_id()
                && text.trim() == "/stage"
            {
                self.ensure_live(&path, &id, window, cx);
                if let Some(thread) = self.active_thread(cx) {
                    thread.update(cx, |t, cx| {
                        t.post_notice(
                            "/stage works on this machine's files; on the server, stage in its terminal.",
                            true,
                            cx,
                        )
                    });
                }
                cx.notify();
                return;
            }
            // The rename note goes to the agent only; it stays for the next
            // prompt when this one does not go.
            let note = self.rename_notes.remove(&path);
            let prompt = match &note {
                Some(note) => format!("{text}{}", crate::agent::chart::hint(note)),
                None => text.clone(),
            };
            // A remote thread hands the prompt on without saying whether it
            // went: whether it can go is asked first.
            let reachable = self.env().remote.as_ref().is_some_and(|r| {
                self.server_reach(&r.server) == Some(crate::servers_view::Reach::Connected)
            });
            let sent = match self.active_session_id() {
                Some(id) => {
                    self.ensure_live(&path, &id, window, cx);
                    let thread = self.active_thread(cx);
                    if let Some(thread) = &thread {
                        thread.update(cx, |t, cx| {
                            if reachable {
                                t.send(prompt, cx);
                            } else {
                                t.post_notice(
                                    "The server is not connected, so this did not reach it.",
                                    true,
                                    cx,
                                );
                            }
                        });
                    }
                    reachable && thread.is_some()
                }
                None => {
                    let sent = self.servers_start(
                        &path,
                        agent,
                        &prompt,
                        &[],
                        super::servers::Asker::Desktop,
                    );
                    // Until the server answers: it names the branch once the
                    // session started, and comes back here if it did not.
                    if let Some(reference) = sent {
                        let first = super::servers::FirstPrompt {
                            env: path.clone(),
                            text: text.clone(),
                            agent: agent.kind(),
                            note: note.clone(),
                            desktop: true,
                        };
                        self.servers_first_prompt(reference, first);
                        self.composer_changed(cx);
                        cx.notify();
                        return;
                    }
                    let why = self
                        .servers_refusal()
                        .unwrap_or_else(|| "The server cannot be reached.".into());
                    self.notify(
                        crate::island::AlertKind::Warning,
                        "The prompt did not go",
                        &why,
                        None,
                        cx,
                    );
                    false
                }
            };
            if sent {
                // The branch is named after the first prompt that went, there
                // as here, by the agent of the session it went to.
                let session_agent = self.active_session_id().and_then(|id| {
                    let env = self.find_env(&path)?;
                    Some(env.sessions[env.session_index(&id)?].agent)
                });
                let agent = session_agent.unwrap_or(agent);
                self.name_after_first_prompt(&path, &text, agent.kind(), cx);
            } else {
                // Not sent: back into the composer, as the user wrote it.
                self.env_mut().draft.set(&text);
                self.composer_changed(cx);
                if let Some(note) = note {
                    self.rename_notes.insert(path, note);
                }
            }
            cx.notify();
            return;
        }
        if let Some(id) = self.active_session_id() {
            let path = self.env().path.clone();
            let agent = self
                .find_env(&path)
                .and_then(|e| e.sessions.iter().find(|s| s.id == id))
                .map_or(self.env().agent, |s| s.agent);
            self.name_after_first_prompt(&path, &text, agent.kind(), cx);
            if text.trim() == "/stage" {
                self.stage_session(&path, &id, window, cx, |this, staged, path, id, _, cx| {
                    let text = match staged {
                        Ok(files) if files.is_empty() => (
                            "Nothing this session changed is left to stage.".to_string(),
                            false,
                        ),
                        Ok(files) => (
                            format!(
                                "Staged {} {} this session changed: {}",
                                files.len(),
                                if files.len() == 1 { "file" } else { "files" },
                                files.join(", ")
                            ),
                            false,
                        ),
                        Err(e) => (format!("Staging failed: {e}"), true),
                    };
                    if let Some(thread) = this
                        .session_mut(&path, &id)
                        .and_then(|s| s.live.as_ref().map(|l| l.thread.clone()))
                    {
                        thread.update(cx, |t, cx| t.post_notice(text.0, text.1, cx));
                    }
                });
                return;
            }
            // This session started on the branch's old name.
            let note = self.rename_notes.remove(&path);
            if crate::headless::shown(&id) {
                let text = match &note {
                    Some(note) => format!("{text}{}", crate::agent::chart::hint(note)),
                    None => text.clone(),
                };
                if !self.adopt_headless(&path, &id, &text, window, cx) {
                    return;
                }
            }
            self.ensure_live(&path, &id, window, cx);
            self.user_took_over(&id);
            if let Some(thread) = self.active_thread(cx) {
                thread.update(cx, |t, cx| {
                    if let Some(note) = note {
                        t.add_note(note);
                    }
                    if now {
                        t.interrupt_with(text, cx)
                    } else {
                        t.send(text, cx)
                    }
                });
            }
            if let Some(s) = self.session_mut(&path, &id) {
                s.updated = model::unix_now();
            }
            cx.notify();
            return;
        }

        let (env_path, agent) = (self.env().path.clone(), self.env().agent);
        self.start_session_in(&env_path, agent, summary(&text), text, true, window, cx);
    }

    /// Stages, with `git add`, only the files session `id` changed, then calls
    /// `then` with the paths staged (relative to the environment).
    pub(super) fn stage_session(
        &mut self,
        env_path: &Path,
        id: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
        then: impl FnOnce(
            &mut Self,
            Result<Vec<String>, String>,
            PathBuf,
            SharedString,
            &mut Window,
            &mut Context<Self>,
        ) + 'static,
    ) {
        self.ensure_live(env_path, id, window, cx);
        let files: Vec<String> = self
            .session_mut(env_path, id)
            .and_then(|s| s.live.as_ref().map(|l| l.thread.read(cx).edited_files()))
            .unwrap_or_default()
            .into_iter()
            .filter_map(|p| {
                Some(
                    p.strip_prefix(env_path)
                        .ok()?
                        .to_string_lossy()
                        .into_owned(),
                )
            })
            .filter(|p| !p.is_empty())
            .collect();
        let (dir, id) = (env_path.to_path_buf(), SharedString::from(id.to_string()));
        cx.spawn_in(window, async move |this, cx| {
            let d = dir.clone();
            let staged = cx
                .background_spawn(crate::guard::task(async move {
                    crate::git::stage_only(&d, &files).map_err(|e| e.to_string())
                }))
                .await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.refresh_status(&dir, cx);
                then(this, staged, dir.clone(), id, window, cx);
                cx.notify();
            });
        })
        .detach();
    }

    /// Sends `text` to a session of any environment, starting its agent if it isn't running.
    pub(super) fn send_to_session(
        &mut self,
        env_path: &Path,
        id: &str,
        text: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        self.ensure_live(env_path, id, window, cx);
        let Some(s) = self.session_mut(env_path, id) else {
            return false;
        };
        let Some(thread) = s.live.as_ref().map(|l| l.thread.clone()) else {
            return false;
        };
        s.updated = model::unix_now();
        s.status = Status::Working;
        thread.update(cx, |t, cx| t.send(text, cx));
        self.save(cx);
        cx.notify();
        true
    }

    /// Puts a session's tab in the strip: in place of its environment's blank
    /// New session tab when that one is in front there, else after its other
    /// tabs. It comes to the front when the environment is `visible`; otherwise
    /// it is what the environment shows the next time.
    fn open_session_tab(
        &mut self,
        env_path: &Path,
        tab: Tab,
        visible: bool,
        cx: &mut Context<Self>,
    ) {
        let id = tab.id;
        let strip = &mut self.strip;
        let front = strip.front_of(env_path);
        let index = match front.filter(|i| matches!(strip.tabs[*i].kind, TabKind::NewSession)) {
            Some(i) => {
                strip.tabs[i] = tab;
                i
            }
            None => strip.add(tab),
        };
        if visible {
            strip.active_tab = index;
            self.note_active_tab(cx);
        } else {
            strip.front.insert(env_path.to_path_buf(), id);
        }
    }

    /// Starts a session in any environment with `text` as its first prompt; `show`
    /// opens it in a tab (the active one of that environment).
    pub(super) fn start_session_in(
        &mut self,
        env_path: &Path,
        agent: model::Agent,
        title: SharedString,
        text: String,
        show: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<SharedString> {
        self.start_session_with(env_path, agent, title, text, show, Vec::new(), window, cx)
    }

    /// [`Self::start_session_in`], with `extra` settings over the ones the
    /// environment remembers, for this session only.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn start_session_with(
        &mut self,
        env_path: &Path,
        agent: model::Agent,
        title: SharedString,
        text: String,
        show: bool,
        extra: Vec<(Setting, String)>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<SharedString> {
        let visible = self.has_projects() && self.env().path == env_path;
        let id = new_session_id();
        let env = self.find_env_mut(env_path)?;
        let mut session = Session::new(id.clone(), title.clone(), agent);
        session.status = Status::Working;
        env.sessions.insert(0, session);
        if show {
            let tab = Tab::of(
                env_path,
                title,
                TabKind::Session {
                    agent,
                    id: id.clone(),
                },
            );
            self.open_session_tab(env_path, tab, visible, cx);
        }
        crate::telemetry::capture(
            crate::telemetry::events::SESSION_STARTED,
            &[("agent", agent.key())],
        );
        let env_path = env_path.to_path_buf();
        // Every way a session starts (the composer, automations, `arakun agent new`) passes here.
        self.name_after_first_prompt(&env_path, &text, agent.kind(), cx);
        let thread = Thread::start(agent.kind(), env_path.clone(), None, cx);
        self.apply_env_settings(&env_path, agent, extra, &thread, cx);
        thread.update(cx, |t, cx| t.send(text, cx));
        self.attach(&env_path, &id, thread, window, cx);
        self.save(cx);
        cx.notify();
        Some(id)
    }

    /// Gives a new session of `agent` the settings its environment remembers,
    /// with `extra` over them.
    fn apply_env_settings(
        &self,
        env_path: &Path,
        agent: model::Agent,
        extra: Vec<(Setting, String)>,
        thread: &Entity<Thread>,
        cx: &mut Context<Self>,
    ) {
        let settings = (agent == model::Agent::Droid)
            .then(|| self.draft_settings_for(env_path))
            .flatten();
        let mut claude_settings = match (agent, self.find_env(env_path)) {
            (model::Agent::Claude, Some(env)) => env.claude_settings.clone(),
            (model::Agent::OpenCode, Some(env)) => opencode_draft(env),
            (model::Agent::Codex, Some(env)) => codex_draft(env),
            (model::Agent::Devin, Some(env)) => devin_draft(env),
            (model::Agent::WorkersAi, Some(env)) => env.workers_ai_settings.clone(),
            _ => Vec::new(),
        };
        if !extra.is_empty() {
            for (s, v) in extra {
                // The effort remembered went with the model remembered.
                claude_settings
                    .retain(|(d, _)| *d != s && !(s == Setting::Model && *d == Setting::Effort));
                claude_settings.push((s, v));
            }
        }
        // The model first, also with nothing picked: an agent checks a
        // reasoning level against the model it is on, and would skip one that
        // only the remembered model has.
        claude_settings.sort_by_key(|(s, _)| *s != Setting::Model);
        thread.update(cx, |t, cx| {
            // Queued until the session exists, then sent with `initialize_session`
            // (Droid) or one by one before the first prompt (the ACP agents).
            if let Some(settings) = &settings {
                for s in Setting::ALL {
                    let value = settings.get(s);
                    if !value.is_empty() {
                        t.set_setting(s, value, cx);
                    }
                }
            }
            for (s, value) in &claude_settings {
                t.set_setting(*s, value, cx);
            }
        });
    }

    /// Hands the composer's session to `agent`, in the same tab and checkout: a
    /// session of `agent` takes its place, showing the conversation so far and
    /// told about it with the next prompt; the old agent stops.
    pub(super) fn continue_with(
        &mut self,
        agent: model::Agent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let env_path = self.env().path.clone();
        let Some(TabKind::Session {
            agent: from,
            id: old_id,
        }) = self
            .strip
            .tabs
            .get(self.composer_tab())
            .map(|t| t.kind.clone())
        else {
            return;
        };
        // A server's session goes on with the agent it has there.
        if from == agent || crate::headless::shown(&old_id) || self.env().remote.is_some() {
            return;
        }
        let entries = self.session_entries(&env_path, &old_id, cx);
        let Some(ix) = self.env().session_index(&old_id) else {
            return;
        };
        let old = self.env_mut().sessions.remove(ix);
        let mut agent_session = old.meta.as_ref().and_then(|m| m.session_id.clone());
        if let Some(live) = &old.live {
            let t = live.thread.read(cx);
            agent_session = t.meta().session_id.or(agent_session);
            if let Err(e) = write_transcript(&state::transcript_path(&old_id), &t.transcript(cx)) {
                eprintln!("arakun: could not save {old_id}: {e}");
            }
            report_failure(&live.thread, true, cx);
            live.thread.update(cx, |t, _| t.shutdown());
        }
        forget_alerts(&old_id);

        let id = new_session_id();
        let mut session = Session::new(id.clone(), old.title.clone(), agent);
        (session.added, session.removed) = (old.added, old.removed);
        session.ci_run = old.ci_run;
        #[cfg(feature = "coderlabs")]
        {
            session.task = old.task.clone();
        }
        session.continues = old.continues.clone();
        let talked = entries
            .iter()
            .any(|e| matches!(e, crate::agent::Entry::User { .. }));
        let thread = if talked {
            session.continues.push(model::Earlier {
                id: old_id.to_string(),
                agent_session,
            });
            let linked = state::data_dir()
                .join("linked")
                .join(format!("{old_id}.md"));
            let markdown =
                crate::agent::notes::transcript_markdown(&old.title, from.label(), &entries);
            if let Err(e) = std::fs::create_dir_all(state::data_dir().join("linked"))
                .and_then(|_| std::fs::write(&linked, markdown))
            {
                eprintln!("arakun: could not write {}: {e}", linked.display());
            }
            let handoff = crate::agent::notes::handoff(
                from.label(),
                &old.title,
                &entries,
                &linked.to_string_lossy(),
                HANDOFF_BUDGET,
            );
            Thread::continued(
                agent.kind(),
                env_path.clone(),
                old.title.to_string(),
                entries,
                handoff,
                cx,
            )
        } else {
            // Nothing was said yet: a fresh session of `agent` and nothing to keep.
            let path = state::transcript_path(&old_id);
            cx.background_spawn(crate::guard::task(async move {
                let _ = std::fs::remove_file(path);
            }))
            .detach();
            Thread::start(agent.kind(), env_path.clone(), None, cx)
        };
        drop(old);
        let env = self.env_mut();
        env.sessions.insert(ix, session);
        for tab in &mut self.strip.tabs {
            if matches!(&tab.kind, TabKind::Session { id: tid, .. } if *tid == old_id) {
                tab.kind = TabKind::Session {
                    agent,
                    id: id.clone(),
                };
            }
        }
        self.apply_env_settings(&env_path, agent, Vec::new(), &thread, cx);
        self.attach(&env_path, &id, thread, window, cx);
        self.save_transcript(&env_path, &id, cx);
        self.note_active_tab(cx);
        self.save(cx);
        cx.notify();
    }

    /// Opens a session from the list in a tab (reusing its tab if it has one).
    pub(super) fn open_session(
        &mut self,
        id: SharedString,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let env = self.env_mut();
        let Some(i) = env.session_index(&id) else {
            return;
        };
        let s = &env.sessions[i];
        let tab = Tab::of(
            &env.path,
            s.title.clone(),
            TabKind::Session {
                agent: s.agent,
                id: id.clone(),
            },
        );
        let strip = &mut self.strip;
        strip.active_tab = match strip.session_tab(&id) {
            Some(t) => t,
            None => {
                // The environment's only tab, a blank one, turns into the session.
                let mut own = strip
                    .tabs
                    .iter()
                    .enumerate()
                    .filter(|(_, t)| t.env == tab.env);
                match (own.next(), own.next()) {
                    (Some((only, t)), None) if matches!(t.kind, TabKind::NewSession) => {
                        strip.tabs[only] = tab;
                        only
                    }
                    _ => strip.add(tab),
                }
            }
        };
        self.note_active_tab(cx);
        self.ensure_active_live(window, cx);
        self.save(cx);
        cx.notify();
    }

    pub(super) fn cancel_active(&mut self, cx: &mut Context<Self>) -> bool {
        let Some(thread) = self.active_thread(cx) else {
            return false;
        };
        let busy = thread.read(cx).is_busy();
        if busy {
            thread.update(cx, |t, cx| t.cancel(cx));
        }
        busy
    }

    // ---- settings before the first message ---------------------------------

    /// Loads Droid's defaults and model list off the main thread; the list is
    /// cached, so Droid is only asked when the cache is missing or old.
    /// Claude's, OpenCode's, Codex's and Devin's options are cached the same way; the agent
    /// is only asked when none of its sessions has reported them yet.
    pub(super) fn warm_droid_catalog(&mut self, cx: &mut Context<Self>) {
        let droid = catalog::droid_native();
        cx.spawn(async move |this, cx| {
            if droid {
                cx.background_spawn(crate::guard::task_or_default(async { catalog::refresh() }))
                    .await;
                let _ = this.update(cx, |_, cx| cx.notify());
            }
            cx.background_spawn(crate::guard::task(async { catalog::refresh_claude() }))
                .await;
            let _ = this.update(cx, |_, cx| cx.notify());
            cx.background_spawn(crate::guard::task(async { catalog::refresh_opencode() }))
                .await;
            let _ = this.update(cx, |_, cx| cx.notify());
            cx.background_spawn(crate::guard::task(async { catalog::refresh_codex() }))
                .await;
            let _ = this.update(cx, |_, cx| cx.notify());
            cx.background_spawn(crate::guard::task(async { catalog::refresh_devin() }))
                .await;
            let _ = this.update(cx, |_, cx| cx.notify());
        })
        .detach();
    }

    /// The settings the next native Droid session of the visible environment
    /// starts with: Droid's defaults plus what was picked on the New session tab.
    /// Values Droid would reject (an unknown model) are left empty.
    pub(super) fn draft_settings(&self) -> Option<NativeSettings> {
        if !self.has_projects() {
            return None;
        }
        self.draft_settings_for(&self.env().path)
    }

    /// The Droid settings picked on an environment's New session tab, over the defaults.
    pub(super) fn draft_settings_for(&self, env_path: &Path) -> Option<NativeSettings> {
        if !catalog::droid_native() {
            return None;
        }
        let env = self.find_env(env_path)?;
        let catalog = catalog::current();
        let mut settings = catalog.defaults.clone();
        for (s, v) in &env.draft_settings {
            crate::agent::apply_setting(&mut settings, &catalog.models, *s, v);
        }
        if !catalog.models.is_empty() {
            let model = catalog
                .models
                .iter()
                .find(|m| m.id == settings.model_id && m.disabled.is_none());
            match model {
                None => {
                    settings.model_id.clear();
                    settings.reasoning_effort.clear();
                }
                Some(m)
                    if !m.efforts.is_empty() && !m.efforts.contains(&settings.reasoning_effort) =>
                {
                    settings.reasoning_effort = m.default_effort.clone().unwrap_or_default();
                }
                Some(_) => {}
            }
        }
        Some(settings)
    }

    /// The session the composer writes to, when it is running.
    pub(super) fn composer_thread(&self) -> Option<Entity<Thread>> {
        if !self.has_projects() {
            return None;
        }
        let env = self.env();
        let Some(TabKind::Session { id, .. }) =
            self.strip.tabs.get(self.composer_tab()).map(|t| &t.kind)
        else {
            return None;
        };
        env.sessions[env.session_index(id)?]
            .live
            .as_ref()
            .map(|l| l.thread.clone())
    }

    /// The agent of the composer's session, or the one picked for the next session.
    pub(super) fn composer_agent(&self) -> model::Agent {
        let env = self.env();
        match self.strip.tabs.get(self.composer_tab()).map(|t| &t.kind) {
            Some(TabKind::Session { agent, .. }) => *agent,
            _ => env.agent,
        }
    }

    pub(super) fn composer_in_session(&self) -> bool {
        matches!(
            self.strip.tabs.get(self.composer_tab()).map(|t| &t.kind),
            Some(TabKind::Session { .. })
        )
    }

    /// A setting's value and choices: the running session's, or what the next
    /// session of this environment starts with.
    pub(super) fn setting_choices(
        &self,
        s: Setting,
        cx: &gpui::App,
    ) -> Option<(String, Vec<SettingOption>)> {
        if let Some(thread) = self.composer_thread() {
            return thread.read(cx).setting(s).filter(|(_, o)| !o.is_empty());
        }
        if !self.has_projects() || self.composer_in_session() {
            return None;
        }
        self.draft_choices(&self.env().path, self.env().agent, s)
    }

    /// What the next `agent` session of the environment at `env_path` starts with
    /// for a setting, and its choices.
    pub(super) fn draft_choices(
        &self,
        env_path: &Path,
        agent: model::Agent,
        s: Setting,
    ) -> Option<(String, Vec<SettingOption>)> {
        let env = self.find_env(env_path)?;
        match agent {
            model::Agent::Droid => {
                if s == Setting::Fast {
                    return None;
                }
                let settings = self.draft_settings_for(env_path)?;
                let models = &catalog::current().models;
                if s == Setting::Model && models.is_empty() {
                    return None;
                }
                Some(crate::agent::setting_options(&settings, models, s))
            }
            model::Agent::Claude => {
                let options = catalog::claude_options();
                let o = crate::agent::acp_option(&options, s)?;
                let (mut current, choices) = crate::agent::acp_setting_options(o, s);
                if let Some((_, v)) = env.claude_settings.iter().find(|(d, _)| *d == s)
                    && choices.iter().any(|c| c.id == *v)
                {
                    current = v.clone();
                }
                Some((current, choices))
            }
            model::Agent::OpenCode => {
                let options = catalog::opencode_options();
                let draft = opencode_draft(env);
                let picked =
                    |s: Setting| draft.iter().find(|(d, _)| *d == s).map(|(_, v)| v.clone());
                // Reasoning levels are per model: the cached ones are the default model's.
                if s == Setting::Effort
                    && let Some(model) = picked(Setting::Model)
                    && crate::agent::acp_option(&options, Setting::Model)
                        .is_some_and(|o| o.current != model)
                {
                    return None;
                }
                let o = crate::agent::acp_option(&options, s)?;
                let (mut current, choices) = crate::agent::acp_setting_options(o, s);
                if let Some(v) = picked(s).filter(|v| choices.iter().any(|c| c.id == *v)) {
                    current = v;
                }
                Some((current, choices))
            }
            model::Agent::Codex => {
                let options = catalog::codex_options();
                let draft = codex_draft(env);
                let picked =
                    |s: Setting| draft.iter().find(|(d, _)| *d == s).map(|(_, v)| v.clone());
                // Reasoning levels are per model: the cached ones are the default model's.
                if s == Setting::Effort
                    && let Some(model) = picked(Setting::Model)
                    && crate::agent::acp_option(&options, Setting::Model)
                        .is_some_and(|o| o.current != model)
                {
                    return None;
                }
                let o = crate::agent::acp_option(&options, s)?;
                let (mut current, choices) = crate::agent::acp_setting_options(o, s);
                if let Some(v) = picked(s).filter(|v| choices.iter().any(|c| c.id == *v)) {
                    current = v;
                }
                Some((current, choices))
            }
            // Devin's mode and model; it has no reasoning levels.
            model::Agent::Devin => {
                let options = catalog::devin_options();
                let draft = devin_draft(env);
                let o = crate::agent::acp_option(&options, s)?;
                let (mut current, choices) = crate::agent::acp_setting_options(o, s);
                if let Some((_, v)) = draft
                    .iter()
                    .find(|(d, v)| *d == s && choices.iter().any(|c| c.id == *v))
                {
                    current = v.clone();
                }
                Some((current, choices))
            }
            model::Agent::WorkersAi => {
                let options = crate::agent::workers_ai::draft_options(&env.workers_ai_settings);
                crate::agent::acp_option(&options, s)
                    .map(|o| crate::agent::acp_setting_options(o, s))
            }
        }
    }

    /// The label a chip shows for a setting's value.
    pub(super) fn setting_value_label(&self, s: Setting, cx: &gpui::App) -> Option<String> {
        if let Some(thread) = self.composer_thread() {
            return thread.read(cx).setting_label(s);
        }
        let (current, options) = self.setting_choices(s, cx)?;
        Some(match self.env().agent {
            model::Agent::Droid => {
                let settings = self.draft_settings()?;
                if settings.get(s).is_empty() {
                    "Default".to_string()
                } else {
                    crate::agent::setting_label(&settings, &catalog::current().models, s)
                }
            }
            model::Agent::Claude
            | model::Agent::OpenCode
            | model::Agent::Codex
            | model::Agent::Devin
            | model::Agent::WorkersAi => crate::agent::acp_label(s, &current, &options),
        })
    }

    /// Picks a setting for the composer's session, or for the next one. Either way
    /// the environment remembers it, so its next session starts the same.
    pub(super) fn choose_setting(&mut self, s: Setting, value: &str, cx: &mut Context<Self>) {
        let agent = self.composer_agent();
        // Picking the model a running session is on: the next session gets
        // its reasoning level too. Another session may have left the
        // environment a different model, and that pick would then clear it.
        let live_effort = (s == Setting::Model && self.composer_thread().is_some())
            .then(|| {
                let on = |s| self.setting_choices(s, cx).map(|(v, _)| v);
                on(Setting::Model)
                    .is_some_and(|m| m == value)
                    .then(|| on(Setting::Effort))
                    .flatten()
            })
            .flatten()
            .filter(|v| !v.is_empty());
        if let Some(thread) = self.composer_thread() {
            thread.update(cx, |t, cx| t.set_setting(s, value, cx));
        } else if self.composer_in_session() {
            return;
        }
        if self.has_projects() {
            let path = self.env().path.clone();
            self.remember_setting(&path, agent, s, value, cx);
            if let Some(effort) = live_effort {
                self.remember_setting(&path, agent, Setting::Effort, &effort, cx);
            }
        }
        cx.notify();
    }

    /// What the next `agent` session of the environment at `env_path` starts with.
    pub(super) fn remember_setting(
        &mut self,
        env_path: &Path,
        agent: model::Agent,
        s: Setting,
        value: &str,
        cx: &mut Context<Self>,
    ) {
        // The value the menu shows: an agent still on its default model has
        // none in its draft.
        let shown = self.draft_choices(env_path, agent, s).map(|(v, _)| v);
        let Some(env) = self.find_env_mut(env_path) else {
            return;
        };
        // What OpenCode takes from the pick made anywhere, until this
        // environment has a model of its own.
        let inherited = (agent == model::Agent::OpenCode).then(|| opencode_draft(env));
        let draft = match agent {
            model::Agent::Droid => &mut env.draft_settings,
            model::Agent::Claude => &mut env.claude_settings,
            model::Agent::OpenCode => &mut env.opencode_settings,
            model::Agent::Codex => &mut env.codex_settings,
            model::Agent::Devin => &mut env.devin_settings,
            model::Agent::WorkersAi => &mut env.workers_ai_settings,
        };
        put_in_draft(
            draft,
            shown.as_deref(),
            inherited.unwrap_or_default(),
            s,
            value,
        );
        // Other environments' next OpenCode sessions start on this model too.
        if agent == model::Agent::OpenCode && matches!(s, Setting::Model | Setting::Effort) {
            let model_settings = opencode_draft(env)
                .into_iter()
                .filter(|(d, _)| matches!(d, Setting::Model | Setting::Effort));
            catalog::remember_opencode_pick(model_settings.collect());
        }
        self.save(cx);
    }

    /// A setting's value and choices for `agent`: the composer's own, or what
    /// the environment's next session of another agent starts with.
    pub(super) fn agent_choices(
        &self,
        agent: model::Agent,
        s: Setting,
        cx: &gpui::App,
    ) -> Option<(String, Vec<SettingOption>)> {
        // An agent without levels (Droid before its catalog loads) has no choices.
        let choices = if agent == self.composer_agent() {
            self.setting_choices(s, cx)
        } else if self.has_projects() {
            self.draft_choices(&self.env().path, agent, s)
        } else {
            None
        };
        choices.filter(|(_, o)| !o.is_empty())
    }

    /// The label of `agent`'s value for a setting, as `setting_value_label`.
    pub(super) fn agent_value_label(
        &self,
        agent: model::Agent,
        s: Setting,
        cx: &gpui::App,
    ) -> Option<String> {
        if agent == self.composer_agent() {
            return self.setting_value_label(s, cx);
        }
        let (current, options) = self.agent_choices(agent, s, cx)?;
        // Droid's short model name, as its chip shows it.
        if agent == model::Agent::Droid
            && let Some(settings) = self.draft_settings()
        {
            return Some(if settings.get(s).is_empty() {
                "Default".into()
            } else {
                crate::agent::setting_label(&settings, &catalog::current().models, s)
            });
        }
        let label = crate::agent::acp_label(s, &current, &options);
        Some(if label.is_empty() {
            "Default".into()
        } else {
            label
        })
    }

    /// Whether the composer may move to another agent: a session another
    /// process runs can't be handed over from here.
    pub(super) fn can_switch_agent(&self) -> bool {
        !matches!(
            self.strip.tabs.get(self.composer_tab()).map(|t| &t.kind),
            Some(TabKind::Session { id, .. }) if crate::headless::shown(id)
        )
    }

    /// Moves the composer to `agent`: the next session's agent on the New
    /// session tab, or a hand-over of the running one.
    pub(super) fn switch_agent(
        &mut self,
        agent: model::Agent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if agent == self.composer_agent() || !self.can_switch_agent() {
            return;
        }
        if self.composer_in_session() {
            self.continue_with(agent, window, cx);
        } else {
            self.env_mut().agent = agent;
            model::set_default_agent(agent);
            self.save(cx);
        }
        cx.notify();
    }

    /// Picks a setting of `agent`. For another agent than the composer's it is
    /// remembered, so the switch to that agent starts with it. Only a model
    /// pick switches a running session: handing it over stops its agent, which
    /// a click meant for reasoning or fast mode should not do.
    pub(super) fn pick_setting(
        &mut self,
        agent: model::Agent,
        s: Setting,
        value: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if agent == self.composer_agent() {
            self.choose_setting(s, value, cx);
            return;
        }
        if !self.has_projects() || !self.can_switch_agent() {
            return;
        }
        let path = self.env().path.clone();
        self.remember_setting(&path, agent, s, value, cx);
        if s == Setting::Model || !self.composer_in_session() {
            self.switch_agent(agent, window, cx);
        }
        cx.notify();
    }

    /// Back to `agent`'s defaults, for the composer's session (when it is
    /// `agent`'s) and the next ones.
    pub(super) fn reset_settings(&mut self, agent: model::Agent, cx: &mut Context<Self>) {
        if !self.has_projects() {
            return;
        }
        {
            let env = self.env_mut();
            // Only what the model menu shows; the permission level stays.
            let model_menu =
                |s: &Setting| matches!(s, Setting::Model | Setting::Effort | Setting::Fast);
            match agent {
                model::Agent::Droid => env.draft_settings.retain(|(s, _)| !model_menu(s)),
                model::Agent::Claude => env.claude_settings.retain(|(s, _)| !model_menu(s)),
                model::Agent::OpenCode => {
                    env.opencode_settings.retain(|(s, _)| !model_menu(s));
                    catalog::remember_opencode_pick(Vec::new());
                }
                model::Agent::Codex => env.codex_settings.retain(|(s, _)| !model_menu(s)),
                model::Agent::Devin => env.devin_settings.retain(|(s, _)| !model_menu(s)),
                model::Agent::WorkersAi => env.workers_ai_settings.retain(|(s, _)| !model_menu(s)),
            }
        }
        self.save(cx);
        if let Some(thread) = self
            .composer_thread()
            .filter(|_| agent == self.composer_agent())
        {
            let defaults: Vec<(Setting, String)> = match agent {
                model::Agent::Droid => {
                    let d = &catalog::current().defaults;
                    [Setting::Model, Setting::Effort]
                        .into_iter()
                        .map(|s| (s, d.get(s).to_string()))
                        .filter(|(_, v)| !v.is_empty())
                        .collect()
                }
                model::Agent::Claude | model::Agent::Codex | model::Agent::Devin => {
                    let options = match agent {
                        model::Agent::Claude => catalog::claude_options(),
                        model::Agent::Codex => catalog::codex_options(),
                        _ => catalog::devin_options(),
                    };
                    [Setting::Model, Setting::Effort, Setting::Fast]
                        .into_iter()
                        .filter_map(|s| {
                            Some((s, crate::agent::acp_option(&options, s)?.current.clone()))
                        })
                        .collect()
                }
                // OpenCode's default model; its reasoning follows.
                model::Agent::OpenCode => {
                    crate::agent::acp_option(&catalog::opencode_options(), Setting::Model)
                        .map(|o| (Setting::Model, o.current.clone()))
                        .into_iter()
                        .collect()
                }
                model::Agent::WorkersAi => crate::agent::workers_ai::draft_options(&[])
                    .into_iter()
                    .map(|o| {
                        (
                            if o.category.as_deref() == Some("model") {
                                Setting::Model
                            } else {
                                Setting::Effort
                            },
                            o.current,
                        )
                    })
                    .collect(),
            };
            thread.update(cx, |t, cx| {
                for (s, v) in defaults {
                    if t.setting(s).is_some_and(|(current, _)| current != v) {
                        t.set_setting(s, &v, cx);
                    }
                }
            });
        }
        cx.notify();
    }

    /// Shift+Tab: the next permission level (Claude's mode, Droid's autonomy).
    pub(super) fn cycle_permissions(&mut self, cx: &mut Context<Self>) {
        let s = match self.composer_agent() {
            model::Agent::Droid if self.setting_choices(Setting::Autonomy, cx).is_some() => {
                Setting::Autonomy
            }
            _ => Setting::Mode,
        };
        let Some((current, options)) = self.setting_choices(s, cx) else {
            return;
        };
        let options: Vec<_> = options
            .into_iter()
            .filter(|o| o.disabled.is_none())
            .collect();
        let Some(next) = options
            .iter()
            .position(|o| o.id == current)
            .map(|i| &options[(i + 1) % options.len()])
            .or(options.first())
        else {
            return;
        };
        self.composer_flash = Some(format!("{}: {}", s.label(), next.name).into());
        let id = next.id.clone();
        self.choose_setting(s, &id, cx);
    }

    // ---- questions read aloud ----------------------------------------------------

    /// Reads a session's new AskUser question aloud once, even while the session is on
    /// screen, and stops reading a question that was answered or dismissed.
    fn speak_question(
        &mut self,
        env_path: &Path,
        id: &SharedString,
        thread: &Entity<Thread>,
        cx: &mut Context<Self>,
    ) {
        let pending = thread
            .read(cx)
            .pending_question()
            .map(|q| (q.tool_call_id.clone(), q.questions.clone()));
        let last = SPOKEN.with(|s| s.borrow().get(id.as_ref()).cloned());
        if last.as_deref() == pending.as_ref().map(|(key, _)| key.as_str()) {
            return;
        }
        if let Some(old) = &last {
            crate::voice::cancel(old);
        }
        let Some((key, questions)) = pending else {
            SPOKEN.with(|s| s.borrow_mut().remove(id.as_ref()));
            return;
        };
        SPOKEN.with(|s| s.borrow_mut().insert(id.to_string(), key.clone()));
        if !crate::voice::settings().enabled {
            return;
        }
        let title = self
            .find_env(env_path)
            .and_then(|e| e.session_index(id).map(|i| e.sessions[i].title.to_string()))
            .filter(|t| !t.is_empty())
            .unwrap_or_else(|| thread.read(cx).title().to_string());
        let (text, lang) = crate::voice::question_text(&title, &questions);
        crate::voice::speak(key, text, lang);
    }

    // ---- desktop notifications ---------------------------------------------
    /// One desktop notification when a session the user is not looking at finishes a
    /// turn, wants an answer, or fails.
    pub(super) fn notify_desktop(
        &mut self,
        env_path: &Path,
        id: &SharedString,
        thread: &Entity<Thread>,
        ev: &ThreadEvent,
        window: &Window,
        cx: &mut Context<Self>,
    ) {
        let t = thread.read(cx);
        let (kind, body): (Notice, String) = match ev {
            ThreadEvent::Finished { stop_reason } => match stop_reason.as_str() {
                "cancelled" => return,
                "error" => (Notice::Error, error_text(t)),
                _ => (Notice::Finished, "Finished its turn.".into()),
            },
            ThreadEvent::NeedsPermission => {
                let body = if let Some(p) = t.pending_permission() {
                    if p.plan.is_some() {
                        "Has a plan ready for your approval.".to_string()
                    } else {
                        format!("Needs permission: {}", p.title)
                    }
                } else if t.pending_question().is_some() {
                    "Has a question for you.".into()
                } else {
                    "Needs your input.".into()
                };
                (Notice::Input, body)
            }
            ThreadEvent::StatusChanged if t.needs_install().is_some() => {
                (Notice::Input, error_text(t))
            }
            ThreadEvent::StatusChanged if matches!(t.status(), ThreadStatus::Error(_)) => {
                (Notice::Error, error_text(t))
            }
            ThreadEvent::StatusChanged => {
                // Back at work: the next turn end or question notifies again.
                if t.status() == ThreadStatus::Working {
                    NOTIFIED.with(|n| n.borrow_mut().remove(id.as_ref()));
                }
                return;
            }
            _ => return,
        };
        // A failing turn reports both a status change and its end; one notice is enough.
        let repeated = NOTIFIED.with(|n| n.borrow_mut().insert(id.to_string(), kind) == Some(kind));
        if repeated {
            return;
        }
        let title = self
            .find_env(env_path)
            .and_then(|e| e.session_index(id).map(|i| e.sessions[i].title.clone()))
            .filter(|t| !t.is_empty())
            .unwrap_or_else(|| t.title());
        // Phones hear when the user is away from this window; the desktop, when
        // the session is not the one on screen.
        let at_computer = window.is_window_active();
        if !at_computer {
            let pid = self.phone_id(env_path, id);
            self.notify_phones(&pid, kind, &title, &body, cx);
        }
        let visible = at_computer
            && self.has_projects()
            && self.env().path == env_path
            && self.active_session_id().as_ref() == Some(id);
        if visible {
            return;
        }
        // The island says more (model, last reply); the desktop only stands in for it.
        let alert = self.session_alert(id, thread, kind, &title, cx);
        if !self.show_alert(alert, cx) {
            send_notification(id, &title, &body, kind == Notice::Error, cx);
        }
    }

    /// Writes a live session's transcript in the background.
    pub(super) fn save_transcript(&mut self, env_path: &Path, id: &str, cx: &mut Context<Self>) {
        let Some(thread) = self
            .session_mut(env_path, id)
            .and_then(|s| s.live.as_ref().map(|l| l.thread.clone()))
        else {
            return;
        };
        let (path, transcript) = (state::transcript_path(id), thread.read(cx).transcript(cx));
        cx.background_spawn(crate::guard::task(async move {
            if let Err(e) = write_transcript(&path, &transcript) {
                eprintln!("arakun: could not save {}: {e}", path.display());
            }
        }))
        .detach();
    }

    pub(super) fn find_env(&self, path: &Path) -> Option<&model::Environment> {
        self.projects
            .iter()
            .flat_map(|p| &p.environments)
            .find(|e| e.path == path)
    }

    // ---- session actions -------------------------------------------------

    /// A forked or rewound copy of a session, listed next to its source and opened in a tab.
    fn add_forked_session(
        &mut self,
        env_path: &Path,
        meta: crate::agent::SessionMeta,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let id = new_session_id();
        let Some(env) = self.find_env_mut(env_path) else {
            return;
        };
        let title: SharedString = meta.title.clone().into();
        let agent = model::Agent::from_key(meta.agent.id());
        let mut session = Session::new(id.clone(), title.clone(), agent);
        session.meta = Some(meta);
        env.sessions.insert(0, session);
        let visible = self.has_projects() && self.env().path == env_path;
        if visible {
            self.open_session(id, window, cx);
        } else {
            // Forked from a session of another environment: its tab waits there.
            self.strip
                .add(Tab::of(env_path, title, TabKind::Session { agent, id }));
            self.save(cx);
            cx.notify();
        }
    }

    pub(super) fn rename_session(&mut self, id: &str, title: &str, cx: &mut Context<Self>) {
        let title = title.trim();
        if title.is_empty() || !self.has_projects() {
            return;
        }
        let new_title: SharedString = title.to_string().into();
        let env = self.env_mut();
        let Some(i) = env.session_index(id) else {
            return;
        };
        let session = &mut env.sessions[i];
        // `meta` keeps the agent's title: `ensure_live` spots the difference and
        // passes the rename on the next time the session starts.
        session.title = new_title.clone();
        let thread = session.live.as_ref().map(|l| l.thread.clone());
        if let Some(i) = self.strip.session_tab(id) {
            self.strip.tabs[i].title = new_title.clone();
        }
        if let Some(thread) = thread {
            thread.update(cx, |t, cx| t.rename(title, cx));
        }
        self.save(cx);
        cx.notify();
    }

    /// Fork or compact a native Droid session; the session opens in a tab so the
    /// result is on screen, and the request waits there until Droid is ready.
    pub(super) fn session_action(
        &mut self,
        id: SharedString,
        fork: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.open_session(id.clone(), window, cx);
        let env = self.env();
        let Some(thread) = env
            .session_index(&id)
            .and_then(|i| env.sessions[i].live.as_ref())
            .map(|l| l.thread.clone())
        else {
            return;
        };
        thread.update(cx, |t, cx| {
            if fork {
                t.fork(cx)
            } else {
                t.compact(None, cx)
            }
        });
    }

    /// Takes a session off arakun's list (its tab too). Droid's own copy stays.
    pub(super) fn remove_session(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) {
        if !self.has_projects() {
            return;
        }
        // Closing its tab may bring another environment on screen: the session
        // is taken off the one that was on screen when this was asked.
        let path = self.env().path.clone();
        while let Some(i) = self.strip.session_tab(id) {
            self.discard_tab(i, window, cx);
        }
        let removed = self
            .find_env_mut(&path)
            .and_then(|env| env.session_index(id).map(|i| env.sessions.remove(i)));
        if let Some(live) = removed.as_ref().and_then(|s| s.live.as_ref()) {
            report_failure(&live.thread, true, cx);
            live.thread.update(cx, |t, _| t.shutdown());
        }
        let mut paths = vec![state::transcript_path(id)];
        for earlier in removed.iter().flat_map(|s| &s.continues) {
            paths.push(state::transcript_path(&earlier.id));
            paths.push(
                state::data_dir()
                    .join("linked")
                    .join(format!("{}.md", earlier.id)),
            );
        }
        // A headless session lives in its own folder, which the app scans every
        // second; it has to go too or the session comes back.
        let headless = crate::headless::shown(id).then(|| id.to_string());
        if headless.is_some() {
            crate::headless::begin_discard(id);
            crate::headless::set_shown(id, false);
        }
        cx.background_spawn(crate::guard::task(async move {
            for path in paths {
                let _ = std::fs::remove_file(path);
            }
            if let Some(id) = headless {
                crate::headless::discard(&id);
            }
        }))
        .detach();
        forget_alerts(id);
        self.save(cx);
        cx.notify();
    }

    /// Droid session ids arakun already lists, in any environment.
    pub(super) fn known_agent_ids(&self) -> std::collections::HashSet<String> {
        self.projects
            .iter()
            .flat_map(|p| &p.environments)
            .flat_map(|e| &e.sessions)
            .flat_map(|s| {
                let own = s.meta.as_ref().and_then(|m| m.session_id.clone());
                own.into_iter()
                    .chain(s.continues.iter().filter_map(|c| c.agent_session.clone()))
            })
            .collect()
    }

    pub(super) fn import_agent_sessions(
        &mut self,
        found: Vec<catalog::AgentSession>,
        cx: &mut Context<Self>,
    ) {
        if !self.has_projects() || found.is_empty() {
            return;
        }
        let known = self.known_agent_ids();
        let env = self.env_mut();
        let mut found = found;
        found.retain(|d| !known.contains(&d.id));
        found.sort_by_key(|a| a.updated);
        for d in found {
            let agent = match d.agent {
                AgentKind::Claude => model::Agent::Claude,
                AgentKind::Droid => model::Agent::Droid,
                AgentKind::OpenCode => model::Agent::OpenCode,
                AgentKind::Codex => model::Agent::Codex,
                AgentKind::Devin => model::Agent::Devin,
                AgentKind::WorkersAi => model::Agent::WorkersAi,
            };
            let mut session = Session::new(new_session_id(), d.title.clone().into(), agent);
            session.updated = d.updated;
            session.meta = Some(crate::agent::SessionMeta {
                agent: d.agent,
                session_id: Some(d.id),
                title: d.title,
                cwd: d.cwd,
                created: d.updated,
                updated: d.updated,
                pr: None,
            });
            env.sessions.insert(0, session);
        }
        self.save(cx);
        cx.notify();
    }

    /// Writes every live transcript; called before the window goes away. When
    /// arakun is restarting, turns cut off are marked to go on in the next one,
    /// and their sessions are listed so it reopens them at once.
    pub(super) fn save_transcripts(&self, cx: &gpui::App) {
        let mut resume = Vec::new();
        for env in self.projects.iter().flat_map(|p| &p.environments) {
            // A server keeps its own sessions' transcripts.
            if env.remote.is_some() {
                continue;
            }
            for s in env
                .sessions
                .iter()
                .filter(|s| !crate::headless::shown(&s.id))
            {
                if let Some(live) = &s.live {
                    let mut t = live.thread.read(cx).transcript(cx);
                    if self.restarting && t.interrupted {
                        t.resume = true;
                        resume.push(s.id.to_string());
                    }
                    let _ = write_transcript(&state::transcript_path(&s.id), &t);
                }
            }
        }
        if self.restarting {
            let _ = crate::durable::write(
                &state::resume_path(),
                &serde_json::to_vec(&resume).unwrap_or_default(),
            );
        }
    }

    /// After `arakun restart`: brings back the sessions whose turns it cut off,
    /// so they go on without being opened first.
    pub(super) fn resume_after_restart(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let path = state::resume_path();
        let Ok(bytes) = std::fs::read(&path) else {
            return;
        };
        let _ = std::fs::remove_file(&path);
        let ids: Vec<String> = serde_json::from_slice(&bytes).unwrap_or_default();
        for id in ids {
            let env_path = self
                .projects
                .iter()
                .flat_map(|p| &p.environments)
                .find(|e| e.session_index(&id).is_some())
                .map(|e| e.path.clone());
            if let Some(env_path) = env_path {
                self.ensure_live(&env_path, &id, window, cx);
            }
        }
    }
}

/// What a session did that deserves a word: on the desktop, and on the phones.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Notice {
    Finished,
    Input,
    Error,
}

thread_local! {
    /// The last desktop notice per session, so one event is announced once.
    static NOTIFIED: RefCell<HashMap<String, Notice>> = RefCell::new(HashMap::new());
    /// The AskUser question last read aloud per session, by tool call id.
    static SPOKEN: RefCell<HashMap<String, String>> = RefCell::new(HashMap::new());
}

fn error_text(t: &Thread) -> String {
    match t.status() {
        ThreadStatus::Error(msg) => {
            let line = msg.lines().next().unwrap_or("").trim();
            match line.char_indices().nth(160) {
                Some((i, _)) => format!("{}…", &line[..i]),
                None if line.is_empty() => "Stopped with an error.".into(),
                None => line.to_string(),
            }
        }
        _ => "Stopped with an error.".into(),
    }
}

/// The installed `arakun` theme icon if there is one, else the one in the source tree.
fn notification_icon() -> Option<String> {
    let themed = ["/usr/share/icons/hicolor", "/usr/local/share/icons/hicolor"]
        .into_iter()
        .map(std::path::PathBuf::from)
        .chain(std::iter::once(
            model::home().join(".local/share/icons/hicolor"),
        ))
        .any(|dir| {
            ["48x48", "64x64", "128x128", "256x256", "scalable"]
                .iter()
                .any(|s| {
                    dir.join(s).join("apps/arakun.png").exists()
                        || dir.join(s).join("apps/arakun.svg").exists()
                })
        });
    if themed {
        return Some("arakun".into());
    }
    let bundled = Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/brand/arakun.png");
    bundled.exists().then(|| bundled.display().to_string())
}

/// `key`: what a click on it opens, as "View" on the island does (`Open::key`);
/// empty when it is about nothing to open.
pub(super) fn send_notification(
    key: &str,
    title: &str,
    body: &str,
    critical: bool,
    cx: &gpui::App,
) {
    if std::env::var_os("ARAKUN_NOTIFY").is_some_and(|v| v == "off") {
        return;
    }
    // osascript's notifications belong to Script Editor, and a click opens it. From
    // Arakun.app they are Arakun's own, and main.rs hears the click.
    #[cfg(target_os = "macos")]
    if crate::platform::in_app_bundle() && std::env::var_os("ARAKUN_DEV").is_none() {
        cx.show_system_notification(gpui::SystemNotification {
            tag: notification_tag(key),
            title: crate::redact::text(title).into_owned().into(),
            body: crate::redact::text(body).into_owned().into(),
            actions: Vec::new(),
        });
        return;
    }
    #[cfg(not(target_os = "macos"))]
    let _ = (key, cx);
    let icon = if cfg!(windows) {
        None
    } else {
        notification_icon()
    };
    crate::platform::notify(
        &crate::redact::text(title),
        &crate::redact::text(body),
        critical,
        icon.as_deref(),
    );
}

/// A newer notification with the same tag replaces the older one: one per session is
/// enough, but notices about nothing in particular each get their own. `open:none:…`
/// opens nothing, so a click on one only brings the window forward.
#[cfg(any(test, target_os = "macos"))]
fn notification_tag(key: &str) -> SharedString {
    static NEXT: AtomicU32 = AtomicU32::new(0);
    if key.is_empty() {
        format!("open:none:{}", NEXT.fetch_add(1, Ordering::Relaxed)).into()
    } else {
        key.to_string().into()
    }
}

/// Drops what was remembered about a session's alerts, and any it is still speaking.
fn forget_alerts(id: &str) {
    NOTIFIED.with(|n| n.borrow_mut().remove(id));
    if let Some(key) = SPOKEN.with(|s| s.borrow_mut().remove(id)) {
        crate::voice::cancel(&key);
    }
}

/// Written so only the owner can read it: it holds the whole chat, tool
/// output included.
fn write_transcript(path: &Path, t: &crate::agent::Transcript) -> std::io::Result<()> {
    crate::durable::write_private(path, &serde_json::to_vec(t)?)
}

/// The Codex settings an environment's next session starts with, the model first:
/// the reasoning levels on offer depend on it. A model Codex no longer offers is
/// dropped, since setting it would only fail.
pub(super) fn codex_draft(env: &model::Environment) -> Vec<(Setting, String)> {
    offered_draft(&env.codex_settings, &catalog::codex_options())
}

/// The Devin settings an environment's next session starts with. Signed out,
/// Devin offers no models, so a remembered one waits until it does.
pub(super) fn devin_draft(env: &model::Environment) -> Vec<(Setting, String)> {
    offered_draft(&env.devin_settings, &catalog::devin_options())
}

/// `picked`, the model first, without a model the agent no longer offers.
fn offered_draft(
    picked: &[(Setting, String)],
    options: &[crate::agent::ConfigOption],
) -> Vec<(Setting, String)> {
    let mut draft = picked.to_vec();
    if let Some(o) = crate::agent::acp_option(options, Setting::Model) {
        draft.retain(|(s, v)| *s != Setting::Model || o.options.iter().any(|c| c.id == *v));
    }
    draft.sort_by_key(|(s, _)| *s != Setting::Model);
    draft
}

/// Puts the pick of `value` for `s` in an agent's `draft`. `shown` is the
/// value the menu showed, and `inherited` what the draft takes from elsewhere
/// until it has a model of its own (OpenCode's last pick).
fn put_in_draft(
    draft: &mut Vec<(Setting, String)>,
    shown: Option<&str>,
    inherited: Vec<(Setting, String)>,
    s: Setting,
    value: &str,
) {
    let changed = value_changed(shown, draft, s, value);
    draft.retain(|(d, _)| *d != s);
    draft.push((s, value.to_string()));
    if s != Setting::Model {
        return;
    }
    let has_effort = |draft: &[(Setting, String)]| draft.iter().any(|(d, _)| *d == Setting::Effort);
    if changed {
        // A model change may drop the effort picked for the old model. Picking
        // the same model again keeps it.
        draft.retain(|(d, _)| *d != Setting::Effort);
    } else if !has_effort(draft)
        && let Some(effort) = inherited.into_iter().find(|(d, _)| *d == Setting::Effort)
    {
        // With a model of its own the environment stops taking that pick,
        // and its reasoning level would go with it.
        draft.push(effort);
    }
}

/// Whether picking `value` for `s` changes it: against the value the menu
/// shows (`shown`), or against the draft when the agent has no choices yet.
fn value_changed(
    shown: Option<&str>,
    draft: &[(Setting, String)],
    s: Setting,
    value: &str,
) -> bool {
    match shown {
        Some(shown) => shown != value,
        None => !draft.iter().any(|(d, v)| *d == s && v == value),
    }
}

/// The OpenCode settings an environment's next session starts with: its own
/// picks, and where it has no model of its own, the model last picked anywhere.
/// The model goes first: the reasoning levels on offer depend on it.
pub(super) fn opencode_draft(env: &model::Environment) -> Vec<(Setting, String)> {
    let mut draft = env.opencode_settings.clone();
    if !draft.iter().any(|(s, _)| *s == Setting::Model) {
        for (s, v) in catalog::opencode_picked() {
            if !draft.iter().any(|(d, _)| *d == s) {
                draft.push((s, v));
            }
        }
    }
    // A model OpenCode no longer offers (arakun now pins its list) would only fail.
    let options = catalog::opencode_options();
    if let Some(o) = crate::agent::acp_option(&options, Setting::Model) {
        draft.retain(|(s, v)| *s != Setting::Model || o.options.iter().any(|c| c.id == *v));
    }
    draft.sort_by_key(|(s, _)| *s != Setting::Model);
    draft
}

#[cfg(test)]
mod tests {
    #[test]
    fn agent_version_keeps_only_the_version() {
        use super::agent_version;
        assert_eq!(agent_version("Factory Droid 0.240.1"), Some("0.240.1"));
        assert_eq!(
            agent_version("claude-code-acp v1.2.3-beta.1"),
            Some("1.2.3-beta.1")
        );
        assert_eq!(agent_version("Acme internal build"), None);
        assert_eq!(agent_version("x 1.0/../secret"), None);
    }

    use super::*;

    #[test]
    fn a_session_keeps_one_notification_and_other_notices_each_get_their_own() {
        assert_eq!(notification_tag("abc123"), "abc123");
        let (a, b) = (notification_tag(""), notification_tag(""));
        assert!(a.starts_with("open:none:"));
        assert_ne!(a, b);
    }

    #[test]
    fn reselecting_an_inherited_model_keeps_its_reasoning_level() {
        let (m, e) = (Setting::Model, Setting::Effort);
        let pick = vec![(m, "gpt".to_string()), (e, "high".to_string())];
        // The environment has no settings of its own and shows the pick's model.
        let mut draft = Vec::new();
        put_in_draft(&mut draft, Some("gpt"), pick.clone(), m, "gpt");
        // The draft is also what is written back as the pick made anywhere.
        assert_eq!(draft, pick);
        // Another model: the level went with the old one.
        put_in_draft(&mut draft, Some("gpt"), pick.clone(), m, "other");
        assert_eq!(draft, [(m, "other".to_string())]);
        // A level of its own is not replaced by the inherited one.
        let mut draft = vec![(e, "low".to_string())];
        put_in_draft(&mut draft, Some("gpt"), pick, m, "gpt");
        assert_eq!(draft, [(e, "low".to_string()), (m, "gpt".to_string())]);
    }

    #[test]
    fn picking_the_model_an_agent_is_on_by_default_changes_nothing() {
        let m = Setting::Model;
        // On its default model the draft has no model, only the level picked.
        let draft = vec![(Setting::Effort, "medium".to_string())];
        assert!(!value_changed(Some("sonnet"), &draft, m, "sonnet"));
        assert!(value_changed(Some("sonnet"), &draft, m, "opus"));
        // No choices loaded yet: the draft decides.
        assert!(value_changed(None, &draft, m, "sonnet"));
        let draft = vec![(m, "opus".to_string())];
        assert!(!value_changed(None, &draft, m, "opus"));
    }
}
